//! 对外入口与流程编排：检查、更新、修复、安全模式与事件回调。

use super::bundle_flow::BundleResolution;
use super::diagnosis::read_rollback_reason;
use super::{
    emit, is_cancelled, CheckOutcome, DeltaUpdater, FallbackReason, StagedRepair, UpdateOutcome,
    UpdateResult,
};
use crate::core::lock::UpdateLock;
use crate::core::safety::{record_failed_attempt, should_abort};
use crate::error::{DeltaError, Result};
use crate::platform::PlatformUpdater;
use crate::session::UpdateEvent;
use std::sync::atomic::AtomicBool;

impl DeltaUpdater {
    /// 只检查是否有新版本，不下载补丁。
    ///
    /// 适合 UI 的"发现新版本"阶段：用户确认后再调用 [`DeltaUpdater::update`]。
    ///
    /// 同样会做签名校验、版本单调性判断与安全模式判断。
    ///
    /// 安全模式激活时返回 `Err(DeltaError::SafeMode)`——此时**连清单都不拉取**，
    /// 与 `update()` 返回 `Ok(UpdateOutcome::SafeMode)` 是同一语义的两种形态：
    /// 检查无法进行，而更新可以"执行并决定不更新"。
    pub async fn check(&self) -> Result<CheckOutcome> {
        crate::core::crypto::install_crypto_provider();

        if should_abort(&self.work_dir).await {
            self.emit_safe_mode().await;
            return Err(DeltaError::SafeMode);
        }

        let manifest = self.fetch_verified_manifest().await?;
        let needs_repair = self.needs_bundle_repair();

        let is_newer = manifest.is_newer_than(&self.config.current_version)?;
        // 灰度判定必须在任何补丁/整包清单请求之前：未命中的安装
        // 不应为一个自己拿不到的版本消耗流量。
        let rollout_percentage = if is_newer {
            self.rollout_percentage(&manifest)
        } else {
            None
        };
        let deferred_by_rollout = is_newer && self.deferred_by_rollout(&manifest)?;

        // 灰度未命中时连全量地址都不返回：否则宿主可能绕过比例直接下载安装包。
        let full_download_url = if deferred_by_rollout {
            None
        } else {
            manifest
                .platforms
                .get(&self.config.platform)
                .map(|p| p.url.clone())
        };
        // 配置了整包清单时以整包为准；否则沿用原来的单文件补丁。
        let bundle = if is_newer && !deferred_by_rollout {
            self.resolve_bundle_patch(&manifest.version).await?
        } else {
            BundleResolution::NotConfigured
        };
        // 只有“完全没配置整包”时才需要单文件适用边界：
        // 整包命中时 macOS `.app` 通过目录切换更新；整包不可用时
        // 由 `Unsupported` 自带更准确的原因，不要用单文件判定覆盖它。
        let environment = if is_newer
            && !deferred_by_rollout
            && matches!(bundle, BundleResolution::NotConfigured)
        {
            crate::platform::target::incremental_fallback_reason(&self.executable_path)
        } else {
            None
        };
        // 不是更新时不应展示补丁信息：清单里可能残留同版本的补丁记录
        let single_patch = if is_newer && !deferred_by_rollout {
            manifest.find_patch(&self.config.current_version, &self.config.platform)
        } else {
            None
        };

        let patch_size = match &bundle {
            BundleResolution::Ready { patch, .. } => patch.patch_size,
            BundleResolution::NotConfigured => single_patch.and_then(|p| p.patch_size),
            _ => None,
        };

        // 适用边界：平台/布局不适合增量时，UI 不应展示"有补丁可下载"。
        // 没有匹配补丁同理——两者都让 has_patch 为 false，但原因不同，
        // 必须都填进 fallback_reason，宿主才能给出准确提示。
        let fallback_reason = if !is_newer || deferred_by_rollout {
            None
        } else if let Some(detail) = environment {
            Some(FallbackReason::EnvironmentUnsupported(detail))
        } else {
            match &bundle {
                BundleResolution::Ready { .. } => None,
                BundleResolution::Unsupported(detail) => {
                    Some(FallbackReason::EnvironmentUnsupported(detail))
                }
                BundleResolution::NoPatch => Some(FallbackReason::NoMatchingPatch {
                    from: self.config.current_version.clone(),
                    to: manifest.version.clone(),
                }),
                BundleResolution::NotConfigured if single_patch.is_some() => None,
                BundleResolution::NotConfigured => Some(FallbackReason::NoMatchingPatch {
                    from: self.config.current_version.clone(),
                    to: manifest.version.clone(),
                }),
            }
        };
        let has_patch = is_newer && !deferred_by_rollout && fallback_reason.is_none();

        crate::core::events::check(&manifest.version, is_newer, has_patch);

        Ok(CheckOutcome {
            version: manifest.version.clone(),
            notes: manifest.notes.clone(),
            pub_date: manifest.pub_date.clone(),
            is_newer,
            has_patch,
            is_delta: has_patch,
            rollout_percentage,
            deferred_by_rollout,
            needs_repair,
            download_size: patch_size,
            patch_size,
            full_download_url,
            fallback_reason,
            manifest: Box::new(manifest),
        })
    }

