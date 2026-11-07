//! 命令返回给前端的 DTO：稳定错误码与全量下载结果。

use serde::Serialize;
use tauri_delta_updater::{DeltaError, FullDownloadOutcome};

/// 命令失败时返回给前端的结构。
#[derive(Debug, Clone, Serialize)]
pub struct ErrorDto {
    /// 稳定错误码，前端按它分支，不要匹配 `message`。
    pub code: &'static str,
    /// 面向用户的中文文案。
    pub message: String,
    /// 是否属于"稍后再试"类网络错误。
    pub retryable: bool,
    /// 是否为安全阻断（签名/哈希校验失败），UI 应据此升级提示语气。
    pub security: bool,
}

/// 跨重启待安装记录的精简视图（`pending` 命令）。
#[derive(Debug, Clone, Serialize)]
pub struct PendingUpdateDto {
    /// 目标版本号。
    pub version: String,
    /// `exe`（单文件）或 `bundle`（整包）。
    pub kind: &'static str,
    /// 记录创建时间（Unix 秒）。
    pub created_at: u64,
}

impl PendingUpdateDto {
    /// 只暴露前端需要的信息；路径留在 Rust 侧，避免泄露文件系统布局。
    pub(crate) fn from_pending(pending: &tauri_delta_updater::PendingUpdate) -> Self {
        Self {
            version: pending.version().to_string(),
            kind: if pending.is_bundle() { "bundle" } else { "exe" },
            created_at: pending.created_at,
        }
    }
}

/// 全量下载的结果（`download_full` 命令）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FullDownloadDto {
    /// 安装包已下载并通过签名校验；`path` 交给平台安装执行器。
    Downloaded {
        /// 目标版本。
        version: String,
        /// 安装包在工作目录里的绝对路径。
        path: String,
    },
    /// 已经是最新版本，没有下载。
    UpToDate {
        /// 清单声明的版本。
        version: String,
    },
}

impl FullDownloadDto {
    /// 把库层结果转成前端契约。
    ///
    /// `FullDownloadOutcome` 是 `non_exhaustive` 的：库新增变体时不能让插件
    /// panic，而是返回一个明确的错误码，提示插件版本过旧。
    pub(crate) fn from_outcome(outcome: FullDownloadOutcome) -> Result<Self, ErrorDto> {
        match outcome {
            FullDownloadOutcome::Downloaded { installer } => Ok(Self::Downloaded {
                version: installer.version().to_string(),
                path: installer.path().display().to_string(),
            }),
            FullDownloadOutcome::UpToDate { version } => Ok(Self::UpToDate { version }),
            _ => Err(ErrorDto {
                code: "unsupported_outcome",
                message: "插件版本过旧，无法识别全量下载结果，请升级插件".into(),
                retryable: false,
                security: false,
            }),
        }
    }
}

impl ErrorDto {
    /// 更新器不可用（构造失败或配置缺失）：功能降级，应用本身仍可正常运行。
    pub(crate) fn unavailable(reason: String) -> Self {
        Self {
            code: "updater_unavailable",
            message: format!("更新器不可用: {reason}"),
            retryable: false,
            security: false,
        }
    }

    /// 没有已就绪的产物却调用了 `launch`。
    pub(crate) fn nothing_to_launch() -> Self {
        Self {
            code: "nothing_to_launch",
            message: "没有待启动的更新产物：请先调用 prepare".into(),
            retryable: false,
            security: false,
        }
    }
}

impl From<DeltaError> for ErrorDto {
    fn from(err: DeltaError) -> Self {
        Self {
            code: err.code(),
            message: err.to_string(),
            retryable: err.is_retryable(),
            security: err.is_security_blocking(),
        }
    }
}
