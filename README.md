# tauri-delta-updater

A standalone incremental update engine for Tauri v2: clients download a small
binary patch, and the engine owns replacement, rollback and full-installer
fallback. The manifest stays wire-compatible with `tauri-plugin-updater`
(`latest.json`, `pubkey`, `platforms[].signature`), but the manifest itself is
signed and every fallback is a typed reason.

Version semantics: [VERSIONING.md](VERSIONING.md) · Docs index: [docs/README.md](docs/README.md) · Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)

**Maturity: early (0.5.x).** Single maintainer. Windows is the most exercised
platform; macOS and Linux paths are functional but less battle-tested. Run one
full update cycle on each platform ([DEVICE_TESTS.md](docs/DEVICE_TESTS.md))
before production use; CI covers static checks, synthetic scripts and
end-to-end mocks, not code signing, UAC or Gatekeeper.

## Measured on Windows

A real release pair, not a synthetic byte edit: this project's own Windows CLI
(release build, AMD64), v0.4.0 → v0.5.0. Measured on 2026-11-09 with
`cargo run --example simulate_update -- old.exe new.exe`, which generates,
applies and SHA-256-verifies the patch through the same code path the client
uses.

| | Size |
|---|---|
| Full download (v0.5.0 executable) | **8.44 MB** |
| Delta download (v0.4.0 → v0.5.0 patch) | **0.59 MB** |
| Reduction | **93%** (patch = 7.0% of full) |

