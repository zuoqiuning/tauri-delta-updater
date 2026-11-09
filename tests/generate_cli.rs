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

/// `--from` 是 `--old-dir` / `--old-binary` 的统一别名：目录与文件都要识别。
#[test]
fn test_generate_cli_accepts_from_alias() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();
    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--from".to_string(),
        s(&old_versions),
        "--app-binary".to_string(),
        s(&new_exe),
        "--version".to_string(),
        "0.1.1".to_string(),
        "--base-url".to_string(),
        "https://cdn.example.com".to_string(),
        "--private-key".to_string(),
        s(&key_file),
        "--output-dir".to_string(),
        s(dir.join("dist")),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(code, 0, "--from 别名必须可用: {stdout}\n{stderr}");
}

/// 发布源默认必须 HTTPS；显式 `--allow-http` 后才允许明文。
#[test]
fn test_generate_cli_rejects_http_base_url() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();
    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let mut args = vec![
        "--old-dir".to_string(),
        s(&old_versions),
        "--app-binary".to_string(),
        s(&new_exe),
        "--version".to_string(),
        "0.1.1".to_string(),
        "--base-url".to_string(),
        "http://cdn.example.com".to_string(),
        "--private-key".to_string(),
        s(&key_file),
        "--output-dir".to_string(),
        s(dir.join("dist")),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "明文 HTTP 默认必须失败");
    assert!(stderr.contains("allow-http"), "{stderr}");

    args.push("--allow-http".to_string());
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(code, 0, "显式允许后应成功: {stdout}\n{stderr}");
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
        manifest["patches"]
            .get("darwin-aarch64")
            .and_then(|p| p.as_array())
            .is_none_or(|a| a.is_empty()),
        "架构不同的旧主程序不得生成补丁（空平台键会被省略）"
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

/// 多个基线并行生成：补丁、报告与清单必须完整且稳定。
#[test]
fn test_generate_cli_parallel_baselines() {
    let dir = tempdir();
    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();

    // 三个基线内容各不相同，确保三条补丁都真正生成
    let versions = ["0.1.0", "0.1.1", "0.1.2"];
    for (version, modulus) in versions.iter().zip([251u32, 239, 233]) {
        let data: Vec<u8> = (0..48 * 1024u32).map(|i| (i % modulus) as u8).collect();
        std::fs::write(old_dir.join(format!("App_{version}_x64.exe")), data).unwrap();
    }
    let new_bin = dir.join("App_0.2.0_x64.exe");
    let new_data: Vec<u8> = (0..48 * 1024u32).map(|i| (i % 229) as u8).collect();
    std::fs::write(&new_bin, &new_data).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();
    let report = dir.join("report.json");

    let args = vec![
        "--old-dir".into(),
        s(&old_dir),
        "--app-binary".into(),
        s(&new_bin),
        "--version".into(),
        "0.2.0".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--platform".into(),
        "windows-x86_64".into(),
        "--report".into(),
        s(&report),
        "--jobs".into(),
        "3".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "多基线并行生成必须成功\nstdout: {stdout}\nstderr: {stderr}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    let patches = manifest["patches"]["windows-x86_64"]
        .as_array()
        .expect("清单必须包含补丁数组");
    let mut from_versions: Vec<&str> = patches
        .iter()
        .map(|patch| patch["from_version"].as_str().unwrap())
        .collect();
    from_versions.sort_unstable();
    assert_eq!(from_versions, versions, "三个基线都必须有补丁");

    for version in versions {
        assert!(
            old_dir.join(format!("{version}_to_0.2.0.patch")).exists(),
            "{version} 的补丁文件必须落盘"
        );
    }

    let report_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(
        report_json["patches"].as_array().map(|a| a.len()),
        Some(3),
        "报告必须完整记录三个基线: {report_json}"
    );
    assert_eq!(
        report_json["skipped"].as_array().map(|a| a.len()),
        Some(0),
        "并行生成不应产生跳过记录"
    );
}

/// `--legacy-patch` 与 `--large-file-delta` 互斥：混用必须直接失败。
#[test]
fn test_generate_cli_rejects_conflicting_patch_modes() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args: Vec<String> = [
        "--old-binary".to_string(),
        s(&old_exe),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--legacy-patch".into(),
        "--large-file-delta".into(),
    ]
    .to_vec();
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "互斥参数必须失败: {stderr}");
    assert!(
        stderr.contains("large-file-delta"),
        "stderr 必须点名冲突参数: {stderr}"
    );
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
        manifest["patches"]
            .get("windows-x86_64")
            .and_then(|p| p.as_array())
            .is_none_or(|a| a.is_empty()),
        "劣质补丁不得写入清单（空平台键会被省略）"
    );
}

