//! 结构化事件断言。
//!
//! 单独成文件的原因：`tracing` 的线程局部订阅器在并行测试里会被 libtest
//! 的线程调度打散；这里用**进程级全局订阅器** + 单测试二进制，保证断言稳定。
//!
//! 验收目标：增量命中时 `delta.patch.matched` 必须出现，`delta.fallback`
//! 必须不出现——否则"永远静默回退"的更新器也能在事件层看起来正常。

#![cfg(feature = "tracing")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tauri_delta_updater::{calculate_hash, create_patch, DeltaUpdater, UpdateConfig};
use tempfile::tempdir;

/// 静态文件服务器：按路径返回内容。
struct MockServer {
    base_url: String,
    served: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl MockServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let handler = Arc::clone(&served);

        std::thread::spawn(move || {
            for stream in listener.incoming().take(16) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let path = String::from_utf8_lossy(&buf)
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let body = handler.lock().unwrap().get(&path).cloned();
                let response = match body {
                    Some(body) => {
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
        }
    }

    fn set(&self, path: &str, body: Vec<u8>) {
        self.served.lock().unwrap().insert(path.to_string(), body);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

/// 捕获 tracing 输出的写入器。
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Buffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn delta_hit_emits_matched_not_fallback() {
    let buffer = Buffer::default();
    tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .init();

    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let old_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let old_exe = work_dir.join("app.exe");
    let new_exe = work_dir.join("app_new.exe");
    std::fs::write(&old_exe, &old_bytes).unwrap();
    std::fs::write(&new_exe, &new_bytes).unwrap();

    let patch_path = dir.path().join("update.patch");
    create_patch(&old_exe, &new_exe, &patch_path).await.unwrap();
    let patch_hash = calculate_hash(&patch_path).await.unwrap();
    let target_hash = calculate_hash(&new_exe).await.unwrap();

    let (sk, pk_b64) = {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pk = STANDARD.encode(kp.pk.to_box().unwrap().to_string().as_bytes());
        (kp.sk, pk)
    };

    let server = MockServer::start();
    let platform = tauri_delta_updater::current_platform();
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "test",
        "platforms": { platform: { "signature": "dummy", "url": format!("{}/full.exe", server.base_url) } },
        "patches": {
            platform: [{
                "from_version": "0.1.0",
                "to_version": "0.2.0",
                "patch_url": format!("{}/update.patch", server.base_url),
                "patch_hash": patch_hash,
                "target_hash": target_hash,
            }]
        }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let signature = minisign::sign(None, &sk, std::io::Cursor::new(&manifest_bytes), None, None)
        .unwrap()
        .to_string();

    server.set("/latest.json", manifest_bytes);
    server.set("/latest.json.sig", signature.into_bytes());
    server.set("/update.patch", std::fs::read(&patch_path).unwrap());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir)
            .auto_launch(false),
    )
    .unwrap();

    // prepare() 走完整的"匹配 → 下载 → 应用"路径
    let _ = updater.prepare().await.unwrap();

    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("delta.patch.matched"),
        "增量命中必须发出 matched 事件，实际输出: {text}"
    );
    assert!(
        !text.contains("delta.fallback"),
        "增量命中时不得发出 fallback 事件，实际输出: {text}"
    );
}
