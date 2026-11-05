//! 可选的结构化事件。
//!
//! 启用 `tracing` feature 后，关键节点会发出带字段的事件，宿主可以把它们
//! 桥接到自己的日志、UI 或监控。**默认关闭**：不启用时不编译 `tracing`，
//! 宏调用编译为空操作，没有运行开销。
//!
//! 事件命名遵循 `delta.<阶段>`：
//!
//! | 事件 | 关键字段 | 何时发出 |
//! |---|---|---|
//! | `delta.check` | `version`, `is_newer`, `has_patch` | 清单验签与版本比较之后 |
//! | `delta.patch.matched` | `from`, `to`, `size` | 命中可用补丁 |
//! | `delta.patch.rejected` | `reason` | 补丁哈希/应用失败 |
//! | `delta.fallback` | `reason` | 回退全量下载 |
//! | `delta.patched` | `version` | 合并完成、脚本就绪 |
//! | `delta.launch` | — | 替换脚本已启动 |
//! | `delta.safety_mode.entered` | `count` | 进入安全模式 |
//! | `delta.safety_mode.cooldown_active` | `remaining_secs` | 冷却中 |
//!
//! 回滚发生在应用退出后的替换脚本里，库进程无法观测；脚本会把
//! `delta.rollback.triggered` 写入 `tdx_update.log`。
//!
//! 这些事件只描述状态，不包含路径以外的敏感信息；路径本身也只发文件名。

/// 发出 `delta.check` 事件。
pub(crate) fn check(version: &str, is_newer: bool, has_patch: bool) {
    #[cfg(feature = "tracing")]
    tracing::info!(
        target: "tauri_delta_updater",
        event = "delta.check",
        version,
        is_newer,
        has_patch,
        "清单检查完成"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (version, is_newer, has_patch);
}

/// 发出 `delta.patch.matched` 事件。
pub(crate) fn patch_matched(from: &str, to: &str, size: Option<u64>) {
    #[cfg(feature = "tracing")]
    tracing::info!(
        target: "tauri_delta_updater",
        event = "delta.patch.matched",
        from,
        to,
        size = size.unwrap_or(0),
        "命中增量补丁"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = (from, to, size);
}

/// 发出 `delta.patch.rejected` 事件。
pub(crate) fn patch_rejected(reason: &str) {
    #[cfg(feature = "tracing")]
    tracing::warn!(
        target: "tauri_delta_updater",
        event = "delta.patch.rejected",
        reason,
        "补丁被拒绝"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = reason;
}

/// 发出 `delta.fallback` 事件。
pub(crate) fn fallback(reason: &str) {
    #[cfg(feature = "tracing")]
    tracing::info!(
        target: "tauri_delta_updater",
        event = "delta.fallback",
        reason,
        "回退全量下载"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = reason;
}

/// 发出 `delta.patched` 事件。
pub(crate) fn patched(version: &str) {
    #[cfg(feature = "tracing")]
    tracing::info!(
        target: "tauri_delta_updater",
        event = "delta.patched",
        version,
        "增量合并完成"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = version;
}

/// 发出 `delta.safety_mode.entered` 事件。
pub(crate) fn safety_mode_entered(count: u32) {
    #[cfg(feature = "tracing")]
    tracing::warn!(
        target: "tauri_delta_updater",
        event = "delta.safety_mode.entered",
        count,
        "进入安全模式"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = count;
}

/// 发出 `delta.safety_mode.cooldown_active` 事件。
pub(crate) fn cooldown_active(remaining_secs: u64) {
    #[cfg(feature = "tracing")]
    tracing::info!(
        target: "tauri_delta_updater",
        event = "delta.safety_mode.cooldown_active",
        remaining_secs,
        "安全模式冷却中"
    );
    #[cfg(not(feature = "tracing"))]
    let _ = remaining_secs;
}

/// 发出 `delta.launch` 事件。
///
/// 回滚本身发生在应用退出后的替换脚本里，库进程无法观测；
/// 脚本会把 `delta.rollback.triggered` 写入 `tdx_update.log`。
pub(crate) fn launch() {
    #[cfg(feature = "tracing")]
    tracing::info!(
        target: "tauri_delta_updater",
        event = "delta.launch",
        "已启动替换脚本"
    );
}
