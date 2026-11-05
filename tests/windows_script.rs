//! Windows 替换脚本的端到端测试。
//!
//! 只在 Windows 上运行。这些测试会**真正执行**生成的 `.ps1`，
//! 因此能覆盖纯字符串断言抓不到的问题：
//! 路径转义、含特殊字符的安装目录、文件编码、脚本自删。
//!
//! 替换脚本只接受 `VerifiedArtifact`（类型级防护），因此每个测试
//! 都先走一遍真实的补丁应用流程来产出它。
//!
//! 跨平台构建时本文件整体跳过，不影响其它平台。

#![cfg(windows)]

use std::path::Path;
use std::time::{Duration, Instant};
use tauri_delta_updater::{apply_patch, calculate_hash, create_patch, VerifiedArtifact};

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

/// 造一个含特殊字符的安装目录。
///
/// 这些字符在 Windows 路径里都是合法的，但在 cmd / PowerShell 里各有特殊含义，
/// 任何一条转义规则写错都会让脚本当场失效。
fn make_special_dir(base: &Path) -> std::path::PathBuf {
    let dir = base.join("weird & dir (x) 50% off! bang");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 用真实的补丁应用流程构造已验证产物（内容 = `new_content`）。
///
/// `base` 是补丁基底（内容为旧的文件），`staged` 是产物落点（必须不存在）。
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

/// 生成脚本并执行，验证"新文件被替换成当前文件名"。
///
/// 脚本会等待旧进程退出、备份旧文件、复制新文件、启动新版本，
/// 然后等待启动成功标志。测试里用一个已经退出的 PID，
/// 并在观察到文件被替换后写入成功标志，让脚本走完正常流程。
async fn run_script_and_expect_replace(dir: &Path, old_pid: u32) {
    let current_exe = dir.join("app&old!.exe");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("app&new!.exe");

    let artifact = make_verified(dir, &current_exe, b"NEW", &staged).await;

    let script = tauri_delta_updater::platform::windows::generate_updater_for_pid(
        &current_exe,
        &artifact,
        dir,
        old_pid,
    )
    .unwrap();
    assert!(script.exists(), "脚本必须生成");

    // 脚本必须是 UTF-8 带 BOM：无 BOM 时 PowerShell 5.1 会按 ANSI 代码页解析
    // 中文，导致语法错误
    let raw = std::fs::read(&script).unwrap();
    assert!(
        raw.starts_with(&[0xEF, 0xBB, 0xBF]),
        "脚本必须带 UTF-8 BOM，否则 PowerShell 会把中文解析成乱码"
    );

    // 执行脚本。用生产代码同一条启动路径，确保测的是真实行为
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    // 新文件被复制成当前文件名，脚本即进入等待启动确认阶段
    let replaced = wait_until(Duration::from_secs(30), || {
        std::fs::read(&current_exe)
            .map(|c| c == b"NEW")
            .unwrap_or(false)
    });
    assert!(replaced, "新版本必须被复制到当前文件名下");
    assert_ne!(
        std::fs::read(&current_exe).unwrap(),
        b"OLD",
        "旧内容必须被完全替换"
    );

    // 写入启动成功标志，脚本据此保留新版本并清理备份
    std::fs::write(
        dir.join(tauri_delta_updater::core::artifacts::SUCCESS_FLAG),
        "ok",
    )
    .unwrap();

    // 备份应被清理
    let backup = dir.join("app&old!.exe.bak");
    assert!(
        wait_until(Duration::from_secs(30), || !backup.exists()),
        "启动成功后备份必须被清理，实际仍存在: {}",
        backup.display()
    );

    // 新版本内容保持不变
    assert_eq!(std::fs::read(&current_exe).unwrap(), b"NEW");

    // 脚本不删除自身：残留脚本由下一次更新的清理逻辑回收
    // （与 cmd 时代的教训一致——执行期间删除正在读取的文件会引发竞态）。
    assert!(script.exists(), "脚本执行完应保留在原处，等待清理逻辑回收");
}

/// 已经在退出的进程 PID，用来让脚本的"等待旧进程退出"立即通过。
///
/// 用一个不存在的 PID：Windows 的 PID 空间很大，
/// 随便挑一个高位 PID 几乎不可能撞上真实进程。
/// 注意 PID 必须是 Int32 范围，否则 PowerShell 的 `-Id` 参数会直接报错。
fn dead_pid() -> u32 {
    0x7FFF_FFFE
}

/// 端到端：含特殊字符的安装目录下，替换脚本能正常工作。
#[tokio::test]
async fn test_script_end_to_end_with_special_chars() {
    let dir = tempdir_path();
    let special = make_special_dir(&dir);
    run_script_and_expect_replace(&special, dead_pid()).await;
}

/// 端到端：普通路径下同样能工作，防止特殊字符测试掩盖了通用问题。
#[tokio::test]
async fn test_script_end_to_end_plain_path() {
    let dir = tempdir_path();
    let plain = dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    run_script_and_expect_replace(&plain, dead_pid()).await;
}

/// 脚本内容必须包含关键指令，且不得使用已知会失效的写法。
#[tokio::test]
async fn test_script_contains_required_commands() {
    let dir = tempdir_path();
    let special = make_special_dir(&dir);
    let current_exe = special.join("app&old!.exe");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = special.join("app&new!.exe");

    let artifact = make_verified(&special, &current_exe, b"NEW", &staged).await;

    let script = tauri_delta_updater::platform::windows::generate_updater_for_pid(
        &current_exe,
        &artifact,
        &dir,
        dead_pid(),
    )
    .unwrap();
    let content = String::from_utf8(std::fs::read(&script).unwrap()[3..].to_vec()).unwrap();

    // 路径必须用 -LiteralPath：普通路径参数会做通配符与转义解释，
    // 含 `[` `]` `*` 的目录会直接匹配失败
    assert!(
        content.contains("-LiteralPath $target"),
        "替换必须使用 -LiteralPath"
    );

    // 按 PID 等待旧进程退出
    assert!(
        content.contains("Get-Process -Id $oldPid"),
        "必须按 PID 判定旧进程是否退出"
    );

    // 回滚时按路径结束进程，避免误杀同名应用
    assert!(
        content.contains("$_.Path -eq $target"),
        "回滚必须按可执行文件路径结束进程，而不是按进程名"
    );
    assert!(
        !content.contains("taskkill"),
        "不得按进程名批量结束，否则会误杀同名应用"
    );

    // 路径中的特殊字符必须原样保留
    assert!(
        content.contains("weird & dir (x) 50% off! bang"),
        "路径中的特殊字符必须原样写入"
    );
    assert!(
        content.contains("app&old!.exe"),
        "文件名中的特殊字符必须原样写入"
    );

    // 提权后必须降权：explorer.exe 代理只接收目标路径这一个参数。
    // 附加工作目录/开关会让 explorer 忽略目标，表现为"更新完成但没启动"。
    assert!(
        content.contains("Start-Process -FilePath 'explorer.exe'"),
        "提权路径必须经 explorer 以登录用户启动"
    );
    assert!(
        !content.contains("'explorer.exe' -WorkingDirectory"),
        "不得向 explorer 代理附加工作目录参数"
    );

    // 回滚必须写入可机读的结构化事件行，供宿主/支持人员 grep
    assert!(
        content.contains("event=delta.rollback.triggered"),
        "回滚必须写入结构化事件行"
    );
}

/// 端到端：上次更新中断（目标缺失 + 备份存在）时，脚本先还原备份再继续。
#[tokio::test]
async fn test_script_recovers_missing_target_from_backup() {
    let dir = tempdir_path();
    let current_exe = dir.join("app.exe");
    // 模拟中断现场：exe 已被改名为 .bak，目标不存在
    let backup = dir.join("app.exe.bak");
    std::fs::write(&backup, b"OLD").unwrap();
    let staged = dir.join("app_new.exe");

    // 补丁基底用备份文件（内容 OLD），产物内容为 NEW
    let artifact = make_verified(&dir, &backup, b"NEW", &staged).await;

    let script = tauri_delta_updater::platform::windows::generate_updater_for_pid(
        &current_exe,
        &artifact,
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    // 崩溃恢复是目标文件重新出现的唯一途径（初始不存在，且无恢复时脚本
    // 会在备份步骤直接失败退出）。
    //
    // ⚠️ 不要试图断言中间态：脚本执行快于本测试的轮询间隔，
    // "目标缺失 + 备份存在"的中间态几乎不可观测（实测初版因等不到
    // "还原后的 OLD 内容"而超时失败）。只断言终态——恢复成功则目标
    // 文件重新出现；恢复失败则目标永不出现、断言超时失败，这正是
    // 预期的失败模式。
    let recovered = wait_until(Duration::from_secs(30), || current_exe.exists());
    assert!(recovered, "目标缺失时脚本必须先用 .bak 还原目标文件");

    // 还原后脚本继续正常更新流程：写入标志让它走完，最终替换为 NEW 并清理备份
    std::fs::write(
        dir.join(tauri_delta_updater::core::artifacts::SUCCESS_FLAG),
        "ok",
    )
    .unwrap();
    let replaced = wait_until(Duration::from_secs(30), || {
        std::fs::read(&current_exe)
            .map(|c| c == b"NEW")
            .unwrap_or(false)
    });
    assert!(replaced, "恢复后的目标文件必须被新版本替换");
    assert!(
        wait_until(Duration::from_secs(30), || !dir
            .join("app.exe.bak")
            .exists()),
        "更新成功后备份必须被清理"
    );
}

/// 端到端：上次更新成功但清理失败（目标存在 + 备份存在）时，脚本不得误还原。
///
/// 若判据写成"备份存在就还原"，会把新版本回滚成旧版本。
#[tokio::test]
async fn test_script_does_not_restore_when_target_exists() {
    let dir = tempdir_path();
    let current_exe = dir.join("app.exe");
    // 模拟"已更新但 .bak 未清理"：目标是新版本，备份是旧版本
    let backup = dir.join("app.exe.bak");
    std::fs::write(&current_exe, b"NEW").unwrap();
    std::fs::write(&backup, b"OLD").unwrap();
    let staged = dir.join("app_new.exe");

    // 补丁基底用备份（OLD），产物内容 NEW2
    let artifact = make_verified(&dir, &backup, b"NEW2", &staged).await;

    let script = tauri_delta_updater::platform::windows::generate_updater_for_pid(
        &current_exe,
        &artifact,
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    // 脚本正常更新：目标先变 NEW2，旧备份在备份步骤被覆盖/清理，绝不回滚成 OLD
    let replaced = wait_until(Duration::from_secs(30), || {
        std::fs::read(&current_exe)
            .map(|c| c == b"NEW2")
            .unwrap_or(false)
    });
    assert!(replaced, "目标存在时脚本必须正常替换，而不是还原旧备份");
    assert_ne!(
        std::fs::read(&current_exe).unwrap(),
        b"OLD",
        "存在 .bak 时不得把已更新的新版本回滚成旧版本"
    );
}

/// 生成一个临时目录。
fn tempdir_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tdx-script-test-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
