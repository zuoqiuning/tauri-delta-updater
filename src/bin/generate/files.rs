//! 文件名解析：版本、平台与产物类型推断。

use regex::Regex;
use std::path::Path;
use tauri_delta_updater::detect_arch;

/// 从文件名提取版本号，支持 `Sereno_0.1.0_x64-setup.exe` / `Sereno-v0.1.2-arm64.exe` 等格式。
pub(crate) fn extract_version(file_name: &str) -> Option<String> {
    let re = Regex::new(r"[vV]?(\d+\.\d+\.\d+(?:-[\w.]+)?)").ok()?;
    re.captures(file_name)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// 从文件名推断操作系统标识。
pub(crate) fn infer_os(file_name: &str) -> Option<&'static str> {
    let lower = file_name.to_lowercase();
    if lower.ends_with(".exe") || lower.ends_with(".msi") {
        Some("windows")
    } else if lower.ends_with(".app.tar.gz") || lower.ends_with(".dmg") {
        Some("darwin")
    } else if lower.ends_with(".appimage") || lower.ends_with(".deb") || lower.ends_with(".rpm") {
        Some("linux")
    } else {
        None
    }
}

/// 从文件名推断完整平台标识。
///
/// 只推断架构与操作系统；无法识别时返回 `None`，由调用方要求显式 `--platform`，
/// 绝不默认成 `windows-x86_64`——那会静默生成跨平台/跨架构补丁。
pub(crate) fn infer_platform(file_name: &str) -> Option<String> {
    let os = infer_os(file_name)?;
    let arch = detect_arch(file_name);
    if arch == "unknown" {
        return None;
    }
    Some(format!("{os}-{arch}"))
}

/// 判断旧目录里的文件是否是"主程序产物"（不是安装包、补丁、元数据）。
pub(crate) fn is_app_binary_candidate(file_name: &str) -> bool {
    let lower = file_name.to_lowercase();
    const SKIP: &[&str] = &[
        ".patch",
        ".json",
        ".sig",
        ".txt",
        ".md",
        ".log",
        ".msi",
        ".dmg",
        ".pkg",
        ".deb",
        ".rpm",
        ".appimage",
        ".tar",
        ".gz",
        ".xz",
        ".zip",
        ".7z",
        ".bz2",
    ];
    !SKIP.iter().any(|ext| lower.ends_with(ext))
}

/// 从旧应用目录推断版本号。
///
/// macOS 发布产物本身叫 `MyApp.app`，版本通常写在父目录
/// （`v0.1.0/MyApp.app`）；目录名里已经有版本时优先用目录名。
pub(crate) fn old_app_version(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_string_lossy();
    if let Some(version) = extract_version(&name) {
        return Some(version);
    }
    if name.ends_with(".app") {
        let parent = dir.parent()?.file_name()?.to_string_lossy();
        return extract_version(&parent);
    }
    None
}

/// 通道对应的清单文件名。
///
/// `stable` 保持历史命名（已有文档/客户端都指向 `latest.json`）；
/// 其它通道用 `latest-<channel>.json`，客户端只需改 manifest_url。
pub(crate) fn manifest_names(channel: &str) -> (String, String) {
    if channel == "stable" {
        ("latest.json".to_string(), "latest.bundle.json".to_string())
    } else {
        (
            format!("latest-{channel}.json"),
            format!("latest-{channel}.bundle.json"),
        )
    }
}
