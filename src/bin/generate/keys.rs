//! minisign 私钥加载与公钥导出。

use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::path::Path;

/// 读取私钥。
///
/// 支持三种来源，兼容本地开发与 CI 两种用法：
/// - 指向密钥文件的路径
/// - base64 编码的密钥内容（Tauri CI 中 `TAURI_SIGNING_PRIVATE_KEY` 的格式）
/// - 密钥箱文本或原始二进制密钥
///
/// 注意：绝不能调用 `into_secret_key(None)` —— 它会在无 TTY 的环境下
/// 交互式索要口令，导致 CI 流程挂起。这里始终显式传入口令。
pub(crate) fn load_secret_key(
    source: &str,
    password: Option<&str>,
) -> Result<minisign::SecretKey, String> {
    // 1. 路径指向真实文件
    let path = Path::new(source);
    if path.is_file() {
        let raw = std::fs::read(path).map_err(|e| format!("读取私钥失败: {e}"))?;
        return parse_secret_key(&raw, password);
    }

    // 2. base64 编码的内容（CI 常用形式）
    if let Ok(decoded) = STANDARD.decode(source.trim()) {
        if let Ok(key) = parse_secret_key(&decoded, password) {
            return Ok(key);
        }
    }

    // 3. 直接就是密钥内容
    parse_secret_key(source.as_bytes(), password)
}

/// 从原始字节还原私钥。
///
/// 字节可能是 minisign 密钥箱文本（第二行是 base64 负载），
/// 也可能是 158 字节的原始密钥。
pub(crate) fn parse_secret_key(
    raw: &[u8],
    password: Option<&str>,
) -> Result<minisign::SecretKey, String> {
    if let Ok(text) = std::str::from_utf8(raw) {
        if let Some(payload) = base64_payload(text) {
            // 有口令时按密钥箱解密，错误口令会被直接拒绝
            if let Some(pwd) = password {
                return minisign::SecretKeyBox::from_string(text)
                    .map_err(|e| format!("私钥格式错误: {e}"))?
                    .into_secret_key(Some(pwd.to_string()))
                    .map_err(|e| format!("私钥口令错误或私钥损坏: {e}"));
            }

            // 未加密的私钥：负载即原始密钥字节
            if is_unencrypted_payload(&payload) {
                return minisign::SecretKey::from_bytes(&payload)
                    .map_err(|e| format!("无法解析私钥: {e}"));
            }

            // 加密私钥但未提供口令：明确报错，而不是退回原始字节
            // （原始字节解析会得到一个错误的密钥，签名结果无法通过校验）
            return Err("私钥已加密，请提供口令".to_string());
        }
    }

    // 原始二进制私钥
    minisign::SecretKey::from_bytes(raw).map_err(|e| format!("无法解析私钥: {e}"))
}

/// 提取密钥箱文本中的 base64 负载。
pub(crate) fn base64_payload(text: &str) -> Option<Vec<u8>> {
    let line = text.lines().nth(1)?.trim();
    if line.is_empty() {
        return None;
    }
    STANDARD.decode(line).ok()
}

/// 判断密钥负载是否为未加密的原始私钥。
///
/// 依据 minisign 密钥格式（参考 minisign 0.7.x `SecretKey::from_bytes` 的布局）：
/// 负载依次为 sig_alg(2) + kdf_alg(2) + chk_alg(2) + kdf_salt(32) +
/// opslimit(8) + memlimit(8) + keynum(8) + sk(64) + chk(32)，
/// 其中末尾 32 字节是校验和。**未加密**密钥的校验和字段在 `to_box` 前恒为零，
/// 加密时会被 scrypt 流异或为非零值——以此区分两种密钥。
///
/// 注意：这是对 minisign crate 内部布局的经验性依赖。
/// 升级 `minisign` 依赖时必须回归验证：
/// 用 `KeyPair::generate_unencrypted_keypair` 与 `generate_encrypted_keypair`
/// 各生成一份密钥，确认本函数判定结果不变。
pub(crate) fn is_unencrypted_payload(payload: &[u8]) -> bool {
    const CHK_BYTES: usize = 32;
    payload.len() > CHK_BYTES && payload[payload.len() - CHK_BYTES..].iter().all(|b| *b == 0)
}

/// 从私钥导出公钥，供客户端校验签名。
pub(crate) fn derive_public_key(sk: &minisign::SecretKey) -> Result<minisign::PublicKey, String> {
    minisign::PublicKey::from_secret_key(sk).map_err(|e| format!("无法导出公钥: {e}"))
}
