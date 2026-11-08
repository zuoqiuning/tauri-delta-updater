//! 端到端：整包增量、macOS 目录切换、修复、回退与签名边界。

mod common;
use common::*;

use std::collections::HashMap;

use tauri_delta_updater::{
    create_bundle_patch, BundleManifest, DeltaUpdater, FallbackReason, UpdateConfig, UpdateOutcome,
};
use tempfile::tempdir;

/// 端到端：配置整包清单后，客户端下载整包补丁、解压暂存并生成多文件替换脚本。
///
/// 非 Windows 平台不执行多文件替换：配置了整包时应回退全量，
/// 而不是只替换主程序、留下与旧版本不匹配的 sidecar/resources。
#[tokio::test]
async fn test_end_to_end_bundle_update() {
    let dir = tempdir().unwrap();
    let install = dir.path().join("install");
    let new_app = dir.path().join("new_app");
    let work_dir = dir.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();

    write_file(&install, "app.exe", b"old main");
    write_file(&install, "resources/app.js", b"old resource");
    write_file(&install, "sidecars/helper.exe", b"same helper");

    write_file(&new_app, "app.exe", b"new main, v2");
    write_file(&new_app, "resources/app.js", b"new resource, v2");
    write_file(&new_app, "resources/extra.txt", b"added file");
    write_file(&new_app, "sidecars/helper.exe", b"same helper");

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let server = MockServer::start(HashMap::new());

    let patch_path = dir.path().join("bundle.patch");
    let bundle = create_bundle_patch(
        &install,
        &new_app,
        "0.1.0",
        "0.2.0",
        server.url("/bundle.patch"),
        &patch_path,
        dir.path(),
    )
    .await
    .unwrap();
    server.set("/bundle.patch", std::fs::read(&patch_path).unwrap());

    let bundle_manifest = BundleManifest {
        version: "0.2.0".into(),
        platform: platform.to_string(),
        bundles: vec![bundle.clone()],
        extra: Default::default(),
    };
    let bytes = serde_json::to_vec_pretty(&bundle_manifest).unwrap();
    let sig = sign_bound(&sk, &bytes, "tdu:v=0.2.0");
    server.set("/latest.bundle.json", bytes);
    server.set("/latest.bundle.json.sig", sig.into_bytes());

    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "bundle",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: { "signature": "dummy", "url": server.url("/full.exe") }
        },
        "patches": {}
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &bytes);
    server.set("/latest.json", bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(install.join("app.exe"))
            .work_dir(&work_dir)
            .bundle_manifest_url(server.url("/latest.bundle.json"))
            .auto_launch(false),
    )
    .unwrap();

    let supports =
        tauri_delta_updater::PlatformUpdater::bundle_supported_for(&install.join("app.exe"));
    let info = updater.check().await.unwrap();
    assert_eq!(info.version, "0.2.0");
    if supports {
        assert!(info.has_patch, "整包补丁应被识别为可用");
        assert_eq!(info.patch_size, bundle.patch_size);
        assert!(info.fallback_reason.is_none());
    } else {
        assert!(!info.has_patch);
        assert!(matches!(
            info.fallback_reason,
            Some(FallbackReason::EnvironmentUnsupported(_))
        ));
    }

    match updater.prepare().await.unwrap() {
        UpdateOutcome::Bundled { staged } => {
            assert_eq!(staged.manifest().version, "0.2.0");
            assert_eq!(staged.files().len(), 4, "新版本清单应包含全部文件");
            assert_eq!(
                std::fs::read(staged.staging_dir().join("app.exe")).unwrap(),
                b"new main, v2"
            );
            assert_eq!(
                std::fs::read(staged.staging_dir().join("resources/app.js")).unwrap(),
                b"new resource, v2"
            );
            assert_eq!(
                std::fs::read(staged.staging_dir().join("resources/extra.txt")).unwrap(),
                b"added file"
            );
            assert_eq!(
                std::fs::read(staged.staging_dir().join("sidecars/helper.exe")).unwrap(),
                b"same helper"
            );
            assert!(staged.script_path().exists(), "整包替换脚本必须生成");
            assert!(
                staged.script_path().ends_with("tdx_bundle.ps1"),
                "脚本文件名必须可辨认，避免与单文件脚本混淆"
            );
        }
        other if !supports => match other {
            UpdateOutcome::FallbackToFullDownload {
                reason: FallbackReason::EnvironmentUnsupported(_),
                ..
            } => return,
            other => panic!("非 Windows 平台配置整包时应回退全量，实际: {other:?}"),
        },
        other => panic!("配置整包清单时应走整包增量，实际: {other:?}"),
    }

    // 整包命中时不得请求全量安装包
    assert!(
        !server.was_requested("/full.exe"),
        "整包命中时不得请求全量安装包"
    );
}

