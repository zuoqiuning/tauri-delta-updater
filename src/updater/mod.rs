//! 高层更新流程。
//!
//! 提供三种粒度，接入方按需选择：
//!
//! - [`DeltaUpdater::check`]：只检查是否有新版本，不下载任何东西。
//!   用于 UI 的"发现新版本"阶段。
//! - [`DeltaUpdater::prepare`]：下载并合并补丁，**不启动替换脚本**。
//!   用于宿主需要自己控制替换时机（先弹提示、再退出）的场景。
//! - [`DeltaUpdater::update`]：一步到位，检查 → 下载 → 合并 → 启动替换。
//!
//! 只想快速接入时用 [`DeltaUpdater::update`]；
//! 需要自己控制每一步时，仍可直接使用 `core` 下的各个底层函数。

use crate::core::paths::{default_work_dir, ensure_writable};
use crate::error::{DeltaError, Result};
use crate::session::UpdateEvent;
use std::path::{Path, PathBuf};

mod bundle_flow;
mod config;
mod delta;
mod diagnosis;
mod fetch;
mod flow;
mod full;
mod outcome;
mod pending;
mod plan;
mod rollout;
mod simulate;
mod staged;

pub use config::{default_signature_url, FallbackPolicy, LockScope, UpdateConfig};
pub use diagnosis::{Diagnosis, MessageKey, ScriptEvent};
pub use outcome::{
    CheckOutcome, FallbackReason, OsRequirement, SafeModeReason, UpdateCheck, UpdateOutcome,
    UpdateResult,
};
pub use pending::{PendingArtifact, PendingUpdate};
pub use simulate::{simulate_update, SimulationReport};
pub use staged::{
    FullDownload, FullDownloadOutcome, Staged, StagedBundle, StagedFullInstaller, StagedRepair,
};

/// 版本/系统/灰度门槛的统一结果（见 [`DeltaUpdater::evaluate_gates`]）。
#[derive(Debug, Default)]
pub(super) struct UpdateGates {
    pub(super) is_newer: bool,
    pub(super) rollout_percentage: Option<u8>,
    pub(super) deferred_by_rollout: bool,
    pub(super) unsupported_os: Option<(String, String)>,
}

/// 增量更新器。
#[derive(Debug)]
pub struct DeltaUpdater {
    config: UpdateConfig,
    executable_path: PathBuf,
    /// 用户可写的工作目录：补丁、暂存文件、脚本、日志、标志。
    work_dir: PathBuf,
    client: reqwest::Client,
}

/// 校验端点必须是带主机名的 http(s) URL。
fn validate_http_url(field: &str, url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| DeltaError::Config(format!("{field} 不是合法 URL（{url}）: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(DeltaError::Config(format!(
            "{field} 必须是带主机名的 http(s) URL: {url}"
        )));
    }
    Ok(())
}

