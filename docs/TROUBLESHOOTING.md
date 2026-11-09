# Integration, troubleshooting and upgrading

## Integration comparison

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

## Troubleshooting quick reference

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
| Need to know **why the last attempt failed** | `diagnose().last_attempt` reads `tdx_last_attempt.json`: outcome (`patched` / `up_to_date` / `fallback_to_full_download` / `safe_mode` / `cancelled` / `failed` …), duration, target version and the stable error code. No log parsing. |
| `DeltaError::SafeMode` from `check()` | Three deterministic failures; wait for the 6-hour cooldown or call `reset_safety()`. |
| `DeltaError::AuthenticodeUnsigned` on Windows | The running build is signed but the patched artifact is not — usually a release that forgot to sign the new executable. Blocked on purpose: AppLocker/WDAC would refuse to start it. Sign the new build (or ship the full installer) and republish; `diagnose().authenticode` shows the current state. **Offline / certificate chain not cached** can also produce this; set `authenticode_policy: "permissive"` to keep a warning instead of blocking (explicitly unsigned artifacts are still blocked). `off` disables the check entirely (not recommended). |
| `plugin:delta-updater\|check` not found / denied at runtime | The capability is missing. Add `"delta-updater:default"` to `src-tauri/capabilities/default.json` and restart. |
| `plugin:delta-updater\|install_full` denied at runtime | The default set is incremental-only. Add `"delta-updater:full"` to the capability of the window that calls `installFull()` / `downloadFull()` / `checkCompat()`. |
| `requires_full_download` with `chain_over_budget` | A multi-hop chain exists but its total size exceeds the installer budget (`0.8 × installer`). Not a bug: the client deliberately prefers the full installer. Publish a direct patch for the old version, or lower the number of hops. |
| Concurrent updates across user accounts on a per-machine install | The default lock lives in the per-user cache, so two accounts can race. Set `lock_scope: "install_dir"` to add a machine-wide lock in the install directory (falls back to the work-dir lock with a warning when that directory is read-only). |
| `FullInstallFailed` with `install_failure: user_cancelled_elevation` | The user declined the UAC / authorization prompt. The app is untouched; show "cancelled" rather than a generic error. |
| `cargo delta init --check` reports field-type / enum errors | The config is present but unusable at runtime. Fix the named field (`install_mode`, `trust_policy`, `proxy`, `headers`, `installer_args`, …) and re-run; the README field table is authoritative. |
| `cargo delta init --check` cannot find the update config | The project is not a Tauri root (`src-tauri/tauri.conf.json` missing) or the key is absent. Both `plugins.delta-updater` and the official `plugins.updater` are accepted as config sources. |
| `doctor` fails on a fallback endpoint (404 / version mismatch) | A mirror is not synced with the primary bucket. Publish the same `latest.json` + `.sig` (and patches) to every endpoint; a stale mirror makes failover reject updates via version monotonicity. |
| `nothing_to_launch` from `launch` | `launch` was called without a successful `prepare` in this process. Call `prepare` first; `requires_full_download` / `up_to_date` / `safe_mode` have nothing to launch. |
| `pending` returns `null` after a successful `prepare` | The persisted record failed re-validation (version no longer newer than the running one, artifact hash changed, or the work directory moved) and was discarded. Run `prepare` again. |
| Every command returns `updater_unavailable` | The updater could not be constructed (read-only work directory, missing/invalid `plugins.delta-updater`). The app still starts by design; read the message, fix the config or permissions, restart. The reason is also appended to `tdx_update.log`. |
| After a crash, the app reports `RepairRequired` / `needs_repair` | A previous whole-app swap was interrupted. Exit the process (or call `install()`); the repair script restores the backup and relaunches the old version — no network needed. |
| "It said updated, but the version is still old after restart" | Read `diagnose().last_rollback` (`update_failed` / `startup_timeout`): the new version never confirmed startup, so the script restored the old one. Parse `diagnose().events_path` (`tdx_events.jsonl`) for the structured rollback event; details are in `tdx_update.log`. Usually the app exits before `confirm_startup_success*` runs, or the new build crashes at launch. |
| Update "silently disappears" | The user declined elevation. The old version is relaunched; see `tdx_update.log` next to the install directory (and in the work directory). |
| Extra `tdx_update.ps1` / `tdx_update.sh` in the work directory | Normal: scripts don't delete themselves; the next update reclaims them. |
| `Apps & Features` / `dpkg` still shows the old version | In-place exe replacement bypasses installer/package metadata. Use the full-download path for managed deployments. |

**Rollback reasons** (`diagnose().last_rollback` / `rollback_history`):

