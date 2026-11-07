//! 端到端：全量安装包下载、验签与版本判断。

mod common;
use common::*;

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tauri_delta_updater::{
    DeltaError, DeltaUpdater, FullDownloadOutcome, UpdateConfig, UpdateEvent,
};
use tempfile::tempdir;

/// 全量回退夹具：清单里带**真实安装包签名**，服务端提供安装包内容。
async fn full_fixture(
    installer: &[u8],
    current_version: &str,
) -> (tempfile::TempDir, DeltaUpdater, MockServer) {
    let dir = tempdir().unwrap();
    let old_exe = dir.path().join("app.exe");
    std::fs::write(&old_exe, b"OLD").unwrap();

    let server = MockServer::start(HashMap::new());
    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    // 安装包签名与清单签名是两把不同的"签名对象"：
    // platforms[].signature 必须对安装包字节本身签名。
    let installer_sig = STANDARD.encode(sign(&sk, installer).as_bytes());
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "full installer",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: { "signature": installer_sig, "url": server.url("/full.exe") }
        },
        "patches": {}
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    server.set("/latest.json", bytes.clone());
    server.set("/latest.json.sig", sign(&sk, &bytes).into_bytes());
    server.set("/full.exe", installer.to_vec());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version(current_version)
            .platform(platform)
            .executable_path(&old_exe)
            .work_dir(&dir),
    )
    .unwrap();

    (dir, updater, server)
}

/// 全量下载：安装包落盘、签名校验通过、阶段事件正确。
#[tokio::test]
async fn test_full_download_verifies_installer() {
    let installer = b"MZ this is a fake NSIS installer payload".to_vec();
    let (_dir, updater, server) = full_fixture(&installer, "0.1.0").await;

    let mut events = Vec::new();
    let outcome = updater
        .download_full_with_events(&tauri_delta_updater::CancelToken::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();

    let FullDownloadOutcome::Downloaded { installer: staged } = outcome else {
        panic!("必须下载到安装包");
    };
    assert_eq!(staged.version(), "0.2.0");
    assert!(
        staged.path().is_file(),
        "安装包必须落盘到工作目录: {}",
        staged.path().display()
    );
    assert_eq!(std::fs::read(staged.path()).unwrap(), installer);
    assert!(
        staged
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".exe"),
        "扩展名必须按 URL 推断，否则 Windows 无法执行"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UpdateEvent::DownloadingFull { .. })),
        "必须发出全量下载进度事件"
    );
    assert!(server.was_requested("/full.exe"));
}

/// 安装包签名不匹配必须拒绝并删除下载内容（不能被替换后的安装器利用）。
#[tokio::test]
async fn test_full_download_rejects_tampered_installer() {
    let installer = b"MZ signed payload".to_vec();
    let (dir, updater, server) = full_fixture(&installer, "0.1.0").await;
    // 服务端偷偷换成另一份字节
    server.set("/full.exe", b"MZ evil payload".to_vec());

    let err = updater.download_full().await.unwrap_err();
    assert!(
        matches!(err, DeltaError::SignatureInvalid(_)),
        "安装包被替换时必须报签名错误，实际: {err:?}"
    );
    // 签名失败后不应留下可执行文件
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("tdx_full_installer")
        })
        .collect();
    assert!(leftovers.is_empty(), "验签失败的安装包必须删除");
}

/// 同版本（或更低版本）不得下载安装包：这是防降级的最后一道闸门。
#[tokio::test]
async fn test_full_download_up_to_date_makes_no_request() {
    let installer = b"MZ payload".to_vec();
    // 客户端已经是清单里的版本：即使签名合法也不该再下载安装包
    let (_dir, updater, server) = full_fixture(&installer, "0.2.0").await;

    let outcome = updater.download_full().await.unwrap();
    assert!(
        matches!(outcome, FullDownloadOutcome::UpToDate { .. }),
        "同版本必须返回 UpToDate，实际: {outcome:?}"
    );
    assert!(
        !server.was_requested("/full.exe"),
        "已是最新时绝不能请求安装包"
    );
}
