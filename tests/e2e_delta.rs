//! 端到端：单文件增量、检查/准备/启动、会话、取消与配置校验。

mod common;
use common::*;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tauri_delta_updater::{
    CancelToken, DeltaError, DeltaUpdater, FallbackPolicy, TrustPolicy, UpdateConfig, UpdateEvent,
    UpdateOutcome, UpdateResult,
};
use tempfile::tempdir;

/// 端到端：清单 + 补丁 → 一次调用 → 生成新版本。
#[tokio::test]
async fn test_end_to_end_delta_update() {
    let (_dir, updater, server, new_bytes) = delta_fixture().await;

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

/// 自定义请求头必须真的发到服务端；缺失时服务端返回 403。
#[tokio::test]
async fn test_custom_headers_are_sent() {
    let (_dir, updater, server, _new_bytes) =
        delta_fixture_with_headers(vec![("X-Token".into(), "secret".into())]).await;
    server.require_header("X-Token", "secret");
    assert!(
        updater.check().await.is_ok(),
        "带正确请求头时必须能拉取清单"
    );

    // 反向验证：同一个服务端要求请求头时，不带头的客户端必须被拒绝。
    let (_dir2, plain, server2, _new2) = delta_fixture().await;
    server2.require_header("X-Token", "secret");
    let err = plain.check().await.unwrap_err();
    assert!(
        matches!(err, DeltaError::HttpStatus(403)),
        "缺少请求头必须被服务端拒绝，实际: {err:?}"
    );
}

/// 移动端与未知平台没有替换脚本：必须在构造阶段拒绝，而不是下载后才失败。
#[test]
fn test_updater_rejects_mobile_or_unknown_platform() {
    let dir = tempdir().unwrap();
    let exe = dir.path().join("app");
    std::fs::write(&exe, b"app").unwrap();
    for platform in ["android-aarch64", "ios-aarch64", "unknown-unknown"] {
        let err = DeltaUpdater::new(
            UpdateConfig::new("https://cdn.example.com/latest.json", "PK")
                .current_version("1.0.0")
                .platform(platform)
                .executable_path(&exe)
                .work_dir(dir.path()),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("不支持的平台"),
            "{platform} 必须在构造阶段被拒绝: {err}"
        );
    }
}

/// 显式配置的签名地址必须被真正使用，而不是始终按清单地址推导。
#[tokio::test]
async fn test_custom_signature_url_is_used() {
    let dir = tempdir().unwrap();
    let exe = dir.path().join("app.exe");
    let work_dir = dir.path().join("work");
    std::fs::write(&exe, b"app").unwrap();
    std::fs::create_dir_all(&work_dir).unwrap();

    let (sk, pk) = signing_keypair();
    let server = MockServer::start(HashMap::new());
    let platform = tauri_delta_updater::current_platform();
    let (manifest_bytes, signature) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        "patch-hash",
        "target-hash",
        None,
    );
    server.set("/latest.json", manifest_bytes);
    // 默认 .sig 地址放错误内容：如果客户端仍按地址推导，验签必然失败。
    server.set("/latest.json.sig", b"wrong signature".to_vec());
    server.set("/signatures/latest.sig", signature.into_bytes());

    let mut config = UpdateConfig::new(server.url("/latest.json"), pk)
        .current_version("0.1.0")
        .executable_path(&exe)
        .work_dir(&work_dir);
    config.signature_url = server.url("/signatures/latest.sig");
    let updater = DeltaUpdater::new(config).unwrap();

    updater
        .check()
        .await
        .expect("必须从显式配置的签名地址拉取并验签");
    assert!(
        server.was_requested("/signatures/latest.sig"),
        "自定义签名地址必须被实际请求"
    );
}

