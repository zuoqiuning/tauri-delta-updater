//! 端到端测试共用的本地 HTTP 服务与签名夹具。
//!
//! 通用部分由 `tdx-testkit` 提供——与外部接入者使用同一份实现，
//! 避免"仓库内能测、接入方只能重造"。这里只保留本仓库特有的 `delta_fixture*`
//! 组合（构造旧 exe、补丁、清单并装配 `DeltaUpdater`）。

#![allow(dead_code)]
// 每个测试二进制只用到夹具的一部分；未用到的重导出不应产生告警。
#![allow(unused_imports)]

pub use tdx_testkit::{
    build_manifest, build_manifest_with_rollout, sha256, sign, sign_bound, signing_keypair,
    write_file, MockServer,
};

use std::collections::HashMap;
use tauri_delta_updater::{DeltaUpdater, UpdateConfig};
use tempfile::tempdir;

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
            "0.1.0",
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
            "0.1.0",
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