/// `--old-binary` 可以在没有 `--old-dir` 时单独使用，补丁落到输出目录。
#[test]
fn test_generate_cli_accepts_old_binary_without_old_dir() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--old-binary".into(),
        s(&old_exe),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
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

    assert!(
        dir.join("0.1.0_to_0.1.1.patch").exists(),
        "没有 --old-dir 时补丁应写到输出目录"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["patches"]["windows-x86_64"][0]["from_version"],
        "0.1.0"
    );
}

/// 显式基线校验失败时必须直接报错，而不是静默跳过。
#[test]
fn test_generate_cli_rejects_bad_old_binary() {
    let dir = tempdir();
    let (_old_exe, new_exe) = make_pair(&dir);

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    // 与目标版本相同：显式基线一定是在发布流水线里配错了
    let baseline_dir = dir.join("baseline");
    std::fs::create_dir_all(&baseline_dir).unwrap();
    let same_version = baseline_dir.join("App_0.1.1_x64.exe");
    std::fs::write(&same_version, b"same version").unwrap();

    let args = vec![
        "--old-binary".into(),
        s(&same_version),
        "--app-binary".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "显式基线版本与目标相同时必须失败");
    assert!(
        stderr.contains("版本与 --version"),
        "错误必须说明原因，实际 stderr: {stderr}"
    );
}

/// `--installer` 与 `--app-binary` 内容相同几乎一定是用错了参数：必须直接拒绝。
#[test]
fn test_generate_cli_rejects_installer_equal_to_app_binary() {
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
        "--installer".into(),
        s(&new_exe),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "安装包与主程序内容相同时必须失败");
    assert!(
        stderr.contains("--installer 与 --app-binary"),
        "错误必须点名两个参数，实际 stderr: {stderr}"
    );
}

/// `--report` 输出机器可读结果，`--archive-dir` 自动把本次主程序变成下次的基线。
#[test]
fn test_generate_cli_writes_report_and_archives_baseline() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);

    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::copy(&old_exe, old_dir.join("App_0.1.0_x64.exe")).unwrap();

    let archive_dir = dir.join("archive");
    let report_path = dir.join("release-report.json");

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
        "--report".into(),
        s(&report_path),
        "--archive-dir".into(),
        s(&archive_dir),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["version"], "0.1.1");
    assert_eq!(report["platform"], "windows-x86_64");
    let patches = report["patches"].as_array().expect("patches 必须是数组");
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0]["from_version"], "0.1.0");
    assert_eq!(patches[0]["patch_file"], "0.1.0_to_0.1.1.patch");
    assert!(patches[0]["ratio"].is_number(), "报告应包含补丁占比");

    // 产物名已含版本与架构时原样归档，不重复拼接
    let archived = archive_dir.join("App_0.1.1_x64.exe");
    assert!(archived.exists(), "必须自动归档本次主程序");
    assert_eq!(
        std::fs::read(&archived).unwrap(),
        std::fs::read(&new_exe).unwrap(),
        "归档内容必须与本次主程序一致"
    );
}

/// 名字里没有版本/架构的主程序，归档时补全成可被下次自动识别的名称。
#[test]
fn test_generate_cli_archives_plain_named_binary() {
    let dir = tempdir();
    let old_exe = dir.join("App_0.1.0_x64.exe");
    let new_exe = dir.join("MyApp.exe");
    std::fs::write(
        &old_exe,
        (0..1024u32).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    std::fs::write(
        &new_exe,
        (0..1024u32).map(|i| (i % 241) as u8).collect::<Vec<_>>(),
    )
    .unwrap();

    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::copy(&old_exe, old_dir.join("App_0.1.0_x64.exe")).unwrap();

    let archive_dir = dir.join("archive");
    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--old-dir".into(),
        s(&old_dir),
        "--app-binary".into(),
        s(&new_exe),
        "--platform".into(),
        "windows-x86_64".into(),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--archive-dir".into(),
        s(&archive_dir),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        archive_dir.join("MyApp_0.1.1_x86_64.exe").exists(),
        "缺版本/架构的名字必须补全，实际归档目录: {:?}",
        std::fs::read_dir(&archive_dir).map(|d| d
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect::<Vec<_>>())
    );
}

