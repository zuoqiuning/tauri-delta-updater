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

/// 把签名文本拆成独立的 minisign 签名块。
///
/// 一个 `.sig` 文件可以包含多个签名（每个都以 `untrusted comment:` 开头），
/// 用于密钥轮换期的双签名。单签名文件的解析结果与拆分前完全一致。
fn split_signature_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with(MINISIGN_COMMENT_PREFIX) && !current.is_empty() {
            blocks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// 信任策略：多个受信任密钥时，需要满足什么条件。
///
/// 密钥轮换的推荐路径：先把新旧两把公钥都固化进客户端（`AnyOf`），
/// 发布端双签一段时间；确认旧客户端都升级后，再让新版本只信任新密钥。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TrustPolicy {
    /// 任意一把受信任密钥的有效签名即可。默认，适合密钥轮换的过渡期。
    #[default]
    AnyOf,
    /// **所有**受信任密钥都必须签名（双签强制）。
    ///
    /// 发布端漏签一把就会让所有更新失败；这是有意的强约束，
    /// 只在确实需要"两把钥匙同时在场"的窗口使用。
    AllOf,
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
    verify_manifest_signature_with(manifest_content, signature, public_key_b64, true)
}

/// 与 [`verify_manifest_signature`] 相同，但可显式指定是否接受旧版（legacy）签名。
///
/// 本项目发布端固定 minisign 0.7.9，产出的是 pre-hash 格式（有单测守住）；
/// `allow_legacy = true` 只用于兼容第三方历史管线。宿主可通过
/// [`crate::UpdateConfig::allow_legacy_signatures`] 收紧为 `false`。
///
/// # 兼容性
///
/// 关闭后，用 minisign < 0.7 生成的清单会被拒绝。这是签名校验行为变更，
/// 默认值保持 `true`，避免升级时破坏现有发布管线。
pub fn verify_manifest_signature_with(
    manifest_content: &[u8],
    signature: &str,
    public_key_b64: &str,
    allow_legacy: bool,
) -> Result<()> {
    verify_manifest_signature_multi(
        manifest_content,
        signature,
        &[public_key_b64],
        TrustPolicy::AnyOf,
        allow_legacy,
    )
}

/// 用一组受信任密钥校验签名（支持双签 / 密钥轮换）。
///
/// `keys` 至少包含一把公钥；`policy` 决定任意一把还是全部都要签。
/// 签名文本可以包含多个 `minisign` 签名块（发布端双签时按块拼接）。
pub fn verify_manifest_signature_multi(
    content: &[u8],
    signature: &str,
    keys: &[&str],
    policy: TrustPolicy,
    allow_legacy: bool,
) -> Result<()> {
    if keys.is_empty() {
        return Err(DeltaError::SignatureInvalid(
            "没有配置任何受信任公钥".to_string(),
        ));
    }

    let text = decode_minisign_text(signature, "签名")?;
    let blocks = split_signature_blocks(&text);
    let signatures: Vec<Signature> = blocks
        .iter()
        .filter_map(|block| Signature::decode(block).ok())
        .collect();
    if signatures.is_empty() {
        return Err(DeltaError::SignatureInvalid(
            "签名无效：没有任何可解析的 minisign 签名块".to_string(),
        ));
    }

    // 公钥格式错误是配置问题，必须显式报出来，而不是伪装成"签名被篡改"。
    let mut parsed_keys = Vec::with_capacity(keys.len());
    for key in keys {
        parsed_keys.push(parse_public_key(key)?);
    }

    let verify_one = |key: &PublicKey| {
        signatures
            .iter()
            .any(|signature| key.verify(content, signature, allow_legacy).is_ok())
    };

    let tampered = || {
        DeltaError::SignatureInvalid("清单签名校验失败！文件可能已被篡改，已阻断更新。".to_string())
    };

    match policy {
        TrustPolicy::AnyOf => {
            if parsed_keys.iter().any(verify_one) {
                Ok(())
            } else {
                Err(tampered())
            }
        }
        TrustPolicy::AllOf => {
            let missing = parsed_keys.iter().filter(|key| !verify_one(key)).count();
            if missing == 0 {
                Ok(())
            } else {
                Err(DeltaError::SignatureInvalid(format!(
                    "双签校验失败：{missing} 把受信任密钥没有提供有效签名（trust_policy = all 要求全部签名）"
                )))
            }
        }
    }
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
    verify_file_signature_multi(file, signature, &[public_key_b64], TrustPolicy::AnyOf)
}

