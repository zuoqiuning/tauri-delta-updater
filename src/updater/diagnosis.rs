//! 环境诊断：只读快照，不发起网络请求。

use crate::platform::full_install::InstallerKind;
use crate::platform::target::EnvironmentReason;
use crate::platform::PlatformUpdater;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{DeltaUpdater, SafeModeReason, UpdateInProgress};

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

/// 诊断摘要的可本地化键。
///
/// 与 [`Diagnosis::user_message`] 的返回值一一对应，但只给出稳定的语义键；
/// UI 应按 `MessageKey` 自行本地化，而不是直接展示硬编码中文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageKey {
    /// 环境正常，可走增量。
    EnvironmentOk,
    /// 安全模式暂停更新。
    SafeMode,
    /// 需要先修复上次中断的替换。
    NeedsRepair,
    /// 工作目录不可写。
    WorkDirNotWritable,
    /// 当前环境不支持原地增量。
    EnvironmentUnsupported,
    /// 新版本启动超时后回滚。
    StartupTimeout,
    /// 新版本未进入宿主代码。
    StartupNotStarted,
    /// 新版本启动后崩溃。
    StartupCrashed,
    /// 宿主健康检查失败。
    HealthCheckFailed,
    /// 替换脚本执行失败。
    UpdateFailed,
    /// 已修复中断的替换。
    RepairedPartialUpdate,
    /// 未知回滚原因。
    RollbackUnknown,
}

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
    /// 当前环境不适合**单文件**增量的结构化原因（只读 AppImage 目录、macOS bundle 等）。
    ///
    /// AppImage 本身是支持的：替换目标是 `$APPIMAGE` 指向的 `.AppImage` 本体；
    /// 只有定位不到本体或所在目录不可写时才填这里。
    pub fallback_reason: Option<EnvironmentReason>,
    /// 配置了整包清单且当前环境支持整包替换（macOS `.app` / Windows）。
    pub bundle_capable: bool,
    /// 是否处于安全模式。
    pub safe_mode: bool,
    /// 安全模式的触发原因；未处于安全模式时为 `None`。
    ///
    /// UI 可据此区分“可能被攻击（`security_block`）”与“磁盘/权限问题
    /// （`environment_failure`）”，给出不同的操作建议。
    pub safe_mode_reason: Option<SafeModeReason>,
    /// 连续全量安装未生效的次数（安装器启动失败，或下次启动超过宽限期仍未达标）。
    ///
    /// 与 [`Self::safe_mode`] **无关**：全量安装是用户显式选择或回退路径，
    /// 用户取消安装器、拒绝提权只会累计本字段，不会冻结增量通道。
    /// UI 可据此提示"全量安装未生效，请检查安装器或权限"。
    pub full_failures: u32,
    /// 一次尚未确认完成的替换（引擎启动了替换脚本，但新版本没有确认启动）。
    ///
    /// 正常启动为 `None`。`Some` 表示上一次更新没有走完：脚本崩溃、被杀或替换
    /// 卡住。UI 应提示用户"上次更新未完成"，并建议重新安装或等待下一次更新；
    /// 替换脚本在成功或回滚完成时会清除该标记。
    pub update_in_progress: Option<UpdateInProgress>,
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
    /// 成功后本字段为 `None`；历史回滚链见 [`Self::rollback_history`]。
    pub last_rollback: Option<String>,
    /// 历史回滚链：从 `tdx_events.jsonl` 过滤出的 `delta.rollback.triggered`。
    ///
    /// 与 [`Self::last_rollback`] 的区别：后者只反映当前未清除的回滚标记，
    /// 成功启动一次后就会被清空；本字段保留完整历史，适合诊断页展示
    /// "这个版本之前为什么回滚过"。最多保留最近 20 条。
    pub rollback_history: Vec<ScriptEvent>,
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
    /// 当前配置的 Authenticode 策略（审计用）。
    pub authenticode_policy: crate::platform::AuthenticodePolicy,
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
    /// 当前是否配置了明文 HTTP 端点（非回环地址）。
    ///
    /// 明文不影响完整性（清单/补丁仍验签），但会暴露版本号等元数据，
    /// 并让中间人可以看到更新活动的节奏。生产环境应改用 HTTPS。
    pub insecure_transport: bool,
    /// 第一个明文 HTTP 端点（非回环）；全部 HTTPS/回环时为 `None`。
    ///
    /// 只给宿主排障用：端点可能包含私有 CDN token，展示前请自行脱敏。
    pub insecure_endpoint: Option<String>,
    /// 配置警告的结构化键，与 [`Self::warnings`] 一一对应、顺序一致。
    ///
    /// UI 应按 key 本地化；`warnings()` 的中文文案只适合日志与调试。
    /// 由 [`DeltaUpdater::diagnose`] 填充。
    pub warning_keys: Vec<WarningKey>,
    /// 跨用户锁是否真正生效；`None` 表示未配置 `lock_scope: install_dir`。
    ///
    /// `Some(false)` 表示安装目录不可写、已退回工作目录锁：per-machine 安装的
    /// 多个用户账户仍可能并发替换（诊断页应提示）。
    pub machine_lock_active: Option<bool>,
    /// 最近一次更新尝试的结构化结果（成功、取消、失败都能区分）。
    ///
    /// 由 `update()` / `install()` / `prepare()` 结束时写入；记录缺失或损坏时
    /// 为 `None`。比解析 `script_events` 更直接：一眼看出"上次为什么失败"。
    pub last_attempt: Option<super::UpdateAttempt>,
    /// 配置显式声明的全量安装器类型；`None` 表示按文件后缀推断。
    ///
    /// 用 Inno Setup 打包的宿主必须显式配置，否则排障时无法从诊断页看出
    /// "为什么 `.exe` 被当成 NSIS 调用"。
    ///
    /// 该值只反映本引擎全量路径的配置；宿主若仍由官方 `tauri-plugin-updater`
    /// 执行全量安装，它不会被消费。
    pub installer_kind: Option<InstallerKind>,
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
    ///
    /// # 注意
    ///
    /// 返回的是**中文**摘要，适合快速调试/日志。面向多语言 UI 时应按
    /// `fallback_reason` / `last_rollback` / `safe_mode` 等结构化字段分支，
    /// 而不是直接展示本方法，否则会绑定中文文案。
    #[deprecated(note = "use message_key() and localize in the UI")]
    pub fn user_message(&self) -> &'static str {
        match self.message_key() {
            MessageKey::EnvironmentOk => "环境正常。",
            MessageKey::SafeMode => "增量更新已暂停；冷却后会自动重试，也可以手动下载完整安装包。",
            MessageKey::NeedsRepair => "上次更新未完成，应用正在恢复旧版本；重启后无需再次操作。",
            MessageKey::StartupTimeout => "新版本启动超时，已回滚到旧版本；请稍后重试。",
            MessageKey::StartupNotStarted => "新版本未能启动，已回滚到旧版本；请稍后重试。",
            MessageKey::StartupCrashed => "新版本启动后退出，已回滚到旧版本；请稍后重试。",
            MessageKey::HealthCheckFailed => "新版本健康检查未通过，已回滚到旧版本。",
            MessageKey::UpdateFailed => "上次更新未能完成，已回滚到旧版本。",
            MessageKey::RepairedPartialUpdate => "已修复上次中断的更新。",
            MessageKey::RollbackUnknown => "上次更新已回滚到旧版本。",
            MessageKey::WorkDirNotWritable => "更新目录不可写，请检查权限后重试。",
            MessageKey::EnvironmentUnsupported => "当前环境不支持原地增量更新，将使用完整安装包。",
        }
    }

    /// 与 [`Self::user_message`] 等价的稳定语义键，供 UI 本地化。
    pub fn message_key(&self) -> MessageKey {
        if self.safe_mode {
            return MessageKey::SafeMode;
        }
        if self.needs_repair {
            return MessageKey::NeedsRepair;
        }
        if let Some(reason) = self.last_rollback.as_deref() {
            return match reason {
                "startup_timeout" => MessageKey::StartupTimeout,
                "startup_not_started" => MessageKey::StartupNotStarted,
                "startup_crashed" => MessageKey::StartupCrashed,
                "health_check_failed" => MessageKey::HealthCheckFailed,
                "update_failed" => MessageKey::UpdateFailed,
                "repaired_partial_update" => MessageKey::RepairedPartialUpdate,
                _ => MessageKey::RollbackUnknown,
            };
        }
        if !self.work_dir_writable {
            return MessageKey::WorkDirNotWritable;
        }
        if self.fallback_reason.is_some() && !self.bundle_capable {
            return MessageKey::EnvironmentUnsupported;
        }
        MessageKey::EnvironmentOk
    }

    /// 启动阶段可操作的配置警告；空表示没有发现明显缺口。
    ///
    /// 与 [`Self::user_message`] 的区别：这里只报告**配置/环境**问题，
    /// 不包含"上次为什么回滚"这类历史事件。插件在注册时会把它们打到
    /// 启动日志，让接入方第一次运行就看到问题，而不是等用户触发更新。
    ///
    /// 返回的是中文文案；面向多语言 UI 请使用 [`Self::warning_keys`]。
    pub fn warnings(&self) -> Vec<&'static str> {
        self.warning_keys()
            .into_iter()
            .map(WarningKey::text)
            .collect()
    }

    /// 配置警告的结构化键：UI 可据此本地化，不必匹配中文文案。
    pub fn warning_keys(&self) -> Vec<WarningKey> {
        let mut keys = Vec::new();
        if !self.work_dir_writable {
            keys.push(WarningKey::WorkDirNotWritable);
        }
        if self.fallback_reason.is_some() && !self.bundle_capable {
            keys.push(WarningKey::EnvironmentUnsupported);
        }
        if self.insecure_transport {
            keys.push(WarningKey::InsecureTransport);
        }
        if self.machine_lock_active == Some(false) {
            keys.push(WarningKey::MachineLockFallback);
        }
        #[cfg(target_os = "linux")]
        if self.privilege_tools_available == Some(false) {
            keys.push(WarningKey::PrivilegeToolsMissing);
        }
        keys
    }
}