/// 一个补丁都没有时，`--fail-on-empty` 必须让流水线失败而不是静默发布空清单。
#[test]
fn test_generate_cli_fail_on_empty() {
    let dir = tempdir();
    let (_old_exe, new_exe) = make_pair(&dir);

    let old_dir = dir.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();

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
        "--fail-on-empty".into(),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "空补丁必须触发失败");
    assert!(
        stderr.contains("未生成任何补丁"),
        "错误必须指出原因，实际 stderr: {stderr}"
    );
    assert!(!dir.join("latest.json").exists(), "闸门触发时不得写清单");
}

/// `--legacy-patch` 必须产出未压缩的 BSDIFF40，供不认 zstd 容器的旧客户端。
#[test]
fn test_generate_cli_legacy_patch_is_uncompressed() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

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
        "--legacy-patch".into(),
        "--fail-on-empty".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let patch = std::fs::read(old_versions.join("0.1.0_to_0.1.1.patch")).unwrap();
    assert_eq!(&patch[..8], b"BSDIFF40", "legacy 补丁必须是 BSDIFF40");
    assert_ne!(&patch[..4], &[0x28, 0xB5, 0x2F, 0xFD], "不能是 zstd 容器");

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["patches"]["windows-x86_64"][0]["patch_hash"],
        sha256_hex(&patch),
        "清单哈希必须对应未压缩补丁"
    );
}

/// `--sha256sums` 必须覆盖清单、签名、公钥、安装包与补丁，格式为 `哈希  文件名`。
#[test]
fn test_generate_cli_writes_sha256sums() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let installer = dir.join("App_0.1.1_x64-setup.exe");
    let mut installer_bytes = vec![0u8; 64 * 1024];
    installer_bytes[..9].copy_from_slice(b"installer");
    std::fs::write(&installer, &installer_bytes).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let sums = dir.join("SHA256SUMS");
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
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--sha256sums".into(),
        s(&sums),
        "--fail-on-empty".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let text = std::fs::read_to_string(&sums).unwrap();
    for name in [
        "latest.json",
        "latest.json.sig",
        "latest.json.pub",
        "App_0.1.1_x64-setup.exe",
        "0.1.0_to_0.1.1.patch",
    ] {
        assert!(text.contains(name), "SHA256SUMS 缺少 {name}: {text}");
    }
    for line in text.lines() {
        let (hash, name) = line.split_once("  ").expect("格式必须是 '哈希  文件名'");
        assert_eq!(hash.len(), 64, "哈希必须是 64 位: {line}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{line}");
        assert!(!name.is_empty());
    }
}

/// 整包模式：目录级差分包 + 独立签名的 `latest.bundle.json`。
#[test]
fn test_generate_cli_bundle_mode_emits_signed_manifest() {
    let dir = tempdir();
    let new_app = dir.join("new_app");
    let old_app = dir.join("old_app_v0.1.0");

    std::fs::create_dir_all(new_app.join("resources")).unwrap();
    std::fs::create_dir_all(old_app.join("resources")).unwrap();
    std::fs::write(old_app.join("app.exe"), b"old main binary").unwrap();
    std::fs::write(old_app.join("resources/a.txt"), b"old resource").unwrap();
    std::fs::write(new_app.join("app.exe"), b"new main binary, longer").unwrap();
    std::fs::write(new_app.join("resources/a.txt"), b"new resource, longer").unwrap();
    std::fs::write(new_app.join("resources/b.txt"), b"added resource").unwrap();

    let new_exe = new_app.join("app.exe");
    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();
    let sums = dir.join("SHA256SUMS");

    let args = vec![
        "--app-binary".into(),
        s(&new_exe),
        "--new-app-dir".into(),
        s(&new_app),
        "--old-app-dir".into(),
        s(&old_app),
        "--platform".into(),
        "windows-x86_64".into(),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--sha256sums".into(),
        s(&sums),
        "--fail-on-empty".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "整包模式必须成功退出\nstdout: {stdout}\nstderr: {stderr}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.bundle.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], "0.1.1");
    assert_eq!(manifest["platform"], "windows-x86_64");

    let bundles = manifest["bundles"].as_array().expect("bundles 必须是数组");
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0]["from_version"], "0.1.0");
    assert_eq!(bundles[0]["to_version"], "0.1.1");

    let base_files = bundles[0]["base_files"]
        .as_array()
        .expect("必须记录旧版本文件清单");
    let paths: Vec<&str> = base_files
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"app.exe"));
    assert!(paths.contains(&"resources/a.txt"));

    let target_files = bundles[0]["target_files"]
        .as_array()
        .expect("必须记录新版本文件清单");
    assert!(
        target_files.iter().any(|f| f["path"] == "resources/b.txt"),
        "新增资源必须出现在新版本清单中"
    );

    assert!(
        dir.join("0.1.0_to_0.1.1.bundle.patch").exists(),
        "整包补丁必须落盘"
    );
    let checksums = std::fs::read_to_string(&sums).unwrap();
    assert!(
        checksums.contains("0.1.0_to_0.1.1.bundle.patch"),
        "SHA256SUMS 必须覆盖整包补丁: {checksums}"
    );

    // 独立清单必须能用同一把公钥验证
    let pk = minisign::PublicKey::from_secret_key(&_sk).unwrap();
    let sig_text = std::fs::read_to_string(dir.join("latest.bundle.json.sig")).unwrap();
    let sig_box = minisign::SignatureBox::from_string(&sig_text).unwrap();
    minisign::verify(
        &pk,
        &sig_box,
        std::io::Cursor::new(std::fs::read(dir.join("latest.bundle.json")).unwrap()),
        true,
        false,
        true,
    )
    .expect("latest.bundle.json 签名必须可验证");
}

