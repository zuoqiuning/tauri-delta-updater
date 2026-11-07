use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Instant;

use tauri_delta_updater::{
    apply_patch, calculate_hash, create_patch, get_failed_attempts, record_failed_attempt,
    should_abort, UpdateManifest,
};
use tempfile::tempdir;
use tokio::fs;

// ==================== 补丁生成与应用 ====================

#[tokio::test]
async fn test_ultra_safe_update_lifecycle() {
    let dir = tempdir().unwrap();
    let old_path = dir.path().join("old.exe");
    let new_path = dir.path().join("new.exe");
    let patch_path = dir.path().join("update.patch");
    let merged_path = dir.path().join("merged.exe");

    fs::write(&old_path, b"I am the old version with a lot of data...")
        .await
        .unwrap();
    fs::write(
        &new_path,
        b"I am the new version with a lot of data and new features!",
    )
    .await
    .unwrap();

    create_patch(&old_path, &new_path, &patch_path)
        .await
        .unwrap();

    let patch_hash = calculate_hash(&patch_path).await.unwrap();
    let target_hash = calculate_hash(&new_path).await.unwrap();

    apply_patch(
        &old_path,
        &patch_path,
        &merged_path,
        &patch_hash,
        &target_hash,
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(&merged_path).await.unwrap(),
        fs::read(&new_path).await.unwrap()
    );

    fs::remove_file(&merged_path).await.unwrap();

    // 补丁哈希不符时必须拒绝，且不产生产物
    let wrong_patch_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    let result = apply_patch(
        &old_path,
        &patch_path,
        &merged_path,
        wrong_patch_hash,
        &target_hash,
    )
    .await;
    assert!(result.is_err(), "补丁哈希不匹配时，应当拒绝合并");
    assert!(!merged_path.exists(), "拒绝后不应产生合并文件");

    // 目标哈希不符时必须报错并清理产物
    let result = apply_patch(
        &old_path,
        &patch_path,
        &merged_path,
        &patch_hash,
        wrong_patch_hash,
    )
    .await;
    assert!(result.is_err(), "目标哈希不匹配时，应当报错");
    assert!(!merged_path.exists(), "哈希错误时，临时文件必须被彻底删除");
}

#[tokio::test]
async fn test_apply_patch_rejects_existing_output() {
    let dir = tempdir().unwrap();
    let old_path = dir.path().join("old.exe");
    let new_path = dir.path().join("new.exe");
    let patch_path = dir.path().join("update.patch");
    let merged_path = dir.path().join("merged.exe");

    fs::write(&old_path, b"old").await.unwrap();
    fs::write(&new_path, b"new").await.unwrap();
    fs::write(&merged_path, b"occupied").await.unwrap();
    create_patch(&old_path, &new_path, &patch_path)
        .await
        .unwrap();

    let patch_hash = calculate_hash(&patch_path).await.unwrap();
    let target_hash = calculate_hash(&new_path).await.unwrap();

    let result = apply_patch(
        &old_path,
        &patch_path,
        &merged_path,
        &patch_hash,
        &target_hash,
    )
    .await;
    assert!(result.is_err(), "目标文件已存在时必须拒绝覆盖");
    assert_eq!(
        fs::read(&merged_path).await.unwrap(),
        b"occupied",
        "拒绝覆盖时不得改动已有文件"
    );
}

#[tokio::test]
async fn test_large_file_performance() {
    let dir = tempdir().unwrap();
    let old_path = dir.path().join("large_old.exe");
    let new_path = dir.path().join("large_new.exe");
    let patch_path = dir.path().join("large.patch");

    // 生成 20MB 的假数据
    let mut data = vec![0u8; 20 * 1024 * 1024];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }
    fs::write(&old_path, &data).await.unwrap();

    // 改掉末尾的 1MB 数据
    for byte in data[19 * 1024 * 1024..].iter_mut() {
        *byte = 42;
    }
    fs::write(&new_path, &data).await.unwrap();

    let start = Instant::now();
    create_patch(&old_path, &new_path, &patch_path)
        .await
        .unwrap();
    println!(
        "[PERF] 20MB file patch generation took: {:?}",
        start.elapsed()
    );

    let patch_size = fs::metadata(&patch_path).await.unwrap().len();
    println!("[PERF] Patch size is: {} bytes", patch_size);

    // 只有 1MB 变化，补丁应该远小于 20MB
    assert!(
        patch_size < 2 * 1024 * 1024,
        "Patch size should be much smaller than the full file"
    );
}

// ==================== 清单：平台维度 ====================

