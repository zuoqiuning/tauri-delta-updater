#!/bin/sh
# 由 tauri-delta-updater 生成，请勿手工修改。
# 需要 UTF-8 无 BOM；所有路径按单引号字面量写入。

set -u

LOG_FILE=__LOG__
ROLLBACK_FILE=__ROLLBACK__
EVENTS_FILE=__EVENTS__
SUCCESS_FLAG=__FLAG__
STATE_FILE=__STATE__
NOTICE_ENABLED=__NOTICE_ENABLED__
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
CHECK_EXE_LINK=__CHECK_EXE_LINK__
APP_PID=''

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

# 回滚原因落盘 + 结构化事件：宿主进程已经退出，下一次启动的 diagnose() 读取。
# 先写临时文件再 mv：写到一半被杀死不会留下空原因。
write_rollback() {
    printf '%s' "$1" > "$ROLLBACK_FILE.tmp" 2>/dev/null \
        && mv -f "$ROLLBACK_FILE.tmp" "$ROLLBACK_FILE" 2>/dev/null
    printf '%s' 'rolled_back' > "$STATE_FILE.tmp" 2>/dev/null \
        && mv -f "$STATE_FILE.tmp" "$STATE_FILE" 2>/dev/null
    printf '{"event":"delta.rollback.triggered","reason":"%s","time":"%s"}\n' \
        "$1" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >> "$EVENTS_FILE" 2>/dev/null || true
}

# 记录“已有更具体原因、本次不覆盖”的事实，便于事后审计两次写入。
write_rollback_preserved() {
    printf '{"event":"delta.rollback.preserved","reason":"%s","time":"%s"}\n' \
        "$1" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >> "$EVENTS_FILE" 2>/dev/null || true
}

# 正向阶段事件：主进程退出后仍能判断"卡在哪一步"。
write_stage() {
    printf '{"event":"%s","time":"%s"}\n' "$1" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
        >> "$EVENTS_FILE" 2>/dev/null || true
}

# 回滚后给用户的最小提示：桌面通知，不阻塞脚本；没有通知工具时静默跳过。
show_rollback_notice() {
    [ "$NOTICE_ENABLED" = "1" ] || return 0
    NOTICE_TITLE='Update failed'
    NOTICE_TEXT='The update did not complete. The previous version has been restored.'
    LANG_HINT="${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}"
    if [ -z "$LANG_HINT" ] && command -v defaults > /dev/null 2>&1; then
        LANG_HINT=$(defaults read -g AppleLocale 2>/dev/null)
    fi
    case "$LANG_HINT" in
        zh*) NOTICE_TITLE='更新失败'; NOTICE_TEXT='更新未完成，已恢复到上一版本。' ;;
    esac
    if command -v notify-send > /dev/null 2>&1; then
        notify-send "$NOTICE_TITLE" "$NOTICE_TEXT" > /dev/null 2>&1 || true
    elif command -v osascript > /dev/null 2>&1; then
        osascript -e "display notification \"$NOTICE_TEXT\" with title \"$NOTICE_TITLE\"" > /dev/null 2>&1 || true
    fi
}
__DEMOTE__

# 一切失败路径的统一收口：还原残留备份，再把旧版本拉起来。
# 少了这一步，用户在"提权被拒/复制失败"后会看到应用直接消失。
restart_old() {
    # 已经有明确原因（超时/健康检查失败）时不要覆盖成笼统的 update_failed。
    EXISTING=''
    [ -f "$ROLLBACK_FILE" ] && EXISTING=$(cat "$ROLLBACK_FILE" 2>/dev/null)
    if [ -z "$EXISTING" ]; then
        log 'event=delta.rollback.triggered reason=update_failed'
        write_rollback 'update_failed'
    else
        write_rollback_preserved "$EXISTING"
    fi
    # 替换可能只写了一半（磁盘满、I/O 错误）：只要备份还在就必须无条件还原，
    # 不能因为目标文件“存在”就把半截内容当成旧版本启动。
    if [ -f "$BACKUP" ]; then
        rm -f "$TARGET_EXE"
        mv "$BACKUP" "$TARGET_EXE" 2>/dev/null
    fi
    if [ -f "$TARGET_EXE" ]; then
        log '更新中止，重新启动旧版本。'
        launch_app
    fi
    show_rollback_notice
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
    if [ "$CHECK_EXE_LINK" = "1" ] && [ -r "/proc/$OLD_PID/exe" ]; then
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
# 清掉上一轮的回滚原因；健康检查失败时宿主会重新写入，脚本立即回滚。
rm -f "$ROLLBACK_FILE"
printf '%s' 'staged' > "$STATE_FILE" 2>/dev/null
write_stage 'delta.stage.replace_started'
launch_app

WAITED=0
while [ "$WAITED" -lt "$CONFIRM_TIMEOUT" ]; do
    if [ -f "$SUCCESS_FLAG" ]; then
        log '新版本启动成功，清理备份。'
        write_stage 'delta.stage.startup_confirmed'
        rm -f "$BACKUP"
        # 脚本不删除自身：执行期间删除正在被读取的脚本有竞态，
        # 残留脚本由下一次更新的清理逻辑回收。
        exit 0
    fi
    if [ -f "$ROLLBACK_FILE" ]; then
        break
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

REASON='startup_timeout'
if [ -f "$ROLLBACK_FILE" ]; then
    REASON=$(cat "$ROLLBACK_FILE" 2>/dev/null)
    [ -n "$REASON" ] || REASON='health_check_failed'
else
    STATE=$(cat "$STATE_FILE" 2>/dev/null)
    if [ "$STATE" = 'staged' ]; then
        REASON='startup_not_started'
    elif [ "$STATE" = 'booting' ] && [ -n "${APP_PID:-}" ] && ! kill -0 "$APP_PID" 2>/dev/null; then
        # 已经执行到宿主代码但进程已退出：崩溃/被杀，而不是初始化慢。
        REASON='startup_crashed'
    fi
fi
log "新版本未完成启动确认（$REASON），回滚..."
log "event=delta.rollback.triggered reason=$REASON"
write_rollback "$REASON"
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