/// macOS 发布：`.app` 本身通常不带版本号，版本从父目录读取
/// （`v0.1.0/MyApp.app`），目录里是 `Contents/...` 相对路径。
#[test]
fn test_generate_cli_bundle_reads_version_from_app_parent() {
    let dir = tempdir();
    let new_app = dir.join("MyApp.app");
    let old_app = dir.join("v0.1.0").join("MyApp.app");
    for app in [&new_app, &old_app] {
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    }
    std::fs::write(old_app.join("Contents/MacOS/MyApp"), b"old main").unwrap();
    std::fs::write(old_app.join("Contents/Resources/a.txt"), b"old resource").unwrap();
    std::fs::write(new_app.join("Contents/MacOS/MyApp"), b"new main, longer").unwrap();
    std::fs::write(
        new_app.join("Contents/Resources/a.txt"),
        b"new resource, longer",
    )
    .unwrap();

    let new_exe = new_app.join("Contents/MacOS/MyApp");
    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--app-binary".into(),
        s(&new_exe),
        "--new-app-dir".into(),
        s(&new_app),
        "--old-app-dir".into(),
        s(&old_app),
        "--platform".into(),
        "darwin-aarch64".into(),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
        "--fail-on-empty".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "`.app` 的版本在父目录时也必须成功\nstdout: {stdout}\nstderr: {stderr}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.bundle.json")).unwrap()).unwrap();
    let bundles = manifest["bundles"].as_array().expect("bundles 必须是数组");
    assert_eq!(bundles.len(), 1);
    assert_eq!(
        bundles[0]["from_version"], "0.1.0",
        "版本号必须从 .app 的父目录读取"
    );
    assert_eq!(bundles[0]["to_version"], "0.1.1");

    let paths: Vec<&str> = bundles[0]["base_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"Contents/MacOS/MyApp"),
        "清单必须以 .app 为根，路径形如 Contents/MacOS/MyApp"
    );
}

/// `--old-app-dir` 目录名没有版本号时必须报错，而不是生成无法匹配的补丁。
#[test]
fn test_generate_cli_bundle_requires_version_in_dir_name() {
    let dir = tempdir();
    let new_app = dir.join("new_app");
    let old_app = dir.join("old_app_without_version");
    std::fs::create_dir_all(&new_app).unwrap();
    std::fs::create_dir_all(&old_app).unwrap();
    std::fs::write(new_app.join("app.exe"), b"new").unwrap();
    std::fs::write(old_app.join("app.exe"), b"old").unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--app-binary".into(),
        s(new_app.join("app.exe")),
        "--new-app-dir".into(),
        s(&new_app),
        "--old-app-dir".into(),
        s(&old_app),
        "--platform".into(),
        "windows-x86_64".into(),
        "--version".into(),
        "0.1.1".into(),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        s(&key_file),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "目录名缺版本号时必须失败");
    assert!(
        stderr.contains("必须包含版本号"),
        "错误必须说明原因，实际 stderr: {stderr}"
    );
}

