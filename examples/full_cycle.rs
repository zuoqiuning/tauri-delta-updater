//! 完整流程示例：启动确认 + 带进度安装。
//!
//! 两个关键点：
//! 1. 每次启动都调用 `confirm_startup_success_if_pending()`——普通启动零写入；
//! 2. `install_with_progress` 返回 UI 语义的 `UpdateResult`，无需理解补丁细节。

use tauri_delta_updater::{DeltaUpdater, UpdateResult};

#[tokio::main]
async fn main() {
    // 1. 启动确认：只有上一次确实替换过新版本时才会写标志
    match tauri_delta_updater::PlatformUpdater::confirm_startup_success_if_pending() {
        Ok(true) => println!("已确认上次更新启动成功"),
        Ok(false) => {}
        Err(e) => eprintln!("启动确认失败: {e}"),
    }

    // 2. 初始化（平台、工作目录、HTTP 客户端自动推导）
    let updater = match DeltaUpdater::auto(
        "https://cdn.example.com/latest.json",
        "BASE64_PUBLIC_KEY",
        env!("CARGO_PKG_VERSION"),
    ) {
        Ok(updater) => updater,
        Err(e) => {
            eprintln!("初始化失败: {e}");
            return;
        }
    };

    // 3. 带进度安装
    match updater
        .install_with_progress(|downloaded, total| {
            if total > 0 {
                println!("下载进度: {downloaded}/{total}");
            }
        })
        .await
    {
        Ok(UpdateResult::Updated { version }) => {
            println!("已更新到 {version}，退出进程以完成替换");
            std::process::exit(0);
        }
        Ok(UpdateResult::UpToDate { version }) => println!("已是最新版本 {version}"),
        Ok(UpdateResult::RequiresFullDownload { url, reason }) => {
            println!("增量不可用（{reason}），请走全量下载: {url:?}");
        }
        Ok(UpdateResult::SafeMode) => println!("连续失败已达上限，请提示用户手动安装"),
        // UpdateResult 是 non_exhaustive 的：将来新增变体不会破坏现有代码
        Ok(_) => {}
        Err(e) => eprintln!("更新失败: {e}"),
    }
}
