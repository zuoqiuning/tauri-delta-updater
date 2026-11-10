#!/bin/sh
# 由 tauri-delta-updater 生成，请勿手工修改。
# macOS 整包替换：以 .app 目录为单位备份与切换，保留新包的签名与公证信息。

set -u

LOG_FILE=__LOG__
ROLLBACK_FILE=__ROLLBACK__
EVENTS_FILE=__EVENTS__
SUCCESS_FLAG=__FLAG__
STATE_FILE=__STATE__
IN_PROGRESS=__IN_PROGRESS__
NOTICE_ENABLED=__NOTICE_ENABLED__
APP_ROOT=__APP_ROOT__
APP_DIR=__APP_DIR__
STAGING=__STAGING__
BACKUP=__BACKUP__
TARGET_EXE=__TARGET_EXE__
MARKER=__MARKER__
OLD_PID=__OLD_PID__
WAIT_TIMEOUT=__WAIT_TIMEOUT__
CONFIRM_TIMEOUT=__CONFIRM_TIMEOUT__
WORK_DIR=__WORK_DIR__
APP_PID=''

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

# 回滚原因落盘：宿主进程已经退出，下一次启动的 diagnose() 读取它。
write_rollback() {
    log "event=delta.rollback.triggered reason=$1"
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

file_sha() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo ''
    fi
}

launch_app() {
    "$TARGET_EXE" > /dev/null 2>&1 &
    APP_PID=$!
}

# 所有失败路径的统一收口：还原备份，再把旧版本拉起来。
restart_old() {
    # 回滚完成：上一次替换已经结束（以失败告终），“进行中”标记不能留到下次启动。
    rm -f "$IN_PROGRESS" 2>/dev/null
    # 已经有明确原因（超时/健康检查失败）时不要覆盖成笼统的 update_failed。
    EXISTING=''
    [ -f "$ROLLBACK_FILE" ] && EXISTING=$(cat "$ROLLBACK_FILE" 2>/dev/null)
    if [ -z "$EXISTING" ]; then
        write_rollback 'update_failed'
    else
        write_rollback_preserved "$EXISTING"
    fi
    if [ ! -d "$APP_ROOT" ] && [ -d "$BACKUP" ]; then
        mv "$BACKUP" "$APP_ROOT" 2>/dev/null
    fi
    if [ -x "$TARGET_EXE" ]; then
        log '更新中止，重新启动旧版本。'
        launch_app
    fi
    show_rollback_notice
}

verify_staging() {
    # shell 的返回码：0 = 通过，非 0 = 失败。用 _verify_failed 而不是 _ok，
    # 避免把"1 = 正常"这套布尔语义直接 return 出去（那会反过来）。
    _verify_failed=0
__VERIFY__
    return $_verify_failed
}

# 应用目录必须可写：系统级目录对普通用户只读时不做任何文件操作，
# 旧版本继续运行——这比"删掉一半再回滚"安全得多。
if [ ! -w "$APP_DIR" ]; then
    log '应用目录不可写，整包更新中止（请将应用移到可写目录或使用安装器）。'
    exit 1
fi

log '开始整包更新流程...'

# 崩溃恢复：只要备份目录还在，上一轮切换就没有完成——
# 新版本启动成功时会调用 confirm_startup_success 顺手删除备份。
# 因此这里不判断 APP_ROOT 是否存在：复制中断留下的半成品也要丢掉。
if [ -d "$BACKUP" ]; then
    log '检测到上次中断遗留的备份，正在恢复...'
    rm -rf "$APP_ROOT"
    mv "$BACKUP" "$APP_ROOT" 2>/dev/null
fi

# 等待旧进程退出。目录切换要求应用完全退出，否则运行中的进程会持有旧文件。
WAITED=0
while [ "$WAITED" -lt "$WAIT_TIMEOUT" ]; do
    if ! kill -0 "$OLD_PID" 2>/dev/null; then
        break
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done

if kill -0 "$OLD_PID" 2>/dev/null; then
    log '等待旧进程退出超时，更新中止。'
    exit 1
fi

if ! verify_staging; then
    log '暂存目录校验失败，更新中止。'
    restart_old
    exit 1
fi

# 已签名的包必须能在切换前通过 codesign 校验。重建如果丢了任何一个
# 被签名的文件或符号链接，这里就会失败——比切换后 Gatekeeper 拒绝启动安全。
# 未签名的开发构建没有 CodeResources，跳过。
if [ -f "$STAGING/Contents/_CodeSignature/CodeResources" ] \
    && command -v codesign > /dev/null 2>&1; then
    if ! codesign --verify --deep --strict "$STAGING" > /dev/null 2>&1; then
        log '重建包的签名校验失败，更新中止。'
        restart_old
        exit 1
    fi
    log '重建包签名校验通过。'
fi

# 目录改名在同一卷上是原子操作；跨卷时 mv 退化为复制后删除，
# 但备份仍然完整，失败路径不受影响。
rm -rf "$BACKUP"
if ! mv "$APP_ROOT" "$BACKUP"; then
    log '备份旧版本失败，更新中止。'
    restart_old
    exit 1
fi

if ! mv "$STAGING" "$APP_ROOT"; then
    log '切换到新版本失败，正在回滚。'
    rm -rf "$APP_ROOT"
    mv "$BACKUP" "$APP_ROOT" 2>/dev/null
    restart_old
    exit 1
fi

# 归档已恢复权限位；这里再兜底一次，确保主程序可执行。
chmod +x "$TARGET_EXE" 2>/dev/null

log '启动新版本，等待启动确认...'
# 告诉新版本去哪个目录写启动成功标志（自定义 work_dir 时尤其重要）
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
        rm -rf "$BACKUP"
        rm -f "$IN_PROGRESS" 2>/dev/null
        # 脚本不删除自身：执行期间删除正在被读取的文件有竞态。
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
write_rollback "$REASON"
if [ -n "${APP_PID:-}" ]; then
    if command -v pkill > /dev/null 2>&1; then
        pkill -TERM -P "$APP_PID" 2>/dev/null
    fi
    kill -TERM "$APP_PID" 2>/dev/null
fi
sleep 1
rm -rf "$APP_ROOT"
mv "$BACKUP" "$APP_ROOT" 2>/dev/null
launch_app
