//! 安全模式：连续失败计数与冷却恢复。
//!
//! 计数文件格式：第一行是连续失败次数，第二行是最后一次失败的 Unix 时间戳，
//! 第三行是失败类型（`security_block` / `environment_failure` /
//! `transient_retries_exhausted`）。旧版文件只有第一行，按"冷却已过期"处理——
//! 升级用户不会被永久锁死。

use crate::core::artifacts;
use crate::error::Result;
use crate::updater::SafeModeReason;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

/// 记录一次更新失败。
///
/// 返回累计的连续失败次数。原因按环境失败处理；
/// 需要区分安全阻断时用 [`record_failed_attempt_with_reason`]。
pub async fn record_failed_attempt(base_dir: &Path) -> Result<u32> {
    record_failed_attempt_with_reason(base_dir, SafeModeReason::EnvironmentFailure).await
}

/// 记录一次带原因的更新失败。
pub async fn record_failed_attempt_with_reason(
    base_dir: &Path,
    reason: SafeModeReason,
) -> Result<u32> {
    let fail_file = base_dir.join(artifacts::FAILURE_COUNTER);
    let count = read_state(&fail_file).await.count.saturating_add(1);
    write_failure(&fail_file, count, reason).await;
    Ok(count)
}

/// 记录一次全量安装未生效，返回累计次数。
///
/// **与增量失败完全隔离**：全量安装是用户显式选择或回退路径，安装器被取消、
/// 提权被拒绝、安装包缺失都不应冻结增量通道。本计数只用于诊断展示，
/// 不参与 [`should_abort`] / [`is_safe_mode_active`]。
pub(crate) async fn record_full_failure(base_dir: &Path, reason: SafeModeReason) -> u32 {
    let path = base_dir.join(artifacts::FULL_FAILURE_COUNTER);
    let count = read_state(&path).await.count.saturating_add(1);
    write_failure(&path, count, reason).await;
    count
}

/// [`record_full_failure`] 的同步版本（启动确认阶段使用）。
pub(crate) fn record_full_failure_sync(base_dir: &Path, reason: SafeModeReason) -> u32 {
    let path = base_dir.join(artifacts::FULL_FAILURE_COUNTER);
    let count = read_state_sync(&path).count.saturating_add(1);
    write_failure_sync(&path, count, reason);
    count
}

/// 连续全量安装未生效的次数；只用于诊断，不影响增量安全模式。
pub(crate) async fn full_failures(base_dir: &Path) -> u32 {
    read_state(&base_dir.join(artifacts::FULL_FAILURE_COUNTER))
        .await
        .count
}

/// 全量安装确认成功后清零。
pub(crate) fn clear_full_failures_sync(base_dir: &Path) {
    let _ = std::fs::remove_file(base_dir.join(artifacts::FULL_FAILURE_COUNTER));
}

/// 写入失败计数文件（尽力而为）。
async fn write_failure(path: &Path, count: u32, reason: SafeModeReason) {
    let content = format!("{count}\n{}\n{}", now_secs(), reason_key(reason));
    let _ = fs::write(path, content).await;
}

/// [`write_failure`] 的同步版本。
fn write_failure_sync(path: &Path, count: u32, reason: SafeModeReason) {
    let content = format!("{count}\n{}\n{}", now_secs(), reason_key(reason));
    let _ = std::fs::write(path, content);
}

/// 最近一次失败的类型；没有记录或旧格式文件只有计数时为 `None`。
pub async fn last_failure_reason(base_dir: &Path) -> Option<SafeModeReason> {
    read_state(&base_dir.join(artifacts::FAILURE_COUNTER))
        .await
        .last_failure_reason
}

/// 安全模式原因的持久化键。
fn reason_key(reason: SafeModeReason) -> &'static str {
    match reason {
        SafeModeReason::SecurityBlock => "security_block",
        SafeModeReason::EnvironmentFailure => "environment_failure",
        SafeModeReason::TransientRetriesExhausted => "transient_retries_exhausted",
    }
}

/// 由持久化键还原原因；未知键按环境失败处理。
fn reason_from_key(key: &str) -> Option<SafeModeReason> {
    match key.trim() {
        "security_block" => Some(SafeModeReason::SecurityBlock),
        "environment_failure" => Some(SafeModeReason::EnvironmentFailure),
        "transient_retries_exhausted" => Some(SafeModeReason::TransientRetriesExhausted),
        _ => None,
    }
}

