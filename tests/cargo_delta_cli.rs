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

/// 合法公钥：`--check` 会校验格式，占位符 "PK" 已不可用。
fn pubkey() -> String {
    common::signing_keypair().1
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
    assert_eq!(
        capability["identifier"], "default",
        "已有 capability 缺 identifier 时必须补齐，否则 tauri build 失败"
    );

    let cargo = read(&root, "Cargo.toml");
    assert!(cargo.contains("tauri-plugin-delta-updater"));
    let minor: String = env!("CARGO_PKG_VERSION")
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    assert!(
        cargo.contains(&format!("tauri-plugin-delta-updater = \"{minor}\"")),
        "默认必须只约束到 minor（\"{minor}\"）: {cargo}"
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
            &pubkey(),
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

/// `--check` 供 CI 做接入守卫：README 的写法不带 endpoint/pubkey，
/// 缺失时非零，配置完成后零并打印注册片段。
#[test]
fn init_check_is_a_ci_guard() {
    let (_dir, root) = project();
    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0, "未配置时 --check 必须失败");
    assert!(
        stderr.contains("未通过检查"),
        "错误必须说明检查失败: {stderr}"
    );
    assert!(
        stderr.contains("--force"),
        "修复命令必须能覆盖已有配置: {stderr}"
    );
    assert!(stderr.contains("delta-updater"));

    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            &pubkey(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    // 无参数 --check：这是 README / CI 中的真实用法，必须能运行。
    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
    assert_eq!(code, 0, "--check 应通过: {stdout}\n{stderr}");
    assert!(stdout.contains("接入配置完整"));
    assert!(stdout.contains("单栈"), "未保留官方插件时必须提示单栈");
    assert!(
        stdout.contains(".plugin(tauri_plugin_delta_updater::init_from_config())"),
        "必须输出可直接粘贴的注册片段"
    );
}

/// 自定义 capability 文件校验失败时，错误必须点名实际使用的文件，
/// 而不是写死 default.json 把用户引到错误位置。
#[test]
fn init_check_names_the_capability_file_it_uses() {
    let (_dir, root) = project();
    std::fs::remove_file(root.join("src-tauri/capabilities/default.json")).unwrap();
    std::fs::write(
        root.join("src-tauri/capabilities/main.json"),
        r#"{"permissions":["core:default"]}"#,
    )
    .unwrap();

    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("capabilities/main.json"),
        "错误必须点名实际使用的 capability: {stderr}"
    );
}

/// 迁移时保留官方配置键（不重命名）也必须能通过 --check：
/// 插件运行时会在缺失 plugins.delta-updater 时回退读取 plugins.updater。
#[test]
fn init_check_accepts_official_config_key() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            &pubkey(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    // 把配置键改成官方键，模拟“保留 plugins.updater”的迁移方式。
    let path = root.join("src-tauri/tauri.conf.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("\"delta-updater\"", "\"updater\"")).unwrap();

    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
    assert_eq!(code, 0, "--check 必须承认官方配置键: {stdout}\n{stderr}");
    assert!(
        stdout.contains("配置来源：plugins.updater"),
        "必须说明配置来自官方键: {stdout}"
    );
}

/// `--pubkey-file` 必须读取原始 `.pub` 文件并转成官方配置约定的 base64。
#[test]
fn init_accepts_pubkey_file() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let (_dir, root) = project();
    let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let raw = keypair.pk.to_box().unwrap().to_string();
    let key_path = root.join("myapp.pub");
    std::fs::write(&key_path, &raw).unwrap();
    let key_arg = key_path.to_str().unwrap();

    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey-file",
            key_arg,
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let config: serde_json::Value = serde_json::from_str(&read(&root, "tauri.conf.json")).unwrap();
    assert_eq!(
        config["plugins"]["delta-updater"]["pubkey"],
        STANDARD.encode(raw.trim().as_bytes()),
        "原始 .pub 文件必须编码为 base64 写入配置"
    );

    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
    assert_eq!(code, 0, "生成的公钥必须通过 --check: {stdout}\n{stderr}");

    let (code, _stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--pubkey-file",
            key_arg,
            "--apply",
        ],
    );
    assert_ne!(code, 0, "--pubkey 与 --pubkey-file 不能同时使用");
    assert!(stderr.contains("pubkey-file"), "{stderr}");
}

