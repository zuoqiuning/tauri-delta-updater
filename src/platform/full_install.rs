//! 全量安装包的执行器。
//!
//! 增量路径替换的是"已安装的主程序/整包目录"，全量路径必须交给**安装器**：
//!
//! - Windows：NSIS `.exe`（`/P /UPDATE /R`，Tauri 约定的静默更新参数）、
//!   MSI（`/passive /promptrestart AUTOLAUNCHAPP=True`）或 Inno Setup
//!   （`/SILENT` / `/VERYSILENT`），需要提权；
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstallerKind {
    /// Windows NSIS 安装器（`.exe`，静默更新参数 `/P /UPDATE /R`）。
    Nsis,
    /// Windows MSI 安装包。
    Msi,
    /// macOS 的 `.app.tar.gz`。
    MacAppTarGz,
    /// Linux AppImage。
    AppImage,
    /// Inno Setup 打包的 Windows 安装器（`.exe`）。
    ///
    /// Inno 与 NSIS 的产物后缀相同，无法只靠文件名区分。未显式声明
    /// [`crate::UpdateConfig::installer_kind`] 时，引擎会扫描安装包中的
    /// Inno/NSIS 标识字符串做尽力而为的识别；自定义 loader 或标记被剥离时
    /// 仍应显式配置，否则可能被按 NSIS 参数调用（Inno 不识别 `/P /UPDATE /R`）。
    Inno,
    /// 无法识别的类型（不会被执行）。
    Other,
}

/// 全量安装失败的结构化类型。
///
/// 前端可据此精确分支：取消提权应提示“已取消”，平台不匹配应提示
/// “下载了错误的安装包”，而不是统一显示一条错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstallFailureKind {
    /// 用户在 UAC / 系统授权框上点了“否”。
    UserCancelledElevation,
    /// 解包后找不到预期的安装器 / `.app` 目录。
    InstallerNotFound,
    /// 安装器/解包工具返回非零状态。
    InstallerReturnedNonZero,
    /// 安装包类型与当前平台不匹配（例如在 Windows 上执行 `.app.tar.gz`）。
    PlatformMismatch,
    /// 安装包类型无法识别或不受支持。
    UnsupportedKind,
    /// 启动安装器本身失败（ShellExecute / 进程创建错误）。
    LaunchFailed,
    /// 其它失败。
    Other,
}

/// Windows 安装器的交互模式（对应 Tauri 官方 updater 的 `installMode`）。
///
/// 默认 `Passive`，与官方保持同一行为；其它模式只改变安装器 UI 与提权方式，
/// 不改变“安装完成后重启应用”的语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum InstallMode {
    /// 显示进度条但不要求交互（NSIS `/P`，MSI `/passive`）。官方默认。
    #[default]
    Passive,
    /// 显示基础交互界面（NSIS 默认 UI，MSI `/qb`）。
    BasicUi,
    /// 无安装界面（NSIS `/S`，MSI `/qn`）。安装器无法自行提权，
    /// 仅适用于用户级安装或进程本身已是管理员。
    ///
    /// 注意：`quiet` 不等同于“无任何提示”——需要提权时，Windows 仍会
    /// 弹出 UAC 确认框，该提示无法被安装器参数抑制。
    Quiet,
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

/// 构造带结构化类型的全量安装错误。
fn install_failed(
    kind: InstallFailureKind,
    message: impl Into<String>,
) -> crate::error::DeltaError {
    crate::error::DeltaError::FullInstallFailed {
        kind,
        message: message.into(),
    }
}

/// 执行全量安装。返回 `Ok(())` 后调用方应立即退出进程。
pub fn install(
    current_exe: &Path,
    installer: &Path,
    work_dir: &Path,
    timings: ScriptTimings,
) -> Result<()> {
    install_with_mode(
        current_exe,
        installer,
        work_dir,
        timings,
        InstallMode::default(),
    )
}

/// 与 [`install`] 相同，但可指定 Windows 安装器交互模式。
pub fn install_with_mode(
    current_exe: &Path,
    installer: &Path,
    work_dir: &Path,
    timings: ScriptTimings,
    mode: InstallMode,
) -> Result<()> {
    install_with_mode_and_kind(current_exe, installer, work_dir, timings, mode, None)
}