    /// 执行完整的增量更新流程。
    ///
    /// 过程出错时会记录失败次数；连续失败达到上限后返回 [`UpdateOutcome::SafeMode`]。
    pub async fn update(&self) -> Result<UpdateOutcome> {
        self.run_entry(self.config.auto_launch, None, None, None)
            .await
    }

    /// 默认接入路径：面向 UI 的更新结果。
    ///
    /// 与 [`Self::update`] 相同，但把 `UpdateOutcome` 映射成
    /// [`UpdateResult`]——宿主只需要处理"更新了 / 已最新 / 需要全量 / 安全模式"，
    /// 不必理解补丁 URL、哈希与暂存文件。
    ///
    /// 返回 [`UpdateResult::Updated`] 或 [`UpdateResult::RepairRequired`] 时，
    /// 替换/修复脚本**一定已经启动**（即使配置了 `auto_launch(false)`，
    /// 本方法也会补上启动），宿主应立即退出进程。
    pub async fn install(&self) -> Result<UpdateResult> {
        let outcome = self.update().await?;
        self.finish(outcome)
    }

    /// 默认接入路径 + 下载进度回调。
    ///
    /// 与 [`Self::install`] 相同，只是额外接收 `(已下载, 总大小)`。
    /// 这是 UI 开发者最常用的形态：不需要理解 `UpdateOutcome`，
    /// 也不需要为了进度条掉到高级 API。
    ///
    /// 回调只覆盖**下载**阶段。`downloaded == total` 之后是合并与哈希校验
    /// （大文件可能耗时数秒），UI 可以据此切换到"正在安装"状态。
    ///
    /// 回调要求 `Send`：返回的 Future 才能放进 Tauri 的 async command。
    pub async fn install_with_progress<F>(&self, on_progress: F) -> Result<UpdateResult>
    where
        F: FnMut(u64, u64) + Send,
    {
        let outcome = self
            .run_entry(
                self.config.auto_launch,
                Some(Box::new(on_progress)),
                None,
                None,
            )
            .await?;
        self.finish(outcome)
    }

