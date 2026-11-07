# Security Policy

`tauri-delta-updater` is an update engine: a vulnerability here can become a
code-execution path on every installed copy. Reports are taken seriously.

## Supported versions

| Version | Supported |
|---|---|
| 0.2.x | Yes |
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
  "Security boundaries")
- A leaked signing private key (that is a release-process compromise, not an
  engine bug)
- Denial of service that requires local filesystem access

## Hardening already in place

Signed manifests (minisign), version binding in the trusted comment, semantic
version monotonicity, dual SHA-256 anchoring, bounded responses, path traversal
rejection, Authenticode downgrade blocking on Windows, and a deterministic
rollback path. Details and residual risks are documented in the README and
`docs/设计文档.md` (section 9).