/// 配置警告的稳定语义键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WarningKey {
    /// 工作目录不可写：所有更新都会失败。
    WorkDirNotWritable,
    /// 当前环境不支持单文件增量，且未配置整包清单。
    EnvironmentUnsupported,
    /// 清单端点使用明文 HTTP（非回环）。
    InsecureTransport,
    /// 配置了 `lock_scope: install_dir`，但安装目录只读、跨用户锁未生效。
    MachineLockFallback,
    /// Linux 上缺少 `runuser` / `su`，提权后无法降权。
    PrivilegeToolsMissing,
}

impl WarningKey {
    /// 面向开发者日志的中文说明。
    pub fn text(self) -> &'static str {
        match self {
            Self::WorkDirNotWritable => {
                "更新工作目录不可写：请检查目录权限，否则所有更新都会失败。"
            }
            Self::EnvironmentUnsupported => {
                "当前环境不支持单文件增量，且未配置整包清单：更新将回退为完整安装包下载。"
            }
            Self::InsecureTransport => {
                "清单端点使用明文 HTTP：完整性仍由签名保证，但版本信息会暴露在传输中，生产环境请改用 HTTPS。"
            }
            Self::MachineLockFallback => {
                "已配置跨用户锁，但安装目录不可写：已退回工作目录锁，多用户同时更新仍可能冲突。"
            }
            Self::PrivilegeToolsMissing => {
                "未找到 runuser 或 su：提权更新后可能无法降权回原用户。"
            }
        }
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
    /// 返回的是**本地只读快照**：不发网络请求；但可写性判断会写入并删除一个
    /// 探针文件，Windows 上还会执行一次 Authenticode 检查（可能较慢）。
    /// 典型用途：
    /// - UI 的"关于/诊断"页面；
    /// - 接入排障：安装目录是否只读、工作目录是否可写、
    ///   当前平台是否在增量适用边界内、是否已被安全模式暂停；
    /// - 支持人员按 `log_path` 收集日志。
    pub async fn diagnose(&self) -> Diagnosis {
        let install_dir = self.executable_path.parent();
        // WinVerifyTrust 是同步系统调用，离线时证书链探测可能很慢：
        // 放 blocking 线程，避免卡住其它命令。
        let (authenticode, authenticode_check_skipped) = {
            let exe = self.executable_path.clone();
            tokio::task::spawn_blocking(move || authenticode_diagnostics(&exe))
                .await
                .unwrap_or((None, None))
        };
        let script_events = read_script_events(&self.work_dir);
        let rollback_history = script_events
            .iter()
            .filter(|event| event.event == crate::core::artifacts::EVENT_ROLLBACK)
            .cloned()
            .collect();
        let safe_mode = crate::core::safety::is_safe_mode_active(&self.work_dir).await;
        let safe_mode_reason = if safe_mode {
            crate::core::safety::last_failure_reason(&self.work_dir).await
        } else {
            None
        };
        let mut diagnosis = Diagnosis {
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
            safe_mode,
            safe_mode_reason,
            full_failures: crate::core::safety::full_failures(&self.work_dir).await,
            update_in_progress: crate::updater::swap_marker::read(&self.work_dir),
            needs_repair: self.needs_bundle_repair(),
            startup_state: crate::core::startup::read(&self.work_dir),
            last_rollback: read_rollback_reason(&self.work_dir),
            rollback_history,
            authenticode,
            authenticode_check_skipped,
            authenticode_policy: self.config.authenticode_policy,
            log_path: self.work_dir.join(crate::core::artifacts::LOG_FILE),
            events_path: self.work_dir.join(crate::core::artifacts::EVENTS_FILE),
            script_events,
            privilege_tools_available: privilege_tools_available(),
            insecure_transport: self.config.uses_insecure_transport(),
            insecure_endpoint: self.config.insecure_endpoint().map(str::to_string),
            warning_keys: Vec::new(),
            machine_lock_active: (self.config.lock_scope == crate::LockScope::InstallDir).then(
                || {
                    !self
                        .work_dir
                        .join(crate::core::artifacts::MACHINE_LOCK_FALLBACK_FILE)
                        .exists()
                },
            ),
            last_attempt: super::attempt::read(&self.work_dir),
            installer_kind: self.config.installer_kind,
        };
        // 键由最终字段推导，避免构造顺序造成的漂移。
        diagnosis.warning_keys = diagnosis.warning_keys();
        diagnosis
    }

