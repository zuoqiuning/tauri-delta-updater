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

/// 替换脚本日志（单行化）：断言失败时把它带进 panic 信息，
/// CI 上无需下载 Actions 原始日志也能看到脚本实际做了什么。
fn script_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::LOG_FILE))
        .unwrap_or_else(|_| "<无日志>".into())
        .replace(['\r', '\n'], " | ")
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

/// 健康检查失败时，宿主写回滚原因，脚本必须立即回滚而不是等满确认超时。
#[tokio::test]
async fn test_script_rolls_back_on_health_failure() {
    let dir = tempdir_path();
    let plain = dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let current_exe = plain.join("app.exe");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = plain.join("app_new.exe");
    let artifact = make_verified(&plain, &current_exe, b"NEW", &staged).await;

    let script = tauri_delta_updater::platform::windows::generate_updater_for_pid(
        &current_exe,
        &artifact,
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    assert!(
        wait_until(Duration::from_secs(30), || std::fs::read(&current_exe)
            .map(|c| c == b"NEW")
            .unwrap_or(false)),
        "新版本必须先完成替换"
    );
    // 脚本在启动新版本前会清掉旧回滚标记并写 staged；等它进入等待确认阶段再模拟宿主失败。
    assert!(
        wait_until(Duration::from_secs(30), || {
            tauri_delta_updater::core::startup::read(&dir)
                == Some(tauri_delta_updater::StartupState::Staged)
        }),
        "脚本必须进入等待启动确认阶段"
    );

    // 模拟新版本启动后健康检查失败：宿主写回滚原因。
    std::fs::write(
        dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG),
        tauri_delta_updater::core::artifacts::HEALTH_CHECK_FAILED,
    )
    .unwrap();

    assert!(
        wait_until(Duration::from_secs(30), || std::fs::read(&current_exe)
            .map(|c| c == b"OLD")
            .unwrap_or(false)),
        "健康检查失败后脚本必须立即回滚到旧版本"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG))
            .unwrap(),
        tauri_delta_updater::core::artifacts::HEALTH_CHECK_FAILED
    );
    assert_eq!(
        tauri_delta_updater::core::startup::read(&dir),
        Some(tauri_delta_updater::StartupState::RolledBack)
    );
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

/// 端到端：修复脚本删除本次新增文件、还原备份并重启旧版本。
#[tokio::test]
async fn test_bundle_repair_restores_partial_update() {
    let dir = tempdir_path();
    let install = dir.join("install");
    let backup = dir.join(tauri_delta_updater::core::artifacts::BUNDLE_BACKUP_DIR);

    // 安装目录停在“替换到一半”：主程序与资源是新版本，extra.txt 是新文件。
    std::fs::create_dir_all(install.join("resources")).unwrap();
    std::fs::write(install.join("app.exe"), b"NEW").unwrap();
    std::fs::write(install.join("resources/a.txt"), b"NEWR").unwrap();
    std::fs::write(install.join("extra.txt"), b"ADDED").unwrap();

    // 备份目录只保存被覆盖/删除的旧文件；新增文件靠日志识别。
    std::fs::create_dir_all(backup.join("resources")).unwrap();
    std::fs::write(backup.join("app.exe"), b"OLD").unwrap();
    std::fs::write(backup.join("resources/a.txt"), b"OLDR").unwrap();
    std::fs::write(
        dir.join(tauri_delta_updater::core::artifacts::BUNDLE_JOURNAL_FILE),
        b"add extra.txt\n",
    )
    .unwrap();

    let script = tauri_delta_updater::platform::windows::generate_bundle_repair_for_pid(
        &install.join("app.exe"),
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    let log = dir.join(tauri_delta_updater::core::artifacts::LOG_FILE);
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read(install.join("app.exe"))
                .map(|c| c == b"OLD")
                .unwrap_or(false)
        }),
        "主程序必须被还原成旧版本；日志: {}",
        script_log(&dir)
    );
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(&log)
                .map(|text| text.contains("修复完成"))
                .unwrap_or(false)
        }),
        "修复脚本必须完成并记录日志"
    );
    assert_eq!(
        std::fs::read(install.join("resources/a.txt")).unwrap(),
        b"OLDR"
    );
    assert!(
        !install.join("extra.txt").exists(),
        "本次更新新增的文件必须被删除"
    );
    assert!(!backup.exists(), "还原后备份目录必须被移除");
    assert!(
        !dir.join(tauri_delta_updater::core::artifacts::BUNDLE_JOURNAL_FILE)
            .exists(),
        "还原后日志必须被移除"
    );
    let rolled_back =
        std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG))
            .expect("修复原因必须落盘");
    assert_eq!(rolled_back.trim(), "repaired_partial_update");
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

