//! macOS 平台适配。

use crate::core::diff::VerifiedArtifact;
use crate::error::Result;
use std::path::{Path, PathBuf};

/// 查找可执行文件所属的 `.app` bundle 根目录。
///
/// Tauri 在 macOS 上的产物是 `MyApp.app/Contents/MacOS/MyApp`。
/// 替换 bundle 内的二进制会破坏代码签名（只能 ad-hoc 重签），
/// 因此这类环境在 [`crate::incremental_fallback_reason`] 里直接回退全量。
///
/// 不在 bundle 内时返回 `None`。
pub fn bundle_root(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find_map(|dir| {
        let is_bundle = dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".app"));
        is_bundle.then(|| dir.to_path_buf())
    })
}

/// 判断可执行文件是否运行在 Gatekeeper 的 App Translocation 目录里。
///
/// 被隔离的应用从 DMG/下载目录直接启动时，系统会把 `.app` 复制到
/// `/private/var/folders/.../AppTranslocation/...` 再运行。更新那个副本
/// 既不会影响用户真正的应用，也会随系统清理消失，必须回退全量。
pub fn is_translocated(exe: &Path) -> bool {
    exe.ancestors().any(|dir| {
        dir.file_name()
            .is_some_and(|name| name == "AppTranslocation")
    })
}

/// 生成 macOS 替换脚本（bundle 外的裸二进制）。
pub fn generate_updater(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
) -> Result<PathBuf> {
    generate_updater_with_timings(
        current_exe,
        new_exe,
        work_dir,
        crate::platform::ScriptTimings::default(),
    )
}

/// 与 [`generate_updater`] 相同，但使用配置里的等待时长。
pub fn generate_updater_with_timings(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    timings: crate::platform::ScriptTimings,
) -> Result<PathBuf> {
    crate::platform::unix_script::generate(
        current_exe,
        new_exe,
        work_dir,
        std::process::id(),
        false,
        true,
        timings,
    )
}

/// 内置更新流程使用：回滚后是否提示用户。
pub(crate) fn generate_updater_with_notice(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    timings: crate::platform::ScriptTimings,
    notice: bool,
) -> Result<PathBuf> {
    crate::platform::unix_script::generate_with_notice(
        current_exe,
        new_exe,
        work_dir,
        std::process::id(),
        false,
        true,
        timings,
        notice,
    )
}

/// 与 [`generate_updater`] 相同，但显式指定要等待退出的 PID（供测试使用）。
pub fn generate_updater_for_pid(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    crate::platform::unix_script::generate(
        current_exe,
        new_exe,
        work_dir,
        old_pid,
        false,
        true,
        crate::platform::ScriptTimings::default(),
    )
}

/// 生成整包（`.app` 目录）替换脚本。
///
/// 逐文件改写会破坏代码签名，因此 macOS 上以整个 `.app` 为单位切换：
/// 旧包改名为备份，暂存目录整体移入，失败或启动超时则整体换回。
pub fn generate_bundle_updater(
    current_exe: &Path,
    staging_dir: &Path,
    _base_files: &[crate::core::bundle::BundleFile],
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
) -> Result<PathBuf> {
    generate_bundle_updater_with_timings(
        current_exe,
        staging_dir,
        target_files,
        work_dir,
        crate::platform::ScriptTimings::default(),
    )
}