/// 用一组受信任密钥校验磁盘上大文件的签名（支持双签）。
///
/// 与 [`verify_manifest_signature_multi`] 使用同一套信任策略；
/// 用只读 mmap 读取，不把安装包整体读进内存。
pub fn verify_file_signature_multi(
    file: &Path,
    signature: &str,
    keys: &[&str],
    policy: TrustPolicy,
) -> Result<()> {
    let opened = std::fs::File::open(file)?;
    // mmap 只读，文件在验签期间不会被写；所有权由 `opened` 保证。
    let mapped = unsafe { memmap2::Mmap::map(&opened)? };
    verify_manifest_signature_multi(&mapped, signature, keys, policy, true)
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
    let block = split_signature_blocks(&text).into_iter().next()?;
    let sig = Signature::decode(&block).ok()?;
    sig.trusted_comment()
        .strip_prefix(VERSION_BINDING_PREFIX)?
        .split(';')
        .next()
        .map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        manifest_version_binding, verify_manifest_signature, verify_manifest_signature_multi,
        TrustPolicy,
    };
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    fn keypair() -> (minisign::SecretKey, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk_b64 = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
        (kp.sk, pk_b64)
    }

    fn sign_with(keys: &[&minisign::SecretKey], content: &[u8]) -> String {
        keys.iter()
            .map(|sk| {
                minisign::sign(
                    None,
                    sk,
                    std::io::Cursor::new(content),
                    Some("tdu:v=1.0.0"),
                    None,
                )
                .unwrap()
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 双签的 AnyOf：任意一把受信任密钥的有效签名都接受。
    #[test]
    fn any_of_accepts_either_key() {
        let (sk_old, pk_old) = keypair();
        let (_sk_new, pk_new) = keypair();
        let content = b"{\"version\":\"1.0.0\"}";

        let signed_by_old = sign_with(&[&sk_old], content);
        let keys = [pk_old.as_str(), pk_new.as_str()];
        assert!(verify_manifest_signature_multi(
            content,
            &signed_by_old,
            &keys,
            TrustPolicy::AnyOf,
            true
        )
        .is_ok());
        assert!(
            verify_manifest_signature_multi(
                content,
                &signed_by_old,
                &keys,
                TrustPolicy::AllOf,
                true
            )
            .is_err(),
            "只签一把时 AllOf 必须拒绝"
        );
    }

    /// 双签的 AllOf：两把密钥都签名才通过。
    #[test]
    fn all_of_requires_both_signatures() {
        let (sk_old, pk_old) = keypair();
        let (sk_new, pk_new) = keypair();
        let content = b"{}";
        let keys = [pk_old.as_str(), pk_new.as_str()];

        let both = sign_with(&[&sk_old, &sk_new], content);
        assert!(
            verify_manifest_signature_multi(content, &both, &keys, TrustPolicy::AllOf, true)
                .is_ok()
        );

        // 内容被换掉后，两块签名都必须失效。
        assert!(verify_manifest_signature_multi(
            b"{\"v\":2}",
            &both,
            &keys,
            TrustPolicy::AllOf,
            true
        )
        .is_err());
    }

    /// 多块签名里的版本绑定必须仍可读取（取第一块）。
    #[test]
    fn multi_block_version_binding_reads_first_block() {
        let (sk_old, _pk_old) = keypair();
        let (sk_new, _pk_new) = keypair();
        let signed = sign_with(&[&sk_old, &sk_new], b"{}");
        assert_eq!(
            manifest_version_binding(&signed).as_deref(),
            Some("1.0.0"),
            "双签时版本绑定不能被多块拼接破坏"
        );
    }

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
