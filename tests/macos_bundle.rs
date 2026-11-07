//! macOS `.app` 整包替换脚本的端到端测试。
//!
//! 只在 macOS 上运行（CI 的 macos-latest 覆盖）。脚本会真实执行，
//! 因此覆盖目录切换、崩溃恢复与回滚，而不只是字符串断言。
//! 其它平台整体跳过，不影响本机构建。

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    condition()
}

/// 替换脚本日志（单行化）：断言失败时带进 panic 信息，
/// CI 上不必下载 Actions 原始日志也能看到脚本实际做了什么。
fn script_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::LOG_FILE))
        .unwrap_or_else(|_| "<无日志>".into())
        .replace(['\r', '\n'], " | ")
}

fn write_file(root: &Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// 一个不会撞上真实进程的 PID。
fn dead_pid() -> u32 {
    0x7FFF_FFFE
}

fn backup_of(app: &Path) -> PathBuf {
    app.with_file_name(format!(
        ".{}.tdx-backup",
        app.file_name().unwrap().to_string_lossy()
    ))
}

fn tempdir_path() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tdx-macos-bundle-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// 成功路径：暂存目录整体换入，备份在启动确认后清理。
#[tokio::test]
async fn macos_bundle_swaps_app_directory() {
    use tauri_delta_updater::describe_dir;

    let dir = tempdir_path();
    let app = dir.join("My App.app");
    let staging = dir.join("staging");
    let flag = dir.join(tauri_delta_updater::core::artifacts::SUCCESS_FLAG);

    write_file(&app, "Contents/MacOS/MyApp", b"#!/bin/sh\nexit 0\n");
    write_file(&app, "Contents/Resources/a.txt", b"OLD");
    // 新版本主程序写入启动成功标志，模拟真实的 confirm_startup_success
    let new_exe = format!("#!/bin/sh\nprintf ok > '{}'\n", flag.display());
    write_file(&staging, "Contents/MacOS/MyApp", new_exe.as_bytes());
    write_file(&staging, "Contents/Resources/a.txt", b"NEW");
    set_executable(&app.join("Contents/MacOS/MyApp"));
    set_executable(&staging.join("Contents/MacOS/MyApp"));

    let files = describe_dir(&staging).unwrap();
    let script = tauri_delta_updater::platform::macos::generate_bundle_updater_for_pid(
        &app.join("Contents/MacOS/MyApp"),
        &staging,
        &files,
        &dir,
        dead_pid(),
    )
    .unwrap();

    tauri_delta_updater::PlatformUpdater::launch_updater(&script).unwrap();

    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read(app.join("Contents/Resources/a.txt"))
                .map(|c| c == b"NEW")
                .unwrap_or(false)
        }),
        "暂存目录必须被整体换入；日志: {}",
        script_log(&dir)
    );
    assert_eq!(
        std::fs::read_to_string(app.join("Contents/MacOS/MyApp")).unwrap(),
        new_exe
    );
    assert!(
        wait_until(Duration::from_secs(30), || !staging.exists()),
        "切换后暂存目录应消失（已被移动，而不是复制）"
    );
    assert!(
        wait_until(Duration::from_secs(30), || !backup_of(&app).exists()),
        "启动确认后备份必须被清理"
    );
}

/// 修复：上次切换中断后，整包备份被还原并重新启动旧版本。
#[tokio::test]
async fn macos_bundle_repair_restores_backup() {
    let dir = tempdir_path();
    let app = dir.join("MyApp.app");
    let backup = backup_of(&app);

    write_file(&app, "Contents/Resources/a.txt", b"PARTIAL NEW");
    write_file(&backup, "Contents/MacOS/MyApp", b"#!/bin/sh\nexit 0\n");
    write_file(&backup, "Contents/Resources/a.txt", b"OLD");

    let script = tauri_delta_updater::platform::macos::generate_bundle_repair_for_pid(
        &app.join("Contents/MacOS/MyApp"),
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::PlatformUpdater::launch_updater(&script).unwrap();

    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read(app.join("Contents/Resources/a.txt"))
                .map(|c| c == b"OLD")
                .unwrap_or(false)
        }),
        "整包备份必须被整体还原"
    );
    assert!(!backup.exists(), "还原后备份目录必须消失");
    // 备份还原与回滚原因落盘之间有时序差：日志/目录先就位，flag 随后写入。
    let flag = dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG);
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(&flag)
                .map(|text| text.trim() == "repaired_partial_update")
                .unwrap_or(false)
        }),
        "修复原因必须落盘"
    );
}

/// 失败路径：暂存文件被篡改时在切换前中止，旧包保持完整。
#[tokio::test]
async fn macos_bundle_rejects_tampered_staging() {
    use tauri_delta_updater::describe_dir;

    let dir = tempdir_path();
    let app = dir.join("MyApp.app");
    let staging = dir.join("staging");

    write_file(&app, "Contents/MacOS/MyApp", b"#!/bin/sh\nexit 0\n");
    write_file(&app, "Contents/Resources/a.txt", b"OLD");
    write_file(&staging, "Contents/MacOS/MyApp", b"#!/bin/sh\nexit 0\n");
    write_file(&staging, "Contents/Resources/a.txt", b"NEW");
    set_executable(&app.join("Contents/MacOS/MyApp"));
    set_executable(&staging.join("Contents/MacOS/MyApp"));

    let files = describe_dir(&staging).unwrap();
    // 生成清单后篡改暂存文件：脚本必须自己重新校验
    write_file(&staging, "Contents/Resources/a.txt", b"TAMPERED");

    let script = tauri_delta_updater::platform::macos::generate_bundle_updater_for_pid(
        &app.join("Contents/MacOS/MyApp"),
        &staging,
        &files,
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::PlatformUpdater::launch_updater(&script).unwrap();

    let log = dir.join(tauri_delta_updater::core::artifacts::LOG_FILE);
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(&log)
                .map(|text| text.contains("暂存目录校验失败"))
                .unwrap_or(false)
        }),
        "篡改的暂存目录必须被拒绝；日志: {}",
        script_log(&dir)
    );
    assert_eq!(
        std::fs::read(app.join("Contents/Resources/a.txt")).unwrap(),
        b"OLD",
        "校验失败时不得改动旧 .app"
    );
    // 脚本先写日志再落盘回滚原因，读 flag 必须等待而不是立即断言。
    let flag = dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG);
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(&flag)
                .map(|text| text.trim() == "update_failed")
                .unwrap_or(false)
        }),
        "回滚原因必须落盘供下次启动诊断"
    );
}
