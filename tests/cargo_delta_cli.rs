//! `cargo delta init/migrate` 的端到端测试：默认 dry-run，`--apply` 才落盘。

#![cfg(feature = "generator")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    let tauri = root.join("src-tauri");
    std::fs::create_dir_all(tauri.join("capabilities")).unwrap();
    std::fs::write(
        tauri.join("tauri.conf.json"),
        r#"{"productName":"Demo","plugins":{}}"#,
    )
    .unwrap();
    std::fs::write(
        tauri.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        tauri.join("capabilities/default.json"),
        r#"{"permissions":["core:default"]}"#,
    )
    .unwrap();
    (dir, root)
}

fn run(root: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-delta"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("必须能启动 cargo-delta");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn read(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join("src-tauri").join(relative)).unwrap()
}

/// 默认 dry-run：输出 diff，但不改任何文件。
#[test]
fn init_is_dry_run_by_default() {
    let (_dir, root) = project();
    let before = read(&root, "tauri.conf.json");

    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("--- a/"), "必须输出统一 diff");
    assert!(stdout.contains("delta-updater"));
    assert!(stdout.contains("未写入任何文件"));
    assert_eq!(read(&root, "tauri.conf.json"), before, "dry-run 不得写文件");
}

/// `--apply` 写入配置、capability 与依赖；重复执行要求 --force。
#[test]
fn init_apply_writes_all_files() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let config: serde_json::Value = serde_json::from_str(&read(&root, "tauri.conf.json")).unwrap();
    let delta = &config["plugins"]["delta-updater"];
    assert_eq!(delta["endpoints"][0], "https://cdn.example.com/latest.json");
    assert_eq!(delta["pubkey"], "PK");

    let capability: serde_json::Value =
        serde_json::from_str(&read(&root, "capabilities/default.json")).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    assert!(permissions.iter().any(|v| v == "delta-updater:default"));

    let cargo = read(&root, "Cargo.toml");
    assert!(cargo.contains("tauri-plugin-delta-updater"));
    assert!(
        cargo.contains("tauri-plugin-delta-updater = \"0.2\""),
        "默认必须只约束到 minor（\"0.2\"）: {cargo}"
    );

    let (code, _stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--apply",
        ],
    );
    assert_ne!(code, 0, "已存在配置时必须要求 --force");
    assert!(stderr.contains("--force"), "错误必须说明如何覆盖: {stderr}");
}

/// 改写配置时必须保留原有键顺序，只追加新键。
///
/// `serde_json` 默认按键名字母序输出；不加 `preserve_order` 会把用户整份
/// `tauri.conf.json` 重排，产生无意义的 diff。
#[test]
fn init_preserves_config_key_order() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let text = read(&root, "tauri.conf.json");
    let product = text.find("\"productName\"").expect("原有键必须保留");
    let plugins = text.find("\"plugins\"").expect("plugins 必须存在");
    assert!(
        product < plugins,
        "原有键顺序必须保留（productName 在 plugins 之前）: {text}"
    );
}

/// `--registry` 写注册表版本，且版本号必须与工具自身一致（防止再次写死旧版本）。
#[test]
fn init_registry_writes_current_version() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--registry",
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let cargo = read(&root, "Cargo.toml");
    assert_eq!(
        cargo
            .lines()
            .find(|line| line.starts_with("tauri-plugin-delta-updater"))
            .expect("必须写入依赖"),
        format!(
            "tauri-plugin-delta-updater = \"{}\"",
            env!("CARGO_PKG_VERSION")
        ),
        "注册表版本必须与工具版本一致"
    );

    // 写完后 --check 必须认为依赖已满足，而不是因为引号形式误判缺失
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--check",
        ],
    );
    assert_eq!(code, 0, "--check 应通过: {stdout}\n{stderr}");
}

/// `--check` 供 CI 做接入守卫：缺失时非零，配置完成后零并打印注册片段。
#[test]
fn init_check_is_a_ci_guard() {
    let (_dir, root) = project();
    let args = [
        "init",
        "--endpoint",
        "https://cdn.example.com/latest.json",
        "--pubkey",
        "PK",
        "--check",
    ];
    let (code, _stdout, stderr) = run(&root, &args);
    assert_ne!(code, 0, "未配置时 --check 必须失败");
    assert!(stderr.contains("缺少"), "错误必须列出缺失项: {stderr}");
    assert!(stderr.contains("delta-updater"));

    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (code, stdout, stderr) = run(&root, &args);
    assert_eq!(code, 0, "--check 应通过: {stdout}\n{stderr}");
    assert!(stdout.contains("接入配置完整"));
    assert!(stdout.contains("单栈"), "未保留官方插件时必须提示单栈");
    assert!(
        stdout.contains(".plugin(tauri_plugin_delta_updater::init_from_config())"),
        "必须输出可直接粘贴的注册片段"
    );
}