    /// 诊断包：`diagnose()` 快照 + 脚本日志尾部 + 原始事件流，一份 JSON 文本。
    ///
    /// 用户报障时只需把这段文本写入文件发过来，维护者不必再逐个目录找日志。
    /// 不包含密钥、补丁内容或安装包字节；[`Diagnosis::insecure_endpoint`]
    /// 可能含 CDN token，公开展示前请自行脱敏。
    ///
    /// ```no_run
    /// # async fn example(updater: &tauri_delta_updater::DeltaUpdater) {
    /// let bundle = updater.diagnostic_bundle().await;
    /// std::fs::write("diagnostics.json", bundle).unwrap();
    /// # }
    /// ```
    pub async fn diagnostic_bundle(&self) -> String {
        let diagnosis = self.diagnose().await;
        let log_tail = tail_lines(&self.work_dir.join(crate::core::artifacts::LOG_FILE), 200);
        let events_tail = tail_lines(
            &self.work_dir.join(crate::core::artifacts::EVENTS_FILE),
            200,
        );
        let generated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bundle = serde_json::json!({
            "schema": 1,
            "generated_at": generated_at,
            "engine_version": env!("CARGO_PKG_VERSION"),
            "current_version": self.config.current_version,
            "diagnosis": diagnosis,
            "log_tail": log_tail,
            "events_tail": events_tail,
        });
        serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| bundle.to_string())
    }
}

