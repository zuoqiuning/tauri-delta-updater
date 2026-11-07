//! `cargo delta init/migrate` 的端到端测试：默认 dry-run，`--apply` 才落盘。

#![cfg(feature = "generator")]

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
        cargo.contains(&format!(
            "tauri-plugin-delta-updater = {{ git = \"{}\" }}",
            env!("CARGO_PKG_REPOSITORY")
        )),
        "默认必须写可直接构建的 git 依赖，而不是不存在的版本号: {cargo}"
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
