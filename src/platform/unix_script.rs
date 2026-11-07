//! macOS / Linux 共用的替换脚本生成。
//!
//! 两平台的流程与判据完全一致（等待旧进程、备份、替换、启动确认、回滚），
//! 差异只有"是否需要提权"与"提权后如何降权启动"。
//! 早先两份脚本逐字复制，改一处漏一处，这里合并成一份模板。

use crate::core::artifacts;
use crate::core::diff::VerifiedArtifact;
use crate::error::{DeltaError, Result};
use crate::platform::shared::{file_name, shell_quote};
use crate::platform::ScriptTimings;
use std::path::{Path, PathBuf};

/// 生成 Unix 替换脚本。
///
/// `elevation` 为 `true` 时插入 sudo/pkexec 提权块（仅 Linux 需要）。
///
/// `check_exe_link` 为 `true` 时额外校验 `/proc/<pid>/exe` 是否指向目标文件；
/// AppImage 必须关掉：`/proc/<pid>/exe` 指向 squashfs 挂载点，
/// 与 `.AppImage` 本体不同，开着会立刻判定"旧进程已退出"并提前替换。
pub fn generate(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
    elevation: bool,
    check_exe_link: bool,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到父目录".into()))?;
    let target_exe = exe_dir.join(file_name(current_exe)?);
    let backup = exe_dir.join(format!("{}.bak", file_name(current_exe)?));

    let script_path = work_dir.join(artifacts::updater_script());
    let success_flag = work_dir.join(artifacts::SUCCESS_FLAG);
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let rollback_flag = work_dir.join(artifacts::ROLLBACK_FLAG);

    let _ = std::fs::remove_file(&success_flag);

    let elevation_block = if elevation {
        r#"
# 目标目录不可写时（例如通过 .deb 安装到 /usr/bin），提权重跑。
# 优先 sudo -n（不提示密码，避免无终端环境卡死），
# 失败后退到 pkexec（桌面环境会弹图形授权框）。
if [ ! -w "$TARGET_DIR" ]; then
    if command -v sudo > /dev/null 2>&1 && sudo -n true 2>/dev/null; then
        exec sudo -n "$0"
    fi
    # pkexec 在无图形环境（SSH/无 DISPLAY）会阻塞在 polkit agent 上，
    # 只在有图形会话时尝试，并用 timeout 防止永久挂起。
    if command -v pkexec > /dev/null 2>&1 && command -v timeout > /dev/null 2>&1 \
        && { [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; }; then
        timeout 30 pkexec "$0"
        code=$?
        if [ "$code" -eq 0 ]; then
            exit 0
        fi
        # 按退出码区分失败原因，便于排查"用户拒绝"与"环境问题"：
        # 124 = timeout 超时；126 = 授权被用户取消；其它 = 环境问题
        if [ "$code" -eq 124 ]; then
            log 'pkexec 提权超时，更新中止。'
        elif [ "$code" -eq 126 ]; then
            log 'pkexec 授权被用户取消，更新中止。'
        else
            log 'pkexec 提权失败，更新中止。'
        fi
        exit 1
    fi
    log '目标目录不可写且无法提权，更新中止。'
    # 此时还没动任何文件，旧进程可能仍在运行，无需重启
    exit 1
fi
"#
    } else {
        ""
    };

    let demote_block = if elevation {
        r#"
# 提权后需要把应用降权回原用户：root 启动的桌面应用会继承 root 权限，
# 下次手动启动又恢复普通权限，行为不一致且扩大攻击面。
ORIG_USER=''
if [ "$(id -u)" -eq 0 ]; then
    if [ -n "${SUDO_USER:-}" ]; then
        ORIG_USER="$SUDO_USER"
    fi
    if [ -z "$ORIG_USER" ] && [ -n "${PKEXEC_UID:-}" ]; then
        ORIG_USER=$(id -nu "$PKEXEC_UID" 2>/dev/null)
    fi
fi
# 提权实例写日志会把文件变成 root 所有，下次普通用户执行就追加失败
if [ "$(id -u)" -eq 0 ] && [ -n "$ORIG_USER" ]; then
    touch "$LOG_FILE" 2>/dev/null
    chown "$ORIG_USER" "$LOG_FILE" 2>/dev/null
fi

launch_app() {
    if [ "$(id -u)" -eq 0 ] && [ -n "$ORIG_USER" ]; then
        if command -v runuser > /dev/null 2>&1; then
            runuser -u "$ORIG_USER" -- "$TARGET_EXE" > /dev/null 2>&1 &
        elif command -v su > /dev/null 2>&1; then
            # 精简镜像可能没有 runuser；su 是 POSIX 基础工具里的兜底
            su -s /bin/sh -c "$TARGET_CMD" "$ORIG_USER" > /dev/null 2>&1 &
        else
            log '缺少 runuser/su，将以当前身份启动应用。'
            "$TARGET_EXE" > /dev/null 2>&1 &
        fi
    else
        "$TARGET_EXE" > /dev/null 2>&1 &
    fi
    APP_PID=$!
}
"#
    } else {
        r#"
launch_app() {
    "$TARGET_EXE" > /dev/null 2>&1 &
    APP_PID=$!
}
"#
    };

    let template = include_str!("unix/updater.sh");

    let target_cmd = format!("exec {}", shell_quote(&target_exe.display().to_string()));

    let script = template
        .replace("__LOG__", &shell_quote(&log_file.display().to_string()))
        .replace(
            "__FLAG__",
            &shell_quote(&success_flag.display().to_string()),
        )
        .replace(
            "__TARGET__",
            &shell_quote(&target_exe.display().to_string()),
        )
        .replace("__TARGET_CMD__", &shell_quote(&target_cmd))
        .replace(
            "__TARGET_DIR__",
            &shell_quote(&exe_dir.display().to_string()),
        )
        .replace(
            "__STAGED__",
            &shell_quote(&new_exe.path().display().to_string()),
        )
        .replace("__STAGED_HASH__", &shell_quote(new_exe.sha256()))
        .replace("__BACKUP__", &shell_quote(&backup.display().to_string()))
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &timings.wait_secs.to_string())
        .replace("__CONFIRM_TIMEOUT__", &timings.confirm_secs.to_string())
        .replace(
            "__WORK_DIR__",
            &shell_quote(&work_dir.display().to_string()),
        )
        .replace(
            "__MARKER__",
            &shell_quote(
                &exe_dir
                    .join(artifacts::WORK_DIR_MARKER)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__CHECK_EXE_LINK__", if check_exe_link { "1" } else { "0" })
        .replace(
            "__ROLLBACK__",
            &shell_quote(&rollback_flag.display().to_string()),
        )
        .replace(
            "__EVENTS__",
            &shell_quote(&work_dir.join(artifacts::EVENTS_FILE).display().to_string()),
        )
        .replace(
            "__STATE__",
            &shell_quote(
                &work_dir
                    .join(artifacts::UPDATE_STATE_FILE)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__DEMOTE__", demote_block)
        .replace("__ELEVATION__", elevation_block);
    crate::platform::shared::assert_rendered(&script);

    std::fs::write(&script_path, script.as_bytes()).map_err(DeltaError::Io)?;
    Ok(script_path)
}

/// 启动替换脚本。
#[cfg(unix)]
pub fn launch_updater(script_path: &Path) -> Result<()> {
    if !script_path.is_file() {
        return Err(DeltaError::PatchApplyFailed(format!(
            "替换脚本不存在: {}",
            script_path.display()
        )));
    }
    let _ = std::process::Command::new("chmod")
        .arg("+x")
        .arg(script_path)
        .status();
    std::process::Command::new("/bin/sh")
        .arg(script_path)
        .spawn()
        .map_err(DeltaError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff::{apply_patch, create_patch};

    /// 渲染是纯字符串逻辑：Windows CI 也能守住 Unix 模板。
    #[test]
    fn rendered_script_replaces_all_placeholders_and_has_rollback_paths() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("app");
        std::fs::write(&old, b"old").unwrap();
        let new_src = dir.path().join("app.new");
        std::fs::write(&new_src, b"new version").unwrap();
        let patch = dir.path().join("update.patch");
        let staged_path = dir.path().join("staged");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let staged = rt.block_on(async {
            create_patch(&old, &new_src, &patch).await.unwrap();
            let patch_hash = crate::core::hasher::calculate_hash(&patch).await.unwrap();
            let target_hash = crate::core::hasher::calculate_hash(&new_src).await.unwrap();
            apply_patch(&old, &patch, &staged_path, &patch_hash, &target_hash)
                .await
                .unwrap()
        });

        for elevation in [false, true] {
            let script = generate(
                &old,
                &staged,
                dir.path(),
                999_999,
                elevation,
                true,
                ScriptTimings::default(),
            )
            .unwrap();
            let content = std::fs::read_to_string(&script).unwrap();

            assert!(content.contains("write_rollback 'update_failed'"));
            assert!(content.contains("REASON='startup_timeout'"));
            assert!(content.contains("health_check_failed"));
            assert!(content.contains("startup_not_started"));
            assert!(content.contains("startup_crashed"));
            assert!(content.contains("STATE_FILE="));
            assert!(content.contains("tdx_events.jsonl"));
            assert!(content.contains("tdx_rollback.txt"));
            assert!(!content.contains("__"), "渲染后不得残留占位符");
            if elevation {
                assert!(content.contains("sudo -n"), "提权模式必须插入提权块");
            } else {
                assert!(!content.contains("pkexec"), "非提权模式不应插入提权块");
            }
        }
    }
}
