//! 环境诊断：只读快照，不发起网络请求。

use crate::platform::PlatformUpdater;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::DeltaUpdater;

/// 替换脚本写入的结构化事件（`tdx_events.jsonl` 的一行）。
///
/// 这是脚本落盘的原始形态：解析失败或带未知字段的行会被跳过，
/// 保证模板以后新增字段时旧宿主不会报错。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScriptEvent {
    /// 事件名，例如 `delta.rollback.triggered`。
    pub event: String,
    /// 回滚原因（`update_failed` / `startup_timeout` / `repaired_partial_update`）。
    pub reason: Option<String>,
    /// ISO-8601 时间戳（UTC）。
    pub time: Option<String>,
}

/// 诊断快照里最多保留的事件条数：回滚是低频事件，保留最近 20 条足够排障。
const MAX_SCRIPT_EVENTS: usize = 20;

/// 环境诊断快照。
///
/// 由 [`DeltaUpdater::diagnose`] 返回，只读、不发起网络请求。
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Diagnosis {
    /// 被更新的可执行文件。
    pub executable: PathBuf,
    /// 工作目录（补丁、脚本、日志所在处）。
    pub work_dir: PathBuf,
    /// 目标平台标识。
    pub platform: String,
    /// 工作目录是否可写。
    pub work_dir_writable: bool,
    /// 安装目录是否可写（为 false 时替换需要提权）。
    pub install_dir_writable: bool,
    /// 当前环境不适合**单文件**增量的原因（只读 AppImage 目录、macOS bundle 等）。
    ///
    /// AppImage 本身是支持的：替换目标是 `$APPIMAGE` 指向的 `.AppImage` 本体；
    /// 只有定位不到本体或所在目录不可写时才填这里。
    pub fallback_reason: Option<&'static str>,
    /// 配置了整包清单且当前环境支持整包替换（macOS `.app` / Windows）。
    pub bundle_capable: bool,
    /// 是否处于安全模式。
    pub safe_mode: bool,
    /// 是否需要先修复上次中断的整包替换（备份仍在）。
    pub needs_repair: bool,
    /// 上次替换的启动状态指针（`staged` / `booting` / `committed` / `failed` / `rolled_back`）。
    ///
    /// 新进程崩溃、被 `kill -9` 或健康检查失败时，指针会停在 `booting`/`failed`，
    /// 下次启动据此区分"从未执行到宿主代码"与"启动了但未确认"。
    pub startup_state: Option<crate::core::startup::StartupState>,
    /// 上一次替换脚本整体回滚的原因（`update_failed` / `startup_timeout` / `repaired_partial_update`）。
    ///
    /// 失败发生在宿主退出之后，进程内拿不到；脚本把原因写入工作目录，
    /// 新版本成功确认启动后自动清除。UI 可据此解释"为什么重启后版本没变"。
    pub last_rollback: Option<String>,
    /// Windows 上原版本是否通过 Authenticode 校验；其它平台为 `None`。
    ///
    /// `Some(true)` 表示已签名，新版本产物未签名会被直接阻断。
    /// `Some(false)` 有两种成因：确实未签名（开发构建，不做"签名退化"拦截），
    /// 或检查无法得出结论（离线且证书链不在缓存，按不可信处理）。
    /// 用 [`Self::authenticode_check_skipped`] 区分，不要只看本字段下结论。
    pub authenticode: Option<bool>,
    /// Windows 上本次 Authenticode 检查的结论状态；其它平台为 `None`。
    ///
    /// - `Some(true)`：检查已执行，但因离线 / 证书链不在缓存而无法判定；
    /// - `Some(false)`：检查已执行并得出结论（可信，或明确不可信）；
    /// - `None`：非 Windows 平台，未执行检查。
    ///
    /// [`Self::authenticode`] 为 `Some(false)` 时，可能是"确实未签名"，
    /// 也可能是"无法判定后按不可信处理"；本字段负责区分。
    pub authenticode_check_skipped: Option<bool>,
    /// 替换脚本日志路径。
    pub log_path: PathBuf,
    /// 结构化事件流路径（JSON Lines）：脚本在回滚等关键阶段追加一行，
    /// 宿主可在下一次启动时解析完整的事件链。
    pub events_path: PathBuf,
    /// 最近的结构化脚本事件（最多 20 条，按时间正序）。
    ///
    /// 已由 [`Self::events_path`] 指向的文件解析；格式错误的行会被忽略。
    pub script_events: Vec<ScriptEvent>,
    /// Linux 上是否找到降权工具（`runuser` 或 `su`）；其它平台为 `None`。
    ///
    /// 为 `false` 时，提权替换后可能无法把应用降权回原用户，
    /// 诊断页应提示这一环境限制。
    pub privilege_tools_available: Option<bool>,
}