/// Windows：检测到上次中断的整包替换时，`update()` 不做网络请求，
/// 直接生成修复脚本（重试前必须先还原安装目录）。
#[cfg(target_os = "windows")]
#[tokio::test]
async fn test_update_prefers_repair_without_network() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let exe = work_dir.join("app.exe");
    std::fs::write(&exe, b"partial new version").unwrap();

    let backup = work_dir.join(tauri_delta_updater::core::artifacts::BUNDLE_BACKUP_DIR);
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("app.exe"), b"old version").unwrap();

    // 清单地址指向不可达端口：如果修复分支之前发起网络请求，测试会失败。
    let updater = DeltaUpdater::new(
        UpdateConfig::new("http://127.0.0.1:1/latest.json", "PK")
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&work_dir)
            .auto_launch(false),
    )
    .unwrap();

    assert!(
        updater.diagnose().await.needs_repair,
        "诊断必须提示需要修复"
    );

    match updater.update().await.unwrap() {
        UpdateOutcome::Repair { staged, reason } => {
            assert!(staged.script_path().exists(), "修复脚本必须生成");
            assert_eq!(reason, "repaired_partial_update");
        }
        other => panic!("存在未完成替换时必须先修复，实际: {other:?}"),
    }

    // auto_launch=false：脚本未启动，备份仍保留，等待宿主择机执行。
    assert!(backup.is_dir(), "未启动修复脚本前备份必须保留");
    assert!(updater.diagnose().await.needs_repair);
}

/// Windows：成功标志已写、备份尚未删除时崩溃，下一次启动不得据此回滚健康版本。
#[cfg(target_os = "windows")]
#[tokio::test]
async fn test_committed_startup_is_not_treated_as_partial_update() {
    use tauri_delta_updater::core::artifacts;

    let dir = tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let exe = work_dir.join("app.exe");
    std::fs::write(&exe, b"healthy new version").unwrap();

    let backup = work_dir.join(artifacts::BUNDLE_BACKUP_DIR);
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("app.exe"), b"old version").unwrap();
    std::fs::write(
        work_dir.join(artifacts::BUNDLE_JOURNAL_FILE),
        "add new.txt\n",
    )
    .unwrap();

    // 模拟“新版本已确认启动，但脚本还没来得及删备份就被终止”。
    tauri_delta_updater::core::startup::write(
        &work_dir,
        tauri_delta_updater::StartupState::Committed,
    )
    .unwrap();

    assert!(
        !tauri_delta_updater::PlatformUpdater::bundle_repair_needed(&exe, &work_dir),
        "已 committed 的替换不得再次被判为未完成"
    );

    let updater = DeltaUpdater::new(
        UpdateConfig::new("http://127.0.0.1:1/latest.json", "PK")
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&work_dir)
            .auto_launch(false),
    )
    .unwrap();
    assert!(!updater.diagnose().await.needs_repair);
}

/// Windows：启动确认成功后必须清理整包备份，避免下一次启动把残留当作未完成替换。
#[cfg(target_os = "windows")]
#[test]
fn test_confirm_cleans_bundle_backup() {
    use tauri_delta_updater::core::artifacts;

    let dir = tempdir().unwrap();
    let work_dir = dir.path();
    let backup = work_dir.join(artifacts::BUNDLE_BACKUP_DIR);
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("app.exe"), b"old").unwrap();
    std::fs::write(work_dir.join(artifacts::BUNDLE_JOURNAL_FILE), "add x\n").unwrap();

    tauri_delta_updater::PlatformUpdater::confirm_startup_success_in(work_dir).unwrap();

    assert!(!backup.exists(), "确认成功后备份目录必须被清理");
    assert!(
        !work_dir.join(artifacts::BUNDLE_JOURNAL_FILE).exists(),
        "确认成功后 journal 必须被清理"
    );
    assert_eq!(
        tauri_delta_updater::core::startup::read(work_dir),
        Some(tauri_delta_updater::StartupState::Committed)
    );
}

