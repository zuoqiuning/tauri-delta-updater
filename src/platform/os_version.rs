//! 系统版本探测与最低版本比较。
//!
//! 只在清单声明了 `platforms[].min_os` 时才会产生一次探测。
//! 探测失败一律按"未知"处理：宁可放行更新，也不因为读不到版本而误伤。

use std::cmp::Ordering;

/// 当前系统版本，形如 `10.0.19045` / `13.4` / `5.15.0-91-generic`。
///
/// 无法确定时返回 `None`（未知 ≠ 不满足）。Linux 返回内核版本。
pub fn current_os_version() -> Option<String> {
    #[cfg(windows)]
    {
        return windows_version();
    }
    #[cfg(target_os = "macos")]
    {
        return macos_version();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_version();
    }
    #[allow(unreachable_code)]
    None
}

/// 比较两个系统版本号，返回"当前相对最低要求"的顺序。
///
/// 按 `.` 分段比较，每段只取前导数字（`5.15.0-91-generic` 视为 `5.15.0`），
/// 缺失段按 0 处理。任何一段无法解析时返回 `None`。
pub(crate) fn compare(current: &str, required: &str) -> Option<Ordering> {
    let current = components(current)?;
    let required = components(required)?;
    Some(current.cmp(&required))
}

/// 把版本号拆成等长的数字段，便于逐段比较。
fn components(version: &str) -> Option<Vec<u64>> {
    let mut parts: Vec<u64> = Vec::new();
    for part in version.trim().split('.') {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        parts.push(digits.parse().ok()?);
    }
    (!parts.is_empty()).then_some(parts)
}

#[cfg(windows)]
fn windows_version() -> Option<String> {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
    let status = unsafe { RtlGetVersion(&mut info) };
    if status < 0 {
        return None;
    }
    Some(format!(
        "{}.{}.{}",
        info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
    ))
}

#[cfg(target_os = "macos")]
fn macos_version() -> Option<String> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

#[cfg(target_os = "linux")]
fn linux_version() -> Option<String> {
    let version = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::{compare, current_os_version};
    use std::cmp::Ordering;

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(compare("10.0.19045", "10.0.19045"), Some(Ordering::Equal));
        assert_eq!(compare("10.0.22000", "10.0.19045"), Some(Ordering::Greater));
        assert_eq!(compare("10.0.10240", "10.0.19045"), Some(Ordering::Less));
        assert_eq!(compare("13.4", "13.0"), Some(Ordering::Greater));
        assert_eq!(compare("13.0.1", "13.0"), Some(Ordering::Greater));
    }

    /// 带后缀的内核版本只比较数字段。
    #[test]
    fn ignores_non_numeric_suffixes() {
        assert_eq!(
            compare("5.15.0-91-generic", "5.15.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare("6.1.0-28", "5.15.0"), Some(Ordering::Greater));
    }

    /// 无法解析时不阻断：未知 ≠ 不满足。
    #[test]
    fn unknown_versions_do_not_block() {
        assert_eq!(compare("unknown", "10.0"), None);
        assert_eq!(compare("", "10.0"), None);
        assert_eq!(compare("10.0", "10..0"), None);
        if let Some(current) = current_os_version() {
            assert_eq!(
                compare(&current, "999999.0"),
                Some(Ordering::Less),
                "夸张的最低版本必须判定为过低"
            );
        }
    }

    /// Linux 的 `min_os` 比较内核 release，不是发行版版本：写 `24.04` 会把
    /// 所有内核判成过低。测试锁死这个语义，避免有人把发行版版本当成内核版本。
    #[test]
    fn linux_min_os_uses_kernel_release() {
        assert_eq!(compare("5.15.0-91-generic", "24.04"), Some(Ordering::Less));
        assert_eq!(compare("6.8.0-40-generic", "5.15"), Some(Ordering::Greater));
    }
}
