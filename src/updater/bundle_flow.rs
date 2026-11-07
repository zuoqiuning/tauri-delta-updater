//! 整包（多文件）增量流程与整包清单解析。

use super::MAX_PATCH_BYTES;
use super::{emit, is_cancelled, DeltaUpdater, StagedBundle, UpdateEvent, UpdateOutcome};
use crate::core::bundle::{apply_bundle_patch, BundleManifest, BundlePatch};
use crate::core::safety::clear_failed_attempts;
use crate::error::{DeltaError, Result};
use crate::platform::PlatformUpdater;
use std::sync::atomic::AtomicBool;

/// 整包清单的解析结果。
pub(super) enum BundleResolution {
    /// 没有配置整包清单，继续使用 `latest.json` 里的单文件补丁。
    NotConfigured,
    /// 命中可用的整包补丁。
    Ready {
        /// 已验签的整包清单。
        manifest: Box<BundleManifest>,
        /// 当前版本对应的整包补丁。
        patch: Box<BundlePatch>,
    },
    /// 配置了整包清单，但当前环境不能做整包替换；附带具体原因。
    Unsupported(&'static str),
    /// 配置了整包清单，但没有当前版本对应的补丁。
    NoPatch,
}

impl DeltaUpdater {
    /// 下载、应用整包补丁，并把新版本文件提取到暂存目录。
    pub(super) async fn run_bundle_patch(
        &self,
        bundle_manifest: Box<BundleManifest>,
        patch: Box<BundlePatch>,
        launch: bool,
        on_event: &mut Option<Box<dyn FnMut(UpdateEvent) + Send + '_>>,
        cancel: Option<&crate::core::cancel::CancelToken>,
    ) -> Result<UpdateOutcome> {
        let bundle_manifest = *bundle_manifest;
        let patch = *patch;
        let version = bundle_manifest.version.clone();
        crate::core::events::patch_matched(
            &patch.from_version,
            &patch.to_version,
            patch.patch_size,
        );

        let patch_path = self
            .work_dir
            .join(crate::core::artifacts::BUNDLE_PATCH_FILE);
        let staging_dir = self
            .work_dir
            .join(crate::core::artifacts::bundle_staging_dir());
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;

        // 跨进程续传：仅当残留补丁属于同一个整包补丁时才继续
        crate::core::downloader::prepare_resume(
            &patch_path,
            &self
                .work_dir
                .join(crate::core::artifacts::BUNDLE_PATCH_META),
            &patch.patch_hash,
        )
        .await;

        let max_bytes = patch
            .patch_size
            .map(|size| size.saturating_add(1024 * 1024))
            .unwrap_or(MAX_PATCH_BYTES);

        // 整包路径会同时存在：补丁、旧归档、新归档与暂存目录。
        // 下载前做保守预检，避免下完补丁才因磁盘不足失败并计入安全模式。
        let base_bytes: u64 = patch.base_files.iter().map(|file| file.size).sum();
        let target_bytes: u64 = patch.target_files.iter().map(|file| file.size).sum();
        let required = patch
            .patch_size
            .unwrap_or(0)
            .saturating_add(base_bytes.saturating_mul(2))
            .saturating_add(target_bytes.saturating_mul(2))
            .saturating_add(128 * 1024 * 1024);
        crate::core::disk::check_disk_space(&self.work_dir, required)?;

        super::download_and_verify_patch(
            &self.client,
            &patch.patch_url,
            &patch_path,
            &self
                .work_dir
                .join(crate::core::artifacts::BUNDLE_PATCH_META),
            &patch.patch_hash,
            max_bytes,
            "Bundle",
            cancel,
            on_event,
        )
        .await?;

        let install_dir = PlatformUpdater::bundle_install_root(&self.executable_path)?;
        apply_bundle_patch(
            &install_dir,
            &patch_path,
            &patch,
            &staging_dir,
            &self.work_dir,
        )
        .await?;

        // 整包：新版本主程序也必须保持原有的 Authenticode 状态
        if let Some(name) = self.executable_path.file_name() {
            PlatformUpdater::verify_patched_binary(&self.executable_path, &staging_dir.join(name))?;
        }

        let _ = tokio::fs::remove_file(&patch_path).await;
        let _ = tokio::fs::remove_file(
            self.work_dir
                .join(crate::core::artifacts::BUNDLE_PATCH_META),
        )
        .await;

        if is_cancelled(cancel) {
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            emit(on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }

        let script = PlatformUpdater::generate_bundle_updater_with(
            &self.executable_path,
            &staging_dir,
            &patch.base_files,
            &patch.target_files,
            &self.work_dir,
            self.script_timings(),
        )?;

        // 启动替换脚本之后就无法撤销了：这是最后一次取消窗口。
        if is_cancelled(cancel) {
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            emit(on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }

        clear_failed_attempts(&self.work_dir).await;
        crate::core::events::patched(&version);

        let staged = StagedBundle {
            staging_dir,
            files: patch.target_files,
            script_path: script,
            manifest: Box::new(bundle_manifest),
            launched: std::sync::Arc::new(AtomicBool::new(false)),
        };

        if launch {
            staged.launch()?;
        } else {
            let artifact = super::pending::PendingArtifact::Bundle {
                staging_dir: staged.staging_dir.clone(),
                files: staged.files.clone(),
            };
            self.store_pending(&version, &staged.script_path, artifact)
                .await?;
        }

        emit(on_event, UpdateEvent::Ready { version });

        Ok(UpdateOutcome::Bundled { staged })
    }

    /// 解析整包清单，判断本次是否走多文件增量。
    pub(super) async fn resolve_bundle_patch(
        &self,
        target_version: &str,
    ) -> Result<BundleResolution> {
        let Some(url) = self.config.bundle_manifest_url.as_deref() else {
            return Ok(BundleResolution::NotConfigured);
        };
        if let Some(reason) = PlatformUpdater::bundle_unsupported_reason(&self.executable_path) {
            return Ok(BundleResolution::Unsupported(reason));
        }

        let manifest = match self.fetch_verified_bundle_manifest(url).await {
            Ok(manifest) => manifest,
            // 发布端还没上传整包清单：与"没有匹配补丁"等价，交给全量回退。
            Err(DeltaError::HttpStatus(404)) => return Ok(BundleResolution::NoPatch),
            Err(e) => return Err(e),
        };

        // 主清单已发布新版本，但整包清单仍停留在旧版本：不能把旧版本的
        // 文件清单套到新版本上，回退全量才是安全的。
        if manifest.version != target_version {
            return Ok(BundleResolution::NoPatch);
        }

        let patch = manifest
            .find_patch(&self.config.current_version, &self.config.platform)
            .cloned();
        match patch {
            Some(patch) => Ok(BundleResolution::Ready {
                manifest: Box::new(manifest),
                patch: Box::new(patch),
            }),
            None => Ok(BundleResolution::NoPatch),
        }
    }
}
