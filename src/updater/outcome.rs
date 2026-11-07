//! 对宿主可见的结果类型：检查结果、更新结果与回退原因。

use crate::models::UpdateManifest;
use serde::Serialize;

use super::{FullDownload, Staged, StagedBundle, StagedRepair};

/// 增量不可用、需要回退全量下载的原因。
///
/// 比自由字符串更适合 UI 分支与本地化：宿主可以 `match` 出具体原因，
/// 而不是对中文文案做字符串包含判断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FallbackReason {
    /// 清单里没有从当前版本到目标版本的补丁（跨版本升级或首次安装）。
    NoMatchingPatch {
        /// 当前版本。
        from: String,
        /// 目标版本。
        to: String,
    },
    /// 当前运行环境不适合原地替换（只读的 AppImage 安装目录、macOS `.app` bundle 等）。
    EnvironmentUnsupported(&'static str),
}

impl std::fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FallbackReason::NoMatchingPatch { from, to } => write!(
                f,
                "清单里没有从 {from} 到 {to} 的增量补丁（跨版本升级或首次安装）"
            ),
            FallbackReason::EnvironmentUnsupported(detail) => f.write_str(detail),
        }
    }
}

/// 检查更新的结果。
///
/// 只拉取并验签清单，不会下载补丁。
#[derive(Debug, Clone, Serialize)]
pub struct CheckOutcome {
    /// 清单里的目标版本号。
    pub version: String,
    /// 更新说明。
    pub notes: String,
    /// 发布时间。
    pub pub_date: String,
    /// 目标版本是否**严格高于**当前版本。
    ///
    /// 防降级的核心判据：清单签名只证明"出自发布者"，不证明"比当前新"。
    /// 为 `false` 时宿主应展示"已是最新"，而不是提示更新。
    pub is_newer: bool,
    /// 当前版本是否有可用补丁。
    pub has_patch: bool,
    /// 本次是否会走增量路径（`has_patch` 的 UI 语义别名）。
    pub is_delta: bool,
    /// 清单给当前平台声明的灰度比例（0–100）。
    ///
    /// 旧清单没有 `rollout` 时为 `None`，等价于全量放行。
    pub rollout_percentage: Option<u8>,
    /// 本次版本虽然更新，但当前安装未落在灰度范围内。
    ///
    /// 为 `true` 时 `is_newer` 仍为 `true`（确实存在新版本），
    /// 但 `has_patch` / `is_delta` 为 `false`，宿主不应触发任何下载。
    pub deferred_by_rollout: bool,
    /// 实际会下载的体积（增量时为补丁体积；全量时为 `None`）。
    pub download_size: Option<u64>,
    /// 补丁文件大小（字节）。清单未提供时为 `None`。
    pub patch_size: Option<u64>,
    /// 上次整包替换未完成，需要先修复（还原备份）再重试更新。
    ///
    /// 为 `true` 时本轮的 `has_patch` / 版本信息仅供参考：安装目录处于
    /// 中间状态，`update()` 会先返回修复结果。
    pub needs_repair: bool,
    /// 全量安装包地址，用于回退到 `tauri-plugin-updater`。
    pub full_download_url: Option<String>,
    /// 已验签的清单，便于展示更多信息。
    pub manifest: Box<UpdateManifest>,
    /// 增量不可用的原因。
    ///
    /// - `is_newer == false` 时为 `None`（本来就不需要更新）；
    /// - 没有匹配补丁 → [`FallbackReason::NoMatchingPatch`]；
    /// - 环境不适合原地替换 → [`FallbackReason::EnvironmentUnsupported`]；
    /// - 为 `None` 且 `is_newer == true` 表示可以走增量路径。
    pub fallback_reason: Option<FallbackReason>,
}

impl CheckOutcome {
    /// 当前平台的全量安装包信息。没有对应平台条目时为 `None`。
    ///
    /// 用于增量不可用（无匹配补丁、只读 AppImage、macOS bundle、安全模式）时，
    /// 把 `{version, url, signature}` 原样交给官方 updater 或自定义安装流程。
    pub fn full_download(&self) -> Option<FullDownload> {
        self.full_download_for(crate::platform::target::current_platform())
    }

    /// 指定平台的全量安装包信息。
    ///
    /// 清单里的 `platforms` 键是发布者写的 target 名（如
    /// `windows-x86_64`），与 [`CheckOutcome::full_download`] 的当前构建平台
    /// 在跨平台发布场景下可能不同，因此需要显式指定。
    pub fn full_download_for(&self, platform: &str) -> Option<FullDownload> {
        FullDownload::from_manifest(&self.manifest, platform)
    }

    /// 补丁体积占完整安装包的比例（0.0–1.0）。
    ///
    /// **数值越低，增量收益越大**：`0.1` 表示补丁只有全量的 10%；
    /// `1.0` 表示补丁接近全量，下载完整安装包可能更快。
    /// 依赖发布端写入的 `platforms[].size`；旧清单没有该字段时返回 `None`。
    pub fn patch_size_ratio(&self) -> Option<f64> {
        self.patch_size_ratio_for(crate::platform::target::current_platform())
    }

