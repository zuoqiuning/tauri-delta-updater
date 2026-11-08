//! 前端类型与调用封装：与 Rust 侧的 serde 契约一一对应。
//!
//! 不引入任何运行期状态：所有函数都直接转发到插件命令，
//! `Channel` 的生命周期由调用方持有（事件回调只在本次调用期间有效）。

import { Channel, invoke } from '@tauri-apps/api/core';

/** 增量不可用的原因（与 `FallbackReason` 一致）。 */
export type FallbackReason =
  | { no_matching_patch: { from: string; to: string } }
  | { environment_unsupported: string };

/** 全量安装器类型（与 `InstallerKind` 一致）。 */
export type InstallerKind =
  | 'nsis'
  | 'msi'
  | 'mac_app_tar_gz'
  | 'app_image'
  | 'inno'
  | 'other';

/** 更新阶段事件（与 `UpdateEvent` 一致，按 `stage` 区分）。 */
export type UpdateEvent =
  | { stage: 'checking' }
  | { stage: 'downloading'; downloaded: number; total: number }
  | { stage: 'downloading_full'; downloaded: number; total: number }
  | { stage: 'applying' }
  | { stage: 'ready'; version: string }
  | { stage: 'up_to_date'; version: string }
  | { stage: 'deferred'; version: string; percentage: number }
  | { stage: 'fallback'; url: string | null; reason: FallbackReason }
  | { stage: 'repairing'; reason: string }
  | { stage: 'safe_mode' }
  | { stage: 'cancelled' };

/** 更新结果（与 `UpdateResult` 一致，按 `kind` 区分）。 */
export type UpdateResult =
  | { kind: 'updated'; version: string }
  /** `prepare` 专用：产物已就绪但替换脚本尚未启动，调用 `launch()` 后再退出。 */
  | { kind: 'ready'; version: string }
  | { kind: 'repair_required'; reason: string }
  | { kind: 'up_to_date'; version: string }
  | { kind: 'deferred'; version: string; percentage: number }
  | { kind: 'requires_full_download'; url: string | null; reason: FallbackReason }
  /** The running OS is below `platforms[].min_os`; nothing was downloaded. */
  | { kind: 'unsupported_os'; required: string; current: string }
  | { kind: 'safe_mode' };

/** 命令失败时插件返回的结构（`code` 是稳定契约，`message` 仅用于展示）。 */
export interface ErrorDto {
  code: string;
  message: string;
  retryable: boolean;
  security: boolean;
}

/** 全量下载结果（`downloadFull` 返回值）。 */
export type FullDownloadDto =
  | { kind: 'downloaded'; version: string; path: string }
  | { kind: 'up_to_date'; version: string }
  | { kind: 'deferred'; version: string; percentage: number }
  | { kind: 'unsupported_os'; required: string; current: string };

/** 跨重启待安装记录（`pending()` 返回值）；路径留在 Rust 侧。 */
export interface PendingUpdate {
  version: string;
  kind: 'exe' | 'bundle';
  /** 记录创建时间（Unix 秒）。 */
  created_at: number;
}

/** 单个增量补丁的描述。 */
export interface PatchInfo {
  from_version: string;
  to_version: string;
  patch_url: string;
  patch_hash: string;
  target_hash: string;
  patch_size?: number;
  scope?: 'main_executable_only' | 'full_app';
}

/** 清单的平台条目（兼容官方 updater 格式）。 */
export interface PlatformInfo {
  signature: string;
  url: string;
  size?: number;
  /** 该平台要求的最低系统版本（如 `10.0.19045`）；旧清单没有该字段。 */
  min_os?: string;
  rollout?: { percentage: number };
}

/** 已验签的清单（只暴露 UI 常用的字段）。 */
export interface UpdateManifest {
  version: string;
  notes: string;
  /** Localized notes (locale → text); `notes` is the fallback. */
  notes_i18n?: Record<string, string>;
  pub_date: string;
  min_version?: string;
  platforms: Record<string, PlatformInfo>;
  patches: Record<string, PatchInfo[]>;
  /** Unknown top-level fields preserved from the signed manifest. */
  [key: string]: unknown;
}

/** `check()` 的结果。 */
export interface CheckOutcome {
  version: string;
  notes: string;
  pub_date: string;
  is_newer: boolean;
  has_patch: boolean;
  is_delta: boolean;
  rollout_percentage: number | null;
  deferred_by_rollout: boolean;
  download_size: number | null;
  patch_size: number | null;
  needs_repair: boolean;
  full_download_url: string | null;
  /** 本机系统版本低于清单要求时的结构化原因；否则为 `null`。 */
  unsupported_os: { required: string; current: string } | null;
  manifest: UpdateManifest;
  fallback_reason: FallbackReason | null;
}

/** 替换脚本写出的结构化事件（JSONL 的一行）。 */
export interface ScriptEvent {
  event: string;
  reason: string | null;
  time: string | null;
}

/** 启动状态指针。 */
export type StartupState =
  | 'staged'
  | 'booting'
  | 'committed'
  | 'failed'
  | 'rolled_back';

