//! 跨平台更新接口。

use crate::core::diff::VerifiedArtifact;
use crate::error::Result;
use std::path::{Path, PathBuf};

/// Linux 平台适配。
#[cfg(target_os = "linux")]
pub mod linux;
/// macOS 平台适配。
#[cfg(target_os = "macos")]
pub mod macos;
/// 各平台共用的辅助逻辑。
pub mod shared;
/// 平台标识（Tauri updater 的 target 名称）。
pub mod target;
/// macOS / Linux 共用的替换脚本模板。
#[cfg(unix)]
pub mod unix_script;
/// Windows 平台适配。
#[cfg(target_os = "windows")]
pub mod windows;

/// 跨平台的更新器接口
pub struct PlatformUpdater;

impl PlatformUpdater {
    /// 生成当前平台的替换脚本。
    ///
    /// `new_exe` 必须是 [`VerifiedArtifact`]——只有通过双端哈希校验的产物
    /// 才能进入替换流程，未验证的字节在类型上无法到达这里。
    ///
    /// `work_dir` 存放脚本、日志与启动标志。它必须是用户可写目录
    /// （默认由 [`crate::default_work_dir`] 推导），不能是只读安装目录。
    pub fn generate_updater(
        current_exe: &Path,
        new_exe: &VerifiedArtifact,
        work_dir: &Path,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        return windows::generate_updater(current_exe, new_exe, work_dir);
        #[cfg(target_os = "macos")]
        return macos::generate_updater(current_exe, new_exe, work_dir);
        #[cfg(target_os = "linux")]
        return linux::generate_updater(current_exe, new_exe, work_dir);
        #[allow(unreachable_code)]
        {
            let _ = (current_exe, new_exe, work_dir);
            Err(crate::error::DeltaError::Config(
                "当前操作系统不支持增量替换".into(),
            ))
        }
    }

    /// 启动替换脚本。
    pub fn launch_updater(script_path: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        return windows::launch_updater(script_path);
        #[cfg(target_os = "macos")]
        return macos::launch_updater(script_path);
        #[cfg(target_os = "linux")]
        return linux::launch_updater(script_path);
        #[allow(unreachable_code)]
        {
            let _ = script_path;
            Err(crate::error::DeltaError::Config(
                "当前操作系统不支持增量替换".into(),
            ))
        }
    }

    /// 标记新版本启动成功（使用默认工作目录）。
    ///
    /// 只有通过 [`crate::UpdateConfig::work_dir`] 自定义了工作目录时，
    /// 才需要改用 [`Self::confirm_startup_success_in`]。
    pub fn confirm_startup_success() -> Result<()> {
        shared::confirm_startup_success()
    }

    /// 在指定工作目录标记新版本启动成功。
    ///
    /// 自定义 `work_dir` 时必须调用这个版本，否则替换脚本会在另一个目录
    /// 等不到标志而误判失败并回滚。
    pub fn confirm_startup_success_in(work_dir: &Path) -> Result<()> {
        shared::confirm_startup_success_in(work_dir)
    }
}
