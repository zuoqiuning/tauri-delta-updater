use crate::core::cancel::CancelToken;
use crate::error::{DeltaError, Result};
use futures_util::StreamExt;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// 下载失败后重试的最大次数。清单/签名等小文件请求共用同一策略。
pub(crate) const MAX_RETRIES: u32 = 4;

/// 计算第 `attempt` 次重试的等待时长（指数退避 + 抖动）。
///
/// 抖动避免同一批客户端在 CDN 抖动时同步重连，形成重试风暴。
pub(crate) fn backoff_delay(attempt: u32) -> std::time::Duration {
    let base_secs = 2u64.pow(attempt.min(4));
    let jitter_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0))
        % 500;
    std::time::Duration::from_millis(base_secs * 1000 + jitter_ms)
}

/// 跨进程续传的最长保留时间：超过后残留补丁与身份记录在下次清理时丢弃。
///
/// 没有上限的话，一次中断的下载可能永久占用工作目录（补丁接近全量体积）。
pub const RESUME_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// 身份记录是否存在且足够新。
pub async fn resume_fresh(meta_path: &Path) -> bool {
    fs::metadata(meta_path)
        .await
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age.as_secs() < RESUME_MAX_AGE_SECS)
}

/// 让断点续传只对**同一个补丁**生效。
///
/// 下载前调用：身份记录与 `expected_hash`（清单里已签名的 patch_hash）一致时
/// 保留半截文件继续续传；否则丢弃残留并写入新的身份记录。
///
/// 这解决了"上一次更新的半截补丁被 Range 追加成本补丁"的错位：
/// 那种情况会在下载完成后才以哈希不符失败，白白消耗流量。
pub async fn prepare_resume(patch_path: &Path, meta_path: &Path, expected_hash: &str) {
    let same = resume_fresh(meta_path).await
        && fs::read_to_string(meta_path)
            .await
            .map(|content| content.trim().eq_ignore_ascii_case(expected_hash))
            .unwrap_or(false);

    if !same {
        let _ = fs::remove_file(patch_path).await;
        let _ = fs::write(meta_path, expected_hash.as_bytes()).await;
    }
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
///
/// 跨进程续传还要求残留文件属于**同一个补丁**。直接调用本函数时由调用方
/// 自行保证；高层流程用 [`prepare_resume`] 以已签名的 `patch_hash` 做身份校验。
pub async fn download_patch_with_progress<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    download_patch_with_progress_controlled(client, url, save_path, max_bytes, None, on_progress)
        .await
}

