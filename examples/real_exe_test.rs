//! 用真实可执行文件验证增量更新与静默替换流程。
//!
//! 需要 `real_test/` 目录下存在 `Sereno_0.1.0.exe` 与 `Sereno_0.1.1.exe`。
//! 准备方式：把任意两个相邻构建版本重命名为上述文件名放进 `real_test/`；
//! 该目录被 `.gitignore` 忽略，不会进入发布包。

use std::path::PathBuf;
use std::time::Instant;
use tauri_delta_updater::{
    apply_patch, calculate_hash, cleanup_previous_updates, create_patch, PlatformUpdater,
    UpdateLock,
};
use tokio::fs;

#[tokio::main]
async fn main() {
    let test_dir = PathBuf::from("real_test");
    let old_exe = test_dir.join("Sereno_0.1.0.exe");
    let new_exe = test_dir.join("Sereno_0.1.1.exe");
    let patch_file = test_dir.join("real_update.patch");
    let staged_new_exe = test_dir.join("staged_new.exe");

    println!("[1] 获取更新锁...");
    let lock_path = test_dir.join(".update.lock");
    let _lock = UpdateLock::acquire(&lock_path).expect("获取锁失败");

    println!("[2] 清理历史垃圾...");
    cleanup_previous_updates(&test_dir, &old_exe).await;

    println!("[3] 正在对比真实主程序，生成补丁...");
    let start = Instant::now();
    create_patch(&old_exe, &new_exe, &patch_file).await.unwrap();
    let patch_size = fs::metadata(&patch_file).await.unwrap().len();

    println!("    耗时: {:?}", start.elapsed());
    println!("    补丁体积: {:.2} KB", patch_size as f64 / 1024.0);

    println!("[4] 模拟客户端合并补丁...");
    let patch_hash = calculate_hash(&patch_file).await.unwrap();
    let target_hash = calculate_hash(&new_exe).await.unwrap();

    let artifact = apply_patch(
        &old_exe,
        &patch_file,
        &staged_new_exe,
        &patch_hash,
        &target_hash,
    )
    .await
    .unwrap();
    println!("    合并成功，文件校验通过！");

    let _ = fs::remove_file(&patch_file).await;

    println!("[5] 准备启动静默替换...");
    println!("    ⚠️ 接下来，你的 Sereno_0.1.0.exe 将被替换为 0.1.1 版本！");

    let script_path = PlatformUpdater::generate_updater(&old_exe, &artifact, &test_dir).unwrap();
    PlatformUpdater::launch_updater(&script_path).unwrap();

    println!("    🚀 替换脚本已启动。主进程即将退出。");
    println!("    请盯紧屏幕，看看启动器是不是自己弹出来了！");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    std::process::exit(0);
}
