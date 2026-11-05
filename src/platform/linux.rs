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
    crate::platform::unix_script::generate(current_exe, new_exe, work_dir, std::process::id(), true)
}

/// 与 [`generate_updater`] 相同，但显式指定要等待退出的 PID（供测试使用）。
pub fn generate_updater_for_pid(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    crate::platform::unix_script::generate(current_exe, new_exe, work_dir, old_pid, true)
}

/// 启动替换脚本。
pub use crate::platform::unix_script::launch_updater;

/// 标记新版本启动成功。
pub use crate::platform::shared::confirm_startup_success;
