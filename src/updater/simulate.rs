//! 本地验证闭环：无网络、无签名密钥也能跑完生成 → 合并 → 校验。

use crate::core::diff::{apply_patch, create_patch};
use crate::core::hasher::calculate_hash;
use crate::error::{DeltaError, Result};
use std::path::Path;

/// 本地验证闭环：在没有网络、没有签名密钥的情况下跑完
/// "生成补丁 → 应用补丁 → 校验产物"。
///
/// 这是给开发者在接入前亲眼确认"增量真的生效"用的：
///
/// ```no_run
/// # async fn example() -> tauri_delta_updater::Result<()> {
/// let report = tauri_delta_updater::simulate_update(
///     "old.exe",
///     "new.exe",
///     std::path::Path::new("target/delta-sim"),
/// )
/// .await?;
/// println!(
///     "补丁 {} 字节，全量 {} 字节，压缩比 {:.1}%",
///     report.patch_size,
///     report.target_size,
///     report.ratio() * 100.0
/// );
/// # Ok(())
/// # }
/// ```
///
/// 全过程只写 `work_dir`，不接触原始文件；结束时会删除临时产物。
pub async fn simulate_update(
    old_file: impl AsRef<Path>,
    new_file: impl AsRef<Path>,
    work_dir: impl AsRef<Path>,
) -> Result<SimulationReport> {
    let work_dir = work_dir.as_ref();
    crate::core::paths::ensure_writable(work_dir)?;

    let old = old_file.as_ref();
    let new = new_file.as_ref();

    let patch_path = work_dir.join("simulate.patch");
    let staged_path = work_dir.join("simulate.staged");
    let _ = tokio::fs::remove_file(&patch_path).await;
    let _ = tokio::fs::remove_file(&staged_path).await;

    create_patch(old, new, &patch_path).await?;

    let patch_size = std::fs::metadata(&patch_path)
        .map_err(DeltaError::Io)?
        .len();
    let target_size = std::fs::metadata(new).map_err(DeltaError::Io)?.len();
    let patch_hash = calculate_hash(&patch_path).await?;
    let target_hash = calculate_hash(new).await?;

    // 与真实客户端完全相同的路径：双端哈希校验 + 产物落盘
    let artifact = apply_patch(old, &patch_path, &staged_path, &patch_hash, &target_hash).await?;
    artifact.verify_integrity()?;

    let _ = tokio::fs::remove_file(&patch_path).await;
    let _ = tokio::fs::remove_file(&staged_path).await;

    Ok(SimulationReport {
        patch_size,
        target_size,
        patch_hash,
        target_hash,
    })
}

/// [`simulate_update`] 的结果。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SimulationReport {
    /// 补丁体积（字节）。
    pub patch_size: u64,
    /// 新版本体积（字节）。
    pub target_size: u64,
    /// 补丁 SHA-256。
    pub patch_hash: String,
    /// 新版本 SHA-256。
    pub target_hash: String,
}

impl SimulationReport {
    /// 补丁占全量的比例（0.0–1.0）。
    pub fn ratio(&self) -> f64 {
        if self.target_size == 0 {
            0.0
        } else {
            self.patch_size as f64 / self.target_size as f64
        }
    }
}