The ratio depends on what changed between builds; run the same example on your
own artifacts to get your number (see [Limits](#limits)). A bundled Tauri
installer is larger than this bare executable, but the patch only carries the
bytes that changed.

## Is this for you?

- **Main executable only** → single-file patches are the sweet spot.
- **Sidecars / resources** → configure `bundle_endpoint` and use whole-app deltas.
- **You need signed manifests, downgrade protection, deterministic rollback** → this engine.
- **You only want smaller downloads** and your current official-updater flow is stable → keep the official updater and use a transparent delta layer instead.
- **Skip it** if your installer is small (roughly under 30 MB) and you release
  less than once a month: CDN + full downloads are simpler.

| | Smaller download | Owns replacement / rollback | Native binary | Frontend-only |
|---|---|---|---|---|
| official `tauri-plugin-updater` | no | yes (the installer) | yes | no |
| `tauri-plugin-updater-delta` | yes | **no** — hands the patch back | yes | no |
| `tauri-plugin-hot-update` | n/a | no | no | yes |
| **this engine** | yes | yes (script, backup, repair, safe mode) | yes | no |

Platforms: Windows `.exe` (NSIS / MSI / Inno Setup), macOS `.app`, Linux bare
binaries and AppImage. Linux `.deb` / `.rpm` stay with the package manager.

## Quick start — Tauri plugin

### 1. Scaffold the integration

```bash
cargo install tauri-delta-updater --features generator
cd my-app

cargo delta init \
  --endpoint https://cdn.example.com/latest.json \
  --generate-key myapp.key       # dry-run: prints a diff, writes nothing
cargo delta init ... --apply     # writes config, capability and dependency
cargo delta init ... --check     # CI guard; non-zero if anything is missing
```

`--pubkey` / `--pubkey-file` work too. `--generate-key` creates
`myapp.key` + `myapp.key.pub` (Unix `0600`), prints only the paths, and never
overwrites an existing key.

Migrating from the official plugin? `cargo delta migrate` copies
`plugins.updater` into `plugins.delta-updater` and keeps the official plugin as
a fallback; add `--remove-official` to drop it.

Keep the guard in CI:

```yaml
- run: |
    cargo install tauri-delta-updater --features generator
    cargo delta init --check
```

For production, pin the exact version with `--registry` and read the
`升级注意` section of [CHANGELOG.md](CHANGELOG.md) before every minor upgrade:
defaults can tighten between minors (0.4.0 narrowed the default permissions,
for example).

### 2. Register it

`src-tauri/Cargo.toml`:

```toml
[dependencies]
tauri-plugin-delta-updater = "0.5"
```

`src-tauri/src/lib.rs`:

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_delta_updater::init_from_config())
    .run(tauri::generate_context!())?;
```

`capabilities/default.json` — `default` covers the incremental path only:

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["delta-updater:default"]
}
```

Add `delta-updater:full` only to windows that call `installFull()` /
`downloadFull()` or the `checkCompat()` fallback. `read` and `update` are
available separately for tighter scopes.

`tauri.conf.json`:

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

Every other key has a safe default; see
[CONFIG_CHEATSHEET.md](docs/CONFIG_CHEATSHEET.md) for proxies, headers,
timeouts, installer args, trust policy, rollout and fallback endpoints.

### 3. Call it from the frontend

```bash
npm install tauri-plugin-delta-updater   # typed bindings; current: 0.5.0
```

```ts
import { invoke, Channel } from '@tauri-apps/api/core';

const events = new Channel();
events.onmessage = (e) => {
  // checking | downloading | applying | ready | up_to_date
  // deferred | fallback | repairing | safe_mode | cancelled
  if (e.stage === 'downloading') console.log(e.downloaded, e.total); // your progress UI
};

await invoke('plugin:delta-updater|install', { onEvent: events });
```

Need to control the exit moment (save drafts, show a prompt)? Use
`prepare` → `launch` instead of `install`. If the user quits before `launch`,
the staged artifact survives a restart: `pending()` → `installPending()`.
Errors arrive as `{ code, message, retryable, security }` — branch on `code`,
never on the message.

The npm package also exports `check`, `checkCompat`, `downloadFull`,
`installFull`, `diagnose`, `configWarnings`, `cancel`, `resetSafety`,
`requiresExit`, plus the `UpdateEvent` / `UpdateResult` / `ErrorDto` types.

> **Single-instance apps:** the plugin exits after a short delay so the
> replacement script can take over. If you also run
> `tauri-plugin-single-instance`, verify on each platform that its lock is
> released, or the relaunched build looks like a second instance.

## Quick start — any Rust host

```rust
use tauri_delta_updater::{DeltaUpdater, UpdateResult};

// No-op unless the previous run actually swapped in a new version.
let _ = tauri_delta_updater::PlatformUpdater::confirm_startup_success_if_pending()?;

let updater = DeltaUpdater::auto(
    "https://cdn.example.com/latest.json",
    "BASE64_PUBLIC_KEY",
    env!("CARGO_PKG_VERSION"),
)?;

match updater.install().await? {
    result if result.requires_exit() => std::process::exit(0),
    UpdateResult::UpToDate { .. } => {}
    UpdateResult::RequiresFullDownload { .. } => { /* install_full() or hand off */ }
    UpdateResult::SafeMode { reason } => { let _ = reason; /* manual install */ }
    _ => {}
}
```

The engine derives the platform, a user-writable work directory and the HTTP
client. Call `confirm_startup_success_with(|| ...)` if "up but unhealthy" is
possible — a failed health check rolls back immediately instead of waiting for
the startup timeout.

Wondering whether a patch is worth it? No network or key needed:

```bash
cargo run --example simulate_update -- old.exe new.exe target/delta-sim
```

For cancellable sessions with stage events, use `UpdateSession`:
`updater.session()` → `run(on_event)` + `cancel()` (see
[USAGE.md](docs/USAGE.md#cancellable-updates-session)).

## Which track?

| | Tauri plugin | Rust library |
|---|---|---|
| Tauri app with a webview | recommended | possible, you own the wiring |
| Non-Tauri Rust host | — | yes |
| Configuration | `tauri.conf.json` + capabilities | `UpdateConfig` builder |
| Progress / cancel | `install` events + `cancel` command | `UpdateSession` + `CancelToken` |
| Install on next launch | `install_on_next_launch` | `pending()` + `install_pending()` |
| Official-updater migration | `checkCompat()` from npm | manual |

Both tracks run the same engine, outcomes and safe-mode logic; the plugin only
adds IPC, permissions and config loading.

## Examples

- [examples/tauri-app](examples/tauri-app) — minimal plugin app with capabilities and config.
- [examples/migration-example](examples/migration-example) — official-updater frontend, before/after `checkCompat()`.
- [examples/full_cycle.rs](examples/full_cycle.rs) — local end-to-end harness (generate → apply → verify).
- [examples/simulate_update.rs](examples/simulate_update.rs) — measure a real patch for your own binaries.

## What it handles

- **Delta patches** — qbsdiff + zstd, multi-hop chains, per-patch verification on the publisher.
- **Large files** — opt-in TDXB1 block delta when bsdiff's working set would exceed the budget.
- **Replacement** — script-generated swap, backup, repair after a crash, startup health check.
- **Rollback** — deterministic restore on copy failure, startup timeout, health failure or rollback notice.
- **Safe mode** — stops after repeated failures and reports a structured reason (security vs environment).
- **Gradual rollout** — signed `platforms[].rollout` percentage and `min_os` gate.
- **Restart-surviving pending installs** — `prepare` / `pending` / `installPending`.
- **Whole-app updates** — bundle manifest covering sidecars and resources.
- **Full-installer fallback** — download, verify and run NSIS/MSI/Inno/AppImage installers.
- **Observability** — typed outcomes, `diagnose()` with the last attempt, stage events, JSONL script events.

## Security

`latest.json` is signed as a whole (minisign), the version is bound in the
trusted comment (`tdu:v=`), downgrade rejection runs in Rust rather than behind
an IPC switch, and every patch/installer hash is anchored in the signed
manifest. This closes the class of issue behind
[CVE-2026-95625](https://nvd.nist.gov/vuln/detail/cve-2026-95625) (official
updater: unsigned manifest) and CVE-2026-95624 (downgrade switch reachable from
the webview). Threat matrix and residual risks: [SECURITY.md](SECURITY.md) ·
TDXB1 parser model: [BLOCK_DELTA_THREAT_MODEL.md](docs/BLOCK_DELTA_THREAT_MODEL.md).

## Limits

- bsdiff needs roughly `max(17 × old, 9 × old + new)` memory: single files over
  2 GB are rejected, and generation skips baselines above a 4 GiB working set.
- `--large-file-delta` lifts that ceiling (multi-GB files), but **only 0.5.0+
  clients can apply TDXB1**; it is off by default and conflicts with
  `--legacy-patch`.
- Publisher parallelism is bounded by a memory budget (default 4 GiB):
  `--jobs` / `--patch-memory-budget-mb`, or `TDX_PATCH_JOBS` /
  `TDX_PATCH_MEMORY_BUDGET_MB`.
- Patches close to full-installer size are skipped with `--max-patch-ratio`;
  a poor-ratio patch is still correct, just not smaller.

## Documentation

| Doc | Use it for |
|---|---|
| [USAGE.md](docs/USAGE.md) | Scenario-based integration, frontend result handling, cancellable sessions |
| [CONFIG_CHEATSHEET.md](docs/CONFIG_CHEATSHEET.md) | Configuration decision tree, plugin fields, permission sets |
| [PUBLISHING.md](docs/PUBLISHING.md) | `generate` / `cargo delta release` / `doctor` |
| [TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Symptoms, error codes, version migration |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | Module boundaries, invariants, failure-recovery matrix |
| [RELEASING.md](docs/RELEASING.md) | Release gates, three-platform order, authorization |
| [DEVICE_TESTS.md](docs/DEVICE_TESTS.md) | Real-device checklist before publishing |
| [SOURCE_MAP.md](docs/SOURCE_MAP.md) | Where to change what, and which test guards it |

## License

MIT. See [LICENSE](LICENSE).