/// 端到端：整包脚本逐文件替换、删除新版本已移除的文件，并在成功后清理备份。
#[tokio::test]
async fn test_bundle_script_replaces_and_deletes_files() {
    use tauri_delta_updater::describe_dir;

    let dir = tempdir_path();
    let install = dir.join("install");
    let staging = dir.join("staging");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&staging).unwrap();

    // 旧安装目录：主程序、资源与一个新版本已移除的旧文件
    std::fs::write(install.join("app.exe"), b"OLD").unwrap();
    std::fs::create_dir_all(install.join("resources")).unwrap();
    std::fs::write(install.join("resources/app.js"), b"OLDR").unwrap();
    std::fs::write(install.join("legacy-only.txt"), b"REMOVE ME").unwrap();

    // 新版本：主程序与资源变化，新增一个文件，旧文件不再存在
    std::fs::write(staging.join("app.exe"), b"NEW").unwrap();
    std::fs::create_dir_all(staging.join("resources")).unwrap();
    std::fs::write(staging.join("resources/app.js"), b"NEWR").unwrap();
    std::fs::write(staging.join("resources/extra.txt"), b"EXTRA").unwrap();

    let base = describe_dir(&install).unwrap();
    let target = describe_dir(&staging).unwrap();

    let script = tauri_delta_updater::platform::windows::generate_bundle_updater_for_pid(
        &install.join("app.exe"),
        &staging,
        &base,
        &target,
        &dir,
        dead_pid(),
    )
    .unwrap();

    let raw = std::fs::read(&script).unwrap();
    assert!(
        raw.starts_with(&[0xEF, 0xBB, 0xBF]),
        "整包脚本也必须带 UTF-8 BOM"
    );
    let content = String::from_utf8(raw[3..].to_vec()).unwrap();
    assert!(content.contains("event=delta.rollback.triggered"));
    assert!(
        content.contains("legacy-only.txt"),
        "新版本移除的文件必须出现在删除清单里"
    );
    assert!(
        content.contains("Path = 'app.exe'"),
        "脚本必须包含逐文件替换清单"
    );

    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    // 脚本逐文件复制；只等主程序会与后续文件产生竞态，必须等全部就位。
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read(install.join("app.exe"))
                .map(|c| c == b"NEW")
                .unwrap_or(false)
                && std::fs::read(install.join("resources/app.js"))
                    .map(|c| c == b"NEWR")
                    .unwrap_or(false)
                && std::fs::read(install.join("resources/extra.txt"))
                    .map(|c| c == b"EXTRA")
                    .unwrap_or(false)
                && !install.join("legacy-only.txt").exists()
        }),
        "全部文件必须被替换，且新版本移除的文件必须被删除"
    );

    // 写入启动成功标志，脚本才会保留新版本并清理备份
    std::fs::write(
        dir.join(tauri_delta_updater::core::artifacts::SUCCESS_FLAG),
        "ok",
    )
    .unwrap();

    let backup = dir.join(tauri_delta_updater::core::artifacts::BUNDLE_BACKUP_DIR);
    assert!(
        wait_until(Duration::from_secs(30), || !backup.exists()),
        "启动成功后备份目录必须被清理"
    );
    assert!(
        wait_until(Duration::from_secs(30), || !staging.exists()),
        "启动成功后暂存目录必须被清理"
    );
}

/// 端到端：替换中途失败时，已经改写的文件必须被整体回滚。
#[tokio::test]
async fn test_bundle_script_rolls_back_when_copy_fails() {
    use tauri_delta_updater::describe_dir;

    let dir = tempdir_path();
    let install = dir.join("install");
    let staging = dir.join("staging");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&staging).unwrap();

    std::fs::write(install.join("app.exe"), b"OLD").unwrap();
    // 目标位置被一个目录占住：复制到它必然失败，用来触发替换中途回滚。
    std::fs::create_dir_all(install.join("resources/bad.txt")).unwrap();

    std::fs::write(staging.join("app.exe"), b"NEW").unwrap();
    std::fs::create_dir_all(staging.join("resources")).unwrap();
    std::fs::write(staging.join("resources/bad.txt"), b"NEWBAD").unwrap();

    let base = describe_dir(&install).unwrap();
    let target = describe_dir(&staging).unwrap();

    let script = tauri_delta_updater::platform::windows::generate_bundle_updater_for_pid(
        &install.join("app.exe"),
        &staging,
        &base,
        &target,
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    let log = dir.join(tauri_delta_updater::core::artifacts::LOG_FILE);
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(&log)
                .map(|text| text.contains("替换失败，正在回滚"))
                .unwrap_or(false)
        }),
        "复制失败必须触发回滚"
    );
    // 日志先于回滚写入，等文件真正恢复后再断言。
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read(install.join("app.exe"))
                .map(|c| c == b"OLD")
                .unwrap_or(false)
        }),
        "已经替换成 NEW 的主程序必须被回滚成旧内容；日志: {}",
        script_log(&dir)
    );
    assert!(
        wait_until(Duration::from_secs(30), || {
            !dir.join(tauri_delta_updater::core::artifacts::BUNDLE_BACKUP_DIR)
                .exists()
        }),
        "还原后备份目录必须被清理"
    );
    // 备份目录清理与回滚原因落盘之间可能有毫秒级时序差，等待文件可读再断言。
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG))
                .is_ok()
        }),
        "回滚原因必须落盘供下次启动诊断"
    );
    let rolled_back =
        std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::ROLLBACK_FLAG))
            .expect("回滚原因必须可读");
    assert_eq!(rolled_back.trim(), "update_failed");

    let events =
        std::fs::read_to_string(dir.join(tauri_delta_updater::core::artifacts::EVENTS_FILE))
            .expect("回滚必须同时写入结构化事件流");
    let value: serde_json::Value =
        serde_json::from_str(events.lines().last().expect("至少一条事件")).unwrap();
    assert_eq!(value["event"], "delta.rollback.triggered");
    assert_eq!(value["reason"], "update_failed");
}

