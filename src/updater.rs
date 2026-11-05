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

use crate::core::diff::{apply_patch, create_patch};
use crate::core::disk::cleanup_previous_updates;
use crate::core::downloader::download_patch_with_progress;
use crate::core::hasher::calculate_hash;
use crate::core::lock::UpdateLock;
use crate::core::paths::{default_work_dir, ensure_writable};
use crate::core::safety::{clear_failed_attempts, record_failed_attempt, should_abort};
use crate::core::verify::verify_manifest_signature;
use crate::error::{DeltaError, Result};
use crate::models::UpdateManifest;
use crate::platform::target::current_platform;
use crate::platform::PlatformUpdater;
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 增量更新的配置。
///
/// 除 `manifest_url` 与 `public_key` 外都有合理默认值，
/// 通常只需要设置这两项。
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// `latest.json` 的下载地址。
    ///
    /// # 注意
    ///
    /// [`Self::signature_url`] 在 [`Self::new`] 里由它推导。
    /// **构造之后再手动修改本字段，必须同步修改 `signature_url`**，
    /// 否则签名会从旧地址拉取，导致验签拿到不匹配的文件。
    pub manifest_url: String,
    /// `latest.json.sig` 的下载地址。
    pub signature_url: String,
    /// 用于校验清单签名的公钥。
    ///
    /// 必须是**固化在应用里**的公钥，不能从服务器下载。
    pub public_key: String,
    /// 当前运行中的版本号，用于匹配补丁与拒绝降级。
    ///
    /// 必须是语义化版本（与 Tauri 的 `app.package_info().version` 一致）。
    pub current_version: String,
    /// 目标平台标识。默认使用当前构建的平台。
    pub platform: String,
    /// 被更新的可执行文件。
    ///
    /// 为 `None` 时自动使用当前进程的可执行文件。
    /// 当前进程的可执行文件无法确定时（例如某些沙箱环境），
    /// [`DeltaUpdater::new`] 会返回明确的错误，而不是退化成相对路径。
    pub executable_path: Option<PathBuf>,
    /// 工作目录，用于存放补丁、暂存文件、替换脚本与日志。
    ///
    /// 为 `None` 时使用用户缓存目录（见 [`crate::default_work_dir`]），
    /// **不再默认放在 exe 旁边**：系统级安装目录对普通用户只读，
    /// 默认放在那里会让更新在下载阶段就失败。
    pub work_dir: Option<PathBuf>,
    /// 合并完成后是否自动启动替换脚本。
    ///
    /// 默认 `true`。设为 `false` 时由宿主调用 [`Staged::launch`] 自行决定时机。
    pub auto_launch: bool,
    /// 等待旧进程退出的最长秒数。默认 60。
    pub wait_timeout_secs: u32,
    /// 等待新版本写入启动成功标志的最长秒数。默认 60。
    pub confirm_timeout_secs: u32,
    /// 注入自定义 HTTP 客户端，用于设置代理、UA、超时等。
    ///
    /// 为 `None` 时使用默认客户端。宿主若有统一的网络层，建议注入自己的；
    /// 自定义客户端使用 rustls 时，必须在创建前调用
    /// [`crate::install_crypto_provider`]。
    pub client: Option<reqwest::Client>,
}

impl UpdateConfig {
    /// 以清单地址与公钥创建配置，其余项自动填充。
    pub fn new(manifest_url: impl Into<String>, public_key: impl Into<String>) -> Self {
        let manifest_url = manifest_url.into();

        Self {
            // 默认与清单同目录同名的 .sig 文件
            signature_url: default_signature_url(&manifest_url),
            manifest_url,
            public_key: public_key.into(),
            current_version: String::new(),
            platform: current_platform().to_string(),
            executable_path: None,
            work_dir: None,
            auto_launch: true,
            wait_timeout_secs: 60,
            confirm_timeout_secs: 60,
            client: None,
        }
    }

    /// 设置当前版本号。
    pub fn current_version(mut self, version: impl Into<String>) -> Self {
        self.current_version = version.into();
        self
    }

    /// 覆盖目标平台标识。默认使用当前构建的平台，通常无需设置。
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = platform.into();
        self
    }

    /// 覆盖被更新的可执行文件路径。
    pub fn executable_path(mut self, path: impl AsRef<Path>) -> Self {
        self.executable_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// 覆盖工作目录。
    ///
    /// 必须是用户可写目录。自定义后，启动确认要调用
    /// [`DeltaUpdater::confirm_startup_success`]（实例方法），
    /// 而不是静态的 [`PlatformUpdater::confirm_startup_success`]。
    pub fn work_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.work_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// 关闭自动启动替换脚本，改由宿主调用 [`Staged::launch`]。
    pub fn auto_launch(mut self, auto: bool) -> Self {
        self.auto_launch = auto;
        self
    }

    /// 设置等待旧进程退出的超时（秒）。
    pub fn wait_timeout_secs(mut self, secs: u32) -> Self {
        self.wait_timeout_secs = secs;
        self
    }

    /// 设置等待新版本启动确认的超时（秒）。
    pub fn confirm_timeout_secs(mut self, secs: u32) -> Self {
        self.confirm_timeout_secs = secs;
        self
    }

    /// 注入自定义 HTTP 客户端（代理、UA、超时等）。
    pub fn client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }
}

