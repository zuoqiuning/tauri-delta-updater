//! 端到端测试：用本地 HTTP 服务器模拟完整的发布与更新流程。
//!
//! 验证 `DeltaUpdater` 这一行接入路径真的能跑通，
//! 而不只是编译通过。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tauri_delta_updater::{DeltaUpdater, UpdateConfig, UpdateOutcome};
use tempfile::tempdir;

/// 一个静态文件服务器，按路径返回内容，并记录收到的每个请求路径。
struct MockServer {
    base_url: String,
    served: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
    requested: Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockServer {
    fn start(files: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::new(std::sync::Mutex::new(files));
        let requested: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let handler_state = Arc::clone(&served);
        let handler_requested = Arc::clone(&requested);

        std::thread::spawn(move || {
            for stream in listener.incoming().take(16) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let request = String::from_utf8_lossy(&buf);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                handler_requested.lock().unwrap().push(path.clone());

                let body = handler_state.lock().unwrap().get(&path).cloned();
                let response = match body {
                    Some(body) => {
                        // Connection: close 让 reqwest 不复用连接。服务端每个响应后
                        // 即关闭连接，若客户端仍把连接放回池子，复用时恰好撞上 FIN
                        // 就会报 "error sending request"——真实复现过的偶发失败。
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        [head.as_bytes().to_vec(), body].concat()
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };

                let _ = stream.write_all(&response);
                let _ = stream.flush();
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            served,
            requested,
        }
    }

    /// 某个路径是否被请求过。
    ///
    /// 用于堵"delta 从不 delta"的测试盲区：一个永远走全量回退的增量更新器，
    /// 与正常工作的增量更新器，在只断言产物正确时看起来一模一样。
    /// 增量命中的测试必须证明全量安装包**从未被请求**。
    fn was_requested(&self, path: &str) -> bool {
        self.requested.lock().unwrap().iter().any(|p| p == path)
    }

    /// 服务启动后更新某个路径的内容。
    ///
    /// 用于"manifest 引用自身的 URL"这类先有鸡还是先有蛋的场景：
    /// 先起服务拿到端口，再往里放引用该端口的清单。
    fn set(&self, path: &str, body: Vec<u8>) {
        self.served.lock().unwrap().insert(path.to_string(), body);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

/// 生成一对签名密钥，返回 (私钥, 公钥 base64)。
fn signing_keypair() -> (minisign::SecretKey, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_b64 = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
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

/// 构造一份已签名、含补丁的清单。
///
/// `base_url` 指向 MockServer：全量安装包地址也落在 mock 上（`/full.exe`，
/// 故意**不**提供内容），这样测试能通过请求日志证明"增量命中时全量从未被请求"。
fn build_manifest(
    sk: &minisign::SecretKey,
    base_url: &str,
    version: &str,
    platform: &str,
    patch_hash: &str,
    target_hash: &str,
    patch_size: Option<u64>,
) -> (Vec<u8>, String) {
    let manifest = serde_json::json!({
        "version": version,
        "notes": "test",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: {
                "signature": "dummy",
                "url": format!("{base_url}/full.exe")
            }
        },
        "patches": {
            platform: [{
                "from_version": "0.1.0",
                "to_version": version,
                "patch_url": format!("{base_url}/update.patch"),
                "patch_hash": patch_hash,
                "target_hash": target_hash,
                "patch_size": patch_size
            }]
        }
    });

    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(sk, &bytes);
    (bytes, sig)
}

/// 端到端：清单 + 补丁 → 一次调用 → 生成新版本。
#[tokio::test]
async fn test_end_to_end_delta_update() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    // 模拟"旧版本"可执行文件
    let old_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, &old_bytes).unwrap();

    // 模拟"新版本"可执行文件
    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let new_exe = work_dir.join("app_new.exe");
    std::fs::write(&new_exe, &new_bytes).unwrap();
    let new_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&new_bytes);
        format!("{:x}", h.finalize())
    };

    // 生成补丁
    let patch_path = dir.path().join("update.patch");
    tauri_delta_updater::create_patch(&old_exe, &new_exe, &patch_path)
        .await
        .unwrap();
    let patch_bytes = std::fs::read(&patch_path).unwrap();
    let patch_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&patch_bytes);
        format!("{:x}", h.finalize())
    };

    // 签名清单
    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let patch_size = patch_bytes.len() as u64;

    // 先起服务拿到端口，再往里放引用该端口的清单与签名
    let server = MockServer::start(HashMap::from([("/update.patch".to_string(), patch_bytes)]));
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        &patch_hash,
        &new_hash,
        Some(patch_size),
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    // 一次调用完成整个流程
    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let outcome = updater.update().await.unwrap();

    match outcome {
        UpdateOutcome::Patched { staged } => {
            assert_eq!(staged.manifest().version, "0.2.0");
            assert!(staged.staged_exe().exists(), "合并后的新版本必须存在");
            assert_eq!(
                std::fs::read(staged.staged_exe()).unwrap(),
                new_bytes,
                "合并结果必须与新版本完全一致"
            );
            // 堵"delta 从不 delta"盲区：增量命中时，全量安装包 URL 必须从未被请求。
            // 一个永远静默回退的更新器也能产出正确产物，只有请求日志能暴露它。
            assert!(
                !server.was_requested("/full.exe"),
                "增量命中时不得请求全量安装包"
            );
        }
        other => panic!("应当完成增量更新，实际得到: {other:?}"),
    }
}