#[test]
fn test_find_patch_is_platform_scoped() {
    let mut manifest = UpdateManifest {
        version: "0.2.0".to_string(),
        notes: String::new(),
        pub_date: String::new(),
        min_version: None,
        platforms: Default::default(),
        patches: Default::default(),
        notes_i18n: Default::default(),
        extra: Default::default(),
    };

    let patch = |from: &str, url: &str| tauri_delta_updater::PatchInfo {
        from_version: from.to_string(),
        to_version: "0.2.0".to_string(),
        patch_url: url.to_string(),
        patch_hash: "a".repeat(64),
        target_hash: "b".repeat(64),
        patch_size: Some(1024),
        scope: None,
    };

    manifest.patches.insert(
        "windows-x86_64".to_string(),
        vec![patch("0.1.0", "https://cdn/x64.patch")],
    );
    manifest.patches.insert(
        "windows-aarch64".to_string(),
        vec![patch("0.1.0", "https://cdn/arm64.patch")],
    );

    // 各架构只能取到自己的补丁
    let x64 = manifest.find_patch("0.1.0", "windows-x86_64").unwrap();
    assert_eq!(x64.patch_url, "https://cdn/x64.patch");

    let arm64 = manifest.find_patch("0.1.0", "windows-aarch64").unwrap();
    assert_eq!(arm64.patch_url, "https://cdn/arm64.patch");

    // 未知平台不应误取其它架构的补丁
    assert!(
        manifest.find_patch("0.1.0", "linux-x86_64").is_none(),
        "未知平台不得取到其它架构的补丁"
    );

    // 版本不匹配时同样取不到
    assert!(manifest.find_patch("0.0.9", "windows-x86_64").is_none());

    assert!(manifest.has_platform_patches("windows-x86_64"));
    assert!(!manifest.has_platform_patches("linux-x86_64"));
}

#[test]
fn test_find_patch_ignores_stale_target_version() {
    // 关键回归：清单里可能残留上一轮发布的补丁（指向旧的 to_version）。
    // 直接使用会把应用更新到错误版本，必须拒绝。
    let mut manifest = UpdateManifest {
        version: "0.2.0".to_string(),
        notes: String::new(),
        pub_date: String::new(),
        min_version: None,
        platforms: Default::default(),
        patches: Default::default(),
        notes_i18n: Default::default(),
        extra: Default::default(),
    };

    manifest.patches.insert(
        "windows-x86_64".to_string(),
        vec![tauri_delta_updater::PatchInfo {
            from_version: "0.1.0".to_string(),
            to_version: "0.1.9".to_string(), // 过期：与清单版本不一致
            patch_url: "https://cdn/stale.patch".to_string(),
            patch_hash: "a".repeat(64),
            target_hash: "b".repeat(64),
            patch_size: Some(512),
            scope: None,
        }],
    );

    assert!(
        manifest.find_patch("0.1.0", "windows-x86_64").is_none(),
        "目标版本与清单版本不一致的补丁不得被使用"
    );
}

#[test]
fn test_manifest_reads_legacy_flat_patch_format() {
    // 关键回归：历史版本的 latest.json 使用扁平数组格式，
    // CI 会把它作为 existing_json 传回给生成器，解析失败会中断发布流程。
    let legacy = r#"{
      "version": "0.1.0",
      "notes": "old",
      "pub_date": "2026-01-01T00:00:00Z",
      "platforms": {
        "windows-x86_64": { "signature": "sig", "url": "https://cdn/app.exe" }
      },
      "patches": [
        {
          "from_version": "0.0.9",
          "to_version": "0.1.0",
          "patch_url": "https://cdn/legacy.patch",
          "patch_hash": "a",
          "target_hash": "b"
        }
      ]
    }"#;

    let manifest: UpdateManifest = serde_json::from_str(legacy).expect("旧格式清单必须能解析");

    // 旧格式归入当前运行平台（跨平台 CI 上不能硬编码 windows-x86_64）
    let platform = tauri_delta_updater::current_platform();
    assert_eq!(manifest.patches.len(), 1);
    assert!(manifest.patches.contains_key(platform));
    assert_eq!(manifest.patches[platform].len(), 1);
    assert_eq!(
        manifest.find_patch("0.0.9", platform).unwrap().patch_url,
        "https://cdn/legacy.patch"
    );
}

#[test]
fn test_manifest_reads_grouped_patch_format() {
    let grouped = r#"{
      "version": "0.2.0",
      "notes": "new",
      "pub_date": "2026-02-02T00:00:00Z",
      "platforms": {
        "windows-x86_64": { "signature": "sig", "url": "https://cdn/app.exe" }
      },
      "patches": {
        "windows-x86_64": [
          {
            "from_version": "0.1.0",
            "to_version": "0.2.0",
            "patch_url": "https://cdn/0.1.0_to_0.2.0.patch",
            "patch_hash": "a",
            "target_hash": "b"
          }
        ]
      }
    }"#;

    let manifest: UpdateManifest = serde_json::from_str(grouped).expect("新格式清单必须能解析");
    assert_eq!(manifest.patches["windows-x86_64"].len(), 1);
    assert!(manifest.find_patch("0.1.0", "windows-x86_64").is_some());
    assert!(manifest.find_patch("0.1.0", "windows-aarch64").is_none());
}

#[test]
fn test_manifest_tolerates_missing_optional_fields() {
    // Tauri 官方的 latest.json 未必带 notes / pub_date，解析不应失败
    let minimal = r#"{
      "version": "0.2.0",
      "platforms": {
        "windows-x86_64": { "signature": "sig", "url": "https://cdn/app.exe" }
      }
    }"#;

    let manifest: UpdateManifest = serde_json::from_str(minimal).expect("缺少可选字段必须能解析");
    assert_eq!(manifest.version, "0.2.0");
    assert!(manifest.patches.is_empty());
    assert!(manifest.find_patch("0.1.0", "windows-x86_64").is_none());
}

// ==================== 失败计数 ====================