/// 增量不可用、需要回退全量下载的原因。
///
/// 比自由字符串更适合 UI 分支与本地化：宿主可以 `match` 出具体原因，
/// 而不是对中文文案做字符串包含判断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub enum FallbackReason {
    /// 清单里没有从当前版本到目标版本的补丁（跨版本升级或首次安装）。
    NoMatchingPatch {
        /// 当前版本。
        from: String,
        /// 目标版本。
        to: String,
    },
    /// 当前运行环境不适合原地替换（AppImage、macOS `.app` bundle 等）。
    EnvironmentUnsupported(&'static str),
}

impl std::fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FallbackReason::NoMatchingPatch { from, to } => write!(
                f,
                "清单里没有从 {from} 到 {to} 的增量补丁（跨版本升级或首次安装）"
            ),
            FallbackReason::EnvironmentUnsupported(detail) => f.write_str(detail),
        }
    }
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
    /// 当前环境不适合增量的原因（AppImage、macOS bundle 等）。
    pub fallback_reason: Option<&'static str>,
    /// 是否处于安全模式。
    pub safe_mode: bool,
    /// 替换脚本日志路径。
    pub log_path: PathBuf,
}

/// 检查更新的结果。
///
/// 只拉取并验签清单，不会下载补丁。
#[derive(Debug, Clone, Serialize)]
pub struct CheckOutcome {
    /// 清单里的目标版本号。
    pub version: String,
    /// 更新说明。
    pub notes: String,
    /// 发布时间。
    pub pub_date: String,
    /// 目标版本是否**严格高于**当前版本。
    ///
    /// 防降级的核心判据：清单签名只证明"出自发布者"，不证明"比当前新"。
    /// 为 `false` 时宿主应展示"已是最新"，而不是提示更新。
    pub is_newer: bool,
    /// 当前版本是否有可用补丁。
    pub has_patch: bool,
    /// 本次是否会走增量路径（`has_patch` 的 UI 语义别名）。
    pub is_delta: bool,
    /// 实际会下载的体积（增量时为补丁体积；全量时为 `None`，清单不含安装包大小）。
    pub download_size: Option<u64>,
    /// 补丁文件大小（字节）。清单未提供时为 `None`。
    pub patch_size: Option<u64>,
    /// 全量安装包地址，用于回退到 `tauri-plugin-updater`。
    pub full_download_url: Option<String>,
    /// 已验签的清单，便于展示更多信息。
    pub manifest: Box<UpdateManifest>,
    /// 增量不可用的原因。为 `None` 时表示当前环境可以走增量路径。
    pub fallback_reason: Option<FallbackReason>,
}

/// 已就绪、等待替换的更新产物。
///
/// 由 [`DeltaUpdater::prepare`] 生成。宿主可以先做展示、提示等操作，
/// 再调用 [`Staged::launch`] 真正触发文件替换。
#[derive(Debug)]
pub struct Staged {
    /// 已通过双端哈希校验的合并产物（类型上排除未验证字节流入替换流程）
    staged_exe: crate::core::diff::VerifiedArtifact,
    script_path: PathBuf,
    manifest: Box<UpdateManifest>,
    /// 防止替换脚本被启动两次：脚本与新进程竞争替换会让回滚逻辑误判。
    /// 用 Arc 保证所有克隆共享同一份状态，克隆体也无法绕过幂等。
    launched: std::sync::Arc<AtomicBool>,
}

impl Clone for Staged {
    fn clone(&self) -> Self {
        Self {
            staged_exe: self.staged_exe.clone(),
            script_path: self.script_path.clone(),
            manifest: self.manifest.clone(),
            launched: std::sync::Arc::clone(&self.launched),
        }
    }
}

impl Staged {
    /// 合并后的新版本可执行文件路径。
    pub fn staged_exe(&self) -> &Path {
        self.staged_exe.path()
    }

    /// 替换脚本路径。
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    /// 本次使用的清单。
    pub fn manifest(&self) -> &UpdateManifest {
        &self.manifest
    }