impl Diagnosis {
    /// 当前环境是否具备走增量路径的条件（不包含网络与清单可用性）。
    ///
    /// 为 `false` 时，`fallback_reason` 或 `safe_mode` 会说明原因；
    /// 供 UI 的"诊断"页直接给出结论，而不是让宿主自己拼条件。
    pub fn can_use_delta(&self) -> bool {
        (self.fallback_reason.is_none() || self.bundle_capable)
            && !self.safe_mode
            && self.work_dir_writable
    }

    /// 面向最终用户的一句话摘要，宿主可直接展示或作为默认文案。
    ///
    /// 只做语义映射，不发起任何 IO；具体原因仍以结构化字段为准。
    pub fn user_message(&self) -> &'static str {
        if self.safe_mode {
            "增量更新已暂停；冷却后会自动重试，也可以手动下载完整安装包。"
        } else if self.needs_repair {
            "上次更新未完成，应用正在恢复旧版本；重启后无需再次操作。"
        } else if let Some(reason) = self.last_rollback.as_deref() {
            match reason {
                "startup_timeout" => "新版本启动超时，已回滚到旧版本；请稍后重试。",
                "startup_not_started" => "新版本未能启动，已回滚到旧版本；请稍后重试。",
                "startup_crashed" => "新版本启动后退出，已回滚到旧版本；请稍后重试。",
                "health_check_failed" => "新版本健康检查未通过，已回滚到旧版本。",
                "update_failed" => "上次更新未能完成，已回滚到旧版本。",
                "repaired_partial_update" => "已修复上次中断的更新。",
                _ => "上次更新已回滚到旧版本。",
            }
        } else if !self.work_dir_writable {
            "更新目录不可写，请检查权限后重试。"
        } else if self.fallback_reason.is_some() && !self.bundle_capable {
            "当前环境不支持原地增量更新，将使用完整安装包。"
        } else {
            "环境正常。"
        }
    }

    /// 启动阶段可操作的配置警告；空表示没有发现明显缺口。
    ///
    /// 与 [`Self::user_message`] 的区别：这里只报告**配置/环境**问题，
    /// 不包含"上次为什么回滚"这类历史事件。插件在注册时会把它们打到
    /// 启动日志，让接入方第一次运行就看到问题，而不是等用户触发更新。
    pub fn warnings(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if !self.work_dir_writable {
            warnings.push("更新工作目录不可写：请检查目录权限，否则所有更新都会失败。");
        }
        if self.fallback_reason.is_some() && !self.bundle_capable {
            warnings
                .push("当前环境不支持单文件增量，且未配置整包清单：更新将回退为完整安装包下载。");
        }
        #[cfg(target_os = "linux")]
        if self.privilege_tools_available == Some(false) {
            warnings.push("未找到 runuser 或 su：提权更新后可能无法降权回原用户。");
        }
        warnings
    }
}

/// 读取上一次回滚原因（脚本写入，成功启动后清除）。
pub(super) fn read_rollback_reason(work_dir: &Path) -> Option<String> {
    let flag = work_dir.join(crate::core::artifacts::ROLLBACK_FLAG);
    let text = std::fs::read_to_string(flag).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// 读取最近的结构化脚本事件（按时间正序，最多 [`MAX_SCRIPT_EVENTS`] 条）。
pub(super) fn read_script_events(work_dir: &Path) -> Vec<ScriptEvent> {
    let path = work_dir.join(crate::core::artifacts::EVENTS_FILE);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut events: Vec<ScriptEvent> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<ScriptEvent>(line.trim()).ok())
        .collect();
    let drop_count = events.len().saturating_sub(MAX_SCRIPT_EVENTS);
    events.drain(..drop_count);
    events
}

