//! 全量安装的“下次启动确认”记录。
//!
//! Windows 安装器由 `ShellExecuteW` 启动，调用返回时只代表“安装器起来了”，
//! 不代表安装成功。这里把目标版本写入工作目录，下次启动时对比当前版本：
//!
//! - 已达到目标版本 → 确认成功，清掉记录；
//! - 未达到且仍在宽限期内 → 安装器可能还在运行，保持等待；
//! - 未达到且已超过宽限期 → 视为安装未生效，计入一次失败，让连续失败
//!   能像增量路径一样进入安全模式。

use crate::core::artifacts;
use crate::platform::target::normalize_version;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 安装器启动后的确认宽限期（1 小时）。
///
/// 覆盖安装器 UI、重启和其他进程占用；超过它仍未升级，才认为没有生效。
pub(crate) const CONFIRM_GRACE_SECS: u64 = 60 * 60;

/// 待确认的全量安装记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FullPending {
    /// 安装器目标版本。
    pub version: String,
    /// 启动安装器的时间（Unix 秒）。
    pub created_at: u64,
}

/// 启动确认的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingState {
    /// 当前版本已达到目标版本。
    Confirmed,
    /// 仍在宽限期内，继续等待。
    Waiting,
    /// 超过宽限期仍未升级。
    Failed,
}

/// 写入待确认记录；尽力而为，失败不影响安装流程。
pub(crate) fn write(work_dir: &Path, version: &str) {
    let pending = FullPending {
        version: version.to_string(),
        created_at: now_secs(),
    };
    if let Ok(json) = serde_json::to_vec(&pending) {
        let _ = std::fs::write(work_dir.join(artifacts::FULL_PENDING_FILE), json);
    }
}

/// 读取待确认记录；不存在或损坏时为 `None`。
pub(crate) fn read(work_dir: &Path) -> Option<FullPending> {
    let text = std::fs::read_to_string(work_dir.join(artifacts::FULL_PENDING_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 清除待确认记录。
pub(crate) fn clear(work_dir: &Path) {
    let _ = std::fs::remove_file(work_dir.join(artifacts::FULL_PENDING_FILE));
}

/// 根据当前版本判断这次全量安装是否生效。
pub(crate) fn evaluate(current_version: &str, pending: &FullPending) -> PendingState {
    let current = normalize_version(current_version);
    let target = normalize_version(&pending.version);
    let reached = match (
        semver::Version::parse(current),
        semver::Version::parse(target),
    ) {
        (Ok(current), Ok(target)) => current >= target,
        // 版本号异常时退化为字符串比较，至少不能永远卡在 Waiting。
        _ => current == target,
    };
    if reached {
        return PendingState::Confirmed;
    }
    if now_secs().saturating_sub(pending.created_at) < CONFIRM_GRACE_SECS {
        PendingState::Waiting
    } else {
        PendingState::Failed
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{evaluate, PendingState};
    use crate::core::artifacts;
    use crate::updater::{DeltaUpdater, UpdateConfig};
    use std::path::Path;

    fn test_updater(dir: &Path, current: &str) -> DeltaUpdater {
        DeltaUpdater::new(
            UpdateConfig::new("https://cdn.example.com/latest.json", "dGVzdA==")
                .current_version(current)
                .executable_path(dir.join("app.exe"))
                .work_dir(dir),
        )
        .unwrap()
    }

    fn pending(version: &str, age_secs: u64) -> super::FullPending {
        super::FullPending {
            version: version.into(),
            created_at: super::now_secs().saturating_sub(age_secs),
        }
    }

    /// 已到目标版本即确认成功，不受宽限期影响。
    #[test]
    fn reached_version_is_confirmed() {
        assert_eq!(
            evaluate("0.5.0", &pending("0.5.0", 10)),
            PendingState::Confirmed
        );
        assert_eq!(
            evaluate("0.6.0", &pending("0.5.0", 10)),
            PendingState::Confirmed
        );
    }

    /// 宽限期内不判定失败：安装器可能仍在运行或等待重启。
    #[test]
    fn fresh_unreached_install_waits() {
        assert_eq!(
            evaluate("0.4.0", &pending("0.5.0", 60)),
            PendingState::Waiting
        );
    }

    /// 超过宽限期仍未升级，视为安装未生效。
    #[test]
    fn stale_unreached_install_fails() {
        assert_eq!(
            evaluate("0.4.0", &pending("0.5.0", super::CONFIRM_GRACE_SECS + 1)),
            PendingState::Failed
        );
    }

    /// 安装器刚启动、版本还是旧的：保持记录，不计数，也不写成功标志。
    #[tokio::test]
    async fn startup_waits_inside_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        super::write(dir.path(), "0.5.0");
        let updater = test_updater(dir.path(), "0.4.0");

        assert!(!updater.confirm_startup_success_if_pending().unwrap());
        assert!(super::read(dir.path()).is_some(), "宽限期内不能丢弃记录");
        assert_eq!(
            crate::core::safety::get_failed_attempts(dir.path()).await,
            0
        );
    }

    /// 超过宽限期仍是旧版本：清记录并计入一次失败，连续失败可进安全模式。
    #[tokio::test]
    async fn stale_install_counts_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let pending = pending("0.5.0", super::CONFIRM_GRACE_SECS + 60);
        std::fs::write(
            dir.path().join(artifacts::FULL_PENDING_FILE),
            serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        let updater = test_updater(dir.path(), "0.4.0");

        assert!(!updater.confirm_startup_success_if_pending().unwrap());
        assert!(super::read(dir.path()).is_none(), "失败判定后必须清记录");
        assert_eq!(
            crate::core::safety::get_failed_attempts(dir.path()).await,
            1
        );
    }

    /// 版本已达到目标：确认成功（写成功标志、清失败计数与记录）。
    #[tokio::test]
    async fn reached_version_confirms_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        crate::core::safety::record_failed_attempt_sync(
            dir.path(),
            crate::updater::SafeModeReason::EnvironmentFailure,
        );
        super::write(dir.path(), "0.4.0");
        let updater = test_updater(dir.path(), "0.4.0");

        assert!(updater.confirm_startup_success_if_pending().unwrap());
        assert!(super::read(dir.path()).is_none());
        assert!(dir.path().join(artifacts::SUCCESS_FLAG).is_file());
        assert_eq!(
            crate::core::safety::get_failed_attempts(dir.path()).await,
            0
        );
    }
}