impl DeltaUpdater {
    /// 按配置创建更新器。
    ///
    /// 会解析出被更新的可执行文件与工作目录，并**在构造阶段**验证工作目录可写。
    ///
    /// 配置里没指定可执行文件时使用当前进程的可执行文件；
    /// 无法确定时返回错误——退化成相对路径会让补丁与产物落到意料之外的目录，
    /// 比直接失败更难排查。
    pub fn new(config: UpdateConfig) -> Result<Self> {
        let mut config = config;

        // 清单地址与公钥是必需项：留空时错误会以"网络请求失败/签名不匹配"
        // 的形式出现在很远的地方，先在这里给出明确原因。
        if config.manifest_url.trim().is_empty() {
            return Err(DeltaError::Config(
                "manifest_url 不能为空：它指向 latest.json".into(),
            ));
        }
        if config.public_key.trim().is_empty() {
            return Err(DeltaError::Config(
                "public_key 不能为空：用于校验清单签名".into(),
            ));
        }
        if config
            .additional_public_keys
            .iter()
            .any(|key| key.trim().is_empty())
        {
            return Err(DeltaError::Config(
                "additional_public_keys 不能包含空公钥".into(),
            ));
        }
        if let Some(url) = &config.bundle_manifest_url {
            if url.trim().is_empty() {
                return Err(DeltaError::Config(
                    "bundle_manifest_url 不能为空：它指向 latest.bundle.json".into(),
                ));
            }
        }

        // 移动端与未知目标没有替换脚本：在构造阶段就拒绝，
        // 而不是让宿主走完检查/下载后才在生成脚本时失败。
        let platform_os = config.platform.split_once('-').map(|(os, _)| os);
        if !matches!(platform_os, Some("windows" | "darwin" | "linux")) {
            return Err(DeltaError::Config(format!(
                "不支持的平台 {:?}：本引擎只支持 Windows、macOS 与 Linux 桌面端",
                config.platform
            )));
        }
        if config
            .fallback_manifest_urls
            .iter()
            .any(|url| url.trim().is_empty())
        {
            return Err(DeltaError::Config(
                "fallback_endpoints 不能包含空地址".into(),
            ));
        }
        // 明文 HTTP 只允许回环地址；非回环端点默认仅警告，
        // 宿主可显式关闭（allow_insecure_transport(false)）直接拒绝。
        // 签名地址与整包清单同样参与检查：它们也承载受信任内容。
        if !config.allow_insecure_transport {
            if let Some(url) = config.insecure_endpoint() {
                return Err(DeltaError::Config(format!(
                    "端点使用明文 HTTP 且未允许：{url}（改用 HTTPS 或设置 allow_insecure_transport）"
                )));
            }
        }

        // 与发布端保持同一套版本格式：去掉首尾空白与 `v` 前缀。
        // 否则 `" 1.2.3 "` 或 `"v1.2.3"` 会与清单里的 `1.2.3` 匹配不上，
        // 表现为"永远没有补丁"这种最难排查的症状。
        config.current_version =
            crate::platform::target::normalize_version(&config.current_version).to_string();

        // 手工改过 manifest_url 但没同步 signature_url 时，空值会让验签从一个
        // 不存在的地址拉取；按清单地址重新推导，保持构造器的默认不变式。
        if config.signature_url.trim().is_empty() {
            config.signature_url =
                crate::updater::config::default_signature_url(&config.manifest_url);
        }

        // 所有会发起请求的端点都必须是带主机名的 http(s) URL。
        // 错 scheme 在第一次请求时只会变成难懂的 builder error，必须提前拒绝。
        validate_http_url("manifest_url", &config.manifest_url)?;
        validate_http_url("signature_url", &config.signature_url)?;
        for (index, url) in config.fallback_manifest_urls.iter().enumerate() {
            validate_http_url(&format!("fallback_endpoints[{index}]"), url)?;
        }
        if let Some(url) = &config.bundle_manifest_url {
            validate_http_url("bundle_manifest_url", url)?;
        }

        // current_version 为空时 find_patch 永远返回 None，会静默退化成全量下载；
        // 不是语义化版本时无法做降级判据。两者都是最难排查的接入错误，
        // 必须在构造时就拒绝。
        if config.current_version.is_empty() {
            return Err(DeltaError::Config(
                "current_version 不能为空：它决定匹配哪个版本的补丁".into(),
            ));
        }

        // 0 秒会让脚本立刻判定"旧进程没有退出"并中止，本质上是配置错误。
        if config.wait_timeout_secs == 0 || config.confirm_timeout_secs == 0 {
            return Err(DeltaError::Config(
                "wait_timeout_secs / confirm_timeout_secs 必须大于 0".into(),
            ));
        }
        // NUL 会截断最终传给安装器的命令行：提前拒绝，避免参数被悄悄丢掉。
        if config.installer_args.iter().any(|arg| arg.contains('\0')) {
            return Err(DeltaError::Config(
                "installer_args 不能包含 NUL 字符（会截断 Windows 命令行）".into(),
            ));
        }
        semver::Version::parse(&config.current_version).map_err(|e| {
            DeltaError::Config(format!(
                "current_version {:?} 不是合法的语义化版本（应形如 1.2.3）: {e}",
                config.current_version
            ))
        })?;

        // 确保 TLS 后端可用：宿主自建客户端时也必须在创建前安装，
        // 这里提前装好，后续任何客户端都能直接使用。
        crate::core::crypto::install_crypto_provider();

        let executable_path = match &config.executable_path {
            Some(path) => path.clone(),
            // AppImage 下 current_exe() 指向临时挂载点，真正该替换的是 $APPIMAGE。
            None => crate::platform::target::runtime_executable().map_err(|e| {
                DeltaError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("无法确定当前可执行文件路径，请显式设置 executable_path: {e}"),
                ))
            })?,
        };

        let work_dir = match &config.work_dir {
            Some(dir) => dir.clone(),
            None => default_work_dir(&executable_path),
        };

        // 构造阶段就验证可写性：系统级安装目录只读时，
        // 等到下载完成才失败会让用户白等一次，并累计进安全模式。
        ensure_writable(&work_dir)?;
        // 长期运行的应用会无限追加日志：每次启动检查一次大小并轮转。
        crate::core::disk::rotate_logs(&work_dir);

        // 替换脚本产生的备份必须与目标 exe 同目录，因此 exe 必须在某个目录下
        if executable_path.parent().is_none() {
            return Err(DeltaError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "无法确定可执行文件所在目录",
            )));
        }

        let client = match config.client.clone() {
            Some(client) => {
                // 自定义客户端拥有完整网络配置（headers/代理/超时）；
                // 再叠加 headers 会被静默忽略，因此直接拒绝，避免“配了却没生效”。
                if !config.headers.is_empty() {
                    return Err(DeltaError::Config(
                        "headers 只能配合内置客户端；自定义 client 请自行设置默认请求头".into(),
                    ));
                }
                client
            }
            // 默认客户端带超时：连接 15 秒、单次读空闲 60 秒。
            // 不设总超时——大补丁在慢网络下可能合法地下很久，
            // 读超时只在"连接挂着但数据彻底停了"时触发。
            None => {
                let mut builder = crate::core::crypto::default_client_builder();
                if !config.headers.is_empty() {
                    let mut map = reqwest::header::HeaderMap::new();
                    for (name, value) in &config.headers {
                        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                            .map_err(|e| {
                                DeltaError::Config(format!("非法请求头名 {name:?}: {e}"))
                            })?;
                        let header_value =
                            reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                                DeltaError::Config(format!("非法请求头值 {value:?}: {e}"))
                            })?;
                        map.insert(header_name, header_value);
                    }
                    builder = builder.default_headers(map);
                }
                builder
                    .build()
                    .map_err(|e| DeltaError::Network(format!("构建默认 HTTP 客户端失败: {e}")))?
            }
        };

        Ok(Self {
            config,
            executable_path,
            work_dir,
            client,
        })
    }

    /// 零配置入口：只需清单地址、公钥与当前版本。
    ///
    /// 平台键、工作目录、HTTP 客户端全部自动推导。适合 80% 的接入场景；
    /// 需要自定义代理、工作目录或错误处理时再用 [`Self::new`] + [`UpdateConfig`]。
    ///
    /// 版本号传 Tauri 的 `app.package_info().version.to_string()`；
    /// 非 Tauri 宿主用 [`crate::host_version!`]，它展开为**调用方 crate** 的版本，
    /// 而不是本库自身的版本。
    ///
    /// ```no_run
    /// # fn example() -> tauri_delta_updater::Result<()> {
    /// let updater = tauri_delta_updater::DeltaUpdater::auto(
    ///     "https://cdn.example.com/latest.json",
    ///     "BASE64_PUBLIC_KEY",
    ///     tauri_delta_updater::host_version!(),
    /// )?;
    /// # let _ = updater;
    /// # Ok(())
    /// # }
    /// ```
    pub fn auto(
        manifest_url: impl Into<String>,
        public_key: impl Into<String>,
        current_version: impl Into<String>,
    ) -> Result<Self> {
        Self::new(UpdateConfig::new(manifest_url, public_key).current_version(current_version))
    }

    /// 工作目录（补丁、暂存文件、替换脚本与日志所在处）。
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// 当前版本号（来自配置）。
    pub fn current_version(&self) -> &str {
        &self.config.current_version
    }

    /// 插件是否应在下次启动时自动执行待安装记录。
    pub fn install_on_next_launch(&self) -> bool {
        self.config.install_on_next_launch
    }

    /// 清单为该平台声明的最低系统版本是否低于本机。
    ///
    /// 返回 `None` 表示未声明、或本机版本无法探测——未知不阻断。
    pub(super) fn unsupported_os(
        &self,
        manifest: &crate::models::UpdateManifest,
    ) -> Option<(String, String)> {
        let required = manifest
            .platforms
            .get(&self.config.platform)?
            .min_os
            .as_deref()?;
        let current = crate::platform::os_version::current_os_version()?;
        let below =
            crate::platform::os_version::compare(&current, required)? == std::cmp::Ordering::Less;
        below.then(|| (required.to_string(), current))
    }

    /// 版本、系统与灰度门槛的统一判定。
    ///
    /// `check()` 与 `update()` 必须使用同一顺序与同一份结果，否则会出现
    /// “检查说可更新、执行时却拒绝”或相反。任何新增门槛都应加在这里，
    /// 不要在两处各写一遍。
    pub(super) fn evaluate_gates(
        &self,
        manifest: &crate::models::UpdateManifest,
    ) -> Result<UpdateGates> {
        let is_newer = manifest.is_newer_than(&self.config.current_version)?;
        if !is_newer {
            return Ok(UpdateGates {
                is_newer: false,
                ..UpdateGates::default()
            });
        }
        let unsupported_os = self.unsupported_os(manifest);
        let rollout_percentage = self.rollout_percentage(manifest);
        let deferred_by_rollout = self.deferred_by_rollout(manifest)?;
        Ok(UpdateGates {
            is_newer: true,
            rollout_percentage,
            deferred_by_rollout,
            unsupported_os,
        })
    }

    /// 配置里的脚本等待时长，生成替换脚本时写入。
    pub(super) fn script_timings(&self) -> crate::platform::ScriptTimings {
        crate::platform::ScriptTimings {
            wait_secs: self.config.wait_timeout_secs,
            confirm_secs: self.config.confirm_timeout_secs,
        }
    }

    /// 按 `lock_scope` 获取更新锁。
    ///
    /// 所有会写安装目录的流程（更新、修复、全量安装、待安装记录）都必须
    /// 走这里，保证多用户场景下 `InstallDir` 模式真正生效。
    pub(super) fn acquire_lock(&self) -> Result<crate::core::lock::UpdateLock> {
        match self.config.lock_scope {
            config::LockScope::WorkDir => crate::core::lock::UpdateLock::acquire_in(&self.work_dir),
            config::LockScope::InstallDir => crate::core::lock::UpdateLock::acquire_scoped(
                &self.work_dir,
                self.executable_path.parent(),
            ),
        }
    }

    /// 清单端点序列：主地址 + 备用地址。
    pub(super) fn manifest_endpoints(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.config.manifest_url.as_str()).chain(
            self.config
                .fallback_manifest_urls
                .iter()
                .map(String::as_str),
        )
    }
}