/// 带进度回调且支持协作式取消的补丁下载。
///
/// 与 [`download_patch_with_progress`] 相同，额外接受取消令牌：
/// 每个分块写入前检查一次，取消时删除半截文件并返回 [`DeltaError::Cancelled`]。
///
/// 取消只在下一次分块到达时生效——服务端长时间不返回数据时，
/// 仍要等读超时（默认 60 秒）才能返回。
pub async fn download_patch_with_progress_controlled<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    cancel: Option<&CancelToken>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let mut retry_count = 0;

    loop {
        if is_cancelled(cancel) {
            return Err(DeltaError::Cancelled);
        }

        match download_attempt(client, url, save_path, max_bytes, cancel, &mut on_progress).await {
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

/// 带进度回调、支持取消的全量安装包下载。
///
/// 与补丁下载共用同一套续传/重试逻辑，额外做一件事：**拿到
/// `Content-Length` 后先查磁盘空间**。安装包通常几十到几百 MB，
/// 下到一半才发现空间不足既浪费流量，也会把用户留在失败状态。
pub async fn download_full_with_progress_controlled<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    cancel: Option<&CancelToken>,
    work_dir: &Path,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let mut retry_count = 0;

    loop {
        if is_cancelled(cancel) {
            return Err(DeltaError::Cancelled);
        }

        match download_attempt_full(
            client,
            url,
            save_path,
            max_bytes,
            cancel,
            work_dir,
            &mut on_progress,
        )
        .await
        {
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

/// 单次全量下载尝试（与补丁路径相同的续传语义 + 磁盘预检）。
async fn download_attempt_full<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    cancel: Option<&CancelToken>,
    work_dir: &Path,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let resume_from = existing_size(save_path).await;

    let (response, resumed) = match send_request(client, url, resume_from).await {
        Ok(pair) => pair,
        Err(DeltaError::ResumeRejected) if resume_from > 0 => {
            let _ = fs::remove_file(save_path).await;
            send_request(client, url, 0).await?
        }
        Err(e) => return Err(e),
    };

    if resume_from > 0 && !resumed {
        let _ = fs::remove_file(save_path).await;
        let (response, _) = send_request(client, url, 0).await?;
        return write_body(
            response,
            save_path,
            0,
            max_bytes,
            cancel,
            Some(work_dir),
            on_progress,
        )
        .await;
    }

    write_body(
        response,
        save_path,
        resume_from,
        max_bytes,
        cancel,
        Some(work_dir),
        on_progress,
    )
    .await
}

/// 是否已请求取消。
fn is_cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.is_some_and(CancelToken::is_cancelled)
}

/// 单次下载尝试。
///
/// 若本地已存在半截文件则尝试续传；服务端不支持续传时自动回退为完整下载。
async fn download_attempt<F>(
    client: &reqwest::Client,
    url: &str,
    save_path: &Path,
    max_bytes: u64,
    cancel: Option<&CancelToken>,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    if is_cancelled(cancel) {
        return Err(DeltaError::Cancelled);
    }

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
        return write_body(response, save_path, 0, max_bytes, cancel, None, on_progress).await;
    }

    write_body(
        response,
        save_path,
        resume_from,
        max_bytes,
        cancel,
        None,
        on_progress,
    )
    .await
}

/// 把响应体写入文件。
///
/// `already_downloaded` 是本次下载开始前本地已有的字节数。
///
/// `disk_check_dir` 为 `Some` 时，在写入前按剩余字节数检查磁盘空间
/// （全量安装包走这条路径；补丁本身很小，不需要）。
async fn write_body<F>(
    response: reqwest::Response,
    save_path: &Path,
    already_downloaded: u64,
    max_bytes: u64,
    cancel: Option<&CancelToken>,
    disk_check_dir: Option<&Path>,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    // 206 响应的 Content-Length 是"剩余字节数"，需加上已有部分才是总量
    let body_len = response.content_length().unwrap_or(0);
    if let (Some(dir), true) = (disk_check_dir, body_len > 0) {
        // 空间不足不该重试：重试只会再次失败并浪费带宽。
        crate::core::disk::check_disk_space(dir, body_len)?;
    }
    let total_size = already_downloaded + body_len;
    let mut downloaded = already_downloaded;

    let mut file = if already_downloaded > 0 {
        fs::OpenOptions::new().append(true).open(save_path).await?
    } else {
        fs::File::create(save_path).await?
    };

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        // 取消检查放在写入之前：取消后不留半截字节，也不触发进度回调。
        if is_cancelled(cancel) {
            drop(file);
            let _ = fs::remove_file(save_path).await;
            return Err(DeltaError::Cancelled);
        }
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

    // 已知总长度时校验最终大小，防止截断的补丁被当成完整文件。
    // 只删除"比预期长"的文件（服务端多发了字节，无法靠 Range 修正）；
    // 比预期短说明连接中断，保留半截文件让上层重试时用 Range 接着下。
    if total_size > 0 && downloaded > total_size {
        let _ = fs::remove_file(save_path).await;
        return Err(DeltaError::Network(format!(
            "下载内容超出预期: 期望 {total_size} 字节, 实际 {downloaded} 字节"
        )));
    }
    if total_size > 0 && downloaded < total_size {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn paths() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let patch = dir.path().join("patch.bin");
        let meta = dir.path().join("patch.id");
        (dir, patch, meta)
    }

    /// 同一个补丁的半截文件必须保留，供 Range 续传。
    #[tokio::test]
    async fn keeps_partial_for_same_patch() {
        let (_dir, patch, meta) = paths();
        tokio::fs::write(&meta, "abc").await.unwrap();
        tokio::fs::write(&patch, b"half").await.unwrap();

        prepare_resume(&patch, &meta, "ABC").await;

        assert_eq!(tokio::fs::read(&patch).await.unwrap(), b"half");
        assert_eq!(tokio::fs::read_to_string(&meta).await.unwrap(), "abc");
    }

    /// 换了一个补丁就必须丢弃残留：否则 Range 会把两份不同补丁拼在一起。
    #[tokio::test]
    async fn drops_partial_for_other_patch() {
        let (_dir, patch, meta) = paths();
        tokio::fs::write(&meta, "abc").await.unwrap();
        tokio::fs::write(&patch, b"half").await.unwrap();

        prepare_resume(&patch, &meta, "def").await;

        assert!(!patch.exists(), "身份不符时必须丢弃半截补丁");
        assert_eq!(tokio::fs::read_to_string(&meta).await.unwrap(), "def");
    }

    /// 没有身份记录的历史残留同样丢弃（无法证明它属于当前补丁）。
    #[tokio::test]
    async fn drops_partial_without_identity() {
        let (_dir, patch, meta) = paths();
        tokio::fs::write(&patch, b"half").await.unwrap();

        prepare_resume(&patch, &meta, "abc").await;

        assert!(!patch.exists());
        assert_eq!(tokio::fs::read_to_string(&meta).await.unwrap(), "abc");
    }
}
