//! 各平台共用的辅助逻辑。
//!
//! 原本这些函数在 windows / macos / linux 三份文件里逐字重复，
//! 改一处漏两处是迟早的事，因此集中到这里。

use crate::core::artifacts;
use crate::core::safety;
use crate::error::{DeltaError, Result};
use std::path::{Path, PathBuf};

/// 读取文件名，失败时返回明确的错误而不是 panic。
pub fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| DeltaError::PatchApplyFailed(format!("无效的文件名: {}", path.display())))
}

/// 转义要写入 shell 脚本的单引号字符串。
///
/// 单引号字符串里只有 `'` 需要转义（写成 `'\''`），
/// 其余字符（包括空格 `$` `` ` `` `\` `"` `&` `|`）都是字面量。
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 把字符串编码成 PowerShell 的单引号字面量。
///
/// 单引号字符串里只有 `'` 需要转义（写成 `''`），
/// 其余字符（包括 `&` `|` `%` `!` `$` 反引号）都是字面量。
pub fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// 标记新版本启动成功。
///
/// 优先读取替换脚本写在 exe 旁边的**工作目录指针**（[`artifacts::WORK_DIR_MARKER`]），
/// 因此即使宿主通过 `UpdateConfig::work_dir` 指定了自定义目录也能正确落位。
/// 没有指针时退回到由可执行文件路径推导的默认工作目录。
pub fn confirm_startup_success() -> Result<()> {
    let exe_path = std::env::current_exe().map_err(DeltaError::Io)?;
    let exe_dir = exe_path.parent();

    let work_dir = exe_dir
        .map(|dir| dir.join(artifacts::WORK_DIR_MARKER))
        .filter(|marker| marker.is_file())
        .and_then(|marker| std::fs::read_to_string(marker).ok())
        .map(|content| PathBuf::from(content.trim_start_matches('\u{feff}').trim()))
        .unwrap_or_else(|| crate::core::paths::default_work_dir(&exe_path));

    if let Some(dir) = exe_dir {
        let _ = std::fs::remove_file(dir.join(artifacts::WORK_DIR_MARKER));
    }

    confirm_startup_success_in(&work_dir)
}

/// 在指定工作目录写入启动成功标志，并清除连续失败计数。
///
/// # 为什么标志不放在 exe 旁边
///
/// 系统级安装（`C:\Program Files`、`/usr/bin`）对普通用户只读，
/// 新版本应用自己写不进去。标志必须放在用户可写的工作目录。
///
/// # 升级兼容
///
/// 0.3.x 生成的替换脚本在 exe 目录下等待旧标志名。新版本启动时
/// **额外尽力写一次旧标志**：否则从 0.3.x 升级到 0.4.0 的那一次更新，
/// 旧脚本会等不到标志而回滚。写入失败（目录只读）时静默忽略——
/// 那正是需要新机制的场景。
pub fn confirm_startup_success_in(work_dir: &Path) -> Result<()> {
    safety::clear_failed_attempts_sync(work_dir);

    std::fs::create_dir_all(work_dir).map_err(DeltaError::Io)?;
    std::fs::write(work_dir.join(artifacts::SUCCESS_FLAG), "ok").map_err(DeltaError::Io)?;

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = std::fs::write(dir.join(artifacts::LEGACY_SUCCESS_FLAG), "ok");
        }
    }
    Ok(())
}