    /// 启动替换脚本，开始文件替换与重启。
    ///
    /// **只能成功启动一次**：重复调用返回 [`DeltaError::Config`]
    /// （两个脚本竞争替换会让回滚逻辑误判）。启动失败不占名额，可以重试。
    /// 调用后主进程应当尽快退出，否则旧程序会占用 exe 文件导致替换失败。
    ///
    /// # 并发语义
    ///
    /// 幂等状态经 `Arc` 在所有克隆间共享：并发调用时只有一个线程会真正启动
    /// 替换脚本，其余立即返回 [`DeltaError::Config`]。启动失败后状态重置，
    /// 之后任何线程（包括之前失败的）都可以重试；
    /// 需要串行化重试时序的调用方应自行加锁。
    pub fn launch(&self) -> Result<()> {
        if self
            .launched
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return Err(DeltaError::Config(
                "替换脚本已经启动过，不能再次启动：重复启动会让两个脚本竞争替换，导致回滚误判"
                    .into(),
            ));
        }
        match PlatformUpdater::launch_updater(&self.script_path) {
            Ok(()) => {
                crate::core::events::launch();
                Ok(())
            }
            // 启动失败不占名额，允许重试
            Err(e) => {
                self.launched.store(false, Ordering::Relaxed);
                Err(e)
            }
        }
    }
}

/// 一次更新的结果。
#[derive(Debug)]
#[non_exhaustive]
pub enum UpdateOutcome {
    /// 已通过增量补丁生成新版本。
    ///
    /// 若 `auto_launch` 为 `true`（默认），替换脚本已经启动；
    /// 否则需要宿主调用 [`Staged::launch`]。
    ///
    /// # 调用方必须让当前进程退出
    ///
    /// 替换脚本会等待旧进程退出后才替换可执行文件。
    /// **若主进程不退出，脚本会等待超时后判定启动失败并回滚**，
    /// 用户会看到"更新了又退回旧版本"。
    ///
    /// 正确做法：
    ///
    /// ```no_run
    /// # async fn example() -> tauri_delta_updater::Result<()> {
    /// # let updater = tauri_delta_updater::DeltaUpdater::new(
    /// #     tauri_delta_updater::UpdateConfig::new("https://cdn/latest.json", "PUBKEY"),
    /// # )?;
    /// if let tauri_delta_updater::UpdateOutcome::Patched { staged } = updater.update().await? {
    ///     // 先通知前端，再退出进程
    ///     let _ = staged.script_path();
    ///     std::process::exit(0);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    Patched {
        /// 已就绪的更新产物。
        staged: Staged,
    },
    /// 已经是最新版本（或清单版本低于当前版本，按"不更新"处理）。
    ///
    /// 这是防降级的关键分支：重放一份旧的、签名合法的清单时，
    /// 必须落到这里，绝不能返回带旧安装包地址的回退分支。
    UpToDate {
        /// 清单声明的版本号。
        version: String,
    },
    /// 增量不可用，调用方应回退到全量下载（交回官方 updater）。
    ///
    /// 这不是错误，是本库"增量加速器"定位的正常分支：
    ///
    /// - 清单里没有匹配当前版本的补丁（跨版本升级、首次安装）；
    /// - 当前平台/布局不适合增量替换（AppImage、macOS `.app` bundle 等）。
    ///
    /// `reason` 是结构化的回退原因，可直接 `match` 或展示。
    FallbackToFullDownload {
        /// 清单里指向的完整安装包地址（若有）。
        full_download_url: Option<String>,
        /// 回退原因。
        reason: FallbackReason,
    },
    /// 连续失败次数已达上限，已停止尝试增量更新。
    ///
    /// 调用方应提示用户手动下载安装包。
    ///
    /// 注意语义边界：触发原因可能是安全阻断（签名/哈希失败），
    /// 也可能是持续性环境问题（磁盘满、权限不足）。
    /// 展示 UI 时用 [`DeltaError::is_security_blocking()`] 区分，
    /// 不要把所有 SafeMode 都当成安全风险提示。
    SafeMode,
}

/// 面向 UI 的更新结果。
///
/// 这是默认接入路径（[`DeltaUpdater::install`]）的返回值：
/// 只需要区分"更新了 / 已最新 / 需要全量 / 安全模式"，
/// 不需要理解补丁 URL、哈希或暂存文件。
///
/// 需要完全控制流程时，仍可使用 [`DeltaUpdater::update`] 返回的
/// [`UpdateOutcome`]。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UpdateResult {
    /// 已通过增量补丁更新。
    Updated {
        /// 更新后的版本号。
        version: String,
    },
    /// 已经是最新版本。
    UpToDate {
        /// 清单声明的版本号。
        version: String,
    },
    /// 需要走全量下载（宿主可交给官方 updater）。
    RequiresFullDownload {
        /// 全量安装包地址（若有）。
        url: Option<String>,
        /// 回退原因。
        reason: FallbackReason,
    },
    /// 连续失败已达上限，已进入安全模式。
    SafeMode,
}

