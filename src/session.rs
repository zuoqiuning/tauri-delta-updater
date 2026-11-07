//! 可观察、可取消的更新会话。
//!
//! [`DeltaUpdater::install`] 把整个流程压成一次调用，适合"进度条 + 结果"的简单 UI；
//! 当 UI 需要知道**现在处在哪个阶段**、并且希望用户能中途取消时，用
//! [`DeltaUpdater::session`]：
//!
//! ```no_run
//! # async fn example() -> tauri_delta_updater::Result<()> {
//! # let updater: tauri_delta_updater::DeltaUpdater = unimplemented!();
//! let session = updater.session();
//! let token = session.cancel_token(); // 交给 UI/另一个任务
//!
//! let result = session
//!     .run(|event| println!("{event:?}"))
//!     .await?;
//! # let _ = (result, token);
//! # Ok(())
//! # }
//! ```
//!
//! 取消是协作式的：下载会立即中止并删除半截补丁，合并只能在其完成后检查，
//! 替换脚本一旦启动就无法撤销。

use crate::error::Result;
use crate::updater::{DeltaUpdater, FallbackReason, UpdateResult};
use serde::Serialize;

/// 更新过程中发出的阶段事件。
///
/// 事件按 `stage` 字段区分（`snake_case`），可直接序列化给前端。
/// 最后一次事件对应最终结果，宿主可以只监听事件而不读返回值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UpdateEvent {
    /// 正在拉取并验签清单。
    Checking,
    /// 正在下载补丁。`total` 未知时为 0。
    Downloading {
        /// 已下载字节数。
        downloaded: u64,
        /// 总字节数（未知时为 0）。
        total: u64,
    },
    /// 正在下载**全量安装包**（`download_full` 专用；补丁路径不会发这个事件）。
    DownloadingFull {
        /// 已下载字节数。
        downloaded: u64,
        /// 总字节数（未知时为 0）。
        total: u64,
    },
    /// 下载完成，正在合并补丁并校验哈希。
    Applying,
    /// 合并完成，替换脚本已启动（`auto_launch` 为 `false` 时表示产物已就绪）。
    Ready {
        /// 更新后的版本号。
        version: String,
    },
    /// 已是最新版本。
    UpToDate {
        /// 清单声明的版本号。
        version: String,
    },
    /// 新版本存在，但当前安装不在本次灰度范围内。
    Deferred {
        /// 清单声明的版本号。
        version: String,
        /// 发布端声明的放量比例（0–100）。
        percentage: u8,
    },
    /// 增量不可用，需要走全量下载。
    Fallback {
        /// 全量安装包地址（若有）。
        url: Option<String>,
        /// 回退原因。
        reason: FallbackReason,
    },
    /// 检测到上次未完成的整包替换，正在准备修复脚本。
    ///
    /// 宿主应尽快退出进程；脚本会还原旧文件并重新启动旧版本。
    Repairing {
        /// 修复原因（`repaired_partial_update` 等）。
        reason: String,
    },
    /// 连续失败已达上限，已进入安全模式。
    SafeMode,
    /// 更新已被取消，半截文件已清理。
    Cancelled,
}

/// 一次更新会话。
///
/// 由 [`DeltaUpdater::session`] 创建，持有取消令牌并保证
/// "收到 [`UpdateEvent::Ready`] 时替换脚本已按配置启动"。
pub struct UpdateSession<'a> {
    updater: &'a DeltaUpdater,
    cancel: crate::core::cancel::CancelToken,
}

impl<'a> UpdateSession<'a> {
    /// 为一个更新器创建会话。
    pub fn new(updater: &'a DeltaUpdater) -> Self {
        Self {
            updater,
            cancel: crate::core::cancel::CancelToken::new(),
        }
    }

    /// 获取可克隆的取消令牌，交给 UI 线程或另一个命令使用。
    pub fn cancel_token(&self) -> crate::core::cancel::CancelToken {
        self.cancel.clone()
    }

    /// 请求取消本次更新。等价于 `self.cancel_token().cancel()`。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// 运行会话，并把每个阶段回调给 `on_event`。
    ///
    /// 返回值语义与 [`DeltaUpdater::install`] 一致：`Updated` / `RepairRequired`
    /// 表示脚本已启动。
    /// 取消时返回 [`crate::DeltaError::Cancelled`]，且不计入安全模式的失败次数。
    ///
    /// 回调要求 `Send`：这样返回的 Future 才能放进 Tauri 的 async command。
    pub async fn run<F>(self, on_event: F) -> Result<UpdateResult>
    where
        F: FnMut(UpdateEvent) + Send,
    {
        self.updater
            .install_with_events(&self.cancel, on_event)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::UpdateEvent;

    /// 事件的前端形状是公开契约：统一用 `stage` 区分阶段。
    #[test]
    fn serializes_stage_contract() {
        use serde_json::json;

        assert_eq!(
            serde_json::to_value(UpdateEvent::Checking).unwrap(),
            json!({ "stage": "checking" })
        );
        assert_eq!(
            serde_json::to_value(UpdateEvent::Downloading {
                downloaded: 3,
                total: 9
            })
            .unwrap(),
            json!({ "stage": "downloading", "downloaded": 3, "total": 9 })
        );
        assert_eq!(
            serde_json::to_value(UpdateEvent::Ready {
                version: "1.2.3".into()
            })
            .unwrap(),
            json!({ "stage": "ready", "version": "1.2.3" })
        );
        assert_eq!(
            serde_json::to_value(UpdateEvent::Repairing {
                reason: "startup_timeout".into()
            })
            .unwrap(),
            json!({ "stage": "repairing", "reason": "startup_timeout" })
        );
        assert_eq!(
            serde_json::to_value(UpdateEvent::Deferred {
                version: "1.2.3".into(),
                percentage: 20
            })
            .unwrap(),
            json!({ "stage": "deferred", "version": "1.2.3", "percentage": 20 })
        );
    }
}