/// 获取当前连续失败次数。
pub async fn get_failed_attempts(base_dir: &Path) -> u32 {
    read_state(&base_dir.join(artifacts::FAILURE_COUNTER))
        .await
        .count
}

/// 判断是否应停止尝试增量更新。
///
/// 连续失败达到阈值后返回 `true`，此时应改为全量下载或提示用户手动处理，
/// 避免陷入"更新 → 失败 → 再更新"的死循环。
///
/// 达到阈值后进入冷却期：冷却结束会自动清零并放行一次新的尝试。
/// 没有冷却的安全模式等于"永久禁用增量"，一次磁盘满就可能让用户
/// 再也走不了增量路径。
pub async fn should_abort(base_dir: &Path) -> bool {
    let state = read_state(&base_dir.join(artifacts::FAILURE_COUNTER)).await;
    if state.count < artifacts::MAX_FAILED_ATTEMPTS {
        return false;
    }
    // 旧格式没有时间戳（last_failure = 0）：视为冷却已过期，允许重试
    let elapsed = now_secs().saturating_sub(state.last_failure);
    if elapsed >= artifacts::SAFE_MODE_COOLDOWN_SECS {
        // 冷却结束：清零，让本轮失败从 1 重新开始计数
        clear_failed_attempts(base_dir).await;
        return false;
    }
    true
}

/// 安全模式是否处于激活状态（不触发冷却恢复，只读）。
pub async fn is_safe_mode_active(base_dir: &Path) -> bool {
    let state = read_state(&base_dir.join(artifacts::FAILURE_COUNTER)).await;
    state.count >= artifacts::MAX_FAILED_ATTEMPTS
        && now_secs().saturating_sub(state.last_failure) < artifacts::SAFE_MODE_COOLDOWN_SECS
}

/// 安全模式剩余冷却秒数；未处于安全模式时返回 `None`。
///
/// 供 UI 展示"还需等待多久"与结构化事件使用。
pub async fn safe_mode_remaining(base_dir: &Path) -> Option<u64> {
    let state = read_state(&base_dir.join(artifacts::FAILURE_COUNTER)).await;
    if state.count < artifacts::MAX_FAILED_ATTEMPTS {
        return None;
    }
    let elapsed = now_secs().saturating_sub(state.last_failure);
    if elapsed >= artifacts::SAFE_MODE_COOLDOWN_SECS {
        None
    } else {
        Some(artifacts::SAFE_MODE_COOLDOWN_SECS - elapsed)
    }
}

/// 手动退出安全模式（宿主提供"重试"按钮时使用）。
pub async fn reset_safety(base_dir: &Path) {
    clear_failed_attempts(base_dir).await;
}

/// 更新成功后，清除失败记录。
pub async fn clear_failed_attempts(base_dir: &Path) {
    clear_failed_attempts_sync(base_dir);
}

/// 同步清除失败记录，供启动确认等同步上下文使用。
pub fn clear_failed_attempts_sync(base_dir: &Path) {
    let _ = std::fs::remove_file(base_dir.join(artifacts::FAILURE_COUNTER));
}

/// 失败计数状态。
struct SafetyState {
    count: u32,
    last_failure: u64,
    last_failure_reason: Option<SafeModeReason>,
}

/// 读取失败计数状态，文件不存在或内容损坏时视为 0。
async fn read_state(path: &Path) -> SafetyState {
    parse_state(&fs::read_to_string(path).await.unwrap_or_default())
}

/// 同步版本，供启动确认路径使用。
fn read_state_sync(path: &Path) -> SafetyState {
    parse_state(&std::fs::read_to_string(path).unwrap_or_default())
}

fn parse_state(content: &str) -> SafetyState {
    let mut lines = content.lines();
    SafetyState {
        count: lines
            .next()
            .and_then(|l| l.trim().parse::<u32>().ok())
            .unwrap_or(0),
        last_failure: lines
            .next()
            .and_then(|l| l.trim().parse::<u64>().ok())
            .unwrap_or(0),
        last_failure_reason: lines.next().and_then(reason_from_key),
    }
}

