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

/// 模板守卫：所有 `__PLACEHOLDER__` 必须被替换。
///
/// 漏替换不会让脚本生成失败，只会在真机执行时才暴露。
/// 返回错误而不是 `debug_assert`：release 构建也必须拒绝写出一个
/// 无法执行的脚本。
pub(crate) fn assert_rendered(script: &str) -> Result<()> {
    if !script.contains("__") {
        return Ok(());
    }
    // 把漏替换的占位符列出来，排障时不必打开模板逐行搜索。
    // 只认 `__UPPER_CASE__` 形状：用户路径里可能合法地包含 `__`，
    // 不能因为出现双下划线就拒绝生成脚本。
    let mut leftovers: Vec<&str> = Vec::new();
    let mut cursor = 0;
    while let Some(start) = script[cursor..].find("__").map(|pos| cursor + pos) {
        let Some(close) = script[start + 2..].find("__") else {
            break;
        };
        let end = start + 2 + close + 2;
        let inner = &script[start + 2..end - 2];
        if !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && !leftovers.contains(&&script[start..end])
        {
            leftovers.push(&script[start..end]);
        }
        cursor = end;
    }
    if leftovers.is_empty() {
        return Ok(());
    }
    Err(DeltaError::Config(format!(
        "脚本模板存在未替换的占位符: {}",
        leftovers.join(", ")
    )))
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
    let (work_dir, marker) = resolve_work_dir()?;
    if let Some(marker) = marker {
        let _ = std::fs::remove_file(marker);
    }
    confirm_startup_success_in(&work_dir)
}

/// 带健康检查的启动确认：只有检查通过才写成功标志。
///
/// 检查失败时立刻写回滚原因 `health_check_failed`，替换脚本会尽快回滚，
/// 而不是等满 `confirm_timeout_secs` 才判定超时。
pub fn confirm_startup_success_with<F>(health: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let (work_dir, marker) = resolve_work_dir()?;
    confirm_startup_success_with_in(&work_dir, marker.as_deref(), health)
}

/// 在指定工作目录执行健康检查并确认启动。
pub fn confirm_startup_success_with_in<F>(
    work_dir: &Path,
    marker: Option<&Path>,
    health: F,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    match health() {
        Ok(()) => {
            if let Some(marker) = marker {
                let _ = std::fs::remove_file(marker);
            }
            confirm_startup_success_in(work_dir)
        }
        Err(err) => {
            // 只有本次确实是"替换后的首次启动"才写回滚标记；
            // 普通启动的健康检查失败不应污染下一次更新的状态。
            if let Some(marker) = marker {
                let _ = std::fs::remove_file(marker);
                mark_health_check_failed(work_dir);
            }
            Err(err)
        }
    }
}

/// 健康检查失败：写状态与回滚原因，交由替换脚本回滚。
pub fn mark_health_check_failed(work_dir: &Path) {
    let _ = crate::core::startup::write(work_dir, crate::core::startup::StartupState::Failed);
    let _ = std::fs::write(
        work_dir.join(artifacts::ROLLBACK_FLAG),
        artifacts::HEALTH_CHECK_FAILED,
    );
}

/// 解析本次启动的工作目录指针。
///
/// 返回 `(工作目录, 指针文件路径)`；指针不存在时第二项为 `None`，
/// 工作目录退回默认值。
fn resolve_work_dir() -> Result<(PathBuf, Option<PathBuf>)> {
    let exe_path = crate::platform::target::runtime_executable().map_err(DeltaError::Io)?;
    let Some(exe_dir) = exe_path.parent() else {
        return Ok((crate::core::paths::default_work_dir(&exe_path), None));
    };
    let marker = exe_dir.join(artifacts::WORK_DIR_MARKER);
    if !marker.is_file() {
        return Ok((crate::core::paths::default_work_dir(&exe_path), None));
    }
    let work_dir = std::fs::read_to_string(&marker)
        .map(|content| PathBuf::from(content.trim_start_matches('\u{feff}').trim()))
        .ok()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| crate::core::paths::default_work_dir(&exe_path));
    Ok((work_dir, Some(marker)))
}

/// 本次启动是否来自一次替换。
///
/// 新引擎的替换脚本会写 [`artifacts::WORK_DIR_MARKER`]；**旧引擎（0.2.3 及更早）**
/// 驱动的替换不会写这个标记，只会在 exe 目录留下旧脚本文件。跨代际升级时
/// 必须把旧脚本也当作替换证据，否则新版本不会确认启动，旧脚本会等到超时回滚。
///
/// 旧脚本在确认成功后由 [`confirm_startup_success_in`] 清理，因此普通启动不会
/// 一直命中；新脚本不自删，但它会写 marker，marker 被确认流程删除后即不再命中。
pub(crate) fn has_replacement_evidence(exe_dir: &Path) -> bool {
    exe_dir.join(artifacts::WORK_DIR_MARKER).is_file()
        || artifacts::LEGACY_SCRIPTS
            .iter()
            .any(|name| exe_dir.join(name).is_file())
}

