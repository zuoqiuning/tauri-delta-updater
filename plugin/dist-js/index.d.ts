/** 增量不可用的原因（与 `FallbackReason` 一致）。 */
export type FallbackReason = {
    no_matching_patch: {
        from: string;
        to: string;
    };
} | {
    environment_unsupported: string;
};
/** 更新阶段事件（与 `UpdateEvent` 一致，按 `stage` 区分）。 */
export type UpdateEvent = {
    stage: 'checking';
} | {
    stage: 'downloading';
    downloaded: number;
    total: number;
} | {
    stage: 'downloading_full';
    downloaded: number;
    total: number;
} | {
    stage: 'applying';
} | {
    stage: 'ready';
    version: string;
} | {
    stage: 'up_to_date';
    version: string;
} | {
    stage: 'deferred';
    version: string;
    percentage: number;
} | {
    stage: 'fallback';
    url: string | null;
    reason: FallbackReason;
} | {
    stage: 'repairing';
    reason: string;
} | {
    stage: 'safe_mode';
} | {
    stage: 'cancelled';
};
/** 更新结果（与 `UpdateResult` 一致，按 `kind` 区分）。 */
export type UpdateResult = {
    kind: 'updated';
    version: string;
}
/** `prepare` 专用：产物已就绪但替换脚本尚未启动，调用 `launch()` 后再退出。 */
 | {
    kind: 'ready';
    version: string;
} | {
    kind: 'repair_required';
    reason: string;
} | {
    kind: 'up_to_date';
    version: string;
} | {
    kind: 'deferred';
    version: string;
    percentage: number;
} | {
    kind: 'requires_full_download';
    url: string | null;
    reason: FallbackReason;
} | {
    kind: 'safe_mode';
};
/** 命令失败时插件返回的结构（`code` 是稳定契约，`message` 仅用于展示）。 */
export interface ErrorDto {
    code: string;
    message: string;
    retryable: boolean;
    security: boolean;
}
/** 全量下载结果（`downloadFull` 返回值）。 */
export type FullDownloadDto = {
    kind: 'downloaded';
    version: string;
    path: string;
} | {
    kind: 'up_to_date';
    version: string;
};
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
    rollout?: {
        percentage: number;
    };
}
/** 已验签的清单（只暴露 UI 常用的字段）。 */
export interface UpdateManifest {
    version: string;
    notes: string;
    pub_date: string;
    min_version?: string;
    platforms: Record<string, PlatformInfo>;
    patches: Record<string, PatchInfo[]>;
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
export type StartupState = 'staged' | 'booting' | 'committed' | 'failed' | 'rolled_back';
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
    authenticode: boolean | null;
    authenticode_check_skipped: boolean | null;
    log_path: string;
    events_path: string;
    script_events: ScriptEvent[];
    privilege_tools_available: boolean | null;
}
/** 判断 `invoke` 抛出的值是否是插件错误 DTO。 */
export declare function isUpdaterError(value: unknown): value is ErrorDto;
/** 只检查是否有新版本，不下载任何内容。 */
export declare function check(): Promise<CheckOutcome>;
/** 下载 → 合并 → 启动替换，插件会在产物就绪后退出进程。 */
export declare function install(onEvent?: (event: UpdateEvent) => void): Promise<UpdateResult>;
/**
 * 下载并合并，但**不启动替换**。
 *
 * 返回 `{ kind: 'ready' }` 时由调用方决定时机，再调用 {@link launch}。
 */
export declare function prepare(onEvent?: (event: UpdateEvent) => void): Promise<UpdateResult>;
/** 启动 `prepare` 就绪的替换脚本并退出进程；没有待启动产物时抛 `nothing_to_launch`。 */
export declare function launch(): Promise<UpdateResult>;
/** 执行完整更新（增量不可用时自动回退安装器），成功后插件退出进程。 */
export declare function installFull(onEvent?: (event: UpdateEvent) => void): Promise<UpdateResult>;
/** 只下载全量安装包并验签，不执行安装。 */
export declare function downloadFull(onEvent?: (event: UpdateEvent) => void): Promise<FullDownloadDto>;
/** 环境诊断：可写性、平台适用性、安全模式、回滚原因与日志路径。 */
export declare function diagnose(): Promise<Diagnosis>;
/** 取消正在进行的更新；返回 `false` 表示当前没有可取消的会话。 */
export declare function cancel(): Promise<boolean>;
/** 手动退出安全模式（UI 提供"重试"按钮时使用）。 */
export declare function resetSafety(): Promise<void>;