/// 双签清单 + `AllOf`：客户端必须接受双签、拒绝单签。
#[tokio::test]
async fn test_multi_key_all_of_is_enforced() {
    let dir = tempdir().unwrap();
    let exe = dir.path().join("app.exe");
    let work_dir = dir.path().join("work");
    std::fs::write(&exe, b"app").unwrap();
    std::fs::create_dir_all(&work_dir).unwrap();

    let (sk_a, pk_a) = signing_keypair();
    let (sk_b, pk_b) = signing_keypair();
    let server = MockServer::start(HashMap::new());
    let platform = tauri_delta_updater::current_platform();
    let (manifest_bytes, _) = build_manifest(
        &sk_a,
        &server.base_url,
        "0.2.0",
        platform,
        "patch-hash",
        "target-hash",
        None,
    );
    let dual = format!(
        "{}\n{}",
        sign(&sk_a, &manifest_bytes),
        sign(&sk_b, &manifest_bytes)
    );
    server.set("/latest.json", manifest_bytes.clone());
    server.set("/latest.json.sig", dual.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_a)
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&work_dir)
            .additional_public_keys(vec![pk_b])
            .trust_policy(TrustPolicy::AllOf),
    )
    .unwrap();
    assert!(
        updater.check().await.is_ok(),
        "新旧两把密钥都签名的清单必须通过 AllOf"
    );

    server.set(
        "/latest.json.sig",
        sign(&sk_a, &manifest_bytes).into_bytes(),
    );
    let err = updater.check().await.unwrap_err();
    assert!(
        matches!(err, DeltaError::SignatureInvalid(_)),
        "只有一把签名时 AllOf 必须拒绝，实际: {err:?}"
    );
}

/// 最低系统版本：低于清单要求时，在任何下载之前就被拒绝。
#[tokio::test]
async fn test_min_os_blocks_update_before_download() {
    let dir = tempdir().unwrap();
    let exe = dir.path().join("app.exe");
    let work_dir = dir.path().join("work");
    std::fs::write(&exe, b"app").unwrap();
    std::fs::create_dir_all(&work_dir).unwrap();

    let (sk, pk) = signing_keypair();
    let server = MockServer::start(HashMap::new());
    let platform = tauri_delta_updater::current_platform();
    let (manifest_bytes, _) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        "patch-hash",
        "target-hash",
        None,
    );
    // 夸张的最低版本：任何真实系统都应判为过低。
    let mut value: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    value["platforms"][platform]["min_os"] = "999999.0".into();
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    server.set("/latest.json", bytes.clone());
    server.set("/latest.json.sig", sign(&sk, &bytes).into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk)
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let check = updater.check().await.unwrap();
    assert!(
        !check.has_patch && check.full_download_url.is_none(),
        "系统不满足时必须隐藏补丁与全量地址"
    );
    assert!(
        matches!(
            check.fallback_reason,
            Some(tauri_delta_updater::FallbackReason::EnvironmentUnsupported(
                _
            ))
        ),
        "check() 必须说明环境不支持"
    );

    match updater.update().await.unwrap() {
        UpdateOutcome::UnsupportedOs { required, current } => {
            assert_eq!(required, "999999.0");
            assert!(!current.is_empty(), "必须回报本机版本");
        }
        other => panic!("系统版本过低应返回 UnsupportedOs，实际: {other:?}"),
    }
    assert!(
        !server.was_requested("/update.patch"),
        "系统版本不足时不得下载补丁"
    );
}

/// 端点切换策略：只有策略允许时才回退到备用清单，验签失败永远终止。
#[tokio::test]
async fn test_manifest_endpoint_falls_back_only_when_policy_allows() {
    let dir = tempdir().unwrap();
    let exe = dir.path().join("app.exe");
    std::fs::write(&exe, b"app").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();

    let server = MockServer::start(HashMap::new());
    let (sk, pk) = signing_keypair();
    let (manifest, signature) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        tauri_delta_updater::current_platform(),
        &"a".repeat(64),
        &"b".repeat(64),
        Some(1024),
    );
    server.set("/latest.json", manifest);
    server.set("/latest.json.sig", signature.into_bytes());

    let base = UpdateConfig::new(server.url("/missing.json"), pk)
        .current_version("0.1.0")
        .executable_path(&exe)
        .work_dir(&work)
        .fallback_endpoints(vec![server.url("/latest.json")]);

    let permissive =
        DeltaUpdater::new(base.clone().fallback_policy(FallbackPolicy::OnAnyFailure)).unwrap();
    assert!(
        permissive.check().await.is_ok(),
        "OnAnyFailure 时主端点 404 必须切换到备用端点"
    );

    let transport_only = DeltaUpdater::new(base).unwrap();
    let err = transport_only.check().await.unwrap_err();
    assert!(
        matches!(err, DeltaError::HttpStatus(404)),
        "默认策略不得在 404 时切换: {err:?}"
    );
}