/// 依赖写成明显过期的版本时，`--check` 必须失败：它宣称守护“接入不腐烂”。
#[test]
fn init_check_rejects_stale_dependency_version() {
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

    let cargo_path = root.join("src-tauri/Cargo.toml");
    let text = std::fs::read_to_string(&cargo_path).unwrap();
    let stale = text
        .lines()
        .map(|line| {
            if line.starts_with("tauri-plugin-delta-updater") {
                "tauri-plugin-delta-updater = \"0.1\""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&cargo_path, format!("{stale}\n")).unwrap();

    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0, "版本落后时 --check 必须失败");
    assert!(
        stderr.contains("落后"),
        "错误必须指出依赖版本落后: {stderr}"
    );
}

/// 项目没有 capability 文件时，生成的文件必须是 Tauri 2 可反序列化的：
/// 缺 `identifier`/`windows` 会让 `tauri build` 直接失败。
#[test]
fn init_writes_valid_capability_when_missing() {
    let (_dir, root) = project();
    std::fs::remove_file(root.join("src-tauri/capabilities/default.json")).unwrap();

    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            &pubkey(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let capability: serde_json::Value =
        serde_json::from_str(&read(&root, "capabilities/default.json")).unwrap();
    assert_eq!(capability["identifier"], "default");
    assert_eq!(
        capability["windows"][0], "main",
        "新建 capability 必须声明作用窗口"
    );
    let permissions = capability["permissions"].as_array().unwrap();
    assert!(permissions.iter().any(|v| v == "delta-updater:default"));

    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
    assert_eq!(code, 0, "生成后 --check 应通过: {stdout}\n{stderr}");
}

/// 明文 HTTP 的非回环端点必须让 `--check` 失败；显式允许后才通过。
/// 客户端运行时同样拒绝，CI 守卫必须提前在接入阶段拦下。
#[test]
fn init_check_rejects_insecure_endpoint() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "http://cdn.example.com/latest.json",
            "--pubkey",
            &pubkey(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0, "明文 HTTP 端点必须让 --check 失败");
    assert!(
        stderr.contains("allow_insecure_transport"),
        "错误必须说明如何显式放行: {stderr}"
    );

    // 独立托管的签名地址同样不能是明文 HTTP。
    let path = root.join("src-tauri/tauri.conf.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    config["plugins"]["delta-updater"]["signature_url"] =
        serde_json::json!("http://mirror.example.com/latest.json.sig");
    std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();

    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0, "明文 HTTP 签名地址必须让 --check 失败");
    assert!(stderr.contains("signature_url"), "{stderr}");

    // 显式开启后必须放行：本地测试服务器是合法场景。
    config["plugins"]["delta-updater"]["allow_insecure_transport"] = serde_json::json!(true);
    std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();

    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
    assert_eq!(code, 0, "--check 应通过: {stdout}\n{stderr}");
}

/// 非 http(s) 的 scheme 只会在运行时炸；`--check` 必须静态拦下。
#[test]
fn init_check_rejects_unknown_scheme() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "ftp://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0, "非 http(s) 端点必须让 --check 失败");
    assert!(stderr.contains("不是 http(s) 地址"), "{stderr}");
}

/// 公钥复制粘贴截断是最常见的接入错误；`--check` 必须在离线阶段指出。
#[test]
fn init_check_rejects_invalid_pubkey() {
    let (_dir, root) = project();
    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "BASE64_PUBLIC_KEY",
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (code, _stdout, stderr) = run(&root, &["init", "--check"]);
    assert_ne!(code, 0, "无效公钥必须让 --check 失败");
    assert!(stderr.contains("公钥"), "{stderr}");
}