/// UI 检查结果的语义别名。
pub type UpdateCheck = CheckOutcome;

/// 增量更新器。
pub struct DeltaUpdater {
    config: UpdateConfig,
    executable_path: PathBuf,
    /// 用户可写的工作目录：补丁、暂存文件、脚本、日志、标志。
    work_dir: PathBuf,
    client: reqwest::Client,
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
        // current_version 为空时 find_patch 永远返回 None，会静默退化成全量下载；
        // 不是语义化版本时无法做降级判据。两者都是最难排查的接入错误，
        // 必须在构造时就拒绝。
        if config.current_version.is_empty() {
            return Err(DeltaError::Config(
                "current_version 不能为空：它决定匹配哪个版本的补丁".into(),
            ));
        }
        semver::Version::parse(config.current_version.trim()).map_err(|e| {
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
            None => std::env::current_exe().map_err(|e| {
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

        // 替换脚本产生的备份必须与目标 exe 同目录，因此 exe 必须在某个目录下
        if executable_path.parent().is_none() {
            return Err(DeltaError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "无法确定可执行文件所在目录",
            )));
        }

        let client = match config.client.clone() {
            Some(client) => client,
            // 默认客户端带超时：连接 15 秒、单次读空闲 60 秒。
            // 不设总超时——大补丁在慢网络下可能合法地下很久，
            // 读超时只在"连接挂着但数据彻底停了"时触发。
            None => reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(60))
                .build()
                .map_err(|e| DeltaError::Network(format!("构建默认 HTTP 客户端失败: {e}")))?,
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
    pub fn auto(
        manifest_url: impl Into<String>,
        public_key: impl Into<String>,
        current_version: impl Into<String>,
    ) -> Result<Self> {
        Self::new(UpdateConfig::new(manifest_url, public_key).current_version(current_version))
    }

    /// 环境诊断：告诉开发者"当前环境能不能走增量、失败日志去哪里看"。
    ///
    /// 返回的是**只读快照**，不做网络请求。典型用途：
    /// - UI 的"关于/诊断"页面；
    /// - 接入排障：安装目录是否只读、工作目录是否可写、
    ///   当前平台是否在增量适用边界内、是否已被安全模式暂停；
    /// - 支持人员按 `log_path` 收集日志。
    pub async fn diagnose(&self) -> Diagnosis {
        let install_dir = self.executable_path.parent();
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
            safe_mode: crate::core::safety::is_safe_mode_active(&self.work_dir).await,
            log_path: self.work_dir.join(crate::core::artifacts::LOG_FILE),
        }
    }

    /// 工作目录（补丁、暂存文件、替换脚本与日志所在处）。
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// 只检查是否有新版本，不下载补丁。
    ///
    /// 适合 UI 的"发现新版本"阶段：用户确认后再调用 [`DeltaUpdater::update`]。
    ///
    /// 同样会做签名校验、版本单调性判断与安全模式判断。
    ///
    /// 安全模式激活时返回 `Err(DeltaError::SafeMode)`——此时**连清单都不拉取**，
    /// 与 `update()` 返回 `Ok(UpdateOutcome::SafeMode)` 是同一语义的两种形态：
    /// 检查无法进行，而更新可以"执行并决定不更新"。
    pub async fn check(&self) -> Result<CheckOutcome> {
        crate::core::crypto::install_crypto_provider();

        if should_abort(&self.work_dir).await {
            self.emit_safe_mode().await;
            return Err(DeltaError::SafeMode);
        }

        let manifest = self.fetch_verified_manifest().await?;

        let is_newer = manifest.is_newer_than(&self.config.current_version)?;
        // 不是更新时不应展示补丁信息：清单里可能残留同版本的补丁记录
        let patch = if is_newer {
            manifest.find_patch(&self.config.current_version, &self.config.platform)
        } else {
            None
        };
        let full_download_url = manifest
            .platforms
            .get(&self.config.platform)
            .map(|p| p.url.clone());
        // 适用边界：平台/布局不适合增量时，UI 不应展示"有补丁可下载"
        let fallback_reason = if is_newer {
            crate::platform::target::incremental_fallback_reason(&self.executable_path)
                .map(FallbackReason::EnvironmentUnsupported)
        } else {
            None
        };
        let has_patch = is_newer && patch.is_some() && fallback_reason.is_none();

        crate::core::events::check(&manifest.version, is_newer, has_patch);

        Ok(CheckOutcome {
            version: manifest.version.clone(),
            notes: manifest.notes.clone(),
            pub_date: manifest.pub_date.clone(),
            is_newer,
            has_patch,
            is_delta: has_patch,
            download_size: patch.and_then(|p| p.patch_size),
            patch_size: patch.and_then(|p| p.patch_size),
            full_download_url,
            fallback_reason,
            manifest: Box::new(manifest),
        })
    }

    /// 执行完整的增量更新流程。
    ///
    /// 过程出错时会记录失败次数；连续失败达到上限后返回 [`UpdateOutcome::SafeMode`]。
    pub async fn update(&self) -> Result<UpdateOutcome> {
        crate::core::crypto::install_crypto_provider();

        if should_abort(&self.work_dir).await {
            self.emit_safe_mode().await;
            return Ok(UpdateOutcome::SafeMode);
        }

        self.run().await
    }

    /// 默认接入路径：面向 UI 的更新结果。
    ///
    /// 与 [`Self::update`] 相同，但把 `UpdateOutcome` 映射成
    /// [`UpdateResult`]——宿主只需要处理"更新了 / 已最新 / 需要全量 / 安全模式"，
    /// 不必理解补丁 URL、哈希与暂存文件。
    pub async fn install(&self) -> Result<UpdateResult> {
        Ok(match self.update().await? {
            UpdateOutcome::Patched { staged } => UpdateResult::Updated {
                version: staged.manifest().version.clone(),
            },
            UpdateOutcome::UpToDate { version } => UpdateResult::UpToDate { version },
            UpdateOutcome::FallbackToFullDownload {
                full_download_url,
                reason,
            } => UpdateResult::RequiresFullDownload {
                url: full_download_url,
                reason,
            },
            UpdateOutcome::SafeMode => UpdateResult::SafeMode,
        })
    }

    /// 默认接入路径的一步到位版本：更新成功后启动替换并退出进程。
    ///
    /// 其它结果（已最新 / 需要全量 / 安全模式）原样返回，由宿主决定下一步。
    pub async fn install_and_exit(&self) -> Result<UpdateResult> {
        match self.update().await? {
            UpdateOutcome::Patched { staged } => {
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                std::process::exit(0);
            }
            UpdateOutcome::UpToDate { version } => Ok(UpdateResult::UpToDate { version }),
            UpdateOutcome::FallbackToFullDownload {
                full_download_url,
                reason,
            } => Ok(UpdateResult::RequiresFullDownload {
                url: full_download_url,
                reason,
            }),
            UpdateOutcome::SafeMode => Ok(UpdateResult::SafeMode),
        }
    }

    /// 发出安全模式相关事件（计数 + 剩余冷却时间）。
    async fn emit_safe_mode(&self) {
        let count = crate::core::safety::get_failed_attempts(&self.work_dir).await;
        crate::core::events::safety_mode_entered(count);
        if let Some(remaining) = crate::core::safety::safe_mode_remaining(&self.work_dir).await {
            crate::core::events::cooldown_active(remaining);
        }
    }

    /// 执行更新，成功打过补丁后**直接退出进程**。
    ///
    /// 这是给"不想自己处理退出时机"的宿主的一步到位入口：
    /// 补丁就绪后启动替换脚本并 `std::process::exit(0)`，
    /// 从根上杜绝"主进程不退出 → 被回滚"这个最常见的接入错误。
    ///
    /// - `UpdateOutcome::Patched`：已退出，永远不会返回；
    /// - 其它结果：原样返回 `Ok(())`，由宿主决定全量下载或提示用户；
    /// - 过程出错：返回 `Err`，同样由宿主决定。
    ///
    /// # 注意
    ///
    /// `std::process::exit` 会跳过析构函数——在调用它之前先落盘需要持久化的状态。
    /// 需要在退出前通知前端/发送 IPC 的宿主，请改用 [`Self::update`] + `std::process::exit`。
    ///
    /// Windows 上若更新目录需要管理员权限，替换脚本会弹 UAC 授权框。
    /// 最好在调用本方法前告知用户"更新过程中可能出现授权提示"，
    /// 避免用户把弹窗当成恶意行为而点击拒绝。
    pub async fn update_and_exit(&self) -> Result<()> {
        crate::core::crypto::install_crypto_provider();

        match self.update().await? {
            UpdateOutcome::Patched { staged } => {
                // auto_launch=false 时 update() 没有启动脚本，这里补上
                if !self.config.auto_launch {
                    staged.launch()?;
                }
                std::process::exit(0);
            }
            _ => Ok(()),
        }
    }

    /// 带进度回调的更新流程。
    ///
    /// `on_progress` 接收 `(已下载字节数, 总字节数)`，总字节数未知时为 0。
    pub async fn update_with_progress<F>(&self, on_progress: F) -> Result<UpdateOutcome>
    where
        F: FnMut(u64, u64),
    {
        crate::core::crypto::install_crypto_provider();

        if should_abort(&self.work_dir).await {
            self.emit_safe_mode().await;
            return Ok(UpdateOutcome::SafeMode);
        }

        self.run_prepare(self.config.auto_launch, Some(Box::new(on_progress)))
            .await
    }

    /// 下载并合并补丁，**不启动替换脚本**。
    ///
    /// 宿主可以在退出前先展示提示、发送 IPC 消息，再调用
    /// [`Staged::launch`] 触发替换，避免"脚本已启动但 UI 还没弹出来"的竞态。
    pub async fn prepare(&self) -> Result<UpdateOutcome> {
        crate::core::crypto::install_crypto_provider();

        if should_abort(&self.work_dir).await {
            self.emit_safe_mode().await;
            return Ok(UpdateOutcome::SafeMode);
        }

        self.run_prepare(false, None).await
    }

    /// 标记新版本启动成功（使用本更新器的工作目录）。
    ///
    /// 新版本必须在启动早期调用一次（或调用静态的
    /// [`PlatformUpdater::confirm_startup_success`]），
    /// 否则替换脚本会误判失败并回滚。
    pub fn confirm_startup_success(&self) -> Result<()> {
        if let Some(dir) = self.executable_path.parent() {
            let _ = std::fs::remove_file(dir.join(crate::core::artifacts::WORK_DIR_MARKER));
        }
        crate::platform::shared::confirm_startup_success_in(&self.work_dir)
    }

    /// 手动退出安全模式（宿主提供"重试"按钮时使用）。
    pub async fn reset_safety(&self) {
        crate::core::safety::reset_safety(&self.work_dir).await;
    }

    /// 内部实现：假定调用方已经完成安全模式检查与失败计数。
    async fn run(&self) -> Result<UpdateOutcome> {
        self.run_prepare(self.config.auto_launch, None).await
    }

    /// 核心流程。
    ///
    /// - `launch`：结束时是否启动替换脚本
    /// - `on_progress`：可选的下载进度回调
    async fn run_prepare(
        &self,
        launch: bool,
        on_progress: Option<Box<dyn FnMut(u64, u64) + '_>>,
    ) -> Result<UpdateOutcome> {
        // 互斥锁：防止多个更新进程同时改写文件。
        // 失败计数也在锁内记录，避免并发时的读-改-写竞争丢计数。
        let _lock = UpdateLock::acquire_in(&self.work_dir)?;

        let result = self.run_locked(launch, on_progress).await;

        // 暂态网络错误（Timeout/Network/5xx）不算"更新流程坏了"，重试即可；
        // 只有确定性错误（验签失败、哈希不符、磁盘不足等）才计入安全模式，
        // 否则服务器抖动几次就会把用户锁进安全模式。
        if let Err(e) = &result {
            crate::core::events::patch_rejected(&e.to_string());
            if !e.is_retryable() {
                let _ = record_failed_attempt(&self.work_dir).await;
            }
        }

        result
    }

    /// 持锁执行的实际更新流程。
    async fn run_locked(
        &self,
        launch: bool,
        mut on_progress: Option<Box<dyn FnMut(u64, u64) + '_>>,
    ) -> Result<UpdateOutcome> {
        // 清理上一轮遗留的临时文件
        cleanup_previous_updates(&self.work_dir, &self.executable_path).await;

        // 拉取并验签清单
        let manifest = self.fetch_verified_manifest().await?;

        // 版本单调性：签名合法 ≠ 比当前新。重放旧清单时必须在这里止步，
        // 否则会经全量回退把用户降级到旧版本。
        if !manifest.is_newer_than(&self.config.current_version)? {
            crate::core::events::check(&manifest.version, false, false);
            return Ok(UpdateOutcome::UpToDate {
                version: manifest.version,
            });
        }

        let full_download_url = manifest
            .platforms
            .get(&self.config.platform)
            .map(|p| p.url.clone());

        // 适用边界：平台/布局不适合增量替换时回退全量（正确性优先）。
        // 判定发生在下载补丁之前——清单仍会拉取（要知道有无新版本与全量地址），
        // 但不会为用不上的补丁消耗任何流量。
        if let Some(reason) =
            crate::platform::target::incremental_fallback_reason(&self.executable_path)
        {
            let reason = FallbackReason::EnvironmentUnsupported(reason);
            crate::core::events::fallback(&reason.to_string());
            return Ok(UpdateOutcome::FallbackToFullDownload {
                full_download_url,
                reason,
            });
        }

        // 匹配补丁
        let Some(patch) = manifest.find_patch(&self.config.current_version, &self.config.platform)
        else {
            let reason = FallbackReason::NoMatchingPatch {
                from: self.config.current_version.clone(),
                to: manifest.version.clone(),
            };
            crate::core::events::fallback(&reason.to_string());
            return Ok(UpdateOutcome::FallbackToFullDownload {
                full_download_url,
                reason,
            });
        };

        crate::core::events::patch_matched(
            &patch.from_version,
            &patch.to_version,
            patch.patch_size,
        );

        // 下载补丁
        let patch_path = self.work_dir.join(crate::core::artifacts::PATCH_FILE);
        // 体积上限从清单的 patch_size 推导（清单签名覆盖了它，所以可信）：
        // 1 MB 余量足够吸收服务端的分块差异；响应体在传输中仍可能被替换，
        // 无上限的流式写入会灌满磁盘。
        // 清单未声明 patch_size 时退到固定硬上限，保证任何路径下都有约束。
        let max_bytes = patch
            .patch_size
            .map(|size| size.saturating_add(1024 * 1024))
            .unwrap_or(MAX_PATCH_BYTES);

        // &mut dyn FnMut 本身实现了 FnMut，可直接作为回调传下去
        if let Some(callback) = on_progress.as_mut() {
            download_patch_with_progress(
                &self.client,
                &patch.patch_url,
                &patch_path,
                max_bytes,
                callback,
            )
            .await?;
        } else {
            download_patch_with_progress(
                &self.client,
                &patch.patch_url,
                &patch_path,
                max_bytes,
                |_, _| {},
            )
            .await?;
        }

        // 校验补丁哈希（下载后、应用前，避免用损坏的补丁浪费算力）
        let actual_patch_hash = calculate_hash(&patch_path).await?;
        if actual_patch_hash != patch.patch_hash.to_lowercase() {
            let _ = tokio::fs::remove_file(&patch_path).await;
            return Err(DeltaError::HashMismatch {
                expected: format!("Patch: {}", patch.patch_hash),
                actual: format!("Patch: {actual_patch_hash}"),
            });
        }

        // 应用补丁，生成新版本（产物经双端哈希校验，类型上排除未验证字节）
        let staged_path = self.work_dir.join(crate::core::artifacts::staged_file());
        let _ = tokio::fs::remove_file(&staged_path).await;
        let artifact = apply_patch(
            &self.executable_path,
            &patch_path,
            &staged_path,
            &patch.patch_hash,
            &patch.target_hash,
        )
        .await?;

        // 清理补丁文件
        let _ = tokio::fs::remove_file(&patch_path).await;

        // 脚本执行前最后一次确认产物仍是校验时的那份字节，
        // 封住"校验完成 → 脚本执行"之间的 TOCTOU 窗口。
        artifact.verify_integrity()?;

        // 生成替换脚本（只接受已验证产物；脚本放用户可写的工作目录）
        let script =
            PlatformUpdater::generate_updater(&self.executable_path, &artifact, &self.work_dir)?;

        // 本次更新成功，清除连续失败计数
        clear_failed_attempts(&self.work_dir).await;
        crate::core::events::patched(&manifest.version);

        let staged = Staged {
            staged_exe: artifact,
            script_path: script,
            manifest: Box::new(manifest),
            launched: std::sync::Arc::new(AtomicBool::new(false)),
        };

        // 按需启动替换脚本
        if launch {
            staged.launch()?;
        }

        Ok(UpdateOutcome::Patched { staged })
    }

    /// 下载并校验清单签名，返回可信的清单。
    async fn fetch_verified_manifest(&self) -> Result<UpdateManifest> {
        let manifest_bytes = fetch_bytes(&self.client, &self.config.manifest_url).await?;
        let signature = fetch_text(&self.client, &self.config.signature_url).await?;

        verify_manifest_signature(&manifest_bytes, &signature, &self.config.public_key)?;

        let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| DeltaError::ManifestInvalid(format!("清单格式错误: {e}")))?;

        // 版本绑定（发布端一致性）：签名合法 ≠ 版本正确。
        // 注意：绑定值与 manifest.version 都来自同一份签名材料，
        // 因此它**不能**替代客户端的版本单调性判断（见 is_newer_than）。
        // 这里只用于发现发布端把一个版本号写进另一版本清单的错误。
        if let Some(bound) = crate::core::verify::manifest_version_binding(&signature) {
            if bound != manifest.version {
                return Err(DeltaError::SignatureInvalid(format!(
                    "清单签名的版本绑定（{bound}）与清单声明版本（{}）不一致，疑似发布端配置错误",
                    manifest.version
                )));
            }
        }

        Ok(manifest)
    }
}

