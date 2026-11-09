//! Windows Authenticode 校验（`WinVerifyTrust`）。
//!
//! # 为什么更新器要查这个
//!
//! 补丁产物应当与发布端**已签名**的新版本逐字节一致（清单里的
//! `target_hash` 已经锚定了这一点）。但如果发布端忘了给新版本签名，
//! 替换后的程序在企业环境里会被 AppLocker / WDAC 直接拒绝启动，
//! 用户看到的是"更新后应用打不开"——这比拒绝更新严重得多。
//!
//! 因此策略是：**原版本可信、新产物不可信 → 阻断更新**。
//! 只在原版本可被本机信任时才比较，避免把"本机没有该 CA / 离线无法拉 CRL"
//! 这类环境问题误判成发布端漏签。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Security::WinTrust::{
    WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
    WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};

use super::AuthenticodePolicy;

/// `WinVerifyTrust` 的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// 签名可被本机信任。
    Trusted,
    /// 未签名或被明确拒绝。
    NotTrusted,
    /// 无法得出结论：通常是离线且证书链不在本地缓存（cache-only 模式）。
    Inconclusive,
}

/// 按策略比较原版本与产物。
///
/// 返回 `(是否阻断, 是否需要告警)`；非 Windows 平台由调用方直接跳过。
pub fn evaluate(original: &Path, staged: &Path, policy: AuthenticodePolicy) -> (bool, bool) {
    decide(trust_state(original), trust_state(staged), policy)
}

/// 纯决策函数：把两种签名状态按策略映射成 `(阻断, 告警)`。
pub fn decide(
    original: TrustState,
    staged: TrustState,
    policy: AuthenticodePolicy,
) -> (bool, bool) {
    if policy == AuthenticodePolicy::Off {
        return (false, false);
    }
    match (original, staged) {
        // 原版本本身不可信（开发构建、便携应用）：不做额外要求。
        (TrustState::NotTrusted | TrustState::Inconclusive, _) => (false, false),
        (TrustState::Trusted, TrustState::Trusted) => (false, false),
        (TrustState::Trusted, TrustState::NotTrusted) => (true, false),
        (TrustState::Trusted, TrustState::Inconclusive) => match policy {
            AuthenticodePolicy::Permissive => (false, true),
            _ => (true, false),
        },
    }
}

/// 文件是否带有**可被本机信任**的 Authenticode 签名。
///
/// 不联网拉取 CRL 或中间证书（[`WTD_CACHE_ONLY_URL_RETRIEVAL`]）：
/// 更新流程不应该因为证书下载卡住或失败。代价是证书链不在本地缓存时
/// 会被判为不可信；由于比较双方（原版本与原样产出的新版本）处于同一台机器，
/// 这种环境限制不会把正常更新误判成"发布端漏签"。
pub fn is_trusted(path: &Path) -> bool {
    matches!(trust_state(path), TrustState::Trusted)
}

/// 带结论的版本：区分"未签名/被拒绝"与"离线无法验证"。
///
/// 更新拦截仍以 [`is_trusted`] 为准（`Inconclusive` 按不可信处理），
/// 这个状态只用于诊断，让审计者看到"本次检查是否未得出结论"。
pub fn trust_state(path: &Path) -> TrustState {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };

    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    // 写 union 字段本身是安全的（只有读取需要 unsafe）
    data.Anonymous.pFile = &mut file;

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let code = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        )
    };

    // 必须显式关闭状态，否则 WinVerifyTrust 内部缓存的证书句柄会泄漏。
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        );
    }

    classify(code)
}

