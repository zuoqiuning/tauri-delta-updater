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

/// 补丁身份记录：内容是清单里已签名的 `patch_hash`。
///
/// 跨进程续传必须能回答"残留的半截文件还是同一个补丁吗"：
/// 直接按 Range 追加到上一个版本的补丁上，只会拼出一个哈希必然不符的文件。
pub const PATCH_META: &str = "tdx_update.patch.id";

/// 整包补丁文件。
pub const BUNDLE_PATCH_FILE: &str = "tdx_update.bundle.patch";

/// 整包补丁身份记录。
pub const BUNDLE_PATCH_META: &str = "tdx_update.bundle.patch.id";

/// 全量安装包的文件名前缀。
///
/// 实际文件名是 `tdx_full_installer_<version><扩展名>`：版本在名字里，
/// 天然区分"同一个安装包"与"上一个版本的残留"，续传不会拼错文件。
pub const FULL_INSTALLER_PREFIX: &str = "tdx_full_installer";

/// 全量安装的临时解包目录（macOS `.app.tar.gz` 解到这里的 `.app`）。
pub const FULL_STAGING_DIR: &str = "tdx_full_staging";

/// 整包暂存目录：解压出的新版本文件在这里逐项校验后才进入替换脚本。
///
/// macOS 使用 `.app` 后缀，让 `codesign --verify` 把暂存目录当作 bundle 校验；
/// 目录名本身不参与签名，只用于工作目录内定位与清理。
pub fn bundle_staging_dir() -> &'static str {
    if cfg!(target_os = "macos") {
        "tdx_bundle_staging.app"
    } else {
        "tdx_bundle_staging"
    }
}

/// 整包替换前的备份目录。
///
/// 放在工作目录而不是安装目录：系统级安装目录对普通用户只读，
/// 备份写不进去会让替换在第一步就失败；脚本成功后自动清理。
pub const BUNDLE_BACKUP_DIR: &str = "tdx_bundle_backup";

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

/// 多跳补丁链的中间产物（交替使用：`apply_patch` 的输入与输出不能是同一个文件）。
pub const STAGED_HOP_A: &str = "tdx_staged_hop_a";

/// 多跳补丁链的中间产物（第二路）。
pub const STAGED_HOP_B: &str = "tdx_staged_hop_b";

/// 旧待安装产物的临时副本：新更新失败时用它还原，成功时删除。
pub const STAGED_BACKUP: &str = "tdx_staged_backup";

/// 更新脚本日志。
pub const LOG_FILE: &str = "tdx_update.log";

/// 替换脚本的结构化事件流（JSON Lines，一行一个事件）。
///
/// 回滚发生在宿主退出之后，进程内只能看到最终原因；关键阶段同时写入
/// 这个文件，宿主下次启动时可以逐行解析，而不必用正则去读自由格式日志。
/// 记录回滚与正向阶段事件，字段固定为 `event` / `reason` / `time`，
/// 未知字段应被忽略以兼容后续扩展。
pub const EVENTS_FILE: &str = "tdx_events.jsonl";

/// 事件名：替换脚本整体回滚。
pub const EVENT_ROLLBACK: &str = "delta.rollback.triggered";
/// 事件名：补丁下载并校验完成（Rust 侧写入）。
pub const EVENT_DOWNLOAD_COMPLETED: &str = "delta.stage.download_completed";
/// 事件名：替换脚本已生成（Rust 侧写入）。
pub const EVENT_SCRIPT_READY: &str = "delta.stage.script_ready";
/// 事件名：脚本开始替换文件（脚本侧写入）。
pub const EVENT_REPLACE_STARTED: &str = "delta.stage.replace_started";
/// 事件名：新版本启动确认成功（脚本侧写入）。
pub const EVENT_STARTUP_CONFIRMED: &str = "delta.stage.startup_confirmed";

