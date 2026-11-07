//! 单文件增量流程：下载补丁、合并、校验并生成替换脚本。

use super::bundle_flow::BundleResolution;
use super::{emit, is_cancelled, DeltaUpdater, FallbackReason, Staged, UpdateEvent, UpdateOutcome};
use crate::core::diff::apply_patch;
use crate::core::disk::cleanup_previous_updates;
use crate::core::safety::clear_failed_attempts;
use crate::error::{DeltaError, Result};
use crate::platform::PlatformUpdater;
use std::sync::atomic::AtomicBool;

use super::MAX_PATCH_BYTES;

impl DeltaUpdater {
    /// 持锁执行的实际更新流程。
    pub(super) async fn run_locked(
        &self,
        launch: bool,
        mut on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>>,
        cancel: Option<&crate::core::cancel::CancelToken>,
    ) -> Result<UpdateOutcome> {
        // 清理上一轮遗留的临时文件
        cleanup_previous_updates(&self.work_dir, &self.executable_path).await;

        if is_cancelled(cancel) {
            emit(&mut on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }
        emit(&mut on_event, UpdateEvent::Checking);

        // 拉取并验签清单
        let manifest = self.fetch_verified_manifest().await?;

        // 版本单调性：签名合法 ≠ 比当前新。重放旧清单时必须在这里止步，
        // 否则会经全量回退把用户降级到旧版本。
        if !manifest.is_newer_than(&self.config.current_version)? {
            crate::core::events::check(&manifest.version, false, false);
            emit(
                &mut on_event,
                UpdateEvent::UpToDate {
                    version: manifest.version.clone(),
                },
            );
            return Ok(UpdateOutcome::UpToDate {
                version: manifest.version,
            });
        }

        // 灰度：未命中的安装不进入任何下载/合并路径。
        // 同一版本的桶位稳定，下一次检查会得到同样结果，直到发布端放量。
        if self.deferred_by_rollout(&manifest)? {
            let percentage = self.rollout_percentage(&manifest).unwrap_or(0);
            crate::core::events::check(&manifest.version, true, false);
            emit(
                &mut on_event,
                UpdateEvent::Deferred {
                    version: manifest.version.clone(),
                    percentage,
                },
            );
            return Ok(UpdateOutcome::Deferred {
                version: manifest.version,
                percentage,
            });
        }

        let full_download_url = manifest
            .platforms
            .get(&self.config.platform)
            .map(|p| p.url.clone());

        // 适用边界：平台/布局不适合增量替换时回退全量（正确性优先）。
        // 判定发生在下载补丁之前——清单仍会拉取（要知道有无新版本与全量地址），
        // 但不会为用不上的补丁消耗任何流量。
        // 配置了整包清单时优先整包：它覆盖 sidecar/resources，单文件补丁做不到。
        let bundle = self.resolve_bundle_patch(&manifest.version).await?;

        // 只有完全没配置整包时才走单文件适用边界；整包不可用时
        // 由 `Unsupported` 自带的准确原因直接回退。
        if matches!(bundle, BundleResolution::NotConfigured) {
            if let Some(reason) =
                crate::platform::target::incremental_fallback_reason(&self.executable_path)
            {
                return Ok(fallback_to_full(
                    full_download_url,
                    FallbackReason::EnvironmentUnsupported(reason),
                    &mut on_event,
                ));
            }
        }

        match bundle {
            BundleResolution::Ready {
                manifest: bundle_manifest,
                patch,
            } => {
                return self
                    .run_bundle_patch(bundle_manifest, patch, launch, &mut on_event, cancel)
                    .await;
            }
            BundleResolution::Unsupported(detail) => {
                return Ok(fallback_to_full(
                    full_download_url,
                    FallbackReason::EnvironmentUnsupported(detail),
                    &mut on_event,
                ));
            }
            BundleResolution::NoPatch => {
                return Ok(fallback_to_full(
                    full_download_url,
                    FallbackReason::NoMatchingPatch {
                        from: self.config.current_version.clone(),
                        to: manifest.version.clone(),
                    },
                    &mut on_event,
                ));
            }
            BundleResolution::NotConfigured => {}
        }

        // 匹配补丁
        let Some(patch) = manifest.find_patch(&self.config.current_version, &self.config.platform)
        else {
            return Ok(fallback_to_full(
                full_download_url,
                FallbackReason::NoMatchingPatch {
                    from: self.config.current_version.clone(),
                    to: manifest.version.clone(),
                },
                &mut on_event,
            ));
        };

        crate::core::events::patch_matched(
            &patch.from_version,
            &patch.to_version,
            patch.patch_size,
        );

        // 下载补丁
        let patch_path = self.work_dir.join(crate::core::artifacts::PATCH_FILE);
        // 体积上限从清单的 patch_size 推导（清单签名覆盖了它，所以可信）：
        // 1 MB 余量足够吸收服务端的分块差异；响应体在传输中仍可能被替换，
        // 无上限的流式写入会灌满磁盘。
        // 清单未声明 patch_size 时退到固定硬上限，保证任何路径下都有约束。
        let max_bytes = patch
            .patch_size
            .map(|size| size.saturating_add(1024 * 1024))
            .unwrap_or(MAX_PATCH_BYTES);

        super::download_and_verify_patch(
            &self.client,
            &patch.patch_url,
            &patch_path,
            &self.work_dir.join(crate::core::artifacts::PATCH_META),
            &patch.patch_hash,
            max_bytes,
            "Patch",
            cancel,
            &mut on_event,
        )
        .await?;

        // 应用补丁，生成新版本（产物经双端哈希校验，类型上排除未验证字节）
        let staged_path = self.work_dir.join(crate::core::artifacts::staged_file());
        let _ = tokio::fs::remove_file(&staged_path).await;
        let artifact = apply_patch(
            &self.executable_path,
            &patch_path,
            &staged_path,
            &patch.patch_hash,
            &patch.target_hash,
        )
        .await?;

        // 清理补丁文件与其身份记录
        let _ = tokio::fs::remove_file(&patch_path).await;
        let _ =
            tokio::fs::remove_file(self.work_dir.join(crate::core::artifacts::PATCH_META)).await;

        // 脚本执行前最后一次确认产物仍是校验时的那份字节，
        // 封住"校验完成 → 脚本执行"之间的 TOCTOU 窗口。
        artifact.verify_integrity()?;

        // Authenticode 退化检查：原版本已签名而产物未签名时阻断。
        // 内容哈希正确不等于能启动——受管环境会拒绝未签名程序。
        PlatformUpdater::verify_patched_binary(&self.executable_path, artifact.path())?;

        // 生成替换脚本（只接受已验证产物；脚本放用户可写的工作目录）
        let script = PlatformUpdater::generate_updater_with(
            &self.executable_path,
            &artifact,
            &self.work_dir,
            self.script_timings(),
        )?;

        // 启动替换脚本之后就无法撤销了：这是最后一次取消窗口。
        if is_cancelled(cancel) {
            let _ = tokio::fs::remove_file(&staged_path).await;
            emit(&mut on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }

        // 本次更新成功，清除连续失败计数
        clear_failed_attempts(&self.work_dir).await;
        crate::core::events::patched(&manifest.version);

        let version = manifest.version.clone();
        let staged = Staged {
            staged_exe: artifact,
            script_path: script,
            manifest: Box::new(manifest),
            launched: std::sync::Arc::new(AtomicBool::new(false)),
        };

        // 按需启动替换脚本
        if launch {
            staged.launch()?;
        }

        emit(&mut on_event, UpdateEvent::Ready { version });

        Ok(UpdateOutcome::Patched { staged })
    }
}

/// 回退到全量下载：记录原因、发出阶段事件并构造结果。
///
/// 回退分支有四条（环境不适用、整包不适用、整包无补丁、单文件无补丁），
/// 走同一个出口，避免事件与日志在某个分支里漏发。
fn fallback_to_full<'a>(
    full_download_url: Option<String>,
    reason: FallbackReason,
    on_event: &mut Option<Box<dyn FnMut(UpdateEvent) + Send + 'a>>,
) -> UpdateOutcome {
    crate::core::events::fallback(&reason.to_string());
    emit(
        on_event,
        UpdateEvent::Fallback {
            url: full_download_url.clone(),
            reason: reason.clone(),
        },
    );
    UpdateOutcome::FallbackToFullDownload {
        full_download_url,
        reason,
    }
}