/// 补丁的最大下载体积（清单未声明 `patch_size` 时的兜底）。
///
/// 正常补丁远小于此值；超过它只可能是服务端被劫持或配置错误。
pub(super) const MAX_PATCH_BYTES: u64 = 512 * 1024 * 1024;

/// 发出阶段事件；没有订阅者时是零开销空操作。
pub(super) fn emit<'a>(
    on_event: &mut Option<Box<dyn FnMut(UpdateEvent) + Send + 'a>>,
    event: UpdateEvent,
) {
    if let Some(cb) = on_event.as_mut() {
        cb(event);
    }
}

/// 是否已请求取消。
pub(super) fn is_cancelled(cancel: Option<&crate::core::cancel::CancelToken>) -> bool {
    cancel.is_some_and(crate::core::cancel::CancelToken::is_cancelled)
}

/// 下载补丁并完成续传身份复核、取消处理与哈希校验。
///
/// 单文件与整包两条路径共用这一段，只差哈希错误里的标签（`Patch` / `Bundle`）。
/// 取消检查在下载前后各做一次：最后一块之后取消也要清掉半截文件。
#[allow(clippy::too_many_arguments)] // 参数是一组独立上下文，合并成结构体反而增加噪音
pub(super) async fn download_and_verify_patch<'a>(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    meta: &Path,
    expected_hash: &str,
    max_bytes: u64,
    label: &str,
    cancel: Option<&crate::core::cancel::CancelToken>,
    on_event: &mut Option<Box<dyn FnMut(UpdateEvent) + Send + 'a>>,
) -> Result<()> {
    if is_cancelled(cancel) {
        emit(on_event, UpdateEvent::Cancelled);
        return Err(DeltaError::Cancelled);
    }

    // 之前中断的下载只有在身份一致时才续传，否则从头开始
    crate::core::downloader::prepare_resume(path, meta, expected_hash).await;

    crate::core::downloader::download_patch_with_progress_controlled(
        client,
        url,
        path,
        max_bytes,
        cancel,
        |downloaded, total| emit(on_event, UpdateEvent::Downloading { downloaded, total }),
    )
    .await?;

    // 取消可能发生在下载"最后一块之后"：文件已完整落盘，但仍按取消清理。
    if is_cancelled(cancel) {
        let _ = tokio::fs::remove_file(path).await;
        emit(on_event, UpdateEvent::Cancelled);
        return Err(DeltaError::Cancelled);
    }

    let actual = crate::core::hasher::calculate_hash(path).await?;
    if actual != expected_hash.to_lowercase() {
        let _ = tokio::fs::remove_file(path).await;
        return Err(DeltaError::HashMismatch {
            expected: format!("{label}: {expected_hash}"),
            actual: format!("{label}: {actual}"),
        });
    }

    // 合并 / 解包不可中断：这是进入耗时操作前的最后一次取消窗口。
    if is_cancelled(cancel) {
        let _ = tokio::fs::remove_file(path).await;
        emit(on_event, UpdateEvent::Cancelled);
        return Err(DeltaError::Cancelled);
    }
    emit(on_event, UpdateEvent::Applying);
    Ok(())
}

