//! 整包补丁生成与 `latest.bundle.json`。

use super::files::old_app_version;
use super::ReleaseOptions;
use crate::{create_bundle_patch, normalize_version, BundleManifest, BundlePatch};
use std::path::{Path, PathBuf};

/// 生成整包（多文件）补丁，并写出独立签名的 `latest.bundle.json`。
///
/// 未提供 `--new-app-dir` 时是零操作。整包补丁与单文件补丁互不影响：
/// 客户端可以只消费 `latest.bundle.json`，旧客户端继续使用 `latest.json`。
///
/// 返回实际写出的补丁路径（用于 `SHA256SUMS` 与发布报告）。
pub(crate) async fn generate_bundles(
    options: &ReleaseOptions,
    new_version: &str,
    platform_key: &str,
    output_dir: &Path,
    patch_dir: &Path,
    manifest_file: &str,
    signing_keys: &[minisign::SecretKey],
) -> Result<Vec<PathBuf>, String> {
    let Some(new_dir) = &options.new_app_dir else {
        return Ok(Vec::new());
    };

    let canonical_new =
        std::fs::canonicalize(new_dir).map_err(|e| format!("解析 --new-app-dir 失败: {e}"))?;
    let canonical_app = std::fs::canonicalize(&options.app_binary)
        .map_err(|e| format!("解析 --app-binary 失败: {e}"))?;
    if !canonical_app.starts_with(&canonical_new) {
        return Err("--app-binary 必须位于 --new-app-dir 内".into());
    }

    let mut bundles: Vec<BundlePatch> = Vec::new();
    for dir in &options.old_app_dirs {
        let from_version = old_app_version(dir)
            .map(|v| normalize_version(&v).to_string())
            .ok_or_else(|| {
                format!(
                    "--old-app-dir 路径里必须包含版本号（目录名或 .app 的父目录名，如 v0.1.0）: {}",
                    dir.display()
                )
            })?;
        semver::Version::parse(&from_version)
            .map_err(|e| format!("--old-app-dir 版本号非法（{from_version}）: {e}"))?;
        if from_version == new_version {
            println!("[SKIP] 跳过与本次发布同版本的整包基线: {}", dir.display());
            continue;
        }

        let patch_name = format!("{from_version}_to_{new_version}.bundle.patch");
        let patch_path = patch_dir.join(&patch_name);
        let patch_url = format!("{}/{patch_name}", options.base_url.trim_end_matches('/'));

        println!("[INFO] 生成整包补丁: {from_version} -> {new_version}");
        let bundle = create_bundle_patch(
            dir,
            new_dir,
            &from_version,
            new_version,
            patch_url,
            &patch_path,
            patch_dir,
        )
        .await
        .map_err(|e| format!("生成整包补丁失败（{from_version}）: {e}"))?;
        println!(
            "[OK] 整包补丁 {patch_name}: {} 个文件 -> {} 个文件，{:?} 字节",
            bundle.base_files.len(),
            bundle.target_files.len(),
            bundle.patch_size
        );
        bundles.push(bundle);
    }

    bundles.sort_by(|a, b| {
        semver::Version::parse(&a.from_version)
            .ok()
            .cmp(&semver::Version::parse(&b.from_version).ok())
    });
    bundles.dedup_by(|a, b| a.from_version == b.from_version);

    if bundles.is_empty() && options.fail_on_empty {
        return Err("没有生成任何整包补丁：检查 --old-app-dir 的目录名是否包含版本号".into());
    }

    let manifest = BundleManifest {
        version: new_version.to_string(),
        platform: platform_key.to_string(),
        bundles,
        extra: Default::default(),
    };
    let count = manifest.bundles.len();
    // 以最终清单为准计算路径：dedup 之后不会重复列出同一个补丁。
    let patch_paths: Vec<PathBuf> = manifest
        .bundles
        .iter()
        .map(|bundle| {
            patch_dir.join(format!(
                "{}_to_{}.bundle.patch",
                bundle.from_version, new_version
            ))
        })
        .collect();

    let json =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("序列化整包清单失败: {e}"))?;
    std::fs::create_dir_all(output_dir).map_err(|e| format!("创建输出目录失败: {e}"))?;
    std::fs::write(output_dir.join(manifest_file), &json)
        .map_err(|e| format!("写入 {manifest_file} 失败: {e}"))?;
    let signature =
        super::sign_with_keys(signing_keys, &json, Some(&format!("tdu:v={new_version}")))
            .map_err(|e| format!("签名整包清单失败: {e}"))?;
    std::fs::write(output_dir.join(format!("{manifest_file}.sig")), signature)
        .map_err(|e| format!("写入 {manifest_file}.sig 失败: {e}"))?;

    println!("[OK] {manifest_file} + .sig 已生成（整包补丁 {count} 个）");
    Ok(patch_paths)
}