/// 追加一行结构化事件；失败静默（诊断事件不能影响更新流程）。
///
/// 同步写入：几字节的追加不值得把整个调用方的 async 状态机变大
/// （Windows 测试线程栈只有 2 MB，状态机膨胀会直接栈溢出）。
/// 只有事件名，没有时间戳：JSONL 的行序就是时间序，脚本侧事件自带 `time`。
pub fn append_event(work_dir: &std::path::Path, event: &str) {
    use std::io::Write;

    let line = format!("{{\"event\":\"{event}\"}}\n");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(work_dir.join(EVENTS_FILE))
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// 整包替换日志：记录本次运行新增了哪些文件（崩溃后修复时删除它们）。
///
/// 备份目录只保存"被覆盖/被删除"的旧文件；新版本新增的文件没有旧副本，
/// 必须靠日志才能知道该删哪些，否则修复后旧版本会看到多余的新文件。
pub const BUNDLE_JOURNAL_FILE: &str = "tdx_bundle_journal.txt";

/// 跨重启待安装记录（`prepare` 成功后落盘，`launch`/下次启动时消费）。
pub const PENDING_FILE: &str = "tdx_pending.json";

/// 启动状态指针：`staged` → `booting` → `committed`，回滚时写 `rolled_back`。
///
/// 替换脚本在启动新版本前写 `staged`；新版本启动早期写 `booting`；
/// 宿主健康检查通过并调用确认后写 `committed`。下次启动的 `diagnose()`
/// 据此区分"从未启动"、"启动了但未确认"与"已提交"。
pub const UPDATE_STATE_FILE: &str = "tdx_update_state";

/// 回滚原因：新版本启动成功但在宿主的健康检查阶段失败。
pub const HEALTH_CHECK_FAILED: &str = "health_check_failed";

/// 回滚原因：新版本进程从未执行到宿主的启动代码。
pub const STARTUP_NOT_STARTED: &str = "startup_not_started";

/// 回滚标记：替换脚本整体回滚时写入原因，供下一次启动的 `diagnose()` 展示。
///
/// 更新失败发生在宿主退出之后的脚本里，进程内无法上报；把原因落盘，
/// 才不会表现为“提示更新成功、重启后版本没变”且没有任何线索。
pub const ROLLBACK_FLAG: &str = "tdx_rollback.txt";

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

/// 整包（多文件）替换脚本文件名（按平台）。
pub fn bundle_updater_script() -> &'static str {
    if cfg!(windows) {
        "tdx_bundle.ps1"
    } else {
        "tdx_bundle.sh"
    }
}

/// 整包修复脚本文件名（按平台）。
pub fn bundle_repair_script() -> &'static str {
    if cfg!(windows) {
        "tdx_repair.ps1"
    } else {
        "tdx_repair.sh"
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

/// 跨用户互斥锁（`LockScope::InstallDir`）。
///
/// 放在安装目录而不是用户缓存目录：per-machine 安装的多个用户账户
/// 必须争同一把锁，否则会同时替换 `Program Files` 里的文件。
/// 目录只读（需要提权）时无法创建，引擎会退回仅工作目录锁并告警。
pub const MACHINE_LOCK_FILE: &str = "tdx_machine.lock";

/// 跨用户锁降级的标记：内容为失败原因（安装目录路径）。
///
/// 安装目录只读时无法建立机器级锁，只能退回工作目录锁；诊断页读取本文件
/// 说明"跨用户锁没有生效"，而不是让宿主自己猜测。
pub const MACHINE_LOCK_FALLBACK_FILE: &str = "tdx_machine_lock_fallback";

/// 连续失败多少次后停止尝试增量更新。
pub const MAX_FAILED_ATTEMPTS: u32 = 3;

/// 安全模式冷却时间：达到阈值后等待这么久，允许一次新的尝试。
///
/// 没有冷却的安全模式是"永久禁用增量"：一次磁盘满或权限错误就可能让
/// 用户再也不走增量路径。冷却让环境问题修复后能自动恢复。
pub const SAFE_MODE_COOLDOWN_SECS: u64 = 6 * 60 * 60;
