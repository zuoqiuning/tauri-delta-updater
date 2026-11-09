//! # tauri-delta-updater
//!
//! 为 Tauri v2 应用提供极致、安全、防弹的增量更新引擎。
//!
//! ## 一行接入
//!
//! 绝大多数接入方只需要 [`DeltaUpdater`]：一次调用完成
//! 拉取清单 → 验签 → 版本单调性检查 → 匹配补丁 → 下载 → 校验 → 应用 → 启动替换。
//!
//! 零配置形式（推荐）：平台、工作目录、HTTP 客户端全部自动推导。
//!
//! ```no_run
//! # async fn example() -> tauri_delta_updater::Result<()> {
//! let updater = tauri_delta_updater::DeltaUpdater::auto(
//!     "https://cdn.example.com/latest.json",
//!     // 固化在应用里的公钥，切勿从服务器下载
//!     "BASE64_PUBLIC_KEY",
//!     "0.1.0",
//! )?;
//! # Ok(())
//! # }
//! ```
//!
//! 需要注入代理/UA 或自定义工作目录时，用 [`UpdateConfig`]：
//!
//! ```no_run
//! use tauri_delta_updater::{DeltaUpdater, UpdateConfig, UpdateOutcome};
//!
//! # async fn example() -> tauri_delta_updater::Result<()> {
//! let updater = DeltaUpdater::new(
//!     UpdateConfig::new(
//!         "https://cdn.example.com/latest.json",
//!         "BASE64_PUBLIC_KEY",
//!     )
//!     .current_version("0.1.0"),
//! )?;
//!
//! match updater.update().await? {
//!     UpdateOutcome::Patched { staged } => {
//!         println!("已更新到 {}", staged.manifest().version);
//!     }
//!     UpdateOutcome::UpToDate { version } => {
//!         // 已是最新（含"清单版本不高于当前版本"的防降级分支）
//!         println!("已是最新版本 {version}");
//!     }
//!     UpdateOutcome::FallbackToFullDownload { full_download_url, reason } => {
//!         // 增量不可用（无匹配补丁，或环境不适合单文件增量如只读 AppImage、
//!         // macOS `.app` bundle），交回 tauri-plugin-updater 走全量下载。
//!         // 配置整包清单后，macOS `.app` 改走目录切换，不再落在这里。
//!         let _ = (full_download_url, reason);
//!     }
//!     UpdateOutcome::SafeMode { reason } => {
//!         // 连续失败已达上限；reason 区分安全阻断与环境问题
//!         let _ = reason;
//!     }
//!     // UpdateOutcome 是 non_exhaustive 的，将来新增变体不会破坏现有代码
//!     _ => {}
//! }
//! # Ok(())
//! # }
//! ```
//!
//! 新版本启动成功后调用一次 [`PlatformUpdater::confirm_startup_success`]，
//! 用于解锁安全模式并让替换脚本保留新版本。若通过
//! [`UpdateConfig::work_dir`] 自定义了工作目录，改调
//! [`DeltaUpdater::confirm_startup_success`]（实例方法）。
//!
//! 更推荐无条件调用 [`PlatformUpdater::confirm_startup_success_if_pending`]：
//! 它只在本次启动确实来自一次替换时才写标志，普通启动零写入。
//!
//! **跨代际升级（新引擎的第一个版本）必须改用无条件确认**：
//! 替换由旧引擎脚本驱动，旧脚本不会写 `tdx_update_dir` 指针。`if_pending`
//! 现在也会把残留的旧脚本识别为替换证据，但新引擎首发仍应显式调用
//! [`PlatformUpdater::confirm_startup_success`]，避免依赖兼容判据。
//!
//! ## 首次接入检查清单
//!
//! 1. 启动时调用 `confirm_startup_success()`；新引擎首发不要用 `if_pending`；
//! 2. `install()` 返回 `Updated` 或 `RepairRequired` 后退出进程
//!    （或直接用 `install_and_exit()`）；
//! 3. 把 `RequiresFullDownload` 交给官方 updater 或安装流程。
//!
//! ## 接入前先本地验证
//!
//! 不需要网络、不需要签名密钥，也不需要完整的 Tauri 应用：
//!
//! ```no_run
//! # async fn example() -> tauri_delta_updater::Result<()> {
//! let report = tauri_delta_updater::simulate_update(
//!     "old.exe",
//!     "new.exe",
//!     "target/delta-sim",
//! )
//! .await?;
//! println!(
//!     "补丁 {} 字节 / 全量 {} 字节（{:.1}%）",
//!     report.patch_size,
//!     report.target_size,
//!     report.ratio() * 100.0
//! );
//! # Ok(())
//! # }
//! ```
//!
//! ## 排障
//!
//! [`DeltaUpdater::diagnose`] 返回工作目录/安装目录可写性、平台适用性、
//! 安全模式状态与日志路径，用于 UI 的"诊断"页或接入排障。
//!
//! 启用 `tracing` feature 后，关键节点会发出 `delta.*` 结构化事件。
//!
//! ## 分阶段接入
//!
//! 真实 UI 流程往往是"先发现新版本 → 用户确认 → 再下载"。
//! [`DeltaUpdater::check`] 只拉清单不下载， [`DeltaUpdater::prepare`] 下载并合并
//! 但不启动替换脚本，由宿主掌控替换时机。
//!
//! ```no_run
//! use tauri_delta_updater::{DeltaUpdater, UpdateConfig, UpdateOutcome};
//!
//! # async fn example() -> tauri_delta_updater::Result<()> {
//! let updater = DeltaUpdater::new(
//!     UpdateConfig::new("https://cdn.example.com/latest.json", "BASE64_PUBLIC_KEY")
//!         .current_version("0.1.0")
//!         .auto_launch(false),
//! )?;
//!
//! // 1. 只检查，不下载
//! let info = updater.check().await?;
//! println!("发现新版本 {}，补丁 {}", info.version, info.has_patch);
//! // 增量不可用时，info.full_download() 直接给出 { version, url, signature }
//!
//! // 2. 用户确认后下载并合并，但先不启动替换
//! if let UpdateOutcome::Patched { staged } = updater.prepare().await? {
//!     // 提示用户即将重启
//!     println!("正在重启以完成更新...");
//!     staged.launch()?;   // 由宿主决定何时触发替换
//!     std::process::exit(0);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## 可观察 / 可取消
//!
//! [`DeltaUpdater::session`] 返回 [`UpdateSession`]：按阶段回调
//! [`UpdateEvent`]（`Checking` / `Downloading` / `Applying` / `Ready` …），
//! 并用 [`CancelToken`] 从任意线程取消。取消会清理半截文件，
//! 且不计入安全模式。
//!
//! ## 覆盖 sidecar / resources（整包增量）
//!
//! 单文件补丁只覆盖主程序。使用 `externalBin` 或 `resources` 的应用，配置
//! [`UpdateConfig::bundle_manifest_url`]（指向发布端生成的 `latest.bundle.json`）
//! 后改走整包：Windows 逐文件替换，macOS 以整个 `.app` 目录切换并保留代码
//! 签名。结果返回 [`UpdateOutcome::Bundled`]。
//!
//! ```no_run
//! # fn example() -> tauri_delta_updater::Result<()> {
//! let updater = tauri_delta_updater::DeltaUpdater::new(
//!     tauri_delta_updater::UpdateConfig::new(
//!         "https://cdn.example.com/latest.json",
//!         "BASE64_PUBLIC_KEY",
//!     )
//!     .current_version("0.1.0")
//!     .bundle_manifest_url("https://cdn.example.com/latest.bundle.json"),
//! )?;
//! # let _ = updater;
//! # Ok(())
//! # }
//! ```
//!
//! ## 分步调用
//!
//! 需要自己控制每一步时，可直接使用底层函数。
//!
//! ### 1. 查找补丁
//!
//! ```
//! use tauri_delta_updater::UpdateManifest;
//!
//! // `UpdateManifest` 是 `#[non_exhaustive]`：清单按真实发布格式解析，
//! // 不要用结构体字面量构造（新增字段会破坏下游编译）。
//! let manifest: UpdateManifest = serde_json::from_str(r#"{"version":"0.1.2"}"#).unwrap();
//!
//! // 无匹配补丁时返回 None，调用方应回退到全量下载
//! assert!(manifest.find_patch("0.1.0", "windows-x86_64").is_none());
//! // 版本单调性：签名只证明"出自发布者"，不证明"比当前新"
//! assert!(manifest.is_newer_than("0.1.0").unwrap());
//! assert!(!manifest.is_newer_than("0.1.2").unwrap());
//! ```
//!
//! ### 2. 校验清单签名
//!
//! ```
//! use tauri_delta_updater::verify_manifest_signature;
//!
//! // 验签失败应视为攻击，直接拒绝更新
//! let result = verify_manifest_signature(b"{}", "invalid_sig", "invalid_pubkey");
//! assert!(result.is_err());
//! ```
//!
//! ### 3. 计算文件哈希
//!
//! ```
//! # async fn example() {
//! use tauri_delta_updater::calculate_hash;
//! let hash = calculate_hash("Cargo.toml").await.unwrap();
//! assert_eq!(hash.len(), 64); // SHA-256 十六进制字符串
//! # }
//! ```

