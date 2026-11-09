//! `tauri-delta-updater` 接入方的集成测试夹具。
//!
//! 写一个真实的增量更新测试需要三样东西：能返回清单/补丁/安装包的 HTTP 服务、
//! 一对 minisign 密钥、以及一份签名正确的清单。这三样在引擎仓库里已有实现，
//! 但一直藏在 `tests/` 内部——外部接入者只能重造。本 crate 把它导出：
//!
//! - [`MockServer`]：按路径返回字节、记录请求、可注入 502、可要求请求头，
//!   并支持 `Range`（验证断点续传）；
//! - [`signing_keypair`] / [`sign`] / [`sign_bound`]：生成密钥与签名；
//! - [`manifest_json`] / [`signed_manifest`] / [`build_manifest`]：构造清单；
//! - [`sha256`] / [`write_file`]：夹具常用的小工具。
//!
//! 它只依赖签名与 JSON 库，不依赖引擎本身——独立发布、独立升级，
//! 不会把测试依赖带进生产构建。
//!
//! ```no_run
//! use tdx_testkit::{build_manifest, signing_keypair, MockServer};
//!
//! let (sk, public_key_b64) = signing_keypair();
//! let server = MockServer::empty();
//!
//! // 先起服务拿到端口，再构造引用该端口的清单（清单要指向真实地址）。
//! let patch = std::fs::read("update.patch").unwrap();
//! let patch_hash = tdx_testkit::sha256(&patch);
//! let (manifest, _sig) = build_manifest(
//!     &sk,
//!     &server.base_url,
//!     "0.2.0",
//!     "windows-x86_64",
//!     "0.1.0",
//!     &patch_hash,
//!     "TARGET_SHA256",
//!     Some(patch.len() as u64),
//! );
//! server.set("/update.patch", patch);
//! server.serve_signed("/latest.json", manifest, &sk);
//!
//! // UpdateConfig::new(server.url("/latest.json"), public_key_b64)
//! # let _ = (public_key_b64, server.url("/latest.json"));
//! ```

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use minisign::SecretKey;

/// 一个静态文件服务器，按路径返回内容，并记录收到的每个请求。
///
/// 只处理 `GET`，每个连接读一次请求后即关闭（`Connection: close`），
/// 足够覆盖更新器的下载/重试/续传场景，不需要异步运行时。
pub struct MockServer {
    /// 形如 `http://127.0.0.1:PORT`。
    pub base_url: String,
    served: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requested: Arc<Mutex<Vec<String>>>,
    failures: Arc<Mutex<HashMap<String, usize>>>,
    required_header: Arc<Mutex<Option<(String, String)>>>,
}