/// `prepare_with_events`：只下载合并、不启动替换，事件流与安装路径一致。
///
/// 插件 `prepare` 命令依赖这条语义：前端拿到 `Ready` 后自己决定退出时机。
#[tokio::test]
async fn test_prepare_with_events_reports_progress_without_launch() {
    let (_dir, updater, _server, new_bytes) = delta_fixture().await;
    let token = CancelToken::new();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);

    let outcome = updater
        .prepare_with_events(&token, move |event| {
            sink.lock().unwrap().push(event);
        })
        .await
        .unwrap();

    match outcome {
        UpdateOutcome::Patched { staged } => {
            assert_eq!(std::fs::read(staged.staged_exe()).unwrap(), new_bytes);
            assert!(
                staged.script_path().exists(),
                "替换脚本已生成，但不得自动启动"
            );
        }
        other => panic!("prepare 应完成合并，实际得到: {other:?}"),
    }

    let events = events.lock().unwrap();
    assert!(
        matches!(events.first(), Some(UpdateEvent::Checking)),
        "事件流必须从 Checking 开始: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, UpdateEvent::Downloading { .. })),
        "必须报告下载进度: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(UpdateEvent::Ready { .. })),
        "最后一条事件必须是 Ready: {events:?}"
    );
}

/// 配置了整包清单、但当前平台不支持整包替换时，仍必须使用单文件补丁。
///
/// 多平台应用通常共享一份 `tauri.conf.json`（含 `bundle_endpoint`）；
/// 如果 Linux / 非 `.app` 的 macOS 因此直接回退全量，用户会无谓地多下几十 MB。
#[cfg(unix)]
#[tokio::test]
async fn test_bundle_unsupported_still_uses_single_file_patch() {
    let (_dir, updater, server, new_bytes) = delta_fixture_with_bundle().await;

    let check = updater.check().await.unwrap();
    assert!(check.has_patch, "整包不可用时单文件补丁仍应可用");
    assert!(
        check.fallback_reason.is_none(),
        "有单文件补丁时不应报告回退原因: {:?}",
        check.fallback_reason
    );

    match updater.update().await.unwrap() {
        UpdateOutcome::Patched { staged } => {
            assert_eq!(std::fs::read(staged.staged_exe()).unwrap(), new_bytes);
        }
        other => panic!("整包不可用时应走单文件增量，实际得到: {other:?}"),
    }
    assert!(
        !server.was_requested("/latest.bundle.json"),
        "平台不支持整包时不应为整包清单消耗流量"
    );
    assert!(
        !server.was_requested("/full.exe"),
        "单文件增量命中时不得请求全量安装包"
    );
}

/// 灰度 0%：更新必须停在 Deferred，且不发起任何补丁下载。
#[tokio::test]
async fn test_rollout_defers_update_before_download() {
    let (_dir, updater, server, _new_bytes) = delta_fixture_with_rollout(Some(0)).await;

    match updater.update().await.unwrap() {
        UpdateOutcome::Deferred {
            version,
            percentage,
        } => {
            assert_eq!(version, "0.2.0");
            assert_eq!(percentage, 0);
        }
        other => panic!("0% 灰度必须返回 Deferred，实际得到: {other:?}"),
    }
    assert!(
        !server.was_requested("/update.patch"),
        "未命中灰度时不得下载补丁"
    );

    let check = updater.check().await.unwrap();
    assert!(check.is_newer, "新版本确实存在");
    assert!(check.deferred_by_rollout, "check 必须如实报告灰度未命中");
    assert!(!check.has_patch, "灰度未命中时不应展示可下载补丁");
    assert!(
        check.full_download_url.is_none(),
        "灰度未命中时不得引导宿主绕过比例下载全量包"
    );
    assert_eq!(check.rollout_percentage, Some(0));
}