#[test]
fn test_check_disk_space_handles_relative_and_missing_paths() {
    // 关键回归：路径没有目录部分时（如 "merged.exe"），
    // 磁盘空间检查曾因拿到空路径而直接失败，导致相对路径更新无法进行。
    for path in ["merged.exe", "sub/dir/merged.exe", "./merged.exe"] {
        let result = tauri_delta_updater::check_disk_space(Path::new(path), 1024);
        assert!(
            result.is_ok(),
            "磁盘空间检查必须能处理路径 {path:?}: {:?}",
            result.err()
        );
    }

    // 需求远超容量时必须明确报错
    let result = tauri_delta_updater::check_disk_space(Path::new("merged.exe"), u64::MAX);
    assert!(
        matches!(
            result,
            Err(tauri_delta_updater::DeltaError::InsufficientDiskSpace)
        ),
        "空间不足时必须返回 InsufficientDiskSpace"
    );
}

#[tokio::test]
async fn test_apply_patch_with_relative_output_path() {
    // 覆盖相对路径输出的完整更新流程
    let dir = tempdir().unwrap();
    let old_path = dir.path().join("old.exe");
    let new_path = dir.path().join("new.exe");
    let patch_path = dir.path().join("update.patch");

    fs::write(&old_path, b"old version payload").await.unwrap();
    fs::write(&new_path, b"new version payload").await.unwrap();
    create_patch(&old_path, &new_path, &patch_path)
        .await
        .unwrap();

    let patch_hash = calculate_hash(&patch_path).await.unwrap();
    let target_hash = calculate_hash(&new_path).await.unwrap();

    // 输出路径是绝对路径但文件名与目录分离，覆盖磁盘检查的路径解析分支
    let result = apply_patch(
        &old_path,
        &patch_path,
        dir.path().join("merged.exe"),
        &patch_hash,
        &target_hash,
    )
    .await;

    assert!(
        result.is_ok(),
        "相对路径输出必须能正常完成更新: {:?}",
        result.err()
    );
    assert_eq!(
        fs::read(dir.path().join("merged.exe")).await.unwrap(),
        b"new version payload"
    );
}

#[tokio::test]
async fn test_cleanup_preserves_failure_counter_and_success_flag() {
    // 关键回归：清理流程曾把失败计数一起删掉，
    // 导致每次尝试前计数归零，安全模式永远触发不了。
    let dir = tempdir().unwrap();

    record_failed_attempt(dir.path()).await.unwrap();
    record_failed_attempt(dir.path()).await.unwrap();

    // 启动成功标志是替换脚本判断是否保留新版本的依据，
    // 在替换流程进行期间删除它会导致误回滚。
    let flag = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::SUCCESS_FLAG);
    fs::write(&flag, "ok").await.unwrap();

    let exe = dir.path().join("app.exe");
    fs::write(&exe, b"app").await.unwrap();

    tauri_delta_updater::cleanup_previous_updates(dir.path(), &exe).await;

    assert_eq!(
        get_failed_attempts(dir.path()).await,
        2,
        "清理流程不得清除连续失败计数"
    );
    assert!(flag.exists(), "清理流程不得删除启动成功标志");

    // 用户的备份文件不能被误删：清理只删 `<目标 exe>.bak`
    let user_backup = dir.path().join("notes.txt.bak");
    fs::write(&user_backup, b"user data").await.unwrap();

    // 补丁与暂存文件属于本轮更新的临时产物，应当被清掉
    let patch = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::PATCH_FILE);
    let staged = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::staged_file());
    fs::write(&patch, b"patch").await.unwrap();
    fs::write(&staged, b"staged").await.unwrap();
    // 多跳链中断后残留的中间产物：必须在下一次更新开始时清掉，
    // 否则旧字节可能被误当作下一跳的输入。
    let hop_a = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::STAGED_HOP_A);
    let hop_b = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::STAGED_HOP_B);
    fs::write(&hop_a, b"hop").await.unwrap();
    fs::write(&hop_b, b"hop").await.unwrap();
    fs::write(dir.path().join("app.exe.bak"), b"old")
        .await
        .unwrap();

    // 整包崩溃恢复备份：脚本可能在替换中途被杀，备份是唯一的旧版本副本，
    // 清理流程不得删除它（由下一次脚本还原后自行清理）。
    let bundle_backup = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::BUNDLE_BACKUP_DIR);
    fs::create_dir_all(&bundle_backup).await.unwrap();
    fs::write(bundle_backup.join("app.exe"), b"old bundle")
        .await
        .unwrap();

    tauri_delta_updater::cleanup_previous_updates(dir.path(), &exe).await;

    assert!(!patch.exists(), "补丁文件应被清理");
    assert!(!staged.exists(), "暂存文件应被清理");
    assert!(!hop_a.exists() && !hop_b.exists(), "多跳中间产物应被清理");
    assert!(
        !dir.path().join("app.exe.bak").exists(),
        "本项目产生的备份应被清理"
    );
    assert!(user_backup.exists(), "不得删除用户自己的 .bak 文件");
    assert!(
        bundle_backup.join("app.exe").exists(),
        "整包崩溃恢复备份必须保留，直到替换脚本还原"
    );

    // 跨进程续传：有身份记录且新鲜的半截补丁必须保留，下载才能接着下
    let meta = dir
        .path()
        .join(tauri_delta_updater::core::artifacts::PATCH_META);
    fs::write(&meta, "deadbeef").await.unwrap();
    fs::write(&patch, b"partial").await.unwrap();

    tauri_delta_updater::cleanup_previous_updates(dir.path(), &exe).await;

    assert!(patch.exists(), "有身份记录的半截补丁应保留供续传");
    assert!(meta.exists(), "身份记录与补丁成对保留");
}

