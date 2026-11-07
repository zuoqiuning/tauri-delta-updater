//! 端到端测试共用的本地 HTTP 服务与签名夹具。

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tauri_delta_updater::{DeltaUpdater, UpdateConfig};
use tempfile::tempdir;

/// 一个静态文件服务器，按路径返回内容，并记录收到的每个请求路径。
pub struct MockServer {
    pub base_url: String,
    served: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
    requested: Arc<std::sync::Mutex<Vec<String>>>,
    /// 某个路径接下来还要返回多少次 502（用于重试验证）。
    failures: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    /// 全局要求的请求头（用于验证自定义 headers 真的发出去了）。
    required_header: Arc<std::sync::Mutex<Option<(String, String)>>>,
}

impl MockServer {
    pub fn start(files: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::new(std::sync::Mutex::new(files));
        let requested: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let failures: Arc<std::sync::Mutex<HashMap<String, usize>>> = Arc::default();
        let required_header: Arc<std::sync::Mutex<Option<(String, String)>>> = Arc::default();
        let handler_state = Arc::clone(&served);
        let handler_requested = Arc::clone(&requested);
        let handler_failures = Arc::clone(&failures);
        let handler_header = Arc::clone(&required_header);

        std::thread::spawn(move || {
            for stream in listener.incoming().take(32) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let request = String::from_utf8_lossy(&buf);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                handler_requested.lock().unwrap().push(path.clone());

                // 请求头校验：缺头或值不符直接 403，便于验证 headers 配置。
                let header_ok = match handler_header.lock().unwrap().clone() {
                    Some((name, value)) => has_header(&request, &name, &value),
                    None => true,
                };

                let fail = {
                    let mut failures = handler_failures.lock().unwrap();
                    match failures.get_mut(&path) {
                        Some(0) | None => false,
                        Some(remaining) => {
                            *remaining -= 1;
                            true
                        }
                    }
                };

                let body = handler_state.lock().unwrap().get(&path).cloned();
                let response = if !header_ok {
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                } else if fail {
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                } else {
                    match body {
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
            failures,
            required_header,
        }
    }

    /// 要求所有请求携带指定头；缺头或值不符时返回 403。
    pub fn require_header(&self, name: &str, value: &str) {
        *self.required_header.lock().unwrap() = Some((name.to_string(), value.to_string()));
    }

    /// 让某个路径接下来的 `count` 次请求返回 502（验证重试与退避）。
    pub fn fail_next(&self, path: &str, count: usize) {
        self.failures
            .lock()
            .unwrap()
            .insert(path.to_string(), count);
    }

    /// 某个路径是否被请求过。
    ///
    /// 用于堵"delta 从不 delta"的测试盲区：一个永远走全量回退的增量更新器，
    /// 与正常工作的增量更新器，在只断言产物正确时看起来一模一样。
    /// 增量命中的测试必须证明全量安装包**从未被请求**。
    pub fn was_requested(&self, path: &str) -> bool {
        self.requested.lock().unwrap().iter().any(|p| p == path)
    }

    /// 服务启动后更新某个路径的内容。
    ///
    /// 用于"manifest 引用自身的 URL"这类先有鸡还是先有蛋的场景：
    /// 先起服务拿到端口，再往里放引用该端口的清单。
    pub fn set(&self, path: &str, body: Vec<u8>) {
        self.served.lock().unwrap().insert(path.to_string(), body);
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// 某个路径被请求了多少次。
    pub fn request_count(&self, path: &str) -> usize {
        self.requested
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.as_str() == path)
            .count()
    }
}

/// 生成一对签名密钥，返回 (私钥, 公钥 base64)。
pub fn signing_keypair() -> (minisign::SecretKey, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_b64 = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
    (kp.sk, pk_b64)
}

/// 对内容签名，返回 minisign 签名字符串。
pub fn sign(sk: &minisign::SecretKey, content: &[u8]) -> String {
    minisign::sign(None, sk, std::io::Cursor::new(content), None, None)
        .unwrap()
        .to_string()
}

/// 对内容签名并写入 trusted comment（版本绑定的载体）。
pub fn sign_bound(sk: &minisign::SecretKey, content: &[u8], trusted_comment: &str) -> String {
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
pub fn build_manifest(
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

/// 构造一份带灰度比例的已签名清单。
///
/// 比例写在 `platforms[].rollout`（受签名保护），用于验证客户端分桶。
#[allow(clippy::too_many_arguments)]
pub fn build_manifest_with_rollout(
    sk: &minisign::SecretKey,
    base_url: &str,
    version: &str,
    platform: &str,
    patch_hash: &str,
    target_hash: &str,
    patch_size: Option<u64>,
    percentage: u8,
    min_version: Option<&str>,
) -> (Vec<u8>, String) {
    let mut platforms = serde_json::Map::new();
    platforms.insert(
        platform.to_string(),
        serde_json::json!({
            "signature": "dummy",
            "url": format!("{base_url}/full.exe"),
            "rollout": { "percentage": percentage }
        }),
    );
    let mut patches = serde_json::Map::new();
    patches.insert(
        platform.to_string(),
        serde_json::json!([{
            "from_version": "0.1.0",
            "to_version": version,
            "patch_url": format!("{base_url}/update.patch"),
            "patch_hash": patch_hash,
            "target_hash": target_hash,
            "patch_size": patch_size
        }]),
    );

    let mut manifest = serde_json::json!({
        "version": version,
        "notes": "test",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": platforms,
        "patches": patches
    });
    if let Some(min) = min_version {
        manifest["min_version"] = min.into();
    }
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(sk, &bytes);
    (bytes, sig)
}

/// SHA-256 十六进制。
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// 构建一套"0.1.0 → 0.2.0"的完整增量夹具。
///
/// 返回值里的 `TempDir` 必须存活到测试结束：旧 exe 与工作目录都在里面。
pub async fn delta_fixture() -> (tempfile::TempDir, DeltaUpdater, MockServer, Vec<u8>) {
    delta_fixture_with_rollout(None).await
}

/// 与 [`delta_fixture`] 相同，但配置了整包清单地址（用于验证整包不可用时的回退路径）。
pub async fn delta_fixture_with_bundle() -> (tempfile::TempDir, DeltaUpdater, MockServer, Vec<u8>) {
    delta_fixture_with_policy(None, None, true, Vec::new()).await
}

/// 与 [`delta_fixture`] 相同，但客户端带自定义请求头。
pub async fn delta_fixture_with_headers(
    headers: Vec<(String, String)>,
) -> (tempfile::TempDir, DeltaUpdater, MockServer, Vec<u8>) {
    delta_fixture_with_policy(None, None, false, headers).await
}

/// 与 [`delta_fixture`] 相同，但清单可带灰度比例。
pub async fn delta_fixture_with_rollout(
    rollout: Option<u8>,
) -> (tempfile::TempDir, DeltaUpdater, MockServer, Vec<u8>) {
    delta_fixture_with_policy(rollout, None, false, Vec::new()).await
}

/// 灰度 + 最低版本门槛的夹具。
pub async fn delta_fixture_with_rollout_and_min(
    percentage: u8,
    min_version: &str,
) -> (tempfile::TempDir, DeltaUpdater, MockServer, Vec<u8>) {
    delta_fixture_with_policy(Some(percentage), Some(min_version), false, Vec::new()).await
}

async fn delta_fixture_with_policy(
    rollout: Option<u8>,
    min_version: Option<&str>,
    with_bundle: bool,
    headers: Vec<(String, String)>,
) -> (tempfile::TempDir, DeltaUpdater, MockServer, Vec<u8>) {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, &old_bytes).unwrap();

    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let new_exe = work_dir.join("app_new.exe");
    std::fs::write(&new_exe, &new_bytes).unwrap();
    let new_hash = sha256(&new_bytes);

    let patch_path = dir.path().join("update.patch");
    tauri_delta_updater::create_patch(&old_exe, &new_exe, &patch_path)
        .await
        .unwrap();
    let patch_bytes = std::fs::read(&patch_path).unwrap();
    let patch_hash = sha256(&patch_bytes);

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let patch_size = patch_bytes.len() as u64;

    // 先起服务拿到端口，再往里放引用该端口的清单与签名
    let server = MockServer::start(HashMap::from([("/update.patch".to_string(), patch_bytes)]));
    let (manifest_bytes, sig) = match rollout {
        Some(percentage) => build_manifest_with_rollout(
            &sk,
            &server.base_url,
            "0.2.0",
            platform,
            &patch_hash,
            &new_hash,
            Some(patch_size),
            percentage,
            min_version,
        ),
        None => build_manifest(
            &sk,
            &server.base_url,
            "0.2.0",
            platform,
            &patch_hash,
            &new_hash,
            Some(patch_size),
        ),
    };
    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new({
        let config = UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir)
            .headers(headers);
        if with_bundle {
            config.bundle_manifest_url(server.url("/latest.bundle.json"))
        } else {
            config
        }
    })
    .unwrap();

    (dir, updater, server, new_bytes)
}

pub fn write_file(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// 请求文本里是否包含指定请求头（名字大小写不敏感，值精确匹配）。
fn has_header(request: &str, name: &str, expected: &str) -> bool {
    request.lines().any(|line| {
        let Some((key, value)) = line.split_once(':') else {
            return false;
        };
        key.trim().eq_ignore_ascii_case(name) && value.trim() == expected
    })
}