| Reason | Meaning | What to check |
|---|---|---|
| `startup_timeout` | The new build never wrote the success flag | App exits before `confirm_startup_success*`; startup slower than `confirm_timeout_secs` |
| `startup_not_started` | The new process never reached host code | Load-time crash, missing DLL/dependency, antivirus block |
| `startup_crashed` | Host code ran, then exited before confirming | Panic in early setup or a failed window creation |
| `health_check_failed` | The host's health hook returned `Err` | Database/config/dependency the hook checks |
| `update_failed` | Copy/rename in the replacement script failed | Disk space, permissions, another process holding the file |
| `repaired_partial_update` | Repair restored an interrupted swap | Expected after a crash; not a user-visible update failure |

## Upgrading

**Client compatibility matrix** (what a client can consume):

| Client | Patch format | New manifest fields |
|---|---|---|
| Older engine (e.g. host 0.2.3) | raw `BSDIFF40` only; rejects zstd containers with `not a valid patch` | Unknown fields are ignored |
| This engine | zstd container **and** raw `BSDIFF40` | `min_os`, `rollout`, `notes_i18n`, `extra` |

During the transition, publish with `generate --legacy-patch` so old clients can
still consume patches; switch back once they have been replaced.

- **Between engine generations (e.g. 0.2.3 → 0.2.4)**: the swap is driven by the
  **old** engine's script, which does not write the new `tdx_update_dir` marker.
  The first release of the new engine must call the **unconditional**
  `confirm_startup_success()` (or `PlatformUpdater::confirm_startup_success()`)
  at startup. `*_if_pending` now also detects leftover legacy scripts, but the
  unconditional form is the safe default for the transition.
- **Manifests consumed by old clients**: old clients reject zstd patches
  (`not a valid patch`). Publish one transition release with
  `generate --legacy-patch` (uncompressed `BSDIFF40`), then switch back once the
  old clients have been replaced.
- **0.1 → 0.2**: several public structs gained fields (`UpdateManifest`,
  `PlatformInfo`, `CheckOutcome`, `UpdateConfig`, `Diagnosis`) and three enums
  gained `Deferred` (`UpdateOutcome`, `UpdateResult`, `UpdateEvent`). All enums
  are `non_exhaustive` — keep a wildcard arm. Manifests without the new fields
  keep working. Details: [CHANGELOG.md](../CHANGELOG.md).
- **Next (public API hardening)**: `CheckOutcome`, `PatchInfo`, `Rollout`,
  `BundleFile`, `BundlePatch`, `BundleManifest` are now `#[non_exhaustive]`
  (construct them via serde); `CheckOutcome` gained a structured
  `unsupported_os`, and the check/result types are `#[must_use]`. Keep a
  wildcard arm and replace struct literals with serde / builder calls.
- **0.3.0 (breaking)**: `InstallerKind` gained `Inno` and `#[non_exhaustive]`;
  `UpdateConfig` gained `installer_kind` and `#[non_exhaustive]`;
  `UpdateManifest` / `PlatformInfo` are now `#[non_exhaustive]` (construct them
  via serde or builder methods); `allow_legacy_signatures` /
  `allow_insecure_transport` now default to `false`. New APIs include
  `cargo delta release`, `cargo delta doctor`, the `release` module
  (`ReleaseOptions` / `run_release`), rollback notices, `generate --sha256sums`,
  `generate --legacy-patch`, `Diagnosis::rollback_history` and
  `installer_kind`. Details: [CHANGELOG.md](../CHANGELOG.md).
- **0.4.0 (breaking)**: `UpdateOutcome::SafeMode` / `UpdateResult::SafeMode` now
  carry a `reason`; `FallbackReason::EnvironmentUnsupported` carries an
  `EnvironmentReason`; `DeltaError::FullInstallFailed` carries an
  `InstallFailureKind` (also exposed as `install_failure` in the plugin error
  DTO). `delta-updater:default` now grants only the incremental path — windows
  calling `installFull()` / `downloadFull()` / `checkCompat()` must also grant
  `delta-updater:full`. Existing capability files are not rewritten
  automatically; `cargo delta init --check` reports the gap.
- **0.5.0 (no breaking API changes)**: `PatchInfo` gains an optional
  `algorithm` field and `Diagnosis` gains `warning_keys`, `machine_lock_active`
  and `last_attempt` (all on `#[non_exhaustive]` structs). TDXB1 patches are
  only produced with `--large-file-delta` and can only be applied by 0.5.0+
  clients; it conflicts with `--legacy-patch`. `cargo delta init` now writes
  `"0.5"`.