/// 原版本的 Authenticode 结论；非 Windows 平台恒为 `None`。
///
/// 返回 `(是否可信, 检查是否未得出结论)`。
fn authenticode_diagnostics(exe: &Path) -> (Option<bool>, Option<bool>) {
    #[cfg(target_os = "windows")]
    {
        use crate::platform::authenticode::TrustState;
        match crate::platform::authenticode::trust_state(exe) {
            TrustState::Trusted => (Some(true), Some(false)),
            TrustState::NotTrusted => (Some(false), Some(false)),
            TrustState::Inconclusive => (Some(false), Some(true)),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = exe;
        (None, None)
    }
}

/// Linux 上是否至少找到一个降权工具；其它平台为 `None`。
fn privilege_tools_available() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let has_tool = ["runuser", "su"].iter().any(|tool| {
            std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|dir| dir.join(tool).is_file())
            })
        });
        Some(has_tool)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

impl DeltaUpdater {
    /// 环境诊断：告诉开发者"当前环境能不能走增量、失败日志去哪里看"。
    ///
    /// 返回的是**只读快照**，不做网络请求。典型用途：
    /// - UI 的"关于/诊断"页面；
    /// - 接入排障：安装目录是否只读、工作目录是否可写、
    ///   当前平台是否在增量适用边界内、是否已被安全模式暂停；
    /// - 支持人员按 `log_path` 收集日志。
    pub async fn diagnose(&self) -> Diagnosis {
        let install_dir = self.executable_path.parent();
        let (authenticode, authenticode_check_skipped) =
            authenticode_diagnostics(&self.executable_path);
        Diagnosis {
            executable: self.executable_path.clone(),
            work_dir: self.work_dir.clone(),
            platform: self.config.platform.clone(),
            work_dir_writable: crate::core::paths::ensure_writable(&self.work_dir).is_ok(),
            install_dir_writable: install_dir
                .map(crate::core::paths::is_writable)
                .unwrap_or(false),
            fallback_reason: crate::platform::target::incremental_fallback_reason(
                &self.executable_path,
            ),
            bundle_capable: self.config.bundle_manifest_url.is_some()
                && PlatformUpdater::bundle_supported_for(&self.executable_path),
            safe_mode: crate::core::safety::is_safe_mode_active(&self.work_dir).await,
            needs_repair: self.needs_bundle_repair(),
            startup_state: crate::core::startup::read(&self.work_dir),
            last_rollback: read_rollback_reason(&self.work_dir),
            authenticode,
            authenticode_check_skipped,
            log_path: self.work_dir.join(crate::core::artifacts::LOG_FILE),
            events_path: self.work_dir.join(crate::core::artifacts::EVENTS_FILE),
            script_events: read_script_events(&self.work_dir),
            privilege_tools_available: privilege_tools_available(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{read_script_events, ScriptEvent};

    /// 事件流是脚本追加写的：坏行必须跳过，缺失文件必须返回空。
    #[test]
    fn reads_script_events_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::core::artifacts::EVENTS_FILE);

        assert!(read_script_events(dir.path()).is_empty());

        std::fs::write(
            &path,
            concat!(
                "{\"event\":\"delta.rollback.triggered\",\"reason\":\"update_failed\",\"time\":\"2026-01-01T00:00:00Z\"}\n",
                "not json\n",
                "{\"event\":\"delta.rollback.triggered\",\"reason\":\"startup_timeout\"}\n",
            ),
        )
        .unwrap();

        let events = read_script_events(dir.path());
        assert_eq!(events.len(), 2, "坏行必须被跳过");
        assert_eq!(
            events[0],
            ScriptEvent {
                event: "delta.rollback.triggered".into(),
                reason: Some("update_failed".into()),
                time: Some("2026-01-01T00:00:00Z".into()),
            }
        );
        assert_eq!(events[1].reason.as_deref(), Some("startup_timeout"));
        assert_eq!(events[1].time, None);
    }
}
