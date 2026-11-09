use crate::platform::full_install::InstallFailureKind;
use std::path::PathBuf;
use thiserror::Error;

/// 增量更新过程中可能出现的错误。
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum DeltaError {
    /// 文件系统操作失败。
    #[error("文件系统操作失败: {0}")]
    Io(#[from] std::io::Error),

    /// 网络请求或下载失败。
    #[error("网络请求或下载失败: {0}")]
    Network(String),

    /// 网络请求超时。
    #[error("网络请求超时: {0}")]
    Timeout(String),

    /// HTTP 状态码错误。5xx 与 429 属于可重试的服务端故障。
    #[error("下载失败，HTTP 状态码: {0}")]
    HttpStatus(u16),

    /// 生成增量补丁失败。
    #[error("生成增量补丁失败: {0}")]
    PatchGenFailed(String),

    /// 应用增量补丁失败（文件损坏或算法异常）。
    #[error("应用增量补丁失败（文件损坏或算法异常）: {0}")]
    PatchApplyFailed(String),

    /// 签名校验失败。清单或补丁的签名与公钥不匹配，意味着内容被篡改或密钥不对。
    #[error("签名校验失败！内容可能已被篡改，已阻断更新: {0}")]
    SignatureInvalid(String),

    /// 清单内容本身非法（JSON 错误、缺少必需字段、版本号不是语义化版本等）。
    ///
    /// 与 [`DeltaError::SignatureInvalid`] 区分：签名可能合法，但发布端写坏了清单。
    #[error("清单内容非法: {0}")]
    ManifestInvalid(String),

    /// 响应内容非法（例如签名文件不是 UTF-8）。
    ///
    /// 确定性错误，重试没有意义。
    #[error("响应内容非法: {0}")]
    InvalidResponse(String),

    /// 服务端拒绝了断点续传请求，本地残留文件不可信，需要重新完整下载。
    #[error("服务端拒绝了续传范围，本地文件可能已损坏")]
    ResumeRejected,

    /// 补丁响应体超过清单声明的体积上限，疑似被劫持或损坏。
    ///
    /// 确定性错误，重试没有意义。
    #[error("补丁响应体超过 {max_bytes} 字节上限，已中止下载并删除半截文件")]
    PatchTooLarge {
        /// 体积上限（字节）。
        max_bytes: u64,
    },

    /// 解压后的补丁超过上限，疑似损坏或构造的攻击载荷。
    ///
    /// 确定性错误：正常 bsdiff 补丁远小于该上限。
    #[error("补丁解压后超过 {max_bytes} 字节上限，已中止")]
    PatchExpandedTooLarge {
        /// 体积上限（字节）。
        max_bytes: u64,
    },

    /// 差分的预估工作集超过上限。
    ///
    /// 发布端生成补丁时需要 `max(17n, 9n+m)` 的工作内存（n/m 为旧/新文件大小）。
    /// 超过上限时跳过该基线，而不是分配失败后 abort 进程。
    #[error("文件过大：生成补丁预计需要 {required} 字节内存（上限 {limit}），已跳过")]
    PatchWorkingSetTooLarge {
        /// 预估工作集（字节）。
        required: u64,
        /// 允许的上限（字节）。
        limit: u64,
    },

    /// 清单/签名响应体超过体积上限。
    ///
    /// 确定性错误——被劫持的服务端返回超大响应时，重试只会反复消耗带宽。
    #[error("{url} 响应体超过 {limit} 字节上限，已拒绝下载")]
    ResponseTooLarge {
        /// 请求地址。
        url: String,
        /// 体积上限（字节）。
        limit: u64,
    },

    /// 哈希校验不通过：期望值与实际值不符。
    #[error("安全校验失败！期望: {expected}, 实际: {actual}")]
    HashMismatch {
        /// 期望的哈希值。
        expected: String,
        /// 实际计算出的哈希值。
        actual: String,
    },

    /// 目标文件已存在，拒绝覆盖。
    #[error("目标文件已存在，拒绝覆盖: {0}")]
    TargetExists(String),

    /// 磁盘空间不足，无法继续更新。
    #[error("磁盘空间不足，无法继续更新")]
    InsufficientDiskSpace,

    /// 配置错误：在运行前就能确定无法成功。
    #[error("配置错误: {0}")]
    Config(String),

    /// 工作目录不可写。
    ///
    /// 独立变体便于宿主把"安装目录只读"与其它配置错误分开处理
    /// （例如提示用户改用用户级安装）。
    #[error("工作目录不可写: {0}")]
    WorkDirNotWritable(PathBuf),

    /// 版本号非法或无法比较。
    #[error("版本号非法: {0}")]
    VersionInvalid(String),

    /// 文件过大，增量更新不适用。应回退到全量下载。
    #[error("文件过大（{size} 字节），增量更新不适用，请走全量下载")]
    IncrementalNotApplicable {
        /// 文件大小（字节）。
        size: u64,
    },

    /// 更新任务执行失败。
    #[error("更新任务执行失败: {0}")]
    WorkerFailed(String),

    /// 另一个更新进程正在运行，锁竞争。
    ///
    /// 与 [`DeltaError::WorkerFailed`] 区分开：这是**可重试的正常情况**，
    /// 调用方可以 `match` 它并提示"稍后再试"，而不是当成异常失败。
    /// 注意它不属于 [`DeltaError::is_retryable`] 的自动重试范围
    /// （重试应等用户操作或下次调度触发，而不是立即重试）。
    #[error("另一个更新进程正在运行，请稍后再试")]
    UpdateInProgress,

    /// 更新被调用方主动取消。
    ///
    /// 用户取消不是故障：不计入安全模式的失败次数，也不会自动重试。
    /// 半截补丁与暂存产物会在返回前清理。
    #[error("更新已取消")]
    Cancelled,

    /// 整包（多文件）更新数据非法。
    ///
    /// 覆盖路径穿越、条目损坏、归档与文件清单不一致等情况。
    /// 属于确定性错误：重试没有意义，应回退全量下载。
    #[error("整包更新数据非法: {0}")]
    BundleInvalid(String),

    /// 重建的产物没有有效的 Authenticode 签名，而原版本是签名的。
    ///
    /// 内容哈希可能完全正确，但在开启 AppLocker / WDAC / SmartScreen 严格模式
    /// 的机器上，未签名程序会被直接拒绝启动——替换后应用等于变砖。
    /// 这通常意味着发布端漏签了新版本；调用方应回退全量或联系发布方。
    #[error("更新产物缺少有效的 Authenticode 签名（原版本已签名），已阻断更新")]
    AuthenticodeUnsigned,

    /// 全量安装器执行失败：启动失败、用户取消提权、安装器不接受空参数等。
    ///
    /// 与 [`DeltaError::Io`] 区分：这是"安装包已经验签通过，但装不上"，
    /// UI 应提示用户手动运行安装包，而不是重试下载。
    /// `kind` 让前端可以精确分支（例如取消提权时提示“已取消”而不是报错）。
    #[error("全量安装失败: {message}")]
    FullInstallFailed {
        /// 结构化的失败类型。
        kind: InstallFailureKind,
        /// 面向开发者的详细说明。
        message: String,
    },

    /// 多跳补丁链中的某一跳失败（`hop` 从 1 开始）。
    ///
    /// 保留底层错误的错误码、可重试性与安全分类；`Display` 额外指出跳数与
    /// 版本区间，便于定位是哪一跳、哪个中间产物校验失败。
    #[error("补丁链第 {hop}/{total} 跳（{from} → {to}）失败: {source}")]
    ChainHopFailed {
        /// 失败的跳数（从 1 开始）。
        hop: usize,
        /// 链的总跳数。
        total: usize,
        /// 该跳的起始版本。
        from: String,
        /// 该跳的目标版本。
        to: String,
        /// 底层错误（保留原始错误码与安全分类）。
        #[source]
        source: Box<DeltaError>,
    },

    /// 连续更新失败次数已达上限，已进入安全模式。
    ///
    /// [`crate::DeltaUpdater::check`] 用它表示"检查无法进行"——此时连清单
    /// 都不会拉取；[`crate::DeltaUpdater::update`] 则返回
    /// `Ok(UpdateOutcome::SafeMode)`（更新可以执行并决定不更新）。
    ///
    /// 安全模式带有冷却时间（见 [`crate::core::artifacts::SAFE_MODE_COOLDOWN_SECS`]），
    /// 冷却结束后会自动恢复，也可以用 [`crate::DeltaUpdater::reset_safety`]
    /// 手动退出。
    #[error("连续更新失败次数已达上限，已停止尝试增量更新")]
    SafeMode,

    /// 上次整包替换未完成：安装目录处于半新半旧状态。
    ///
    /// 全量下载/安装会覆盖这个现场，但残留备份仍会让下一次启动生成修复脚本，
    /// 把刚装好的新版本回滚。必须先用 [`crate::DeltaUpdater::update`] 或
    /// [`crate::DeltaUpdater::prepare`] 完成修复。
    #[error("上次整包替换未完成，请先执行修复（update / prepare）后再全量安装")]
    RepairRequired,
}

impl DeltaError {
    /// 稳定的机器可读错误码（`snake_case`）。
    ///
    /// 文案会改，错误码不会：宿主/前端应按本方法的返回值分支，
    /// 而不是对 `Display` 的中文做字符串匹配。新增变体只会增加新码，
    /// 已发布的码保持含义不变。
    ///
    /// ```
    /// use tauri_delta_updater::DeltaError;
    /// let err = DeltaError::UpdateInProgress;
    /// assert_eq!(err.code(), "update_in_progress");
    /// ```
    pub fn code(&self) -> &'static str {
        match self {
            DeltaError::Io(_) => "io",
            DeltaError::Network(_) => "network",
            DeltaError::Timeout(_) => "timeout",
            DeltaError::HttpStatus(_) => "http_status",
            DeltaError::PatchGenFailed(_) => "patch_gen_failed",
            DeltaError::PatchApplyFailed(_) => "patch_apply_failed",
            DeltaError::SignatureInvalid(_) => "signature_invalid",
            DeltaError::ManifestInvalid(_) => "manifest_invalid",
            DeltaError::InvalidResponse(_) => "invalid_response",
            DeltaError::ResumeRejected => "resume_rejected",
            DeltaError::PatchTooLarge { .. } => "patch_too_large",
            DeltaError::PatchExpandedTooLarge { .. } => "patch_expanded_too_large",
            DeltaError::PatchWorkingSetTooLarge { .. } => "patch_working_set_too_large",
            DeltaError::ResponseTooLarge { .. } => "response_too_large",
            DeltaError::HashMismatch { .. } => "hash_mismatch",
            DeltaError::TargetExists(_) => "target_exists",
            DeltaError::InsufficientDiskSpace => "insufficient_disk_space",
            DeltaError::Config(_) => "config",
            DeltaError::WorkDirNotWritable(_) => "work_dir_not_writable",
            DeltaError::VersionInvalid(_) => "version_invalid",
            DeltaError::IncrementalNotApplicable { .. } => "incremental_not_applicable",
            DeltaError::WorkerFailed(_) => "worker_failed",
            DeltaError::UpdateInProgress => "update_in_progress",
            DeltaError::Cancelled => "cancelled",
            DeltaError::BundleInvalid(_) => "bundle_invalid",
            DeltaError::AuthenticodeUnsigned => "authenticode_unsigned",
            DeltaError::FullInstallFailed { .. } => "full_install_failed",
            DeltaError::ChainHopFailed { source, .. } => source.code(),
            DeltaError::SafeMode => "safe_mode",
            DeltaError::RepairRequired => "repair_required",
        }
    }

    /// 判断该错误是否值得重试。
    ///
    /// 只对"网络暂时不可用"类错误重试；校验失败、文件已存在等属于确定性错误，
    /// 重试只会浪费时间并掩盖真正的原因。
    pub fn is_retryable(&self) -> bool {
        match self {
            DeltaError::Timeout(_) => true,
            DeltaError::Network(_) => true,
            // 服务端临时故障或限流可以重试
            DeltaError::HttpStatus(code) => *code >= 500 || *code == 429,
            // 链路上的网络/超时错误与单跳完全相同；确定性错误仍不重试。
            DeltaError::ChainHopFailed { source, .. } => source.is_retryable(),
            _ => false,
        }
    }

    /// 判断该错误是否属于**安全阻断**——必须硬性终止更新流程并明确告知用户，
    /// 绝不能静默降级成"普通失败"或默默走全量下载。
    ///
    /// - [`DeltaError::SignatureInvalid`]：清单被篡改或公钥不对，意味着可能正被攻击；
    /// - [`DeltaError::HashMismatch`]：内容与签名锚定的哈希不符，可能被替换或损坏；
    /// - [`DeltaError::AuthenticodeUnsigned`]：产物签名状态比原版本退化了，
    ///   在受管环境里会直接无法启动，同样属于必须硬性阻断的情况。
    ///
    /// 其余错误（网络、磁盘、配置）是普通失败，可按需重试或回退全量。
    pub fn is_security_blocking(&self) -> bool {
        match self {
            DeltaError::SignatureInvalid(_)
            | DeltaError::HashMismatch { .. }
            | DeltaError::AuthenticodeUnsigned => true,
            // 链中任何一跳的哈希不符仍然是安全阻断。
            DeltaError::ChainHopFailed { source, .. } => source.is_security_blocking(),
            _ => false,
        }
    }
}

