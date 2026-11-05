//! 完整的增量更新流程示例。
//!
//! 演示最简接入方式：一次调用完成全部更新步骤。

use tauri_delta_updater::{DeltaUpdater, UpdateConfig, UpdateOutcome};

#[tokio::main]
async fn main() {
    // 清单地址与公钥是唯一必须提供的信息。
    // 公钥必须固化在应用里，不能从服务器下载。
    let updater = DeltaUpdater::new(
        UpdateConfig::new("https://cdn.example.com/latest.json", "BASE64_PUBLIC_KEY")
            .current_version("0.1.0"),
    )
    .unwrap();

    match updater
        .update_with_progress(|downloaded, total| {
            if total > 0 {
                println!("下载进度: {downloaded}/{total}");
            }
        })
        .await
    {
        Ok(UpdateOutcome::Patched { staged }) => {
            println!("已通过增量补丁更新到 {}", staged.manifest().version);
        }
        Ok(UpdateOutcome::FallbackToFullDownload {
            full_download_url,
            reason,
        }) => {
            println!("增量不可用（{reason}），请走全量下载: {full_download_url:?}");
        }
        Ok(UpdateOutcome::SafeMode) => {
            println!("连续更新失败次数已达上限，请提示用户手动下载安装包");
        }
        // UpdateOutcome 是 non_exhaustive 的：将来新增变体不会破坏现有代码
        Ok(_) => {}
        Err(e) => {
            eprintln!("更新失败: {e}");
        }
    }
}
