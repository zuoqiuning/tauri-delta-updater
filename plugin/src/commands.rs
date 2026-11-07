//! Tauri 命令：检查、更新、全量回退、诊断、取消与安全模式重置。

use std::time::Duration;

use tauri::ipc::Channel;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Runtime, State};
use tauri_delta_updater::{
    CancelToken, CheckOutcome, Diagnosis, UpdateConfig, UpdateEvent, UpdateOutcome, UpdateResult,
};

use crate::config::read_file_config;
use crate::dto::{ErrorDto, FullDownloadDto, PendingUpdateDto};
use crate::{register, StagedPending, UpdaterState};

/// 只检查是否有新版本，不下载任何内容。
#[tauri::command]
pub(crate) async fn check(state: State<'_, UpdaterState>) -> Result<CheckOutcome, ErrorDto> {
    state.updater()?.check().await.map_err(ErrorDto::from)
}

/// 执行更新，并把阶段事件推到 `on_event`。
///
/// 更新产物就绪后**插件会自动退出进程**：替换脚本在等旧进程退出，
/// 不退出更新永远不生效。退出前留出 500ms 让最后一条事件到达前端。
#[tauri::command]
pub(crate) async fn install<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
    on_event: Channel<UpdateEvent>,
) -> Result<UpdateResult, ErrorDto> {
    let session = state.updater()?.session();
    let token = session.cancel_token();
    set_cancel(&state, Some(token));

    let result = session
        .run(|event| {
            let _ = on_event.send(event);
        })
        .await;
    set_cancel(&state, None);

    let result = result.map_err(ErrorDto::from)?;

    if matches!(
        result,
        UpdateResult::Updated { .. } | UpdateResult::RepairRequired { .. }
    ) {
        exit_soon(&app);
    }

    Ok(result)
}

/// 下载并合并补丁，**不启动替换脚本**。
///
/// 返回 `{"kind":"ready","version":"..."}` 后，宿主可以先提示用户、
/// 保存草稿、完成清理，再调用 `launch` 触发替换并退出进程。
/// 其余结果（已最新 / 灰度未命中 / 需要全量 / 安全模式）语义与 `install` 一致。
#[tauri::command]
pub(crate) async fn prepare<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
    on_event: Channel<UpdateEvent>,
) -> Result<UpdateResult, ErrorDto> {
    let updater = state.updater()?;
    let token = CancelToken::new();
    set_cancel(&state, Some(token.clone()));

    let outcome = updater
        .prepare_with_events(&token, |event| {
            let _ = on_event.send(event);
        })
        .await;
    set_cancel(&state, None);

    match outcome.map_err(ErrorDto::from)? {
        UpdateOutcome::Patched { staged } => {
            let version = staged.manifest().version.clone();
            state.set_pending(Some(StagedPending::Exe(staged)));
            Ok(UpdateResult::Ready { version })
        }
        UpdateOutcome::Bundled { staged } => {
            let version = staged.manifest().version.clone();
            state.set_pending(Some(StagedPending::Bundle(staged)));
            Ok(UpdateResult::Ready { version })
        }
        UpdateOutcome::Repair { staged, reason } => {
            // 修复是恢复动作，不是用户可延后的更新：立刻启动并让前端退出。
            staged.launch().map_err(ErrorDto::from)?;
            exit_soon(&app);
            Ok(UpdateResult::RepairRequired { reason })
        }
        other => Ok(other.into()),
    }
}

/// 启动 `prepare` 就绪的替换脚本，然后退出进程。
///
/// 没有待启动产物时返回 `nothing_to_launch`；启动失败时可重试（产物仍保留）。
#[tauri::command]
pub(crate) async fn launch<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> Result<UpdateResult, ErrorDto> {
    let pending = state.pending().ok_or_else(ErrorDto::nothing_to_launch)?;
    pending.launch().map_err(ErrorDto::from)?;
    state.set_pending(None);

    let version = pending.version();
    exit_soon(&app);
    Ok(UpdateResult::Updated { version })
}

/// 下载全量安装包并校验签名，**不执行安装**。
///
/// 用于"增量不可用"的回退：前端拿到 `path` 后交给平台安装流程
/// （Windows NSIS/MSI、macOS `.app.tar.gz`、Linux AppImage）。
/// 安装执行器会在后续版本内置到插件；当前阶段先保证下载与验签是走我们自己的。
#[tauri::command]
pub(crate) async fn download_full(
    state: State<'_, UpdaterState>,
    on_event: Channel<UpdateEvent>,
) -> Result<FullDownloadDto, ErrorDto> {
    let updater = state.updater()?;
    let token = CancelToken::new();
    set_cancel(&state, Some(token.clone()));

    let result = updater
        .download_full_with_events(&token, |event| {
            let _ = on_event.send(event);
        })
        .await;
    set_cancel(&state, None);

    result
        .map_err(ErrorDto::from)
        .and_then(FullDownloadDto::from_outcome)
}

/// 执行全量更新：下载 + 验签 + 启动平台安装器。
///
/// 只用于"增量不可用"的回退。与 `install` 一样，产物就绪/安装器启动后
/// 插件会自动退出进程：Windows 安装器需要接管正在运行的可执行文件。
#[tauri::command]
pub(crate) async fn install_full<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
    on_event: Channel<UpdateEvent>,
) -> Result<UpdateResult, ErrorDto> {
    let updater = state.updater()?;
    let token = CancelToken::new();
    set_cancel(&state, Some(token.clone()));

    let result = updater
        .install_full_with_events(&token, |event| {
            let _ = on_event.send(event);
        })
        .await;
    set_cancel(&state, None);

    let result = result.map_err(ErrorDto::from)?;
    if matches!(result, UpdateResult::Updated { .. }) {
        exit_soon(&app);
    }
    Ok(result)
}

