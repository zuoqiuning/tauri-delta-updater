//! 跨平台更新接口。

use crate::core::diff::VerifiedArtifact;
use crate::error::Result;
use std::path::{Path, PathBuf};

/// Windows Authenticode 校验（更新产物是否会因未签名而在企业环境被拒启动）。
#[cfg(target_os = "windows")]
pub mod authenticode;
/// macOS `.app` 的目录切换脚本生成（纯字符串逻辑，跨平台编译）。
pub mod dir_swap;
/// 全量安装包执行器（安装器启动、类型识别）。
pub mod full_install;
/// Linux 平台适配。
#[cfg(target_os = "linux")]
pub mod linux;
/// macOS 平台适配。
#[cfg(target_os = "macos")]
pub mod macos;
/// 系统版本探测与最低版本比较。
pub mod os_version;
/// 各平台共用的辅助逻辑。
pub mod shared;
/// 平台标识（Tauri updater 的 target 名称）。
pub mod target;
/// macOS / Linux 共用的替换脚本模板（纯字符串逻辑，跨平台编译以便测试）。
pub mod unix_script;
/// Windows 平台适配。
#[cfg(target_os = "windows")]
pub mod windows;

/// 替换脚本的等待时长（秒）。
///
/// 来自 [`crate::UpdateConfig::wait_timeout_secs`] /
/// [`crate::UpdateConfig::confirm_timeout_secs`]，由脚本在运行时执行：
/// 旧进程退出等待与"新版本启动成功标志"等待。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptTimings {
    /// 等待旧进程退出的最长秒数。
    pub wait_secs: u32,
    /// 等待新版本写入启动成功标志的最长秒数。
    pub confirm_secs: u32,
}

