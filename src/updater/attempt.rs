//! 上一次更新尝试的结构化记录。
//!
//! `update()` / `install()` / `prepare()` 结束时把结果写入
//! `work_dir/tdx_last_attempt.json`，诊断页直接读取，不必解析日志或
//! `tdx_events.jsonl`。记录是**尽力而为**的：写失败只影响可观测性，
//! 绝不影响更新结果。

use crate::core::artifacts;
use crate::{DeltaError, Result, UpdateOutcome};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 最后一次更新尝试的结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AttemptOutcome {
    /// 增量补丁已合并并（按配置）启动替换。
    Patched,
    /// 整包补丁已合并并（按配置）启动替换。
    Bundled,
    /// 上次中断的整包替换已生成修复脚本。
    Repair,
    /// 已经是最新版本（含防降级重放）。
    UpToDate,
    /// 本次灰度未命中。
    Deferred,
    /// 系统版本低于清单要求。
    UnsupportedOs,
    /// 增量不可用，需要全量下载。
    FallbackToFullDownload,
    /// 处于安全模式，未执行更新。
    SafeMode,
    /// 被调用方取消。
    Cancelled,
    /// 更新失败（`error_code` 给出稳定错误码）。
    Failed,
}

/// 一次更新尝试的可持久化快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UpdateAttempt {
    /// 完成时间（Unix 秒）。UI 自行按本地时区格式化。
    pub timestamp_unix_secs: u64,
    /// 本次尝试耗时（秒，向下取整）。
    pub duration_secs: u64,
    /// 结果分类。
    pub outcome: AttemptOutcome,
    /// 尝试开始时的版本。
    pub current_version: String,
    /// 目标版本；失败或修复时为 `None`。
    pub target_version: Option<String>,
    /// 失败时的稳定错误码（与 [`DeltaError::code`] 一致）。
    pub error_code: Option<String>,
}

/// 记录一次更新尝试的结果；写盘失败会被忽略。
pub(crate) fn record(
    work_dir: &Path,
    current_version: &str,
    started: Instant,
    result: &Result<UpdateOutcome>,
) {
    let (outcome, target_version, error_code) = classify(result);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let attempt = UpdateAttempt {
        timestamp_unix_secs: now,
        duration_secs: started.elapsed().as_secs(),
        outcome,
        current_version: current_version.to_string(),
        target_version,
        error_code,
    };
    write_atomic(work_dir, &attempt);
}

/// 把更新结果映射成记录字段。
fn classify(result: &Result<UpdateOutcome>) -> (AttemptOutcome, Option<String>, Option<String>) {
    match result {
        Ok(outcome) => {
            let outcome_kind = match outcome {
                UpdateOutcome::Patched { .. } => AttemptOutcome::Patched,
                UpdateOutcome::Bundled { .. } => AttemptOutcome::Bundled,
                UpdateOutcome::Repair { .. } => AttemptOutcome::Repair,
                UpdateOutcome::UpToDate { .. } => AttemptOutcome::UpToDate,
                UpdateOutcome::Deferred { .. } => AttemptOutcome::Deferred,
                UpdateOutcome::UnsupportedOs { .. } => AttemptOutcome::UnsupportedOs,
                UpdateOutcome::FallbackToFullDownload { .. } => {
                    AttemptOutcome::FallbackToFullDownload
                }
                UpdateOutcome::SafeMode { .. } => AttemptOutcome::SafeMode,
            };
            let target = match outcome {
                UpdateOutcome::Patched { staged } => Some(staged.manifest().version.clone()),
                UpdateOutcome::Bundled { staged } => Some(staged.manifest().version.clone()),
                UpdateOutcome::UpToDate { version } => Some(version.clone()),
                UpdateOutcome::Deferred { version, .. } => Some(version.clone()),
                _ => None,
            };
            (outcome_kind, target, None)
        }
        Err(DeltaError::Cancelled) => (AttemptOutcome::Cancelled, None, None),
        Err(err) => (AttemptOutcome::Failed, None, Some(err.code().to_string())),
    }
}

/// 读取最近一次记录；文件缺失或损坏时返回 `None`。
pub(crate) fn read(work_dir: &Path) -> Option<UpdateAttempt> {
    let text = std::fs::read_to_string(work_dir.join(artifacts::LAST_ATTEMPT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子写入：先写临时文件再重命名，避免读到半截 JSON。
fn write_atomic(work_dir: &Path, attempt: &UpdateAttempt) {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let Ok(json) = serde_json::to_vec_pretty(attempt) else {
        return;
    };
    let path = work_dir.join(artifacts::LAST_ATTEMPT_FILE);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = work_dir.join(format!(
        "{}.{}.{seq}.tmp",
        artifacts::LAST_ATTEMPT_FILE,
        std::process::id()
    ));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, read, record, AttemptOutcome, UpdateAttempt};
    use crate::DeltaError;
    use std::time::Instant;

    /// 失败与取消必须区分开：取消不算"更新失败"。
    #[test]
    fn classify_distinguishes_cancel_and_failure() {
        let (outcome, target, code) = classify(&Err(DeltaError::Cancelled));
        assert_eq!(outcome, AttemptOutcome::Cancelled);
        assert!(target.is_none() && code.is_none());

        let (outcome, _, code) = classify(&Err(DeltaError::Config("bad".into())));
        assert_eq!(outcome, AttemptOutcome::Failed);
        assert_eq!(code.as_deref(), Some("config"));
    }

    /// 记录写入后可原样读回；损坏的 JSON 不得让诊断崩溃。
    #[test]
    fn record_roundtrips_and_ignores_corruption() {
        let dir = tempfile::tempdir().unwrap();
        record(
            dir.path(),
            "0.1.0",
            Instant::now(),
            &Err(DeltaError::Cancelled),
        );
        let attempt = read(dir.path()).expect("记录必须可读");
        assert_eq!(attempt.outcome, AttemptOutcome::Cancelled);
        assert_eq!(attempt.current_version, "0.1.0");

        std::fs::write(
            dir.path().join(crate::core::artifacts::LAST_ATTEMPT_FILE),
            b"{ not json",
        )
        .unwrap();
        assert!(read(dir.path()).is_none());
    }

    /// 序列化字段必须稳定：UI 依赖这些名字。
    #[test]
    fn attempt_json_fields_are_stable() {
        let attempt = UpdateAttempt {
            timestamp_unix_secs: 1,
            duration_secs: 2,
            outcome: AttemptOutcome::FallbackToFullDownload,
            current_version: "0.1.0".into(),
            target_version: None,
            error_code: None,
        };
        let value = serde_json::to_value(&attempt).unwrap();
        assert_eq!(value["outcome"], "fallback_to_full_download");
        assert!(value.get("timestamp_unix_secs").is_some());
    }
}