/// 增量更新操作的结果类型。
pub type Result<T> = std::result::Result<T, DeltaError>;

#[cfg(test)]
mod tests {
    use super::DeltaError;

    /// 每个变体各构造一个实例：新增变体时这里没跟着加就会编译失败，
    /// 强迫作者显式决定它的错误码。
    fn all_variants() -> Vec<DeltaError> {
        use DeltaError as E;
        vec![
            E::Io(std::io::Error::other("x")),
            E::Network(String::new()),
            E::Timeout(String::new()),
            E::HttpStatus(500),
            E::PatchGenFailed(String::new()),
            E::PatchApplyFailed(String::new()),
            E::SignatureInvalid(String::new()),
            E::ManifestInvalid(String::new()),
            E::InvalidResponse(String::new()),
            E::ResumeRejected,
            E::PatchTooLarge { max_bytes: 1 },
            E::PatchExpandedTooLarge { max_bytes: 1 },
            E::PatchWorkingSetTooLarge {
                required: 1,
                limit: 1,
            },
            E::ResponseTooLarge {
                url: String::new(),
                limit: 1,
            },
            E::HashMismatch {
                expected: String::new(),
                actual: String::new(),
            },
            E::TargetExists(String::new()),
            E::InsufficientDiskSpace,
            E::Config(String::new()),
            E::WorkDirNotWritable(std::path::PathBuf::new()),
            E::VersionInvalid(String::new()),
            E::IncrementalNotApplicable { size: 1 },
            E::WorkerFailed(String::new()),
            E::UpdateInProgress,
            E::Cancelled,
            E::BundleInvalid(String::new()),
            E::AuthenticodeUnsigned,
            E::FullInstallFailed {
                kind: crate::platform::full_install::InstallFailureKind::Other,
                message: String::new(),
            },
            E::ChainHopFailed {
                hop: 1,
                total: 2,
                from: String::new(),
                to: String::new(),
                source: Box::new(E::Network(String::new())),
            },
            E::SafeMode,
            E::RepairRequired,
        ]
    }