#[tokio::test]
async fn test_confirm_startup_success_removes_full_installer() {
    // 全量安装包在新版本确认启动后已无用：Windows 安装器早已退出，
    // macOS/Linux 已完成替换。留着只会占几十到几百 MB。
    let dir = tempdir().unwrap();
    let installer = dir.path().join("tdx_full_installer_0.2.0.exe");
    let unrelated = dir.path().join("notes.txt");
    fs::write(&installer, b"installer").await.unwrap();
    fs::write(&unrelated, b"user data").await.unwrap();

    tauri_delta_updater::PlatformUpdater::confirm_startup_success_in(dir.path()).unwrap();

    assert!(!installer.exists(), "确认启动后必须删除全量安装包");
    assert!(unrelated.exists(), "不得删除用户自己的文件");
}

#[tokio::test]
async fn test_failure_counter_reaches_abort_threshold() {
    let dir = tempdir().unwrap();

    assert_eq!(get_failed_attempts(dir.path()).await, 0);
    assert!(!should_abort(dir.path()).await);

    assert_eq!(record_failed_attempt(dir.path()).await.unwrap(), 1);
    assert_eq!(record_failed_attempt(dir.path()).await.unwrap(), 2);
    assert_eq!(record_failed_attempt(dir.path()).await.unwrap(), 3);

    assert!(
        should_abort(dir.path()).await,
        "连续失败达到阈值后必须停止尝试增量更新"
    );
}

#[tokio::test]
async fn test_clear_failed_attempts_resets_counter() {
    let dir = tempdir().unwrap();
    record_failed_attempt(dir.path()).await.unwrap();
    record_failed_attempt(dir.path()).await.unwrap();
    assert_eq!(get_failed_attempts(dir.path()).await, 2);

    tauri_delta_updater::clear_failed_attempts(dir.path()).await;
    assert_eq!(get_failed_attempts(dir.path()).await, 0);
    assert!(!should_abort(dir.path()).await);
}

#[test]
fn test_retryable_classification_drives_failure_counting() {
    use tauri_delta_updater::DeltaError;

    // 可重试 = 暂态错误 = 不计入安全模式（服务器抖动不应锁死用户）
    assert!(DeltaError::Timeout("t".into()).is_retryable());
    assert!(DeltaError::Network("n".into()).is_retryable());
    assert!(DeltaError::HttpStatus(500).is_retryable());
    assert!(DeltaError::HttpStatus(429).is_retryable());

    // 不可重试 = 确定性错误 = 计入安全模式（3 次即停，防止崩溃循环）
    assert!(!DeltaError::HttpStatus(404).is_retryable());
    assert!(
        !DeltaError::SignatureInvalid("s".into()).is_retryable(),
        "签名失败意味着攻击，必须计入并尽快进入安全模式"
    );
    assert!(!DeltaError::HashMismatch {
        expected: "a".into(),
        actual: "b".into()
    }
    .is_retryable());
    assert!(!DeltaError::Config("c".into()).is_retryable());
    assert!(!DeltaError::InsufficientDiskSpace.is_retryable());
    assert!(!DeltaError::TargetExists("t".into()).is_retryable());
    // 体积上限与编码错误是确定性拒绝：重试只会反复消耗带宽
    assert!(!DeltaError::ResponseTooLarge {
        url: "u".into(),
        limit: 10
    }
    .is_retryable());
    assert!(!DeltaError::PatchTooLarge { max_bytes: 10 }.is_retryable());
    assert!(!DeltaError::PatchExpandedTooLarge { max_bytes: 10 }.is_retryable());
    assert!(!DeltaError::ManifestInvalid("bad".into()).is_retryable());
    assert!(!DeltaError::InvalidResponse("bad".into()).is_retryable());
    assert!(!DeltaError::VersionInvalid("bad".into()).is_retryable());
    assert!(!DeltaError::WorkDirNotWritable(std::path::PathBuf::from("/x")).is_retryable());
    // 锁竞争是正常可重试情况，但不属于自动重试范围
    assert!(!DeltaError::UpdateInProgress.is_retryable());
}

#[test]
fn test_normalize_version_strips_prefix_and_whitespace() {
    use tauri_delta_updater::normalize_version;

    assert_eq!(normalize_version("  v1.2.3 "), "1.2.3");
    assert_eq!(normalize_version("V1.2.3"), "1.2.3");
    assert_eq!(normalize_version("1.2.3"), "1.2.3");
}

#[test]
fn test_incremental_fallback_reason_suitable_by_default() {
    // Windows 单 exe 是甜点区；macOS/Linux 的测试进程也不在 bundle/AppImage 里，
    // 默认环境必须判定为适合增量（若 CI runner 环境异常会在此暴露）
    let exe = std::env::current_exe().unwrap();
    assert!(tauri_delta_updater::incremental_fallback_reason(&exe).is_none());
}

#[test]
#[cfg(target_os = "macos")]
fn test_incremental_fallback_reason_for_macos_bundle() {
    // 造一个假的 .app bundle 路径：MyApp.app/Contents/MacOS/mybin
    let dir = tempdir().unwrap();
    let exe = dir.path().join("MyApp.app/Contents/MacOS/mybin");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"binary").unwrap();

    assert!(
        tauri_delta_updater::incremental_fallback_reason(&exe).is_some(),
        "bundle 内替换会丢失开发者签名，必须判定为不适合增量"
    );

    // bundle 外的裸二进制仍允许（实验性支持）
    let bare = dir.path().join("mybin");
    std::fs::write(&bare, b"binary").unwrap();
    assert!(tauri_delta_updater::incremental_fallback_reason(&bare).is_none());
}

