//! 前端类型与调用封装：与 Rust 侧的 serde 契约一一对应。
//!
//! 不引入任何运行期状态：所有函数都直接转发到插件命令，
//! `Channel` 的生命周期由调用方持有（事件回调只在本次调用期间有效）。
import { Channel, invoke } from '@tauri-apps/api/core';
/** 判断 `invoke` 抛出的值是否是插件错误 DTO。 */
export function isUpdaterError(value) {
    return (typeof value === 'object' &&
        value !== null &&
        typeof value.code === 'string' &&
        typeof value.message === 'string');
}
/** 构造事件通道；命令始终要求 `onEvent` 参数，即使调用方不关心进度。 */
function channel(onEvent) {
    const events = new Channel();
    if (onEvent) {
        events.onmessage = onEvent;
    }
    return events;
}
/** 只检查是否有新版本，不下载任何内容。 */
export function check() {
    return invoke('plugin:delta-updater|check');
}
/** 下载 → 合并 → 启动替换，插件会在产物就绪后退出进程。 */
export function install(onEvent) {
    return invoke('plugin:delta-updater|install', { onEvent: channel(onEvent) });
}
/**
 * 下载并合并，但**不启动替换**。
 *
 * 返回 `{ kind: 'ready' }` 时由调用方决定时机，再调用 {@link launch}。
 */
export function prepare(onEvent) {
    return invoke('plugin:delta-updater|prepare', { onEvent: channel(onEvent) });
}
/** 启动 `prepare` 就绪的替换脚本并退出进程；没有待启动产物时抛 `nothing_to_launch`。 */
export function launch() {
    return invoke('plugin:delta-updater|launch');
}
/** 执行完整更新（增量不可用时自动回退安装器），成功后插件退出进程。 */
export function installFull(onEvent) {
    return invoke('plugin:delta-updater|install_full', {
        onEvent: channel(onEvent),
    });
}
/** 只下载全量安装包并验签，不执行安装。 */
export function downloadFull(onEvent) {
    return invoke('plugin:delta-updater|download_full', {
        onEvent: channel(onEvent),
    });
}
/** 环境诊断：可写性、平台适用性、安全模式、回滚原因与日志路径。 */
export function diagnose() {
    return invoke('plugin:delta-updater|diagnose');
}
/** 取消正在进行的更新；返回 `false` 表示当前没有可取消的会话。 */
export function cancel() {
    return invoke('plugin:delta-updater|cancel');
}
/** 手动退出安全模式（UI 提供"重试"按钮时使用）。 */
export function resetSafety() {
    return invoke('plugin:delta-updater|reset_safety');
}
