//! 整包补丁与整包清单：生成、查找与应用。

use super::archive::{build_archive, describe_dir, extract_archive, hash_file, verify_layout};
use super::BundleFile;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// 整包临时归档的序号：并行生成时保证每个任务拿到不同文件。
static ARCHIVE_COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_archive_paths(work_dir: &Path) -> (PathBuf, PathBuf) {
    let seq = ARCHIVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let prefix = format!("bundle.{}.{seq}", std::process::id());
    (
        work_dir.join(format!("{prefix}.old")),
        work_dir.join(format!("{prefix}.new")),
    )
}

/// 一份整包增量补丁的描述。
///
/// 客户端先按 `base_files` 校验本地安装目录，再构建旧归档并应用补丁；
/// 重建出的新归档必须与 `target_hash` 一致，随后按 `target_files` 提取暂存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BundlePatch {
    /// 起始版本（客户端当前版本）。
    pub from_version: String,
    /// 目标版本。
    pub to_version: String,
    /// 补丁下载地址。
    pub patch_url: String,
    /// 补丁文件的 SHA-256。
    pub patch_hash: String,
    /// 重建出的归档的 SHA-256。
    pub target_hash: String,
    /// 补丁体积（字节）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_size: Option<u64>,
    /// 旧版本安装目录应包含的文件。
    pub base_files: Vec<BundleFile>,
    /// 新版本安装目录应包含的文件。
    pub target_files: Vec<BundleFile>,
}

/// 整包清单（`latest.bundle.json`）。
///
/// 与 `latest.json` 并列发布并独立签名。客户端先用主清单确认版本，
/// 再用本清单里的文件清单与哈希完成整包增量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BundleManifest {
    /// 目标版本。
    pub version: String,
    /// 目标平台标识（如 `windows-x86_64`）。
    pub platform: String,
    /// 每个历史版本的整包补丁。
    #[serde(default)]
    pub bundles: Vec<BundlePatch>,
    /// 未识别的顶层字段，原样保留（与 [`crate::UpdateManifest::extra`] 同语义）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl BundleManifest {
    /// 查找从 `from_version` 到本清单版本、且平台一致的整包补丁。
    ///
    /// 平台不一致或补丁的 `to_version` 与清单版本不符（发布端配置错误）时
    /// 返回 `None`，调用方应回退全量下载，而不是套用其它平台的字节。
    pub fn find_patch(&self, from_version: &str, platform: &str) -> Option<&BundlePatch> {
        if self.platform != platform {
            return None;
        }
        self.bundles
            .iter()
            .find(|patch| patch.from_version == from_version && patch.to_version == self.version)
    }
}

/// 从两个应用目录生成整包补丁。
///
/// `work_dir` 用于存放临时归档（调用方保证可写）；成功时只留下 `patch_out`。
/// `patch_url` 只写进描述里，函数本身不联网。
pub async fn create_bundle_patch(
    old_dir: &Path,
    new_dir: &Path,
    from_version: &str,
    to_version: &str,
    patch_url: impl Into<String>,
    patch_out: &Path,
    work_dir: &Path,
) -> Result<BundlePatch> {
    let base_files = describe_dir(old_dir)?;
    let target_files = describe_dir(new_dir)?;

    // 临时归档必须唯一：多个基线会并行生成整包补丁，
    // 固定文件名会让并发任务互相覆盖（后写入的归档被另一个任务当成输入）。
    let (old_archive, new_archive) = temp_archive_paths(work_dir);
    build_archive(old_dir, &base_files, &old_archive)?;
    build_archive(new_dir, &target_files, &new_archive)?;

    let (_, target_hash) = hash_file(&new_archive)?;
    crate::core::diff::create_patch(&old_archive, &new_archive, patch_out).await?;
    let patch_hash = crate::core::hasher::calculate_hash(patch_out).await?;
    let patch_size = std::fs::metadata(patch_out).map(|m| m.len()).ok();

    let _ = std::fs::remove_file(&old_archive);
    let _ = std::fs::remove_file(&new_archive);

    Ok(BundlePatch {
        from_version: from_version.to_string(),
        to_version: to_version.to_string(),
        patch_url: patch_url.into(),
        patch_hash,
        target_hash,
        patch_size,
        base_files,
        target_files,
    })
}

/// 校验安装目录、应用整包补丁，并把新版本提取到 `staging_dir`。
///
/// `staging_dir` 必须是空目录（或不存在）：本函数不做清理。
/// 返回前会逐文件校验 `target_files`，调用方此后才应触发文件替换。
pub async fn apply_bundle_patch(
    install_dir: &Path,
    patch_file: &Path,
    patch: &BundlePatch,
    staging_dir: &Path,
    work_dir: &Path,
) -> Result<()> {
    // 本地文件必须与发布端记录完全一致，否则重建出的归档不可信。
    verify_layout(install_dir, &patch.base_files)?;

    let (old_archive, new_archive) = temp_archive_paths(work_dir);
    build_archive(install_dir, &patch.base_files, &old_archive)?;

    let result = crate::core::diff::apply_patch(
        &old_archive,
        patch_file,
        &new_archive,
        &patch.patch_hash,
        &patch.target_hash,
    )
    .await;

    let _ = std::fs::remove_file(&old_archive);
    let artifact = result?;

    let extracted = extract_archive(artifact.path(), &patch.target_files, staging_dir);
    let _ = std::fs::remove_file(&new_archive);
    extracted
}