/// 端到端：check() 只检查不下载，且能拿到版本与更新说明。
#[tokio::test]
async fn test_end_to_end_check_does_not_download() {
    let dir = tempdir().unwrap();
    // 工作目录与素材目录分开，才能断言"没有下载任何东西"
    let work_dir = dir.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let fixture_dir = dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).unwrap();

    let old_exe = fixture_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let new_exe = fixture_dir.join("app_new.exe");
    std::fs::write(&new_exe, &new_bytes).unwrap();
    let new_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&new_bytes);
        format!("{:x}", h.finalize())
    };

    let patch_path = fixture_dir.join("update.patch");
    tauri_delta_updater::create_patch(&old_exe, &new_exe, &patch_path)
        .await
        .unwrap();
    let patch_bytes = std::fs::read(&patch_path).unwrap();
    let patch_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&patch_bytes);
        format!("{:x}", h.finalize())
    };
    let patch_size = patch_bytes.len() as u64;

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    // check() 只拉清单，补丁地址是占位也无妨——但清单必须真实可下载
    let server = MockServer::start(HashMap::new());
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        &patch_hash,
        &new_hash,
        Some(patch_size),
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let info = updater.check().await.unwrap();

    assert_eq!(info.version, "0.2.0");
    assert_eq!(info.notes, "test");
    assert!(info.has_patch, "应当识别出可用补丁");
    assert!(info.is_newer, "0.2.0 应当被判定为高于 0.1.0");
    assert!(info.fallback_reason.is_none());
    assert_eq!(
        info.patch_size,
        Some(patch_size),
        "应当告知补丁体积，便于 UI 展示"
    );
    let expected_full_url = server.url("/full.exe");
    assert_eq!(
        info.full_download_url.as_deref(),
        Some(expected_full_url.as_str()),
        "全量地址必须来自清单（同一 mock，供回退与请求追踪）"
    );

    // 关键：check() 不得下载补丁，也不得请求全量安装包
    assert!(
        !server.was_requested("/full.exe"),
        "check() 只拉清单与签名，不得请求全量安装包"
    );
    assert!(
        !work_dir
            .join(tauri_delta_updater::core::artifacts::PATCH_FILE)
            .exists(),
        "check() 不得下载补丁"
    );
    assert!(
        !work_dir
            .join(tauri_delta_updater::core::artifacts::staged_file())
            .exists(),
        "check() 不得合并补丁"
    );
}

/// 端到端：prepare() 合并补丁但不启动替换脚本。
#[tokio::test]
async fn test_end_to_end_prepare_does_not_launch() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old version").unwrap();

    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let new_exe = work_dir.join("app_new.exe");
    std::fs::write(&new_exe, &new_bytes).unwrap();
    let new_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&new_bytes);
        format!("{:x}", h.finalize())
    };

    let patch_path = dir.path().join("update.patch");
    tauri_delta_updater::create_patch(&old_exe, &new_exe, &patch_path)
        .await
        .unwrap();
    let patch_bytes = std::fs::read(&patch_path).unwrap();
    let patch_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&patch_bytes);
        format!("{:x}", h.finalize())
    };

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    // 先起服务拿到端口，再往里放引用该端口的清单与签名
    let server = MockServer::start(HashMap::from([("/update.patch".to_string(), patch_bytes)]));
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        &patch_hash,
        &new_hash,
        None,
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    // auto_launch(false)：prepare() 只准备不启动
    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir)
            .auto_launch(false),
    )
    .unwrap();

    match updater.prepare().await.unwrap() {
        UpdateOutcome::Patched { staged } => {
            assert!(staged.staged_exe().exists(), "prepare() 应当完成合并");
            assert!(staged.script_path().exists(), "prepare() 应当生成替换脚本");
        }
        other => panic!("prepare() 应当完成补丁合并，实际得到: {other:?}"),
    }
}