/// 通道产物命名：非 stable 通道写 `latest-<channel>.json`，stable 保持旧名。
///
/// 灰度不需要改清单格式：先发 beta，验证后把同一份产物按 stable 再发一次。
#[test]
fn test_generate_cli_channel_filenames() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

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
        "--channel".into(),
        "beta".into(),
    ];
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功\nstdout: {stdout}\nstderr: {stderr}"
    );

    assert!(
        dir.join("latest-beta.json").is_file(),
        "beta 通道必须写到 latest-beta.json"
    );
    assert!(
        dir.join("latest-beta.json.sig").is_file(),
        "签名文件必须与清单同名"
    );
    assert!(
        !dir.join("latest.json").exists(),
        "非 stable 通道不得覆盖 stable 清单"
    );
}

/// `--rollout` 写入受签名保护的 `platforms[].rollout`；100 不写字段。
#[test]
fn test_generate_cli_rollout_percentage() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let mut args = vec![
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
    ];

    args.extend([
        "--rollout".to_string(),
        "20".to_string(),
        "--min-version".to_string(),
        "0.1.0".to_string(),
    ]);
    let (code, stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(
        code, 0,
        "generate 必须成功\nstdout: {stdout}\nstderr: {stderr}"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["platforms"]["windows-x86_64"]["rollout"]["percentage"],
        20
    );
    assert_eq!(
        manifest["min_version"], "0.1.0",
        "--min-version 必须写入受签名保护的清单"
    );

    // 100 = 全量：字段必须省略，旧客户端与旧清单保持字节级兼容。
    let full = vec![
        "--old-dir".to_string(),
        s(&old_versions),
        "--app-binary".to_string(),
        s(&new_exe),
        "--version".to_string(),
        "0.1.1".to_string(),
        "--base-url".to_string(),
        "https://cdn.example.com".to_string(),
        "--private-key".to_string(),
        s(&key_file),
        "--rollout".to_string(),
        "100".to_string(),
    ];
    let (code, stdout, stderr) = run_generate(&full, &dir);
    assert_eq!(
        code, 0,
        "100% 发布必须成功\nstdout: {stdout}\nstderr: {stderr}"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert!(
        manifest["platforms"]["windows-x86_64"]
            .get("rollout")
            .is_none(),
        "100% 不应写 rollout 字段"
    );

    // 超过 100 是配置错误，必须在写任何文件之前失败。
    let mut invalid = full;
    let rollout = invalid.iter().position(|arg| arg == "--rollout").unwrap();
    invalid[rollout + 1] = "101".into();
    let (code, _stdout, stderr) = run_generate(&invalid, &dir);
    assert_ne!(code, 0, "--rollout 101 必须失败");
    assert!(
        stderr.contains("--rollout"),
        "错误必须点名参数，实际 stderr: {stderr}"
    );
}

/// 非法通道名必须被拒绝：它会直接进入文件名。
#[test]
fn test_generate_cli_rejects_path_like_channel() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

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
        "--channel".into(),
        "../evil".into(),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "路径型通道名必须失败");
    assert!(
        stderr.contains("非法"),
        "错误必须说明通道名非法，实际 stderr: {stderr}"
    );
}

/// 双签：清单与安装包都必须带上第二把密钥的签名。
#[test]
fn test_generate_cli_dual_signs_with_secondary_key() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk1, sk1_text, pk1) = keypair();
    let (_sk2, sk2_text, pk2) = keypair();
    let key1 = dir.join("key1.msk");
    let key2 = dir.join("key2.msk");
    std::fs::write(&key1, sk1_text.as_bytes()).unwrap();
    std::fs::write(&key2, sk2_text.as_bytes()).unwrap();

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
        s(&key1),
        "--secondary-private-key".into(),
        s(&key2),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(code, 0, "双签必须成功\nstderr: {stderr}");

    let manifest = std::fs::read(dir.join("latest.json")).unwrap();
    let signature = std::fs::read_to_string(dir.join("latest.json.sig")).unwrap();
    assert_eq!(
        signature.matches("untrusted comment:").count(),
        2,
        "两把私钥都应产生一个签名块"
    );

    let keys = [pk1.as_str(), pk2.as_str()];
    assert!(
        tauri_delta_updater::verify_manifest_signature_multi(
            &manifest,
            &signature,
            &keys,
            tauri_delta_updater::TrustPolicy::AllOf,
            true
        )
        .is_ok(),
        "双签清单必须在 AllOf 下通过"
    );

    let manifest_json: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let installer_signature = manifest_json["platforms"]["windows-x86_64"]["signature"]
        .as_str()
        .expect("平台条目必须带安装包签名");
    assert!(
        tauri_delta_updater::verify_file_signature_multi(
            &new_exe,
            installer_signature,
            &keys,
            tauri_delta_updater::TrustPolicy::AllOf
        )
        .is_ok(),
        "安装包也必须被两把密钥同时签名"
    );
}