/// 环境诊断：可写性、平台适用性、安全模式、是否需要修复。不发起网络请求。
#[tauri::command]
pub(crate) async fn diagnose(state: State<'_, UpdaterState>) -> Result<Diagnosis, ErrorDto> {
    Ok(state.updater()?.diagnose().await)
}

/// 查询跨重启待安装记录（`prepare` 落盘、尚未启动替换）。
#[tauri::command]
pub(crate) async fn pending(
    state: State<'_, UpdaterState>,
) -> Result<Option<PendingUpdateDto>, ErrorDto> {
    let updater = state.updater()?;
    let pending = updater.pending_update().await.map_err(ErrorDto::from)?;
    Ok(pending.as_ref().map(PendingUpdateDto::from_pending))
}

/// 丢弃跨重启待安装记录；`true` 表示确实删除了记录。
///
/// 宿主在 `check()` 发现清单版本高于 `pending` 的版本时应调用本命令，
/// 再安装新版本；否则会先执行一个基于旧清单的暂存脚本。
#[tauri::command]
pub(crate) async fn discard_pending(state: State<'_, UpdaterState>) -> Result<bool, ErrorDto> {
    let updater = state.updater()?;
    updater.discard_pending().await.map_err(ErrorDto::from)
}

/// 执行跨重启待安装记录；`true` 表示已启动替换，插件会随后退出进程。
///
/// 与 `install` 一样必须退出：替换脚本在等旧进程退出，不退出更新不会生效。
#[tauri::command]
pub(crate) async fn install_pending<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, UpdaterState>,
) -> Result<bool, ErrorDto> {
    let updater = state.updater()?;
    let launched = updater.install_pending().await.map_err(ErrorDto::from)?;
    if launched {
        exit_soon(&app);
    }
    Ok(launched)
}

/// 注册插件（配置由宿主代码提供）。
///
/// `config` 的 `current_version` 通常传 [`tauri_delta_updater::host_version!`]；
/// 工作目录用默认值即可（用户缓存目录）。
pub fn init<R: Runtime>(config: UpdateConfig) -> TauriPlugin<R> {
    Builder::new("delta-updater")
        .setup(move |app, _api| {
            register(app, Ok(config));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check,
            install,
            prepare,
            launch,
            download_full,
            install_full,
            diagnose,
            cancel,
            reset_safety,
            pending,
            discard_pending,
            install_pending
        ])
        .build()
}

/// 注册插件，配置从 `tauri.conf.json` 读取（宿主 Rust 侧零代码）。
///
/// 读取 `plugins.delta-updater`；找不到时**回退到 `plugins.updater`**，
/// 因此从官方插件迁移只需替换依赖，配置（`endpoints` / `pubkey`）原样可用。
///
/// ```json
/// {
///   "plugins": {
///     "delta-updater": {
///       "endpoints": ["https://cdn.example.com/latest.json"],
///       "pubkey": "RWQ...",
///       "additional_pubkeys": ["RWQ... (old key during rotation)"],
///       "trust_policy": "any",
///       "channel": "stable",
///       "bundle_endpoint": "https://cdn.example.com/latest.bundle.json",
///       "work_dir": "/custom/writable/dir",
///       "wait_timeout_secs": 60,
///       "confirm_timeout_secs": 60,
///       "proxy": "http://127.0.0.1:7890",
///       "allow_legacy_signatures": false,
///       "headers": { "Authorization": "Bearer ..." },
///       "fallback_policy": "transport",
///       "install_mode": "passive",
///       "allow_insecure_transport": false
///     }
///   }
/// }
/// ```
pub fn init_from_config<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("delta-updater")
        .setup(|app, _api| {
            // 配置缺失/非法只让更新功能不可用，不能让宿主应用起不来。
            register(app, read_file_config(app).map_err(|err| err.to_string()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check,
            install,
            prepare,
            launch,
            download_full,
            install_full,
            diagnose,
            cancel,
            reset_safety,
            pending,
            discard_pending,
            install_pending
        ])
        .build()
}

/// 取消正在进行的更新。返回 `false` 表示当前没有可取消的会话。
#[tauri::command]
pub(crate) fn cancel(state: State<'_, UpdaterState>) -> bool {
    match state.cancel.lock() {
        Ok(mut slot) => match slot.take() {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        },
        Err(_) => false,
    }
}

/// 手动退出安全模式（UI 提供"重试"按钮时使用）。
#[tauri::command]
pub(crate) async fn reset_safety(state: State<'_, UpdaterState>) -> Result<(), ErrorDto> {
    state.updater()?.reset_safety().await;
    Ok(())
}

fn set_cancel(state: &State<'_, UpdaterState>, token: Option<CancelToken>) {
    if let Ok(mut slot) = state.cancel.lock() {
        *slot = token;
    }
}

/// 稍后退出进程：先让命令的返回值与最后一条事件到达前端，再退出。
///
/// 立即退出会杀掉 IPC，前端收不到最终状态；500ms 对本地 IPC 足够宽裕。
fn exit_soon<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        app.exit(0);
    });
}