// 公共 API 必须有文档：这是库，缺文档等于缺接口说明。
#![warn(missing_docs)]

/// 底层能力：差分、下载、校验、锁与安全策略。
pub mod core;
/// 错误类型。
pub mod error;
/// 数据模型。
pub mod models;
/// 各平台适配。
pub mod platform;
/// 发布期工具：生成签名的清单、补丁与发布报告（需启用 `generator`）。
#[cfg(feature = "generator")]
pub mod release;
/// 可观察、可取消的更新会话。
pub mod session;
/// 高层更新流程。
pub mod updater;

pub use error::{DeltaError, Result};
pub use models::{
    PatchInfo, PatchScope, PlatformInfo, Rollout, UpdateManifest, MAX_DELTA_CHAIN_HOPS,
};
pub use session::{UpdateEvent, UpdateSession};

pub use core::bundle::{
    apply_bundle_patch, build_archive, create_bundle_patch, describe_dir, extract_archive,
    list_archive, verify_layout, BundleFile, BundleManifest, BundlePatch,
};
pub use core::cancel::CancelToken;
pub use core::crypto::{client_builder, default_client_builder, install_crypto_provider};
/// 在**调用方 crate** 展开为它的 `CARGO_PKG_VERSION`。
///
/// 库内部的 `env!("CARGO_PKG_VERSION")` 会得到库自己的版本，不是宿主应用的版本；
/// 这个宏在调用处展开，因此拿到的是宿主的版本号，适合非 Tauri 的 CLI/服务端宿主：
///
/// ```
/// assert_eq!(
///     tauri_delta_updater::host_version!(),
///     env!("CARGO_PKG_VERSION")
/// );
/// ```
#[macro_export]
macro_rules! host_version {
    () => {
        env!("CARGO_PKG_VERSION")
    };
}