/// 端到端：Staged::launch 只能成功启动一次，重复调用必须报错。
#[tokio::test]
async fn test_end_to_end_launch_is_idempotent() {
    let dir = tempdir().unwrap();
    let staged = prepare_staged(dir.path()).await;

    // 第一次启动成功（脚本在后台等待本进程退出，不影响测试）
    staged.launch().expect("首次 launch 应当成功");
    // 第二次启动必须被拒绝：两个脚本竞争替换会让回滚逻辑误判
    let err = staged.launch().expect_err("重复 launch 必须报错");
    assert!(
        matches!(err, tauri_delta_updater::DeltaError::Config(_)),
        "重复启动应返回 Config 错误，实际: {err:?}"
    );
}

/// 端到端：多个线程持有 Staged 克隆并发调用 launch，必须恰好一个成功。
///
/// Staged 的幂等状态经 Arc 在克隆间共享；若克隆各自持有独立状态，
/// 本测试会观察到多次成功启动。
#[tokio::test]
async fn test_end_to_end_launch_concurrent_exactly_once() {
    let dir = tempdir().unwrap();
    let staged = std::sync::Arc::new(prepare_staged(dir.path()).await);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let staged = std::sync::Arc::clone(&staged);
            std::thread::spawn(move || staged.launch())
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let ok_count = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok_count, 1,
        "并发调用 launch 必须恰好成功一次，实际: {results:?}"
    );
    for err in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            matches!(err, tauri_delta_updater::DeltaError::Config(_)),
            "其余调用应返回 Config 错误，实际: {err:?}"
        );
    }
}

/// 准备一份"补丁已合并、等待启动替换"的 Staged（auto_launch=false）。
async fn prepare_staged(work_dir: &std::path::Path) -> tauri_delta_updater::Staged {
    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old version").unwrap();

    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let new_exe = work_dir.join("app_new.exe");
    std::fs::write(&new_exe, &new_bytes).unwrap();
    let new_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&new_bytes);
        format!("{:x}", h.finalize())
    };

    let patch_path = work_dir.join("update.patch");
    tauri_delta_updater::create_patch(&old_exe, &new_exe, &patch_path)
        .await
        .unwrap();
    let patch_bytes = std::fs::read(&patch_path).unwrap();
    let patch_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&patch_bytes);
        format!("{:x}", h.finalize())
    };

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let server = MockServer::start(HashMap::from([("/update.patch".to_string(), patch_bytes)]));
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        &patch_hash,
        &new_hash,
        None,
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(work_dir)
            .auto_launch(false),
    )
    .unwrap();

    match updater.prepare().await.unwrap() {
        UpdateOutcome::Patched { staged } => staged,
        other => panic!("prepare() 应当完成补丁合并，实际得到: {other:?}"),
    }
}

/// 端到端：可以注入自定义 HTTP 客户端。
#[tokio::test]
async fn test_end_to_end_accepts_custom_client() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": { platform: { "signature": "d", "url": "https://cdn.example.com/full.exe" } },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &manifest_bytes);

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    // 带自定义 UA 与超时的客户端。
    // 用 client_builder() 代替 reqwest::Client::builder()：它会先安装加密后端，
    // 避免忘记初始化而在 .build() 时 panic。
    let client = tauri_delta_updater::client_builder()
        .user_agent("Sereno/1.0")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir)
            .client(client),
    )
    .unwrap();

    let info = updater.check().await.unwrap();
    assert_eq!(info.version, "0.2.0");
}

/// 空 `current_version` 必须在构造时被拒绝，而不是运行时静默退化成全量下载。
#[test]
fn test_new_rejects_empty_current_version() {
    let result = DeltaUpdater::new(UpdateConfig::new(
        "https://cdn.example.com/latest.json",
        "PK",
    ));
    match result {
        Err(tauri_delta_updater::DeltaError::Config(msg)) => {
            assert!(
                msg.contains("current_version"),
                "错误信息必须指明原因: {msg}"
            );
        }
        Err(e) => panic!("空 current_version 应返回 Config 错误，实际: {e}"),
        Ok(_) => panic!("空 current_version 不应构造成功"),
    }
}