/// 仅当本次启动确实来自一次替换时才确认成功。
///
/// 替换证据 = 工作目录指针（新脚本）或旧脚本文件（跨代际升级）。两者都不存在时
/// 函数直接返回 `Ok(false)`，**不做任何写入**，可以无条件下放在 `main` / `setup` 开头。
///
/// ```no_run
/// # fn main() -> tauri_delta_updater::Result<()> {
/// let confirmed = tauri_delta_updater::PlatformUpdater::confirm_startup_success_if_pending()?;
/// # let _ = confirmed;
/// # Ok(())
/// # }
/// ```
pub fn confirm_startup_success_if_pending() -> Result<bool> {
    let exe = crate::platform::target::runtime_executable().map_err(DeltaError::Io)?;
    let Some(dir) = exe.parent() else {
        return Ok(false);
    };
    if !has_replacement_evidence(dir) {
        return Ok(false);
    }
    // 新进程已经执行到宿主代码：先把状态推进到 booting，再等待宿主的确认。
    // 健康检查失败时 `confirm_startup_success_with` 会把它改成 failed。
    if let Ok((work_dir, _)) = resolve_work_dir() {
        let _ = crate::core::startup::write(&work_dir, crate::core::startup::StartupState::Booting);
    }
    confirm_startup_success()?;
    Ok(true)
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
    let _ = crate::core::startup::write(work_dir, crate::core::startup::StartupState::Committed);
    // 新版本已经启动成功：上一次回滚的旧原因不再代表当前状态。
    let _ = std::fs::remove_file(work_dir.join(artifacts::ROLLBACK_FLAG));

    // Windows 整包：确认成功后备份已完成使命。脚本通常会在看到标志后自行删除，
    // 但若进程恰好在此之前崩溃，残留备份会被下一次启动误判为“替换未完成”
    // 并回滚健康版本；这里主动清理，与 macOS 的 cleanup_bundle_backup 对称。
    #[cfg(target_os = "windows")]
    {
        let _ = std::fs::remove_dir_all(work_dir.join(artifacts::BUNDLE_BACKUP_DIR));
        let _ = std::fs::remove_file(work_dir.join(artifacts::BUNDLE_JOURNAL_FILE));
    }

    // 全量安装包已完成使命（Windows 安装器退出 / macOS-Linux 已替换），
    // 这里删除，避免用户缓存目录长期占用几十到几百 MB。
    crate::core::disk::cleanup_full_installers_sync(work_dir);

    // 老版本脚本在目标文件（AppImage 下是 .AppImage 本体）旁边等旧标志名
    if let Ok(exe) = crate::platform::target::runtime_executable() {
        if let Some(dir) = exe.parent() {
            let _ = std::fs::write(dir.join(artifacts::LEGACY_SUCCESS_FLAG), "ok");
            // 旧脚本不自删；确认成功后清掉，避免后续普通启动把残留脚本误判成
            // "本次来自替换"。删除正在运行的脚本是安全的：解释器已加载内容，
            // 且脚本自身只等待成功/回滚标志，不会重新读取文件。
            for name in artifacts::LEGACY_SCRIPTS {
                let _ = std::fs::remove_file(dir.join(name));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::has_replacement_evidence;
    use crate::core::artifacts;
    use tempfile::tempdir;

    /// 占位符守卫必须报出漏替换，但不能误伤含 `__` 的合法路径。
    #[test]
    fn assert_rendered_detects_placeholders_without_false_positives() {
        assert!(super::assert_rendered("target='C:/my__app/app.exe'").is_ok());
        let err = super::assert_rendered("echo __TARGET__").unwrap_err();
        assert!(err.to_string().contains("__TARGET__"), "{err}");
    }

    /// 跨代际升级的证据必须被识别：旧脚本不留新指针。
    #[test]
    fn detects_new_marker_and_legacy_scripts() {
        let dir = tempdir().unwrap();
        assert!(!has_replacement_evidence(dir.path()), "空目录不是替换");

        std::fs::write(dir.path().join(artifacts::WORK_DIR_MARKER), "x").unwrap();
        assert!(has_replacement_evidence(dir.path()), "新指针是替换证据");

        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(artifacts::LEGACY_SCRIPTS[0]), "x").unwrap();
        assert!(
            has_replacement_evidence(dir.path()),
            "旧脚本文件必须被当作替换证据"
        );
    }
}