impl Default for ScriptTimings {
    fn default() -> Self {
        Self {
            wait_secs: 60,
            confirm_secs: 60,
        }
    }
}

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
        Self::generate_updater_with(current_exe, new_exe, work_dir, ScriptTimings::default())
    }

    /// 与 [`Self::generate_updater`] 相同，但可指定等待时长。
    pub fn generate_updater_with(
        current_exe: &Path,
        new_exe: &VerifiedArtifact,
        work_dir: &Path,
        timings: ScriptTimings,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        return windows::generate_updater_with_timings(current_exe, new_exe, work_dir, timings);
        #[cfg(target_os = "macos")]
        return macos::generate_updater_with_timings(current_exe, new_exe, work_dir, timings);
        #[cfg(target_os = "linux")]
        return linux::generate_updater_with_timings(current_exe, new_exe, work_dir, timings);
        #[allow(unreachable_code)]
        {
            let _ = (current_exe, new_exe, work_dir, timings);
            Err(crate::error::DeltaError::Config(
                "当前操作系统不支持增量替换".into(),
            ))
        }
    }

    /// 内置更新流程使用：生成替换脚本，并决定回滚后是否提示用户。
    ///
    /// 与 [`Self::generate_updater_with`] 的差别只有提示开关；直接调用
    /// [`Self::generate_updater`] 的宿主自行掌控界面，默认不弹提示。
    pub(crate) fn generate_updater_with_notice(
        current_exe: &Path,
        new_exe: &VerifiedArtifact,
        work_dir: &Path,
        timings: ScriptTimings,
        notice: bool,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        return windows::generate_updater_with_notice(
            current_exe,
            new_exe,
            work_dir,
            timings,
            notice,
        );
        #[cfg(target_os = "macos")]
        return macos::generate_updater_with_notice(
            current_exe,
            new_exe,
            work_dir,
            timings,
            notice,
        );
        #[cfg(target_os = "linux")]
        return linux::generate_updater_with_notice(
            current_exe,
            new_exe,
            work_dir,
            timings,
            notice,
        );
        #[allow(unreachable_code)]
        {
            let _ = (current_exe, new_exe, work_dir, timings, notice);
            Err(crate::error::DeltaError::Config(
                "当前操作系统不支持增量替换".into(),
            ))
        }
    }

    /// 校验更新产物没有发生"签名退化"。
    ///
    /// Windows 上比较原版本与产物的 Authenticode 状态：
    /// **原版本可信而产物不可信**时拒绝更新（企业环境会拒绝启动未签名程序）。
    /// 原版本本身未签名（开发构建、便携应用）时不做额外要求。
    ///
    /// 其它平台目前是零操作：macOS `.app` 由替换脚本在切换前执行
    /// `codesign --verify --deep --strict`，Linux 不依赖 Authenticode。
    pub fn verify_patched_binary(original: &Path, staged: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            if authenticode::is_trusted(original) && !authenticode::is_trusted(staged) {
                return Err(crate::error::DeltaError::AuthenticodeUnsigned);
            }
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (original, staged);
            Ok(())
        }
    }

    /// 指定可执行文件所在的环境是否支持整包替换。
    pub fn bundle_supported_for(current_exe: &Path) -> bool {
        Self::bundle_unsupported_reason(current_exe).is_none()
    }

    /// 整包替换不可用的具体原因；可用时返回 `None`。
    ///
    /// 把原因做成结构化值而不是一个布尔：UI 能直接告诉用户
    /// “目录不可写”还是“运行在临时目录”，而不是笼统地说“不支持”。
    pub fn bundle_unsupported_reason(current_exe: &Path) -> Option<&'static str> {
        #[cfg(target_os = "windows")]
        {
            let _ = current_exe;
            None
        }
        #[cfg(target_os = "macos")]
        {
            if macos::is_translocated(current_exe) {
                return Some("应用运行在 App Translocation 临时目录，更新只会写入系统副本");
            }
            match macos::bundle_root(current_exe) {
                Some(root) => {
                    if root
                        .parent()
                        .map(crate::core::paths::is_writable)
                        .unwrap_or(false)
                    {
                        None
                    } else {
                        Some("应用目录不可写，无法以目录为单位切换 .app")
                    }
                }
                None => Some("macOS 整包替换只支持位于 .app 内的主程序"),
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let _ = current_exe;
            Some("整包增量替换目前仅支持 Windows 与 macOS .app")
        }
    }

    /// 整包替换的安装根目录。
    ///
    /// Windows 是主程序所在目录；macOS 是整个 `.app` 目录（清单里的相对路径
    /// 也以 `.app` 为根）。
    pub fn bundle_install_root(current_exe: &Path) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            return current_exe.parent().map(Path::to_path_buf).ok_or_else(|| {
                crate::error::DeltaError::PatchApplyFailed("找不到可执行文件所在目录".into())
            });
        }
        #[cfg(target_os = "macos")]
        {
            return macos::bundle_root(current_exe)
                .or_else(|| current_exe.parent().map(Path::to_path_buf))
                .ok_or_else(|| {
                    crate::error::DeltaError::PatchApplyFailed("找不到可执行文件所在目录".into())
                });
        }
        #[allow(unreachable_code)]
        {
            let _ = current_exe;
            Err(crate::error::DeltaError::Config(
                "整包增量替换目前仅支持 Windows 与 macOS .app".into(),
            ))
        }
    }

    /// 是否存在上次中断、尚未还原的整包替换现场。
    pub fn bundle_repair_needed(current_exe: &Path, work_dir: &Path) -> bool {
        // 启动状态已 committed：替换已经完成并确认健康。此时残留的备份/journal
        // 只可能是“写成功标志”与“删除备份”之间崩溃留下的，
        // 据此回滚会把一个健康的新版本降回旧版本。
        if crate::core::startup::read(work_dir)
            == Some(crate::core::startup::StartupState::Committed)
        {
            return false;
        }
        #[cfg(target_os = "windows")]
        {
            let _ = current_exe;
            work_dir
                .join(crate::core::artifacts::BUNDLE_BACKUP_DIR)
                .is_dir()
                || work_dir
                    .join(crate::core::artifacts::BUNDLE_JOURNAL_FILE)
                    .is_file()
        }
        #[cfg(target_os = "macos")]
        {
            // macOS 按 .app 目录切换，不需要工作目录里的整包日志文件
            let _ = work_dir;
            macos::bundle_root(current_exe)
                .and_then(|root| macos::bundle_backup(&root))
                .map(|backup| backup.is_dir())
                .unwrap_or(false)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let _ = (current_exe, work_dir);
            false
        }
    }

    /// 生成当前平台的整包修复脚本（还原上次中断的替换）。
    pub fn generate_bundle_repair(current_exe: &Path, work_dir: &Path) -> Result<PathBuf> {
        Self::generate_bundle_repair_with(current_exe, work_dir, ScriptTimings::default())
    }

    /// 与 [`Self::generate_bundle_repair`] 相同，但可指定等待时长。
    pub fn generate_bundle_repair_with(
        current_exe: &Path,
        work_dir: &Path,
        timings: ScriptTimings,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        return windows::generate_bundle_repair_with_timings(current_exe, work_dir, timings);
        #[cfg(target_os = "macos")]
        return macos::generate_bundle_repair_with_timings(current_exe, work_dir, timings);
        #[allow(unreachable_code)]
        {
            let _ = (current_exe, work_dir, timings);
            Err(crate::error::DeltaError::Config(
                "当前平台不支持整包修复".into(),
            ))
        }
    }

    /// 生成当前平台的多文件（整包）替换脚本。
    ///
    /// `base_files` 是客户端当前的安装布局，`target_files` 是暂存目录里的
    /// 新布局；实现只替换有变化的文件，并删除新版本已移除的文件。
    pub fn generate_bundle_updater(
        current_exe: &Path,
        staging_dir: &Path,
        base_files: &[crate::core::bundle::BundleFile],
        target_files: &[crate::core::bundle::BundleFile],
        work_dir: &Path,
    ) -> Result<PathBuf> {
        Self::generate_bundle_updater_with(
            current_exe,
            staging_dir,
            base_files,
            target_files,
            work_dir,
            ScriptTimings::default(),
        )
    }

    /// 与 [`Self::generate_bundle_updater`] 相同，但可指定等待时长。
    pub fn generate_bundle_updater_with(
        current_exe: &Path,
        staging_dir: &Path,
        base_files: &[crate::core::bundle::BundleFile],
        target_files: &[crate::core::bundle::BundleFile],
        work_dir: &Path,
        timings: ScriptTimings,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        return windows::generate_bundle_updater_with_timings(
            current_exe,
            staging_dir,
            base_files,
            target_files,
            work_dir,
            timings,
        );
        // macOS 以整个 .app 为单位切换，不需要逐文件比较新旧布局
        #[cfg(target_os = "macos")]
        {
            let _ = base_files;
            return macos::generate_bundle_updater_with_timings(
                current_exe,
                staging_dir,
                target_files,
                work_dir,
                timings,
            );
        }
        #[allow(unreachable_code)]
        {
            let _ = (
                current_exe,
                staging_dir,
                base_files,
                target_files,
                work_dir,
                timings,
            );
            Err(crate::error::DeltaError::Config(
                "整包增量替换目前仅支持 Windows 与 macOS .app".into(),
            ))
        }
    }

    /// 内置更新流程使用：生成整包替换脚本，并决定回滚后是否提示用户。
    pub(crate) fn generate_bundle_updater_with_notice(
        current_exe: &Path,
        staging_dir: &Path,
        base_files: &[crate::core::bundle::BundleFile],
        target_files: &[crate::core::bundle::BundleFile],
        work_dir: &Path,
        timings: ScriptTimings,
        notice: bool,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        return windows::generate_bundle_updater_with_notice(
            current_exe,
            staging_dir,
            base_files,
            target_files,
            work_dir,
            timings,
            notice,
        );
        // macOS 以整个 .app 为单位切换，不需要逐文件比较新旧布局
        #[cfg(target_os = "macos")]
        {
            let _ = base_files;
            return macos::generate_bundle_updater_with_notice(
                current_exe,
                staging_dir,
                target_files,
                work_dir,
                timings,
                notice,
            );
        }
        #[allow(unreachable_code)]
        {
            let _ = (
                current_exe,
                staging_dir,
                base_files,
                target_files,
                work_dir,
                timings,
                notice,
            );
            Err(crate::error::DeltaError::Config(
                "整包增量替换目前仅支持 Windows 与 macOS .app".into(),
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
        shared::confirm_startup_success()?;
        #[cfg(target_os = "macos")]
        macos::cleanup_bundle_backup();
        Ok(())
    }

    /// 仅当本次启动来自一次替换时才确认成功；普通启动零写入。
    ///
    /// 可无条件放在应用启动早期，返回 `true` 表示本次确实完成了一次更新确认。
    pub fn confirm_startup_success_if_pending() -> Result<bool> {
        let confirmed = shared::confirm_startup_success_if_pending()?;
        #[cfg(target_os = "macos")]
        if confirmed {
            macos::cleanup_bundle_backup();
        }
        Ok(confirmed)
    }

    /// 带健康检查的启动确认（默认工作目录）。
    ///
    /// 只有 `health` 返回 `Ok` 才写成功标志；失败时立刻写
    /// `health_check_failed` 回滚原因，替换脚本会尽快回滚。
    ///
    /// ```no_run
    /// # fn main() -> tauri_delta_updater::Result<()> {
    /// tauri_delta_updater::PlatformUpdater::confirm_startup_success_with(|| {
    ///     // 宿主的健康检查：数据库可连接、配置可加载……
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn confirm_startup_success_with<F>(health: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        shared::confirm_startup_success_with(health)?;
        #[cfg(target_os = "macos")]
        macos::cleanup_bundle_backup();
        Ok(())
    }

    /// 在指定工作目录标记新版本启动成功。
    ///
    /// 自定义 `work_dir` 时必须调用这个版本，否则替换脚本会在另一个目录
    /// 等不到标志而误判失败并回滚。
    pub fn confirm_startup_success_in(work_dir: &Path) -> Result<()> {
        shared::confirm_startup_success_in(work_dir)?;
        #[cfg(target_os = "macos")]
        macos::cleanup_bundle_backup();
        Ok(())
    }
}
