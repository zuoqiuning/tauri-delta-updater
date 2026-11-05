//! macOS / Linux 共用的替换脚本生成。
//!
//! 两平台的流程与判据完全一致（等待旧进程、备份、替换、启动确认、回滚），
//! 差异只有"是否需要提权"与"提权后如何降权启动"。
//! 早先两份脚本逐字复制，改一处漏一处，这里合并成一份模板。

use crate::core::artifacts;
use crate::core::diff::VerifiedArtifact;
use crate::error::{DeltaError, Result};
use crate::platform::shared::{file_name, shell_quote};
use std::path::{Path, PathBuf};

/// 等待旧进程退出与等待新版本启动确认的最长秒数。
pub const WAIT_TIMEOUT_SECS: u32 = 60;
/// 等待新版本写入启动成功标志的最长秒数。
pub const CONFIRM_TIMEOUT_SECS: u32 = 60;

/// 生成 Unix 替换脚本。
///
/// `elevation` 为 `true` 时插入 sudo/pkexec 提权块（仅 Linux 需要）。
pub fn generate(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
    elevation: bool,
) -> Result<PathBuf> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到父目录".into()))?;
    let target_exe = exe_dir.join(file_name(current_exe)?);
    let backup = exe_dir.join(format!("{}.bak", file_name(current_exe)?));

    let script_path = work_dir.join(artifacts::updater_script());
    let success_flag = work_dir.join(artifacts::SUCCESS_FLAG);
    let log_file = work_dir.join(artifacts::LOG_FILE);

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

    let template = r#"#!/bin/sh
# 由 tauri-delta-updater 生成，请勿手工修改。
# 需要 UTF-8 无 BOM；所有路径按单引号字面量写入。

set -u

LOG_FILE=__LOG__
SUCCESS_FLAG=__FLAG__
TARGET_EXE=__TARGET__
TARGET_CMD=__TARGET_CMD__
TARGET_DIR=__TARGET_DIR__
STAGED=__STAGED__
STAGED_HASH=__STAGED_HASH__
BACKUP=__BACKUP__
OLD_PID=__OLD_PID__
WAIT_TIMEOUT=__WAIT_TIMEOUT__
CONFIRM_TIMEOUT=__CONFIRM_TIMEOUT__
WORK_DIR=__WORK_DIR__
MARKER=__MARKER__
APP_PID=''

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}
__DEMOTE__

# 一切失败路径的统一收口：还原残留备份，再把旧版本拉起来。
# 少了这一步，用户在"提权被拒/复制失败"后会看到应用直接消失。
restart_old() {
    log 'event=delta.rollback.triggered reason=update_failed'
    if [ ! -f "$TARGET_EXE" ] && [ -f "$BACKUP" ]; then
        mv "$BACKUP" "$TARGET_EXE" 2>/dev/null
    fi
    if [ -f "$TARGET_EXE" ]; then
        log '更新中止，重新启动旧版本。'
        launch_app
    fi
}

file_sha() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo ''
    fi
}
__ELEVATION__

log '开始更新流程...'

# 崩溃恢复：上一轮若在"exe → .bak"之后、替换完成之前被打断，
# 先还原备份，否则本轮更新会在残缺状态上继续。
if [ ! -f "$TARGET_EXE" ] && [ -f "$BACKUP" ]; then
    log '检测到上次中断遗留的备份，正在恢复...'
    mv "$BACKUP" "$TARGET_EXE" 2>/dev/null
fi

# 等待旧进程退出：按 PID，并在 Linux 上校验 /proc/<pid>/exe，
# 避免 PID 回收后误等，也避免按进程名误伤同名应用。
TARGET_REAL="$TARGET_EXE"
if command -v readlink > /dev/null 2>&1; then
    TARGET_REAL=$(readlink -f "$TARGET_EXE" 2>/dev/null || echo "$TARGET_EXE")
