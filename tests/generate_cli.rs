//! `generate` 命令行工具的端到端测试。
//!
//! 直接跑编译出来的二进制，验证参数解析、主程序/安装包分离、补丁生成与清单产出，
//! 覆盖纯单元测试抓不到的环节（尤其是私钥的三种来源与跨平台推断）。
//!
//! 需要启用 `generator` feature：
//! `cargo test --all-features`。

#![cfg(feature = "generator")]

use std::path::{Path, PathBuf};
use std::process::Command;

/// 生成一对未加密的 minisign 密钥。
/// 返回 (密钥对象, 私钥文本, 公钥 base64)。
fn keypair() -> (minisign::SecretKey, String, String) {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let sk_text = kp.sk.to_box(None).unwrap().into_string();
    let pk = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
    (kp.sk, sk_text, pk)
}

/// 造两个内容不同、体积相近的二进制。
fn make_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let old = dir.join("App_0.1.0_x64.exe");
    let new = dir.join("App_0.1.1_x64.exe");
    let old_data: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
    let new_data: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 241) as u8).collect();
    std::fs::write(&old, &old_data).unwrap();
    std::fs::write(&new, &new_data).unwrap();
    (old, new)
}

/// 运行 `generate`，返回 (退出码, stdout, stderr)。
fn run_generate(args: &[String], cwd: &Path) -> (i32, String, String) {
    let exe = env!("CARGO_BIN_EXE_generate");
    let output = Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("必须能启动 generate");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn s(value: impl AsRef<std::ffi::OsStr>) -> String {
    value.as_ref().to_string_lossy().to_string()
}

/// 端到端：主程序与安装包分离，生成清单、补丁与安装包签名。
#[test]
fn test_generate_cli_produces_manifest_and_patch() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);

    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    // 安装包与主程序是两份不同的字节；体积需接近真实安装包，
    // 否则 --max-patch-ratio 的默认阈值会把正常补丁判成"无收益"
    let installer = dir.join("App_0.1.1_x64-setup.exe");
    let mut installer_bytes = vec![0u8; 64 * 1024];
    let header = b"installer payload, not the app";
    installer_bytes[..header.len()].copy_from_slice(header);
    std::fs::write(&installer, &installer_bytes).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let base_url = "https://cdn.example.com";
    let args = vec![
        "--old-dir".into(),
        s(&old_versions),
        "--app-binary".into(),
        s(&new_exe),
        "--installer".into(),
        s(&installer),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        base_url.into(),
        "--private-key".into(),
        s(&key_file),
        "--notes".into(),
        "test notes".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], "0.1.1");
    assert_eq!(manifest["notes"], "test notes");

    // 补丁必须按平台分组，且目标哈希是**主程序**的哈希
    let patch = &manifest["patches"]["windows-x86_64"][0];
    assert_eq!(patch["from_version"], "0.1.0");
    assert_eq!(patch["to_version"], "0.1.1");
    assert_eq!(
        patch["patch_url"],
        "https://cdn.example.com/0.1.0_to_0.1.1.patch"
    );
    assert_eq!(
        patch["target_hash"],
        sha256_hex(&std::fs::read(&new_exe).unwrap()),
        "target_hash 必须是新主程序的哈希，不是安装包的哈希"
    );
    assert_eq!(
        patch["scope"], "main_executable_only",
        "补丁必须显式声明只覆盖主程序，宿主才能提示 sidecar/resources 风险"
    );

    // 平台条目必须指向安装包，而不是主程序
    let platform = &manifest["platforms"]["windows-x86_64"];
    assert_eq!(
        platform["url"],
        "https://cdn.example.com/App_0.1.1_x64-setup.exe"
    );
    let sig_b64 = platform["signature"].as_str().unwrap();
    let sig_text = STANDARD.decode(sig_b64).unwrap();
    let sig_box = minisign::SignatureBox::from_string(
        &String::from_utf8(sig_text).expect("签名必须是 UTF-8"),
    )
    .expect("必须是合法的 minisign 签名");
    let public_key = minisign::PublicKey::from_secret_key(&_sk).unwrap();
    minisign::verify(
        &public_key,
        &sig_box,
        std::io::Cursor::new(std::fs::read(&installer).unwrap()),
        true,
        false,
        true,
    )
    .expect("platforms[].signature 必须能验证**安装包**");

    // 补丁文件必须真的生成
    assert!(old_versions.join("0.1.0_to_0.1.1.patch").exists());
    // 公钥文件带警告但仍是合法密钥箱
    let pub_text = std::fs::read_to_string(dir.join("latest.json.pub")).unwrap();
    assert!(pub_text.starts_with("untrusted comment: DO NOT TRUST"));
    minisign::PublicKey::from_box(
        minisign::PublicKeyBox::from_string(&pub_text).expect("公钥必须可解析"),
    )
    .expect("公钥必须可解析");
}

/// 版本号带 `v` 前缀时必须被规范化，且签名的版本绑定同为规范化形态。
#[test]
fn test_generate_cli_normalizes_v_prefix() {
    let dir = tempdir();
    let (_old_exe, new_exe) = make_pair(&dir);

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_versions),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "v0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], "0.1.1");

    let sig_text = std::fs::read_to_string(dir.join("latest.json.sig")).unwrap();
    assert_eq!(
        tauri_delta_updater::manifest_version_binding(&sig_text).as_deref(),
        Some("0.1.1"),
        "trusted comment 的版本绑定必须是规范化后的版本号"
    );
}