    #[test]
    fn codes_are_unique_snake_case() {
        // ChainHopFailed 有意委托底层错误码（宿主仍按 hash_mismatch / network 分支），
        // 因此不参与"每个变体一个唯一码"的计数；委托关系由上一条测试守住。
        let mut codes: Vec<&'static str> = all_variants()
            .iter()
            .filter(|err| !matches!(err, DeltaError::ChainHopFailed { .. }))
            .map(DeltaError::code)
            .collect();
        for code in &codes {
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "错误码必须是 snake_case: {code}"
            );
            assert!(!code.starts_with('_') && !code.ends_with('_'), "{code}");
        }
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "错误码必须唯一，否则前端无法分支");
    }

    /// 链式错误必须保留底层错误的错误码与安全分类，同时指出失败跳数。
    #[test]
    fn chain_hop_failed_preserves_inner_classification() {
        let security = DeltaError::ChainHopFailed {
            hop: 2,
            total: 3,
            from: "0.1.0".into(),
            to: "0.2.0".into(),
            source: Box::new(DeltaError::HashMismatch {
                expected: "a".into(),
                actual: "b".into(),
            }),
        };
        assert_eq!(security.code(), "hash_mismatch");
        assert!(security.is_security_blocking());
        assert!(!security.is_retryable());
        assert!(
            security.to_string().contains("第 2/3 跳"),
            "错误文案必须指出失败跳数: {security}"
        );

        let transient = DeltaError::ChainHopFailed {
            hop: 1,
            total: 2,
            from: "0.1.0".into(),
            to: "0.2.0".into(),
            source: Box::new(DeltaError::Network("timeout".into())),
        };
        assert_eq!(transient.code(), "network");
        assert!(transient.is_retryable());
        assert!(!transient.is_security_blocking());
    }
}