/// macOS：`.app` 内主程序走整包时，安装根目录是整个 `.app`，
/// 暂存目录以 `Contents/...` 为相对路径，脚本是目录切换版本。
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_end_to_end_macos_app_bundle() {
    let dir = tempdir().unwrap();
    let old_app = dir.path().join("old/MyApp.app");
    let new_app = dir.path().join("new/MyApp.app");
    let work_dir = dir.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();

    write_file(&old_app, "Contents/MacOS/MyApp", b"old main");
    write_file(&old_app, "Contents/Resources/a.txt", b"old resource");
    write_file(&new_app, "Contents/MacOS/MyApp", b"new main, v2");
    write_file(&new_app, "Contents/Resources/a.txt", b"new resource, v2");

    let (sk, pk_b64) = signing_keypair();
    let platform = tauri_delta_updater::current_platform();
    let server = MockServer::start(HashMap::new());

    let patch_path = dir.path().join("bundle.patch");
    let bundle = create_bundle_patch(
        &old_app,
        &new_app,
        "0.1.0",
        "0.2.0",
        server.url("/bundle.patch"),
        &patch_path,
        dir.path(),
    )
    .await
    .unwrap();
    server.set("/bundle.patch", std::fs::read(&patch_path).unwrap());

    let bundle_manifest = BundleManifest {
        version: "0.2.0".into(),
        platform: platform.to_string(),
        bundles: vec![bundle.clone()],
        extra: Default::default(),
    };
    let bytes = serde_json::to_vec_pretty(&bundle_manifest).unwrap();
    let sig = sign_bound(&sk, &bytes, "tdu:v=0.2.0");
    server.set("/latest.bundle.json", bytes);
    server.set("/latest.bundle.json.sig", sig.into_bytes());

    let manifest = serde_json::json!({
        "version": "0.2.0",
        "notes": "macos bundle",
        "pub_date": "2026-01-01T00:00:00Z",
        "platforms": {
            platform: { "signature": "dummy", "url": server.url("/full.exe") }
        },
        "patches": {}
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let sig = sign(&sk, &bytes);
    server.set("/latest.json", bytes);
    server.set("/latest.json.sig", sig.into_bytes());

    let updater = DeltaUpdater::new(
        UpdateConfig::new(server.url("/latest.json"), pk_b64)
            .current_version("0.1.0")
            .executable_path(old_app.join("Contents/MacOS/MyApp"))
            .work_dir(&work_dir)
            .bundle_manifest_url(server.url("/latest.bundle.json"))
            .auto_launch(false),
    )
    .unwrap();

    // `.app` 内的主程序在单文件路径上会回退，但整包可用时应当判定为可增量。
    let info = updater.check().await.unwrap();
    assert!(info.has_patch, "macOS .app 的整包补丁应可用");
    assert!(info.fallback_reason.is_none());

    match updater.prepare().await.unwrap() {
        UpdateOutcome::Bundled { staged } => {
            assert_eq!(
                std::fs::read(staged.staging_dir().join("Contents/Resources/a.txt")).unwrap(),
                b"new resource, v2"
            );
            assert_eq!(
                std::fs::read(staged.staging_dir().join("Contents/MacOS/MyApp")).unwrap(),
                b"new main, v2"
            );
            assert!(
                staged.script_path().ends_with("tdx_bundle.sh"),
                "macOS 必须使用目录切换脚本，而不是 Windows 逐文件脚本"
            );
        }
        other => panic!("macOS .app 应走整包增量，实际: {other:?}"),
    }
    assert!(!server.was_requested("/full.exe"));
}

fn write_file(root: &std::path::Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
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

    // check() 与 update() 必须对"为什么没走增量"给出同一个可机读原因
    let info = updater.check().await.unwrap();
    assert!(info.is_newer);
    assert!(!info.has_patch);
    assert!(
        matches!(
            info.fallback_reason,
            Some(tauri_delta_updater::FallbackReason::NoMatchingPatch { .. })
        ),
        "无补丁时 check() 必须带 NoMatchingPatch，实际: {:?}",
        info.fallback_reason
    );
    let full = info.full_download().expect("清单应包含当前平台条目");
    assert_eq!(full.url, "https://cdn.example.com/full.exe");
    assert_eq!(full.version, "0.2.0");

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
