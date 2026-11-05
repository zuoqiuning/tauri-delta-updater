//! 签名链路的回归测试。
//!
//! 覆盖：签名可被正确验证、被篡改的内容必须被拒绝、
//! base64 与原始 minisign 文件两种编码形式都能正确处理。

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tauri_delta_updater::{manifest_version_binding, verify_manifest_signature};

/// 生成一对测试用密钥，返回 (私钥, 公钥 base64)。
fn test_keypair() -> (minisign::SecretKey, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_text = kp.pk.to_box().unwrap().to_string();
    let pk_b64 = STANDARD.encode(pk_text.as_bytes());
    (kp.sk, pk_b64)
}

/// 对内容签名，返回 minisign 签名字符串。
fn sign(sk: &minisign::SecretKey, content: &[u8]) -> String {
    minisign::sign(None, sk, std::io::Cursor::new(content), None, None)
        .unwrap()
        .to_string()
}

/// 对内容签名并写入 trusted comment（版本绑定的载体）。
fn sign_bound(sk: &minisign::SecretKey, content: &[u8], trusted_comment: &str) -> String {
    minisign::sign(
        None,
        sk,
        std::io::Cursor::new(content),
        Some(trusted_comment),
        None,
    )
    .unwrap()
    .to_string()
}

#[test]
fn test_verify_manifest_signature_accepts_base64_form() {
    let (sk, pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0","patches":[]}"#;
    let sig_b64 = STANDARD.encode(sign(&sk, manifest).as_bytes());

    let result = verify_manifest_signature(manifest, &sig_b64, &pk_b64);
    assert!(
        result.is_ok(),
        "Tauri 约定的 base64 形式必须能验证通过: {:?}",
        result.err()
    );
}

#[test]
fn test_verify_manifest_signature_accepts_raw_form() {
    // 关键回归：生成器写出的 latest.json.sig 是原始 minisign 文本，
    // 验签接口必须能直接处理，不能要求调用方手工 base64。
    let (sk, pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0","patches":[]}"#;
    let sig_text = sign(&sk, manifest);

    let result = verify_manifest_signature(manifest, &sig_text, &pk_b64);
    assert!(
        result.is_ok(),
        "原始 .sig 文件内容必须能直接验证通过: {:?}",
        result.err()
    );
}

#[test]
fn test_verify_manifest_signature_rejects_tampered_manifest() {
    let (sk, pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0","patches":[{"target_hash":"good"}]}"#;
    let sig_text = sign(&sk, manifest);

    // 篡改 target_hash —— 这正是签名要防护的攻击
    let tampered = br#"{"version":"0.2.0","patches":[{"target_hash":"evil"}]}"#;
    let result = verify_manifest_signature(tampered, &sig_text, &pk_b64);
    assert!(result.is_err(), "内容被篡改后必须拒绝验证");
    assert!(
        matches!(
            result,
            Err(tauri_delta_updater::DeltaError::SignatureInvalid(_))
        ),
        "签名失败必须归类为 SignatureInvalid，便于与文件损坏区分"
    );
}

#[test]
fn test_verify_manifest_signature_rejects_wrong_public_key() {
    let (sk, _) = test_keypair();
    let (_, other_pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0"}"#;
    let sig_text = sign(&sk, manifest);

    let result = verify_manifest_signature(manifest, &sig_text, &other_pk_b64);
    assert!(result.is_err(), "公钥不匹配时必须拒绝验证");
}

#[test]
fn test_verify_manifest_signature_rejects_garbage_input() {
    let (_, pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0"}"#;

    for bad in ["", "not-base64!!", "!!!", "aGVsbG8="] {
        let result = verify_manifest_signature(manifest, bad, &pk_b64);
        assert!(result.is_err(), "非法签名输入必须被拒绝: {bad:?}");
    }

    // 公钥同样不能接受非法输入
    let result = verify_manifest_signature(manifest, "AAAA", "not-a-key");
    assert!(result.is_err(), "非法公钥必须被拒绝");
}

#[test]
fn test_verify_manifest_signature_file() {
    let (sk, pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0","patches":[]}"#;
    let sig_text = sign(&sk, manifest);

    let dir = tempfile::tempdir().unwrap();
    let sig_path = dir.path().join("latest.json.sig");
    std::fs::write(&sig_path, &sig_text).unwrap();

    let result = tauri_delta_updater::verify_manifest_signature_file(manifest, &sig_path, &pk_b64);
    assert!(
        result.is_ok(),
        "从 .sig 文件验证必须通过: {:?}",
        result.err()
    );

    // 文件内容被篡改同样必须被拒绝
    std::fs::write(&sig_path, "tampered").unwrap();
    let result = tauri_delta_updater::verify_manifest_signature_file(manifest, &sig_path, &pk_b64);
    assert!(result.is_err(), "签名文件被篡改后必须拒绝验证");
}

#[test]
fn test_manifest_version_binding_extraction() {
    let (sk, _pk_b64) = test_keypair();
    let manifest = br#"{"version":"0.2.0"}"#;

    // generate 0.2.6+ 的签名带版本绑定
    let bound = sign_bound(&sk, manifest, "tdu:v=0.2.0");
    assert_eq!(
        manifest_version_binding(&bound).as_deref(),
        Some("0.2.0"),
        "必须能从 trusted comment 提取版本绑定"
    );

    // Tauri 的 base64 形式同样必须能提取
    let bound_b64 = STANDARD.encode(bound.as_bytes());
    assert_eq!(
        manifest_version_binding(&bound_b64).as_deref(),
        Some("0.2.0")
    );

    // 旧版发布工具不写绑定 → None（按兼容处理，不启用重放防护）
    let unbound = sign(&sk, manifest);
    assert!(manifest_version_binding(&unbound).is_none());

    // 绑定格式不符（非 tdu:v= 前缀）也按无绑定处理
    let other_comment = sign_bound(&sk, manifest, "some other comment");
    assert!(manifest_version_binding(&other_comment).is_none());

    // 未来扩展参数（; 分隔）只取版本段，旧解析端不误判
    let extended = sign_bound(&sk, manifest, "tdu:v=0.2.0;app=demo;plat=windows-x86_64");
    assert_eq!(
        manifest_version_binding(&extended).as_deref(),
        Some("0.2.0"),
        "扩展参数不得污染版本段"
    );
}