#[cfg(test)]
mod ui_result_tests {
    use super::{
        DeltaUpdater, Diagnosis, FallbackReason, MessageKey, SafeModeReason, UpdateConfig,
        UpdateOutcome, UpdateResult,
    };
    use crate::platform::target::EnvironmentReason;
    use std::path::PathBuf;

    #[test]
    fn maps_up_to_date() {
        let ui: UpdateResult = UpdateOutcome::UpToDate {
            version: "1.2.3".into(),
        }
        .into();
        match ui {
            UpdateResult::UpToDate { version } => assert_eq!(version, "1.2.3"),
            other => panic!("UpToDate 映射错误: {other:?}"),
        }
    }

    #[test]
    fn maps_fallback() {
        let ui: UpdateResult = UpdateOutcome::FallbackToFullDownload {
            full_download_url: Some("https://cdn.example.com/full.exe".into()),
            reason: FallbackReason::NoMatchingPatch {
                from: "0.1.0".into(),
                to: "0.1.1".into(),
            },
        }
        .into();
        match ui {
            UpdateResult::RequiresFullDownload { url, reason } => {
                assert_eq!(url.as_deref(), Some("https://cdn.example.com/full.exe"));
                assert!(matches!(reason, FallbackReason::NoMatchingPatch { .. }));
            }
            other => panic!("Fallback 映射错误: {other:?}"),
        }
    }