/// `min_version` 必须压过灰度比例：低于门槛的客户端即使未命中桶位也强制更新。
#[tokio::test]
async fn test_min_version_overrides_rollout() {
    let (_dir, updater, server, new_bytes) = delta_fixture_with_rollout_and_min(0, "0.1.1").await;

    match updater.update().await.unwrap() {
        UpdateOutcome::Patched { staged } => {
            assert_eq!(
                std::fs::read(staged.staged_exe()).unwrap(),
                new_bytes,
                "低于 min_version 时必须完成更新"
            );
        }
        other => panic!("min_version 必须强制更新，实际得到: {other:?}"),
    }
    assert!(
        server.was_requested("/update.patch"),
        "强制更新必须实际下载补丁"
    );
}

/// 端到端：会话按阶段发出事件，最后一条是结果。
#[tokio::test]
async fn test_session_emits_phase_events() {
    let (_dir, updater, _server, _new_bytes) = delta_fixture().await;

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);

    let result = updater
        .session()
        .run(move |event| sink.lock().unwrap().push(event))
        .await
        .unwrap();

    assert!(matches!(result, UpdateResult::Updated { .. }));

    let events = events.lock().unwrap();
    assert!(
        matches!(events.first(), Some(UpdateEvent::Checking)),
        "第一条事件必须是 Checking，实际: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UpdateEvent::Downloading { .. })),
        "必须发出下载进度事件，实际: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, UpdateEvent::Applying)),
        "必须发出合并阶段事件，实际: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(UpdateEvent::Ready { version }) if version.as_str() == "0.2.0"),
        "最后一条事件必须是 Ready(0.2.0)，实际: {events:?}"
    );
}

/// 取消发生在开始之前：不发任何网络请求，也不计入失败次数。
#[tokio::test]
async fn test_session_cancel_before_start_skips_network() {
    let server = MockServer::start(HashMap::new());

    let dir = tempdir().unwrap();
    let work_dir = dir.path().join("work");
    let old_exe = dir.path().join("app.exe");
    std::fs::write(&old_exe, b"old").unwrap();

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), "PK")
            .current_version("0.1.0")
            .executable_path(&old_exe)
            .work_dir(&work_dir),
    )
    .unwrap();

    let session = updater.session();
    session.cancel();

    let mut events = Vec::new();
    let err = session.run(|event| events.push(event)).await.unwrap_err();

    assert!(matches!(err, DeltaError::Cancelled));
    assert_eq!(events, vec![UpdateEvent::Cancelled]);
    assert!(
        !server.was_requested("/latest.json"),
        "取消后不得再发起网络请求"
    );
    assert_eq!(
        tauri_delta_updater::get_failed_attempts(&work_dir).await,
        0,
        "用户取消不应计入安全模式"
    );
}

