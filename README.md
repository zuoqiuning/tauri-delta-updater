[![CI](https://github.com/zuoqiuning/tauri-delta-updater/actions/workflows/ci.yml/badge.svg)](https://github.com/zuoqiuning/tauri-delta-updater/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

# tauri-delta-updater

> 📦 版本与发布策略见 [VERSIONING.md](VERSIONING.md)。

A high-performance, secure, and fault-tolerant delta update engine for Tauri v2 applications.

## Quick start

```rust
use tauri_delta_updater::{DeltaUpdater, UpdateResult};

let updater = DeltaUpdater::auto(
    "https://cdn.example.com/latest.json",
    "BASE64_PUBLIC_KEY",
    app.package_info().version.to_string(),
)?;

match updater.install().await? {
    UpdateResult::Updated { .. } => std::process::exit(0),
    UpdateResult::UpToDate { .. } => {}
    UpdateResult::RequiresFullDownload { .. } => { /* hand off to the official updater */ }
    UpdateResult::SafeMode => { /* prompt for a manual install */ }
}
```

That is the whole default path. The library derives the platform, a user-writable
work directory, and the HTTP client; "no delta available" is a normal outcome,
not an error you have to assemble yourself.

## Overview

`tauri-delta-updater` is a Rust library that implements incremental (delta) updates for Tauri v2 applications. Instead of downloading the full installer on every release, clients download a small binary patch and merge it locally — dramatically reducing bandwidth and wait time. It extends the standard `latest.json` manifest used by the official `tauri-plugin-updater`, so both update channels can coexist.

**Positioning: an incremental accelerator, not a parallel update stack.** Register the official `tauri-plugin-updater` as usual, and let this library take over when a safe delta exists. When it doesn't — no compatible delta, or an environment where in-place replacement isn't safe — the library reports that a full download is required, with a structured reason, and the official updater does what it always does. Falling back is not a failure; correctness comes first.

Platform support:

| Platform | Status | Notes |
|---|---|---|
| Windows | **Stable** | Single-file exe is the sweet spot; the replacement script runs on PowerShell, handles elevation, special-character install paths, and relaunches through `explorer.exe` with **only the target path** (explorer ignores extra arguments) so the app does not inherit the admin token |
| macOS | Limited | Bare (non-bundle) binaries only, experimental. Inside a `.app` bundle the library **automatically falls back** — in-place replacement would lose the developer signature and notarization. The replacement script refuses to run on bundles rather than silently ad-hoc re-signing |
| Linux | Limited | Bare binaries; **AppImage automatically falls back** (the executable lives on a temporary squashfs mount). `.deb` installs elevate via passwordless `sudo` or `pkexec`, then **drop privileges back** via `runuser` (falling back to `su`) and return file ownership to the original user |

> [!IMPORTANT]
> **Scope of what a patch covers.** The patch's base and product are the **main
> executable**. Apps using `externalBin` (sidecars) or `resources` are outside
> this scope. The manifest now marks every patch with
> `"scope": "main_executable_only"` so clients can warn instead of discovering
> it through an update→rollback loop.
>
> System-level installs also bypass the package manager / installer metadata:
> `Apps & Features` / `dpkg` keep reporting the old version, and the uninstaller
> still describes the old layout. Prefer the full-download path for managed
> deployments.

> [!NOTE]
> **Install directory permissions.** The patch, staging file, replacement script,
> and log all live in a **user-writable work directory** (derived from
> `%LOCALAPPDATA%` / `~/Library/Caches` / `$XDG_CACHE_HOME`), not next to the
> executable. Only the final replacement touches the install directory, so
> `C:\Program Files` and `/usr/bin` installs no longer fail during download.
> If elevation is required, the script shows a UAC/polkit prompt after the main
> process has exited; if the user declines, the **old version is relaunched**
> instead of the app simply disappearing.

## Features

- **High compression ratio** — powered by `qbsdiff`. A 1 MB change inside a 20 MB binary produced a 210-byte patch in our benchmark. Actual ratios depend on how similar the two binaries are; see [Patch size expectations](#patch-size-expectations).
- **Security first** — dual SHA-256 verification: the downloaded patch *and* the merged result are both hash-checked. The staged artifact's hash is handed to the replacement script, which verifies it again before and after the copy (closing the verification→swap TOCTOU window).
- **Signed manifests** — Ed25519 (minisign) signatures on `latest.json` prevent man-in-the-middle tampering. Signature failures are reported as a distinct error so tampering can be told apart from a corrupted download.
- **Downgrade protection, three layers, on by default** — (1) the **manifest itself is signed**, so an attacker cannot repoint it at an old installer without breaking the signature; (2) after verification, `manifest.version` is compared against the running version with **semantic versioning**, and a non-newer manifest returns `UpdateOutcome::UpToDate` instead of handing the host an old installer URL; (3) the generator's version binding in the trusted comment catches publisher-side "signature from A on manifest B" mistakes. The first two layers block replay; the third is defense in depth. None of them depends on an optional switch.
- **Typed fallback reasons** — `FallbackReason::NoMatchingPatch` / `EnvironmentUnsupported`, instead of a free-form string that forces UI code to match Chinese prose.
- **Local verification harness** — `simulate_update(old, new, work_dir)` runs generate → apply → dual-hash verification with no network or signing key, so you can see the real patch ratio before integrating.
- **Environment diagnosis** — `DeltaUpdater::diagnose()` reports work-directory/install-directory writability, platform eligibility, safe-mode state, and the exact log path.
- **Optional structured events** — enable the `tracing` feature to receive `delta.check`, `delta.patch.matched`, `delta.patch.rejected`, `delta.fallback`, `delta.safety_mode.entered`, `delta.safety_mode.cooldown_active`, and `delta.patched` events; zero cost when disabled.
- **Zero-config constructor** — `DeltaUpdater::auto(url, pubkey, version)` derives the platform, user work directory, and HTTP client.
- **UI-level default path** — `install()` returns `UpdateResult::{Updated, UpToDate, RequiresFullDownload, SafeMode}`; the advanced `update()` still returns the full `UpdateOutcome` when you need patch-level control.
- **Platform-scoped patches** — patches are grouped per platform key, so an x64 client never picks up an arm64 patch.
- **Staged rollout APIs** — `check()` to query without downloading, `prepare()` to download and merge without launching the swap, and `Staged::launch()` to trigger the swap when you're ready.
- **Auto-rollback with a real restart** — if the new version fails to write its startup flag within the timeout, the script stops the new process, restores the backup, **and relaunches the old version**. The same recovery runs when elevation is declined or a copy fails.
- **Crash-safe replacement** — if a previous run was interrupted between "rename exe → `.bak`" and the swap, the next script run restores the backup first; a leftover `.bak` next to an intact target is never restored.
- **Safe mode with automatic recovery** — after 3 consecutive *deterministic* failures (bad signature, hash mismatch, disk full...), delta updates pause. Transient network errors don't count. After a 6-hour cooldown a single new attempt is allowed automatically; hosts can also call `reset_safety()`.
- **Concurrency protection** — a cross-process file lock prevents parallel updaters from corrupting state. Lock contention surfaces as `DeltaError::UpdateInProgress`, not a generic worker error.
- **Network resilience** — HTTP Range resumption (only trusted with `206 Partial Content`; a `200` answer restarts the download cleanly) and exponential-backoff retries **with jitter**.
- **Injectable HTTP client** — pass your own `reqwest::Client`, or build it with `client_builder()` which installs the TLS backend for you.
- **zstd-compressed patches** — raw bsdiff output is zstd-compressed at generation time and sniffed by magic on apply, so legacy uncompressed patches keep working.
- **Bounded downloads and bounded expansion** — the patch limit derives from the signed `patch_size` (+1 MB); manifests are capped at 10 MB. Compressed patches are streamed to a temp file and memory-mapped, with a decompressed-size ceiling, so a 2 GB target can't turn into multi-GB RSS.
- **Memory-mapped delta** — old files and patches are read via read-only `mmap`; the merged result is written straight to disk.
- **Scoped cleanup** — only artifacts this project created are removed. An unrelated `notes.txt.bak` in the install directory survives.
- **Crash-tolerant auto cleanup** — patch, staging, script, expanded-patch temp, and the target's own `.bak` are reclaimed on the next update.

## Patch size expectations

Patch size depends on **how similar the two binaries are**, not on their size:

- **Very similar** (only resources or metadata changed): patches can be a few hundred bytes.
- **Typical** (a couple of modules rebuilt, new build timestamps): expect tens of KB to a few MB.
- **Very different** (full recompile with a different toolchain): the patch can approach the size of the full installer — `generate --max-patch-ratio` can skip those.

A patch is always verified against both hashes, so a poor-ratio patch is still *correct*, just not smaller.

### Memory and file size limits

`qbsdiff` needs the old and new files available while computing or applying a patch; its working memory is roughly `max(17 × old_size, 9 × old_size + new_size)`, which is why large files are rejected rather than attempted. Single files larger than `MAX_INCREMENTAL_FILE_BYTES` (2 GB) are rejected with `DeltaError::IncrementalNotApplicable` — fall back to the full installer for those. Old files and patches are memory-mapped, the merged result is streamed to disk, and decompressed patch size is capped at `2 × old_size + 128 MB`.

## Test scenarios

The engine is verified under adversarial conditions:

- **Bandwidth savings** — a 20 MB executable with a 1 MB change yields a patch far below the full size (asserted in `test_large_file_performance`).
- **Real script execution** — the generated PowerShell / `sh` scripts are actually executed: special-character install directories, PID/path matching, crash recovery, backup cleanup, startup flag.
- **Delta really ran** — the end-to-end mock server records every requested path; delta-hit tests assert the full-installer URL was **never** requested. A delta updater that always silently falls back can no longer stay green.
- **Downgrade replay** — a validly-signed *older* manifest must return `UpToDate` and must not request the old installer.
- **Failure recovery** — every failure after the old process exits funnels through one handler that restores the backup and relaunches the old version. The 60-second startup-timeout branch needs a real timeout window and is covered by the manual procedure in the design document.
- **Resume safety** — a server that ignores `Range` headers no longer produces a corrupted patch; the download restarts cleanly.
- **Lock contention** — `UpdateInProgress` is matched, not string-parsed (Unix; Windows byte-range locks are per-process and cannot be reproduced in-process).
- **Safe mode** — after the threshold, `check()` returns `DeltaError::SafeMode` without any network request; the 6-hour cooldown resets automatically.

## Manifest structure

The manifest is a superset of the Tauri official `latest.json` format:

```json
{
  "version": "0.1.1",
  "notes": "Update notes",
  "pub_date": "2026-11-04T16:35:22.903Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6...",
      "url": "https://example.com/Sereno_0.1.1_x64-setup.exe"
    }
  },
  "patches": {
    "windows-x86_64": [
      {
        "from_version": "0.1.0",
        "to_version": "0.1.1",
        "patch_url": "https://example.com/0.1.0_to_0.1.1.patch",
        "patch_hash": "6df96393...",
        "target_hash": "bbd05cf6...",
        "patch_size": 2048,
        "scope": "main_executable_only"
      }
    ]
  }
}
```

`patches` is grouped by platform key, matching the keys in `platforms`. The legacy flat-array form is still accepted when reading, so manifests published by earlier versions keep working.

## Usage guide

### 1. Add the dependency

```toml
[dependencies]
tauri-delta-updater = "0.1"
```

The library has **no default features**. If you need the `generate` CLI, enable the
publisher tooling explicitly:

```toml
tauri-delta-updater = { version = "0.1", features = ["generator"] }
```

`rust-version = "1.88"` (the real floor of the `reqwest 0.13` → `url` → `icu_*`
chain; declared honestly and checked by a CI job).

### 2. Client-side integration (Tauri backend)

**One call does it all** — fetch manifest → verify signature → version check → match patch → download → verify → apply → launch swap. The zero-config form derives platform, work directory, and HTTP client:

```rust
let updater = tauri_delta_updater::DeltaUpdater::auto(
    "https://cdn.example.com/latest.json",
    "BASE64_PUBLIC_KEY",
    app.package_info().version.to_string(),
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
    UpdateResult::UpToDate { .. } => { /* nothing to do */ }
    UpdateResult::RequiresFullDownload { url, reason } => {
        // hand off to tauri-plugin-updater; `reason` explains why
        let _ = (url, reason);
    }
    UpdateResult::SafeMode => { /* prompt for a manual install */ }
}
```

`install_and_exit()` does the same but exits automatically after a successful
update.

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

Once the new version has started, call `PlatformUpdater::confirm_startup_success()`
once. If you set a custom `UpdateConfig::work_dir`, call
`updater.confirm_startup_success()` (the instance method) instead — it writes to
the configured directory. The replacement script also drops a `tdx_update_dir`
pointer next to the executable, so the static call can resolve custom directories
on its own.

<details>
<summary>Staged integration (check first, download after user confirmation)</summary>

```rust
// 1. Check only — nothing is downloaded.
let info = updater.check().await?;
if info.is_newer {
    println!(
        "New version {} available, patch {} bytes",
        info.version,
        info.patch_size.unwrap_or(0)
    );
}

// 2. After the user confirms: download and merge, but don't swap yet.
if let UpdateOutcome::Patched { staged } = updater.prepare().await? {
    println!("Restarting to finish the update...");
    staged.launch()?;   // One-shot: a second successful launch is rejected.
    std::process::exit(0);
}
```

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

### 3. CI/CD integration (release workflow)

Build the CLI generator (the `generator` feature is required):

```bash
cargo build --release --features generator --bin generate
```

> [!IMPORTANT]
> **The generator takes the *installed main executable*, not the installer.**
> `--app-binary` / `--old-dir` are the patch base and product. `--installer`
> supplies `platforms[].url` and `platforms[].signature` for the full-download
> path. Feeding the NSIS/MSI installer as the patch base produces a patch that
> can never match the installed `app.exe`.

```bash
./target/release/generate \
  --old-dir ./old_binaries \
  --app-binary ./target/release/Sereno.exe \
  --installer ./target/release/Sereno_0.1.1_x64-setup.exe \
  --version 0.1.1 \
  --base-url https://your-cos-endpoint.com \
  --private-key "$TAURI_SIGNING_PRIVATE_KEY" \
  --notes "What's new" \
  --output-dir ./dist
```

Key arguments:

| Argument | Meaning |
|---|---|
| `--old-dir` | Directory of **old main executables**. Every matching version gets a patch. |
| `--app-binary` | The new main executable (patch target / `target_hash`). |
| `--installer` | The new full installer; used for the full-download URL and its signature. Defaults to `--app-binary` for portable single-file apps. |
| `--version` | Target version; a leading `v` is normalized. |
| `--platform` | Optional platform key; otherwise inferred from the filename (`windows-x86_64`, `linux-x86_64`, `darwin-aarch64`...). Inference failure is an error, never a silent `windows-x86_64` default. |
| `--max-patch-ratio` | Skip patches larger than this fraction of the installer (default `1.0`: only skip when the patch has no benefit). |
| `--zstd-level` | Patch compression level, 1–22 (default 19). Lower it to 15–17 when CI time matters for large patches. |
| `--existing` | Previous `latest.json`, to preserve other platforms' entries and patches. |
| `--password` | Private-key password (or `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). |
| `--output-dir` | Where `latest.json`, `.sig`, and `.pub` are written (default: current directory). |
| `--dry-run` | Plan and verify without writing files. |

The generator only produces patches for old executables matching the target
architecture/OS; every patch is **applied and hash-verified** before it is
written into the manifest, so a bad patch fails CI instead of a user's rollback.

**Maximize hit rate with a rolling window**: keep the last 5–8 old executables in
`old_binaries/`. Patches are immutable — cache them on your CDN with
`Cache-Control: immutable, max-age=31536000`.

> [!IMPORTANT]
> **The tag and the manifest version must use the same format.** The client
> reports `CARGO_PKG_VERSION` (e.g. `0.1.1`, no `v`); the generator normalizes
> the prefix for you.

> [!WARNING]
> **The public key must be pinned in your application** — never fetched from the
> same server that serves `latest.json`. `latest.json.pub` is generated for
> operator convenience only; its first line carries a `DO NOT TRUST` warning.

### Manifest compatibility

`latest.json` with grouped `patches`, `patch_size`, `scope`, version monotonicity,
and a user-writable work directory. Older manifests with a flat `patches` array
and no `patch_size` are still accepted when reading.

### 5-minute local verification

You do not need a network, a signed manifest, or a Tauri app to see whether delta
updates actually work. Put two versions of your executable anywhere, then:

```rust
#[tokio::main]
async fn main() -> tauri_delta_updater::Result<()> {
    let report = tauri_delta_updater::simulate_update(
        "old.exe",
        "new.exe",
        "target/delta-sim",
    )
    .await?;
    println!(
        "patch {} bytes / full {} bytes ({:.1}%)",
        report.patch_size,
        report.target_size,
        report.ratio() * 100.0
    );
    Ok(())
}
```

`simulate_update` runs the same path the client uses: generate → zstd-compress →
apply → dual SHA-256 verification. It writes only under the given directory and
cleans up after itself. If the ratio is close to 100%, the two builds are too
different for a delta to help.

### Integration comparison

| | `tauri-delta-updater` | official `tauri-plugin-updater` | `tauri-plugin-updater-delta` |
|---|---|---|---|
| Integration | library call (2–3 lines) | plugin registration | plugin registration |
| Delta algorithm | qbsdiff + zstd, PE-agnostic | full download | patch over official `Update` |
| Fallback | typed `FallbackReason`, host decides | n/a | transparent |
| Security model | own manifest signature + double hash + rollback | official signature/installer | official signature + local hash |
| Rollback | own script, crash recovery, startup confirm | installer | installer |
| Platform reach | Windows stable; macOS bundle/AppImage fall back | all Tauri targets | all Tauri targets |

If you only want "make the download smaller" and nothing else, the official
updater plus a transparent layer is the simpler choice. If you want a manifest
signature, deterministic rollback, safe mode, and the ability to inspect every
fallback, this library is the deeper option.

### Troubleshooting quick reference

| Symptom | Cause / fix |
|---|---|
| Always falls back to full download | Manifest key doesn't match what the client looks up. Use `current_platform()` on the client and let `generate` infer the key. |
| Always falls back to full download | `to_version` doesn't match `manifest.version`, or the client version differs in format. The generator normalizes the `v` prefix. |
| `DeltaError::Config` at startup | `current_version` is empty or not semver. |
| `DeltaError::WorkDirNotWritable` | A custom `work_dir` is not writable by the user. Omit it to use the user cache. |
| `DeltaError::IncrementalNotApplicable` | The file exceeds the 2 GB incremental limit; use the full installer. |
| `DeltaError::ManifestInvalid` | The signed manifest is malformed (bad JSON / not semver). |
| `DeltaError::ResponseTooLarge` | A manifest or signature response exceeded 10 MB. Deterministic; not retried. |
| `DeltaError::UpdateInProgress` | Another updater holds the lock. Prompt "try again later"; not an anomaly. |
| `DeltaError::SafeMode` from `check()` | Three deterministic failures; wait for the 6-hour cooldown or call `reset_safety()`. |
| New version gets rolled back | Startup success was never confirmed. Call `confirm_startup_success()` early, and exit the process after launching the update. |
| Update "silently disappears" | The user declined elevation. The old version is relaunched; see `tdx_update.log` next to the install directory (and in the work directory). |
| Extra `tdx_update.ps1` / `tdx_update.sh` in the work directory | Normal: scripts don't delete themselves; the next update reclaims them. |
| `Apps & Features` / `dpkg` still shows the old version | In-place exe replacement bypasses installer/package metadata. Use the full-download path for managed deployments. |

## License

This project is licensed under the [MIT License](LICENSE).

Copyright © 2026 丘屿知宁 (zuoqiuning)
