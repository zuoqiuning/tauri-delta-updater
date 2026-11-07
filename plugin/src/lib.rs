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
use tauri_delta_updater::{CancelToken, DeltaUpdater, UpdateConfig};

struct UpdaterState {
    updater: Arc<DeltaUpdater>,
    /// 正在进行的更新会话的取消令牌；空闲时为 `None`。
    cancel: Mutex<Option<CancelToken>>,
}

/// 注册流程：构造更新器、确认上次启动、托管状态。
fn register<R: Runtime>(
    app: &AppHandle<R>,
    config: UpdateConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let updater = Arc::new(DeltaUpdater::new(config)?);

    // 本次启动是否来自一次替换：必须在窗口和前端起来之前确认，
    // 否则替换脚本会在 confirm_timeout 后误判失败并回滚。
    let confirmed = updater
        .confirm_startup_success_if_pending()
        .unwrap_or(false);
    let _ = app.emit("delta-updater://startup", confirmed);

    // 启动 doctor：配置缺口（工作目录不可写、macOS 未配整包、Linux 没有
    // 降权工具）在第一次启动就打到日志，而不是等用户触发更新才发现回退。
    // 诊断是只读快照，不发起网络请求，因此可以安全地在后台跑。
    let doctor = Arc::clone(&updater);
    tauri::async_runtime::spawn(async move {
        let diagnosis = doctor.diagnose().await;
        for warning in diagnosis.warnings() {
            eprintln!("[tauri-delta-updater] {warning}");
        }
    });

    app.manage(UpdaterState {
        updater,
        cancel: Mutex::new(None),
    });
    Ok(())
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
        let dir = std::env::temp_dir().join(format!("tdx-plugin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"app").unwrap();

        let config = UpdateConfig::new("https://example.com/latest.json", "PK")
            .current_version("0.1.0")
            .executable_path(&exe)
            .work_dir(&dir);

        let app = tauri::test::mock_builder()
            .plugin(init(config))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("插件初始化不应失败");

        assert!(
            app.try_state::<UpdaterState>().is_some(),
            "插件必须注册更新器状态"
        );
    }
}