/// 项目没有 `default.json` 但有自定义 capability 时，init 必须写入已有文件，
/// 而不是凭空新建一个不生效的 default.json。
#[test]
fn init_reuses_custom_capability_file() {
    let (_dir, root) = project();
    std::fs::remove_file(root.join("src-tauri/capabilities/default.json")).unwrap();
    std::fs::write(
        root.join("src-tauri/capabilities/main.json"),
        r#"{"identifier":"main","windows":["main"],"permissions":["core:default"]}"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run(
        &root,
        &[
            "init",
            "--endpoint",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            &pubkey(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("改用 main.json"),
        "必须提示改用了哪个 capability: {stdout}"
    );
    assert!(
        read(&root, "capabilities/main.json").contains("delta-updater:default"),
        "权限必须写进已有 capability"
    );
    assert!(
        !root.join("src-tauri/capabilities/default.json").exists(),
        "不得新建 default.json"
    );

    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
    assert_eq!(code, 0, "--check 应通过: {stdout}\n{stderr}");
}

/// migrate 复制官方配置；--remove-official 同时移除官方配置与权限。
#[test]
fn migrate_copies_and_can_remove_official() {
    let (_dir, root) = project();
    let pk = pubkey();
    std::fs::write(
        root.join("src-tauri/tauri.conf.json"),
        format!(
            r#"{{"productName":"Demo","plugins":{{"updater":{{"endpoints":["https://cdn.example.com/latest.json"],"pubkey":"{pk}"}}}}}}"#
        ),
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
    assert_eq!(config["plugins"]["delta-updater"]["pubkey"], pk);
    assert!(
        config["plugins"]["updater"].is_object(),
        "默认保留官方插件作为全量回退"
    );

    let (code, stdout, stderr) = run(&root, &["init", "--check"]);
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

/// 体检对带查询参数的 CDN 地址必须像客户端一样推导签名地址：
/// `.sig` 追加在路径末尾，而不是查询串之后。
#[test]
fn doctor_keeps_query_parameters_in_signature_url() {
    use common::MockServer;
    use std::collections::HashMap;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = MockServer::start(HashMap::new());
    let (sk, pk) = common::signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let manifest = serde_json::json!({
        "version": "0.1.1",
        "notes": "query fixture",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: { "signature": "dummy", "url": server.url("/full.exe") }
        },
        "patches": {}
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let signature = common::sign_bound(&sk, &manifest_bytes, "tdu:v=0.1.1");
    // 客户端请求的签名地址带同一个 token；旧的 `{endpoint}.sig` 会得到
    // `/latest.json?token=abc.sig` 而 404。
    server.set("/latest.json?token=abc", manifest_bytes);
    server.set("/latest.json.sig?token=abc", signature.into_bytes());
    server.set("/full.exe", b"installer".to_vec());

    let endpoint = server.url("/latest.json?token=abc");
    let (code, stdout, stderr) = run(root, &["doctor", &endpoint, "--pubkey", &pk]);
    assert_eq!(
        code, 0,
        "带查询参数的签名地址必须能被解析\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("清单签名有效"), "{stdout}");
}

/// `doctor --from-config` 必须读取项目配置（含官方键回退与占位符展开）并完成体检。
#[test]
fn doctor_reads_project_config() {
    use common::MockServer;
    use std::collections::HashMap;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let server = MockServer::start(HashMap::new());
    let (sk, pk) = common::signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let manifest = serde_json::json!({
        "version": "0.1.1",
        "notes": "from config",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: { "signature": "dummy", "url": server.url("/full.exe") }
        },
        "patches": {}
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let signature = common::sign_bound(&sk, &manifest_bytes, "tdu:v=0.1.1");
    server.set("/stable/latest.json", manifest_bytes);
    server.set("/stable/latest.json.sig", signature.into_bytes());
    server.set("/full.exe", b"installer".to_vec());

    let tauri = root.join("src-tauri");
    std::fs::create_dir_all(tauri.join("capabilities")).unwrap();
    let config = serde_json::json!({
        "productName": "Demo",
        "plugins": {
            "updater": {
                "endpoints": [server.url("/{{channel}}/latest.json")],
                "pubkey": pk,
                "channel": "stable"
            }
        }
    });
    std::fs::write(
        tauri.join("tauri.conf.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(
        tauri.join("Cargo.toml"),
        "[package]\nname=\"demo\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(tauri.join("capabilities/default.json"), "{}").unwrap();

    let (code, stdout, stderr) = run(root, &["doctor", "--from-config", "--project", "."]);
    assert_eq!(code, 0, "体检必须通过: {stdout}\n{stderr}");
    assert!(stdout.contains("清单签名有效"), "{stdout}");
}

/// 全局参数被无关子命令接受但静默忽略，会让用户误以为它生效。
#[test]
fn release_and_doctor_reject_irrelevant_global_flags() {
    let (_dir, root) = project();
    let (code, _stdout, stderr) = run(
        &root,
        &[
            "doctor",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--apply",
        ],
    );
    assert_ne!(code, 0, "doctor 不接受 --apply");
    assert!(stderr.contains("--apply"), "{stderr}");

    let (code, _stdout, stderr) = run(
        &root,
        &[
            "doctor",
            "https://cdn.example.com/latest.json",
            "--pubkey",
            "PK",
            "--project",
            ".",
        ],
    );
    assert_ne!(code, 0, "--project 只允许与 --from-config 一起使用");
    assert!(stderr.contains("--project"), "{stderr}");
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
