//! Linux 平台适配。

use crate::core::diff::VerifiedArtifact;
use crate::error::Result;
use std::path::{Path, PathBuf};

/// 生成 Linux 替换脚本。
///
/// **提权**：通过 `.deb` 安装到 `/usr/bin` 的应用，普通用户没有写权限。
/// 脚本检测到目标目录不可写时优先用免密 `sudo` 重跑自己，失败则尝试 `pkexec`；
/// 提权完成后会把新版本**降权回原用户**启动，并把文件所有权交还。
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
        true,
        check_exe_link(),
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
        true,
        check_exe_link(),
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
        true,
        check_exe_link(),
        crate::platform::ScriptTimings::default(),
    )
}

/// AppImage 下 `/proc/<pid>/exe` 指向 squashfs 挂载点，与 `.AppImage` 本体不一致，
/// 不能作为"旧进程是否已退出"的判据。
fn check_exe_link() -> bool {
    crate::platform::target::appimage_target().is_none()
}

/// 启动替换脚本。
pub use crate::platform::unix_script::launch_updater;

/// 标记新版本启动成功。
pub use crate::platform::shared::{confirm_startup_success, confirm_startup_success_if_pending};