/// 取消发生在传输中：清掉半截补丁与暂存产物，且不计入失败次数。
#[tokio::test]
async fn test_session_cancel_during_transfer_cleans_up() {
    let (_dir, updater, _server, _new_bytes) = delta_fixture().await;
    let work_dir = updater.work_dir().to_path_buf();

    let session = updater.session();
    let token = session.cancel_token();
    // 第一个下载事件到达时立刻取消：覆盖"下载中途/下载刚结束"两种时序。
    let cancel_from_event = token.clone();

    let err = session
        .run(move |event| {
            if matches!(event, UpdateEvent::Downloading { .. }) {
                cancel_from_event.cancel();
            }
        })
        .await
        .unwrap_err();

    assert!(matches!(err, DeltaError::Cancelled));
    assert!(token.is_cancelled());
    assert_eq!(
        tauri_delta_updater::get_failed_attempts(&work_dir).await,
        0,
        "用户取消不应计入安全模式"
    );
    assert!(
        !work_dir
            .join(tauri_delta_updater::core::artifacts::PATCH_FILE)
            .exists(),
        "取消后不得留下半截补丁"
    );
    assert!(
        !work_dir
            .join(tauri_delta_updater::core::artifacts::staged_file())
            .exists(),
        "取消后不得留下暂存产物"
    );
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

/// prepare 之后清单升到更高版本、但新补丁下载失败时，旧 pending 必须保留：
/// 不能出现"新版本没下到、旧产物也被清理掉了"的两头空。
#[test]
fn test_failed_update_keeps_prepared_pending() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(failed_update_keeps_prepared_pending());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn failed_update_keeps_prepared_pending() {
    use sha2::{Digest, Sha256};

    let sha256_hex = |bytes: &[u8]| {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    };

    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let old_exe = work_dir.join("app.exe");
    std::fs::write(&old_exe, b"old version").unwrap();

    let new_bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
    let new_exe = work_dir.join("app_new.exe");
    std::fs::write(&new_exe, &new_bytes).unwrap();
    let new_hash = sha256_hex(&new_bytes);

    let patch_path = dir.path().join("update.patch");
    tauri_delta_updater::create_patch(&old_exe, &new_exe, &patch_path)
        .await
        .unwrap();
    let patch_bytes = std::fs::read(&patch_path).unwrap();
    let patch_hash = sha256_hex(&patch_bytes);

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let server = MockServer::start(HashMap::from([("/update.patch".to_string(), patch_bytes)]));

    let (manifest_020, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.2.0",
        platform,
        &patch_hash,
        &new_hash,
        None,
    );
    server.set("/latest.json", manifest_020);
    server.set("/latest.json.sig", sig.into_bytes());

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
            assert!(staged.staged_exe().exists(), "prepare 应完成合并");
        }
        other => panic!("prepare 必须返回 Patched，实际 {other:?}"),
    }
    assert!(
        updater.pending_update().await.unwrap().is_some(),
        "prepare 应落盘待安装记录"
    );

    // 清单升到 0.3.0，但补丁下载持续 502：更新失败，旧 pending 不能被误删。
    let (manifest_030, sig) = build_manifest(
        &sk,
        &server.base_url,
        "0.3.0",
        platform,
        &patch_hash,
        &new_hash,
        None,
    );
    server.set("/latest.json", manifest_030);
    server.set("/latest.json.sig", sig.into_bytes());
    server.fail_next("/update.patch", 10);

    assert!(updater.update().await.is_err(), "补丁 502 必须失败");

    let pending = updater
        .pending_update()
        .await
        .unwrap()
        .expect("下载失败后旧 pending 必须保留");
    let staged = pending.staged_exe().expect("单文件 pending");
    assert!(staged.exists(), "已就绪产物不能被清理阶段删除");
    assert!(pending.script_path().exists(), "替换脚本不能被清理阶段删除");
}

