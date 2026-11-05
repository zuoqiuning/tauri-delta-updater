use crate::error::{DeltaError, Result};
use futures_util::StreamExt;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// 下载失败后重试的最大次数。
const MAX_RETRIES: u32 = 4;

/// 计算第 `attempt` 次重试的等待时长（指数退避 + 抖动）。
///
/// 抖动避免同一批客户端在 CDN 抖动时同步重连，形成重试风暴。
fn backoff_delay(attempt: u32) -> std::time::Duration {
    let base_secs = 2u64.pow(attempt.min(4));
    let jitter_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0))
        % 500;
    std::time::Duration::from_millis(base_secs * 1000 + jitter_ms)
}

/// 带进度回调的补丁下载，支持断点续传。
///
/// `on_progress` 接收 `(已下载字节数, 总字节数)`；总字节数未知时为 0。
///
/// `max_bytes` 是补丁体积上限（含断点续传的已有部分），
/// 超限时中止下载并删除半截文件。传 `0` 表示不限制。
/// 调用方应从清单的 `patch_size` 推导（例如 `patch_size * 2 + 1 MB` 余量）：
/// 补丁 URL 虽被清单签名覆盖，但响应体在传输中可能被替换，
/// 无上限的流式写入会灌满磁盘。
///
/// 断点续传严格依赖服务端返回 `206 Partial Content`：
/// 若服务端忽略 `Range` 头返回 `200`，必须丢弃本地残留重新下载，
/// 否则会把完整文件追加到半截文件后面，产出损坏的补丁。
pub async fn download_patch_with_progress<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let mut retry_count = 0;

    loop {
        match download_attempt(client, url, save_path, max_bytes, &mut on_progress).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !e.is_retryable() || retry_count >= MAX_RETRIES {
                    return Err(e);
                }
                retry_count += 1;
                tokio::time::sleep(backoff_delay(retry_count)).await;
            }
        }
    }
}

/// 单次下载尝试。
///
/// 若本地已存在半截文件则尝试续传；服务端不支持续传时自动回退为完整下载。
async fn download_attempt<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let resume_from = existing_size(save_path).await;

    // 续传被服务端拒绝（本地文件比服务端还大）时，丢弃残留后完整重下
    let (response, resumed) = match send_request(client, url, resume_from).await {
        Ok(pair) => pair,
        Err(DeltaError::ResumeRejected) if resume_from > 0 => {
            let _ = fs::remove_file(save_path).await;
            send_request(client, url, 0).await?
        }
        Err(e) => return Err(e),
    };

    // 服务端忽略 Range 头返回 200：本地残留必须丢弃，否则会拼出损坏文件
    if resume_from > 0 && !resumed {
        let _ = fs::remove_file(save_path).await;
        let (response, _) = send_request(client, url, 0).await?;
        return write_body(response, save_path, 0, max_bytes, on_progress).await;
    }

    write_body(response, save_path, resume_from, max_bytes, on_progress).await
}

/// 把响应体写入文件。
///
/// `already_downloaded` 是本次下载开始前本地已有的字节数。
async fn write_body<F>(
    response: reqwest::Response,
    save_path: &Path,
    already_downloaded: u64,
    max_bytes: u64,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    // 206 响应的 Content-Length 是"剩余字节数"，需加上已有部分才是总量
    let body_len = response.content_length().unwrap_or(0);
    let total_size = already_downloaded + body_len;
    let mut downloaded = already_downloaded;

    let mut file = if already_downloaded > 0 {
        fs::OpenOptions::new().append(true).open(save_path).await?
    } else {
        fs::File::create(save_path).await?
    };

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeltaError::Network(format!("流读取失败: {e}")))?;
        downloaded += chunk.len() as u64;
        // 体积上限：防被劫持的服务端用超大响应灌满磁盘。
        // 必须在写入前判断，否则超限字节已经落盘。
        if max_bytes > 0 && downloaded > max_bytes {
            drop(file);
            let _ = fs::remove_file(save_path).await;
            return Err(DeltaError::PatchTooLarge { max_bytes });
        }
        file.write_all(&chunk).await?;
        on_progress(downloaded, total_size);
    }
    file.flush().await?;
    file.sync_all().await?;

    // 已知总长度时校验最终大小，防止截断的补丁被当成完整文件
    if total_size > 0 && downloaded != total_size {
        let _ = fs::remove_file(save_path).await;
        return Err(DeltaError::Network(format!(
            "下载不完整: 期望 {total_size} 字节, 实际 {downloaded} 字节"
        )));
    }

    Ok(())
}

/// 发起下载请求，返回响应体与"是否真的在续传"。
///
/// 只有服务端明确返回 `206 Partial Content` 时才算续传成功。
async fn send_request(
    client: &reqwest::Client,
    url: &str,
    resume_from: u64,
) -> Result<(reqwest::Response, bool)> {
    let mut request = client.get(url);
    if resume_from > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            DeltaError::Timeout(e.to_string())
        } else {
            DeltaError::Network(format!("网络请求失败: {e}"))
        }
    })?;

    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        return Err(DeltaError::ResumeRejected);
    }
    if !status.is_success() {
        return Err(DeltaError::HttpStatus(status.as_u16()));
    }

    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    Ok((response, resumed))
}

/// 获取已存在文件的大小，文件不存在或读取失败时返回 0。
async fn existing_size(path: &Path) -> u64 {
    fs::metadata(path).await.map(|m| m.len()).unwrap_or(0)
}