/// 端到端：暂存文件被篡改时，脚本在改写任何文件前中止并保持旧版本可用。
#[tokio::test]
async fn test_bundle_script_rejects_tampered_staging() {
    use tauri_delta_updater::describe_dir;

    let dir = tempdir_path();
    let install = dir.join("install");
    let staging = dir.join("staging");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&staging).unwrap();

    std::fs::write(install.join("app.exe"), b"OLD").unwrap();
    std::fs::write(staging.join("app.exe"), b"NEW").unwrap();

    let base = describe_dir(&install).unwrap();
    let target = describe_dir(&staging).unwrap();

    // 生成清单后再篡改暂存文件：脚本必须自己重新校验，而不是信任清单
    std::fs::write(staging.join("app.exe"), b"TAMPERED").unwrap();

    let script = tauri_delta_updater::platform::windows::generate_bundle_updater_for_pid(
        &install.join("app.exe"),
        &staging,
        &base,
        &target,
        &dir,
        dead_pid(),
    )
    .unwrap();
    tauri_delta_updater::platform::windows::launch_updater(&script).unwrap();

    let log = dir.join(tauri_delta_updater::core::artifacts::LOG_FILE);
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_to_string(&log)
                .map(|text| text.contains("暂存文件哈希校验失败"))
                .unwrap_or(false)
        }),
        "篡改的暂存文件必须被脚本拒绝"
    );
    assert_eq!(
        std::fs::read(install.join("app.exe")).unwrap(),
        b"OLD",
        "校验失败时不得改动安装目录"
    );
}

/// 配置里的等待时长必须真的写进脚本。
///
/// 此前脚本把 60 秒写死，`UpdateConfig::confirm_timeout_secs` 是个无效开关：
/// 启动慢的应用会被误判回滚，想加速回滚的宿主也做不到。
#[tokio::test]
async fn test_script_honors_configured_timeouts() {
    let dir = tempdir_path();
    let current_exe = dir.join("app.exe");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("app-new.exe");
    let artifact = make_verified(&dir, &current_exe, b"NEW", &staged).await;

    let script = tauri_delta_updater::platform::windows::generate_updater_for_pid_with(
        &current_exe,
        &artifact,
        &dir,
        999_999,
        tauri_delta_updater::ScriptTimings {
            wait_secs: 7,
            confirm_secs: 11,
        },
    )
    .unwrap();

    let content = std::fs::read_to_string(&script).unwrap();
    assert!(
        content.contains("$waitTimeout  = 7"),
        "脚本必须使用配置的旧进程等待时长，实际脚本未包含 7"
    );
    assert!(
        content.contains("$confirmTimeout = 11"),
        "脚本必须使用配置的启动确认时长，实际脚本未包含 11"
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

/// 模板外置后的回归守卫：任何漏替换的占位符都会让脚本在真机上静默失效。
///
/// 三种脚本（单文件 / 整包 / 修复）都必须把 `__TOKEN__` 全部换成字面量。
fn assert_no_placeholder(script: &Path) {
    let content = std::fs::read_to_string(script).unwrap();
    let leftover: Vec<&str> = content.lines().filter(|line| line.contains("__")).collect();
    assert!(
        leftover.is_empty(),
        "{} 仍有未替换的占位符: {:?}",
        script.display(),
        leftover
    );
}

#[test]
fn test_generated_scripts_have_no_placeholders() {
    let dir = tempdir_path();
    let current_exe = dir.join("app.exe");
    std::fs::write(&current_exe, b"OLD").unwrap();
    let staged = dir.join("app-new.exe");
    let artifact = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(make_verified(&dir, &current_exe, b"NEW", &staged));

    let updater = tauri_delta_updater::platform::windows::generate_updater_for_pid(
        &current_exe,
        &artifact,
        &dir,
        999_999,
    )
    .unwrap();
    assert_no_placeholder(&updater);

    let staging = dir.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("app.exe"), b"NEW").unwrap();
    let files = tauri_delta_updater::describe_dir(&staging).unwrap();
    let bundle = tauri_delta_updater::platform::windows::generate_bundle_updater_for_pid(
        &current_exe,
        &staging,
        &files,
        &files,
        &dir,
        999_999,
    )
    .unwrap();
    assert_no_placeholder(&bundle);

    let repair = tauri_delta_updater::platform::windows::generate_bundle_repair_for_pid(
        &current_exe,
        &dir,
        999_999,
    )
    .unwrap();
    assert_no_placeholder(&repair);
}