/// 端到端：没有匹配补丁时应当回退到全量下载，而不是报错。
#[tokio::test]
async fn test_end_to_end_falls_back_when_no_patch() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "test",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: {
                "signature": "dummy",
                "url": "https://cdn.example.com/full.exe"
            }
        },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &manifest_bytes);

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    match updater.update().await.unwrap() {
        UpdateOutcome::FallbackToFullDownload {
            full_download_url,
            reason,
        } => {
            assert_eq!(
                full_download_url.as_deref(),
                Some("https://cdn.example.com/full.exe"),
                "回退时必须带上清单里声明的全量下载地址"
            );
            assert!(
                reason.to_string().contains("没有从 0.1.0 到 0.2.0"),
                "无补丁回退必须说明原因，实际: {reason}"
            );
            assert!(
                matches!(
                    reason,
                    tauri_delta_updater::FallbackReason::NoMatchingPatch { .. }
                ),
                "原因必须是可机读的 NoMatchingPatch"
            );
        }
        other => panic!("应当回退到全量下载，实际得到: {other:?}"),
    }
}

/// 端到端：清单签名不匹配时必须阻断更新。
#[tokio::test]
async fn test_end_to_end_rejects_bad_signature() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let (sk, _) = signing_keypair();
    let (_, other_pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": { platform: { "signature": "d", "url": "u" } },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &manifest_bytes);

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), other_pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let result = updater.update().await;
    assert!(result.is_err(), "公钥不匹配时必须拒绝更新，不得放行");
    assert!(matches!(
        result,
        Err(tauri_delta_updater::DeltaError::SignatureInvalid(_))
    ));
}

/// 端到端：签名合法但版本绑定与清单不符时必须拒绝（防重放过期清单）。
///
/// 攻击模型：攻击者拿到一份旧的、签名合法的清单（例如包含已知漏洞版本的
/// 补丁），原样重放。签名本身能通过验签，但 generate 写入签名的 trusted
/// comment 里的版本绑定对不上清单声明版本，必须硬阻断。
#[tokio::test]
async fn test_end_to_end_rejects_version_binding_mismatch() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    // 清单声明 0.2.0，签名却是 0.1.9 版的合法签名（trusted comment 写 0.1.9）
    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": { platform: { "signature": "d", "url": "https://cdn.example.com/full.exe" } },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign_bound(&sk, &manifest_bytes, "tdu:v=0.1.9");

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let result = updater.update().await;
    assert!(result.is_err(), "版本绑定不符时必须拒绝更新");
    match result {
        Err(tauri_delta_updater::DeltaError::SignatureInvalid(msg)) => {
            assert!(
                msg.contains("0.1.9") && msg.contains("0.2.0"),
                "错误信息必须同时指出绑定版本与清单版本，实际: {msg}"
            );
        }
        other => panic!("应当返回 SignatureInvalid，实际: {other:?}"),
    }
}

/// 端到端：版本绑定与清单一致时正常放行（确认绑定校验不误伤）。
#[tokio::test]
async fn test_end_to_end_accepts_matching_version_binding() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": { platform: { "signature": "d", "url": "https://cdn.example.com/full.exe" } },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign_bound(&sk, &manifest_bytes, "tdu:v=0.2.0");

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    // 无补丁 → 走回退分支，但必须是通过了全部校验的正常回退
    match updater.update().await.unwrap() {
        UpdateOutcome::FallbackToFullDownload { .. } => {}
        other => panic!("绑定一致时应正常进入回退分支，实际得到: {other:?}"),
    }
}

/// 连续失败达到上限后必须停止尝试增量更新。
#[tokio::test]
async fn test_end_to_end_safe_mode_after_repeated_failures() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    // 签名错误的清单，每次都会失败（签名密钥与校验公钥必须是不同密钥对）
    let (sk, _) = signing_keypair();
    let (_, other_pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": { platform: { "signature": "d", "url": "u" } },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &manifest_bytes);

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), other_pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    // 连续失败，失败计数应被记录。
    //
    // ⚠️ 不要改回"固定调用 3 次并断言计数 == 3"：0.2.5 起暂态网络错误
    // （Timeout/Network/5xx）按设计不计入失败计数，CI 环境偶发一次网络
    // 抖动就会让计数停在 2（macOS 实际复现过）。正确写法是按"计数达到
    // 阈值"驱动，暂态错误只是多试一轮。
    let mut attempts = 0;
    while tauri_delta_updater::get_failed_attempts(&work_dir).await < 3 {
        attempts += 1;
        assert!(
            attempts <= 8,
            "反复尝试仍未能累计到安全模式阈值，疑似确定性错误未被计数"
        );
        match updater.update().await {
            Err(e) => eprintln!("[attempt {attempts}] update 失败: {e:?}"),
            Ok(UpdateOutcome::SafeMode) => {} // 计数恰在本轮达到阈值
            Ok(other) => panic!("签名错误的清单必须导致更新失败，实际得到: {other:?}"),
        }
    }
    assert!(
        attempts >= 3,
        "至少需要 3 次确定性失败才能达到阈值，实际只尝试了 {attempts} 次"
    );

    // 达到上限后进入安全模式，不再发起网络请求
    match updater.update().await.unwrap() {
        UpdateOutcome::SafeMode => {}
        other => panic!("应当进入安全模式，实际得到: {other:?}"),
    }
}