    /// 可观察、可取消的更新会话。
    ///
    /// 需要知道"当前处于哪个阶段"或允许用户中途取消时使用；
    /// 语义与 [`Self::install`] 一致（`Updated` / `RepairRequired` 表示脚本已启动）。
    pub fn session(&self) -> crate::session::UpdateSession<'_> {
        crate::session::UpdateSession::new(self)
    }

    /// 默认接入路径 + 阶段事件 + 取消。
    ///
    /// 事件按阶段回调（`Checking` / `Downloading` / `Applying` / `Ready` …），
    /// 取消通过 [`crate::CancelToken`] 触发，取消后返回 [`DeltaError::Cancelled`]
    /// 且不计入安全模式的失败次数。
    ///
    /// 回调要求 `Send`：返回的 Future 才能放进 Tauri 的 async command。
    pub async fn install_with_events<F>(
        &self,
        cancel: &crate::core::cancel::CancelToken,
        on_event: F,
    ) -> Result<UpdateResult>
    where
        F: FnMut(UpdateEvent) + Send,
    {
        let outcome = self
            .run_entry(
                self.config.auto_launch,
                None,
                Some(Box::new(on_event)),
                Some(cancel),
            )
            .await?;
        self.finish(outcome)
    }

    /// 默认接入路径的一步到位版本：更新成功后启动替换并退出进程。
    ///
    /// 其它结果（已最新 / 需要全量 / 安全模式）原样返回，由宿主决定下一步。
    ///
    /// # 与 single-instance 插件共存
    ///
    /// `std::process::exit(0)` 会跳过 `Drop`，单实例插件的 socket/lock 清理
    /// 依赖平台实现。**同时使用 `tauri-plugin-single-instance` 时，上线前必须
    /// 在每个目标平台实测**：旧实例锁残留会让重启的新版本被判定为第二实例。
    /// 需要可控退出时改用 [`Self::install_with_events`]，先发完最后一条事件
    /// 再调用宿主自己的退出逻辑（如 `app.exit(0)`）。
    pub async fn install_and_exit(&self) -> Result<UpdateResult> {
        let result = self.install().await?;
        if matches!(
            result,
            UpdateResult::Updated { .. } | UpdateResult::RepairRequired { .. }
        ) {
            std::process::exit(0);
        }
        Ok(result)
    }

    /// 把更新结果收敛成 UI 语义，并保证 `Updated` 意味着脚本已启动。
    fn finish(&self, outcome: UpdateOutcome) -> Result<UpdateResult> {
        match outcome {
            UpdateOutcome::Patched { staged } => {
                // auto_launch=false 时 update() 只合并、不启动；
                // install() 承诺"Updated 即可退出"，所以这里补上启动。
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                Ok(UpdateResult::Updated {
                    version: staged.manifest().version.clone(),
                })
            }
            UpdateOutcome::Bundled { staged } => {
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                Ok(UpdateResult::Updated {
                    version: staged.manifest().version.clone(),
                })
            }
            UpdateOutcome::Repair { staged, reason } => {
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                Ok(UpdateResult::RepairRequired { reason })
            }
            other => Ok(other.into()),
        }
    }

    /// 发出安全模式相关事件（计数 + 剩余冷却时间）。
    async fn emit_safe_mode(&self) {
        let count = crate::core::safety::get_failed_attempts(&self.work_dir).await;
        crate::core::events::safety_mode_entered(count);
        if let Some(remaining) = crate::core::safety::safe_mode_remaining(&self.work_dir).await {
            crate::core::events::cooldown_active(remaining);
        }
    }

    /// 执行更新，成功打过补丁后**直接退出进程**。
    ///
    /// 这是给"不想自己处理退出时机"的宿主的一步到位入口：
    /// 补丁就绪后启动替换脚本并 `std::process::exit(0)`，
    /// 从根上杜绝"主进程不退出 → 被回滚"这个最常见的接入错误。
    ///
    /// - `UpdateOutcome::Patched`：已退出，永远不会返回；
    /// - 其它结果：原样返回 `Ok(())`，由宿主决定全量下载或提示用户；
    /// - 过程出错：返回 `Err`，同样由宿主决定。
    ///
    /// # 注意
    ///
    /// `std::process::exit` 会跳过析构函数——在调用它之前先落盘需要持久化的状态。
    /// 需要在退出前通知前端/发送 IPC 的宿主，请改用 [`Self::update`] + `std::process::exit`。
    ///
    /// Windows 上若更新目录需要管理员权限，替换脚本会弹 UAC 授权框。
    /// 最好在调用本方法前告知用户"更新过程中可能出现授权提示"，
    /// 避免用户把弹窗当成恶意行为而点击拒绝。
    pub async fn update_and_exit(&self) -> Result<()> {
        crate::core::crypto::install_crypto_provider();

        match self.update().await? {
            UpdateOutcome::Patched { staged } => {
                // auto_launch=false 时 update() 没有启动脚本，这里补上
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                std::process::exit(0);
            }
            UpdateOutcome::Bundled { staged } => {
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                std::process::exit(0);
            }
            UpdateOutcome::Repair { staged, .. } => {
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                std::process::exit(0);
            }
            _ => Ok(()),
        }
    }

    /// 带进度回调的更新流程。
    ///
    /// `on_progress` 接收 `(已下载字节数, 总字节数)`，总字节数未知时为 0。
    /// 回调要求 `Send`：返回的 Future 才能放进 Tauri 的 async command。
    pub async fn update_with_progress<F>(&self, on_progress: F) -> Result<UpdateOutcome>
    where
        F: FnMut(u64, u64) + Send,
    {
        self.run_entry(
            self.config.auto_launch,
            Some(Box::new(on_progress)),
            None,
            None,
        )
        .await
    }

    /// 下载并合并补丁，**不启动替换脚本**。
    ///
    /// 宿主可以在退出前先展示提示、发送 IPC 消息，再调用
    /// [`Staged::launch`] 触发替换，避免"脚本已启动但 UI 还没弹出来"的竞态。
    pub async fn prepare(&self) -> Result<UpdateOutcome> {
        self.run_entry(false, None, None, None).await
    }

    /// 标记新版本启动成功（使用本更新器的工作目录）。
    ///
    /// 新版本必须在启动早期调用一次（或调用静态的
    /// [`PlatformUpdater::confirm_startup_success`]），
    /// 否则替换脚本会误判失败并回滚。
    pub fn confirm_startup_success(&self) -> Result<()> {
        if let Some(dir) = self.executable_path.parent() {
            let _ = std::fs::remove_file(dir.join(crate::core::artifacts::WORK_DIR_MARKER));
        }
        PlatformUpdater::confirm_startup_success_in(&self.work_dir)
    }

    /// 带健康检查的启动确认（使用本更新器的工作目录）。
    ///
    /// 只有 `health` 返回 `Ok` 才写成功标志；失败时写
    /// `health_check_failed` 回滚原因，替换脚本会尽快回滚。
    ///
    /// ```no_run
    /// # async fn example() -> tauri_delta_updater::Result<()> {
    /// # let updater: tauri_delta_updater::DeltaUpdater = unimplemented!();
    /// updater.confirm_startup_success_with(|| {
    ///     // 数据库可连接、配置可加载、关键依赖版本匹配……
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn confirm_startup_success_with<F>(&self, health: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        let marker = self
            .executable_path
            .parent()
            .map(|dir| dir.join(crate::core::artifacts::WORK_DIR_MARKER))
            .filter(|marker| marker.is_file());
        crate::platform::shared::confirm_startup_success_with_in(
            &self.work_dir,
            marker.as_deref(),
            health,
        )
    }

    /// 仅在本次启动来自一次替换时确认成功；普通启动是零写入的空操作。
    ///
    /// 建议无条件下放在应用启动早期：
    ///
    /// ```no_run
    /// # fn main() -> tauri_delta_updater::Result<()> {
    /// let confirmed = tauri_delta_updater::PlatformUpdater::confirm_startup_success_if_pending()?;
    /// # let _ = confirmed;
    /// # Ok(())
    /// # }
    /// ```
    pub fn confirm_startup_success_if_pending(&self) -> Result<bool> {
        let Some(dir) = self.executable_path.parent() else {
            return Ok(false);
        };
        if !dir.join(crate::core::artifacts::WORK_DIR_MARKER).is_file() {
            return Ok(false);
        }
        self.confirm_startup_success()?;
        Ok(true)
    }

    /// 手动退出安全模式（宿主提供"重试"按钮时使用）。
    pub async fn reset_safety(&self) {
        crate::core::safety::reset_safety(&self.work_dir).await;
    }

    /// 统一入口：安全模式检查 → 回调归一化 → 持锁执行。
    ///
    /// - `on_progress`：只关心字节数的简单 UI；
    /// - `on_event`：需要阶段事件的会话/UI；
    /// - 两者同时提供时以 `on_event` 为准，进度由事件的 `Downloading` 承载。
    async fn run_entry(
        &self,
        launch: bool,
        on_progress: Option<Box<dyn FnMut(u64, u64) + Send + '_>>,
        on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>>,
        cancel: Option<&crate::core::cancel::CancelToken>,
    ) -> Result<UpdateOutcome> {
        crate::core::crypto::install_crypto_provider();

        // 只保留一个事件回调：纯进度回调被适配成 Downloading 事件，
        // 内部各阶段只需要调用一种回调，避免两套通知逻辑漂移。
        let mut on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>> =
            match (on_event, on_progress) {
                (Some(cb), _) => Some(cb),
                (None, Some(mut progress)) => Some(Box::new(move |event: UpdateEvent| {
                    if let UpdateEvent::Downloading { downloaded, total } = event {
                        progress(downloaded, total);
                    }
                })),
                (None, None) => None,
            };

        // 修复优先于安全模式：备份仍在说明安装目录处于中间状态，
        // 还原它是恢复而不是一次新的更新尝试，不应被失败计数挡住。
        if self.needs_bundle_repair() {
            return self.run_repair(launch, on_event, cancel).await;
        }

        if should_abort(&self.work_dir).await {
            self.emit_safe_mode().await;
            if let Some(cb) = on_event.as_mut() {
                cb(UpdateEvent::SafeMode);
            }
            return Ok(UpdateOutcome::SafeMode);
        }

        if is_cancelled(cancel) {
            if let Some(cb) = on_event.as_mut() {
                cb(UpdateEvent::Cancelled);
            }
            return Err(DeltaError::Cancelled);
        }

        self.run_prepare(launch, on_event, cancel).await
    }

    /// 是否存在上次中断、尚未还原的整包替换现场。
    pub(super) fn needs_bundle_repair(&self) -> bool {
        PlatformUpdater::bundle_repair_needed(&self.executable_path, &self.work_dir)
    }

    /// 生成并（按配置）启动整包修复脚本。
    async fn run_repair(
        &self,
        launch: bool,
        mut on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>>,
        cancel: Option<&crate::core::cancel::CancelToken>,
    ) -> Result<UpdateOutcome> {
        let reason = read_rollback_reason(&self.work_dir)
            .unwrap_or_else(|| "repaired_partial_update".to_string());
        if is_cancelled(cancel) {
            emit(&mut on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }
        emit(
            &mut on_event,
            UpdateEvent::Repairing {
                reason: reason.clone(),
            },
        );

        // 修复同样要持锁：它与更新会写同一批文件。
        let _lock = UpdateLock::acquire_in(&self.work_dir)?;
        let script = PlatformUpdater::generate_bundle_repair_with(
            &self.executable_path,
            &self.work_dir,
            self.script_timings(),
        )?;
        let staged = StagedRepair {
            script_path: script,
            reason: reason.clone(),
            launched: std::sync::Arc::new(AtomicBool::new(false)),
        };
        if launch {
            staged.launch()?;
        }
        Ok(UpdateOutcome::Repair { staged, reason })
    }

    /// 核心流程。
    ///
    /// - `launch`：结束时是否启动替换脚本
    /// - `on_event`：可选的阶段事件回调
    /// - `cancel`：可选的取消令牌
    async fn run_prepare(
        &self,
        launch: bool,
        on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>>,
        cancel: Option<&crate::core::cancel::CancelToken>,
    ) -> Result<UpdateOutcome> {
        // 互斥锁：防止多个更新进程同时改写文件。
        // 失败计数也在锁内记录，避免并发时的读-改-写竞争丢计数。
        let _lock = UpdateLock::acquire_in(&self.work_dir)?;

        let result = self.run_locked(launch, on_event, cancel).await;

        // 暂态网络错误（Timeout/Network/5xx）不算"更新流程坏了"，重试即可；
        // 只有确定性错误（验签失败、哈希不符、磁盘不足等）才计入安全模式，
        // 否则服务器抖动几次就会把用户锁进安全模式。
        // 用户主动取消同理：不是故障，不能记为失败。
        if let Err(e) = &result {
            crate::core::events::patch_rejected(&e.to_string());
            if !e.is_retryable() && !matches!(e, DeltaError::Cancelled) {
                let _ = record_failed_attempt(&self.work_dir).await;
            }
        }

        result
    }
}
