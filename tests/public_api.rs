//! 公共 API 快照：重构（目录/模块拆分）时这些路径必须继续可用。
//!
//! 本文件只做编译期校验，不包含运行时断言。新增公共符号时同步加入这里，
//! 删除或改名会让重构在 CI 的编译阶段直接失败，而不是等到用户升级时才发现。

#![allow(unused_imports)]

use tauri_delta_updater::{
    apply_bundle_patch, apply_patch, build_archive, calculate_hash, check_disk_space,
    cleanup_previous_updates, client_builder, create_bundle_patch, create_patch,
    create_patch_with_level, current_platform, default_work_dir, describe_dir, detect_arch,
    ensure_writable, extract_archive, get_failed_attempts, incremental_fallback_reason,
    install_crypto_provider, is_safe_mode_active, is_writable, list_archive,
    manifest_version_binding, normalize_version, record_failed_attempt, reset_safety,
    safe_mode_remaining, should_abort, simulate_update, verify_file_signature,
    verify_file_signature_multi, verify_layout, verify_manifest_signature,
    verify_manifest_signature_file, verify_manifest_signature_multi,
    verify_manifest_signature_with, BundleFile, BundleManifest, BundlePatch, CancelToken,
    CheckOutcome, DeltaError, DeltaUpdater, Diagnosis, FallbackPolicy, FallbackReason,
    FullDownload, FullDownloadOutcome, InstallMode, InstallerKind, PatchInfo, PatchScope,
    PendingArtifact, PendingUpdate, PlatformInfo, PlatformUpdater, Result, Rollout, ScriptEvent,
    ScriptTimings, SimulationReport, Staged, StagedBundle, StagedFullInstaller, StagedRepair,
    StartupState, TrustPolicy, UpdateCheck, UpdateConfig, UpdateEvent, UpdateLock, UpdateManifest,
    UpdateOutcome, UpdateResult, UpdateSession, VerifiedArtifact, MAX_DELTA_CHAIN_HOPS,
    MAX_INCREMENTAL_FILE_BYTES,
};

/// 模块级路径同样属于契约（文档与示例直接引用这些路径）。
const _: () = {
    use tauri_delta_updater::core::bundle::BundleFileKind;
    use tauri_delta_updater::core::downloader::{prepare_resume, resume_fresh};
    use tauri_delta_updater::core::rollout::{bucket, load_or_create_install_id, selected};
    use tauri_delta_updater::host_version;
    use tauri_delta_updater::platform::target::current_platform as platform_key;
    use tauri_delta_updater::platform::ScriptTimings as PlatformTimings;
};

#[test]
fn public_api_snapshot_compiles() {
    // 真正的断言是“本文件能编译”。运行时不需要做任何事。
}