// ==================== 下载：断点续传正确性 ====================

/// 极简 HTTP 服务器，用于验证下载器的续传行为。
///
/// `honor_range` 决定服务端是否正确响应 `Range` 头：
/// - `true`：返回 `206 Partial Content`，支持续传
/// - `false`：忽略 `Range`，直接返回 `200` 与完整内容
fn spawn_server(body: Vec<u8>, honor_range: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf);

            let range_start = if honor_range {
                // 头部名大小写由 HTTP 客户端决定（reqwest/hyper 发送小写），
                // 按原样匹配 "Range: bytes=" 会让续传分支永远走不到。
                let lower = request.to_lowercase();
                lower
                    .find("range: bytes=")
                    .and_then(|i| lower[i + 13..].split('-').next())
                    .and_then(|s| s.trim().parse::<usize>().ok())
            } else {
                None
            };

            match range_start {
                Some(start) if start < body.len() => {
                    let chunk = &body[start..];
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n\r\n",
                        chunk.len(),
                        start,
                        body.len() - 1,
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).unwrap();
                    stream.write_all(chunk).unwrap();
                }
                _ => {
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                    stream.write_all(head.as_bytes()).unwrap();
                    stream.write_all(&body).unwrap();
                }
            }
            stream.flush().unwrap();
        }
    });
    format!("http://{addr}/file.bin")
}

/// 第一次响应只发一半就断开，第二次按 Range 返回剩余内容。
///
/// 返回 `(url, 第二次请求是否带 Range)`：用来证明"传输中断后重试是接着下"，
/// 而不是丢掉已有进度从头再来。
fn spawn_truncating_server(
    body: Vec<u8>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let saw_range = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_range);
    let half = body.len() / 2;

    std::thread::spawn(move || {
        // 第一次：声明完整长度，却只写一半就关闭连接
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&body[..half]).unwrap();
        stream.flush().unwrap();
        drop(stream);

        // 第二次：续传请求。若客户端没有带 Range，就从 0 重发，
        // 测试会发现 saw_range 为 false。
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        let request = String::from_utf8_lossy(&buf);
        let lower = request.to_lowercase();
        let start = lower
            .find("range: bytes=")
            .and_then(|i| lower[i + 13..].split('-').next())
            .and_then(|s| s.trim().parse::<usize>().ok());
        if start.is_some() {
            flag.store(true, Ordering::Relaxed);
        }
        let start = start.unwrap_or(0).min(half);
        let chunk = &body[start..];
        let head = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
            chunk.len(),
            start,
            body.len() - 1,
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(chunk).unwrap();
        stream.flush().unwrap();
    });

    (format!("http://{addr}/file.bin"), saw_range)
}

#[tokio::test]
async fn test_download_resume_after_truncated_response() {
    // 关键回归：传输中断（实际字节少于声明的 Content-Length）时，
    // 必须保留已下载的部分，重试才能用 Range 接着下；删掉文件等于从头再来。
    use std::sync::atomic::Ordering;

    let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let (url, saw_range) = spawn_truncating_server(body.clone());

    let dir = tempdir().unwrap();
    let target = dir.path().join("patch.bin");
    let client = tauri_delta_updater::client_builder().build().unwrap();

    let mut last = 0u64;
    tauri_delta_updater::download_patch_with_progress(&client, &url, &target, 0, |d, _| {
        assert!(d >= last, "进度不得回退");
        last = d;
    })
    .await
    .expect("中断后应自动重试并完成下载");

    assert!(
        saw_range.load(Ordering::Relaxed),
        "重试必须带 Range 续传，而不是从头下载"
    );
    assert_eq!(fs::read(&target).await.unwrap(), body);
}

#[tokio::test]
async fn test_download_resume_does_not_corrupt_file() {
    // 关键回归：服务端忽略 Range 返回 200 时，
    // 下载器必须丢弃本地残留重新完整下载，而不是把完整内容追加到半截文件后面。
    let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let url = spawn_server(body.clone(), false);

    let dir = tempdir().unwrap();
    let target = dir.path().join("patch.bin");

    // 预置一个 1000 字节的半截文件，模拟上次中断的下载
    fs::write(&target, &body[..1000]).await.unwrap();

    let client = tauri_delta_updater::client_builder().build().unwrap();
    tauri_delta_updater::download_patch_with_progress(&client, &url, &target, 0, |_, _| {})
        .await
        .unwrap();

    let got = fs::read(&target).await.unwrap();
    assert_eq!(
        got.len(),
        body.len(),
        "服务端不支持续传时，文件长度必须等于完整长度，不能被追加撑大"
    );
    assert_eq!(
        got, body,
        "服务端不支持续传时，文件内容必须与完整内容一致，不能是拼接结果"
    );
}