/// 与 [`generate_bundle_updater`] 相同，但使用配置里的等待时长。
pub fn generate_bundle_updater_with_timings(
    current_exe: &Path,
    staging_dir: &Path,
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    timings: crate::platform::ScriptTimings,
) -> Result<PathBuf> {
    generate_bundle_updater_for_pid_with(
        current_exe,
        staging_dir,
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
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    generate_bundle_updater_for_pid_with(
        current_exe,
        staging_dir,
        target_files,
        work_dir,
        old_pid,
        crate::platform::ScriptTimings::default(),
    )
}

/// [`generate_bundle_updater_for_pid`] 的等待时长可调版本。
pub fn generate_bundle_updater_for_pid_with(
    current_exe: &Path,
    staging_dir: &Path,
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    old_pid: u32,
    timings: crate::platform::ScriptTimings,
) -> Result<PathBuf> {
    let app_root = bundle_root(current_exe).ok_or_else(|| {
        crate::error::DeltaError::Config("macOS 整包替换只支持位于 .app 内的主程序".into())
    })?;
    crate::platform::dir_swap::generate(
        &app_root,
        staging_dir,
        current_exe,
        target_files,
        work_dir,
        old_pid,
        timings,
    )
}

/// 内置更新流程使用：回滚后是否提示用户。
pub(crate) fn generate_bundle_updater_with_notice(
    current_exe: &Path,
    staging_dir: &Path,
    target_files: &[crate::core::bundle::BundleFile],
    work_dir: &Path,
    timings: crate::platform::ScriptTimings,
    notice: bool,
) -> Result<PathBuf> {
    let app_root = bundle_root(current_exe).ok_or_else(|| {
        crate::error::DeltaError::Config("macOS 整包替换只支持位于 .app 内的主程序".into())
    })?;
    crate::platform::dir_swap::generate_with_notice(
        &app_root,
        staging_dir,
        current_exe,
        target_files,
        work_dir,
        std::process::id(),
        timings,
        notice,
    )
}

/// 整包切换的隐藏备份目录：`.<App>.app.tdx-backup`。
pub fn bundle_backup(app_root: &Path) -> Option<PathBuf> {
    let name = app_root.file_name()?;
    Some(app_root.with_file_name(format!(".{}.tdx-backup", name.to_string_lossy())))
}

/// 生成整包修复脚本。
pub fn generate_bundle_repair(current_exe: &Path, work_dir: &Path) -> Result<PathBuf> {
    generate_bundle_repair_with_timings(
        current_exe,
        work_dir,
        crate::platform::ScriptTimings::default(),
    )
}

/// 与 [`generate_bundle_repair`] 相同，但使用配置里的等待时长。
pub fn generate_bundle_repair_with_timings(
    current_exe: &Path,
    work_dir: &Path,
    timings: crate::platform::ScriptTimings,
) -> Result<PathBuf> {
    generate_bundle_repair_for_pid_with(current_exe, work_dir, std::process::id(), timings)
}

/// 与 [`generate_bundle_repair`] 相同，但显式指定要等待退出的 PID（供测试使用）。
pub fn generate_bundle_repair_for_pid(
    current_exe: &Path,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    generate_bundle_repair_for_pid_with(
        current_exe,
        work_dir,
        old_pid,
        crate::platform::ScriptTimings::default(),
    )
}

/// [`generate_bundle_repair_for_pid`] 的等待时长可调版本。
pub fn generate_bundle_repair_for_pid_with(
    current_exe: &Path,
    work_dir: &Path,
    old_pid: u32,
    timings: crate::platform::ScriptTimings,
) -> Result<PathBuf> {
    let app_root = bundle_root(current_exe).ok_or_else(|| {
        crate::error::DeltaError::Config("macOS 整包修复只支持位于 .app 内的主程序".into())
    })?;
    crate::platform::dir_swap::generate_repair(&app_root, current_exe, work_dir, old_pid, timings)
}

/// 删除整包切换遗留的 `.app.tdx-backup`。
///
/// 新版本确认启动成功时调用，等于给"切换已完成"盖章：
/// 替换脚本此后看到备份仍在，就能确定上一轮切换没有完成并安全回滚。
pub fn cleanup_bundle_backup() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(root) = bundle_root(&exe) else {
        return;
    };
    if let Some(backup) = bundle_backup(&root) {
        let _ = std::fs::remove_dir_all(backup);
    }
}

/// 启动替换脚本。
pub use crate::platform::unix_script::launch_updater;

/// 标记新版本启动成功。
pub use crate::platform::shared::{confirm_startup_success, confirm_startup_success_if_pending};

#[cfg(test)]
mod tests {
    use super::is_translocated;
    use std::path::Path;

    #[test]
    fn detects_app_translocation() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/x/AppTranslocation/abc/d/My.app/Contents/MacOS/My"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/My.app/Contents/MacOS/My"
        )));
    }
}