/// 本地验证闭环：在没有网络、没有签名密钥的情况下跑完
/// "生成补丁 → 应用补丁 → 校验产物"。
///
/// 这是给开发者在接入前亲眼确认"增量真的生效"用的：
///
/// ```no_run
/// # async fn example() -> tauri_delta_updater::Result<()> {
/// let report = tauri_delta_updater::simulate_update(
///     "old.exe",
///     "new.exe",
///     std::path::Path::new("target/delta-sim"),
/// )
/// .await?;
/// println!(
///     "补丁 {} 字节，全量 {} 字节，压缩比 {:.1}%",
///     report.patch_size,
///     report.target_size,
///     report.ratio() * 100.0
/// );
/// # Ok(())
/// # }
/// ```
///
/// 全过程只写 `work_dir`，不接触原始文件；结束时会删除临时产物。
pub async fn simulate_update(
    old_file: impl AsRef<Path>,
    new_file: impl AsRef<Path>,
    work_dir: impl AsRef<Path>,
) -> Result<SimulationReport> {
    let work_dir = work_dir.as_ref();
    crate::core::paths::ensure_writable(work_dir)?;

    let old = old_file.as_ref();
    let new = new_file.as_ref();

    let patch_path = work_dir.join("simulate.patch");
    let staged_path = work_dir.join("simulate.staged");
    let _ = tokio::fs::remove_file(&patch_path).await;
    let _ = tokio::fs::remove_file(&staged_path).await;

    create_patch(old, new, &patch_path).await?;

    let patch_size = std::fs::metadata(&patch_path)
        .map_err(DeltaError::Io)?
        .len();
    let target_size = std::fs::metadata(new).map_err(DeltaError::Io)?.len();
    let patch_hash = calculate_hash(&patch_path).await?;
    let target_hash = calculate_hash(new).await?;

    // 与真实客户端完全相同的路径：双端哈希校验 + 产物落盘
    let artifact = apply_patch(old, &patch_path, &staged_path, &patch_hash, &target_hash).await?;
    artifact.verify_integrity()?;

    let _ = tokio::fs::remove_file(&patch_path).await;
    let _ = tokio::fs::remove_file(&staged_path).await;

    Ok(SimulationReport {
        patch_size,
        target_size,
        patch_hash,
        target_hash,
    })
}