    /// 指定平台的补丁体积占比。没有补丁大小或安装包大小时返回 `None`。
    pub fn patch_size_ratio_for(&self, platform: &str) -> Option<f64> {
        let patch = self.patch_size?;
        let full = self.manifest.platforms.get(platform)?.size?;
        (full > 0).then(|| patch as f64 / full as f64)
    }
}

/// 一次更新的结果。
#[derive(Debug)]
#[non_exhaustive]
pub enum UpdateOutcome {
    /// 已通过增量补丁生成新版本。
    ///
    /// 若 `auto_launch` 为 `true`（默认），替换脚本已经启动；
    /// 否则需要宿主调用 [`Staged::launch`]。
    ///
    /// # 调用方必须让当前进程退出
    ///
    /// 替换脚本会等待旧进程退出后才替换可执行文件。
    /// **若主进程不退出，脚本会等待超时后判定启动失败并回滚**，
    /// 用户会看到"更新了又退回旧版本"。
    ///
    /// 正确做法：
    ///
    /// ```no_run
    /// # async fn example() -> tauri_delta_updater::Result<()> {
    /// # let updater = tauri_delta_updater::DeltaUpdater::new(
    /// #     tauri_delta_updater::UpdateConfig::new("https://cdn/latest.json", "PUBKEY"),
    /// # )?;
    /// if let tauri_delta_updater::UpdateOutcome::Patched { staged } = updater.update().await? {
    ///     // 先通知前端，再退出进程
    ///     let _ = staged.script_path();
    ///     std::process::exit(0);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    Patched {
        /// 已就绪的更新产物。
        staged: Staged,
    },
    /// 已通过整包（多文件）增量生成新版本。
    ///
    /// 与 [`UpdateOutcome::Patched`] 的区别是替换范围：整包补丁同时覆盖
    /// 主程序、sidecar 与 resources。退出进程的要求完全相同——
    /// 替换脚本会等待旧进程退出后才开始改写文件。
    Bundled {
        /// 已就绪的整包更新产物。
        staged: StagedBundle,
    },
    /// 上次整包替换未完成，已生成修复脚本。
    ///
    /// 修复脚本会还原备份并重新启动旧版本；**本次没有发生版本更新**。
    /// 宿主在 `auto_launch` 为 `true` 时也必须尽快退出进程。
    Repair {
        /// 已就绪的修复产物。
        staged: StagedRepair,
        /// 修复原因（上次回滚原因或 `repaired_partial_update`）。
        reason: String,
    },
    /// 已经是最新版本（或清单版本低于当前版本，按"不更新"处理）。
    ///
    /// 这是防降级的关键分支：重放一份旧的、签名合法的清单时，
    /// 必须落到这里，绝不能返回带旧安装包地址的回退分支。
    UpToDate {
        /// 清单声明的版本号。
        version: String,
    },
    /// 新版本存在，但当前安装不在本次灰度范围内。
    ///
    /// 不是错误，也不是"已是最新"：发布端尚未把这一比例放量到当前安装。
    /// 同一版本的桶位是稳定的，所以下一次检查结果不变；等发布端提高比例
    /// 或发布新版本重新分桶后才会更新。
    Deferred {
        /// 清单声明的版本号。
        version: String,
        /// 发布端声明的放量比例（0–100）。
        percentage: u8,
    },
    /// 增量不可用，调用方应回退到全量下载（交回官方 updater）。
    ///
    /// 这不是错误，是本库"完整更新引擎"定位的正常分支：
    ///
    /// - 清单里没有匹配当前版本的补丁（跨版本升级、首次安装）；
    /// - 当前平台/布局不适合增量替换（只读 AppImage 目录、macOS `.app` bundle 等）。
    ///
    /// `reason` 是结构化的回退原因，可直接 `match` 或展示。
    FallbackToFullDownload {
        /// 清单里指向的完整安装包地址（若有）。
        full_download_url: Option<String>,
        /// 回退原因。
        reason: FallbackReason,
    },
    /// 连续失败次数已达上限，已停止尝试增量更新。
    ///
    /// 调用方应提示用户手动下载安装包。
    ///
    /// 注意语义边界：触发原因可能是安全阻断（签名/哈希失败），
    /// 也可能是持续性环境问题（磁盘满、权限不足）。
    /// 展示 UI 时用 [`DeltaError::is_security_blocking()`] 区分，
    /// 不要把所有 SafeMode 都当成安全风险提示。
    SafeMode,
}