fi
WAITED=0
while [ "$WAITED" -lt "$WAIT_TIMEOUT" ]; do
    if ! kill -0 "$OLD_PID" 2>/dev/null; then
        break
    fi
    if [ -r "/proc/$OLD_PID/exe" ]; then
        link=$(readlink "/proc/$OLD_PID/exe" 2>/dev/null)
        if [ -n "$link" ] && [ "$link" != "$TARGET_REAL" ]; then
            break
        fi
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

if kill -0 "$OLD_PID" 2>/dev/null; then
    log '等待旧进程退出超时，更新中止。'
    exit 1
fi

# 复制前再校验一次暂存文件：apply_patch 返回到这里之间文件可能被替换
actual=$(file_sha "$STAGED")
if [ -n "$actual" ] && [ "$actual" != "$STAGED_HASH" ]; then
    log '暂存文件哈希校验失败，更新中止。'
    restart_old
    exit 1
fi

# 备份旧版本
rm -f "$BACKUP"
if ! mv "$TARGET_EXE" "$BACKUP"; then
    log '备份旧版本失败，更新中止。'
    restart_old
    exit 1
fi

# 替换为新版本
if ! cp "$STAGED" "$TARGET_EXE"; then
    log '替换失败，正在回滚。'
    restart_old
    exit 1
fi
chmod +x "$TARGET_EXE"

actual=$(file_sha "$TARGET_EXE")
if [ -n "$actual" ] && [ "$actual" != "$STAGED_HASH" ]; then
    log '替换后的文件哈希不符，正在回滚。'
    rm -f "$TARGET_EXE"
    restart_old
    exit 1
fi
rm -f "$STAGED"

# 提权替换会把目标文件变成 root 所有，交还给原用户
if [ "$(id -u)" -eq 0 ] && [ -n "${ORIG_USER:-}" ]; then
    chown "$ORIG_USER" "$TARGET_EXE" 2>/dev/null
fi

log '启动新版本，等待启动确认...'
# 告诉新版本应该去哪个目录写启动成功标志（自定义 work_dir 时尤其重要）
printf '%s' "$WORK_DIR" > "$MARKER" 2>/dev/null
launch_app

WAITED=0
while [ "$WAITED" -lt "$CONFIRM_TIMEOUT" ]; do
    if [ -f "$SUCCESS_FLAG" ]; then
        log '新版本启动成功，清理备份。'
        rm -f "$BACKUP"
        # 脚本不删除自身：执行期间删除正在被读取的脚本有竞态，
        # 残留脚本由下一次更新的清理逻辑回收。
        exit 0
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

log '新版本启动超时，回滚...'
log 'event=delta.rollback.triggered reason=startup_timeout'
# 先结束本次启动的进程（可能是 runuser 的父进程，需要连带子进程）
if [ -n "${APP_PID:-}" ]; then
    if command -v pkill > /dev/null 2>&1; then
        pkill -TERM -P "$APP_PID" 2>/dev/null
    fi
    kill -TERM "$APP_PID" 2>/dev/null
fi
# Linux 兜底：按可执行文件完整路径结束进程，不按进程名误杀同名应用
if [ -d /proc ]; then
    for proc in /proc/[0-9]*; do
        if [ "$(readlink "$proc/exe" 2>/dev/null)" = "$TARGET_REAL" ]; then
            kill -TERM "${proc#/proc/}" 2>/dev/null
        fi
    done
fi
sleep 1
rm -f "$TARGET_EXE"
mv "$BACKUP" "$TARGET_EXE" 2>/dev/null
launch_app
"#;

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
        .replace("__WAIT_TIMEOUT__", &WAIT_TIMEOUT_SECS.to_string())
        .replace("__CONFIRM_TIMEOUT__", &CONFIRM_TIMEOUT_SECS.to_string())
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
        .replace("__DEMOTE__", demote_block)
        .replace("__ELEVATION__", elevation_block);

    std::fs::write(&script_path, script.as_bytes()).map_err(DeltaError::Io)?;
    Ok(script_path)
}

/// 启动替换脚本。
pub fn launch_updater(script_path: &Path) -> Result<()> {
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
