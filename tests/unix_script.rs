//! macOS/Linux 替换脚本的端到端测试。
//!
//! 只在 Unix 上运行（CI 的 ubuntu/macos 覆盖）。与 `windows_script.rs` 同一思路：
//! **真正执行**生成的 `.sh`，覆盖字符串断言抓不到的问题——
//! 进程等待、备份/替换时序、成功标志判定。
//!
//! 替换脚本只接受 `VerifiedArtifact`（类型级防护），因此每个测试
//! 都先走一遍真实的补丁应用流程来产出它。
//! 测试不涉及 `codesign`/`xattr`（bundle 已在运行时边界自动回退）。

#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};
use tauri_delta_updater::{
    apply_patch, calculate_hash, create_patch, PlatformUpdater, VerifiedArtifact,
};

/// 等待条件成立，最多等 `timeout`。
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

/// 用真实的补丁应用流程构造已验证产物（内容 = `new_content`）。
async fn make_verified(
    work_dir: &Path,
    base: &Path,
    new_content: &[u8],
    staged: &Path,
) -> VerifiedArtifact {
    let new_src = work_dir.join(".new_src.bin");
    std::fs::write(&new_src, new_content).unwrap();
    let patch = work_dir.join(".patch.bin");
    create_patch(base, &new_src, &patch).await.unwrap();
    let patch_hash = calculate_hash(&patch).await.unwrap();
    let target_hash = calculate_hash(&new_src).await.unwrap();
    let artifact = apply_patch(base, &patch, staged, &patch_hash, &target_hash)
        .await
        .unwrap();
    let _ = std::fs::remove_file(&new_src);
    let _ = std::fs::remove_file(&patch);
    artifact
}

/// 用一个已经退出的 PID 生成脚本。
///
/// 脚本按 PID 等待旧进程退出；若用测试进程自己的 PID，脚本会一直等到超时。
fn generate_script_with_dead_pid(
    current_exe: &Path,
    artifact: &VerifiedArtifact,
    work_dir: &Path,
) -> std::path::PathBuf {
    const DEAD_PID: u32 = 999_999;
    #[cfg(target_os = "linux")]
    {
        return tauri_delta_updater::platform::linux::generate_updater_for_pid(
            current_exe,
            artifact,
            work_dir,
            DEAD_PID,
        )
        .unwrap();
    }
    #[cfg(target_os = "macos")]
    {
        return tauri_delta_updater::platform::macos::generate_updater_for_pid(
            current_exe,
            artifact,
            work_dir,
            DEAD_PID,
        )
        .unwrap();
    }
    #[allow(unreachable_code)]
    {
        let _ = (current_exe, artifact, work_dir);
        unreachable!()
    }
}

/// 生成脚本并执行，验证完整替换流程。
async fn run_script_and_expect_replace(dir: &Path) {
    use tauri_delta_updater::core::artifacts;

    let current_exe = dir.join("tdx-app");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("tdx-app-new");

    let artifact = make_verified(dir, &current_exe, b"NEW", &staged).await;
    let script = generate_script_with_dead_pid(&current_exe, &artifact, dir);
    assert!(script.exists(), "脚本必须生成");

    // 执行脚本。用生产代码同一条启动路径，确保测的是真实行为
    PlatformUpdater::launch_updater(&script).unwrap();

    // 新文件被复制成当前文件名，脚本即进入等待启动确认阶段
    let replaced = wait_until(Duration::from_secs(30), || {
        std::fs::read(&current_exe)
            .map(|c| c == b"NEW")
            .unwrap_or(false)
    });
    assert!(replaced, "新版本必须被复制到当前文件名下");

    // 写入启动成功标志，脚本据此保留新版本并清理备份
    std::fs::write(dir.join(artifacts::SUCCESS_FLAG), "ok").unwrap();

    // 备份应被清理
    let backup = dir.join("tdx-app.bak");
    assert!(
        wait_until(Duration::from_secs(30), || !backup.exists()),
        "启动成功后备份必须被清理，实际仍存在: {}",
        backup.display()
    );

    // 新版本内容保持不变
    assert_eq!(std::fs::read(&current_exe).unwrap(), b"NEW");

    // 脚本不删除自身，等待清理逻辑回收
    assert!(script.exists(), "脚本执行完应保留在原处，等待清理逻辑回收");
}

/// 端到端：普通文件（非 bundle、无提权场景）下替换脚本正常工作。
#[tokio::test]
async fn test_unix_script_end_to_end() {
    let dir = tempdir_path();
    run_script_and_expect_replace(&dir).await;
}