/// 面向 UI 的更新结果。
///
/// 这是默认接入路径（[`DeltaUpdater::install`]）的返回值：
/// 只需要区分"更新了 / 已最新 / 需要全量 / 安全模式"，
/// 不需要理解补丁 URL、哈希或暂存文件。
///
/// 需要完全控制流程时，仍可使用 [`DeltaUpdater::update`] 返回的
/// [`UpdateOutcome`]。
///
/// # 序列化
///
/// 变体按 `kind` 区分（`snake_case`），可直接从 Tauri command 返回给前端：
///
/// ```json
/// {"kind":"updated","version":"1.2.3"}
/// {"kind":"repair_required","reason":"repaired_partial_update"}
/// {"kind":"up_to_date","version":"1.2.3"}
/// {"kind":"deferred","version":"1.2.3","percentage":20}
/// {"kind":"requires_full_download","url":"https://...","reason":{"no_matching_patch":{"from":"1.2.3","to":"1.3.0"}}}
/// {"kind":"safe_mode"}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UpdateResult {
    /// 已通过增量补丁更新。
    Updated {
        /// 更新后的版本号。
        version: String,
    },
    /// 上次整包替换未完成，修复脚本已就绪（或已启动）。
    ///
    /// 宿主应立即退出进程；重启后仍是原版本，可以重新尝试更新。
    RepairRequired {
        /// 修复原因。
        reason: String,
    },
    /// 已经是最新版本。
    UpToDate {
        /// 清单声明的版本号。
        version: String,
    },
    /// 新版本存在，但当前安装不在灰度范围内。
    ///
    /// UI 应展示"正在灰度放量，稍后自动更新"，而不是"已是最新"或错误。
    Deferred {
        /// 清单声明的版本号。
        version: String,
        /// 发布端声明的放量比例（0–100）。
        percentage: u8,
    },
    /// 需要走全量下载（宿主可交给官方 updater）。
    RequiresFullDownload {
        /// 全量安装包地址（若有）。
        url: Option<String>,
        /// 回退原因。
        reason: FallbackReason,
    },
    /// 连续失败已达上限，已进入安全模式。
    SafeMode,
}

/// UI 检查结果的语义别名。
pub type UpdateCheck = CheckOutcome;

impl From<UpdateOutcome> for UpdateResult {
    fn from(outcome: UpdateOutcome) -> Self {
        match outcome {
            UpdateOutcome::Patched { staged } => UpdateResult::Updated {
                version: staged.manifest().version.clone(),
            },
            UpdateOutcome::Bundled { staged } => UpdateResult::Updated {
                version: staged.manifest().version.clone(),
            },
            UpdateOutcome::Repair { reason, .. } => UpdateResult::RepairRequired { reason },
            UpdateOutcome::UpToDate { version } => UpdateResult::UpToDate { version },
            UpdateOutcome::Deferred {
                version,
                percentage,
            } => UpdateResult::Deferred {
                version,
                percentage,
            },
            UpdateOutcome::FallbackToFullDownload {
                full_download_url,
                reason,
            } => UpdateResult::RequiresFullDownload {
                url: full_download_url,
                reason,
            },
            UpdateOutcome::SafeMode => UpdateResult::SafeMode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CheckOutcome;
    use crate::models::UpdateManifest;

    fn outcome(patch_size: Option<u64>, installer_size: Option<u64>) -> CheckOutcome {
        let mut platform = serde_json::json!({
            "url": "https://cdn.example.com/app.exe",
            "signature": "sig"
        });
        if let Some(size) = installer_size {
            platform["size"] = size.into();
        }
        let manifest: UpdateManifest = serde_json::from_value(serde_json::json!({
            "version": "1.2.3",
            "platforms": { "windows-x86_64": platform }
        }))
        .unwrap();

        CheckOutcome {
            version: "1.2.3".into(),
            notes: String::new(),
            pub_date: String::new(),
            is_newer: true,
            has_patch: patch_size.is_some(),
            is_delta: patch_size.is_some(),
            rollout_percentage: None,
            deferred_by_rollout: false,
            download_size: patch_size,
            patch_size,
            needs_repair: false,
            full_download_url: Some("https://cdn.example.com/app.exe".into()),
            manifest: Box::new(manifest),
            fallback_reason: None,
        }
    }

    /// 占比来自清单签名覆盖的安装包体积，而不是客户端猜测。
    #[test]
    fn patch_size_ratio_uses_signed_installer_size() {
        assert_eq!(
            outcome(Some(1024), Some(10240)).patch_size_ratio_for("windows-x86_64"),
            Some(0.1)
        );
    }

    /// 旧清单没有 `platforms[].size` 时返回 `None`，UI 不展示伪指标。
    #[test]
    fn patch_size_ratio_is_unknown_without_size() {
        assert_eq!(
            outcome(Some(1024), None).patch_size_ratio_for("windows-x86_64"),
            None
        );
        assert_eq!(
            outcome(None, Some(10240)).patch_size_ratio_for("windows-x86_64"),
            None
        );
        assert_eq!(
            outcome(Some(1024), Some(0)).patch_size_ratio_for("windows-x86_64"),
            None
        );
    }
}