/// [`simulate_update`] 的结果。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SimulationReport {
    /// 补丁体积（字节）。
    pub patch_size: u64,
    /// 新版本体积（字节）。
    pub target_size: u64,
    /// 补丁 SHA-256。
    pub patch_hash: String,
    /// 新版本 SHA-256。
    pub target_hash: String,
}

impl SimulationReport {
    /// 补丁占全量的比例（0.0–1.0）。
    pub fn ratio(&self) -> f64 {
        if self.target_size == 0 {
            0.0
        } else {
            self.patch_size as f64 / self.target_size as f64
        }
    }
}

/// 清单与签名的最大下载体积。
///
/// 清单与签名正常只有几 KB。设定上限可以避免服务端被劫持后
/// 返回一个超大响应把进程内存打满。
const MAX_MANIFEST_BYTES: usize = 10 * 1024 * 1024;

/// 补丁的最大下载体积（清单未声明 `patch_size` 时的兜底）。
///
/// 正常补丁远小于此值；超过它只可能是服务端被劫持或配置错误。
const MAX_PATCH_BYTES: u64 = 512 * 1024 * 1024;

/// 由清单地址推导签名地址。
///
/// 在路径末尾追加 `.sig`，并**保留查询参数**：
/// `https://cdn/latest.json?token=abc` 应得到
/// `https://cdn/latest.json.sig?token=abc`，
/// 而不是把 `.sig` 追加到 token 后面。
fn default_signature_url(manifest_url: &str) -> String {
    match manifest_url.find('?') {
        Some(pos) => {
            let (base, query) = manifest_url.split_at(pos);
            format!("{base}.sig{query}")
        }
        None => format!("{manifest_url}.sig"),
    }
}

/// 下载二进制内容。
async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            DeltaError::Timeout(format!("请求 {url} 超时: {e}"))
        } else {
            DeltaError::Network(format!("请求 {url} 失败: {e}"))
        }
    })?;

    if !response.status().is_success() {
        return Err(DeltaError::HttpStatus(response.status().as_u16()));
    }

    // 先看服务端声明的长度，超限直接拒绝，避免把超大响应读进内存
    if let Some(len) = response.content_length() {
        if len > MAX_MANIFEST_BYTES as u64 {
            return Err(DeltaError::ResponseTooLarge {
                url: url.to_string(),
                limit: MAX_MANIFEST_BYTES as u64,
            });
        }
    }

    // 服务端可能不给 Content-Length，边读边计数兜底
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeltaError::Network(format!("读取 {url} 失败: {e}")))?;
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(DeltaError::ResponseTooLarge {
                url: url.to_string(),
                limit: MAX_MANIFEST_BYTES as u64,
            });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

/// 下载文本内容。
async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let bytes = fetch_bytes(client, url).await?;
    String::from_utf8(bytes)
        .map_err(|_| DeltaError::InvalidResponse(format!("{url} 不是合法的 UTF-8 文本")))
}
