# Usage guide

## 0. 场景索引（先选路径，再读细节）

| 你的场景 | 最小配置 | 验证方式 | 常见坑 |
|---|---|---|---|
| 只替换主程序（Windows `.exe`、Linux AppImage、macOS 裸二进制） | `endpoints` + `pubkey`；权限 `delta-updater:default` | `cargo delta simulate --old old.exe --new new.exe` 看收益；再跑一次真实更新 | macOS `.app` 不在这个场景里（见下一行）；Linux `.deb`/`.rpm` 不能自动装 |
| 主程序 + sidecar/resources，或 macOS `.app` | 额外 `bundle_endpoint` | `generate --new-app-dir … --old-app-dir …` 后检查 `latest.bundle.json` | 旧版本目录名必须含版本号；macOS 必须真机验证签名/公证 |
| 增量不可用时用平台安装器兜底 | 权限加 `delta-updater:full`；配置 `installer_kind`/`install_mode`/`installer_args` | `installFull()` 或 `checkCompat()` 的自动回退 | 前端没有 `full` 权限会被 capability 拒绝；Inno 必须显式 `installer_kind: "inno"` |
| Inno Setup 打包 | `installer_kind: "inno"` | `cargo delta init --check`、`doctor` | Inno 没有 `/UPDATE` `/R` 等价参数，重启由 `.iss` 的 `[Run]` 负责 |
| 灰度放量 | 发布端 `--rollout 20`、必要时 `--min-version` | 同一安装重复 `check()` 结果稳定 | `deferred_by_rollout` 不是“已最新”；灰度未命中时不返回全量地址 |
| 官方 updater 迁移 | `cargo delta migrate`，前端改用 `checkCompat()` | `cargo delta init --check`、粘贴 migrate 输出 | 官方 `downloadAndInstall` 的回调形状由兼容层转换，需授予 `delta-updater:full` |

## 1. Add the dependency

```toml
[dependencies]
tauri-delta-updater = "0.3"
```

Inside a Tauri app, `tauri-plugin-delta-updater` (the `plugin/` crate) wraps this
one and exposes the same engine as commands and events, so you never touch the
types below. Continue here if you want the library directly.

The library has **no default features**. If you need the `generate` CLI, enable the
publisher tooling explicitly:

```toml
tauri-delta-updater = { version = "0.3", features = ["generator"] }
```

`rust-version = "1.88"` (the real floor of the `reqwest 0.13` → `url` → `icu_*`
chain; declared honestly and checked by a CI job). `reqwest` is constrained to
`>=0.13, <0.14`: an app still on 0.12 will resolve two copies, so upgrade the
app's `reqwest` before adopting this engine.

## 2. Client-side integration (Tauri backend)

**One call does it all** — fetch manifest → verify signature → version check → match patch → download → verify → apply → launch swap. The zero-config form derives platform, work directory, and HTTP client:

```rust
let updater = tauri_delta_updater::DeltaUpdater::auto(
    "https://cdn.example.com/latest.json",
    "BASE64_PUBLIC_KEY",
    app.package_info().version.to_string(),
)?;
```

For a non-Tauri host (CLI, service, tray app), `host_version!()` expands to the
**caller crate's** `CARGO_PKG_VERSION`:

```rust
let updater = tauri_delta_updater::DeltaUpdater::auto(
    "https://cdn.example.com/latest.json",
    "BASE64_PUBLIC_KEY",
    tauri_delta_updater::host_version!(),
)?;
```

The default path returns UI-level outcomes — no patch URL, hash, or staging file
type to handle:

```rust
use tauri_delta_updater::UpdateResult;

match updater.install().await? {
    UpdateResult::Updated { version } => {
        // replacement script already launched; exit the process
        let _ = version;
        std::process::exit(0);
    }
    UpdateResult::RepairRequired { reason } => {
        // a previous whole-app swap died halfway; the repair script is already
        // running — exit so it can restore the backup and relaunch the old version
        let _ = reason;
        std::process::exit(0);
    }
    UpdateResult::UpToDate { .. } => { /* nothing to do */ }
    UpdateResult::Deferred { percentage, .. } => {
        // A newer version exists, but this install is outside the rollout.
        // Show "will update automatically" rather than "already up to date".
        let _ = percentage;
    }
    UpdateResult::RequiresFullDownload { url, reason } => {
        // hand off to install_full() or the official updater; `reason` explains why
        let _ = (url, reason);
    }
    UpdateResult::UnsupportedOs { required, current } => {
        // nothing was downloaded: the running OS is below the manifest's min_os
        let _ = (required, current);
    }
    UpdateResult::SafeMode => { /* prompt for a manual install */ }
    // `UpdateResult` is non_exhaustive: new outcomes won't break your build.
    _ => {}
}
```

