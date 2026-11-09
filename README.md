[![CI](https://github.com/zuoqiuning/tauri-delta-updater/actions/workflows/ci.yml/badge.svg)](https://github.com/zuoqiuning/tauri-delta-updater/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/tauri-delta-updater.svg)](https://crates.io/crates/tauri-delta-updater)
[![docs.rs](https://img.shields.io/docsrs/tauri-delta-updater)](https://docs.rs/tauri-delta-updater)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

# tauri-delta-updater

A high-performance, secure, and fault-tolerant delta update engine for Tauri v2 applications.

Version semantics: [VERSIONING.md](VERSIONING.md) · Release gates: [docs/RELEASING.md](docs/RELEASING.md) · Docs index: [docs/README.md](docs/README.md)

### Is this for you?

Seven questions, asked before the long docs:

1. **Do you ship only the main executable?** Yes → single-file patches are the sweet spot. No (`externalBin` / `resources`) → configure `bundle_endpoint` and use whole-app deltas.
2. **Which platforms?** Windows exe (NSIS / Inno Setup), macOS `.app`, and Linux bare binaries/AppImage are supported. Linux `.deb` / `.rpm` installs should stay with the package manager; use this engine for notification only.
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

**If the official updater adds signed manifests** (it has been closing that
gap), this engine still differs on: deterministic rollback scripts, whole-app
deltas, multi-hop chains, rollout / `min_os`, restart-surviving pending installs,
and per-file repair. Re-evaluate only on the axes you actually need.

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

`init` writes `"0.3"` by default (patch updates within the minor are allowed).
`--registry` pins the exact version (`"0.3.0"`) for pipelines that require the
dependency to match the tool exactly. `--check` warns when a project still pins
the git form, but keeps exit code 0 so existing integrations stay green.

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
> failures (Windows NSIS/MSI/Inno) are owned by the installer, and macOS whole-app
> updates need an Apple Developer signing/notarization setup to validate in
> production. If you only want smaller downloads, keeping the official updater
> and using this engine as the delta layer is the lower-risk default.

`src-tauri/Cargo.toml`:

```toml
[dependencies]
tauri-plugin-delta-updater = "0.3"
# Local checkout: tauri-plugin-delta-updater = { path = "../plugin" }
```

The plugin is distributed as **two packages**: this Rust crate (engine, commands,
permissions) and a separate npm package with the typed frontend bindings. The
crate tarball deliberately excludes `guest-js` / `dist-js`, so `cargo add` alone
gives you `invoke('plugin:delta-updater|...')` — not typed imports. Add the npm
package in your frontend when you want the bindings:

```bash
npm install tauri-plugin-delta-updater   # current: 0.3.0
```

The bundled `examples/tauri-app` uses `path = "../plugin"` for in-repo
development; real projects should depend on `"0.3"` from crates.io.

`src-tauri/capabilities/default.json`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["delta-updater:default"]
}
```

`delta-updater:default` grants every command. For least-privilege setups, compose
the granular sets instead: `delta-updater:read` (`check` / `diagnose` / `pending`),
`delta-updater:update` (`install` / `prepare` / `launch` / `cancel` / `reset_safety` /
`discard_pending` / `install_pending`), and `delta-updater:full`
(`download_full` / `install_full` — the permission that can launch an installer).

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
cache directory), `signature_url` (host the `.sig` separately; defaults to the
manifest URL plus `.sig`), `wait_timeout_secs` / `confirm_timeout_secs` (replacement
timings), `proxy` (`http://127.0.0.1:7890`), `headers` (extra HTTP headers for
private CDNs, e.g. `{"Authorization": "Bearer ..."}`), `allow_legacy_signatures`
(`false` by default; set `true` only for third-party legacy signing pipelines),
`fallback_policy` (`transport` / `any`), `install_mode` (`passive` / `basicUi` /
`quiet`), `installer_kind` (`inno` for Inno Setup builds; NSIS / MSI are inferred
from the `.exe` / `.msi` suffix, and plain `.exe` files are scanned for
Inno/NSIS markers before defaulting to NSIS), `installer_args` (extra Windows
installer arguments appended after the engine's flags; spaces are quoted
automatically), `allow_insecure_transport` (`false` by default;
non-loopback `http://` is rejected unless explicitly enabled), `bundle_type`,
`install_id`,
`install_on_next_launch` (install the staged update on the next start; off by
default), `rollback_notice` (show a minimal system notice after a rollback; on
by default, turn it off for kiosks and headless deployments),
`additional_pubkeys` (extra trusted keys during rotation),
`trust_policy` (`any` default / `all` requires every trusted key to sign), and
`channel` (default `stable`; endpoints may use `{{channel}}` next to
`{{target}}` / `{{arch}}` / `{{current_version}}`).

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
// If check() reports a newer version than the persisted one, drop the stale
// record first; otherwise the app installs the older staged version and
// updates again.
await invoke('plugin:delta-updater|discard_pending');
await invoke('plugin:delta-updater|install_pending');              // → true, process exits
// 增量不可用时，回退路径也不需要官方插件：
await invoke('plugin:delta-updater|install_full', { onEvent: events });
```

The plugin confirms startup before any window opens, streams stage events, and
exits the process once the swap is ready. Errors arrive as
`{ code, message, retryable, security }` — branch on `code`, never on `message`.

The npm package exports `check`, `install`, `prepare`, `launch`, `downloadFull`,
`installFull`, `pending`, `discardPending`, `installPending`, `diagnose`,
`configWarnings`, `cancel`, `resetSafety`, `requiresExit`, plus `UpdateEvent` /
`UpdateResult` / `ErrorDto` types.

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
- A failed Windows NSIS/MSI/Inno install cannot be rolled back by us; the installer
  owns that. We surface `full_install_failed` and the log path.
- Windows installer arguments follow Tauri's own conventions (`/P /UPDATE /R`
  for NSIS, `/passive /promptrestart AUTOLAUNCHAPP=True` for MSI), so updates
  are not mistaken for first-time installs. Inno Setup is auto-detected from the
  installer's marker strings when possible; declare `installer_kind: "inno"`
  explicitly if you use a custom loader or the markers are stripped. Inno
  receives `/SILENT /NORESTART /SP-` (passive) or `/VERYSILENT /NORESTART /SP-`
  (quiet); `/UPDATE` and `/R` are `.iss`-side semantics, not Inno flags. `install_mode` switches to `basicUi` (no UI flag)
  or `quiet`; the default stays `passive`. `quiet` cannot elevate by itself, so
  it only works for per-user installs or when the process is already elevated.
- `endpoints` may use the official updater's `{{target}}` (windows / darwin /
  linux), `{{arch}}`, `{{current_version}}` and `{{bundle_type}}` placeholders.
  The first three expand automatically; `{{bundle_type}}` requires an extra
  `"bundle_type": "nsis"` (or `msi` / `appimage` / `app`) in the plugin config —
  without it startup fails with a clear error instead of sending a URL that
  contains literal braces.
- `install_full` returns `Updated`, then the plugin exits — the installer is
  already running and **must relaunch the app itself**: NSIS receives `/R`; Inno
  must do it in `.iss` via `[Run]` **without** `skipifsilent`. The engine never
  relaunches on this path.
- Official `windows.installMode` and `windows.installerArgs` are migrated to the
  top-level `install_mode` / `installer_args` automatically.
  `dangerousInsecureTransportProtocol` is **not** consumed: the plugin logs a
  startup warning and keeps its safe default; use `allow_insecure_transport`
  instead.

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
yourself. `UpdateResult::requires_exit()` is the single exit predicate: it is
`true` for `Updated` / `RepairRequired` and `false` for `Ready`.

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
| Linux | **Stable** | Bare binaries and **AppImages** (the `.AppImage` file itself is patched in place, `chmod +x`, then relaunched; only a read-only install directory falls back). A bare binary installed by `.deb` / `.rpm` can technically be replaced in place with elevation (`sudo -n` or `pkexec`, then privileges are dropped back via `runuser` / `su`), but the package manager metadata (`dpkg -l`) will report the old version — prefer the package manager for those installs. **Full installers** ending in `.deb` / `.rpm` are refused (`full_install_failed`) |

**Mobile (Android / iOS) is out of scope**: there are no replacement scripts for those targets, so `DeltaUpdater::new` rejects an unsupported platform during construction instead of failing after a download.

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
- **Built-in full-installer path** — `download_full()` downloads `platforms[].url`, verifies `platforms[].signature` with mmap (no 200 MB read into RAM), and `install_full()` runs it: NSIS / MSI / Inno Setup with elevation on Windows, `.app.tar.gz` unpack + directory swap on macOS, AppImage replacement on Linux. The official updater is no longer required for the fallback path.
- **Signed manifests** — Ed25519 (minisign) signatures on `latest.json` prevent man-in-the-middle tampering. Signature failures are reported as a distinct error so tampering can be told apart from a corrupted download.
- **Downgrade protection, three layers, on by default** — (1) the **manifest itself is signed**, so an attacker cannot repoint it at an old installer without breaking the signature; (2) after verification, `manifest.version` is compared against the running version with **semantic versioning**, and a non-newer manifest returns `UpdateOutcome::UpToDate` instead of handing the host an old installer URL; (3) the generator's version binding in the trusted comment catches publisher-side "signature from A on manifest B" mistakes. The first two layers block replay; the third is defense in depth. None of them depends on an optional switch.
- **Typed fallback reasons** — `FallbackReason::NoMatchingPatch` / `EnvironmentUnsupported`, instead of a free-form string that forces UI code to match Chinese prose.
- **Download-cost visibility** — `generate` stamps `platforms[].size` into the signed manifest, so `CheckOutcome::patch_size_ratio()` returns `patch_size / installer_size` before any download. **Lower is better**: `0.1` means the patch is 10% of the installer; values near `1.0` mean the delta saves little and the full installer may be faster. Manifests without `size` return `None` instead of a guessed number.
- **Local verification harness** — `simulate_update(old, new, work_dir)` runs generate → apply → dual-hash verification with no network or signing key, so you can see the real patch ratio before integrating.
- **Environment diagnosis** — `DeltaUpdater::diagnose()` reports work-directory/install-directory writability, platform eligibility, safe-mode state, the exact log path, `last_rollback` (why the last replacement script rolled back, e.g. `update_failed` / `startup_timeout`), `events_path` for the structured stream below, and `user_message()` for a ready-to-display one-line summary. Linux builds also report whether a privilege-drop tool (`runuser` / `su`) was found; Windows builds report `authenticode` (whether the running exe is trusted) and `authenticode_check_skipped` (true when the check could not reach a conclusion, e.g. offline with no cached chain).
- **Multi-hop delta chains** — when no direct patch exists, `update()` applies up to 3 verified hops (0.1 → 0.2 → 0.3), checking each intermediate result against its signed `target_hash` before it becomes the next hop's input. The planner prefers a direct patch, then the shortest viable chain (fewest hops), then the full installer; a chain whose total signed `patch_size` exceeds 80% of the installer falls back to full. The generator keeps only chainable history (`MAX_DELTA_CHAIN_HOPS`) so manifests don't grow forever.
- **Key rotation without breaking old clients** — `additional_public_keys` / `additional_pubkeys` extend the trusted set; `trust_policy: "any"` accepts a signature from any of them, `"all"` requires every key to sign (dual-sign window). `.sig` files may contain multiple minisign blocks, and `generate --secondary-private-key` signs both the manifest and the installer with both keys.
- **Minimum OS gate** — `generate --min-os 10.0.19045` writes a signed `platforms[].min_os`. The client refuses the update **before any download** when the detected OS is older (`UpdateOutcome::UnsupportedOs`), and hides both the patch and the full-installer URL from `check()`. Detection is Windows `RtlGetVersion`, macOS `sw_vers`, Linux kernel release; if it cannot be determined, the update proceeds (unknown ≠ unsupported). This is a **best-effort compatibility guard, not a security boundary**; on Linux it compares the kernel release, not the distro version.
- **Localized release notes and custom metadata** — `generate --notes-i18n zh-CN=... --notes-i18n en=...` writes signed `notes_i18n`; `manifest.notes_for(locale)` resolves exact → language prefix → `notes`. Unknown top-level manifest fields are preserved in `UpdateManifest::extra`, and `generate --existing` carries them across releases, so hosts can read custom release metadata without forking the format.
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
- **Cross-restart staged install** — if the user quits after `prepare()` but before `launch()`, the staged artifact is persisted in `tdx_pending.json`. On the next start, `pending_update()` / `install_pending()` re-verify it (strictly newer version, script and artifact inside the work directory, SHA-256 of the merged exe) and run the swap without touching the network. The plugin exposes `pending` / `install_pending`, and `install_on_next_launch` runs it automatically at startup (off by default). Pending is **offline by design** and does not re-read the manifest: if a release is withdrawn, call `discard_pending` when `check()` reports a version at or below the pending one, or from an explicit revocation path.
- **Auto-rollback with a real restart** — if the new version fails to write its startup flag within the timeout, the script stops the new process, restores the backup, **and relaunches the old version**. The same recovery runs when elevation is declined or a copy fails. The user gets a minimal system notice after the rollback (Windows message box, macOS/Linux desktop notification; disable with `rollback_notice: false`) instead of silently reverting.
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
- **Legacy minisign signatures are rejected by default.** Our generator is pinned
  to minisign 0.7.9, which emits pre-hash signatures (asserted by a unit test),
  so the default only rejects third-party historical pipelines. Set
  `allow_legacy_signatures(true)` if you really consume one; it is a deliberate
  validation relaxation, not a compatibility default.
- **Plaintext HTTP is rejected by default.** Non-loopback `http://` endpoints are
  rejected at construction (loopback is still allowed for local tests).
  `diagnose().insecure_transport` flags them; setting
  `allow_insecure_transport(true)` opts back in, but URLs, versions and update
  cadence are visible to the network. Use HTTPS in production.
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
- **Failure recovery** — every failure after the old process exits funnels through one handler that restores the backup and relaunches the old version. The 60-second startup-timeout branch needs a real timeout window and is covered by the manual procedure in [Device tests](docs/DEVICE_TESTS.md) (“Failure recovery”).
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

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — module boundaries, invariants and failure-recovery matrix.
- [Usage guide](docs/USAGE.md) — client integration, frontend result handling, cancellable sessions.
- [Publishing guide](docs/PUBLISHING.md) — `generate` / `cargo delta release` / `doctor`, manifest compatibility and the 5-minute check.
- [Integration, troubleshooting and upgrading](docs/TROUBLESHOOTING.md) — symptoms, error codes and migration notes.
- [Device tests](docs/DEVICE_TESTS.md) — real-device checklist before publishing.
- [Source map](docs/SOURCE_MAP.md) — where to change what, and which test guards it.
- [Release standard](docs/RELEASE_STANDARD.md) and [releasing](docs/RELEASING.md).

## License

This project is licensed under the [MIT License](LICENSE).

Copyright © 2026 丘屿知宁 (zuoqiuning)
