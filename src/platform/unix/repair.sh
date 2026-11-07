#!/bin/sh
# 由 tauri-delta-updater 生成，请勿手工修改。
# 修复上次中断的 .app 目录切换：整包备份还原后再启动旧版本。

set -u

LOG_FILE=__LOG__
ROLLBACK_FILE=__ROLLBACK__
EVENTS_FILE=__EVENTS__
APP_ROOT=__APP_ROOT__
APP_DIR=__APP_DIR__
BACKUP=__BACKUP__
TARGET_EXE=__TARGET_EXE__
OLD_PID=__OLD_PID__
WAIT_TIMEOUT=__WAIT_TIMEOUT__

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

write_rollback() {
    log "event=delta.rollback.triggered reason=$1"
    printf '%s' "$1" > "$ROLLBACK_FILE" 2>/dev/null
    printf '{"event":"delta.rollback.triggered","reason":"%s","time":"%s"}\n' \
        "$1" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >> "$EVENTS_FILE" 2>/dev/null || true
}

launch_app() {
    "$TARGET_EXE" > /dev/null 2>&1 &
}

if [ ! -w "$APP_DIR" ]; then
    log '应用目录不可写，无法修复上次更新。'
    exit 1
fi

log '开始修复上次未完成的更新...'

WAITED=0
while [ "$WAITED" -lt "$WAIT_TIMEOUT" ]; do
    if ! kill -0 "$OLD_PID" 2>/dev/null; then
        break
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done
if kill -0 "$OLD_PID" 2>/dev/null; then
    log '等待旧进程退出超时，修复中止。'
    exit 1
fi

if [ ! -d "$BACKUP" ]; then
    log '没有需要修复的备份。'
    exit 0
fi

rm -rf "$APP_ROOT"
if ! mv "$BACKUP" "$APP_ROOT"; then
    log '还原备份失败，请手动恢复 .app。'
    exit 1
fi

write_rollback 'repaired_partial_update'
log '修复完成，正在重新启动旧版本。'
launch_app