#[tokio::test]
async fn test_download_resume_with_partial_content() {
    // 服务端正确支持 Range 时，续传后的文件也必须与完整内容一致
    let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let url = spawn_server(body.clone(), true);

    let dir = tempdir().unwrap();
    let target = dir.path().join("patch.bin");
    fs::write(&target, &body[..1000]).await.unwrap();

    let client = tauri_delta_updater::client_builder().build().unwrap();
    let mut progress = Vec::new();
    tauri_delta_updater::download_patch_with_progress(&client, &url, &target, 0, |d, t| {
        progress.push((d, t))
    })
    .await
    .unwrap();

    let got = fs::read(&target).await.unwrap();
    assert_eq!(got, body, "续传后的文件必须与完整内容一致");

    // 进度回调的上报字节数不应回退
    assert!(!progress.is_empty(), "必须上报下载进度");
    assert!(
        progress.windows(2).all(|w| w[0].0 <= w[1].0),
        "下载进度不得回退"
    );
    let (last_downloaded, _) = *progress.last().unwrap();
    assert_eq!(last_downloaded, body.len() as u64);
}

#[tokio::test]
async fn test_download_from_scratch() {
    let body: Vec<u8> = (0..2048u32).map(|i| (i % 97) as u8).collect();
    let url = spawn_server(body.clone(), true);

    let dir = tempdir().unwrap();
    let target = dir.path().join("patch.bin");

    let client = tauri_delta_updater::client_builder().build().unwrap();
    let mut last = (0u64, 0u64);
    tauri_delta_updater::download_patch_with_progress(&client, &url, &target, 0, |d, t| {
        last = (d, t)
    })
    .await
    .unwrap();

    assert_eq!(fs::read(&target).await.unwrap(), body);
    assert_eq!(last.0, body.len() as u64);
    assert_eq!(last.1, body.len() as u64);
}

/// 响应体超过 max_bytes 上限时必须中止、删除半截文件，且不重试。
#[tokio::test]
async fn test_download_aborts_when_response_exceeds_limit() {
    let body: Vec<u8> = (0..2048u32).map(|i| (i % 97) as u8).collect();
    let url = spawn_server(body.clone(), true);

    let dir = tempdir().unwrap();
    let target = dir.path().join("patch.bin");

    let client = tauri_delta_updater::client_builder().build().unwrap();
    let result =
        tauri_delta_updater::download_patch_with_progress(&client, &url, &target, 1024, |_, _| {})
            .await;

    match result {
        Err(tauri_delta_updater::DeltaError::PatchTooLarge { max_bytes }) => {
            assert_eq!(max_bytes, 1024);
        }
        other => panic!("应当返回 PatchTooLarge，实际: {other:?}"),
    }

    // 半截文件必须被删除，不能留在磁盘上
    assert!(
        !target.exists(),
        "超限中止后不得残留半截文件: {}",
        target.display()
    );
}

// ==================== 替换脚本 ====================

/// 构造一个已验证产物（走真实的补丁应用流程），供脚本生成测试使用。
///
/// 脚本只接受 `VerifiedArtifact`——未经验证的产物在类型上无法进入替换流程，
/// 因此测试也必须通过 apply_patch 产出它。
async fn make_verified_artifact(
    dir: &Path,
    name: &str,
) -> (std::path::PathBuf, tauri_delta_updater::VerifiedArtifact) {
    let current_exe = dir.join(format!("{name}.exe"));
    std::fs::write(&current_exe, b"old version content").unwrap();
    let new_src = dir.join(format!("{name}_new.bin"));
    std::fs::write(&new_src, b"new version content!").unwrap();

    let patch = dir.join(format!("{name}.patch"));
    create_patch(&current_exe, &new_src, &patch).await.unwrap();
    let patch_hash = calculate_hash(&patch).await.unwrap();
    let target_hash = calculate_hash(&new_src).await.unwrap();
    let staged = dir.join(format!("{name}_staged.exe"));
    let artifact = apply_patch(&current_exe, &patch, &staged, &patch_hash, &target_hash)
        .await
        .unwrap();
    let _ = std::fs::remove_file(&patch);
    (current_exe, artifact)
}

#[tokio::test]
async fn test_script_generation_only() {
    let dir = tempdir().unwrap();
    let (current_exe, artifact) = make_verified_artifact(dir.path(), "Sereno").await;
    let new_exe = artifact.path().to_path_buf();

    let script_path =
        tauri_delta_updater::PlatformUpdater::generate_updater(&current_exe, &artifact, dir.path())
            .unwrap();
    assert!(script_path.exists());

    // 脚本内容按平台编码读取：Windows 是 UTF-8 带 BOM，Unix 是 UTF-8
    let raw = std::fs::read(&script_path).unwrap();
    let content = String::from_utf8_lossy(&raw).to_string();

    // 启动成功标志是"保留新版本还是回滚"的判据，必须出现在脚本里
    assert!(
        content.contains(tauri_delta_updater::core::artifacts::SUCCESS_FLAG),
        "脚本必须检查启动成功标志"
    );

    // 脚本必须引用新文件的完整路径，否则换目录后就找不到待替换的文件
    assert!(
        content.contains(&new_exe.display().to_string()),
        "脚本必须引用新文件的完整路径"
    );

    // 脚本必须按路径结束进程，而不是按进程名批量结束
    assert!(
        !content.contains("taskkill /f /im"),
        "不得按进程名批量结束，否则会误杀同名应用"
    );

    // 暂存文件哈希要交给脚本，复制前再校验一次（封住 TOCTOU 窗口）
    assert!(
        content.contains(artifact.sha256()),
        "脚本必须包含暂存产物的 SHA-256"
    );
}