/** `diagnose()` 的只读快照（不发起网络请求）。 */
export interface Diagnosis {
  executable: string;
  work_dir: string;
  platform: string;
  work_dir_writable: boolean;
  install_dir_writable: boolean;
  fallback_reason: string | null;
  bundle_capable: boolean;
  safe_mode: boolean;
  needs_repair: boolean;
  startup_state: StartupState | null;
  last_rollback: string | null;
  /** 历史回滚链（`tdx_events.jsonl` 中的 `delta.rollback.triggered`，最多 20 条）。 */
  rollback_history: ScriptEvent[];
  authenticode: boolean | null;
  authenticode_check_skipped: boolean | null;
  log_path: string;
  events_path: string;
  script_events: ScriptEvent[];
  privilege_tools_available: boolean | null;
  /** 端点使用明文 HTTP 且非回环（配置降级，建议在 UI/日志中暴露）。 */
  insecure_transport: boolean;
  /** 显式声明的全量安装器类型；`null` 表示按文件后缀推断。 */
  installer_kind: InstallerKind | null;
}

/** 判断 `invoke` 抛出的值是否是插件错误 DTO。 */
export function isUpdaterError(value: unknown): value is ErrorDto {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as ErrorDto).code === 'string' &&
    typeof (value as ErrorDto).message === 'string'
  );
}

/**
 * 替换/修复脚本是否已经启动，调用方**必须让当前进程退出**。
 *
 * 与 Rust 的 `UpdateResult::requires_exit()` 一致：`updated` 与
 * `repair_required` 为 `true`；`prepare` 返回的 `ready` 为 `false`。
 */
export function requiresExit(result: UpdateResult): boolean {
  return result.kind === 'updated' || result.kind === 'repair_required';
}

/** 构造事件通道；命令始终要求 `onEvent` 参数，即使调用方不关心进度。 */
function channel(onEvent?: (event: UpdateEvent) => void): Channel<UpdateEvent> {
  const events = new Channel<UpdateEvent>();
  if (onEvent) {
    events.onmessage = onEvent;
  }
  return events;
}

/** 只检查是否有新版本，不下载任何内容。 */
export function check(): Promise<CheckOutcome> {
  return invoke('plugin:delta-updater|check');
}

/** 下载 → 合并 → 启动替换，插件会在产物就绪后退出进程。 */
export function install(
  onEvent?: (event: UpdateEvent) => void,
): Promise<UpdateResult> {
  return invoke('plugin:delta-updater|install', { onEvent: channel(onEvent) });
}

/**
 * 下载并合并，但**不启动替换**。
 *
 * 返回 `{ kind: 'ready' }` 时由调用方决定时机，再调用 {@link launch}。
 */
export function prepare(
  onEvent?: (event: UpdateEvent) => void,
): Promise<UpdateResult> {
  return invoke('plugin:delta-updater|prepare', { onEvent: channel(onEvent) });
}

/** 启动 `prepare` 就绪的替换脚本并退出进程；没有待启动产物时抛 `nothing_to_launch`。 */
export function launch(): Promise<UpdateResult> {
  return invoke('plugin:delta-updater|launch');
}

/** 执行完整更新（增量不可用时自动回退安装器），成功后插件退出进程。 */
export function installFull(
  onEvent?: (event: UpdateEvent) => void,
): Promise<UpdateResult> {
  return invoke('plugin:delta-updater|install_full', {
    onEvent: channel(onEvent),
  });
}

/** 只下载全量安装包并验签，不执行安装。 */
export function downloadFull(
  onEvent?: (event: UpdateEvent) => void,
): Promise<FullDownloadDto> {
  return invoke('plugin:delta-updater|download_full', {
    onEvent: channel(onEvent),
  });
}

/** 环境诊断：可写性、平台适用性、安全模式、回滚原因与日志路径。 */
export function diagnose(): Promise<Diagnosis> {
  return invoke('plugin:delta-updater|diagnose');
}

/** 取消正在进行的更新；返回 `false` 表示当前没有可取消的会话。 */
export function cancel(): Promise<boolean> {
  return invoke('plugin:delta-updater|cancel');
}

/**
 * 查询 `prepare` 落盘、尚未安装的记录（不存在时为 `null`）。
 *
 * 记录已通过版本单调性、路径限定与产物哈希校验；不发起网络请求。
 */
export function pending(): Promise<PendingUpdate | null> {
  return invoke('plugin:delta-updater|pending');
}

/**
 * Discard the persisted pending record; `true` if one existed.
 *
 * Call this after `check()` shows a newer version than `pending()`:
 * otherwise the app installs the older staged version first and updates again.
 */
export function discardPending(): Promise<boolean> {
  return invoke('plugin:delta-updater|discard_pending');
}

/**
 * 执行跨重启待安装记录；`true` 表示已启动替换，插件随后退出进程。
 *
 * 通常在启动时调用，把"重启后继续安装"变成一次显式动作。
 */
export function installPending(): Promise<boolean> {
  return invoke('plugin:delta-updater|install_pending');
}

/** 手动退出安全模式（UI 提供"重试"按钮时使用）。 */
export function resetSafety(): Promise<void> {
  return invoke('plugin:delta-updater|reset_safety');
}
