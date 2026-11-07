use crate::error::{DeltaError, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use minisign_verify::{PublicKey, Signature};
use std::path::Path;

/// minisign 文件（.pub / .sig）都以这条注释开头。
///
/// 用它来区分"原始文件内容"与"base64 编码后的文件内容"，
/// 两种形式在 Tauri 生态里都会出现，必须都能正确解析。
const MINISIGN_COMMENT_PREFIX: &str = "untrusted comment:";

/// 把输入还原成 minisign 文件的原始文本。
///
/// 接受两种形式：
/// - 原始文本（.pub / .sig 文件内容）
/// - base64 编码后的文件内容（Tauri 配置与 `latest.json` 里的 `signature` 字段采用此形式）
fn decode_minisign_text(input: &str, what: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.starts_with(MINISIGN_COMMENT_PREFIX) {
        return Ok(trimmed.to_string());
    }
    let bytes = STANDARD
        .decode(trimmed)
        .map_err(|e| DeltaError::SignatureInvalid(format!("{what}不是合法的 base64: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|_| DeltaError::SignatureInvalid(format!("{what}解码后不是合法的 UTF-8 文本")))
}

/// 解析公钥。接受 base64（与 Tauri 配置一致）或原始 `.pub` 文件内容。
fn parse_public_key(input: &str) -> Result<PublicKey> {
    let text = decode_minisign_text(input, "公钥")?;
    PublicKey::decode(&text).map_err(|e| DeltaError::SignatureInvalid(format!("公钥无效: {e}")))
}

/// 解析签名。接受 base64 或原始 `.sig` 文件内容。
fn parse_signature(input: &str) -> Result<Signature> {
    let text = decode_minisign_text(input, "签名")?;
    Signature::decode(&text).map_err(|e| DeltaError::SignatureInvalid(format!("签名无效: {e}")))
}

/// 校验 `latest.json` 的签名，防止中间人劫持篡改 `patches` 中的 `target_hash`。
///
/// `signature` 与 `public_key_b64` 都接受 base64 编码或 minisign 原始文件内容。
///
/// 验签失败意味着清单被篡改或密钥不匹配，调用方必须拒绝更新。
pub fn verify_manifest_signature(
    manifest_content: &[u8],
    signature: &str,
    public_key_b64: &str,
) -> Result<()> {
    let public_key = parse_public_key(public_key_b64)?;
    let signature = parse_signature(signature)?;

    // 第三个参数 true 对应 minisign-verify 的 allow_legacy 语义：
    // 同时接受旧格式（非预哈希）与新版预哈希签名。
    // 这是刻意的兼容取舍——发布链路可能混用两种 minisign 版本生成；
    // 若确认所有签名都由新版工具生成，可收紧为 false 以缩小攻击面。
    public_key
        .verify(manifest_content, &signature, true)
        .map_err(|_| {
            DeltaError::SignatureInvalid(
                "清单签名校验失败！文件可能已被篡改，已阻断更新。".to_string(),
            )
        })
}

/// 从磁盘上的 `.sig` 文件校验 `latest.json` 的签名。
///
/// 这是配合 [`crate::bin::generate`] 生成的 `latest.json.sig` 使用的入口。
pub fn verify_manifest_signature_file(
    manifest_content: &[u8],
    signature_path: &Path,
    public_key_b64: &str,
) -> Result<()> {
    let raw = std::fs::read_to_string(signature_path)?;
    verify_manifest_signature(manifest_content, &raw, public_key_b64)
}

/// 校验磁盘上大文件的签名（全量安装包、归档）。
///
/// 用只读 mmap 读取，而不是 `fs::read`：安装包通常几十到几百 MB，
/// 一次性读进内存会让更新进程的 RSS 突然翻倍。
///
/// `signature` 与 `public_key_b64` 的格式与 [`verify_manifest_signature`] 相同，
/// 因此可以直接喂 `latest.json` 里的 `platforms[].signature`。
pub fn verify_file_signature(file: &Path, signature: &str, public_key_b64: &str) -> Result<()> {
    let opened = std::fs::File::open(file)?;
    // mmap 只读，文件在验签期间不会被写；所有权由 `opened` 保证。
    let mapped = unsafe { memmap2::Mmap::map(&opened)? };
    verify_manifest_signature(&mapped, signature, public_key_b64)
}

/// 版本绑定的 trusted comment 前缀。
///
/// generate 签发清单签名时把目标版本写进 trusted comment（`tdu:v=<version>`）。
/// minisign 的 global signature 覆盖 trusted comment——验签通过即证明
/// 该绑定出自发布者之手，客户端借此拒绝"用旧版本的合法签名重放旧清单"。
/// 未来若扩展更多键值（如 `tdu:v=1.0;app=foo`），用 `;` 分隔，
/// 解析端只取版本段，因此旧客户端不受新增参数影响。
const VERSION_BINDING_PREFIX: &str = "tdu:v=";

/// 提取清单签名里的版本绑定。
///
/// trusted comment 是签名的一部分（global signature 覆盖它），
/// 因此**必须在本函数返回的绑定被校验之前先完成验签**——
/// 未验签的文本不可信。旧版发布工具不写绑定，返回 `None`；
/// 无绑定的清单按兼容处理（不启用重放防护）。
///
/// 按前缀匹配 + 取 `;` 前第一段解析，而不是整行相等：
/// 未来追加键值参数时，旧解析端仍能取到版本段。
///
/// `signature` 接受 base64 编码或 minisign 原始 `.sig` 文本。
pub fn manifest_version_binding(signature: &str) -> Option<String> {
    let text = decode_minisign_text(signature, "签名").ok()?;
    let sig = Signature::decode(&text).ok()?;
    sig.trusted_comment()
        .strip_prefix(VERSION_BINDING_PREFIX)?
        .split(';')
        .next()
        .map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::{manifest_version_binding, verify_manifest_signature};
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    /// 发布端（minisign 0.7.9）产出的签名必须是 pre-hash 格式。
    ///
    /// 验签端当前为了兼容历史发布开启 `allow_legacy`；一旦确认所有签名都是
    /// pre-hash，就可以安全关闭，缩小攻击面。这个测试守住该前提。
    #[test]
    fn minisign_079_signatures_are_prehashed() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_text = kp.pk.to_box().unwrap().to_string();
        let public_key = minisign_verify::PublicKey::decode(&pk_text).unwrap();

        let content = b"{\"version\":\"0.2.0\"}";
        let signed =
            minisign::sign(None, &kp.sk, std::io::Cursor::new(content), None, None).unwrap();
        let signature = minisign_verify::Signature::decode(&signed.to_string()).unwrap();

        assert!(
            public_key.verify(content, &signature, false).is_ok(),
            "minisign 0.7.9 的签名必须能在 allow_legacy=false 下验证通过"
        );
    }

    /// 版本绑定从 trusted comment 读取；没有绑定或格式不符时返回 None。
    #[test]
    fn version_binding_is_parsed_from_trusted_comment() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_b64 = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
        let content = b"{}";

        let bound = minisign::sign(
            None,
            &kp.sk,
            std::io::Cursor::new(content),
            Some("tdu:v=1.2.3;app=demo"),
            None,
        )
        .unwrap()
        .to_string();
        assert_eq!(manifest_version_binding(&bound).as_deref(), Some("1.2.3"));
        assert!(verify_manifest_signature(content, &bound, &pk_b64).is_ok());

        let unbound = minisign::sign(None, &kp.sk, std::io::Cursor::new(content), None, None)
            .unwrap()
            .to_string();
        assert!(manifest_version_binding(&unbound).is_none());
    }
}
