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
//!         // 增量不可用（无匹配补丁，或平台不适合增量如 AppImage/macOS bundle），
//!         // 交回 tauri-plugin-updater 走全量下载
//!         let _ = (full_download_url, reason);
//!     }
//!     UpdateOutcome::SafeMode => {
//!         // 连续失败已达上限，提示用户手动下载
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
//! ## 分步调用
//!
//! 需要自己控制每一步时，可直接使用底层函数。
//!
//! ### 1. 查找补丁
//!
//! ```
//! use tauri_delta_updater::UpdateManifest;
//!
//! let manifest = UpdateManifest {
//!     version: "0.1.2".to_string(),
//!     notes: String::new(),
//!     pub_date: String::new(),
//!     platforms: Default::default(),
//!     patches: Default::default(),
//! };
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
/// 高层更新流程。
pub mod updater;

pub use error::{DeltaError, Result};
pub use models::{PatchInfo, PatchScope, PlatformInfo, UpdateManifest};

pub use core::crypto::{client_builder, install_crypto_provider};
pub use core::diff::{
    apply_patch, create_patch, create_patch_with_level, VerifiedArtifact,
    MAX_INCREMENTAL_FILE_BYTES,
};
pub use core::disk::{check_disk_space, cleanup_previous_updates};
pub use core::downloader::download_patch_with_progress;
pub use core::hasher::calculate_hash;
pub use core::lock::UpdateLock;
pub use core::paths::{default_work_dir, ensure_writable, is_writable};
pub use core::safety::{
    clear_failed_attempts, get_failed_attempts, is_safe_mode_active, record_failed_attempt,
    reset_safety, safe_mode_remaining, should_abort,
};
pub use core::verify::{
    manifest_version_binding, verify_manifest_signature, verify_manifest_signature_file,
};
pub use platform::target::{
    current_platform, detect_arch, incremental_fallback_reason, normalize_version,
};
pub use platform::PlatformUpdater;
pub use updater::{
    simulate_update, CheckOutcome, DeltaUpdater, Diagnosis, FallbackReason, SimulationReport,
    Staged, UpdateCheck, UpdateConfig, UpdateOutcome, UpdateResult,
};