For a progress bar, use `install_with_progress(|downloaded, total| ...)` — it
returns the same `UpdateResult`, so you do not have to drop down to the advanced
API just to show a progress bar. The callback covers the **download** only; once
`downloaded == total`, the patch is being merged and verified, so switch the UI
to an "installing" state.

`install_and_exit()` does the same but exits automatically after a successful
update.

## Returning the result to the frontend

`UpdateResult` is `Serialize` with a `kind` tag (`snake_case`), so a command can
hand it straight to the UI and the UI can switch on `kind`:

```json
{"kind":"up_to_date","version":"1.2.3"}
{"kind":"deferred","version":"1.2.3","percentage":20}
{"kind":"requires_full_download","url":"https://...","reason":{"no_matching_patch":{"from":"1.2.3","to":"1.3.0"}}}
{"kind":"safe_mode"}
```

For `updated`, the backend exits the process so the swap script can run — emit an
event before exiting instead of trying to return a value.
`UpdateResult::requires_exit()` is the library-level predicate for this
(`true` for `updated` / `repair_required`, `false` for `ready`), so you do not
need to duplicate the variant list.

<details>
<summary>Complete Tauri wiring (setup + command)</summary>

```rust
use tauri::{Emitter, Manager};
use tauri_delta_updater::{DeltaUpdater, PlatformUpdater, UpdateResult};

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // No-op on normal launches; only acts after a real swap.
            let _ = PlatformUpdater::confirm_startup_success_if_pending();

            let updater = DeltaUpdater::auto(
                "https://cdn.example.com/latest.json",
                "BASE64_PUBLIC_KEY",
                app.package_info().version.to_string(),
            )?;
            app.manage(updater); // DeltaUpdater is Send + Sync
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![install_update])
        .run(tauri::generate_context!())
        .expect("failed to run app");
}

#[tauri::command]
async fn install_update(
    app: tauri::AppHandle,
    updater: tauri::State<'_, DeltaUpdater>,
) -> Result<UpdateResult, String> {
    let result = updater
        .install_with_progress(|downloaded, total| {
            let _ = app.emit("update://progress", (downloaded, total));
        })
        .await
        .map_err(|e| e.to_string())?;

    if matches!(
        result,
        UpdateResult::Updated { .. } | UpdateResult::RepairRequired { .. }
    ) {
        // The swap script is already running: tell the UI, then exit so it can
        // replace the executable. Do not use `install_and_exit()` here — it
        // exits before the command can return anything.
        let _ = app.emit("update://ready", ());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::process::exit(0);
    }

    Ok(result)
}
```

The frontend calls `invoke('install_update')`, gets the same JSON shape above, and
listens for `update://progress` (download bytes) and `update://ready` (restart).

</details>

## Cancellable updates (session)

`install()` is a single call. When the UI needs to know each phase or offer a
cancel button, use a session:

```rust
use tauri_delta_updater::UpdateEvent;

let session = updater.session();
let token = session.cancel_token(); // clone it for the UI / another command

let result = session
    .run(|event| match event {
        UpdateEvent::Downloading { downloaded, total } => { /* progress bar */ }
        UpdateEvent::Applying => { /* switch to "installing" */ }
        UpdateEvent::Ready { version } => { /* tell the user to restart */ }
        _ => {}
    })
    .await?;

// `token.cancel()` from anywhere stops the update.
```

`UpdateEvent` is `Serialize` with a `stage` tag (`{"stage":"downloading","downloaded":123,"total":456}`), so
it can be emitted to the frontend directly. Cancelling returns
`DeltaError::Cancelled`, removes partial files, and does **not** count toward safe
mode. Cancellation is cooperative: it stops during download and at phase
boundaries; it cannot interrupt the merge itself, and it cannot undo an
already-launched swap.

<details>
<summary>Handing a full download to the official updater</summary>

```rust
use tauri_delta_updater::UpdateResult;

match updater.install().await? {
    UpdateResult::RequiresFullDownload { url: Some(url), reason } => {
        // install() has returned, so the update lock is already released and the
        // official updater can take over safely. `reason` is a typed enum: log or
        // show it to the user, then let the official updater / installer flow run.
        let _ = (url, reason);
    }
    _ => {}
}
```

