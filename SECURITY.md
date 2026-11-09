# Security Policy

`tauri-delta-updater` is an update engine: a vulnerability here can become a
code-execution path on every installed copy. Reports are taken seriously.

## Supported versions

| Version | Supported |
|---|---|
| 0.5.x | Yes |
| 0.4.x | No (security fixes land on 0.5.x; upgrade first) |
| 0.3.x | No (security fixes land on 0.5.x; upgrade first) |
| 0.2.x | No (security fixes land on 0.5.x; upgrade first) |
| < 0.2 | No (upgrade first) |

Security fixes are released as patch versions on the latest minor line.

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Use GitHub's private vulnerability reporting:

1. Open <https://github.com/zuoqiuning/tauri-delta-updater/security/advisories>
2. Click **Report a vulnerability**
3. Include: affected version, platform, reproduction steps, and impact

If private reporting is unavailable, contact the maintainer through the
repository owner's public profile and ask for a private channel before sharing
details.

You can expect an acknowledgement within 7 days and a status update within 30
days. Please allow a coordinated disclosure window before publishing details.

## In scope

- Manifest / signature verification (`src/core/verify.rs`)
- Version monotonicity and downgrade protection (`src/models/manifest.rs`, `src/updater/`)
- Patch download, hash anchoring and application (`src/core/downloader.rs`, `src/core/diff.rs`)
- Bundle archive parsing and extraction (`src/core/bundle/`)
- Replacement / repair scripts and their generated templates (`src/platform/`)
- Command injection, path traversal, privilege handling in those scripts
- Permission model of the Tauri plugin (`plugin/permissions/`)

## Out of scope

- An attacker who can already write the install directory or the user's work
  directory (the startup success flag is plaintext by design; see README
  "Security boundaries"). We deliberately do **not** add an HMAC to the flag:
  the key would live in the same directory, so it raises the bar only for an
  attacker who can write but not read, which is not a realistic desktop
  threat model. Adding it would also make every replacement script depend on
  platform crypto tooling.
- A leaked signing private key (that is a release-process compromise, not an
  engine bug)
- Denial of service that requires local filesystem access

## Hardening already in place

Signed manifests (minisign), version binding in the trusted comment, semantic
version monotonicity, dual SHA-256 anchoring, bounded responses, path traversal
rejection, Authenticode downgrade blocking on Windows, and a deterministic
rollback path. Details and residual risks are documented in the README and this
file.

## Why the manifest itself is signed (CVE-2026-95625)

`CVE-2026-95625` applies to the official `tauri-plugin-updater`: it verifies the
downloaded installer with minisign, but the manifest that carries `version`,
`url` and `signature` is fetched over TLS and never authenticated. Anti-rollback
compares that **unsigned** version field, so an attacker who can serve a crafted
manifest (compromised CDN/mirror, TLS-terminating proxy, cache poisoning) can
force a downgrade to any older *signed* installer without holding the signing
key.

This crate's manifest is signed as a whole, and the chain is closed in four
places:

1. `latest.json` bytes are covered by a minisign signature (`.sig`), not just the
   installer.
2. The trusted comment carries `tdu:v=<version>`, which is compared against the
   manifest's own `version`. A manifest cannot be replayed under another
   version label, and a signature cannot be transplanted between versions.
3. The downgrade guard runs in Rust (`models/manifest.rs`), not behind an IPC
   switch reachable from the webview — the class of issue behind
   `CVE-2026-95624`.
4. `platforms[].signature`, `patches[].patch_hash` and
   `patches[].target_hash` are inside the signed manifest. Patch application
   verifies the downloaded patch hash and the reconstructed artifact hash, so a
   swapped URL or patch cannot produce bytes the publisher did not sign for.

Consequence for positioning: a wrapper such as `tauri-plugin-updater-delta`
delegates `check`/`install` to the official plugin and therefore inherits the
unsigned-manifest boundary. Signing the installer twice does not fix it; the
manifest has to be signed.

Residual risk: a leaked signing key, or a host that ships an attacker-controlled
public key, defeats all of the above. See the rotation guidance below.

## Key rotation (`additional_public_keys` / `TrustPolicy`)

- `AnyOf` (the default) accepts a valid signature from **any** trusted key.
  It provides **no revocation**: a leaked or retired key stays valid until it is
  removed from the embedding app. Remove the old key in a release after the
  rotation window; old releases that still embed it keep accepting it.
- `AllOf` requires a valid signature from **every** trusted key. The number of
  signature blocks in `.sig` / `platforms[].signature` must equal the number of
  embedded keys; one forgotten secondary signature is a release-wide outage.
- Dual-signing must cover **all three artifacts**: `latest.json`,
  `latest.bundle.json` and the installer. `generate --secondary-private-key` does
  this; a hand-rolled pipeline must not sign only the main manifest.

### Rotation checklist

1. Ship a client release that embeds **both** keys (`additional_public_keys`)
   with `trust_policy = any`.
2. Dual-sign every artifact with `--secondary-private-key` until all supported
   clients have that dual-key release.
3. Ship a release that trusts **only** the new key.
4. `any` provides no revocation: after step 3, the old key is rejected; before
   step 3, a leaked old key stays valid. Do not skip step 3.

## Dependency upgrade policy

- **`reqwest`** is pinned to the verified `0.13.x` line (`>=0.13, <0.14`). Patch
  releases within the line may be adopted after CI passes on all three desktop
  platforms; a minor/major bump needs an explicit review of redirect, proxy and
  TLS behaviour.
- **`qbsdiff`** is constrained to `1.4` because the patch format and error
  behaviour changed across minor versions. Upgrading requires:

  ```bash
  cargo test --all-features --locked
  cargo test --all-features --test e2e_delta
  ```

  The weekly `Dependency audit` workflow opens/updates an issue when a newer
  `qbsdiff` is published; do not auto-merge the bump without those tests.

## Minimum OS gate (`platforms[].min_os`)

A best-effort compatibility guard, **not a security boundary**. It is skipped
when the OS version cannot be detected. On Linux it compares the **kernel
release**, not the distribution version: writing `"24.04"` is read as a far
higher kernel version and blocks every update, so do not use this field on
Linux. A compromised publisher can always sign a manifest without `min_os`; the
field only prevents accidentally shipping a build the target OS cannot run.

## Multi-hop chains and pending installs

- Every chain hop is verified against the `target_hash` in the signed manifest
  before it becomes the next hop's input. A mismatch aborts the whole update
  (no silent full-download fallback); `DeltaError::ChainHopFailed` keeps the
  underlying error code and security classification while naming the failing
  hop. The final hop's `target_hash` is the authoritative hash of the new main
  executable; `platforms[].signature` signs the installer, not the exe.
- `tdx_pending.json` is **not signed**. It binds the hash verified at
  `prepare()` time and is confined to the work directory. Local write access to
  the work directory therefore equals code execution at the next start — the
  same residual risk as the plaintext startup flag. It does not re-check the
  manifest online; hosts that require "pending must match the currently
  published manifest" call `check()` and then `discard_pending()` before
  installing.