/// 平台标识必须与 Tauri 的 target 命名一致。
#[test]
fn test_current_platform_matches_tauri_convention() {
    let platform = tauri_delta_updater::current_platform();

    let (os, arch) = platform
        .split_once('-')
        .expect("平台标识必须是 os-arch 形式");

    let expected_os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    assert_eq!(os, expected_os, "操作系统前缀必须与 Tauri 一致");

    let expected_arch = if cfg!(target_arch = "x86") {
        "i686"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };
    assert_eq!(arch, expected_arch, "架构后缀必须与 Tauri 一致");
}

/// 配置项默认值必须合理，无需调用方逐项填写。
#[test]
fn test_update_config_has_sensible_defaults() {
    let config = UpdateConfig::new("https://cdn.example.com/latest.json", "PUBKEY");

    assert_eq!(config.manifest_url, "https://cdn.example.com/latest.json");
    assert_eq!(
        config.signature_url, "https://cdn.example.com/latest.json.sig",
        "签名地址默认应为清单地址加 .sig"
    );
    assert_eq!(config.public_key, "PUBKEY");
    assert_eq!(
        config.platform,
        tauri_delta_updater::current_platform(),
        "平台默认取当前构建的平台"
    );
    assert!(
        config.executable_path.is_none() && config.work_dir.is_none(),
        "可执行文件与工作目录默认留空，由 DeltaUpdater::new 解析"
    );
    assert_eq!(config.wait_timeout_secs, 60);
    assert_eq!(config.confirm_timeout_secs, 60);

    // 链式设置
    let config = config
        .current_version("1.2.3")
        .platform("windows-x86_64")
        .work_dir(PathBuf::from("C:/tmp"))
        .wait_timeout_secs(30)
        .confirm_timeout_secs(45);
    assert_eq!(config.current_version, "1.2.3");
    assert_eq!(config.platform, "windows-x86_64");
    assert_eq!(
        config.work_dir.as_deref(),
        Some(std::path::Path::new("C:/tmp")),
        "显式设置的工作目录必须被保留"
    );
    assert_eq!(config.wait_timeout_secs, 30);
    assert_eq!(config.confirm_timeout_secs, 45);
}

/// 清单版本与当前版本相同时必须返回 UpToDate，且不下载任何东西。
#[tokio::test]
async fn test_end_to_end_up_to_date() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    // 清单版本与当前版本相同
    let server = MockServer::start(HashMap::new());
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.1.0",
        platform,
        &"a".repeat(64),
        &"b".repeat(64),
        None,
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    match updater.update().await.unwrap() {
        UpdateOutcome::UpToDate { version } => assert_eq!(version, "0.1.0"),
        other => panic!("同版本必须返回 UpToDate，实际得到: {other:?}"),
    }
    assert!(
        !server.was_requested("/full.exe"),
        "已是最新时不得请求全量安装包"
    );
    assert!(
        !work_dir
            .join(tauri_delta_updater::core::artifacts::staged_file())
            .exists(),
        "已是最新时不得产生合并产物"
    );
}

/// 重放一份旧的、签名合法的清单（降级）必须被拒绝，而不是回退到旧安装包。
///
/// 攻击模型：攻击者原样重放 0.1.9 的合法清单，希望客户端经"无匹配补丁 →
/// 回退全量"路径安装旧的 0.1.9 安装包。版本单调性判断必须在这里止步。
#[tokio::test]
async fn test_end_to_end_rejects_downgrade_replay() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"current version 0.2.0").unwrap();

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let server = MockServer::start(HashMap::new());
    // 清单是旧的 0.1.9，签名完全合法；全量地址指向旧安装包
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.1.9",
        platform,
        &"a".repeat(64),
        &"b".repeat(64),
        None,
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.2.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    match updater.update().await.unwrap() {
        UpdateOutcome::UpToDate { version } => assert_eq!(version, "0.1.9"),
        other => panic!("降级重放必须落到 UpToDate，实际得到: {other:?}"),
    }
    assert!(
        !server.was_requested("/full.exe"),
        "降级重放时绝不能引导宿主流向旧安装包"
    );
}