/// 端到端：prepare() 合并补丁但不启动替换脚本。
///
/// 场景覆盖 prepare → UpToDate / Deferred 三条路径，调试构建的 async 状态机
/// 在 Windows 默认测试线程栈（2 MB）上会溢出；与 `generate` 一样给足栈空间。
#[test]
fn test_end_to_end_prepare_does_not_launch() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(prepare_does_not_launch());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn prepare_does_not_launch() {
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

            // 跨重启待安装：prepare 必须落盘记录，下次启动才能继续。
            let pending = updater
                .pending_update()
                .await
                .unwrap()
                .expect("prepare() 应当写入待安装记录");
            assert_eq!(pending.version(), "0.2.0");
            assert_eq!(pending.script_path(), staged.script_path());
            assert!(
                pending.staged_exe().is_some(),
                "单文件更新必须记录合并产物路径"
            );

            // 一次"已是最新"的更新调用不得销毁已就绪的待安装产物：
            // 产物文件名固定，清理后无法重建，等于让已完成的下载白费。
            let (same_manifest, same_sig) = build_manifest(
                &sk,
                &server.base_url,
                "0.1.0",
                platform,
                &patch_hash,
                &new_hash,
                None,
            );
            server.set("/latest.json", same_manifest);
            server.set("/latest.json.sig", same_sig.into_bytes());

            match updater.update().await.unwrap() {
                UpdateOutcome::UpToDate { .. } => {}
                other => panic!("同版本清单应当返回 UpToDate，实际: {other:?}"),
            }
            assert!(
                updater.pending_update().await.unwrap().is_some(),
                "UpToDate 之后待安装记录与产物必须仍然可用"
            );

            // 灰度未命中也属于"没有新东西可做"，同样不得清理待安装产物。
            let (deferred_manifest, deferred_sig) = build_manifest_with_rollout(
                &sk,
                &server.base_url,
                "0.2.0",
                platform,
                &patch_hash,
                &new_hash,
                None,
                0,
                None,
            );
            server.set("/latest.json", deferred_manifest);
            server.set("/latest.json.sig", deferred_sig.into_bytes());
            match updater.update().await.unwrap() {
                UpdateOutcome::Deferred { .. } => {}
                other => panic!("灰度 0% 应当返回 Deferred，实际: {other:?}"),
            }
            assert!(
                updater.pending_update().await.unwrap().is_some(),
                "Deferred 之后待安装记录与产物必须仍然可用"
            );
        }
        other => panic!("prepare() 应当完成补丁合并，实际得到: {other:?}"),
    }
}

/// 端到端：没有直达补丁时，逐跳应用 0.1 → 0.2 → 0.3。
///
/// 链上的每一跳都必须校验中间产物哈希；最终产物必须与 0.3 的字节完全一致。
#[tokio::test]
async fn test_end_to_end_multi_hop_chain() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();

    let v1: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let v2: Vec<u8> = (0..4096u32).map(|i| ((i / 2) % 241) as u8).collect();
    let v3: Vec<u8> = (0..4096u32).map(|i| ((i / 3) % 233) as u8).collect();
    let exe = work_dir.join("app.exe");
    let v2_path = dir.path().join("v2.exe");
    let v3_path = dir.path().join("v3.exe");
    std::fs::write(&exe, &v1).unwrap();
    std::fs::write(&v2_path, &v2).unwrap();
    std::fs::write(&v3_path, &v3).unwrap();

    let patch12 = dir.path().join("0.1_to_0.2.patch");
    let patch23 = dir.path().join("0.2_to_0.3.patch");
    tauri_delta_updater::create_patch(&exe, &v2_path, &patch12)
        .await
        .unwrap();
    tauri_delta_updater::create_patch(&v2_path, &v3_path, &patch23)
        .await
        .unwrap();

    let patch12_hash = sha256(&std::fs::read(&patch12).unwrap());
    let patch23_hash = sha256(&std::fs::read(&patch23).unwrap());
    let server = MockServer::start(HashMap::from([
        (
            "/0.1_to_0.2.patch".to_string(),
            std::fs::read(&patch12).unwrap(),
        ),
        (
            "/0.2_to_0.3.patch".to_string(),
            std::fs::read(&patch23).unwrap(),
        ),
    ]));

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    // 只有两跳，刻意不提供 0.1 → 0.3 的直达补丁。
    let manifest = serde_json::json!({
        "version": "0.3.0",
        "notes": "chain",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: {
                "signature": "dummy",
                "url": format!("{}/full.exe", server.base_url),
                "size": 100_000u64
            }
        },
        "patches": {
            platform: [
                {
                    "from_version": "0.1.0",
                    "to_version": "0.2.0",
                    "patch_url": server.url("/0.1_to_0.2.patch"),
                    "patch_hash": patch12_hash,
                    "target_hash": sha256(&v2),
                    "patch_size": std::fs::metadata(&patch12).unwrap().len()
                },
                {
                    "from_version": "0.2.0",
                    "to_version": "0.3.0",
                    "patch_url": server.url("/0.2_to_0.3.patch"),
                    "patch_hash": patch23_hash,
                    "target_hash": sha256(&v3),
                    "patch_size": std::fs::metadata(&patch23).unwrap().len()
                }
            ]
        }
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    server.set("/latest.json", manifest_bytes.clone());
    server.set("/latest.json.sig", sign(&sk, &manifest_bytes).into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64.clone())
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&work_dir)
            .auto_launch(false),
    )
    .unwrap();

    match updater.prepare().await.unwrap() {
        UpdateOutcome::Patched { staged } => {
            assert_eq!(
                std::fs::read(staged.staged_exe()).unwrap(),
                v3,
                "链式合并的最终产物必须与目标版本一致"
            );
        }
        other => panic!("两跳链应当完成合并，实际得到: {other:?}"),
    }
    assert!(
        server.was_requested("/0.1_to_0.2.patch") && server.was_requested("/0.2_to_0.3.patch"),
        "链式更新必须请求两跳补丁"
    );

    // 反向：第二跳的 target_hash 不符时，错误必须指出失败跳数，
    // 且错误码/安全分类保持底层哈希校验的语义。
    let mut bad = manifest.clone();
    bad["patches"][platform][1]["target_hash"] = "0".repeat(64).into();
    let bad_bytes = serde_json::to_vec_pretty(&bad).unwrap();
    server.set("/latest.json", bad_bytes.clone());
    server.set("/latest.json.sig", sign(&sk, &bad_bytes).into_bytes());

    let bad_work = dir.path().join("work_bad");
    std::fs::create_dir_all(&bad_work).unwrap();
    let bad_updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64.clone())
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&bad_work),
    )
    .unwrap();
    let err = bad_updater.prepare().await.unwrap_err();
    match &err {
        DeltaError::ChainHopFailed { hop, total, .. } => {
            assert_eq!((*hop, *total), (2, 2), "必须指出第 2/2 跳失败");
        }
        other => panic!("链式失败必须包装为 ChainHopFailed，实际: {other:?}"),
    }
    assert_eq!(err.code(), "hash_mismatch", "错误码必须保持底层语义");
    assert!(err.is_security_blocking(), "中间产物校验失败属于安全阻断");
    assert!(
        err.to_string().contains("第 2/2 跳"),
        "错误文案必须包含跳数: {err}"
    );
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

