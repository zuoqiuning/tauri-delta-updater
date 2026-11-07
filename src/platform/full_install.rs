//! 全量安装包的执行器。
//!
//! 增量路径替换的是"已安装的主程序/整包目录"，全量路径必须交给**安装器**：
//!
//! - Windows：NSIS `.exe`（静默 `/S`）或 MSI（`msiexec /qn`），需要提权；
//! - macOS：`.app.tar.gz`，解包后走与整包增量相同的目录切换脚本；
//! - Linux：AppImage 文件直接替换（复用单文件替换脚本，带备份与回滚）。
//!
//! 执行器返回后，当前进程必须退出：
//! - Windows 安装器要接管正在运行的可执行文件；
//! - macOS/Linux 的替换脚本在等待旧进程退出。

use crate::error::Result;
use crate::platform::ScriptTimings;
use std::path::Path;

/// 全量安装包的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerKind {
    /// Windows NSIS 安装器（`.exe`，静默参数 `/S`）。
    Nsis,
    /// Windows MSI 安装包。
    Msi,
    /// macOS 的 `.app.tar.gz`。
    MacAppTarGz,
    /// Linux AppImage。
    AppImage,
    /// 无法识别的类型（不会被执行）。
    Other,
}

/// 按文件名推断安装包类型。
pub fn installer_kind(path: &Path) -> InstallerKind {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.ends_with(".tar.gz") {
        InstallerKind::MacAppTarGz
    } else if name.ends_with(".appimage") {
        InstallerKind::AppImage
    } else if name.ends_with(".msi") {
        InstallerKind::Msi
    } else if name.ends_with(".exe") {
        InstallerKind::Nsis
    } else {
        InstallerKind::Other
    }
}