If you went through `check()` first, `CheckOutcome::full_download()` gives the
same handoff as a typed value — `{ version, url, signature }` — so the installer
flow gets the signature without re-parsing the manifest:

```rust
let info = updater.check().await?;
if !info.has_patch {
    if let Some(full) = info.full_download() {
        // full.url + full.signature + full.version → tauri-plugin-updater
    }
}
```

`full_download()` returns `None` whenever `check()` intentionally hid the
installer URL — the manifest is not newer, this install missed the rollout, or
the running OS is below `min_os` — so it cannot be used to bypass those gates.
`install_full()` applies the same gates and returns `Deferred` /
`UnsupportedOs` without downloading.

The fallback can also stay inside this engine: `install_full()` downloads,
verifies and runs the installer (NSIS/MSI, `.app.tar.gz`, AppImage). If you keep
the official updater instead, hand it `url` + `signature` as above. **Do not run
both plugins concurrently**: the manifest can be fetched twice and the security
models do not compose.

If you use `check()` before installing, `info.full_download()` returns
`{ version, url, signature }` as a typed value. With the one-call `install()`
path you only get `url` + `reason`; when you need the installer signature
for the official updater, go through `check()` first.

</details>

<details>
<summary>Inno Setup installers (.exe)</summary>

Inno Setup and NSIS both produce `.exe`, so the type cannot be inferred from the
suffix alone. The engine scans the installer for Inno/NSIS marker strings and
picks Inno when found; if you use a custom loader or the markers are stripped,
declare `installer_kind: "inno"` explicitly. Otherwise the installer is called
with NSIS flags (`/P /UPDATE /R`), which Inno ignores: it opens the full wizard
or fails, and the engine side sees no error.

`installer_kind` only affects **this engine's full-install path**
(`install_full()`). The official `tauri-plugin-updater` has no Inno support and
always passes NSIS arguments, so packaging Inno installers while keeping the
official updater for full downloads still opens the wizard (or fails). Either
keep NSIS installers or move the full fallback to `install_full`.

```rust
use tauri_delta_updater::{InstallerKind, UpdateConfig};

let config = UpdateConfig::new("https://cdn.example.com/latest.json", "PUBKEY")
    .current_version("0.1.0")
    .installer_kind(InstallerKind::Inno);
```

With the plugin, set it in `tauri.conf.json`:

```json
"plugins": { "delta-updater": { "installer_kind": "inno" } }
```

`install_mode` maps to Inno's native flags:

