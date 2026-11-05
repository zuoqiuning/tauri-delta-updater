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

/// 生成 macOS 替换脚本（bundle 外的裸二进制）。
pub fn generate_updater(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
) -> Result<PathBuf> {
    crate::platform::unix_script::generate(
        current_exe,
        new_exe,
        work_dir,
        std::process::id(),
        false,
    )
}

/// 与 [`generate_updater`] 相同，但显式指定要等待退出的 PID（供测试使用）。
pub fn generate_updater_for_pid(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    crate::platform::unix_script::generate(current_exe, new_exe, work_dir, old_pid, false)
}

/// 启动替换脚本。
pub use crate::platform::unix_script::launch_updater;

/// 标记新版本启动成功。
pub use crate::platform::shared::confirm_startup_success;