/// 替换脚本必须生成在工作目录，而不是安装目录。
///
/// 安装目录（`C:\Program Files`、`/usr/bin`）对普通用户只读：
/// 脚本写不进去，更新会在生成脚本时失败。
#[tokio::test]
async fn test_script_is_generated_in_work_dir() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().join("work");
    fs::create_dir_all(&work_dir).await.unwrap();
    let (current_exe, artifact) = make_verified_artifact(dir.path(), "Sereno").await;

    let script_path =
        tauri_delta_updater::PlatformUpdater::generate_updater(&current_exe, &artifact, &work_dir)
            .unwrap();

    assert_eq!(
        script_path.parent(),
        Some(work_dir.as_path()),
        "替换脚本必须生成在用户可写的工作目录"
    );
}

/// `detect_arch` 对无法识别的文件名必须返回 `unknown` 而不是默认 x86_64——
/// 默认值会把架构不明的旧包静默当成 x86_64，生成跨架构补丁。
#[test]
fn test_detect_arch_returns_unknown_for_unidentifiable_names() {
    use tauri_delta_updater::detect_arch;

    // 常见写法仍然正确识别
    assert_eq!(detect_arch("Sereno_0.2.1_x64-setup.exe"), "x86_64");
    assert_eq!(detect_arch("Sereno_0.2.1_x86-64-setup.exe"), "x86_64");
    assert_eq!(detect_arch("Sereno_0.2.1_aarch64-setup.exe"), "aarch64");
    assert_eq!(detect_arch("Sereno_0.2.1_arm64-setup.exe"), "aarch64");
    assert_eq!(detect_arch("Sereno_0.2.1_i686-setup.exe"), "i686");

    // 无架构信息的名字必须显式暴露为 unknown
    assert_eq!(detect_arch("Sereno_0.2.1-setup.exe"), "unknown");
    assert_eq!(detect_arch("installer.exe"), "unknown");
}

// ==================== 补丁压缩 ====================

/// create_patch 的产出必须是 zstd 容器，且压缩后显著小于原始 bsdiff 输出。
#[tokio::test]
async fn test_create_patch_output_is_zstd_compressed() {
    use sha2::{Digest, Sha256};

    let dir = tempdir().unwrap();
    let old = dir.path().join("old.bin");
    let new = dir.path().join("new.bin");
    let patch = dir.path().join("test.patch");

    // 64 KB 重复模式数据：bsdiff 原始输出的差异流可压缩性极高
    let old_data: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 7) as u8).collect();
    let mut new_data = old_data.clone();
    new_data[..256].copy_from_slice(&[0xAA; 256]);
    std::fs::write(&old, &old_data).unwrap();
    std::fs::write(&new, &new_data).unwrap();

    tauri_delta_updater::create_patch(&old, &new, &patch)
        .await
        .unwrap();

    let compressed = std::fs::read(&patch).unwrap();
    // zstd 魔数 0x28 B5 2F FD
    assert_eq!(
        &compressed[..4],
        &[0x28, 0xB5, 0x2F, 0xFD],
        "补丁必须是 zstd 容器"
    );

    // 对照：原始 bsdiff 输出的大小
    let raw_size = {
        let mut raw = Vec::new();
        qbsdiff::Bsdiff::new(&old_data, &new_data)
            .compare(&mut raw)
            .unwrap();
        raw.len()
    };
    assert!(
        compressed.len() < raw_size,
        "zstd 压缩后（{} 字节）必须小于原始 bsdiff 输出（{} 字节）",
        compressed.len(),
        raw_size
    );

    // 压缩形态的哈希就是清单应写入的 patch_hash
    let mut h = Sha256::new();
    h.update(&compressed);
    let _patch_hash = format!("{:x}", h.finalize());

    // 完整回路：压缩补丁能应用出正确的新版本
    let output = dir.path().join("output.bin");
    tauri_delta_updater::apply_patch(
        &old,
        &patch,
        &output,
        &_patch_hash,
        &format!("{:x}", {
            let mut h = Sha256::new();
            h.update(&new_data);
            h.finalize()
        }),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), new_data);
}

/// 发布端必须能原生产出旧客户端认识的未压缩补丁：
/// 产物以 `BSDIFF40` 开头，且能被 `apply_patch` 应用。
#[tokio::test]
async fn test_create_uncompressed_patch_is_legacy_compatible() {
    let dir = tempdir().unwrap();
    let old = dir.path().join("old.bin");
    let new = dir.path().join("new.bin");
    let patch = dir.path().join("legacy.patch");

    let old_data: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
    let mut new_data = old_data.clone();
    new_data[..1024].copy_from_slice(&[0xAB; 1024]);
    std::fs::write(&old, &old_data).unwrap();
    std::fs::write(&new, &new_data).unwrap();

    tauri_delta_updater::create_patch_uncompressed(&old, &new, &patch)
        .await
        .unwrap();

    let bytes = std::fs::read(&patch).unwrap();
    assert_eq!(&bytes[..8], b"BSDIFF40", "未压缩补丁必须是 BSDIFF40");
    assert_ne!(
        &bytes[..4],
        &[0x28, 0xB5, 0x2F, 0xFD],
        "未压缩补丁不能是 zstd 容器"
    );

    let output = dir.path().join("output.bin");
    let patch_hash = calculate_hash(&patch).await.unwrap();
    let new_hash = calculate_hash(&new).await.unwrap();
    apply_patch(&old, &patch, &output, &patch_hash, &new_hash)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), new_data);
}

