//! 更新流程在磁盘上产生的临时文件与标志文件。
//!
//! 集中定义，避免清理逻辑与生成逻辑各写一份名字而对不上。
//!
//! # 文件名不再带应用品牌
//!
//! 早期版本把文件名写死成 `sereno_*`（作者自己的应用名），通用库不应如此。
//! 0.4.0 起统一改为 `tdx_*`；旧名字保留在 [`LEGACY_ARTIFACTS`] 里，
//! 只用于清理与双写兼容（升级瞬间的旧替换脚本仍能读到启动成功标志）。

/// 启动成功标志。新版本启动后写入，替换脚本据此判断是否保留新版本。
///
/// # 威胁模型假设
///
/// 标志内容是明文，同目录下任何有写权限的进程都能伪造它，
/// 让脚本误判"新版本启动成功"并删除备份。
/// 在桌面应用的威胁模型中，能写安装目录通常意味着已在本机执行代码，
/// 那时更新机制早已不是安全边界——因此不做防伪造设计。
/// 共享目录/多用户部署场景请自行评估这一残余风险。
pub const SUCCESS_FLAG: &str = "tdx_start_success.flag";

/// 旧版启动成功标志（0.3.x）。只在升级瞬间双写兼容，以及清理时识别。
pub const LEGACY_SUCCESS_FLAG: &str = "sereno_start_success.flag";

/// 连续失败计数。达到阈值后停止尝试增量更新。
///
/// 文件名保持不变：重命名会让升级用户的历史失败计数丢失，
/// 反而可能在某个持续失败的环境里重新触发一轮失败。
pub const FAILURE_COUNTER: &str = "update_failures.txt";

/// 增量补丁文件。
pub const PATCH_FILE: &str = "tdx_update.patch";

/// 合并后的新版本可执行文件的文件名。
///
/// 按平台选择：Windows 上带 `.exe` 后缀（符合惯例，也便于杀软白名单），
/// Unix 上不带扩展名。`core` 不硬编码任何平台的扩展名。
pub fn staged_file() -> &'static str {
    if cfg!(windows) {
        "tdx_staged_new.exe"
    } else {
        "tdx_staged_new"
    }
}

/// 更新脚本日志。
pub const LOG_FILE: &str = "tdx_update.log";

/// 工作目录指针文件（写在 exe 旁边）。
///
/// 新版本启动后据此知道该去哪个目录写启动成功标志：
/// 宿主可能通过 `UpdateConfig::work_dir` 指定了自定义目录，
/// 静态的 `confirm_startup_success()` 无法凭空知道它。
/// 由替换脚本在启动新版本前写入，确认成功后被读取并删除。
pub const WORK_DIR_MARKER: &str = "tdx_update_dir";

/// 替换脚本文件名（按平台）。
pub fn updater_script() -> &'static str {
    if cfg!(windows) {
        "tdx_update.ps1"
    } else {
        "tdx_update.sh"
    }
}

/// 历史替换脚本，只用于清理。
///
/// 日志不在此列：旧日志是排查升级失败的重要证据，不应主动删除。
pub const LEGACY_SCRIPTS: &[&str] = &["sereno_update.ps1", "sereno_update.sh", "sereno_update.bat"];

/// 更新互斥锁。
///
/// 名字保持不变：升级瞬间新旧两代进程必须争同一把锁，
/// 换名字会让它们在过渡期同时改写文件。
pub const LOCK_FILE: &str = ".update.lock";

/// 连续失败多少次后停止尝试增量更新。
pub const MAX_FAILED_ATTEMPTS: u32 = 3;

/// 安全模式冷却时间：达到阈值后等待这么久，允许一次新的尝试。
///
/// 没有冷却的安全模式是"永久禁用增量"：一次磁盘满或权限错误就可能让
/// 用户再也不走增量路径。冷却让环境问题修复后能自动恢复。
pub const SAFE_MODE_COOLDOWN_SECS: u64 = 6 * 60 * 60;