| `InstallMode` | Inno flags |
|---|---|
| `Passive` (default) | `/SILENT /NORESTART /SP-` |
| `Quiet` | `/VERYSILENT /NORESTART /SP-` |
| `BasicUi` | *(empty — Inno's default UI)* |

`/SP-` suppresses Inno's "This will install…" prompt; `/NORESTART` prevents a
silent-mode system reboot. **`/UPDATE` and `/R` have no Inno equivalent**: whether
this is an update and whether the app restarts afterwards are `.iss`
responsibilities. A minimal `.iss` shape:

```ini
[Setup]
AppId={{YOUR-GUID}}
DisableStartupPrompt=yes
CloseApplications=yes

[Run]
Filename: "{app}\MyApp.exe"; Flags: nowait postinstall
```

Do **not** add `skipifsilent` there: it skips the launch in exactly the
`/SILENT` / `/VERYSILENT` updates this engine performs, leaving the user with a
closed app after a silent update. Detecting a previous install, reading/removing
the old NSIS uninstall entry, and migrating per-user data are also script-side.
The engine only guarantees the installer is launched with the right flags; it
does not replace your `.iss` logic.

</details>

```rust
use tauri_delta_updater::{DeltaUpdater, UpdateConfig, UpdateOutcome};

// Only the manifest URL and the public key are required; everything else defaults.
let updater = DeltaUpdater::new(
    UpdateConfig::new(
        "https://cdn.example.com/latest.json",
        // The public key must be pinned in the app — never fetched from the server.
        "BASE64_PUBLIC_KEY",
    )
    .current_version(app.package_info().version.to_string()),
)?;

match updater.update().await? {
    UpdateOutcome::Patched { staged } => {
        println!("Updated to {}", staged.manifest().version);
    }
    UpdateOutcome::Bundled { staged } => {
        // Whole-app update (main exe + sidecars + resources) when
        // `bundle_manifest_url` is configured.
        println!("Updated whole app to {}", staged.manifest().version);
    }
    UpdateOutcome::UpToDate { version } => {
        println!("Already up to date ({version})");
    }
    UpdateOutcome::FallbackToFullDownload { full_download_url, reason } => {
        // No patch, or an environment where in-place replacement is unsafe.
        // `reason` is a typed FallbackReason; hand off to tauri-plugin-updater.
        let _ = (full_download_url, reason);
    }
    UpdateOutcome::SafeMode => {
        // Too many consecutive failures — prompt the user to install manually.
    }
    // UpdateOutcome is non_exhaustive; new variants won't break your match.
    _ => {}
}
```

> [!IMPORTANT]
> **You must exit the process after a successful update.**
>
> The replacement script waits for the old process to exit before it can swap
> the executable, and waits for the new version to confirm startup. **If the main
> process never exits, the script rolls back** and the user sees "the update
> undid itself".
>
> The correct flow is: notify the frontend → exit the process.
>
> ```rust
> std::process::exit(0);
> ```

For download progress, use `updater.update_with_progress(|downloaded, total| { ... })`.

> [!WARNING]
> **Exiting under the single-instance plugin — test before shipping.** Use
> `std::process::exit(0)` rather than `app.restart()` (restart re-enters the
> event loop and can race with the script as a second instance). `exit(0)` skips
> `Drop`, so verify the plugin's socket/lock cleanup on each platform. If you
> see a stale lock, emit an `update-ready` event, wait briefly, then
> `app.exit(0)`.

Once the new version has started, call
`PlatformUpdater::confirm_startup_success_if_pending()` (or the plain
`confirm_startup_success()` if you prefer). If you set a custom
`UpdateConfig::work_dir`, call `updater.confirm_startup_success()` (the instance
method) instead — it writes to the configured directory. The replacement script
also drops a `tdx_update_dir` pointer next to the executable, so the static call
can resolve custom directories on its own.

<details>
<summary>Staged integration (check first, download after user confirmation)</summary>

```rust
// 1. Check only — nothing is downloaded.
let info = updater.check().await?;
if info.is_newer {
    if info.has_patch {
        println!(
            "New version {} available, patch {} bytes",
            info.version,
            info.patch_size.unwrap_or(0)
        );
    } else {
        // No usable patch (cross-version upgrade, read-only AppImage, macOS bundle, ...).
        // `full_download()` returns `{ version, url, signature }` for the current
        // platform, ready to hand to the official updater or your own installer.
        let full = info.full_download().expect("manifest lists this platform");
        println!("Full installer: {} ({})", full.url, full.version);
    }
}

// 2. After the user confirms: download and merge, but don't swap yet.
match updater.prepare().await? {
    UpdateOutcome::Patched { staged } => {
        println!("Restarting to finish the update...");
        staged.launch()?;   // One-shot: a second successful launch is rejected.
        std::process::exit(0);
    }
    UpdateOutcome::Bundled { staged } => {
        // Same lifecycle for whole-app updates.
        staged.launch()?;
        std::process::exit(0);
    }
    _ => {}
}
```

`full_download_for(platform)` does the same for an explicit manifest key, which is
useful when a single release build inspects several targets.

`UpdateConfig::auto_launch(false)` also stops `update()` from launching the swap script automatically.

</details>

<details>
<summary>Step-by-step control (advanced)</summary>

1. Fetch `latest.json` and `latest.json.sig`.
2. Verify the manifest signature with `verify_manifest_signature`.
3. Reject non-newer manifests with `manifest.is_newer_than(current_version)`.
4. Match a patch with `manifest.find_patch(current_version, current_platform())`.
5. Download it with `download_patch_with_progress`.
6. Apply it with `apply_patch` (returns a `VerifiedArtifact`).
7. Generate and launch the swap script with `PlatformUpdater::generate_updater` + `PlatformUpdater::launch_updater`.
8. After the new version starts, call `PlatformUpdater::confirm_startup_success()`.

```rust
use tauri_delta_updater::{current_platform, verify_manifest_signature};

let manifest_bytes = fetch_latest_json().await?;
let pubkey = app_config().updater_pubkey.clone();

// A failed signature check must abort the update.
verify_manifest_signature(&manifest_bytes, &latest_json_sig, &pubkey)?;

let manifest: tauri_delta_updater::UpdateManifest = serde_json::from_slice(&manifest_bytes)?;
if !manifest.is_newer_than(current_version)? {
    return Ok(());
}

if let Some(patch) = manifest.find_patch(current_version, current_platform()) {
    // Download the patch, verify patch_hash, apply it, verify target_hash.
}
```

</details>