/// 文件最后 `max` 行；文件不存在或不可读时为空。
fn tail_lines(path: &std::path::Path, max: usize) -> Vec<String> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    lines[lines.len().saturating_sub(max)..]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
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

    /// 诊断包必须自带快照与两份尾部日志，用户只发一个文件就能排障。
    #[tokio::test]
    async fn diagnostic_bundle_contains_diagnosis_and_tails() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"x").unwrap();
        let updater = crate::updater::DeltaUpdater::new(
            crate::updater::UpdateConfig::new("https://cdn.example.com/latest.json", "dGVzdA==")
                .current_version("0.1.0")
                .executable_path(&exe)
                .work_dir(dir.path()),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(crate::core::artifacts::LOG_FILE),
            "line1\nline2\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(crate::core::artifacts::EVENTS_FILE),
            "{\"event\":\"delta.x\"}\n",
        )
        .unwrap();
        crate::core::safety::record_full_failure_sync(
            dir.path(),
            crate::updater::SafeModeReason::EnvironmentFailure,
        );
        crate::updater::swap_marker::write(dir.path(), "0.2.0");

        let bundle: serde_json::Value =
            serde_json::from_str(&updater.diagnostic_bundle().await).unwrap();
        assert_eq!(bundle["schema"], 1);
        assert_eq!(bundle["current_version"], "0.1.0");
        assert_eq!(bundle["log_tail"][1], "line2");
        assert_eq!(bundle["events_tail"][0], "{\"event\":\"delta.x\"}");
        assert_eq!(
            bundle["diagnosis"]["full_failures"], 1,
            "全量失败计数必须进入诊断快照"
        );
        assert_eq!(
            bundle["diagnosis"]["update_in_progress"]["version"], "0.2.0",
            "未完成的替换必须进入诊断快照"
        );
        assert!(bundle["diagnosis"]["work_dir"].is_string());
    }
}
