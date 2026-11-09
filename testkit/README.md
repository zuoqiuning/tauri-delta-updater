# tdx-testkit

Integration-test fixtures for [`tauri-delta-updater`](https://crates.io/crates/tauri-delta-updater):
a mock update server with `Range` support, minisign helpers, and signed manifest
builders. It is a dev-only dependency — it never enters your production build.

```toml
[dev-dependencies]
tdx-testkit = "0.6"
```

## What you get

| Item | Purpose |
|---|---|
| `MockServer` | Serves fixtures by path, records requests, injects 502s, enforces headers, answers `Range` requests with `206` |
| `signing_keypair` / `sign` / `sign_bound` | minisign keys and signatures, including version-bound trusted comments |
| `manifest_json` / `signed_manifest` | Build any manifest shape, then sign it |
| `build_manifest` / `build_manifest_with_rollout` | Ready-made delta manifest with patches and rollout |
| `sha256` / `write_file` | Small fixture helpers |

## Example

```rust,no_run
use tdx_testkit::{build_manifest, signing_keypair, MockServer};

let (sk, public_key_b64) = signing_keypair();
let server = MockServer::empty();

let patch = std::fs::read("update.patch").unwrap();
let patch_hash = tdx_testkit::sha256(&patch);
let (manifest, _sig) = build_manifest(
    &sk,
    &server.base_url,
    "0.2.0",
    "windows-x86_64",
    "0.1.0",
    &patch_hash,
    "TARGET_SHA256",
    Some(patch.len() as u64),
);
server.set("/update.patch", patch);
server.serve_signed("/latest.json", manifest, &sk);

// UpdateConfig::new(server.url("/latest.json"), public_key_b64)
# let _ = (public_key_b64, server.url("/latest.json"));
```

`serve_signed` writes the manifest and its `.sig` in one call — the same layout
the engine expects. `fail_next` simulates transient 5xx responses; `was_requested`
lets you assert that the delta path never touched the full installer.

## Limits

`MockServer` handles plain `GET` over loopback only, one connection at a time,
and stops after 1024 requests. That is enough for updater tests; use a real HTTP
server if you need concurrency.

## License

MIT, same as `tauri-delta-updater`.