/// 与 [`install_with_mode`] 相同，但可显式声明安装器类型。
///
/// `kind` 为 `None` 时按文件后缀推断；`.exe` 会推断成 NSIS。
/// Inno Setup 的产物同样是 `.exe`，必须传
/// `Some(InstallerKind::Inno)`，否则引擎会按 NSIS 参数调用。
pub fn install_with_mode_and_kind(
    current_exe: &Path,
    installer: &Path,
    work_dir: &Path,
    timings: ScriptTimings,
    mode: InstallMode,
    kind: Option<InstallerKind>,
) -> Result<()> {
    install_with_mode_kind_and_args(current_exe, installer, work_dir, timings, mode, kind, &[])
}

/// 与 [`install_with_mode_and_kind`] 相同，但可追加 Windows 安装器的额外参数。
///
/// `extra_args` 只对 Windows 的 NSIS / MSI / Inno 生效，其它平台忽略。
/// 含空格或引号的参数会按 Windows 命令行规则自动加引号。
pub fn install_with_mode_kind_and_args(
    current_exe: &Path,
    installer: &Path,
    work_dir: &Path,
    timings: ScriptTimings,
    mode: InstallMode,
    kind: Option<InstallerKind>,
    extra_args: &[String],
) -> Result<()> {
    let kind = resolve_kind(installer, kind)?;
    // 先在所有平台完成校验与拼接：NUL 参数在非 Windows 平台也应尽早失败。
    let extra = render_extra_args(extra_args)?;
    #[cfg(not(target_os = "windows"))]
    let _ = extra;
    match kind {
        InstallerKind::Nsis | InstallerKind::Msi | InstallerKind::Inno => {
            #[cfg(target_os = "windows")]
            {
                let _ = (current_exe, work_dir, timings);
                windows::launch_with_kind(installer, mode, kind, &extra)
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = (current_exe, work_dir, timings, mode);
                Err(install_failed(
                    InstallFailureKind::PlatformMismatch,
                    format!("{} 只能在 Windows 上执行", installer.display()),
                ))
            }
        }
        InstallerKind::MacAppTarGz => {
            #[cfg(target_os = "macos")]
            {
                let _ = mode;
                macos::install_app(current_exe, installer, work_dir, timings)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (current_exe, work_dir, timings, mode);
                Err(install_failed(
                    InstallFailureKind::PlatformMismatch,
                    "macOS .app.tar.gz 只能在 macOS 上执行",
                ))
            }
        }
        InstallerKind::AppImage => {
            #[cfg(target_os = "linux")]
            {
                let _ = mode;
                linux::install_appimage(current_exe, installer, work_dir, timings)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (current_exe, work_dir, timings, mode);
                Err(install_failed(
                    InstallFailureKind::PlatformMismatch,
                    "AppImage 安装包只能在 Linux 上执行",
                ))
            }
        }
        InstallerKind::Other => Err(install_failed(
            InstallFailureKind::UnsupportedKind,
            format!("不支持的全量安装包类型：{}", installer.display()),
        )),
    }
}

/// 拼接 Windows 安装器的额外参数；含空格或引号的参数自动加引号。
///
/// 返回空串表示没有额外参数。NUL 会截断 `ShellExecute` 的命令行，
/// 因此在这里拒绝（构造阶段已先检查一次，这里是低层 API 的第二道防线）。
fn render_extra_args(args: &[String]) -> Result<String> {
    let mut rendered = String::new();
    for arg in args {
        if arg.contains('\0') {
            return Err(crate::error::DeltaError::Config(
                "installer_args 不能包含 NUL 字符（会截断 Windows 命令行）".into(),
            ));
        }
        if arg.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        if arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
            rendered.push_str(&quote_windows_arg(arg));
        } else {
            rendered.push_str(arg);
        }
    }
    Ok(rendered)
}