/// migrate 复制官方配置；--remove-official 同时移除官方配置与权限。
#[test]
fn migrate_copies_and_can_remove_official() {
    let (_dir, root) = project();
    std::fs::write(
        root.join("src-tauri/tauri.conf.json"),
        r#"{"productName":"Demo","plugins":{"updater":{"endpoints":["https://cdn.example.com/latest.json"],"pubkey":"PK"}}}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src-tauri/capabilities/default.json"),
        r#"{"permissions":["core:default","updater:default"]}"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run(&root, &["migrate", "--apply"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let config: serde_json::Value = serde_json::from_str(&read(&root, "tauri.conf.json")).unwrap();
    assert_eq!(
        config["plugins"]["delta-updater"]["endpoints"][0],
        "https://cdn.example.com/latest.json"
    );
    assert_eq!(config["plugins"]["delta-updater"]["pubkey"], "PK");
    assert!(
        config["plugins"]["updater"].is_object(),
        "默认保留官方插件作为全量回退"
    );

    let check_args = [
        "init",
        "--endpoint",
        "https://cdn.example.com/latest.json",
        "--pubkey",
        "PK",
        "--check",
    ];
    let (code, stdout, stderr) = run(&root, &check_args);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("双栈"), "保留官方插件时必须提示双栈");

    let (code, stdout, stderr) = run(
        &root,
        &["migrate", "--remove-official", "--apply", "--force"],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let config: serde_json::Value = serde_json::from_str(&read(&root, "tauri.conf.json")).unwrap();
    assert!(config["plugins"]["updater"].is_null(), "必须移除官方配置");
    let capability: serde_json::Value =
        serde_json::from_str(&read(&root, "capabilities/default.json")).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    assert!(!permissions.iter().any(|v| v == "updater:default"));
    assert!(permissions.iter().any(|v| v == "delta-updater:default"));
}

/// `cargo delta release` 与 `generate` 共用发布实现：最小参数即可产出清单与补丁。
#[test]
fn release_generates_manifest_and_patch() {
    use minisign::KeyPair;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = |p: &Path| p.to_string_lossy().to_string();

    let old_dir = root.join("old_binaries");
    std::fs::create_dir_all(&old_dir).unwrap();
    let old_bytes: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
    let new_bytes: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 241) as u8).collect();
    std::fs::write(old_dir.join("App_0.1.0_x64.exe"), old_bytes).unwrap();
    let new_exe = root.join("App_0.1.1_x64.exe");
    std::fs::write(&new_exe, new_bytes).unwrap();

    // 安装包必须是另一份字节：与主程序相同会被拒绝。
    let installer = root.join("App_0.1.1_x64-setup.exe");
    std::fs::write(&installer, vec![0u8; 64 * 1024]).unwrap();

    let key = root.join("key.msk");
    let sk_text = KeyPair::generate_unencrypted_keypair()
        .unwrap()
        .sk
        .to_box(None)
        .unwrap()
        .into_string();
    std::fs::write(&key, sk_text).unwrap();

    let out = root.join("dist");
    let args = vec![
        "release".to_string(),
        "--from".into(),
        path(&old_dir),
        "--to".into(),
        "0.1.1".into(),
        "--app-binary".into(),
        path(&new_exe),
        "--installer".into(),
        path(&installer),
        "--base-url".into(),
        "https://cdn.example.com".into(),
        "--private-key".into(),
        path(&key),
        "--output-dir".into(),
        path(&out),
    ];
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = run(root, &refs);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    assert!(out.join("latest.json").is_file(), "必须写出清单");
    assert!(out.join("latest.json.sig").is_file(), "必须写出签名");
    assert!(
        old_dir.join("0.1.0_to_0.1.1.patch").is_file(),
        "补丁必须落在唯一的基线目录"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("latest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], "0.1.1");
}

/// `cargo delta doctor` 必须能证明线上桶里真的有客户端需要的字节，
/// 并在补丁被替换后立刻失败。
#[test]
fn doctor_validates_published_bucket_and_detects_tampering() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use common::MockServer;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = MockServer::start(HashMap::new());
    let (sk, pk) = common::signing_keypair();

    let patch_bytes = b"patch payload".to_vec();
    let installer_bytes = b"installer payload".to_vec();
    let patch_hash = format!("{:x}", Sha256::digest(&patch_bytes));
    let target_hash = format!("{:x}", Sha256::digest(b"target executable"));
    let installer_signature = STANDARD.encode(common::sign(&sk, &installer_bytes).as_bytes());

    let manifest = serde_json::json!({
        "version": "0.1.1",
        "notes": "doctor fixture",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            "windows-x86_64": {
                "signature": installer_signature,
                "url": server.url("/full.exe"),
                "size": installer_bytes.len()
            }
        },
        "patches": {
            "windows-x86_64": [{
                "from_version": "0.1.0",
                "to_version": "0.1.1",
                "patch_url": server.url("/0.1.0_to_0.1.1.patch"),
                "patch_hash": patch_hash,
                "target_hash": target_hash,
                "patch_size": patch_bytes.len()
            }]
        }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let manifest_signature = common::sign_bound(&sk, &manifest_bytes, "tdu:v=0.1.1");
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", manifest_signature.into_bytes());
    server.set("/full.exe", installer_bytes);
    server.set("/0.1.0_to_0.1.1.patch", patch_bytes.clone());

    let report_path = root.join("doctor.json");
    let endpoint = server.url("/latest.json");
    let report_arg = report_path.to_string_lossy().to_string();
    let (code, stdout, stderr) = run(
        root,
        &[
            "doctor",
            &endpoint,
            "--pubkey",
            &pk,
            "--deep",
            "--json",
            &report_arg,
        ],
    );
    assert_eq!(code, 0, "体检必须通过\nstdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("清单签名有效"), "{stdout}");
    assert!(
        stdout.contains("安装包签名有效"),
        "deep 必须校验安装包: {stdout}"
    );
    assert!(stdout.contains("哈希匹配"), "{stdout}");
    assert!(stdout.contains("0 个错误"), "{stdout}");

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["errors"].as_array().unwrap().len(), 0);

    // 线上补丁被替换：清单签名仍然有效，但哈希必须对不上。
    server.set("/0.1.0_to_0.1.1.patch", b"tampered".to_vec());
    let (code, stdout, stderr) = run(root, &["doctor", &endpoint, "--pubkey", &pk]);
    assert_ne!(code, 0, "被篡改的补丁必须让体检失败");
    assert!(
        stdout.contains("SHA-256"),
        "错误必须指出哈希不匹配: {stdout}\n{stderr}"
    );
}