pub use core::diff::{
    apply_patch, create_patch, create_patch_uncompressed, create_patch_with_level,
    VerifiedArtifact, MAX_INCREMENTAL_FILE_BYTES,
};
pub use core::disk::{check_disk_space, cleanup_previous_updates};
pub use core::downloader::download_patch_with_progress;
pub use core::hasher::calculate_hash;
pub use core::lock::UpdateLock;
pub use core::paths::{default_work_dir, ensure_writable, is_writable};
pub use core::safety::{
    clear_failed_attempts, get_failed_attempts, is_safe_mode_active, last_failure_reason,
    record_failed_attempt, record_failed_attempt_with_reason, reset_safety, safe_mode_remaining,
    should_abort,
};
pub use core::startup::StartupState;
pub use core::verify::{
    manifest_version_binding, manifest_version_bindings, validate_public_key,
    verify_file_signature, verify_file_signature_multi, verify_file_signature_multi_with_legacy,
    verify_manifest_signature, verify_manifest_signature_file, verify_manifest_signature_multi,
    verify_manifest_signature_with, TrustPolicy,
};
pub use platform::full_install::{InstallFailureKind, InstallMode, InstallerKind};
pub use platform::os_version::current_os_version;
pub use platform::target::{
    current_platform, detect_arch, incremental_fallback_reason, normalize_version,
    EnvironmentReason,
};
/// 替换脚本的等待时长（由 `UpdateConfig` 的 timeout 配置写入脚本）。
pub use platform::ScriptTimings;
pub use platform::{AuthenticodePolicy, PlatformUpdater};
pub use updater::{
    default_signature_url, simulate_update, CheckOutcome, DeltaUpdater, Diagnosis, FallbackPolicy,
    FallbackReason, FullDownload, FullDownloadOutcome, LockScope, MessageKey, OsRequirement,
    PendingArtifact, PendingUpdate, SafeModeReason, ScriptEvent, SimulationReport, Staged,
    StagedBundle, StagedFullInstaller, StagedRepair, UpdateCheck, UpdateConfig, UpdateOutcome,
    UpdateResult,
};
