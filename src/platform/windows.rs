use crate::core::artifacts;
use crate::core::diff::VerifiedArtifact;
use crate::error::{DeltaError, Result};
use std::path::{Path, PathBuf};

use crate::platform::shared::{file_name, ps_literal};
use crate::platform::ScriptTimings;

/// 生成 Windows 平台的可执行文件替换脚本（PowerShell）。
///
/// 脚本流程：
/// 1. 必要时请求管理员权限
/// 2. 等待旧进程真正退出（按 PID + 路径校验）
/// 3. 校验暂存文件哈希（封住"校验完成到脚本执行"之间的替换窗口）
/// 4. 把当前程序备份为 `.bak`
/// 5. 把新程序复制到当前程序名并启动（提权时降权到登录用户）
/// 6. 等待新程序写入启动成功标志，超时则回滚到旧版本并重新拉起
///
/// # 为什么用 PowerShell 而不是批处理
///
/// 批处理在这几个场景下都会出错，且都是实测复现的：
///
/// - cmd 按**字节**偏移读取批处理文件，文件里有 UTF-8 中文时偏移会错位，
///   表现为后续命令被截断并报"不是内部或外部命令"。
/// - `cmd /c "<路径>"` 无法处理含 `&` `(` `%` 的路径，四种写法实测只有一种可用。
/// - 批处理删除自身后 cmd 仍在读该文件，读取随即错乱。
/// - `cmd` 的 `set "VAR=值"` 与 `%VAR%` 展开规则组合起来，转义极易出错。
///
/// PowerShell 用 `-LiteralPath` 处理路径（不做通配符与转义解释），
/// 字符串用单引号字面量（只需把 `'` 转义成 `''`），没有任何这些坑。
///
/// # 脚本文件的编码
///
/// 必须是 **UTF-8 带 BOM**：Windows PowerShell 5.1 依赖 BOM 识别 UTF-8，
/// 无 BOM 时会按系统 ANSI 代码页解析，中文会变成乱码并导致语法错误。
pub fn generate_updater(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
) -> Result<PathBuf> {
    generate_updater_with_timings(current_exe, new_exe, work_dir, ScriptTimings::default())
}

/// 与 [`generate_updater`] 相同，但使用配置里的等待时长（`UpdateConfig`）。
pub fn generate_updater_with_timings(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    generate_updater_for_pid_with(current_exe, new_exe, work_dir, std::process::id(), timings)
}

/// 与 [`generate_updater`] 相同，但显式指定"要等待退出的进程 PID"。
///
/// 正常使用时 PID 就是当前进程；单独暴露出来是为了让脚本行为可被端到端测试
/// 覆盖——测试需要一个已经退出的 PID，而不是测试进程自己。
pub fn generate_updater_for_pid(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    generate_updater_for_pid_with(
        current_exe,
        new_exe,
        work_dir,
        old_pid,
        ScriptTimings::default(),
    )
}

