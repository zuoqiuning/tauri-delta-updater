//! Tauri v2 插件外壳。
//!
//! 只做三件事：注册命令、把阶段事件推给前端、在启动早期完成更新确认。
//! 所有更新逻辑都在 [`tauri_delta_updater`] 里，插件不复制任何一份。
//!
//! ## 接入
//!
//! ```ignore
//! // src-tauri/src/lib.rs
//! tauri::Builder::default()
//!     .plugin(tauri_plugin_delta_updater::init_from_config())
//!     .run(tauri::generate_context!())?;
//! ```
//!
//! ```json
//! // src-tauri/tauri.conf.json —— 与官方 updater 同形
//! {
//!   "plugins": {
//!     "delta-updater": {
//!       "endpoints": ["https://cdn.example.com/latest.json"],
//!       "pubkey": "RWQ..."
//!     }
//!   }
//! }
//! ```
//!
//! 找不到 `plugins.delta-updater` 时回退到 `plugins.updater`，从官方插件迁移
//! 只需替换依赖。需要运行期计算配置时用 `init(UpdateConfig)`。
//!
//! `src-tauri/capabilities/default.json` 需要 `"delta-updater:default"`。
//!
//! ## 前端契约
//!
//! ```ignore
//! import { invoke, Channel } from '@tauri-apps/api/core';
//!
//! const events = new Channel();
//! events.onmessage = (e) => {
//!   // e.stage: checking | downloading | applying | ready | up_to_date
//!   //          | deferred | fallback | repairing | safe_mode | cancelled
//!   // deferred = 新版本存在，但当前安装不在灰度范围内
//!   if (e.stage === 'downloading') setPercent(e.downloaded, e.total);
//! };
//!
//! await invoke('plugin:delta-updater|check');            // 只检查，不下载
//! await invoke('plugin:delta-updater|install', { onEvent: events });
//! // 需要自己控制退出时机（先保存、先提示）：prepare 只下载合并，launch 触发替换并退出
//! await invoke('plugin:delta-updater|prepare', { onEvent: events }); // → kind: "ready"
//! await invoke('plugin:delta-updater|launch');                       // → kind: "updated"，进程退出
//! await invoke('plugin:delta-updater|download_full', { onEvent: events }); // 全量回退：仅下载
//! await invoke('plugin:delta-updater|install_full', { onEvent: events });  // 全量回退：下载并安装
//! await invoke('plugin:delta-updater|cancel');           // 任意时刻取消
//! ```
//!
//! 命令返回的错误是 `{ code, message, retryable, security }`：
//! `code` 是稳定的机器可读错误码（见 [`DeltaError::code`]），UI 按它分支即可。

mod commands;
mod config;
mod dto;

pub use commands::{init, init_from_config};
pub use dto::{ErrorDto, FullDownloadDto};

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_delta_updater::{CancelToken, DeltaUpdater, Staged, StagedBundle, UpdateConfig};

/// 已就绪、等待 `launch` 的更新产物（`prepare` 路径）。
#[derive(Clone)]
enum StagedPending {
    Exe(Staged),
    Bundle(StagedBundle),
}

impl StagedPending {
    fn version(&self) -> String {
        match self {
            StagedPending::Exe(staged) => staged.manifest().version.clone(),
            StagedPending::Bundle(staged) => staged.manifest().version.clone(),
        }
    }

    fn launch(&self) -> tauri_delta_updater::Result<()> {
        match self {
            StagedPending::Exe(staged) => staged.launch(),
            StagedPending::Bundle(staged) => staged.launch(),
        }
    }
}

struct UpdaterState {
    /// 构造成功时可用；失败时为 `None`，命令统一返回 `updater_unavailable`。
    ///
    /// 更新器不可用（缓存目录只读、配置缺失）**不能**阻止宿主应用启动：
    /// 插件 setup 返回 Err 会让 Tauri 的 `App::build` 直接失败。
    updater: Option<Arc<DeltaUpdater>>,
    unavailable: Option<String>,
    /// 正在进行的更新会话的取消令牌；空闲时为 `None`。
    cancel: Mutex<Option<CancelToken>>,
    /// `prepare` 生成、等待 `launch` 的产物。
    pending: Mutex<Option<StagedPending>>,
}

