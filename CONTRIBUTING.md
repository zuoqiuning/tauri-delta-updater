# Contributing

Thanks for taking the time to improve the engine. This project cares more about
correctness of the update path than about feature count — a patch that applies
99% of the time is a bug.

## Before you start

- Read `docs/源码导航.md` (Chinese) — it maps "what you want to change" to files
  and the tests that guard them.
- For anything touching the replacement scripts, read the relevant template in
  `src/platform/` and its real-execution tests first.
- For security-sensitive changes (signature, hashing, path handling, elevation),
  open an issue describing the threat before writing code.

## Requirements

- Rust stable (MSRV is 1.88; CI verifies it separately)
- Windows / macOS / Linux for the platform you touch
- Windows: PowerShell 5.1+ (the generated scripts target it)
- Linux: GTK/WebKitGTK dev packages only if you build the plugin crate

## Verify your change

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --locked --no-fail-fast

# plugin crate (separate manifest, own lockfile)
cargo fmt --manifest-path plugin/Cargo.toml -- --check
cargo clippy --manifest-path plugin/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path plugin/Cargo.toml

# public API path guard (compile-time)
cargo test --all-features --test public_api
```

Low-memory machines can hit `os error 1455` with parallel rustc; use `-j 1`.
That is an environment limit, not a code failure.

Useful local checks:

```bash
# see a real patch ratio without network or a signing key
cargo run --example simulate_update -- old.exe new.exe target/delta-sim

# dry-run the onboarding command against a scratch Tauri project
cargo run --all-features --bin cargo-delta -- delta init --project <dir> --endpoint <url> --pubkey <key>
```

## Rules of thumb

- Every bug fix comes with a test that fails before the fix. The suite has a
  habit of asserting the *absence* of work (e.g. "the full installer URL was
  never requested"); keep that style.
- A patch must be proven before it enters a manifest: `generate` applies every
  patch and verifies the product hash. Do not weaken this.
- New public items: update `tests/public_api.rs` and the rustdoc. Breaking a
  public path should fail CI, not a user's build.
- Script templates: never hardcode paths or platform branching in Rust; extend
  the template and its real-execution test.
- Do not add error handling for cases that cannot happen. Do add it at
  boundaries (HTTP responses, archive bytes, user-supplied paths).

## Commit / PR

- Small, focused commits; explain *why* in the message (the "what" is in the diff).
- PRs should state: affected platform(s), how it was verified, and any new
  fallback / failure path.
- CI must be green, including the plugin crate and the version-consistency job.
- Do not include generated line-ending changes or build artifacts.