/// 提权块必须带图形环境守卫：pkexec 在无 DISPLAY 的会话里会阻塞在
/// polkit agent 上而不是快速失败，无图形环境时必须直接记录日志退出。
///
/// 提权块只存在于 Linux 脚本（macOS 无需提权），因此仅 Linux 执行。
#[test]
#[cfg(target_os = "linux")]
fn test_unix_script_pkexec_requires_graphics_session() {
    let dir = tempdir_path();
    let current_exe = dir.join("tdx-app");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("tdx-app-new");

    // 生成脚本需要 VerifiedArtifact；用当前测试运行时驱动 async 块
    let script = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let artifact = make_verified(&dir, &current_exe, b"NEW", &staged).await;
            generate_script_with_dead_pid(&current_exe, &artifact, &dir)
        });
    let content = std::fs::read_to_string(&script).unwrap();

    // pkexec 只能在有图形会话时尝试，且必须用 timeout 防止永久挂起
    assert!(
        content.contains("[ -n \"${DISPLAY:-}\" ] || [ -n \"${WAYLAND_DISPLAY:-}\" ]"),
        "pkexec 前必须检查图形会话环境"
    );
    assert!(
        content.contains("timeout 30 pkexec"),
        "pkexec 必须用 timeout 包裹，防止无代理时挂起"
    );
    // 无图形环境时必须有明确的失败日志路径，而不是静默卡死
    assert!(
        content.contains("目标目录不可写且无法提权"),
        "无法提权时必须记录明确日志"
    );
    // 提权后必须降权回原用户启动，而不是让应用以 root 常驻。
    // 精简镜像可能没有 runuser，必须有 su 兜底与明确日志。
    assert!(
        content.contains("runuser -u \"$ORIG_USER\""),
        "提权后必须优先用 runuser 降权启动"
    );
    assert!(
        content.contains("su -s /bin/sh -c \"$TARGET_CMD\" \"$ORIG_USER\""),
        "缺少 runuser 时必须有 su 兜底"
    );
    assert!(
        content.contains("缺少 runuser/su"),
        "两种降权工具都缺失时必须有明确日志"
    );
    // 回滚必须写入可机读的结构化事件行，供宿主/支持人员 grep
    assert!(
        content.contains("event=delta.rollback.triggered"),
        "回滚必须写入结构化事件行"
    );
}

/// AppImage：目标是 `.AppImage` 本体，脚本不能用 `/proc/<pid>/exe` 作为
/// "旧进程是否退出"的判据——AppImage 下它指向 squashfs 挂载点，永远不会等于
/// 本体路径，开着会立即判定旧进程已退出并提前替换。
#[test]
#[cfg(target_os = "linux")]
fn test_appimage_script_skips_exe_link_check() {
    let dir = tempdir_path();
    let current_exe = dir.join("tdx-app.AppImage");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("tdx-app-new");

    let script = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let artifact = make_verified(&dir, &current_exe, b"NEW", &staged).await;
            tauri_delta_updater::platform::unix_script::generate(
                &current_exe,
                &artifact,
                &dir,
                999_999,
                true,
                false,
                tauri_delta_updater::ScriptTimings::default(),
            )
            .unwrap()
        });
    let content = std::fs::read_to_string(&script).unwrap();

    assert!(
        content.contains("CHECK_EXE_LINK=0"),
        "AppImage 必须关闭 /proc/<pid>/exe 判定"
    );
    assert!(
        content.contains("chmod +x \"$TARGET_EXE\""),
        "替换 .AppImage 后必须补回可执行位"
    );
    assert!(
        content.contains("tdx-app.AppImage.bak"),
        "备份必须是 .AppImage 本体旁边的文件"
    );
}

/// 复制中途失败时目标可能已经存在半截内容；回滚必须用备份覆盖它，
/// 而不是因为“目标存在”就把半截文件当旧版本启动。
#[tokio::test]
async fn test_unix_script_restores_backup_after_partial_copy() {
    let dir = tempdir_path();
    let current_exe = dir.join("tdx-app");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("tdx-app-new");

    let artifact = make_verified(&dir, &current_exe, b"NEW", &staged).await;
    let script = generate_script_with_dead_pid(&current_exe, &artifact, &dir);
    let content = std::fs::read_to_string(&script).unwrap();

    // 注入“复制写到一半后失败”，复现磁盘满 / I/O 错误留下的半截目标文件。
    let injected = content.replace(
        "if ! cp \"$STAGED\" \"$TARGET_EXE\"; then",
        "if ! (printf 'HALF' > \"$TARGET_EXE\"; false); then",
    );
    assert_ne!(injected, content, "测试必须成功注入复制失败");
    std::fs::write(&script, injected).unwrap();

    PlatformUpdater::launch_updater(&script).unwrap();

    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read(&current_exe)
                .map(|c| c == b"OLD")
                .unwrap_or(false)
        }),
        "半截目标必须被备份覆盖还原: {}",
        current_exe.display()
    );
}

/// 生成一个临时目录。
fn tempdir_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tdx-unix-script-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