impl UpdaterState {
    fn ready(updater: Arc<DeltaUpdater>) -> Self {
        Self {
            updater: Some(updater),
            unavailable: None,
            cancel: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    fn unavailable(reason: String) -> Self {
        Self {
            updater: None,
            unavailable: Some(reason),
            cancel: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    /// 取更新器；不可用时返回带稳定错误码的 DTO。
    fn updater(&self) -> Result<&Arc<DeltaUpdater>, ErrorDto> {
        self.updater.as_ref().ok_or_else(|| {
            ErrorDto::unavailable(
                self.unavailable
                    .clone()
                    .unwrap_or_else(|| "更新器不可用".to_string()),
            )
        })
    }

    fn set_pending(&self, pending: Option<StagedPending>) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = pending;
        }
    }

    /// 读取待启动产物（克隆共享状态，启动失败时仍可重试）。
    fn pending(&self) -> Option<StagedPending> {
        self.pending.lock().ok().and_then(|slot| slot.clone())
    }
}

/// 注册流程：构造更新器、确认上次启动、托管状态。
///
/// 任何构造失败都降级为"更新功能不可用"，不影响应用本身启动。
fn register<R: Runtime>(app: &AppHandle<R>, config: Result<UpdateConfig, String>) {
    let state =
        match config.and_then(|config| DeltaUpdater::new(config).map_err(|err| err.to_string())) {
            Ok(updater) => {
                let updater = Arc::new(updater);

                // 本次启动是否来自一次替换：必须在窗口和前端起来之前确认，
                // 否则替换脚本会在 confirm_timeout 后误判失败并回滚。
                let confirmed = updater
                    .confirm_startup_success_if_pending()
                    .unwrap_or(false);
                let _ = app.emit("delta-updater://startup", confirmed);

                // 跨重启待安装：配置开启时在本次启动尽早执行，并退出进程。
                // 记录经过版本单调性与产物哈希校验；不联网。
                if updater.install_on_next_launch() {
                    let auto = Arc::clone(&updater);
                    tauri::async_runtime::spawn(async move {
                        match auto.install_pending().await {
                            Ok(true) => std::process::exit(0),
                            Ok(false) => {}
                            Err(err) => {
                                let message = format!("[startup] 自动安装待更新失败: {err}");
                                append_log(auto.work_dir(), &[message.as_str()]);
                            }
                        }
                    });
                }

                // 启动 doctor：配置缺口在第一次启动就打到日志/事件，
                // 而不是等用户触发更新才发现回退。
                let doctor = Arc::clone(&updater);
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let diagnosis = doctor.diagnose().await;
                    let warnings = diagnosis.warnings();
                    if warnings.is_empty() {
                        return;
                    }
                    append_log(doctor.work_dir(), &warnings);
                    let _ = handle.emit("delta-updater://diagnosis", warnings);
                });

                UpdaterState::ready(updater)
            }
            Err(reason) => {
                let _ = app.emit("delta-updater://unavailable", reason.clone());
                UpdaterState::unavailable(reason)
            }
        };

    if let Some(reason) = &state.unavailable {
        eprintln!("[tauri-delta-updater] 更新器不可用: {reason}");
    }
    app.manage(state);
}