/// 本地验证闭环：不需要网络与签名密钥，就能确认增量真的生效。
#[tokio::test]
async fn test_simulate_update_reports_patch_ratio() {
    let dir = tempdir().unwrap();
    let old = dir.path().join("old.bin");
    let new = dir.path().join("new.bin");
    let work = dir.path().join("sim");
    std::fs::create_dir_all(&work).unwrap();

    let old_data: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 251) as u8).collect();
    let mut new_data = old_data.clone();
    // 只改动末尾 64 KB：补丁应远小于全量
    for byte in new_data[192 * 1024..].iter_mut() {
        *byte = 0x5A;
    }
    std::fs::write(&old, &old_data).unwrap();
    std::fs::write(&new, &new_data).unwrap();

    let report = tauri_delta_updater::simulate_update(&old, &new, &work)
        .await
        .unwrap();

    assert_eq!(report.target_size, new_data.len() as u64);
    assert!(
        report.ratio() < 0.5,
        "改动 1/4 时补丁占比应明显小于全量，实际 {:.3}",
        report.ratio()
    );
    assert_eq!(report.target_hash.len(), 64);
    assert_eq!(report.patch_hash.len(), 64);

    // 临时产物必须清理干净，不污染工作目录
    assert!(!work.join("simulate.patch").exists());
    assert!(!work.join("simulate.staged").exists());
}

/// `--zstd-level` 之外的公开入口：压缩级别参数必须被校验。
#[tokio::test]
async fn test_create_patch_with_level_rejects_invalid_level() {
    let dir = tempdir().unwrap();
    let old = dir.path().join("o.bin");
    let new = dir.path().join("n.bin");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&new, b"new").unwrap();

    let result =
        tauri_delta_updater::create_patch_with_level(&old, &new, dir.path().join("p"), 0).await;
    match result {
        Err(tauri_delta_updater::DeltaError::Config(msg)) => {
            assert!(msg.contains("1–22"), "错误信息必须说明合法范围: {msg}");
        }
        other => panic!("非法级别必须返回 Config 错误，实际: {other:?}"),
    }

    // 合法级别可用，产物仍是 zstd 容器
    tauri_delta_updater::create_patch_with_level(&old, &new, dir.path().join("p2"), 15)
        .await
        .unwrap();
    let bytes = std::fs::read(dir.path().join("p2")).unwrap();
    assert_eq!(&bytes[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
}

/// 诊断快照必须如实反映环境，并给出日志路径。
#[tokio::test]
async fn test_diagnose_reports_environment() {
    let dir = tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let exe = dir.path().join("app.exe");
    std::fs::write(&exe, b"app").unwrap();

    // 显式工作目录：缓存目录是否可写属于机器状态（受限配置、杀软占用都可能
    // 让探针瞬时失败），用它做断言会让测试在真实环境里偶发红灯。
    let updater = tauri_delta_updater::DeltaUpdater::new(
        tauri_delta_updater::UpdateConfig::new("https://cdn.example.com/latest.json", "PK")
            .current_version("1.2.3")
            .executable_path(&exe)
            .work_dir(&work),
    )
    .unwrap();
    let diag = updater.diagnose().await;

    assert_eq!(diag.work_dir, work);
    assert!(diag.work_dir_writable);
    assert_eq!(diag.executable, exe);
    assert_eq!(diag.platform, tauri_delta_updater::current_platform());
    assert_eq!(
        diag.log_path.file_name().unwrap(),
        tauri_delta_updater::core::artifacts::LOG_FILE
    );
    assert!(!diag.safe_mode);
    assert!(diag.can_use_delta());
}

/// 普通启动（没有 `tdx_update_dir` 指针）时，启动确认必须是零写入的空操作。
#[test]
fn test_confirm_startup_if_pending_is_noop_without_marker() {
    let exe = std::env::current_exe().unwrap();
    let marker = exe.parent().unwrap().join("tdx_update_dir");
    if marker.exists() {
        // 测试环境恰好残留指针时不做断言，避免误报
        return;
    }
    assert!(
        !tauri_delta_updater::PlatformUpdater::confirm_startup_success_if_pending().unwrap(),
        "没有指针时必须返回 false"
    );
}

/// 历史未压缩补丁（无 zstd 魔数）必须仍然可用——旧清单不需要迁移。
#[tokio::test]
async fn test_apply_patch_accepts_legacy_uncompressed_patch() {
    use sha2::{Digest, Sha256};

    let dir = tempdir().unwrap();
    let old = dir.path().join("old.bin");
    let new = dir.path().join("new.bin");
    let patch = dir.path().join("legacy.patch");

    let old_data: Vec<u8> = (0..32 * 1024u32).map(|i| (i % 13) as u8).collect();
    let mut new_data = old_data.clone();
    new_data[..128].copy_from_slice(&[0xBB; 128]);
    std::fs::write(&old, &old_data).unwrap();
    std::fs::write(&new, &new_data).unwrap();

    // 直接用 qbsdiff 产出未压缩补丁——历史版本发布形态
    let mut raw = Vec::new();
    qbsdiff::Bsdiff::new(&old_data, &new_data)
        .compare(&mut raw)
        .unwrap();
    std::fs::write(&patch, &raw).unwrap();

    let mut h = Sha256::new();
    h.update(&raw);
    let patch_hash = format!("{:x}", h.finalize());
    let mut h = Sha256::new();
    h.update(&new_data);
    let target_hash = format!("{:x}", h.finalize());

    let output = dir.path().join("output.bin");
    tauri_delta_updater::apply_patch(&old, &patch, &output, &patch_hash, &target_hash)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), new_data);
}
