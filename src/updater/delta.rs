//! 单文件增量流程：下载补丁、合并、校验并生成替换脚本。

use super::bundle_flow::BundleResolution;
use super::{emit, is_cancelled, DeltaUpdater, FallbackReason, Staged, UpdateEvent, UpdateOutcome};
use crate::core::diff::apply_patch;
use crate::core::disk::cleanup_previous_updates_except;
use crate::core::safety::clear_failed_attempts;
use crate::error::{DeltaError, Result};
use crate::platform::PlatformUpdater;
use std::path::{Path, PathBuf};
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
            crate::core::events::check(
                &manifest.version,
                false,
                false,
                self.config.uses_insecure_transport(),
            );
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

        // 系统版本门槛：低于清单要求时在**任何下载之前**终止，
        // 继续下载只会得到装不上、或替换后无法启动的版本。
        if let Some((required, current)) = self.unsupported_os(&manifest) {
            crate::core::events::check(
                &manifest.version,
                true,
                false,
                self.config.uses_insecure_transport(),
            );
            return Ok(UpdateOutcome::UnsupportedOs { required, current });
        }

        // 灰度：未命中的安装不进入任何下载/合并路径。
        // 同一版本的桶位稳定，下一次检查会得到同样结果，直到发布端放量。
        if self.deferred_by_rollout(&manifest)? {
            let percentage = self.rollout_percentage(&manifest).unwrap_or(0);
            crate::core::events::check(
                &manifest.version,
                true,
                false,
                self.config.uses_insecure_transport(),
            );
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

        // 清理上一轮遗留的临时文件。
        //
        // 放在版本/灰度判断之后：`UpToDate` / `Deferred` 属于"没有新东西可做"，
        // 此时清理会连上次 `prepare` 落盘的待安装产物一起删掉——一次网络故障或
        // 灰度未命中的检查就能让已就绪的更新消失。真正要下载新产物时才清理。
        //
        // pending 引用的脚本与产物必须保留：新补丁下载失败时，用户仍能安装
        // 上一轮已经就绪的版本，而不是"新版本没下到、旧产物也被删了"。
        // 同步读取：清理阶段不需要完整校验，也不该放大本函数的 async 状态机。
        let pending_paths = self.referenced_paths_sync();
        cleanup_previous_updates_except(&self.work_dir, &self.executable_path, &pending_paths)
            .await;

        // 整包优先：它覆盖 sidecar/resources，单文件补丁做不到。
        // 配置了整包清单但当前平台不支持整包替换时（Linux、非 .app 的 macOS），
        // 不能直接回退全量——单文件补丁仍然可用，先记下原因继续尝试。
        let bundle_unsupported = match self.resolve_bundle_patch(&manifest.version).await? {
            BundleResolution::Ready {
                manifest: bundle_manifest,
                patch,
            } => {
                return self
                    .run_bundle_patch(bundle_manifest, patch, launch, &mut on_event, cancel)
                    .await;
            }
            BundleResolution::Unsupported(detail) => Some(detail),
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
            BundleResolution::NotConfigured => None,
        };

        // 单文件适用边界：平台/布局不适合原地替换时回退全量（正确性优先）。
        // 判定发生在下载补丁之前——清单仍会拉取（要知道有无新版本与全量地址），
        // 但不会为用不上的补丁消耗任何流量。
        if let Some(reason) =
            crate::platform::target::incremental_fallback_reason(&self.executable_path)
        {
            return Ok(fallback_to_full(
                full_download_url,
                FallbackReason::EnvironmentUnsupported(reason),
                &mut on_event,
            ));
        }

        // 规划补丁路径：直达优先，其次最多 MAX_CHAIN_HOPS 跳；
        // 链的总成本不划算时返回 None，直接走全量。
        let Some(chain) = super::plan::plan_single(
            &manifest,
            &self.config.platform,
            &self.config.current_version,
        ) else {
            // 整包不支持且单文件也没补丁时，报整包原因更准确：
            // 用户会知道"这台机器本来就不支持整包"，而不是"发布端漏了补丁"。
            let reason = bundle_unsupported
                .map(FallbackReason::EnvironmentUnsupported)
                .unwrap_or(FallbackReason::NoMatchingPatch {
                    from: self.config.current_version.clone(),
                    to: manifest.version.clone(),
                });
            return Ok(fallback_to_full(full_download_url, reason, &mut on_event));
        };

        // 下载并逐跳应用。每跳复用同一个补丁文件名，应用后立即校验**中间产物**
        // 的哈希（每个 `target_hash` 都在签名清单里）——任何一跳不符都中止，
        // 不能把未经验证的字节当作下一跳的输入。
        let patch_path = self.work_dir.join(crate::core::artifacts::PATCH_FILE);
        let staged_path = self.work_dir.join(crate::core::artifacts::staged_file());
        let hop_a = self.work_dir.join(crate::core::artifacts::STAGED_HOP_A);
        let hop_b = self.work_dir.join(crate::core::artifacts::STAGED_HOP_B);
        // 旧 pending 引用同一个暂存路径：先复制一份保底副本，失败时还原。
        // `apply_patch` 会覆盖该路径，不先备份就等于"下载失败也删掉旧产物"。
        let mut pending_guard = if pending_paths.iter().any(|path| path == &staged_path) {
            PendingArtifactGuard::new(
                &staged_path,
                self.work_dir.join(crate::core::artifacts::STAGED_BACKUP),
            )
        } else {
            None
        };
        let _ = tokio::fs::remove_file(&staged_path).await;
        let _ = tokio::fs::remove_file(&hop_a).await;
        let _ = tokio::fs::remove_file(&hop_b).await;

        let last = chain.len() - 1;
        let mut input = self.executable_path.clone();
        let mut artifact = None;
        for (index, patch) in chain.iter().enumerate() {
            crate::core::events::patch_matched(
                &patch.from_version,
                &patch.to_version,
                patch.patch_size,
            );

            // 体积上限从清单的 patch_size 推导（清单签名覆盖了它，所以可信）：
            // 1 MB 余量足够吸收服务端的分块差异；响应体在传输中仍可能被替换，
            // 无上限的流式写入会灌满磁盘。
            // 清单未声明 patch_size 时退到固定硬上限，保证任何路径下都有约束。
            let max_bytes = patch
                .patch_size
                .map(|size| size.saturating_add(1024 * 1024))
                .unwrap_or(MAX_PATCH_BYTES);

            let hop_label = format!(
                "第 {}/{} 跳（{} → {}）",
                index + 1,
                chain.len(),
                patch.from_version,
                patch.to_version
            );
            let hop_error = |source: DeltaError| {
                if chain.len() == 1 {
                    // 直达补丁保持原有错误，不额外包装。
                    return source;
                }
                DeltaError::ChainHopFailed {
                    hop: index + 1,
                    total: chain.len(),
                    from: patch.from_version.clone(),
                    to: patch.to_version.clone(),
                    source: Box::new(source),
                }
            };

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
            .await
            .map_err(|source| {
                crate::core::events::patch_rejected(&format!("{hop_label}: {source}"));
                hop_error(source)
            })?;

            // 中间产物交替写入两个临时文件，最后一跳写最终暂存路径：
            // apply_patch 读输入、写输出，输入与输出不能是同一个文件。
            let output = if index == last {
                staged_path.clone()
            } else if index % 2 == 0 {
                hop_a.clone()
            } else {
                hop_b.clone()
            };
            let _ = tokio::fs::remove_file(&output).await;

            artifact = match apply_patch(
                &input,
                &patch_path,
                &output,
                &patch.patch_hash,
                &patch.target_hash,
            )
            .await
            {
                Ok(verified) => Some(verified),
                Err(source) => {
                    crate::core::events::patch_rejected(&format!("{hop_label}: {source}"));
                    return Err(hop_error(source));
                }
            };
            input = output;

            // 清理补丁文件与其身份记录
            let _ = tokio::fs::remove_file(&patch_path).await;
            let _ = tokio::fs::remove_file(self.work_dir.join(crate::core::artifacts::PATCH_META))
                .await;

            // 阶段事件：该跳产物已就绪。主进程退出后仍可判断"下载/合并走到哪一步"。
            crate::core::artifacts::append_event(
                &self.work_dir,
                crate::core::artifacts::EVENT_DOWNLOAD_COMPLETED,
            );
        }
        let _ = tokio::fs::remove_file(&hop_a).await;
        let _ = tokio::fs::remove_file(&hop_b).await;

        let artifact = artifact.ok_or_else(|| {
            DeltaError::PatchApplyFailed("补丁链为空：规划阶段保证至少有一跳".into())
        })?;

        // 脚本执行前最后一次确认产物仍是校验时的那份字节，
        // 封住"校验完成 → 脚本执行"之间的 TOCTOU 窗口。
        // 大文件哈希放 blocking 线程，避免占住 tokio worker。
        let artifact_for_verify = artifact.clone();
        tokio::task::spawn_blocking(move || artifact_for_verify.verify_integrity())
            .await
            .map_err(|e| DeltaError::WorkerFailed(format!("完整性校验线程失败: {e}")))??;

        // Authenticode 退化检查：原版本已签名而产物未签名时阻断。
        // 内容哈希正确不等于能启动——受管环境会拒绝未签名程序。
        // WinVerifyTrust 是同步系统调用，同样放 blocking 线程。
        let original = self.executable_path.clone();
        let staged = artifact.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            PlatformUpdater::verify_patched_binary(&original, &staged)
        })
        .await
        .map_err(|e| DeltaError::WorkerFailed(format!("Authenticode 校验线程失败: {e}")))??;

        // 生成替换脚本（只接受已验证产物；脚本放用户可写的工作目录）
        let script = PlatformUpdater::generate_updater_with_notice(
            &self.executable_path,
            &artifact,
            &self.work_dir,
            self.script_timings(),
            self.config.rollback_notice,
        )?;

        // 阶段事件：脚本已生成，接下来只剩宿主退出与脚本执行。
        crate::core::artifacts::append_event(
            &self.work_dir,
            crate::core::artifacts::EVENT_SCRIPT_READY,
        );

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

        // 新产物已就绪：旧记录（如有）作废，避免 auto_launch 路径下
        // 下次启动重复安装上一版产物。
        if let Some(guard) = pending_guard.as_mut() {
            guard.commit();
        }
        self.clear_pending_record();

        // 按需启动替换脚本；不立即启动时落盘待安装记录，供下次启动消费。
        if launch {
            staged.launch()?;
        } else {
            let artifact = super::pending::PendingArtifact::Exe {
                staged_exe: staged.staged_exe.path().to_path_buf(),
                target_hash: staged.staged_exe.sha256().to_string(),
            };
            self.store_pending(&version, &staged.script_path, artifact)
                .await?;
        }

        emit(&mut on_event, UpdateEvent::Ready { version });

        Ok(UpdateOutcome::Patched { staged })
    }
}

/// 旧待安装产物的保底副本：新更新失败时还原，成功时丢弃。
///
/// `apply_patch` 的最后一跳会覆盖暂存路径；如果它属于一条 pending 记录，
/// 失败时必须把旧产物放回去，否则用户"新版本没下到、旧产物也没了"。
struct PendingArtifactGuard {
    original: PathBuf,
    keep: PathBuf,
    committed: bool,
}

impl PendingArtifactGuard {
    fn new(original: &Path, keep: PathBuf) -> Option<Self> {
        if !original.is_file() {
            return None;
        }
        let _ = std::fs::remove_file(&keep);
        std::fs::copy(original, &keep).ok()?;
        Some(Self {
            original: original.to_path_buf(),
            keep,
            committed: false,
        })
    }

    fn commit(&mut self) {
        self.committed = true;
        let _ = std::fs::remove_file(&self.keep);
    }
}

impl Drop for PendingArtifactGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if self.keep.is_file() {
            let _ = std::fs::remove_file(&self.original);
            let _ = std::fs::rename(&self.keep, &self.original);
        }
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