impl MockServer {
    /// 预置若干路径的内容；后续可用 [`Self::set`] 增删。
    ///
    /// 服务端最多处理 [`Self::MAX_REQUESTS`] 个连接：测试用完即弃，
    /// 不设上限会让每个用例永久占用一个被阻塞的线程。
    pub fn start(files: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::new(Mutex::new(files));
        let requested: Arc<Mutex<Vec<String>>> = Arc::default();
        let failures: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let required_header: Arc<Mutex<Option<(String, String)>>> = Arc::default();
        let handler_state = Arc::clone(&served);
        let handler_requested = Arc::clone(&requested);
        let handler_failures = Arc::clone(&failures);
        let handler_header = Arc::clone(&required_header);

        std::thread::spawn(move || {
            for stream in listener.incoming().take(Self::MAX_REQUESTS) {
                let Ok(mut stream) = stream else { continue };
                let request = read_request(&mut stream);
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
                    fixed_response("403 Forbidden").to_vec()
                } else if fail {
                    fixed_response("502 Bad Gateway").to_vec()
                } else {
                    match body {
                        Some(body) => match range_start(&request) {
                            Some(start) if start < body.len() as u64 => {
                                partial_response(&body, start as usize)
                            }
                            Some(_) => {
                                // 本地残留比服务端资源还大：续传无法完成，
                                // 引擎据此丢弃残留并完整重下。
                                format!(
                                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                    body.len()
                                )
                                .into_bytes()
                            }
                            None => ok_response(&body),
                        },
                        None => fixed_response("404 Not Found").to_vec(),
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

    /// 空服务；清单需要引用服务自身端口时先起它再 [`Self::set`]。
    pub fn empty() -> Self {
        Self::start(HashMap::new())
    }

    /// 服务端最多处理的连接数。
    pub const MAX_REQUESTS: usize = 1024;

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

    /// 写入内容并把签名放到 `{path}.sig`——引擎就是这么取清单签名的。
    ///
    /// 返回签名字符串，需要嵌进清单时可直接使用。
    pub fn serve_signed(&self, path: &str, body: Vec<u8>, sk: &SecretKey) -> String {
        let signature = sign(sk, &body);
        self.set(path, body);
        self.set(&format!("{path}.sig"), signature.clone().into_bytes());
        signature
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
///
/// 公钥 base64 直接用于 `UpdateConfig::new(..., pubkey)`。
pub fn signing_keypair() -> (SecretKey, String) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk_b64 = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
    (kp.sk, pk_b64)
}

/// 对内容签名，返回 minisign 签名字符串。
pub fn sign(sk: &SecretKey, content: &[u8]) -> String {
    minisign::sign(None, sk, std::io::Cursor::new(content), None, None)
        .unwrap()
        .to_string()
}

/// 对内容签名并写入 trusted comment（版本绑定的载体）。
pub fn sign_bound(sk: &SecretKey, content: &[u8], trusted_comment: &str) -> String {
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

/// 一份待签名清单的最小骨架：版本、平台与全量安装包地址。
///
/// 补丁、灰度、`min_version` 等字段直接往返回的 JSON 上改，再交给
/// [`signed_manifest`]——不需要为每个可选字段背一个参数列表。
pub fn manifest_json(version: &str, platform: &str, full_url: &str) -> serde_json::Value {
    serde_json::json!({
        "version": version,
        "notes": "test",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: {
                "signature": "dummy",
                "url": full_url
            }
        }
    })
}

/// 序列化并签名一份清单，返回 (清单字节, 签名)。
pub fn signed_manifest(manifest: &serde_json::Value, sk: &SecretKey) -> (Vec<u8>, String) {
    let bytes = serde_json::to_vec_pretty(manifest).unwrap();
    let sig = sign(sk, &bytes);
    (bytes, sig)
}

/// 构造一份已签名、含补丁的清单。
///
/// 全量安装包地址落在 `{base_url}/full.exe`，夹具通常**不**提供它的内容：
/// 这样测试能通过请求日志证明"增量命中时全量从未被请求"。
#[allow(clippy::too_many_arguments)]
pub fn build_manifest(
    sk: &SecretKey,
    base_url: &str,
    version: &str,
    platform: &str,
    from_version: &str,
    patch_hash: &str,
    target_hash: &str,
    patch_size: Option<u64>,
) -> (Vec<u8>, String) {
    let mut manifest = manifest_json(version, platform, &format!("{base_url}/full.exe"));
    manifest["patches"] = serde_json::json!({
        platform: [{
            "from_version": from_version,
            "to_version": version,
            "patch_url": format!("{base_url}/update.patch"),
            "patch_hash": patch_hash,
            "target_hash": target_hash,
            "patch_size": patch_size
        }]
    });
    signed_manifest(&manifest, sk)
}

/// 构造一份带灰度比例的已签名清单。
///
/// 比例写在 `platforms[].rollout`（受签名保护），用于验证客户端分桶。
#[allow(clippy::too_many_arguments)]
pub fn build_manifest_with_rollout(
    sk: &SecretKey,
    base_url: &str,
    version: &str,
    platform: &str,
    from_version: &str,
    patch_hash: &str,
    target_hash: &str,
    patch_size: Option<u64>,
    percentage: u8,
    min_version: Option<&str>,
) -> (Vec<u8>, String) {
    let mut manifest = manifest_json(version, platform, &format!("{base_url}/full.exe"));
    manifest["platforms"][platform]["rollout"] = serde_json::json!({ "percentage": percentage });
    manifest["patches"] = serde_json::json!({
        platform: [{
            "from_version": from_version,
            "to_version": version,
            "patch_url": format!("{base_url}/update.patch"),
            "patch_hash": patch_hash,
            "target_hash": target_hash,
            "patch_size": patch_size
        }]
    });
    if let Some(min) = min_version {
        manifest["min_version"] = min.into();
    }
    signed_manifest(&manifest, sk)
}

/// SHA-256 十六进制。
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// 在 `root` 下按相对路径写文件，自动创建父目录。
pub fn write_file(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// 读一次完整请求头。
///
/// TCP 不保证一次 `read` 能拿到整个请求：Windows 上常见"只到了半行"的情况，
/// 按单次读取解析会把 `Range` 头裁掉，续传测试就会静默退化成完整下载。
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() >= 2048 {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
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

/// 解析 `Range: bytes=<start>-` 的起点；没有该头时为 `None`。
fn range_start(request: &str) -> Option<u64> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if !key.trim().eq_ignore_ascii_case("range") {
            return None;
        }
        value
            .trim()
            .strip_prefix("bytes=")?
            .split('-')
            .next()?
            .trim()
            .parse()
            .ok()
    })
}

fn fixed_response(status: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
}

fn ok_response(body: &[u8]) -> Vec<u8> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    [head.as_bytes(), body].concat()
}

fn partial_response(body: &[u8], start: usize) -> Vec<u8> {
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
        body.len() - start,
        start,
        body.len() - 1,
        body.len()
    );
    [head.as_bytes(), &body[start..]].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;

    fn get(server: &MockServer, path: &str, extra: &str) -> String {
        let addr = server.base_url.trim_start_matches("http://");
        let mut stream = TcpStream::connect(addr).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{extra}\r\n").unwrap();
        // 服务端写完即关闭；Windows 上可能表现为 RST，已收到的字节仍然有效。
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => response.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8_lossy(&response).into_owned()
    }

    #[test]
    fn serves_files_and_records_requests() {
        let server = MockServer::start(HashMap::from([("/a".into(), b"hello".to_vec())]));
        assert!(get(&server, "/a", "").ends_with("hello"));
        assert!(get(&server, "/missing", "").starts_with("HTTP/1.1 404"));
        assert!(server.was_requested("/a"));
        assert_eq!(server.request_count("/a"), 1);
    }

    #[test]
    fn range_requests_return_partial_content() {
        let server = MockServer::start(HashMap::from([("/a".into(), b"0123456789".to_vec())]));
        let response = get(&server, "/a", "Range: bytes=6-\r\n");
        assert!(response.starts_with("HTTP/1.1 206"));
        assert!(response.contains("Content-Range: bytes 6-9/10"));
        assert!(response.ends_with("6789"));

        let beyond = get(&server, "/a", "Range: bytes=10-\r\n");
        assert!(beyond.starts_with("HTTP/1.1 416"));
    }

    #[test]
    fn serve_signed_also_stores_the_signature() {
        let (sk, _public) = signing_keypair();
        let server = MockServer::empty();
        let signature = server.serve_signed("/latest.json", b"{}".to_vec(), &sk);

        assert!(!server.was_requested("/latest.json"));
        assert!(get(&server, "/latest.json.sig", "").ends_with(&signature));
        assert!(get(&server, "/latest.json", "").ends_with("{}"));
    }

    #[test]
    fn required_header_is_enforced() {
        let server = MockServer::empty();
        server.set("/a", b"body".to_vec());
        server.require_header("x-token", "secret");

        assert!(get(&server, "/a", "").starts_with("HTTP/1.1 403"));
        assert!(get(&server, "/a", "x-token: secret\r\n").ends_with("body"));
    }

    #[test]
    fn failures_are_counted_down() {
        let server = MockServer::empty();
        server.set("/a", b"body".to_vec());
        server.fail_next("/a", 2);

        assert!(get(&server, "/a", "").starts_with("HTTP/1.1 502"));
        assert!(get(&server, "/a", "").starts_with("HTTP/1.1 502"));
        assert!(get(&server, "/a", "").ends_with("body"));
    }

    #[test]
    fn signed_manifest_embeds_patch_fields() {
        let (sk, _public) = signing_keypair();
        let (bytes, signature) = build_manifest(
            &sk,
            "http://127.0.0.1:1",
            "0.2.0",
            "windows-x86_64",
            "0.1.0",
            "patch-hash",
            "target-hash",
            Some(42),
        );
        let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(manifest["version"], "0.2.0");
        assert_eq!(manifest["patches"]["windows-x86_64"][0]["patch_size"], 42);
        assert!(!signature.is_empty());
    }
}
