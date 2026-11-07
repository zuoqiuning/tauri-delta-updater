//! 平台标识（Tauri updater 的 target 名称）。
//!
//! 清单里的 `platforms` 与 `patches` 都以平台标识为键，
//! 客户端必须用与 Tauri 完全一致的名称才能取到对应条目。

use std::path::{Path, PathBuf};

/// 当前构建对应的平台标识，格式与 Tauri updater 的 target 一致。
///
/// 例如 `windows-x86_64`、`darwin-aarch64`、`linux-x86_64`。
///
/// 直接用它来查询清单，不要手写字符串：
/// 手写时把 `x86_64` 写成 `x64` 就会永远取不到补丁，
/// 而且不会报错，只会静默退化成全量下载。
pub fn current_platform() -> &'static str {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86") {
        "i686"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "arm") {
        "armv7"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        "unknown"
    };

    // 平台与架构的组合有限，预先列出避免运行期分配
    match (os, arch) {
        ("windows", "i686") => "windows-i686",
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "armv7") => "windows-armv7",
        ("windows", "aarch64") => "windows-aarch64",
        ("windows", "riscv64") => "windows-riscv64",
        ("darwin", "i686") => "darwin-i686",
        ("darwin", "x86_64") => "darwin-x86_64",
        ("darwin", "armv7") => "darwin-armv7",
        ("darwin", "aarch64") => "darwin-aarch64",
        ("darwin", "riscv64") => "darwin-riscv64",
        ("linux", "i686") => "linux-i686",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "armv7") => "linux-armv7",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "riscv64") => "linux-riscv64",
        _ => "unknown-unknown",
    }
}

/// 从安装包文件名推断架构标识。
///
/// 用于发布流程：历史安装包的文件名里带有架构信息，
/// 而补丁必须与安装包架构严格对应。
///
/// 匹配顺序很重要：`x86-64` 与 `x86-` 都是常见写法，
/// 必须先判定 64 位，否则 `x86-64` 会被误认为 32 位。
///
/// # 平台推断的边界
///
/// 这里只推断**架构**，不推断操作系统：同一个文件名（例如 `app.exe`）
/// 在不同系统下都可能出现。操作系统由调用方按发布目标决定。
pub fn detect_arch(file_name: &str) -> &'static str {
    let lower = file_name.to_lowercase();

    // 64 位 x86：x86_64 / x86-64 / x64 / amd64
    if lower.contains("x86_64") || lower.contains("x86-64") {
        return "x86_64";
    }
    if lower.contains("aarch64") || lower.contains("arm64") {
        return "aarch64";
    }
    if lower.contains("riscv64") {
        return "riscv64";
    }
    if lower.contains("armv7") || lower.contains("armhf") {
        return "armv7";
    }
    // 32 位 x86：i686 / i386 / x86（独占写法）
    if lower.contains("i686") || lower.contains("i386") || lower.contains("x86-") {
        return "i686";
    }
    // 常见简称
    if lower.contains("x64") || lower.contains("amd64") {
        return "x86_64";
    }

    // 无法识别时必须显式暴露，而不是默认 x86_64：
    // 默认值会把架构不明的旧包静默当成 x86_64 处理，生成跨架构补丁。
    "unknown"
}

/// 规范化版本号：去掉开头的 `v`。
///
/// 发布流程里版本号常常来自 Git tag（`v0.1.1`），
/// 而清单里的版本号要与客户端上报的 `CARGO_PKG_VERSION`（`0.1.1`）一致，
/// 否则补丁永远匹配不上。
pub fn normalize_version(version: &str) -> &str {
    let trimmed = version.trim();
    // `v` 与 `V` 前缀都来自 tag 命名习惯；语义化版本本身不允许前导 V。
    trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed)
}

/// AppImage 启动时真正需要替换的文件（`.AppImage` 本体）。
///
/// AppImage 运行时把 squashfs 挂到 `/tmp/.mount_*`，`current_exe()`
/// 指向挂载点里的内部二进制——替换它没有意义，卸载后就消失了。
/// 下次用户启动执行的是 `$APPIMAGE` 指向的文件，所以补丁目标是它。
#[cfg(target_os = "linux")]
pub fn appimage_target() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

/// 当前进程真正会被执行、也应当被替换的可执行文件。
///
/// - 普通环境：等价于 [`std::env::current_exe`]；
/// - AppImage：返回 `$APPIMAGE` 指向的 `.AppImage` 文件。
///
/// 启动确认（`confirm_startup_success*`）与替换目标必须用同一个函数解析，
/// 否则会出现在挂载点旁边写成功标志、脚本却在 `.AppImage` 旁边等标志的情况。
pub fn runtime_executable() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = appimage_target() {
        return Ok(appimage);
    }
    std::env::current_exe()
}

/// 判断当前运行环境是否适合执行增量替换，不适合时返回回退原因。
///
/// 判定只依赖运行环境（exe 路径与平台），发生在**下载补丁之前**——
/// 清单仍会拉取（客户端需要知道有无新版本与全量地址），
/// 但不会为用不上的补丁消耗流量。
///
/// 本库的定位是**完整更新引擎**：只在能安全覆盖的场景接管替换，
/// 其余场景走全量回退（可交回官方 updater，也可由本库执行安装器）——
/// 回退不是失败，是正确性优先。
///
/// 当前判定：
///
/// - **Linux AppImage**：目标是 `$APPIMAGE` 指向的 `.AppImage` 本体（可直接替换、
///   补可执行位后重启）。只有定位不到本体、或所在目录不可写时才回退；
/// - **macOS `.app` bundle**：替换 bundle 内二进制后只能 ad-hoc 重签，
///   开发者身份丢失，Gatekeeper 与公证链路断裂。
///   （bundle 外的裸二进制仍是实验性支持。）
///
/// Windows 单文件 exe 是本库的甜点区，始终返回 `None`。
pub fn incremental_fallback_reason(exe: &Path) -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        match appimage_target() {
            Some(appimage) => {
                let writable = appimage
                    .parent()
                    .map(crate::core::paths::is_writable)
                    .unwrap_or(false);
                if !writable {
                    return Some("AppImage 所在目录不可写，无法原地替换 .AppImage");
                }
            }
            None => {
                if std::env::var_os("APPIMAGE").is_some() || exe.starts_with("/tmp/.mount") {
                    return Some(
                        "AppImage 运行时：无法定位 .AppImage 本体，替换只会写入临时挂载点",
                    );
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = exe;
    }
    #[cfg(target_os = "macos")]
    {
        if crate::platform::macos::bundle_root(exe).is_some() {
            return Some(
                "macOS .app bundle：增量替换后只能 ad-hoc 重签，会丢失开发者签名与公证状态",
            );
        }
    }
    None
}