/// 执行全量安装。返回 `Ok(())` 后调用方应立即退出进程。
pub fn install(
    current_exe: &Path,
    installer: &Path,
    work_dir: &Path,
    timings: ScriptTimings,
) -> Result<()> {
    match installer_kind(installer) {
        InstallerKind::Nsis | InstallerKind::Msi => {
            #[cfg(target_os = "windows")]
            {
                let _ = (current_exe, work_dir, timings);
                windows::launch(installer)
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = (current_exe, work_dir, timings);
                Err(crate::error::DeltaError::FullInstallFailed(format!(
                    "{} 只能在 Windows 上执行",
                    installer.display()
                )))
            }
        }
        InstallerKind::MacAppTarGz => {
            #[cfg(target_os = "macos")]
            {
                macos::install_app(current_exe, installer, work_dir, timings)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (current_exe, work_dir, timings);
                Err(crate::error::DeltaError::FullInstallFailed(
                    "macOS .app.tar.gz 只能在 macOS 上执行".into(),
                ))
            }
        }
        InstallerKind::AppImage => {
            #[cfg(target_os = "linux")]
            {
                linux::install_appimage(current_exe, installer, work_dir, timings)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (current_exe, work_dir, timings);
                Err(crate::error::DeltaError::FullInstallFailed(
                    "AppImage 安装包只能在 Linux 上执行".into(),
                ))
            }
        }
        InstallerKind::Other => Err(crate::error::DeltaError::FullInstallFailed(format!(
            "不支持的全量安装包类型：{}",
            installer.display()
        ))),
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{installer_kind, InstallerKind};
    use crate::error::{DeltaError, Result};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// 转成 `ShellExecuteW` 需要的 UTF-16 以 null 结尾的字符串。
    fn wide(value: &OsStr) -> Vec<u16> {
        let mut buffer: Vec<u16> = value.encode_wide().collect();
        buffer.push(0);
        buffer
    }

    pub fn launch(installer: &Path) -> Result<()> {
        let kind = installer_kind(installer);
        let (file, params): (PathBuf, String) = match kind {
            // 与官方 updater 一致：被动模式（显示进度）+ `/UPDATE` 告诉 Tauri 的
            // NSIS 安装器这是更新而不是首次安装；`/R` 让安装完成后重新启动应用。
            InstallerKind::Nsis => (installer.to_path_buf(), nsis_parameters()),
            // MSI 用被动模式静默安装，`AUTOLAUNCHAPP` 让安装器重新启动应用。
            InstallerKind::Msi => (
                PathBuf::from("msiexec.exe"),
                format!("/i \"{}\" {}", installer.display(), msi_parameters()),
            ),
            other => {
                return Err(DeltaError::FullInstallFailed(format!(
                    "不支持的全量安装包类型（{other:?}）：{}",
                    installer.display()
                )))
            }
        };

        // 用 `open` 而不是 `runas`：安装器自己的 manifest 决定是否需要提权，
        // 这样按用户安装（per-user）不会无谓地弹 UAC，按机器安装仍会弹。
        let verb = wide(OsStr::new("open"));
        let file_w = wide(file.as_os_str());
        let params_w = wide(OsStr::new(&params));

        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file_w.as_ptr(),
                params_w.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };

        // 返回值 <= 32 是错误码（SE_ERR_ACCESSDENIED = 5 对应用户取消提权）。
        let code = result as isize;
        if code <= 32 {
            return Err(DeltaError::FullInstallFailed(format!(
                "启动安装器失败（ShellExecute 返回 {code}，5 表示用户取消提权）：{}",
                installer.display()
            )));
        }
        Ok(())
    }

    /// NSIS 参数：被动模式 + 更新标记 + 安装后重启应用。
    ///
    /// `/UPDATE` 是 Tauri NSIS 模板识别的更新标记；缺了它安装器会走首次安装流程。
    pub(super) fn nsis_parameters() -> String {
        "/P /UPDATE /R".to_string()
    }

    /// MSI 参数：被动模式 + 不自动重启系统 + 安装后启动应用。
    pub(super) fn msi_parameters() -> String {
        "/passive /promptrestart AUTOLAUNCHAPP=True".to_string()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use crate::core::bundle::describe_dir;
    use crate::error::{DeltaError, Result};
    use crate::platform::ScriptTimings;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// 解包 `.app.tar.gz` 并生成目录切换脚本。
    ///
    /// 与整包增量共用同一套脚本：校验暂存清单、`codesign --verify`、
    /// 目录改名换位、启动确认失败则整体回滚。
    pub fn install_app(
        current_exe: &Path,
        installer: &Path,
        work_dir: &Path,
        timings: ScriptTimings,
    ) -> Result<()> {
        let staging_root: PathBuf = work_dir.join(crate::core::artifacts::FULL_STAGING_DIR);
        let _ = std::fs::remove_dir_all(&staging_root);
        std::fs::create_dir_all(&staging_root).map_err(DeltaError::Io)?;

        // 用系统 tar（bsdtar）解包：不引入 Rust 解压依赖，也自动处理权限位与符号链接。
        let output = Command::new("tar")
            .arg("-xzf")
            .arg(installer)
            .arg("-C")
            .arg(&staging_root)
            .output()
            .map_err(|e| DeltaError::FullInstallFailed(format!("无法启动 tar: {e}")))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&staging_root);
            return Err(DeltaError::FullInstallFailed(format!(
                "解包 {} 失败: {}",
                installer.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let app_dir = find_app_bundle(&staging_root).ok_or_else(|| {
            let _ = std::fs::remove_dir_all(&staging_root);
            DeltaError::FullInstallFailed(format!("{} 里没有找到 .app 目录", installer.display()))
        })?;

        // 安装包不在这里删除：目录切换失败时它是唯一的恢复来源。
        // 成功后由 `confirm_startup_success` 的 `cleanup_full_installers` 回收。

        let files = describe_dir(&app_dir)?;
        let script = crate::platform::macos::generate_bundle_updater_with_timings(
            current_exe,
            &app_dir,
            &files,
            work_dir,
            timings,
        )?;
        crate::platform::PlatformUpdater::launch_updater(&script)?;
        Ok(())
    }

    /// 取暂存目录里第一个 `.app`（Tauri 的 `.app.tar.gz` 顶层就是 bundle）。
    fn find_app_bundle(root: &Path) -> Option<PathBuf> {
        std::fs::read_dir(root)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.is_dir()
                    && path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
            })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::error::Result;
    use crate::platform::ScriptTimings;
    use std::path::Path;

    /// 用单文件替换脚本安装 AppImage（备份、覆盖、`chmod +x`、启动确认、回滚）。
    pub fn install_appimage(
        current_exe: &Path,
        installer: &Path,
        work_dir: &Path,
        timings: ScriptTimings,
    ) -> Result<()> {
        let sha256 = crate::core::hasher::hash_file_sync(installer)?;
        // 安装包已经通过 minisign 验签；这里把哈希交给脚本做复制前后的复核。
        let artifact =
            crate::core::diff::VerifiedArtifact::from_signed_file(installer.to_path_buf(), sha256);
        let script = crate::platform::linux::generate_updater_with_timings(
            current_exe,
            &artifact,
            work_dir,
            timings,
        )?;
        crate::platform::PlatformUpdater::launch_updater(&script)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{install, installer_kind, InstallerKind};
    use crate::error::DeltaError;
    use crate::platform::ScriptTimings;
    use std::path::Path;

    #[test]
    fn kind_is_inferred_from_file_name() {
        assert_eq!(
            installer_kind(Path::new("MyApp_1.2.3_x64-setup.exe")),
            InstallerKind::Nsis
        );
        assert_eq!(
            installer_kind(Path::new("MyApp_1.2.3_x64_en-US.msi")),
            InstallerKind::Msi
        );
        assert_eq!(
            installer_kind(Path::new("MyApp_1.2.3_x64.tar.gz")),
            InstallerKind::MacAppTarGz
        );
        assert_eq!(
            installer_kind(Path::new("MyApp_1.2.3_x86_64.AppImage")),
            InstallerKind::AppImage
        );
        assert_eq!(installer_kind(Path::new("notes.txt")), InstallerKind::Other);
    }

    /// Windows 安装器参数必须与 Tauri 的安装器约定一致。
    ///
    /// 官方 updater 用的是被动模式 + `/UPDATE`；少了 `/UPDATE`，
    /// Tauri 的 NSIS 安装器会把更新当成首次安装，行为完全不同。
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_installer_arguments_match_tauri_conventions() {
        let nsis = super::windows::nsis_parameters();
        assert!(nsis.contains("/P"), "NSIS 必须用被动模式: {nsis}");
        assert!(nsis.contains("/UPDATE"), "NSIS 必须带更新标记: {nsis}");
        assert!(nsis.contains("/R"), "NSIS 安装后必须重启应用: {nsis}");

        let msi = super::windows::msi_parameters();
        assert!(msi.contains("/passive"), "MSI 必须用被动模式: {msi}");
        assert!(
            msi.contains("AUTOLAUNCHAPP=True"),
            "MSI 安装后必须启动应用: {msi}"
        );
    }

    /// 未知类型绝不执行：宁可报错，也不要把任意文件丢进 ShellExecute。
    #[test]
    fn unknown_kind_is_rejected_without_execution() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("payload.txt");
        std::fs::write(&file, b"not an installer").unwrap();

        let err = install(
            Path::new("app.exe"),
            &file,
            dir.path(),
            ScriptTimings::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, DeltaError::FullInstallFailed(_)),
            "未知安装包类型必须返回 FullInstallFailed，实际: {err:?}"
        );
    }

    /// 跨平台类型不匹配时必须明确报错，而不是尝试执行（CI 各平台都会跑）。
    #[test]
    fn mismatched_platform_kind_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("MyApp_1.2.3.tar.gz");
        std::fs::write(&file, b"not a real archive").unwrap();
        let exe = dir.path().join("app");

        #[cfg(target_os = "windows")]
        let err = install(&exe, &file, dir.path(), ScriptTimings::default()).unwrap_err();
        #[cfg(target_os = "linux")]
        let err = install(&exe, &file, dir.path(), ScriptTimings::default()).unwrap_err();
        #[cfg(target_os = "macos")]
        let err = install(&exe, &file, dir.path(), ScriptTimings::default()).unwrap_err();

        assert!(
            matches!(err, DeltaError::FullInstallFailed(_)),
            "平台不匹配的安装包必须报错，实际: {err:?}"
        );
    }
}