/// [`generate_updater_for_pid`] 的等待时长可调版本。
pub fn generate_updater_for_pid_with(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到父目录".into()))?;
    let target_exe = exe_dir.join(file_name(current_exe)?);

    let script_path = work_dir.join(artifacts::updater_script());
    let success_flag = work_dir.join(artifacts::SUCCESS_FLAG);
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let rollback_flag = work_dir.join(artifacts::ROLLBACK_FLAG);
    let backup = exe_dir.join(format!("{}.bak", file_name(current_exe)?));

    // 旧的成功标志必须清掉，否则脚本会误判新版本已启动
    let _ = std::fs::remove_file(&success_flag);

    let template = include_str!("windows/updater.ps1");

    let script = template
        .replace("__TARGET__", &ps_literal(&target_exe.display().to_string()))
        .replace(
            "__STAGED__",
            &ps_literal(&new_exe.path().display().to_string()),
        )
        .replace("__STAGED_HASH__", &ps_literal(new_exe.sha256()))
        .replace("__FLAG__", &ps_literal(&success_flag.display().to_string()))
        .replace("__LOG__", &ps_literal(&log_file.display().to_string()))
        .replace(
            "__ROLLBACK__",
            &ps_literal(&rollback_flag.display().to_string()),
        )
        .replace(
            "__EVENTS__",
            &ps_literal(&work_dir.join(artifacts::EVENTS_FILE).display().to_string()),
        )
        .replace(
            "__STATE__",
            &ps_literal(
                &work_dir
                    .join(artifacts::UPDATE_STATE_FILE)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &timings.wait_secs.to_string())
        .replace("__CONFIRM_TIMEOUT__", &timings.confirm_secs.to_string())
        .replace("__WORK_DIR__", &ps_literal(&work_dir.display().to_string()))
        .replace(
            "__MARKER__",
            &ps_literal(
                &exe_dir
                    .join(artifacts::WORK_DIR_MARKER)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__SELF__", &ps_literal(&script_path.display().to_string()))
        .replace("__BACKUP__", &ps_literal(&backup.display().to_string()));
    crate::platform::shared::assert_rendered(&script);

    // UTF-8 BOM：Windows PowerShell 5.1 靠它识别 UTF-8
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&script_path, bytes).map_err(DeltaError::Io)?;

    Ok(script_path)
}

/// 生成多文件（整包）替换脚本。
///
/// 与单文件脚本的区别：按文件清单逐项备份、替换与删除，任一文件失败
/// 都会整体回滚到备份目录里的旧内容。只复制与旧版本不同的文件，
/// 避免每次更新都重写整个 `resources` 目录。
pub fn generate_bundle_updater(
    current_exe: &Path,
    staging_dir: &Path,
    base_files: &[crate::core::bundle::BundleFile],
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
) -> Result<PathBuf> {
    generate_bundle_updater_with_timings(
        current_exe,
        staging_dir,
        base_files,
        target_files,
        work_dir,
        ScriptTimings::default(),
    )
}

/// 与 [`generate_bundle_updater`] 相同，但使用配置里的等待时长。
pub fn generate_bundle_updater_with_timings(
    current_exe: &Path,
    staging_dir: &Path,
    base_files: &[crate::core::bundle::BundleFile],
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    generate_bundle_updater_for_pid_with(
        current_exe,
        staging_dir,
        base_files,
        target_files,
        work_dir,
        std::process::id(),
        timings,
    )
}

/// 与 [`generate_bundle_updater`] 相同，但显式指定要等待退出的 PID（供测试使用）。
pub fn generate_bundle_updater_for_pid(
    current_exe: &Path,
    staging_dir: &Path,
    base_files: &[crate::core::bundle::BundleFile],
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    generate_bundle_updater_for_pid_with(
        current_exe,
        staging_dir,
        base_files,
        target_files,
        work_dir,
        old_pid,
        ScriptTimings::default(),
    )
}

/// [`generate_bundle_updater_for_pid`] 的等待时长可调版本。
pub fn generate_bundle_updater_for_pid_with(
    current_exe: &Path,
    staging_dir: &Path,
    base_files: &[crate::core::bundle::BundleFile],
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    let exe_name = file_name(current_exe)?;
    // 新版本清单必须包含主程序本身，否则脚本重启后还是旧版本，
    // 表现为"更新成功但版本没变"——这种错误必须在生成脚本前拦住。
    if !target_files.iter().any(|file| file.path == exe_name) {
        return Err(DeltaError::Config(format!(
            "整包清单里没有主程序 {exe_name}，无法启动新版本"
        )));
    }

    let install_dir = current_exe
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到父目录".into()))?;
    let target_exe = install_dir.join(&exe_name);

    let script_path = work_dir.join(artifacts::bundle_updater_script());
    let success_flag = work_dir.join(artifacts::SUCCESS_FLAG);
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let rollback_flag = work_dir.join(artifacts::ROLLBACK_FLAG);
    let backup_dir = work_dir.join(artifacts::BUNDLE_BACKUP_DIR);
    let journal_file = work_dir.join(artifacts::BUNDLE_JOURNAL_FILE);

    let _ = std::fs::remove_file(&success_flag);

    let (copies, deletes) = bundle_plan(base_files, target_files);

    let copy_entries = copies
        .iter()
        .map(|file| {
            format!(
                "    @{{ Path = {}; Hash = {} }}",
                ps_literal(&file.path),
                ps_literal(&file.sha256.to_lowercase())
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let delete_entries = deletes
        .iter()
        .map(|path| format!("    {}", ps_literal(path)))
        .collect::<Vec<_>>()
        .join(",\n");

    let template = include_str!("windows/bundle.ps1");

    let script = template
        .replace("__TARGET__", &ps_literal(&target_exe.display().to_string()))
        .replace(
            "__STAGING__",
            &ps_literal(&staging_dir.display().to_string()),
        )
        .replace("__BACKUP__", &ps_literal(&backup_dir.display().to_string()))
        .replace("__FLAG__", &ps_literal(&success_flag.display().to_string()))
        .replace("__LOG__", &ps_literal(&log_file.display().to_string()))
        .replace(
            "__ROLLBACK__",
            &ps_literal(&rollback_flag.display().to_string()),
        )
        .replace(
            "__EVENTS__",
            &ps_literal(&work_dir.join(artifacts::EVENTS_FILE).display().to_string()),
        )
        .replace(
            "__STATE__",
            &ps_literal(
                &work_dir
                    .join(artifacts::UPDATE_STATE_FILE)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &timings.wait_secs.to_string())
        .replace("__CONFIRM_TIMEOUT__", &timings.confirm_secs.to_string())
        .replace("__WORK_DIR__", &ps_literal(&work_dir.display().to_string()))
        .replace(
            "__MARKER__",
            &ps_literal(
                &install_dir
                    .join(artifacts::WORK_DIR_MARKER)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__COPIES__", &copy_entries)
        .replace("__DELETES__", &delete_entries)
        .replace(
            "__JOURNAL__",
            &ps_literal(&journal_file.display().to_string()),
        )
        .replace("__SELF__", &ps_literal(&script_path.display().to_string()));
    crate::platform::shared::assert_rendered(&script);

    // UTF-8 BOM：Windows PowerShell 5.1 靠它识别 UTF-8
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&script_path, bytes).map_err(DeltaError::Io)?;

    Ok(script_path)
}

/// 生成修复脚本：把上次中断的整包替换还原到旧版本。
///
/// 与更新脚本不同，修复脚本只做两件事：删除本次更新新增的文件、
/// 还原备份目录里的旧文件，然后重新启动旧版本。
pub fn generate_bundle_repair(current_exe: &Path, work_dir: &Path) -> Result<PathBuf> {
    generate_bundle_repair_with_timings(current_exe, work_dir, ScriptTimings::default())
}

/// 与 [`generate_bundle_repair`] 相同，但使用配置里的等待时长。
pub fn generate_bundle_repair_with_timings(
    current_exe: &Path,
    work_dir: &Path,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    generate_bundle_repair_for_pid_with(current_exe, work_dir, std::process::id(), timings)
}

/// 与 [`generate_bundle_repair`] 相同，但显式指定要等待退出的 PID（供测试使用）。
pub fn generate_bundle_repair_for_pid(
    current_exe: &Path,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    generate_bundle_repair_for_pid_with(current_exe, work_dir, old_pid, ScriptTimings::default())
}

/// [`generate_bundle_repair_for_pid`] 的等待时长可调版本。
pub fn generate_bundle_repair_for_pid_with(
    current_exe: &Path,
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到父目录".into()))?;
    let target_exe = install_dir.join(file_name(current_exe)?);
    let backup_dir = work_dir.join(artifacts::BUNDLE_BACKUP_DIR);
    let journal_file = work_dir.join(artifacts::BUNDLE_JOURNAL_FILE);
    let script_path = work_dir.join(artifacts::bundle_repair_script());
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let rollback_flag = work_dir.join(artifacts::ROLLBACK_FLAG);

    let template = include_str!("windows/repair.ps1");

    let script = template
        .replace("__TARGET__", &ps_literal(&target_exe.display().to_string()))
        .replace("__BACKUP__", &ps_literal(&backup_dir.display().to_string()))
        .replace(
            "__JOURNAL__",
            &ps_literal(&journal_file.display().to_string()),
        )
        .replace("__LOG__", &ps_literal(&log_file.display().to_string()))
        .replace(
            "__ROLLBACK__",
            &ps_literal(&rollback_flag.display().to_string()),
        )
        .replace(
            "__EVENTS__",
            &ps_literal(&work_dir.join(artifacts::EVENTS_FILE).display().to_string()),
        )
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &timings.wait_secs.to_string())
        .replace("__WORK_DIR__", &ps_literal(&work_dir.display().to_string()))
        .replace("__SELF__", &ps_literal(&script_path.display().to_string()));
    crate::platform::shared::assert_rendered(&script);

    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&script_path, bytes).map_err(DeltaError::Io)?;

    Ok(script_path)
}

/// 计算需要复制与删除的文件。
///
/// 只复制内容有变化的文件；旧版本独有的文件必须删除，否则残留的
/// sidecar 可能被新版本误加载。
fn bundle_plan(
    base_files: &[crate::core::bundle::BundleFile],
    target_files: &[crate::core::bundle::BundleFile],
) -> (Vec<crate::core::bundle::BundleFile>, Vec<String>) {
    let base_by_path: std::collections::HashMap<&str, _> = base_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let target_paths: std::collections::HashSet<&str> =
        target_files.iter().map(|file| file.path.as_str()).collect();

    let copies = target_files
        .iter()
        .filter(|file| match base_by_path.get(file.path.as_str()) {
            Some(base) => {
                base.size != file.size || base.sha256.to_lowercase() != file.sha256.to_lowercase()
            }
            None => true,
        })
        .cloned()
        .collect();

    let deletes = base_files
        .iter()
        .filter(|file| !target_paths.contains(file.path.as_str()))
        .map(|file| file.path.clone())
        .collect();

    (copies, deletes)
}

/// 启动替换脚本。
///
/// 用 `powershell -File <脚本名>` 并把工作目录设为脚本所在目录：
/// 这样路径里即使有空格或特殊字符也不会被命令行解析出问题。
pub fn launch_updater(script_path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let work_dir = script_path
            .parent()
            .ok_or_else(|| DeltaError::PatchApplyFailed("找不到脚本所在目录".into()))?;
        let script_name = file_name(script_path)?;

        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script_name)
            .current_dir(work_dir);
        // 不弹出黑色控制台窗口
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.spawn().map_err(DeltaError::Io)?;
    }
    Ok(())
}

/// 标记新版本启动成功。
pub use crate::platform::shared::{confirm_startup_success, confirm_startup_success_if_pending};
