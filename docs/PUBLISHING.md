# Publishing guide

Publishing manifests, patches and CDN objects is the **application publisher's**
job; the runtime engine never does it. If your pipeline already produces
`latest.json` and patches, skip this section.

The repository ships optional tooling for publishers who want it:

```bash
cargo install tauri-delta-updater --features generator

cargo delta release --from old_binaries --to 0.1.1 \
  --app-binary ./target/release/MyApp.exe \
  --installer ./target/release/MyApp_0.1.1_x64-setup.exe \
  --base-url https://cdn.example.com \
  --output-dir ./dist --fail-on-empty

cargo delta doctor https://cdn.example.com/latest.json \
  --pubkey "$EMBEDDED_PUBKEY" --deep

# 或直接复用项目配置（端点、公钥、签名地址、整包地址与请求头）：
cargo delta doctor --from-config --project ./my-app
```

`cargo delta release` shares the same implementation as `generate`
(`release::run_release`); `cargo delta doctor` validates a published bucket
(signature, sizes, patch hashes, chain reachability). `--from-config` reads
`plugins.delta-updater` (falling back to `plugins.updater`), expands official
placeholders, uses the configured trust policy and checks every configured
fallback endpoint (manifest signature plus primary/fallback version consistency).
`--fallback-url` tests extra mirrors explicitly. `--help` is always the
authoritative argument list; the reference below is kept for offline reading.

## Publisher CLI

<details>
<summary>Publisher CLI reference (generate / cargo delta release / doctor)</summary>

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

During a cross-generation transition, `--legacy-patch` writes uncompressed
`BSDIFF40` patches: clients on the old engine reject zstd containers with
`not a valid patch`. Patch size grows noticeably; switch back once the old
clients have been replaced.

Baselines are generated **in parallel**, bounded by estimated memory rather than
by a fixed one-at-a-time queue: every job declares its working set
(`max(17n, 9n+m)` for bsdiff), and the scheduler admits jobs while the sum stays
under `--patch-memory-budget-mb`. A single oversized job still runs alone instead
of being rejected. Patch and skip ordering in the report is unchanged, so
parallelism does not affect the manifest diff.

`--large-file-delta` extends coverage past bsdiff's ceiling. For a baseline that
would need more than the budget (roughly a single file over 240 MB), the
publisher falls back to a TDXB1 block delta: the old file is split on
content-defined boundaries, matching chunks become `COPY` instructions, and each
gap between matches is re-diffed with a bounded (≤4 MiB) bsdiff against the
corresponding old region before falling back to a compressed literal. Memory is
therefore independent of total file size, so multi-gigabyte installers can still
ship a delta with patch sizes close to the regular path. The flag is off by
default because TDXB1 is a new container that only 0.5.0+ clients understand.

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
| `--legacy-patch` | Write uncompressed `BSDIFF40` patches for clients still running an older engine that rejects zstd containers. Larger patches; use only during the transition. |
| `--large-file-delta` | When bsdiff's working set exceeds the budget, fall back to a TDXB1 block delta instead of skipping the baseline. Supports single files far beyond the 2 GB bsdiff limit. **Only enable after every client is on a version that can apply TDXB1**; older clients would download a patch they cannot apply. |
| `--jobs` | Upper bound for concurrent patch generation (default: CPU count). Memory is budgeted separately, so this rarely needs changing. |
| `--patch-memory-budget-mb` | Memory budget for patch generation in MiB (default `4096`). Baselines are scheduled in parallel while the estimated working set stays under this budget. |
| `--existing` | Previous `latest.json`, to preserve other platforms' entries and patches. |
| `--password` | Private-key password (or `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). |
| `--secondary-private-key` | Second minisign key; signs the manifest **and** the installer with both keys (`.sig` gets two blocks). Use during key rotation. |
| `--secondary-password` | Password for the second key. |
| `--output-dir` | Where `latest.json`, `.sig`, and `.pub` are written (default: current directory). |
| `--archive-dir` | Copy the current main executable there as the next release's baseline. |
| `--new-app-dir` | Current app directory (exe + sidecars + resources); enables whole-app bundle patches. |
| `--old-app-dir` | Previous app directory (repeatable). The directory name must contain the version; for macOS `.app` bundles the version may live in the parent directory (`v0.1.1/MyApp.app`). |
| `--report` | Write a JSON release report (patch sizes, ratios, skipped baselines). |
| `--sha256sums` | Write `SHA256SUMS` (hash + filename) for the manifest, signature, public key, installer and every generated patch (single-file `*.patch` and whole-app `*.bundle.patch`). Use it to verify the CDN has all artifacts. |
| `--channel` | Release channel (default `stable`). Other names write `latest-<channel>.json` / `latest-<channel>.bundle.json`. |
| `--rollout` | Signed rollout percentage, 0–100 (default 100). Values below 100 write `platforms[].rollout`; `0` pauses the release. |
| `--min-version` | Signed minimum version. Clients below it bypass `rollout` and must update. Omitting it clears the previous floor. |
| `--min-os` | Signed minimum OS version for this platform (Windows `10.0.19045`, macOS `13.0`, Linux kernel `5.15`). Clients that can detect an older OS refuse the update before downloading. Omit to clear it. |
| `--notes-i18n` | Localized release notes (repeatable), format `LOCALE=TEXT` (e.g. `zh-CN=修复了...`). Written to the signed `notes_i18n`; `--notes` remains the fallback. |
| `--fail-on-empty` | Exit non-zero when no patch was produced (CI guard). |
| `--dry-run` | Plan only: print inputs and the patch plan without generating patches or writing files. A real run is what applies and verifies every patch. |

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
`AllOf` requires **one signature block per trusted key**: if the app embeds 3
keys, every release must carry 3 signatures (main manifest, bundle manifest and
installer); a partial dual-sign is rejected with an explicit "bundle manifest
signature failed" message. `AnyOf` has **no revocation** — remove the old key
from the app after the rotation window (see SECURITY.md).

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

- name: Archive release state snapshot
  uses: actions/upload-artifact@v4
  with:
    name: tdx-release-state-${{ github.ref_name }}
    path: |
      dist/latest.json
      dist/latest.json.sig
      dist/release-report.json
    retention-days: 365
```

Upload `dist/` together with the installer, and persist `old_binaries/` (cache or
artifact) so the next run keeps its baselines. The `release-report.json` is handy
as a CI job summary. The snapshot artifact is the rollback/audit record: the CDN
only keeps the *current* manifest, so without it "which rollout/min_os was live
when" cannot be reconstructed later.

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

</details>

## Manifest compatibility

`latest.json` with grouped `patches`, `patch_size`, `scope`, `size`, `rollout`,
version monotonicity, and a user-writable work directory. Older manifests with a
flat `patches` array and no `patch_size` / `size` / `rollout` are still accepted
when reading; a missing `rollout` means 100%.

## 5-minute local verification

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