/// 私钥直接以 base64 传入（CI 里 `TAURI_SIGNING_PRIVATE_KEY` 的格式）也必须能用。
#[test]
fn test_generate_cli_accepts_base64_key() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let dir = tempdir();
    let (_old_exe, new_exe) = make_pair(&dir);

    let (_sk, sk_text, _pk) = keypair();
    let b64 = STANDARD.encode(sk_text.as_bytes());

    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_versions),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        b64,
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(dir.join("latest.json").exists());
}

/// 私钥已加密但没提供口令时必须明确报错，而不是悄悄用错误的密钥签名。
#[test]
fn test_generate_cli_reports_missing_password() {
    let dir = tempdir();
    let (_old_exe, new_exe) = make_pair(&dir);

    let kp = minisign::KeyPair::generate_encrypted_keypair(Some("secret".to_string())).unwrap();
    let encrypted = kp.sk.to_box(None).unwrap().into_string();

    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_versions),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        encrypted,
        // 显式传空口令，避免 CI 环境的 TAURI_SIGNING_PRIVATE_KEY_PASSWORD 干扰
        "--password".into(),
        String::new(),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "私钥加密且无口令时必须失败");
    assert!(
        stderr.contains("口令"),
        "错误信息必须说明缺少口令，实际: {stderr}"
    );
    assert!(!dir.join("latest.json").exists(), "失败时不得产出清单");
}

/// 平台覆盖与文件名推断不一致时：告警并继续，且不生成跨架构补丁。
#[test]
fn test_generate_cli_warns_on_platform_mismatch() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    // 文件名是 x64（推断 windows-x86_64），却显式指定 darwin-aarch64
    let args = vec![
        "--old-dir".into(),
        s(&old_versions),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--platform".into(),
        "darwin-aarch64".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "平台不一致目前只告警不拒绝\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("不一致"),
        "必须显式告警平台不一致，实际 stdout: {stdout}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert!(
        manifest["platforms"]["darwin-aarch64"].is_object(),
        "平台条目必须写入显式指定的平台键"
    );
    assert!(
        manifest["patches"]["darwin-aarch64"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        "架构不同的旧主程序不得生成补丁"
    );
}

/// 非 Windows 平台也能生成补丁：不能被文件名里没有 `.exe` 卡住。
#[test]
fn test_generate_cli_supports_linux_binaries() {
    let dir = tempdir();
    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();

    // Linux 裸二进制没有扩展名
    let old_bin = old_dir.join("app_0.1.0_amd64");
    let new_bin = dir.join("app_0.1.1_amd64");
    std::fs::write(
        &old_bin,
        (0..32 * 1024u32)
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    std::fs::write(
        &new_bin,
        (0..32 * 1024u32)
            .map(|i| (i % 241) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_dir),
        "--app-binary".into(),
        s(&new_bin),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--platform".into(),
        "linux-x86_64".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "Linux 主程序必须能生成补丁\nstdout: {stdout}\nstderr: {stderr}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["patches"]["linux-x86_64"]
            .as_array()
            .map(|a| a.len()),
        Some(1),
        "必须为 Linux 旧主程序生成补丁，实际清单: {manifest}"
    );
    assert_eq!(
        manifest["platforms"]["linux-x86_64"]["url"], "https://cdn.example.com/app_0.1.1_amd64",
        "未提供 --installer 时，全量地址退回主程序路径（裸二进制分发）"
    );
    assert!(old_dir.join("0.1.0_to_0.1.1.patch").exists());
}

/// `--dry-run` 只规划与验证，不写任何文件。
#[test]
fn test_generate_cli_dry_run_writes_nothing() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);

    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::copy(&old_exe, old_dir.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_dir),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--dry-run".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "dry-run 必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("[DRY]"), "必须打印计划，实际: {stdout}");
    assert!(!dir.join("latest.json").exists(), "dry-run 不得写清单");
    assert!(
        !old_dir.join("0.1.0_to_0.1.1.patch").exists(),
        "dry-run 不得写补丁"
    );
}

/// 补丁体积接近安装包时应跳过（`--max-patch-ratio`）。
#[test]
fn test_generate_cli_skips_low_value_patch() {
    let dir = tempdir();
    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();

    // 完全不同的高熵内容（64 位 LCG，避免周期性数据被 bsdiff 找到大量匹配）：
    // 补丁体积接近全量
    let mut state = 0x1234_5678_9abc_def0u64;
    let old_data: Vec<u8> = (0..64 * 1024)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect();
    let mut state = 0x0fed_cba9_8765_4321u64;
    let new_data: Vec<u8> = (0..64 * 1024)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect();
    let old_bin = old_dir.join("App_0.1.0_x64.exe");
    let new_bin = dir.join("App_0.1.1_x64.exe");
    std::fs::write(&old_bin, &old_data).unwrap();
    std::fs::write(&new_bin, &new_data).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_dir),
        "--app-binary".into(),
        s(&new_bin),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--max-patch-ratio".into(),
        "0.1".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "跳过劣质补丁不应导致失败\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("已跳过"),
        "必须记录跳过原因，实际 stdout: {stdout}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert!(
        manifest["patches"]["windows-x86_64"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        "劣质补丁不得写入清单"
    );
}

/// 计算字节的 SHA-256（小写十六进制）。
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 生成一个临时目录。
fn tempdir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tdx-generate-test-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