/// 把启动警告追加到工作目录日志。
///
/// GUI 应用没有控制台，`eprintln!` 在生产环境不可见；日志文件是排障的唯一入口。
fn append_log(work_dir: &std::path::Path, warnings: &[&str]) {
    use std::io::Write;
    let path = work_dir.join(tauri_delta_updater::core::artifacts::LOG_FILE);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    for warning in warnings {
        let _ = writeln!(file, "[startup] {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::{init, ErrorDto, UpdaterState};
    use crate::config::FileConfig;
    use tauri::Manager;
    use tauri_delta_updater::{DeltaError, UpdateConfig};

    #[test]
    fn error_dto_carries_stable_code() {
        let dto = ErrorDto::from(DeltaError::UpdateInProgress);
        assert_eq!(dto.code, "update_in_progress");
        assert!(!dto.retryable && !dto.security);

        let dto = ErrorDto::from(DeltaError::SignatureInvalid("bad".into()));
        assert_eq!(dto.code, "signature_invalid");
        assert!(dto.security, "签名失败必须能被前端识别为安全阻断");
    }

    /// `tauri.conf.json` 的字段与官方 updater 兼容，迁移时可直接复用。
    #[test]
    fn file_config_is_updater_compatible() {
        let value = serde_json::json!({
            "endpoints": ["https://cdn.example.com/latest.json"],
            "pubkey": "RWQ...",
            "bundle_endpoint": "https://cdn.example.com/latest.bundle.json",
            "install_id": "host-provided-install-id"
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        let config = file.into_update_config("1.2.3").unwrap();
        assert_eq!(config.manifest_url, "https://cdn.example.com/latest.json");
        assert_eq!(config.current_version, "1.2.3");
        assert_eq!(
            config.bundle_manifest_url.as_deref(),
            Some("https://cdn.example.com/latest.bundle.json")
        );
        assert_eq!(
            config.install_id.as_deref(),
            Some("host-provided-install-id"),
            "插件配置必须能注入宿主的稳定安装 ID"
        );
    }

    /// 工作目录、超时、代理与端点策略必须能从 `tauri.conf.json` 配置。
    #[test]
    fn file_config_exposes_runtime_controls() {
        use tauri_delta_updater::{FallbackPolicy, InstallMode};

        let value = serde_json::json!({
            "endpoints": [
                "https://cdn.example.com/latest.json",
                "https://backup.example.com/latest.json"
            ],
            "pubkey": "PK",
            "work_dir": "/tmp/tdx",
            "wait_timeout_secs": 20,
            "confirm_timeout_secs": 120,
            "proxy": "http://127.0.0.1:7890",
            "allow_legacy_signatures": false,
            "fallback_policy": "any",
            "headers": { "X-Auth": "token" },
            "install_mode": "quiet",
            "allow_insecure_transport": false,
            "install_on_next_launch": true
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        let config = file.into_update_config("1.0.0").unwrap();
        assert_eq!(
            config.work_dir.as_deref(),
            Some(std::path::Path::new("/tmp/tdx"))
        );
        assert_eq!(config.wait_timeout_secs, 20);
        assert_eq!(config.confirm_timeout_secs, 120);
        assert!(config.client.is_some(), "代理/请求头必须注入自定义客户端");
        assert!(
            !config.allow_legacy_signatures,
            "严格签名模式必须能从配置开启"
        );
        assert_eq!(
            config.fallback_manifest_urls,
            vec!["https://backup.example.com/latest.json".to_string()],
            "endpoints 数组的其余项必须作为备用端点"
        );
        assert!(matches!(
            config.fallback_policy,
            FallbackPolicy::OnAnyFailure
        ));
        assert_eq!(config.install_mode, InstallMode::Quiet);
        assert!(!config.allow_insecure_transport);
        assert!(config.install_on_next_launch);
    }

    /// 非法端点策略/安装模式必须在启动阶段报错，而不是静默忽略。
    #[test]
    fn invalid_runtime_controls_are_rejected() {
        for (key, value) in [
            ("fallback_policy", "sometimes"),
            ("install_mode", "silentish"),
        ] {
            let mut raw = serde_json::json!({
                "endpoints": ["https://cdn.example.com/latest.json"],
                "pubkey": "PK"
            });
            raw[key] = value.into();
            let file: FileConfig = serde_json::from_value(raw).unwrap();
            let err = file.into_update_config("1.0.0").unwrap_err();
            assert!(err.contains(key), "错误必须说明字段 {key}: {err}");
        }
    }

    /// 非法代理必须在启动阶段报错，而不是等到第一次请求才失败。
    #[test]
    fn invalid_proxy_is_rejected() {
        let value = serde_json::json!({
            "endpoints": ["https://cdn.example.com/latest.json"],
            "pubkey": "PK",
            "proxy": "http://["
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        let err = file.into_update_config("1.0.0").unwrap_err();
        assert!(err.contains("proxy"), "错误必须说明字段: {err}");
    }

    /// 官方配置里的额外字段（windows/dmg 等）必须被忽略，而不是报错。
    #[test]
    fn file_config_ignores_official_extra_fields() {
        let value = serde_json::json!({
            "endpoints": ["https://cdn.example.com/latest.json"],
            "pubkey": "RWQ...",
            "windows": { "installMode": "quiet" },
            "dangerousInsecureTransportProtocol": true
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        assert!(file.bundle_endpoint.is_none());
        assert!(file.into_update_config("1.0.0").is_ok());
    }

    /// endpoints 为空时必须明确报错，而不是静默退化成"永远没有更新"。
    #[test]
    fn empty_endpoints_is_rejected() {
        let file = FileConfig {
            endpoints: Vec::new(),
            pubkey: "PK".into(),
            bundle_endpoint: None,
            bundle_type: None,
            install_id: None,
            work_dir: None,
            wait_timeout_secs: None,
            confirm_timeout_secs: None,
            proxy: None,
            allow_legacy_signatures: None,
            fallback_policy: None,
            headers: None,
            install_mode: None,
            allow_insecure_transport: None,
            install_on_next_launch: None,
        };
        let err = file.into_update_config("1.0.0").unwrap_err();
        assert!(err.contains("endpoints"), "错误必须说明字段: {err}");
    }

    /// 配置 `bundle_type` 后 `{{bundle_type}}` 必须展开。
    #[test]
    fn expands_bundle_type_when_configured() {
        let value = serde_json::json!({
            "endpoints": ["https://cdn.example.com/{{bundle_type}}/latest.json"],
            "pubkey": "PK",
            "bundle_type": "nsis"
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        let config = file.into_update_config("1.2.3").unwrap();
        assert_eq!(
            config.manifest_url,
            "https://cdn.example.com/nsis/latest.json"
        );
    }

    /// 用了 `{{bundle_type}}` 却没配置时，必须在启动阶段报错，
    /// 而不是把带花括号的 URL 发给服务端。
    #[test]
    fn bundle_type_placeholder_without_config_is_rejected() {
        let value = serde_json::json!({
            "endpoints": ["https://cdn.example.com/{{bundle_type}}/latest.json"],
            "pubkey": "PK"
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        let err = file.into_update_config("1.2.3").unwrap_err();
        assert!(err.contains("bundle_type"), "错误必须说明字段: {err}");
    }

    /// 官方 `{{target}}` / `{{arch}}` / `{{current_version}}` 占位符必须展开。
    #[test]
    fn expands_official_endpoint_placeholders() {
        let value = serde_json::json!({
            "endpoints": ["https://cdn.example.com/{{target}}/{{arch}}/{{current_version}}/latest.json"],
            "pubkey": "PK",
            "bundle_endpoint": "https://cdn.example.com/{{target}}/{{arch}}/latest.bundle.json"
        });
        let file: FileConfig = serde_json::from_value(value).unwrap();
        let config = file.into_update_config("1.2.3").unwrap();

        let platform = tauri_delta_updater::current_platform();
        let (target, arch) = platform.split_once('-').unwrap();
        assert_eq!(
            config.manifest_url,
            format!("https://cdn.example.com/{target}/{arch}/1.2.3/latest.json")
        );
        assert_eq!(
            config.bundle_manifest_url.as_deref(),
            Some(format!("https://cdn.example.com/{target}/{arch}/latest.bundle.json").as_str())
        );
        assert!(!config.manifest_url.contains("{{"));
    }

    /// 用 mock runtime 验证插件确实注册成功、状态可被命令读取。
    ///
    /// 不需要窗口或 WebView，因此能在 CI 里当作编译级回归。
    #[test]
    fn plugin_registers_updater_state() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"app").unwrap();

        let config = UpdateConfig::new("https://example.com/latest.json", "PK")
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(dir.path());

        let app = tauri::test::mock_builder()
            .plugin(init(config))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("插件初始化不应失败");

        let state = app.try_state::<UpdaterState>().expect("插件必须注册状态");
        assert!(state.updater.is_some(), "可写目录下更新器必须可用");
    }

    /// 工作目录不可写时，插件只能降级功能，绝不能阻止宿主应用启动。
    #[test]
    fn plugin_does_not_block_startup_when_work_dir_is_unwritable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"app").unwrap();
        // 用一个文件作为父目录，`create_dir_all` 必然失败
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, b"not a dir").unwrap();

        let config = UpdateConfig::new("https://example.com/latest.json", "PK")
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(blocker.join("work"));

        let app = tauri::test::mock_builder()
            .plugin(init(config))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("更新器不可用不得阻止应用启动");

        let state = app.try_state::<UpdaterState>().expect("插件仍须注册状态");
        assert!(state.updater.is_none(), "工作目录不可写时必须降级");
        let err = state.updater().expect_err("命令必须返回明确错误");
        assert_eq!(err.code, "updater_unavailable");
    }

    /// 缺少 `plugins.delta-updater` 配置时同样只降级，不阻止启动。
    #[test]
    fn plugin_does_not_block_startup_without_config() {
        let app = tauri::test::mock_builder()
            .plugin(crate::init_from_config())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("配置缺失不得阻止应用启动");

        let state = app.try_state::<UpdaterState>().expect("插件仍须注册状态");
        assert!(state.updater.is_none());
        assert_eq!(
            state.updater().expect_err("必须给出不可用原因").code,
            "updater_unavailable"
        );
    }
}
