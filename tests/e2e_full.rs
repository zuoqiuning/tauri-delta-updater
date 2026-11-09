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
    full_fixture_with(installer, current_version, serde_json::json!({})).await
}

/// 与 [`full_fixture`] 相同，可向平台条目追加字段（`rollout` / `min_os`）。
async fn full_fixture_with(
    installer: &[u8],
    current_version: &str,
    platform_extra: serde_json::Value,
) -> (tempfile::TempDir, DeltaUpdater, MockServer) {
    full_fixture_at(installer, current_version, platform_extra, "/full.exe").await
}

/// 与 [`full_fixture_with`] 相同，但可指定安装包 URL 的路径。
async fn full_fixture_at(
    installer: &[u8],
    current_version: &str,
    platform_extra: serde_json::Value,
    url_path: &str,
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
    let mut platform_entry = serde_json::json!({
        "signature": installer_sig,
        "url": server.url(url_path)
    });
    if let Some(extra) = platform_extra.as_object() {
        for (key, value) in extra {
            platform_entry[key] = value.clone();
        }
    }
    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "full installer",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": { platform: platform_entry },
        "patches": {}
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    server.set("/latest.json", bytes.clone());
    server.set("/latest.json.sig", sign(&sk, &bytes).into_bytes());
    server.set("/full.exe", installer.to_vec());
    server.set(url_path, installer.to_vec());

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

/// `.deb` / `.rpm` 必须在下载前拒绝：下载几十 MB 后才说不支持是纯浪费。
#[tokio::test]
async fn test_full_download_rejects_deb_before_download() {
    let (_dir, updater, _server) =
        full_fixture_at(b"deb package", "0.1.0", serde_json::json!({}), "/app.deb").await;

    let err = updater.download_full().await.unwrap_err();
    assert!(matches!(err, DeltaError::Config(_)), "{err:?}");
    assert!(err.to_string().contains(".deb"), "{err}");
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

/// 灰度未命中时全量路径同样不下载；`check()` 隐藏的地址不能经
/// `full_download()` 重新暴露。
#[tokio::test]
async fn test_full_download_respects_rollout_gate() {
    let installer = b"MZ payload".to_vec();
    let (_dir, updater, server) = full_fixture_with(
        &installer,
        "0.1.0",
        serde_json::json!({ "rollout": { "percentage": 0 } }),
    )
    .await;

    match updater.download_full().await.unwrap() {
        FullDownloadOutcome::Deferred {
            version,
            percentage,
        } => {
            assert_eq!(version, "0.2.0");
            assert_eq!(percentage, 0, "清单声明的放量比例必须原样返回");
        }
        other => panic!("灰度 0% 时必须返回 Deferred，实际: {other:?}"),
    }
    assert!(
        !server.was_requested("/full.exe"),
        "灰度未命中不得下载安装包"
    );

    let check = updater.check().await.unwrap();
    assert!(check.deferred_by_rollout);
    assert!(check.full_download_url.is_none(), "check 必须隐藏全量地址");
    assert!(
        check.full_download().is_none(),
        "full_download() 不得绕过灰度门槛重新暴露 URL"
    );
}

/// 系统版本低于清单要求时全量路径同样拒绝下载。
#[tokio::test]
async fn test_full_download_respects_min_os_gate() {
    let installer = b"MZ payload".to_vec();
    let (_dir, updater, server) = full_fixture_with(
        &installer,
        "0.1.0",
        serde_json::json!({ "min_os": "999.0" }),
    )
    .await;

    let outcome = updater.download_full().await.unwrap();
    assert!(
        matches!(outcome, FullDownloadOutcome::UnsupportedOs { .. }),
        "min_os 不满足时必须返回 UnsupportedOs，实际: {outcome:?}"
    );
    assert!(
        !server.was_requested("/full.exe"),
        "不兼容系统不得下载安装包"
    );

    let check = updater.check().await.unwrap();
    assert!(
        check.full_download().is_none(),
        "full_download() 不得绕过系统版本门槛"
    );
}

/// 全量下载与增量更新共用一把锁：并发调用必须返回可重试的
/// `update_in_progress`，而不是两个下载同时写工作目录。
#[tokio::test]
async fn test_full_download_is_serialized_by_update_lock() {
    let installer = b"MZ payload".to_vec();
    let (dir, updater, _server) = full_fixture(&installer, "0.1.0").await;

    let held = tauri_delta_updater::UpdateLock::acquire_in(dir.path()).unwrap();
    let err = updater.download_full().await.unwrap_err();
    assert!(
        matches!(err, DeltaError::UpdateInProgress),
        "锁被占用时必须返回 UpdateInProgress，实际: {err:?}"
    );
    drop(held);
}

/// Windows：上次整包替换未完成时，全量路径必须先修复而不是覆盖现场。
#[cfg(target_os = "windows")]
#[tokio::test]
async fn test_full_download_refuses_when_repair_is_pending() {
    use tauri_delta_updater::core::artifacts;

    let installer = b"MZ payload".to_vec();
    let (dir, updater, server) = full_fixture(&installer, "0.1.0").await;

    let backup = dir.path().join(artifacts::BUNDLE_BACKUP_DIR);
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("app.exe"), b"old").unwrap();

    let err = updater.download_full().await.unwrap_err();
    assert!(
        matches!(err, DeltaError::RepairRequired),
        "存在未完成整包替换时必须要求先修复，实际: {err:?}"
    );
    assert!(!server.was_requested("/full.exe"), "修复前不得发起下载");
}