/// 按 Windows `CommandLineToArgvW` 规则给单个参数加引号。
fn quote_windows_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // 已有 n 个反斜杠，再加 n+1 个（共 2n+1）后输出字面引号。
                out.push_str(&"\\".repeat(backslashes + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    // 结尾反斜杠必须在右引号前翻倍，否则会被当作转义。
    out.push_str(&"\\".repeat(backslashes));
    out.push('"');
    out
}

/// 解析最终使用的安装器类型，并校验配置项与文件后缀的一致性。
///
/// 只拦明显矛盾（`.msi` 配 Inno、`.tar.gz` 配 Nsis 等）。`.exe` 同时属于
/// NSIS 与 Inno：未显式配置时读取安装包标识做尽力而为的识别，
/// 识别不出来再按 NSIS 处理。
fn resolve_kind(installer: &Path, kind: Option<InstallerKind>) -> Result<InstallerKind> {
    let inferred = installer_kind(installer);
    let Some(kind) = kind else {
        if inferred == InstallerKind::Nsis {
            return Ok(detect_exe_kind(installer));
        }
        return Ok(inferred);
    };
    let compatible = match kind {
        InstallerKind::Nsis | InstallerKind::Inno => inferred == InstallerKind::Nsis,
        InstallerKind::Msi => inferred == InstallerKind::Msi,
        InstallerKind::MacAppTarGz => inferred == InstallerKind::MacAppTarGz,
        InstallerKind::AppImage => inferred == InstallerKind::AppImage,
        InstallerKind::Other => false,
    };
    if !compatible {
        return Err(crate::error::DeltaError::Config(format!(
            "installer_kind 配置为 {kind:?}，但安装包后缀推断为 {inferred:?}：{}",
            installer.display()
        )));
    }
    Ok(kind)
}

/// 从 `.exe` 的内容识别 NSIS / Inno Setup（显式配置始终优先）。
///
/// 只扫描首尾各 8 MB：Inno 的 loader/版本信息在文件头部，setup data 头在
/// 尾部 overlay；避免为了识别类型把几百 MB 安装包整个读一遍。
fn detect_exe_kind(installer: &Path) -> InstallerKind {
    /// 识别标记同时接受 ASCII 与 UTF-16LE（Windows 版本信息常用宽字符）。
    const INNO_MARKERS: &[&str] = &["Inno Setup Setup Data", "Inno Setup Ldr"];
    const NSIS_MARKER: &str = "Nullsoft";
    const SCAN_WINDOW: u64 = 8 * 1024 * 1024;

    use std::io::{Read, Seek, SeekFrom};

    let Ok(meta) = std::fs::metadata(installer) else {
        return InstallerKind::Nsis;
    };
    let Ok(mut file) = std::fs::File::open(installer) else {
        return InstallerKind::Nsis;
    };
    let size = meta.len();

    let found = |data: &[u8]| -> (bool, bool) {
        let contains = |needle: &str| {
            let ascii = needle.as_bytes();
            let utf16: Vec<u8> = needle
                .encode_utf16()
                .flat_map(|unit| [unit as u8, (unit >> 8) as u8])
                .collect();
            data.windows(ascii.len()).any(|window| window == ascii)
                || data.windows(utf16.len()).any(|window| window == utf16)
        };
        (
            INNO_MARKERS.iter().any(|marker| contains(marker)),
            contains(NSIS_MARKER),
        )
    };

    let scan = |file: &mut std::fs::File, offset: u64, len: u64| -> (bool, bool) {
        if len == 0 || file.seek(SeekFrom::Start(offset)).is_err() {
            return (false, false);
        }
        let mut buffer = vec![0u8; len as usize];
        if file.read_exact(&mut buffer).is_err() {
            return (false, false);
        }
        found(&buffer)
    };

    let head_len = size.min(SCAN_WINDOW);
    let (inno, nsis) = scan(&mut file, 0, head_len);
    if inno {
        return InstallerKind::Inno;
    }
    if nsis {
        return InstallerKind::Nsis;
    }
    if size > SCAN_WINDOW {
        let tail_len = SCAN_WINDOW.min(size);
        let (inno_tail, nsis_tail) = scan(&mut file, size - tail_len, tail_len);
        if inno_tail {
            return InstallerKind::Inno;
        }
        if nsis_tail {
            return InstallerKind::Nsis;
        }
    }
    InstallerKind::Nsis
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{install_failed, InstallFailureKind, InstallMode, InstallerKind};
    use crate::error::Result;
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

    /// 用已知类型启动安装器（Inno 与 NSIS 同为 `.exe`，不能重新推断）。
    ///
    /// `extra_args` 已由调用方校验并按 Windows 命令行规则加好引号。
    pub fn launch_with_kind(
        installer: &Path,
        mode: InstallMode,
        kind: InstallerKind,
        extra_args: &str,
    ) -> Result<()> {
        let append = |base: String| -> String {
            if extra_args.is_empty() {
                base
            } else {
                format!("{base} {extra_args}")
            }
        };
        let (file, params): (PathBuf, String) = match kind {
            // 与官方 updater 一致：默认被动模式 + `/UPDATE` 告诉 Tauri 的
            // NSIS 安装器这是更新而不是首次安装；`/R` 让安装完成后重新启动应用。
            InstallerKind::Nsis => (installer.to_path_buf(), append(nsis_parameters(mode))),
            // MSI 用相同语义的安装模式，`AUTOLAUNCHAPP` 让安装器重新启动应用。
            InstallerKind::Msi => (
                PathBuf::from("msiexec.exe"),
                append(format!(
                    "/i \"{}\" {}",
                    installer.display(),
                    msi_parameters(mode)
                )),
            ),
            // Inno 的 `/UPDATE` / `/R` 语义由 `.iss` 脚本承担，没有对应命令行参数。
            InstallerKind::Inno => (installer.to_path_buf(), append(inno_parameters(mode))),
            other => {
                return Err(install_failed(
                    InstallFailureKind::UnsupportedKind,
                    format!(
                        "不支持的全量安装包类型（{other:?}）：{}",
                        installer.display()
                    ),
                ))
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
            let kind = if code == 5 {
                InstallFailureKind::UserCancelledElevation
            } else {
                InstallFailureKind::LaunchFailed
            };
            return Err(install_failed(
                kind,
                format!(
                    "启动安装器失败（ShellExecute 返回 {code}，5 表示用户取消提权）：{}",
                    installer.display()
                ),
            ));
        }
        Ok(())
    }

    /// NSIS 参数：交互模式 + 更新标记 + 安装后重启应用。
    ///
    /// `/UPDATE` 是 Tauri NSIS 模板识别的更新标记；缺了它安装器会走首次安装流程。
    pub(super) fn nsis_parameters(mode: InstallMode) -> String {
        let ui = match mode {
            InstallMode::Passive => "/P ",
            InstallMode::BasicUi => "",
            InstallMode::Quiet => "/S ",
        };
        format!("{ui}/UPDATE /R")
    }

    /// MSI 参数：交互模式 + 不自动重启系统 + 安装后启动应用。
    pub(super) fn msi_parameters(mode: InstallMode) -> String {
        let ui = match mode {
            InstallMode::Passive => "/passive",
            InstallMode::BasicUi => "/qb",
            InstallMode::Quiet => "/qn",
        };
        format!("{ui} /promptrestart AUTOLAUNCHAPP=True")
    }

    /// Inno Setup 参数：交互模式 + 不弹起始确认框 + 不自动重启系统。
    ///
    /// - `Passive` → `/SILENT`：显示进度窗口，不进入安装向导；
    /// - `Quiet` → `/VERYSILENT`：完全无界面；
    /// - `BasicUi` → 空字符串：走 Inno 默认 UI。
    ///
    /// `/SP-` 关闭 Inno 默认的 "This will install…" 起始确认框；
    /// `/NORESTART` 阻止安装器在静默模式下自行重启系统。
    ///
    /// **`/UPDATE` 与 `/R` 没有 Inno 等价参数**：是否为更新、装完是否启动应用，
    /// 必须由 `.iss` 脚本实现（读旧 ARP 记录、`[Run]` 段去掉
    /// `skipifsilent` 等）。引擎只负责把上面这些参数传对。
    pub(super) fn inno_parameters(mode: InstallMode) -> String {
        match mode {
            InstallMode::Passive => "/SILENT /NORESTART /SP-",
            InstallMode::BasicUi => "",
            InstallMode::Quiet => "/VERYSILENT /NORESTART /SP-",
        }
        .to_string()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{install_failed, InstallFailureKind};
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
            .map_err(|e| {
                install_failed(
                    InstallFailureKind::LaunchFailed,
                    format!("无法启动 tar: {e}"),
                )
            })?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&staging_root);
            return Err(install_failed(
                InstallFailureKind::InstallerReturnedNonZero,
                format!(
                    "解包 {} 失败: {}",
                    installer.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }

        let app_dir = find_app_bundle(&staging_root).ok_or_else(|| {
            let _ = std::fs::remove_dir_all(&staging_root);
            install_failed(
                InstallFailureKind::InstallerNotFound,
                format!("{} 里没有找到 .app 目录", installer.display()),
            )
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
    use super::{install, installer_kind, render_extra_args, resolve_kind, InstallerKind};
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
        use super::InstallMode;

        let nsis = super::windows::nsis_parameters(InstallMode::Passive);
        assert!(nsis.contains("/P"), "NSIS 必须用被动模式: {nsis}");
        assert!(nsis.contains("/UPDATE"), "NSIS 必须带更新标记: {nsis}");
        assert!(nsis.contains("/R"), "NSIS 安装后必须重启应用: {nsis}");

        let basic = super::windows::nsis_parameters(InstallMode::BasicUi);
        assert!(
            !basic.contains("/P") && !basic.contains("/S"),
            "basicUi 不应带静默标记: {basic}"
        );
        assert!(
            basic.contains("/UPDATE /R"),
            "必须保留更新标记与重启: {basic}"
        );

        let quiet = super::windows::nsis_parameters(InstallMode::Quiet);
        assert!(quiet.contains("/S"), "quiet 必须带 /S: {quiet}");
        assert!(
            quiet.contains("/UPDATE /R"),
            "必须保留更新标记与重启: {quiet}"
        );

        let msi = super::windows::msi_parameters(InstallMode::Passive);
        assert!(msi.contains("/passive"), "MSI 必须用被动模式: {msi}");
        assert!(
            msi.contains("AUTOLAUNCHAPP=True"),
            "MSI 安装后必须启动应用: {msi}"
        );
        assert!(
            super::windows::msi_parameters(InstallMode::BasicUi).contains("/qb"),
            "basicUi 必须映射到 /qb"
        );
        assert!(
            super::windows::msi_parameters(InstallMode::Quiet).contains("/qn"),
            "quiet 必须映射到 /qn"
        );
    }

    /// Inno 静默参数必须覆盖三种交互模式，并带上防重启/防确认框标记。
    #[cfg(target_os = "windows")]
    #[test]
    fn inno_parameters_match_install_mode() {
        use super::InstallMode;

        let passive = super::windows::inno_parameters(InstallMode::Passive);
        assert!(
            passive.contains("/SILENT"),
            "passive 必须用 /SILENT: {passive}"
        );
        assert!(
            passive.contains("/NORESTART"),
            "静默模式必须阻止自动重启系统: {passive}"
        );
        assert!(passive.contains("/SP-"), "必须关闭起始确认框: {passive}");
        assert!(
            !passive.contains("/VERYSILENT"),
            "/SILENT 不能同时出现 /VERYSILENT: {passive}"
        );

        let quiet = super::windows::inno_parameters(InstallMode::Quiet);
        assert!(quiet.contains("/VERYSILENT"), "quiet 必须完全静默: {quiet}");
        assert!(
            quiet.contains("/NORESTART") && quiet.contains("/SP-"),
            "{quiet}"
        );

        assert!(
            super::windows::inno_parameters(InstallMode::BasicUi).is_empty(),
            "basicUi 走 Inno 默认 UI，不应附加参数"
        );
    }

    /// `.exe` 同时属于 NSIS 与 Inno：显式配置决定实际参数。
    #[test]
    fn explicit_inno_kind_overrides_exe_inference() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("setup.exe");
        std::fs::write(&exe, b"MZ").unwrap();

        assert_eq!(installer_kind(&exe), InstallerKind::Nsis);
        assert_eq!(
            resolve_kind(&exe, Some(InstallerKind::Inno)).unwrap(),
            InstallerKind::Inno
        );
        assert_eq!(
            resolve_kind(&exe, Some(InstallerKind::Nsis)).unwrap(),
            InstallerKind::Nsis
        );
        assert_eq!(resolve_kind(&exe, None).unwrap(), InstallerKind::Nsis);
    }

    /// `.exe` 未显式配置时按内容识别 Inno，避免漏配 `installer_kind` 时弹向导。
    #[test]
    fn detects_inno_setup_from_exe_marker() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("setup.exe");
        std::fs::write(&exe, b"MZ ... Inno Setup Setup Data (6.2.0) ...").unwrap();
        assert_eq!(resolve_kind(&exe, None).unwrap(), InstallerKind::Inno);
    }

    /// NSIS 标识不会被误判为 Inno。
    #[test]
    fn detects_nsis_from_exe_marker() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("setup.exe");
        std::fs::write(&exe, b"MZ ... Nullsoft Install System ...").unwrap();
        assert_eq!(resolve_kind(&exe, None).unwrap(), InstallerKind::Nsis);
    }

    /// 显式类型始终优先：用户比字符串扫描更了解自己的安装器。
    #[test]
    fn explicit_kind_wins_over_content_detection() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("setup.exe");
        std::fs::write(&exe, b"MZ ... Inno Setup Ldr ...").unwrap();
        assert_eq!(
            resolve_kind(&exe, Some(InstallerKind::Nsis)).unwrap(),
            InstallerKind::Nsis
        );
    }

    /// 配置项与后缀明显矛盾时必须早失败，而不是把错误安装器丢给 ShellExecute。
    #[test]
    fn explicit_kind_rejects_mismatched_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let msi = dir.path().join("setup.msi");
        std::fs::write(&msi, b"MSI").unwrap();

        let err = resolve_kind(&msi, Some(InstallerKind::Inno)).unwrap_err();
        assert!(
            matches!(err, DeltaError::Config(_)),
            "配置错误必须返回 Config，实际: {err:?}"
        );
        assert!(
            err.to_string().contains("installer_kind"),
            "错误必须点名字段: {err}"
        );
    }

    /// 额外参数按 Windows 命令行规则拼接；含空格/引号自动加引号，NUL 拒绝。
    #[test]
    fn extra_installer_arguments_are_quoted_and_joined() {
        assert_eq!(render_extra_args(&[]).unwrap(), "");
        assert_eq!(
            render_extra_args(&["/NORESTART".into(), "/LANG=en".into()]).unwrap(),
            "/NORESTART /LANG=en"
        );
        assert_eq!(
            render_extra_args(&["/D=C:\\Program Files\\App".into()]).unwrap(),
            "\"/D=C:\\Program Files\\App\""
        );
        assert_eq!(
            render_extra_args(&["/X=\"quoted\"".into()]).unwrap(),
            "\"/X=\\\"quoted\\\"\""
        );
        assert_eq!(
            render_extra_args(&["".into(), "/A".into()]).unwrap(),
            "/A",
            "空参数不得产生多余空格"
        );
        assert!(render_extra_args(&["bad\0arg".into()]).is_err());
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
            matches!(
                err,
                DeltaError::FullInstallFailed {
                    kind: super::InstallFailureKind::UnsupportedKind,
                    ..
                }
            ),
            "未知安装包类型必须返回 UnsupportedKind，实际: {err:?}"
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
            matches!(
                err,
                DeltaError::FullInstallFailed {
                    kind: super::InstallFailureKind::PlatformMismatch,
                    ..
                }
            ),
            "平台不匹配的安装包必须报 PlatformMismatch，实际: {err:?}"
        );
    }
}