/// `--notes-i18n` 写入多语言更新说明，格式必须是 `LOCALE=TEXT`。
#[test]
fn test_generate_cli_writes_localized_notes() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let args = vec![
        "--old-dir".to_string(),
        s(&old_versions),
        "--app-binary".to_string(),
        s(&new_exe),
        "--version".to_string(),
        "0.1.1".to_string(),
        "--base-url".to_string(),
        "https://cdn.example.com".to_string(),
        "--private-key".to_string(),
        s(&key_file),
        "--notes".to_string(),
        "fallback notes".to_string(),
        "--notes-i18n".to_string(),
        "zh-CN=中文说明".to_string(),
        "--notes-i18n".to_string(),
        "ja=日本語".to_string(),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_eq!(code, 0, "合法 --notes-i18n 必须成功\nstderr: {stderr}");

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(manifest["notes_i18n"]["zh-CN"], "中文说明");
    assert_eq!(manifest["notes_i18n"]["ja"], "日本語");
    assert_eq!(manifest["notes"], "fallback notes");

    let mut bad = args;
    bad.extend(["--notes-i18n".to_string(), "broken".to_string()]);
    let (code, _stdout, stderr) = run_generate(&bad, &dir);
    assert_ne!(code, 0, "缺少 `=` 的 --notes-i18n 必须失败");
    assert!(
        stderr.contains("--notes-i18n"),
        "错误必须说明字段: {stderr}"
    );
}

/// `--min-os` 写入受签名保护的最低系统版本；非法格式必须在写文件前失败。
#[test]
fn test_generate_cli_writes_min_os() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let base_args = |min_os: &str| {
        vec![
            "--old-dir".to_string(),
            s(&old_versions),
            "--app-binary".to_string(),
            s(&new_exe),
            "--version".to_string(),
            "0.1.1".to_string(),
            "--base-url".to_string(),
            "https://cdn.example.com".to_string(),
            "--private-key".to_string(),
            s(&key_file),
            "--min-os".to_string(),
            min_os.to_string(),
        ]
    };

    let (code, _stdout, stderr) = run_generate(&base_args("10.0.19045"), &dir);
    assert_eq!(code, 0, "合法 --min-os 必须成功\nstderr: {stderr}");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["platforms"]["windows-x86_64"]["min_os"],
        "10.0.19045"
    );

    let (code, _stdout, stderr) = run_generate(&base_args("10.x"), &dir);
    assert_ne!(code, 0, "非法 --min-os 必须在写清单前失败");
    assert!(stderr.contains("--min-os"), "错误必须说明字段: {stderr}");
}

/// 输出目录其实是文件时必须立即失败，而不是把清单写丢。
#[test]
fn test_generate_cli_fails_when_output_dir_unwritable() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    // 用一个普通文件冒充目录
    let output_dir = dir.join("not-a-dir");
    std::fs::write(&output_dir, b"file").unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

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
        "--output-dir".into(),
        s(&output_dir),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "输出目录不可写时必须失败");
    assert!(!stderr.is_empty(), "必须给出可读的错误信息");
}

/// `--sha256sums` 指向目录时必须失败，不能静默跳过校验文件。
#[test]
fn test_generate_cli_fails_when_sha256sums_path_unwritable() {
    let dir = tempdir();
    let (old_exe, new_exe) = make_pair(&dir);
    let old_versions = dir.join("old_binaries");
    std::fs::create_dir_all(&old_versions).unwrap();
    std::fs::copy(&old_exe, old_versions.join("App_0.1.0_x64.exe")).unwrap();

    let (_sk, sk_text, _pk) = keypair();
    let key_file = dir.join("key.msk");
    std::fs::write(&key_file, sk_text.as_bytes()).unwrap();

    let sums_dir = dir.join("sums-is-a-dir");
    std::fs::create_dir_all(&sums_dir).unwrap();
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
        "--sha256sums".into(),
        s(&sums_dir),
    ];
    let (code, _stdout, stderr) = run_generate(&args, &dir);
    assert_ne!(code, 0, "SHA256SUMS 路径不可写时必须失败");
    assert!(!stderr.is_empty(), "必须给出可读的错误信息");
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