/// 必需的清单地址与公钥留空时，必须在构造阶段给出明确原因。
#[test]
fn test_new_rejects_empty_manifest_url_and_key() {
    let err = DeltaUpdater::new(UpdateConfig::new("", "PK").current_version("0.1.0")).unwrap_err();
    match err {
        tauri_delta_updater::DeltaError::Config(msg) => {
            assert!(msg.contains("manifest_url"), "错误信息必须指明字段: {msg}")
        }
        other => panic!("空的 manifest_url 应返回 Config 错误，实际: {other}"),
    }

    let err = DeltaUpdater::new(
        UpdateConfig::new("https://cdn.example.com/latest.json", "").current_version("0.1.0"),
    )
    .unwrap_err();
    match err {
        tauri_delta_updater::DeltaError::Config(msg) => {
            assert!(msg.contains("public_key"), "错误信息必须指明字段: {msg}")
        }
        other => panic!("空的 public_key 应返回 Config 错误，实际: {other}"),
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

/// 清单拉取的瞬时故障必须自动重试。
///
/// CDN 抖动返回一次 502 不该让用户看到"检查更新失败"；
/// 清单与补丁走同一套退避策略。
#[tokio::test]
async fn test_manifest_fetch_retries_transient_5xx() {
    let (_dir, updater, server, _new_bytes) = delta_fixture().await;
    server.fail_next("/latest.json", 1);

    let info = updater.check().await.expect("一次 502 之后应重试成功");
    assert!(info.is_newer, "重试后必须拿到正常结果");
    assert!(
        server.request_count("/latest.json") >= 2,
        "必须确实重试过，而不是服务端恰好没失败"
    );
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