/// `check()` 在没有新版本时必须明确告知 is_newer = false。
#[tokio::test]
async fn test_end_to_end_check_reports_up_to_date() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let old_exe = dir.path().join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let server = MockServer::start(HashMap::new());
    let (manifest_bytes, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.1.0",
        platform,
        &"a".repeat(64),
        &"b".repeat(64),
        None,
    );
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let info = updater.check().await.unwrap();
    assert!(!info.is_newer, "同版本必须标记为不是更新");
    assert!(!info.has_patch, "同版本不得展示可下载补丁");
    assert!(info.fallback_reason.is_none());
}

/// 安全模式下 `check()` 返回可 match 的 `DeltaError::SafeMode`，且不发起网络请求。
#[tokio::test]
async fn test_end_to_end_check_reports_safe_mode() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let (sk, _pk_b64) = signing_keypair();
    let (_, wrong_pk) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();

    let manifest = serde_json::json!({
        "version": "0.2.0",
        "platforms": { platform: { "signature": "d", "url": "https://cdn.example.com/full.exe" } },
        "patches": { platform: [] }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &manifest_bytes);

    let server = MockServer::start(HashMap::from([
        ("/latest.json".to_string(), manifest_bytes),
        ("/latest.json.sig".to_string(), sig.into_bytes()),
    ]));

    // 预置失败计数到阈值
    for _ in 0..3 {
        tauri_delta_updater::record_failed_attempt(&work_dir)
            .await
            .unwrap();
    }

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), wrong_pk)
            .current_version("0.1.0")
            .executable_path(work_dir.join("app.exe"))
            .work_dir(&work_dir),
    )
    .unwrap();

    match updater.check().await {
        Err(tauri_delta_updater::DeltaError::SafeMode) => {}
        other => panic!("安全模式下 check() 必须返回 SafeMode，实际: {other:?}"),
    }
}

/// 更新锁必须跨进程互斥：另一个进程持锁时，本进程必须收到 `UpdateInProgress`。
///
/// # 为什么必须用子进程
///
/// `flock`/`fcntl`（Unix）与字节范围锁（Windows）都是**按进程归属**的：
/// 同一进程再次打开同一文件加锁一定成功。因此"同进程两次加锁"无法验证
/// 互斥语义——那正是最初写成同进程断言、在 CI 上失败的原因。
/// 这里让子进程真正持锁，父进程再去抢。
#[test]
fn test_update_lock_rejects_second_process() {
    use std::time::{Duration, Instant};

    let dir = tempdir().unwrap();
    let marker = dir.path().join("locked");
    let exe = std::env::current_exe().unwrap();

    let mut child = std::process::Command::new(exe)
        .args(["--ignored", "--exact", "lock_holder_child", "--nocapture"])
        .env("TDX_LOCK_DIR", dir.path())
        .env("TDX_LOCK_MARKER", &marker)
        .spawn()
        .expect("必须能启动子进程");

    // 等子进程真正拿到锁（写标记文件）再抢
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !marker.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("子进程未能取得更新锁");
    }

    let result = tauri_delta_updater::UpdateLock::acquire_in(dir.path());
    let _ = child.kill();
    let _ = child.wait();

    match result {
        Err(tauri_delta_updater::DeltaError::UpdateInProgress) => {}
        other => panic!("另一个进程持锁时本进程必须收到 UpdateInProgress，实际: {other:?}"),
    }
}

/// 供 [`test_update_lock_rejects_second_process`] 启动的子进程：
/// 拿到锁后写标记文件并保持一段时间。正常测试运行会跳过（`#[ignore]`）。
#[test]
#[ignore]
fn lock_holder_child() {
    let Some(dir) = std::env::var_os("TDX_LOCK_DIR") else {
        return;
    };
    let Some(marker) = std::env::var_os("TDX_LOCK_MARKER") else {
        return;
    };
    let _lock = tauri_delta_updater::UpdateLock::acquire_in(std::path::Path::new(&dir))
        .expect("子进程必须能拿到锁");
    std::fs::write(marker, b"ok").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(20));
}