    #[test]
    fn maps_safe_mode() {
        let ui: UpdateResult = UpdateOutcome::SafeMode {
            reason: SafeModeReason::SecurityBlock,
        }
        .into();
        match ui {
            UpdateResult::SafeMode { reason } => {
                assert_eq!(reason, SafeModeReason::SecurityBlock);
            }
            other => panic!("SafeMode 映射错误: {other:?}"),
        }
    }

    #[test]
    fn full_download_picks_platform_entry() {
        use super::FullDownload;

        let manifest: crate::UpdateManifest = serde_json::from_str(
            r#"{
                "version": "2.0.0",
                "platforms": {
                    "windows-x86_64": {
                        "url": "https://cdn.example.com/app.exe",
                        "signature": "sig-windows"
                    },
                    "darwin-aarch64": {
                        "url": "https://cdn.example.com/app.tar.gz",
                        "signature": "sig-macos"
                    }
                }
            }"#,
        )
        .expect("清单应能解析");

        let win = FullDownload::from_manifest(&manifest, "windows-x86_64").expect("应命中平台");
        assert_eq!(win.version, "2.0.0");
        assert_eq!(win.url, "https://cdn.example.com/app.exe");
        assert_eq!(win.signature, "sig-windows");

        // 清单里没有的平台返回 None，而不是退化成其它平台的安装包
        assert!(FullDownload::from_manifest(&manifest, "linux-x86_64").is_none());
    }

    /// 前端拿到的 JSON 形状是公开契约：`kind` 与原因标签都必须是 snake_case。
    #[test]
    fn serializes_frontend_contract() {
        use serde_json::json;

        let updated = UpdateResult::Updated {
            version: "1.2.3".into(),
        };
        assert_eq!(
            serde_json::to_value(&updated).unwrap(),
            json!({ "kind": "updated", "version": "1.2.3" })
        );

        // prepare 路径：产物就绪但未启动，前端必须与 Updated 区分
        let ready = UpdateResult::Ready {
            version: "1.2.3".into(),
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            json!({ "kind": "ready", "version": "1.2.3" })
        );

        let full = UpdateResult::RequiresFullDownload {
            url: Some("https://cdn.example.com/app.exe".into()),
            reason: FallbackReason::NoMatchingPatch {
                from: "1.2.3".into(),
                to: "1.3.0".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            json!({
                "kind": "requires_full_download",
                "url": "https://cdn.example.com/app.exe",
                "reason": {
                    "no_matching_patch": { "from": "1.2.3", "to": "1.3.0" }
                }
            })
        );

        assert_eq!(
            serde_json::to_value(UpdateResult::RepairRequired {
                reason: "repaired_partial_update".into()
            })
            .unwrap(),
            json!({ "kind": "repair_required", "reason": "repaired_partial_update" })
        );

        assert_eq!(
            serde_json::to_value(UpdateResult::Deferred {
                version: "1.2.3".into(),
                percentage: 20
            })
            .unwrap(),
            json!({ "kind": "deferred", "version": "1.2.3", "percentage": 20 })
        );

        assert_eq!(
            serde_json::to_value(UpdateResult::SafeMode {
                reason: SafeModeReason::SecurityBlock
            })
            .unwrap(),
            json!({ "kind": "safe_mode", "reason": "security_block" })
        );
    }

    /// `v` 前缀与首尾空白会破坏补丁匹配，必须在构造阶段规范化掉。
    #[test]
    fn normalizes_current_version() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"x").unwrap();

        let updater = DeltaUpdater::new(
            UpdateConfig::new("https://cdn.example.com/latest.json", "PK")
                .current_version(" v0.1.0 ")
                .executable_path(&exe)
                .work_dir(dir.path()),
        )
        .expect("规范化后的版本应可构造");

        assert_eq!(updater.config.current_version, "0.1.0");
    }

    /// 诊断页只需要一个结论：现在能不能走增量。
    #[allow(deprecated)] // 该测试专门验证用户可见的中文文案
    #[test]
    fn diagnosis_reports_delta_eligibility() {
        let base = Diagnosis {
            executable: PathBuf::from("app.exe"),
            work_dir: PathBuf::from("work"),
            platform: "windows-x86_64".into(),
            work_dir_writable: true,
            install_dir_writable: true,
            fallback_reason: None,
            bundle_capable: false,
            safe_mode: false,
            safe_mode_reason: None,
            needs_repair: false,
            startup_state: None,
            last_rollback: None,
            rollback_history: Vec::new(),
            authenticode: None,
            authenticode_check_skipped: None,
            authenticode_policy: crate::platform::AuthenticodePolicy::Strict,
            log_path: PathBuf::from("work/tdx_update.log"),
            events_path: PathBuf::from("work/tdx_events.jsonl"),
            script_events: Vec::new(),
            privilege_tools_available: None,
            insecure_transport: false,
            insecure_endpoint: None,
            installer_kind: None,
        };
        assert!(base.can_use_delta());
        assert_eq!(base.user_message(), "环境正常。");
        assert_eq!(base.message_key(), MessageKey::EnvironmentOk);
        assert!(base.warnings().is_empty(), "正常环境不应有配置警告");

        assert_eq!(
            Diagnosis {
                fallback_reason: Some(EnvironmentReason::MacOsBundle),
                ..base.clone()
            }
            .warnings(),
            vec!["当前环境不支持单文件增量，且未配置整包清单：更新将回退为完整安装包下载。"]
        );
        assert_eq!(
            Diagnosis {
                work_dir_writable: false,
                ..base.clone()
            }
            .warnings(),
            vec!["更新工作目录不可写：请检查目录权限，否则所有更新都会失败。"]
        );

        assert!(!Diagnosis {
            fallback_reason: Some(EnvironmentReason::AppImageNotWritable),
            ..base.clone()
        }
        .can_use_delta());
        assert_eq!(
            Diagnosis {
                fallback_reason: Some(EnvironmentReason::AppImageNotWritable),
                ..base.clone()
            }
            .user_message(),
            "当前环境不支持原地增量更新，将使用完整安装包。"
        );
        // macOS `.app` 单文件不可增量，但整包可用时仍然能走增量。
        assert!(Diagnosis {
            fallback_reason: Some(EnvironmentReason::MacOsBundle),
            bundle_capable: true,
            ..base.clone()
        }
        .can_use_delta());
        assert!(!Diagnosis {
            safe_mode: true,
            ..base.clone()
        }
        .can_use_delta());
        assert_eq!(
            Diagnosis {
                safe_mode: true,
                ..base.clone()
            }
            .user_message(),
            "增量更新已暂停；冷却后会自动重试，也可以手动下载完整安装包。"
        );

        assert!(!Diagnosis {
            work_dir_writable: false,
            ..base
        }
        .can_use_delta());
    }

    /// 回滚原因从脚本写入的文件读取：空白视为没有记录。
    #[test]
    fn reads_rollback_reason() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join(crate::core::artifacts::ROLLBACK_FLAG);
        std::fs::write(&flag, "startup_timeout\n").unwrap();
        assert_eq!(
            super::diagnosis::read_rollback_reason(dir.path()).as_deref(),
            Some("startup_timeout")
        );

        std::fs::write(&flag, "   \n").unwrap();
        assert!(super::diagnosis::read_rollback_reason(dir.path()).is_none());
        std::fs::remove_file(&flag).unwrap();
        assert!(super::diagnosis::read_rollback_reason(dir.path()).is_none());
    }

    /// README 的 Tauri 接入依赖两点：更新器能放进 managed state，结果能跨 IPC 序列化。
    #[test]
    fn public_types_are_ipc_friendly() {
        use super::FullDownload;

        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_serialize<T: serde::Serialize>() {}

        assert_send_sync::<DeltaUpdater>();
        assert_send_sync::<UpdateResult>();
        assert_serialize::<UpdateResult>();
        assert_serialize::<FullDownload>();
    }

    /// Tauri 的 async command 要求返回的 Future 是 `Send`。
    /// 这个测试在编译期成立，不需要真的发起更新。
    #[test]
    fn update_futures_are_send() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"x").unwrap();
        let updater = DeltaUpdater::new(
            UpdateConfig::new("https://cdn.example.com/latest.json", "PK")
                .current_version("0.1.0")
                .executable_path(&exe)
                .work_dir(dir.path()),
        )
        .unwrap();

        fn assert_send<T: Send>(_: T) {}

        assert_send(updater.install_with_progress(|_, _| {}));
        assert_send(updater.update_with_progress(|_, _| {}));

        let token = crate::core::cancel::CancelToken::new();
        assert_send(updater.install_with_events(&token, |_| {}));
        assert_send(updater.prepare_with_events(&token, |_| {}));
    }

    /// 构造一个"刚被替换脚本拉起"的更新器：exe 旁存在工作目录指针。
    fn pending_updater(
        dir: &std::path::Path,
    ) -> (DeltaUpdater, std::path::PathBuf, std::path::PathBuf) {
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"x").unwrap();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let marker = dir.join(crate::core::artifacts::WORK_DIR_MARKER);
        std::fs::write(&marker, work.display().to_string()).unwrap();

        let updater = DeltaUpdater::new(
            UpdateConfig::new("https://cdn.example.com/latest.json", "PK")
                .current_version("0.1.0")
                .executable_path(&exe)
                .work_dir(&work),
        )
        .unwrap();
        (updater, work, marker)
    }

    /// 健康检查通过才写成功标志；失败必须立刻落盘回滚原因，而不是等超时。
    #[test]
    fn health_check_controls_startup_commit() {
        let dir = tempfile::tempdir().unwrap();
        let (updater, work, marker) = pending_updater(dir.path());

        updater.confirm_startup_success_with(|| Ok(())).unwrap();
        assert!(!marker.exists(), "确认后必须移除工作目录指针");
        assert!(work.join(crate::core::artifacts::SUCCESS_FLAG).is_file());
        assert_eq!(
            crate::core::startup::read(&work),
            Some(crate::core::startup::StartupState::Committed)
        );

        let dir = tempfile::tempdir().unwrap();
        let (updater, work, marker) = pending_updater(dir.path());
        let err = updater
            .confirm_startup_success_with(|| {
                Err(crate::DeltaError::Config("database unavailable".into()))
            })
            .unwrap_err();
        assert!(matches!(err, crate::DeltaError::Config(_)));
        assert!(!marker.exists(), "失败后也必须移除工作目录指针");
        assert!(
            !work.join(crate::core::artifacts::SUCCESS_FLAG).is_file(),
            "健康检查失败不得写成功标志"
        );
        assert_eq!(
            std::fs::read_to_string(work.join(crate::core::artifacts::ROLLBACK_FLAG)).unwrap(),
            crate::core::artifacts::HEALTH_CHECK_FAILED
        );
        assert_eq!(
            crate::core::startup::read(&work),
            Some(crate::core::startup::StartupState::Failed)
        );
    }
}