/// 当前 Unix 时间戳（秒）。
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 启动确认阶段用同步写入全量失败；异步读取必须看到同一份状态。
    #[tokio::test]
    async fn sync_full_recorder_matches_async_reads() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            record_full_failure_sync(dir.path(), SafeModeReason::EnvironmentFailure),
            1
        );
        assert_eq!(full_failures(dir.path()).await, 1);
        assert_eq!(
            get_failed_attempts(dir.path()).await,
            0,
            "同步写入也必须落在全量计数，不能污染增量计数"
        );
    }

    #[tokio::test]
    async fn cooldown_expires_and_resets_counter() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..artifacts::MAX_FAILED_ATTEMPTS {
            record_failed_attempt(dir.path()).await.unwrap();
        }
        assert!(should_abort(dir.path()).await, "达到阈值必须进入安全模式");
        assert!(is_safe_mode_active(dir.path()).await);

        // 把最后失败时间拨到冷却窗口之外
        let path = dir.path().join(artifacts::FAILURE_COUNTER);
        let old = now_secs() - artifacts::SAFE_MODE_COOLDOWN_SECS - 60;
        fs::write(&path, format!("{}\n{old}", artifacts::MAX_FAILED_ATTEMPTS))
            .await
            .unwrap();

        assert!(
            !should_abort(dir.path()).await,
            "冷却结束后必须允许一次新的尝试"
        );
        assert_eq!(
            get_failed_attempts(dir.path()).await,
            0,
            "冷却结束应清零计数，让本轮失败从 1 重新开始"
        );
        assert!(!is_safe_mode_active(dir.path()).await);
    }

    #[tokio::test]
    async fn legacy_counter_without_timestamp_is_not_permanent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(artifacts::FAILURE_COUNTER);
        // 0.3.x 的格式只有一行计数，没有时间戳
        fs::write(&path, format!("{}", artifacts::MAX_FAILED_ATTEMPTS))
            .await
            .unwrap();

        assert!(
            !should_abort(dir.path()).await,
            "升级用户的历史计数不能被当成永久安全模式"
        );
        assert_eq!(get_failed_attempts(dir.path()).await, 0);
    }

    #[tokio::test]
    async fn reset_safety_clears_counter() {
        let dir = tempfile::tempdir().unwrap();
        record_failed_attempt(dir.path()).await.unwrap();
        record_failed_attempt(dir.path()).await.unwrap();
        assert_eq!(get_failed_attempts(dir.path()).await, 2);

        reset_safety(dir.path()).await;
        assert_eq!(get_failed_attempts(dir.path()).await, 0);
        assert!(!should_abort(dir.path()).await);
    }

    /// 失败原因必须持久化，安全模式才能区分“可能被攻击”与“环境问题”。
    #[tokio::test]
    async fn records_last_failure_reason() {
        let dir = tempfile::tempdir().unwrap();
        record_failed_attempt_with_reason(dir.path(), SafeModeReason::SecurityBlock)
            .await
            .unwrap();
        assert_eq!(
            last_failure_reason(dir.path()).await,
            Some(SafeModeReason::SecurityBlock)
        );

        // 旧格式（只有计数与时间戳）没有原因，返回 None，不猜。
        let path = dir.path().join(artifacts::FAILURE_COUNTER);
        fs::write(&path, format!("1\n{}", now_secs()))
            .await
            .unwrap();
        assert_eq!(last_failure_reason(dir.path()).await, None);
    }

    /// 全量失败是独立失败域：达到阈值也只记录，不冻结增量通道。
    #[tokio::test]
    async fn full_failures_do_not_enter_delta_safe_mode() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..artifacts::MAX_FAILED_ATTEMPTS {
            record_full_failure_sync(dir.path(), SafeModeReason::EnvironmentFailure);
        }

        assert_eq!(full_failures(dir.path()).await, 3);
        assert_eq!(
            get_failed_attempts(dir.path()).await,
            0,
            "全量失败不能写入增量计数"
        );
        assert!(
            !should_abort(dir.path()).await,
            "取消安装器/拒绝提权不能冻结增量更新"
        );
        assert!(!is_safe_mode_active(dir.path()).await);
    }

    /// 反向隔离：增量失败也不应污染全量计数。
    #[tokio::test]
    async fn delta_failures_keep_full_counter_untouched() {
        let dir = tempfile::tempdir().unwrap();
        record_failed_attempt(dir.path()).await.unwrap();

        assert_eq!(get_failed_attempts(dir.path()).await, 1);
        assert_eq!(full_failures(dir.path()).await, 0);
    }
}
