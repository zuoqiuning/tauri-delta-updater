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
}

impl DeltaError {
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
            _ => false,
        }
    }

    /// 判断该错误是否属于**安全阻断**——必须硬性终止更新流程并明确告知用户，
    /// 绝不能静默降级成"普通失败"或默默走全量下载。
    ///
    /// - [`DeltaError::SignatureInvalid`]：清单被篡改或公钥不对，意味着可能正被攻击；
    /// - [`DeltaError::HashMismatch`]：内容与签名锚定的哈希不符，可能被替换或损坏。
    ///
    /// 其余错误（网络、磁盘、配置）是普通失败，可按需重试或回退全量。
    pub fn is_security_blocking(&self) -> bool {
        matches!(
            self,
            DeltaError::SignatureInvalid(_) | DeltaError::HashMismatch { .. }
        )
    }
}

/// 增量更新操作的结果类型。
pub type Result<T> = std::result::Result<T, DeltaError>;
