[![CI](https://github.com/zuoqiuning/tauri-delta-updater/actions/workflows/ci.yml/badge.svg)](https://github.com/zuoqiuning/tauri-delta-updater/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/tauri-delta-updater.svg)](https://crates.io/crates/tauri-delta-updater)
[![docs.rs](https://img.shields.io/docsrs/tauri-delta-updater)](https://docs.rs/tauri-delta-updater)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

# tauri-delta-updater

A high-performance, secure, and fault-tolerant delta update engine for Tauri v2 applications.

### Is this for you?

Seven questions, asked before the long docs:

1. **Do you ship only the main executable?** Yes → single-file patches are the sweet spot. No (`externalBin` / `resources`) → configure `bundle_endpoint` and use whole-app deltas.
2. **Which platforms?** Windows exe/NSIS, macOS `.app`, and Linux bare binaries/AppImage are supported. Linux `.deb` / `.rpm` installs should stay with the package manager; use this engine for notification only.
3. **Can your app exit when the update is ready?** Yes → three lines (`install()` + exit). Not yet → use `prepare()` and exit when your UI is done.
4. **Do you need signed manifests, downgrade protection, deterministic rollback?** Yes → this engine. If you only want a smaller download and nothing else, a transparent layer over the official updater is simpler.
5. **Already running a stable official-updater flow?** Read “Migration cost vs benefit” below before switching; the frontend contract changes.
6. **Do you ship on macOS?** Whole-app swaps are code-complete, but Gatekeeper/notarization cannot be proven in CI. Budget one real update on your signed + notarized build before shipping.
7. **Can you test one full update per platform?** The swap script waits for the old PID to disappear. Whether your `exit(0)` leaves a single-instance lock behind is only observable on a real machine.

**Skip it if** your installer is small (roughly under 30 MB) and you release less than once a month: full downloads plus a CDN are simpler, and the extra moving parts (rollback scripts, elevation, installer conventions) are not worth the maintenance.

**Download size is the entry ticket, not the finish line.** The part users
actually feel is what happens after the bytes arrive: replacing a running
executable, surviving a power loss mid-swap, and keeping the installed app
launchable. Most delta options stop before that line.

| | Smaller download | Owns replacement / rollback | Native binary | Frontend-only |
|---|---|---|---|---|
| official `tauri-plugin-updater` | no | yes (the installer) | yes | no |
| `tauri-plugin-updater-delta` | yes | **no** — hands the patch back to the official updater | yes | no |
| `tauri-plugin-hot-update` | n/a | no | no | yes |
| **this engine** | yes | yes (script, backup, repair, safe mode) | yes | no |

If you only want smaller downloads and are happy to keep the official updater
for installation, the transparent layer is the lower-risk choice. If you want
one component to own the whole update, keep reading.

## Quick start

Two entry points, one engine. Pick one — you do not need both.

### A. Tauri app — plugin (recommended)

**One command (with diff first).** Install the cargo subcommand, then run it
from the app root. It writes `plugins.delta-updater`, the capability, and the
`src-tauri/Cargo.toml` dependency; it prints the one Rust line to add.

```bash
cargo install tauri-delta-updater --features generator
cd my-app
cargo delta init \
  --endpoint https://cdn.example.com/latest.json \
  --pubkey "RWQ..."              # dry-run: prints a unified diff, writes nothing
cargo delta init ... --apply     # writes the files
cargo delta init ... --check     # CI guard: non-zero if config/capability/dependency missing
```

`init` writes the plugin as a **git dependency by default**: the plugin crate is
not published on crates.io yet, and a version-only line would make `cargo build`
fail. Once it is published, pass `--registry` to write `"<version>"` instead.

Already on the official updater? `cargo delta migrate` copies
`plugins.updater` into `plugins.delta-updater` and keeps the official plugin as
the fallback by default. Add `--remove-official` to remove its config and
`updater:default` permission. Both commands print a diff unless `--apply` is
passed.

Keep `--check` in CI so the integration cannot silently rot:

```yaml
- name: Verify delta-updater integration
  run: |
    cargo install tauri-delta-updater --features generator
    cargo delta init --check
```

`--check` also reports whether `plugins.updater` is still present (dual-stack)
or absent (single-stack, full fallback owned by `install_full`).

> [!IMPORTANT]
> **Replacing the official updater is a decision, not an automatic default.**
> This plugin can cover both delta and full-installer paths, but then installer
> failures (Windows NSIS/MSI) are owned by the installer, and macOS whole-app
> updates need an Apple Developer signing/notarization setup to validate in
> production. If you only want smaller downloads, keeping the official updater
> and using this engine as the delta layer is the lower-risk default.

`src-tauri/Cargo.toml`:

```toml
[dependencies]
# Not on crates.io yet; the git form works today and is what `cargo delta init` writes.
tauri-plugin-delta-updater = { git = "https://github.com/zuoqiuning/tauri-delta-updater" }
# Published later: tauri-plugin-delta-updater = "0.2"
# Local checkout:   tauri-plugin-delta-updater = { path = "../plugin" }
```

`src-tauri/capabilities/default.json`:

```json
{ "permissions": ["delta-updater:default"] }
```

`src-tauri/tauri.conf.json` — same shape as the official updater plugin:

```json
{
  "plugins": {
    "delta-updater": {
      "endpoints": ["https://cdn.example.com/latest.json"],
      "pubkey": "RWQ... (pinned, never downloaded)",
      "bundle_endpoint": "https://cdn.example.com/latest.bundle.json"
    }
  }
}
```

Optional keys (all have safe defaults): `work_dir` (override the user-writable
cache directory), `wait_timeout_secs` / `confirm_timeout_secs` (replacement
timings), `proxy` (`http://127.0.0.1:7890`), `headers` (extra HTTP headers for
private CDNs, e.g. `{"Authorization": "Bearer ..."}`), `allow_legacy_signatures`
(`false` rejects pre-0.7 minisign signatures), `fallback_policy`
(`transport` / `any`), `install_mode` (`passive` / `basicUi` / `quiet`),
`allow_insecure_transport`, `bundle_type`, `install_id`,
`install_on_next_launch` (install the staged update on the next start; off by
default), `additional_pubkeys` (extra trusted keys during rotation), and
`trust_policy` (`any` default / `all` requires every trusted key to sign).

`endpoints` is an array: the first URL is the primary manifest, the rest are
backups. With the default `fallback_policy: "transport"`, the updater switches
only on connection failures, timeouts, 5xx or 429; `404` does **not** switch.
`fallback_policy: "any"` switches on any HTTP/transport failure. A signature
failure always aborts — failover can never bypass signature verification.

`src-tauri/src/lib.rs` — the only Rust you write:

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_delta_updater::init_from_config())
    .run(tauri::generate_context!())?;
```

`init_from_config()` reads `plugins.delta-updater`; when that key is missing it
falls back to `plugins.updater`, so an app migrating from the official plugin
keeps its existing `endpoints` / `pubkey` untouched. The `init(UpdateConfig)`
form remains available when the config has to be computed at runtime.

Frontend:

```ts
import { invoke, Channel } from '@tauri-apps/api/core';

const events = new Channel();
events.onmessage = (e) => {
  // e.stage: checking | downloading | applying | ready | up_to_date
  //          | deferred | fallback | repairing | safe_mode | cancelled
  // deferred = a newer version exists, but this install is outside the rollout
  if (e.stage === 'downloading') setPercent(e.downloaded, e.total);
};

await invoke('plugin:delta-updater|install', { onEvent: events });
// Need to control when the app exits (save drafts, show a prompt first)?
// `prepare` downloads and merges but does not touch the running exe:
await invoke('plugin:delta-updater|prepare', { onEvent: events }); // → { kind: "ready" }
// ... then, when the UI is ready:
await invoke('plugin:delta-updater|launch');                       // → { kind: "updated" }, process exits
// The user quit before launch? The staged artifact is persisted; on the next
// start, verify and execute it (no network):
const pending = await invoke('plugin:delta-updater|pending');      // null | { version, kind, created_at }
await invoke('plugin:delta-updater|install_pending');              // → true, process exits
// 增量不可用时，回退路径也不需要官方插件：
await invoke('plugin:delta-updater|install_full', { onEvent: events });
```

The plugin confirms startup before any window opens, streams stage events, and
exits the process once the swap is ready. Errors arrive as
`{ code, message, retryable, security }` — branch on `code`, never on `message`.

Typed bindings (optional) live in `plugin/guest-js` and build to `plugin/dist-js`:
`check`, `install`, `prepare`, `launch`, `downloadFull`, `installFull`,
`pending`, `installPending`, `diagnose`, `cancel`, `resetSafety`, plus
`UpdateEvent` / `UpdateResult` / `ErrorDto` types. The npm package is **not published yet**; until it is, install
it from a checkout (`npm install /path/to/repo/plugin`) or copy `dist-js`.

> [!WARNING]
> **Single-instance apps: test the exit path once per platform.** The plugin
> exits with `app.exit(0)` after a short delay so the replacement script can
> take over the files. If you also run `tauri-plugin-single-instance`, verify
> that its socket/lock is released; a stale lock makes the relaunched build
> look like a second instance.

### Replacing the official `tauri-plugin-updater`

The official updater can be removed entirely; this plugin now covers both the
delta path and the full-installer fallback.

| Step | Remove | Add |
|---|---|---|
| Dependency | `tauri-plugin-updater` | `tauri-plugin-delta-updater` |
| Rust registration | `.plugin(tauri_plugin_updater::Builder::new().build())` | `.plugin(tauri_plugin_delta_updater::init_from_config())` |
| `tauri.conf.json` | — | keep `plugins.updater` as-is (automatic fallback), or rename the key to `plugins.delta-updater` |
| Capabilities | `"updater:default"` | `"delta-updater:default"` |
| Frontend | `import { check } from '@tauri-apps/plugin-updater'` | `invoke('plugin:delta-updater|check')` / `...|install` / `...|install_full` |
| Fallback | `update.downloadAndInstall()` | `invoke('plugin:delta-updater|install_full', { onEvent })` |

The wire format is unchanged: the same `latest.json`, the same `pubkey`, and the
same `platforms[].signature`. Known boundaries:

- Linux `.deb` / `.rpm` are refused (`full_install_failed`) — let the package
  manager update them; AppImage is fully supported.
- A failed Windows NSIS/MSI install cannot be rolled back by us; the installer
  owns that. We surface `full_install_failed` and the log path.
- Windows installer arguments follow Tauri's own conventions (`/P /UPDATE /R`
  for NSIS, `/passive /promptrestart AUTOLAUNCHAPP=True` for MSI), so updates
  are not mistaken for first-time installs. `install_mode` switches to
  `basicUi` (no `/P`) or `quiet` (`/S` / `/qn`); the default stays `passive`.
  `quiet` cannot elevate by itself, so it only works for per-user installs or
  when the process is already elevated.
- `endpoints` may use the official updater's `{{target}}` (windows / darwin /
  linux), `{{arch}}`, `{{current_version}}` and `{{bundle_type}}` placeholders.
  The first three expand automatically; `{{bundle_type}}` requires an extra
  `"bundle_type": "nsis"` (or `msi` / `appimage` / `app`) in the plugin config —
  without it startup fails with a clear error instead of sending a URL that
  contains literal braces.
- `install_full` returns `Updated`, then the plugin exits — the installer is
  already running and will relaunch the app.

**Migration cost vs benefit.** The migration is mostly a **frontend rewrite**:
this plugin streams `{ stage, ... }` events over a `Channel`, while
`@tauri-apps/plugin-updater` uses a `(downloaded, total)` callback — the
progress UI has to be adapted, not just re-imported. Checklist:

1. replace the capability with `delta-updater:default`;
2. rewrite the progress listener to stage events;
3. exit after `Updated` / `RepairRequired` (the script waits for the old process);
4. set `bundle_endpoint` if you ship sidecars or resources.

In exchange you get signed manifests (the official updater signs only binaries),
semantic-version downgrade protection, deterministic rollback with crash
recovery, safe mode, and a built-in full-installer path that replaces the
official updater entirely. If you only need smaller downloads and your current
flow is stable, keeping the official updater is the lower-cost choice.

**Do not register both plugins unless you own the hand-off.** Two plugins fetch
the manifest independently, so the manifest can be read twice (and change
between reads), and this engine's manifest signature / version binding does not
propagate into the official installer path. Pick one: this plugin's
`install_full` already covers the fallback.

### B. Any Rust host — library

```rust
use tauri_delta_updater::{DeltaUpdater, UpdateResult};

// Once per launch, before anything else. A no-op unless the previous run
// actually swapped in a new version, so it is safe to call unconditionally.
let _ = tauri_delta_updater::PlatformUpdater::confirm_startup_success_if_pending()?;

let updater = DeltaUpdater::auto(
    "https://cdn.example.com/latest.json",
    "BASE64_PUBLIC_KEY",
    env!("CARGO_PKG_VERSION"),
)?;

match updater.install().await? {
    UpdateResult::Updated { .. } => std::process::exit(0),
    // A previous whole-app swap died halfway: the repair script is already
    // running. Exit so it can restore the backup and relaunch the old version.
    UpdateResult::RepairRequired { .. } => std::process::exit(0),
    UpdateResult::UpToDate { .. } => {}
    UpdateResult::Deferred { .. } => { /* newer version exists, but this install is not in the rollout yet */ }
    UpdateResult::RequiresFullDownload { .. } => { /* install_full(), or hand off to the official updater */ }
    UpdateResult::SafeMode => { /* prompt for a manual install */ }
    // `UpdateResult` is non_exhaustive: new outcomes won't break your build.
    _ => {}
}
```

The library derives the platform, a user-writable work directory, and the HTTP
client; "no delta available" is a normal outcome, not an error you assemble
yourself.

Before integrating anything, see what a patch would actually save you — no
network or signing key needed:

```bash
cargo run --example simulate_update -- old.exe new.exe target/delta-sim
```

### Three things that are easy to miss

1. **Confirm startup** (library path) — call
   `PlatformUpdater::confirm_startup_success_if_pending()` once at launch.
   Without it, the replacement script waits 60 seconds and rolls the update back.
   The plugin does this for you. If your app can be "up but unhealthy" (DB
   migration, license check), use `confirm_startup_success_with(|| ...)`: a
   failed check writes `health_check_failed` and the script rolls back
   immediately instead of waiting for the timeout.
2. **Exit after `Updated` / `RepairRequired`** — the script waits for the old
   process to exit before it can replace files. `install_and_exit()` or the
   plugin handles it.
3. **Handle `RequiresFullDownload`** — either call `install_full()` (this plugin
   downloads, verifies and runs the platform installer for you), or pass `url` /
   `reason` to your installer flow. If you call `check()` first,
   `info.full_download()` returns `{ version, url, signature }` as a typed value.
   This path is taken for an AppImage whose install directory is read-only, and
   for a macOS `.app` when no bundle manifest is configured.

## Overview

`tauri-delta-updater` is a **standalone incremental update engine** for Tauri v2 applications: a Rust library plus a Tauri plugin that own the entire update flow. Instead of downloading the full installer on every release, clients download a small binary patch and merge it locally. When a delta is not available (no matching patch, read-only AppImage, macOS `.app` without a bundle manifest), the engine downloads, verifies and runs the full installer itself — `tauri-plugin-updater` is not required.

**Positioning: one engine, not a download accelerator.** The manifest format stays compatible with the official updater (`latest.json`, `pubkey`, `platforms[].signature`), so migration is a dependency swap, but the engine also replaces the replacement/rollback responsibilities the official plugin used to own. Falling back is not a failure; correctness comes first, and every fallback is a typed reason you can display or `match` on. If you keep the official plugin instead, you own the hand-off — see “Do not register both plugins” above.

Platform support:

| Platform | Status | Notes |
|---|---|---|
| Windows | **Stable** | Single-file exe is the sweet spot; the replacement script runs on PowerShell, handles elevation, special-character install paths, and relaunches through `explorer.exe` with **only the target path** (explorer ignores extra arguments) so the app does not inherit the admin token. Signed builds keep their Authenticode guarantee: a patched artifact that loses it is rejected before any file is touched |
| macOS | **Stable** (whole-app) | **Whole-app bundles** (requires `bundle_endpoint`; production validation needs an Apple Developer signing + notarization setup): the complete `.app` is swapped as a directory (old bundle renamed to `.<App>.app.tdx-backup`, staged tree moved in), so every signed file and symlink stays byte-identical and the signature/notarization remains valid; the reconstructed bundle is `codesign --verify --deep --strict`'d before any file is touched. The app directory must be writable; otherwise the old version keeps running. **Single-file patches** inside a `.app` automatically fall back instead of silently ad-hoc re-signing (bare binaries are experimental) |
| Linux | **Stable** | Bare binaries and **AppImages** (the `.AppImage` file itself is patched in place, `chmod +x`, then relaunched; only a read-only install directory falls back). `.deb` installs elevate via passwordless `sudo` or `pkexec`, then **drop privileges back** via `runuser` (falling back to `su`) and return file ownership to the original user |

> [!IMPORTANT]
> **macOS whole-app updates need a real signed build.** CI executes the directory-swap script
> against synthetic bundles, which verifies file hashes, rollback and relaunch but cannot prove
> Gatekeeper/notarization behaviour for your Developer ID. Validate the whole flow once on a
> signed and notarized app installed under `/Applications` before shipping.
>
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
- **Authenticode guard (Windows)** — if the running build is signed and the patched artifact is not, the update is blocked with `DeltaError::AuthenticodeUnsigned`. A hash-correct but unsigned binary would be refused by AppLocker/WDAC and "update" would brick the app. Unsigned builds (dev/portable) are unaffected.
- **Built-in full-installer path** — `download_full()` downloads `platforms[].url`, verifies `platforms[].signature` with mmap (no 200 MB read into RAM), and `install_full()` runs it: NSIS/MSI with elevation on Windows, `.app.tar.gz` unpack + directory swap on macOS, AppImage replacement on Linux. The official updater is no longer required for the fallback path.
- **Signed manifests** — Ed25519 (minisign) signatures on `latest.json` prevent man-in-the-middle tampering. Signature failures are reported as a distinct error so tampering can be told apart from a corrupted download.
- **Downgrade protection, three layers, on by default** — (1) the **manifest itself is signed**, so an attacker cannot repoint it at an old installer without breaking the signature; (2) after verification, `manifest.version` is compared against the running version with **semantic versioning**, and a non-newer manifest returns `UpdateOutcome::UpToDate` instead of handing the host an old installer URL; (3) the generator's version binding in the trusted comment catches publisher-side "signature from A on manifest B" mistakes. The first two layers block replay; the third is defense in depth. None of them depends on an optional switch.
- **Typed fallback reasons** — `FallbackReason::NoMatchingPatch` / `EnvironmentUnsupported`, instead of a free-form string that forces UI code to match Chinese prose.
- **Download-cost visibility** — `generate` stamps `platforms[].size` into the signed manifest, so `CheckOutcome::patch_size_ratio()` returns `patch_size / installer_size` before any download. **Lower is better**: `0.1` means the patch is 10% of the installer; values near `1.0` mean the delta saves little and the full installer may be faster. Manifests without `size` return `None` instead of a guessed number.
- **Local verification harness** — `simulate_update(old, new, work_dir)` runs generate → apply → dual-hash verification with no network or signing key, so you can see the real patch ratio before integrating.
- **Environment diagnosis** — `DeltaUpdater::diagnose()` reports work-directory/install-directory writability, platform eligibility, safe-mode state, the exact log path, `last_rollback` (why the last replacement script rolled back, e.g. `update_failed` / `startup_timeout`), `events_path` for the structured stream below, and `user_message()` for a ready-to-display one-line summary. Linux builds also report whether a privilege-drop tool (`runuser` / `su`) was found; Windows builds report `authenticode` (whether the running exe is trusted) and `authenticode_check_skipped` (true when the check could not reach a conclusion, e.g. offline with no cached chain).
- **Multi-hop delta chains** — when no direct patch exists, `update()` applies up to 3 verified hops (0.1 → 0.2 → 0.3), checking each intermediate result against its signed `target_hash` before it becomes the next hop's input. The planner prefers a direct patch, then the cheapest viable chain, then the full installer; a chain whose total size exceeds 80% of the installer falls back to full. The generator keeps only chainable history (`MAX_DELTA_CHAIN_HOPS`) so manifests don't grow forever.
- **Key rotation without breaking old clients** — `additional_public_keys` / `additional_pubkeys` extend the trusted set; `trust_policy: "any"` accepts a signature from any of them, `"all"` requires every key to sign (dual-sign window). `.sig` files may contain multiple minisign blocks, and `generate --secondary-private-key` signs both the manifest and the installer with both keys.
- **Signed percentage rollout** — `generate --rollout 20` writes `platforms[].rollout` into the signed manifest; clients bucket on `sha256(install_id + version) % 100`. Misses get `Deferred` and download nothing; `0` pauses the release, `100` behaves like a normal release. The install ID persists in `tdx_install_id`, or pass `UpdateConfig::install_id(...)`. No server-side bucketing and no second manifest.
- **Forced minimum version** — `generate --min-version 0.1.5` writes a signed floor into the manifest. Clients below it ignore the rollout percentage and must update, so a bad release can be reversed without waiting for the bucket.
- **Deterministic startup handshake** — the replacement script writes `staged`, the new process writes `booting`, and `confirm_startup_success_with(|| ...)` writes `committed` only after the host's health check passes. Failure writes `health_check_failed` and rolls back immediately; `diagnose().startup_state` / `last_rollback` distinguish "never started", "crashed before commit", "started but unhealthy" and "committed".
- **Startup doctor** — `diagnose().warnings()` returns actionable configuration gaps; the plugin prints them to the startup log on first run (work directory not writable, macOS `.app` without a bundle manifest, Linux without `runuser` / `su`).
- **Structured rollback events** — replacement scripts append one JSON line per rollback (and the repair path) to `tdx_events.jsonl` (`{ event, reason, time }`). `diagnose().events_path` points at the file and `diagnose().script_events` already returns the last 20 parsed entries as typed `ScriptEvent` values, so the UI can explain what happened without parsing a free-form log. Malformed or future lines are ignored.
- **Optional structured events** — enable the `tracing` feature to receive `delta.check`, `delta.patch.matched`, `delta.patch.rejected`, `delta.fallback`, `delta.safety_mode.entered`, `delta.safety_mode.cooldown_active`, and `delta.patched` events; zero cost when disabled.
- **Zero-config constructor** — `DeltaUpdater::auto(url, pubkey, version)` derives the platform, user work directory, and HTTP client.
- **Multi-endpoint failover** — `endpoints` accepts backups; the switch policy is explicit and signature failures always abort. Custom `headers` cover private CDNs and auth tokens.
- **UI-level default path** — `install()` returns `UpdateResult::{Updated, RepairRequired, UpToDate, Deferred, RequiresFullDownload, SafeMode}`; the advanced `update()` still returns the full `UpdateOutcome` when you need patch-level control.
- **Platform-scoped patches** — patches are grouped per platform key, so an x64 client never picks up an arm64 patch.
- **Staged rollout APIs** — `check()` to query without downloading, `prepare()` to download and merge without launching the swap, and `Staged::launch()` to trigger the swap when you're ready.
- **Cross-restart staged install** — if the user quits after `prepare()` but before `launch()`, the staged artifact is persisted in `tdx_pending.json`. On the next start, `pending_update()` / `install_pending()` re-verify it (strictly newer version, script and artifact inside the work directory, SHA-256 of the merged exe) and run the swap without touching the network. The plugin exposes `pending` / `install_pending`, and `install_on_next_launch` runs it automatically at startup (off by default).
- **Auto-rollback with a real restart** — if the new version fails to write its startup flag within the timeout, the script stops the new process, restores the backup, **and relaunches the old version**. The same recovery runs when elevation is declined or a copy fails.
- **Crash-safe replacement** — if a previous run was interrupted between "rename exe → `.bak`" and the swap, the next script run restores the backup first; a leftover `.bak` next to an intact target is never restored.
- **Safe mode with automatic recovery** — after 3 consecutive *deterministic* failures (bad signature, hash mismatch, disk full...), delta updates pause. Transient network errors don't count. After a 6-hour cooldown a single new attempt is allowed automatically; hosts can also call `reset_safety()`.
- **Concurrency protection** — a cross-process file lock prevents parallel updaters from corrupting state. Lock contention surfaces as `DeltaError::UpdateInProgress`, not a generic worker error.
- **Network resilience** — HTTP Range resumption (only trusted with `206 Partial Content`; a `200` answer restarts the download cleanly), **resumable across app restarts** (the partial patch is kept and tagged with the signed `patch_hash`, so a leftover from another version is discarded rather than concatenated), and exponential-backoff retries **with jitter** — applied to the manifest/signature requests too, so a single CDN 502 does not surface as "check failed".
- **Injectable HTTP client** — pass your own `reqwest::Client`, or build it with `client_builder()` which installs the TLS backend for you.
- **zstd-compressed patches** — raw bsdiff output is zstd-compressed at generation time and sniffed by magic on apply, so legacy uncompressed patches keep working.
- **Bounded downloads and bounded expansion** — the patch limit derives from the signed `patch_size` (+1 MB); manifests are capped at 10 MB. Compressed patches are streamed to a temp file and memory-mapped, with a decompressed-size ceiling, so a 2 GB target can't turn into multi-GB RSS.
- **Memory-mapped delta** — old files and patches are read via read-only `mmap`; the merged result is written straight to disk.
- **Scoped cleanup** — only artifacts this project created are removed. An unrelated `notes.txt.bak` in the install directory survives.
- **Crash-tolerant auto cleanup** — patch, staging, script, expanded-patch temp, and the target's own `.bak` are reclaimed on the next update.
- **Whole-app bundles** (`core::bundle` + `UpdateConfig::bundle_manifest_url`) —
  diffs an entire app directory (main exe + sidecars + resources) through a
  deterministic, path-sorted archive. Every file is hash-checked before archiving
  and after extraction, and archive paths are rejected if they are absolute or
  contain `..`. Opt in by passing the signed `latest.bundle.json` URL; the updater
  then downloads the directory patch, extracts a verified staging tree, and
  generates a swap script that backs up, replaces, and rolls back as a unit.
  Windows replaces changed files one by one (and deletes removed ones); macOS
  swaps the whole `.app` directory, preserving symlinks, executable bits and
  the code signature. Other platforms fall back to the full download.
- **Interrupted-swap repair** — if a previous swap died halfway, the next call
  returns `UpdateOutcome::Repair` / `UpdateResult::RepairRequired` **without any
  network request**: a repair script removes files added by the failed update,
  restores the backup, and relaunches the old version. `diagnose().needs_repair`
  and `last_rollback` let the UI explain what happened.

## Security boundaries

The official `tauri-plugin-updater` verifies the installer bytes but **not the
manifest** ([CVE-2026-95625](https://nvd.nist.gov/vuln/detail/cve-2026-95625)).
An attacker who can serve the update endpoint can pair a higher unsigned
`version` with an older, validly signed installer and force a downgrade without
the signing key. This engine signs the manifest itself, then applies semantic
version monotonicity after verification — a replayed old manifest lands on
`UpToDate` and never hands the host an old installer URL.

### Threat matrix

| Attack | official `tauri-plugin-updater` | `tauri-plugin-updater-delta` | this engine |
|---|---|---|---|
| CDN serves a newer-version manifest pointing at an older, validly signed installer | [CVE-2026-95625](https://nvd.nist.gov/vuln/detail/cve-2026-95625): the manifest is never signed, so the unsigned `version` can be raised while keeping an old signed installer | Delegated to the official check, same outcome | Blocked: the **manifest itself is signed**, `version` is bound in the trusted comment, and `is_newer_than` runs in Rust |
| Frontend XSS flips the downgrade switch through IPC | [CVE-2026-95624](https://nvd.nist.gov/vuln/detail/cve-2026-95624): `allowDowngrades` was reachable from JS | Inherits the official check | No such IPC parameter: downgrade rejection is a Rust-side invariant, not an option the webview can set |
| Patch bytes replaced in transit | n/a (no patch) | Local hash check | Double SHA-256 + the signed manifest anchors both hashes |
| Power loss or crash mid-swap | The installer owns it | Does not own replacement | `.bak` recovery, a repair script, safe mode, and a `staged → booting → committed` state pointer |
| Process starts but is unhealthy (DB migration, license check) | Not observable | Not observable | Host health check blocks the commit and writes `health_check_failed`, so the script rolls back immediately |

The engine's security model is described above (signed manifests, version
binding, dual hashes, typed artifacts). These boundaries are deliberate and
worth knowing before an audit:

- **The startup success flag is plaintext.** Any process that can write the
  install directory or work directory can forge it and make the script keep the
  new version. That is inside the "the machine is already compromised" threat
  model; it is not a security boundary for the updater.
- **Legacy minisign signatures are accepted.** `verify_manifest_signature`
  accepts the legacy format (`allow_legacy = true`). Our generator is pinned to
  minisign 0.7.9, which emits pre-hash signatures (asserted by a unit test), so
  in practice the compatibility window only matters for third-party pipelines.
  Removing `allow_legacy` changes signature-validation behavior and is therefore
  a MINOR release per `VERSIONING.md`.
- **Plaintext HTTP is allowed by default for compatibility.** Non-loopback
  `http://` endpoints are flagged by `diagnose().insecure_transport`; setting
  `allow_insecure_transport(false)` rejects them at construction. Signatures
  still protect integrity, but URLs, versions and update cadence are visible to
  the network. Use HTTPS in production.
- **The Windows Authenticode check is offline** (`WTD_CACHE_ONLY_URL_RETRIEVAL`).
  A trusted original whose artifact is unsigned is blocked; when the cache has
  no chain for an otherwise valid artifact, the check is skipped rather than
  blocking every offline machine. `diagnose().authenticode` exposes the current
  state; `diagnose().authenticode_check_skipped` tells you when the result was
  skipped (offline / chain not cached) rather than an actual "unsigned" verdict,
  so the residual risk is observable instead of silent.
- **Scripts run with the privileges they request.** UAC / polkit elevation
  happens after the app exits; the trust anchors are the signed manifest and
  the staged artifact's hash, not the script text.

## Patch size expectations

Patch size depends on **how similar the two binaries are**, not on their size:

- **Very similar** (only resources or metadata changed): patches can be a few hundred bytes.
- **Typical** (a couple of modules rebuilt, new build timestamps): expect tens of KB to a few MB.
- **Very different** (full recompile with a different toolchain): the patch can approach the size of the full installer — `generate --max-patch-ratio` can skip those.

A patch is always verified against both hashes, so a poor-ratio patch is still *correct*, just not smaller.

### Memory and file size limits

`qbsdiff` needs the old and new files available while computing or applying a patch; its working memory is roughly `max(17 × old_size, 9 × old_size + new_size)`, which is why large files are rejected rather than attempted. Single files larger than `MAX_INCREMENTAL_FILE_BYTES` (2 GB) are rejected with `DeltaError::IncrementalNotApplicable`; **generating** a patch additionally rejects any baseline whose estimated working set exceeds 4 GiB with `DeltaError::PatchWorkingSetTooLarge`, so `generate` skips it instead of aborting on an allocation failure. Old files and patches are memory-mapped, the merged result is streamed to disk, and decompressed patch size is capped at `2 × old_size + 128 MB`.

Each patch task runs on a dedicated thread that reserves a **512 MB stack** (64 MB on 32-bit targets; bsdiff recursion depth scales with file size). That is virtual address space, not RSS, but it means **do not run two patch generations concurrently in one process**. Applying a patch is the same constraint.

To measure your own app: build two releases and run
`cargo run --example simulate_update -- old.exe new.exe target/delta-sim`.
Compare `patch_size` with the installer size; `generate --report` writes the
same ratio per baseline into the release report. Very similar builds produce a
few hundred bytes; a full recompile with a different toolchain can approach the
full installer size (skip those with `--max-patch-ratio`).

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
  "min_version": "0.1.0",
  "notes": "Update notes",
  "pub_date": "2026-11-04T16:35:22.903Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6...",
      "url": "https://example.com/Sereno_0.1.1_x64-setup.exe",
      "size": 20971520,
      "rollout": { "percentage": 20 }
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

`platforms[].size` is optional and written by `generate`: the installer size in
bytes, covered by the manifest signature. It lets the client compute
`patch_size_ratio()` before downloading anything, without guessing.

`platforms[].rollout` is optional and signed. `"percentage": 20` admits ~20% of
installs (stable `sha256(install_id + version) % 100`); `0` pauses the release
and `100` is a normal full release. A client outside the bucket gets
`Deferred` and downloads nothing. Old manifests without the field are treated
as 100%.

`min_version` is optional and signed. A client below it **ignores the rollout
percentage and must update** — the emergency brake for pulling users off a bad
release. Omit it to clear the floor on the next publish.

## Usage guide

### 1. Add the dependency

```toml
[dependencies]
tauri-delta-updater = "0.2"
```

Inside a Tauri app, `tauri-plugin-delta-updater` (the `plugin/` crate) wraps this
one and exposes the same engine as commands and events, so you never touch the
types below. Continue here if you want the library directly.

The library has **no default features**. If you need the `generate` CLI, enable the
publisher tooling explicitly:

```toml
tauri-delta-updater = { version = "0.2", features = ["generator"] }
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
    UpdateResult::UpToDate { .. } => { /* nothing to do */ }
    UpdateResult::Deferred { percentage, .. } => {
        // A newer version exists, but this install is outside the rollout.
        // Show "will update automatically" rather than "already up to date".
        let _ = percentage;
    }
    UpdateResult::RequiresFullDownload { url, reason } => {
        // hand off to tauri-plugin-updater; `reason` explains why
        let _ = (url, reason);
    }
    UpdateResult::SafeMode => { /* prompt for a manual install */ }
}
```

For a progress bar, use `install_with_progress(|downloaded, total| ...)` — it
returns the same `UpdateResult`, so you do not have to drop down to the advanced
API just to show a progress bar. The callback covers the **download** only; once
`downloaded == total`, the patch is being merged and verified, so switch the UI
to an "installing" state.

`install_and_exit()` does the same but exits automatically after a successful
update.

### Returning the result to the frontend

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

### Cancellable updates (session)

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
> can never match the installed `app.exe`. For AppImage targets, the baseline is
> the `.AppImage` file itself (that is what the client replaces).

```bash
# Build the publisher once (the `generate` binary is behind a feature):
cargo build --release --features generator --bin generate

# Generate a minisign-compatible signing key once:
cargo tauri signer generate -w ~/.tauri/myapp.key
```

`--old-dir` contains your **previous app executables**, not installers, and the
version is parsed from the filename — for example:

```text
old_binaries/
  MyApp_0.1.0_x64.exe
  MyApp_0.1.1_x64.exe
```

```bash
./target/release/generate \
  --old-dir ./old_binaries \
  --app-binary ./target/release/Sereno.exe \
  --installer ./target/release/Sereno_0.1.1_x64-setup.exe \
  --version 0.1.1 \
  --base-url https://your-cos-endpoint.com \
  --private-key "$TAURI_SIGNING_PRIVATE_KEY" \
  --notes "What's new" \
  --output-dir ./dist \
  --archive-dir ./old_binaries \
  --report ./dist/release-report.json \
  --fail-on-empty
```

`--archive-dir` closes the loop: after a successful release it copies the current
main executable into the baseline directory (keeping the original name when it
already contains version + arch, otherwise writing `App_0.1.1_x86_64.exe`). The
next release can reuse the same `--old-dir` without any manual file shuffling.
`--report` writes a machine-readable summary of every baseline (patch size, ratio,
skip reason), and `--fail-on-empty` turns "no patch was produced" into a CI
failure instead of a silently full-download-only release.

`generate` also **retains chainable history**: patch entries from previous
manifests are kept when they form a path to the new version within
`MAX_DELTA_CHAIN_HOPS` (default 3), and everything else is pruned. That lets a
client on `0.1.0` reach `0.3.0` through `0.1 -> 0.2 -> 0.3` without the full
installer. Two consequences for the release pipeline:

- **Keep old patch files on the CDN.** The manifest keeps pointing at them; deleting them turns a chain into a download failure.
- **Ship the old executables to `--old-dir`.** `generate` can only add a new edge when it has the binary that edge starts from.

**Whole-app bundles.** If the app ships sidecars or resources, add
`--new-app-dir` and one or more `--old-app-dir` (the directory name must contain
the version, e.g. `build/v0.1.1`):

```bash
./target/release/generate ... \
  --new-app-dir ./build/app-0.1.2 \
  --old-app-dir ./build/app-0.1.1
```

This also writes `latest.bundle.json` + `.sig` next to `latest.json`. Every
`bundles[]` entry carries `base_files` / `target_files` (path, size, SHA-256) and
points at a directory-level patch.

Clients opt in by pointing at the signed bundle manifest:

```rust
let updater = DeltaUpdater::new(
    UpdateConfig::new("https://cdn.example.com/latest.json", "BASE64_PUBLIC_KEY")
        .current_version("0.1.1")
        .bundle_manifest_url("https://cdn.example.com/latest.bundle.json"),
)?;

match updater.update().await? {
    UpdateOutcome::Bundled { staged } => {
        println!("whole app -> {}", staged.manifest().version);
        // Exit the process so the swap script can replace every file.
        std::process::exit(0);
    }
    _ => {}
}
```

The updater verifies the local layout, rebuilds the old archive, applies the
patch, and extracts a hash-checked staging tree. On Windows the generated script
backs up each changed file, copies the new bytes, deletes files removed in the
target version, and restores the whole backup directory if any step fails.
On macOS the script swaps the entire `.app` directory atomically, so the
reconstructed bundle keeps its code signature and notarization; the app directory
must be writable. Where whole-app swaps are not supported (Linux, or macOS without
a real `.app` bundle), the client **still uses the single-file patch when one is
available** and only falls back to the full installer when neither path works. The
lower-level `core::bundle` APIs remain available if you want to drive the staging
step yourself.

Key arguments:

| Argument | Meaning |
|---|---|
| `--old-dir` | Directory of **old main executables**. Every matching version gets a patch. |
| `--old-binary` | One explicit baseline (repeatable), merged with `--old-dir`. Invalid explicit baselines fail loudly. |
| `--app-binary` | The new main executable (patch target / `target_hash`). |
| `--installer` | The new full installer; used for the full-download URL and its signature. Defaults to `--app-binary` for portable single-file apps. |
| `--version` | Target version; a leading `v` is normalized. |
| `--platform` | Optional platform key; otherwise inferred from the filename (`windows-x86_64`, `linux-x86_64`, `darwin-aarch64`...). Inference failure is an error, never a silent `windows-x86_64` default. |
| `--max-patch-ratio` | Skip patches larger than this fraction of the installer (default `1.0`: only skip when the patch has no benefit). |
| `--zstd-level` | Patch compression level, 1–22 (default 19). Lower it to 15–17 when CI time matters for large patches. |
| `--existing` | Previous `latest.json`, to preserve other platforms' entries and patches. |
| `--password` | Private-key password (or `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). |
| `--secondary-private-key` | Second minisign key; signs the manifest **and** the installer with both keys (`.sig` gets two blocks). Use during key rotation. |
| `--secondary-password` | Password for the second key. |
| `--output-dir` | Where `latest.json`, `.sig`, and `.pub` are written (default: current directory). |
| `--archive-dir` | Copy the current main executable there as the next release's baseline. |
| `--new-app-dir` | Current app directory (exe + sidecars + resources); enables whole-app bundle patches. |
| `--old-app-dir` | Previous app directory (repeatable). The directory name must contain the version; for macOS `.app` bundles the version may live in the parent directory (`v0.1.1/MyApp.app`). |
| `--report` | Write a JSON release report (patch sizes, ratios, skipped baselines). |
| `--channel` | Release channel (default `stable`). Other names write `latest-<channel>.json` / `latest-<channel>.bundle.json`. |
| `--rollout` | Signed rollout percentage, 0–100 (default 100). Values below 100 write `platforms[].rollout`; `0` pauses the release. |
| `--min-version` | Signed minimum version. Clients below it bypass `rollout` and must update. Omitting it clears the previous floor. |
| `--fail-on-empty` | Exit non-zero when no patch was produced (CI guard). |
| `--dry-run` | Plan and verify without writing files. |

The generator only produces patches for old executables matching the target
architecture/OS; every patch is **applied and hash-verified** before it is
written into the manifest, so a bad patch fails CI instead of a user's rollback.

**Rotating the signing key.** Ship the new public key as `additional_pubkeys`
(or `UpdateConfig::additional_public_keys`) *before* it is needed, then publish
dual-signed manifests for a release cycle (`--secondary-private-key`). Clients
on the old key still verify (default `trust_policy: "any"`). Only after old
clients are gone should a release be signed with the new key alone. Setting
`trust_policy: "all"` forces dual-signing but makes a forgotten second
signature a release-wide outage — use it only for the explicit transition window.

<details>
<summary>GitHub Actions sketch</summary>

```yaml
- name: Generate delta manifest
  env:
    TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
  run: |
    cargo build --release --features generator --bin generate
    ./target/release/generate \
      --old-dir old_binaries \
      --app-binary src-tauri/target/release/MyApp.exe \
      --installer src-tauri/target/release/bundle/nsis/MyApp_0.1.2_x64-setup.exe \
      --version 0.1.2 \
      --base-url https://cdn.example.com/releases \
      --output-dir dist \
      --archive-dir old_binaries \
      --report dist/release-report.json \
      --fail-on-empty
```

Upload `dist/` together with the installer, and persist `old_binaries/` (cache or
artifact) so the next run keeps its baselines. The `release-report.json` is handy
as a CI job summary.

</details>

**Maximize hit rate with a rolling window**: keep the last 5–8 old executables in
`old_binaries/`. Patches are immutable — cache them on your CDN with
`Cache-Control: immutable, max-age=31536000`.

**Channel isolation.** Publish to a beta channel (`--channel beta`), point a test
client at `latest-beta.json`, and when you are happy publish the same artifacts
as `stable` (no `--channel`, or `--channel stable`). Clients only ever see the
manifest URL you give them, so this is a hard gate rather than a percentage.

```bash
generate ... --channel beta        # writes latest-beta.json + .sig
# validate a test client against https://cdn.example.com/latest-beta.json
aws s3 cp dist/latest-beta.json     s3://bucket/latest.json
aws s3 cp dist/latest-beta.json.sig s3://bucket/latest.json.sig
```

**Percentage rollout without a server.** Add `--rollout` to the same command:

```bash
generate ... --rollout 20     # signed platforms[].rollout = { "percentage": 20 }
generate ... --rollout 50     # raise the percentage when the first cohort is healthy
generate ... --rollout 100    # full release (field omitted, old clients unaffected)
```

The client computes `sha256(install_id + version) % 100` and only proceeds when
it falls inside the percentage. The bucket is stable for one install on one
version and reshuffles when the version changes, so the same users are not
always first or last. Installs outside the bucket receive
`UpdateOutcome::Deferred` / `UpdateResult::Deferred` / the `deferred` stage and
download nothing. The install ID lives in `tdx_install_id` in the work
directory; hosts that already have a stable identifier can set
`plugins.delta-updater.install_id` (or `UpdateConfig::install_id(...)` in the
library path). `--rollout 0` pauses the release without deleting the manifest.

> [!IMPORTANT]
> **The tag and the manifest version must use the same format.** The client
> reports `CARGO_PKG_VERSION` (e.g. `0.1.1`, no `v`); the generator normalizes
> the prefix for you.

> [!WARNING]
> **The public key must be pinned in your application** — never fetched from the
> same server that serves `latest.json`. `latest.json.pub` is generated for
> operator convenience only; its first line carries a `DO NOT TRUST` warning.

### Manifest compatibility

`latest.json` with grouped `patches`, `patch_size`, `scope`, `size`, `rollout`,
version monotonicity, and a user-writable work directory. Older manifests with a
flat `patches` array and no `patch_size` / `size` / `rollout` are still accepted
when reading; a missing `rollout` means 100%.

### 5-minute local verification

You do not need a network, a signed manifest, or a Tauri app to see whether delta
updates actually work. Put two versions of your executable anywhere, then:

```bash
cargo run --example simulate_update -- old.exe new.exe target/delta-sim
```

Or call it from your own test:

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

| | `tauri-delta-updater` | official `tauri-plugin-updater` | `tauri-plugin-updater-delta` | `tauri-plugin-hot-update` |
|---|---|---|---|---|
| Integration | Tauri plugin (`tauri-plugin-delta-updater`) or a library call (2–3 lines) | plugin registration | plugin registration | plugin registration |
| Delta algorithm | qbsdiff + zstd, PE-agnostic | full download | patch over official `Update` | JS/CSS/HTML only |
| Owns replacement / rollback | yes (script, backup, repair, safe mode) | yes (the installer) | **no** — hands the patch back | no |
| Manifest signed | yes (Ed25519 over `latest.json`) | no: only the installer is signed | inherits official | n/a |
| Downgrade guard | Rust-side semver invariant; no IPC switch | `allowDowngrades` was reachable from JS (CVE-2026-95624) | inherits official | n/a |
| Rollout | signed `platforms[].rollout` percentage | channel / CDN path only | inherited | n/a |
| Platform reach | Windows (exe + whole app); macOS `.app` only as a whole-app bundle; Linux bare binary + AppImage | all Tauri targets | all Tauri targets | web assets only |

If you only want "make the download smaller" and nothing else, the official
updater plus a transparent layer is the simpler choice. If you want a manifest
signature, deterministic rollback, safe mode, and the ability to inspect every
fallback, this library is the deeper option.

### Troubleshooting quick reference

| Symptom | Cause / fix |
|---|---|
| Always falls back to full download | Manifest key doesn't match what the client looks up. Use `current_platform()` on the client and let `generate` infer the key. |
| Always falls back to full download | `to_version` doesn't match `manifest.version`, or the client version differs in format. The generator normalizes the `v` prefix. |
| Always returns `Deferred` | The manifest's `platforms[].rollout` percentage excludes this install (stable bucket). Raise `--rollout`, publish `100`, or remember that `0` pauses the release. |
| `DeltaError::Config` at startup | `current_version` is empty/not semver, or `manifest_url` / `public_key` is empty. Whitespace and a `v` prefix in the version are normalized. |
| `DeltaError::WorkDirNotWritable` | A custom `work_dir` is not writable by the user. Omit it to use the user cache. |
| `DeltaError::IncrementalNotApplicable` | The file exceeds the 2 GB incremental limit; use the full installer. |
| `DeltaError::PatchWorkingSetTooLarge` | Only from `generate`: the baseline's estimated working set exceeds 4 GiB and the baseline is skipped (reported in `--report`); ship a full installer for that version. |
| `DeltaError::ManifestInvalid` | The signed manifest is malformed (bad JSON / not semver). |
| `DeltaError::ResponseTooLarge` | A manifest or signature response exceeded 10 MB. Deterministic; not retried. |
| `DeltaError::UpdateInProgress` | Another updater holds the lock. Prompt "try again later"; not an anomaly. |
| Need a code instead of prose | `DeltaError::code()` returns a stable `snake_case` string (`signature_invalid`, `update_in_progress`, …). Plugin errors already carry `{ code, message, retryable, security }`. |
| `DeltaError::SafeMode` from `check()` | Three deterministic failures; wait for the 6-hour cooldown or call `reset_safety()`. |
| `DeltaError::AuthenticodeUnsigned` on Windows | The running build is signed but the patched artifact is not — usually a release that forgot to sign the new executable. Blocked on purpose: AppLocker/WDAC would refuse to start it. Sign the new build (or ship the full installer) and republish; `diagnose().authenticode` shows the current state. |
| `plugin:delta-updater\|check` not found / denied at runtime | The capability is missing. Add `"delta-updater:default"` to `src-tauri/capabilities/default.json` and restart. |
| `nothing_to_launch` from `launch` | `launch` was called without a successful `prepare` in this process. Call `prepare` first; `requires_full_download` / `up_to_date` / `safe_mode` have nothing to launch. |
| `pending` returns `null` after a successful `prepare` | The persisted record failed re-validation (version no longer newer than the running one, artifact hash changed, or the work directory moved) and was discarded. Run `prepare` again. |
| Every command returns `updater_unavailable` | The updater could not be constructed (read-only work directory, missing/invalid `plugins.delta-updater`). The app still starts by design; read the message, fix the config or permissions, restart. The reason is also appended to `tdx_update.log`. |
| After a crash, the app reports `RepairRequired` / `needs_repair` | A previous whole-app swap was interrupted. Exit the process (or call `install()`); the repair script restores the backup and relaunches the old version — no network needed. |
| "It said updated, but the version is still old after restart" | Read `diagnose().last_rollback` (`update_failed` / `startup_timeout`): the new version never confirmed startup, so the script restored the old one. Parse `diagnose().events_path` (`tdx_events.jsonl`) for the structured rollback event; details are in `tdx_update.log`. Usually the app exits before `confirm_startup_success*` runs, or the new build crashes at launch. |
| Update "silently disappears" | The user declined elevation. The old version is relaunched; see `tdx_update.log` next to the install directory (and in the work directory). |
| Extra `tdx_update.ps1` / `tdx_update.sh` in the work directory | Normal: scripts don't delete themselves; the next update reclaims them. |
| `Apps & Features` / `dpkg` still shows the old version | In-place exe replacement bypasses installer/package metadata. Use the full-download path for managed deployments. |

## Upgrading

- **0.1 → 0.2**: several public structs gained fields (`UpdateManifest`,
  `PlatformInfo`, `CheckOutcome`, `UpdateConfig`, `Diagnosis`) and three enums
  gained `Deferred` (`UpdateOutcome`, `UpdateResult`, `UpdateEvent`). All enums
  are `non_exhaustive` — keep a wildcard arm. Manifests without the new fields
  keep working. Details: [CHANGELOG.md](CHANGELOG.md).
- **Unreleased**: `UpdateResult` gained `Ready` (returned by the plugin's
  `prepare` command); `UpdateConfig` gained `headers`, `fallback_endpoints`,
  `fallback_policy`, `install_mode`, `allow_insecure_transport`,
  `install_on_next_launch`, `additional_public_keys` and `trust_policy`;
  `Diagnosis` gained `insecure_transport`; new `PendingUpdate` / `PendingArtifact`
  types, `TrustPolicy`, `verify_manifest_signature_multi`,
  `verify_file_signature_multi`, `MAX_DELTA_CHAIN_HOPS`, and the `pending` /
  `install_pending` plugin commands. The release workflow now publishes both
  crates; until the plugin appears on crates.io, `cargo delta init` writes a git
  dependency.

## License

This project is licensed under the [MIT License](LICENSE).

Copyright © 2026 丘屿知宁 (zuoqiuning)