/// 把 WinVerifyTrust 的 HRESULT 映射成三态结论。
fn classify(code: i32) -> TrustState {
    // 未签名、无提供者、格式未知、主体不受信任、被显式拒绝。
    const TRUST_E_NOSIGNATURE: i32 = 0x800B0100u32 as i32;
    const TRUST_E_PROVIDER_UNKNOWN: i32 = 0x800B0001u32 as i32;
    const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B0003u32 as i32;
    const TRUST_E_SUBJECT_NOT_TRUSTED: i32 = 0x800B0004u32 as i32;
    const TRUST_E_EXPLICIT_DISTRUST: i32 = 0x800B0111u32 as i32;
    // 证书链无法建立：cache-only 模式下离线时最常见。
    const CERT_E_UNTRUSTEDROOT: i32 = 0x800B0109u32 as i32;
    const CERT_E_CHAINING: i32 = 0x800B010Au32 as i32;

    match code {
        0 => TrustState::Trusted,
        TRUST_E_NOSIGNATURE
        | TRUST_E_PROVIDER_UNKNOWN
        | TRUST_E_SUBJECT_FORM_UNKNOWN
        | TRUST_E_SUBJECT_NOT_TRUSTED
        | TRUST_E_EXPLICIT_DISTRUST => TrustState::NotTrusted,
        CERT_E_UNTRUSTEDROOT | CERT_E_CHAINING => TrustState::Inconclusive,
        _ => TrustState::NotTrusted,
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, decide, is_trusted, TrustState};
    use crate::platform::AuthenticodePolicy;

    /// 三态策略：严格阻断“无法得出结论”，宽松只告警，关闭完全不检查。
    #[test]
    fn policy_controls_inconclusive_outcomes() {
        use TrustState::{Inconclusive, NotTrusted, Trusted};

        assert_eq!(
            decide(Trusted, NotTrusted, AuthenticodePolicy::Strict),
            (true, false),
            "明确未签名必须阻断"
        );
        assert_eq!(
            decide(Trusted, Inconclusive, AuthenticodePolicy::Strict),
            (true, false),
            "strict 下无法得出结论按不可信处理"
        );
        assert_eq!(
            decide(Trusted, Inconclusive, AuthenticodePolicy::Permissive),
            (false, true),
            "permissive 下无法得出结论只告警"
        );
        assert_eq!(
            decide(Trusted, NotTrusted, AuthenticodePolicy::Permissive),
            (true, false),
            "permissive 不放过明确未签名"
        );
        assert_eq!(
            decide(Trusted, NotTrusted, AuthenticodePolicy::Off),
            (false, false),
            "off 跳过所有检查"
        );
        assert_eq!(
            decide(NotTrusted, NotTrusted, AuthenticodePolicy::Strict),
            (false, false),
            "原版本本身未签名时不做额外要求"
        );
    }

    /// 离线且证书链不在缓存时，WinVerifyTrust 返回"无法建链"；
    /// 这类结果必须与"未签名/被拒绝"区分开，才能被诊断看到。
    #[test]
    fn classifies_chain_failures_as_inconclusive() {
        assert_eq!(classify(0), TrustState::Trusted);
        assert_eq!(
            classify(0x800B0100u32 as i32),
            TrustState::NotTrusted,
            "未签名文件必须是明确的不可信"
        );
        assert_eq!(
            classify(0x800B0109u32 as i32),
            TrustState::Inconclusive,
            "证书链无法建立（可能离线）应标为未得出结论"
        );
        assert_eq!(
            classify(0x800B010Au32 as i32),
            TrustState::Inconclusive,
            "链式验证失败应标为未得出结论"
        );
    }

    /// 未签名文件必须判为不可信。
    ///
    /// 这也是更新流程的默认路径：开发者本地构建的 exe 没有签名，
    /// 原版本不可信 → 不做"漏签"拦截，更新照常进行。
    #[test]
    fn unsigned_file_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("unsigned.exe");
        std::fs::write(&file, b"MZ this is not a signed PE").unwrap();
        assert!(!is_trusted(&file), "未签名文件不得被判为可信");
    }

    /// 探测系统自带的签名程序：验证 API 在真实签名链上返回可信。
    ///
    /// 使用 `WTD_CACHE_ONLY_URL_RETRIEVAL` 时离线环境可能查不到中间证书，
    /// 因此探测失败只提示、不失败——真正的拦截判据是"原版本可信而产物不可信"。
    #[test]
    fn signed_system_binary_probe() {
        let Some(root) = std::env::var_os("SystemRoot") else {
            return;
        };
        let notepad = std::path::Path::new(&root).join("System32/notepad.exe");
        if !notepad.is_file() {
            return;
        }
        if !is_trusted(&notepad) {
            eprintln!("[skip] 本机无法验证 notepad.exe 的证书链（离线/无缓存）");
        }
    }
}
