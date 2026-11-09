# tauri-plugin-delta-updater

Typed frontend bindings for the Tauri v2 delta updater. The engine, commands and
permissions live in the Rust crate
[`tauri-plugin-delta-updater`](https://crates.io/crates/tauri-plugin-delta-updater).

## Install

```bash
# Rust side (application)
cargo add tauri-plugin-delta-updater

# Frontend bindings (this package)
npm install tauri-plugin-delta-updater
```

The plugin reuses the official updater's `plugins.updater` config as-is when
`plugins.delta-updater` is absent, so migration can start with the same
`endpoints` + `pubkey`.

`src-tauri/tauri.conf.json`:

```json
{
  "plugins": {
    "delta-updater": {
      "endpoints": ["https://cdn.example.com/latest.json"],
      "pubkey": "RWQ... (pinned inside the app)"
    }
  }
}
```

`src-tauri/capabilities/default.json`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "windows": ["main"],
  "permissions": ["delta-updater:default"]
}
```

`src-tauri/src/lib.rs`:

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_delta_updater::init_from_config())
    .run(tauri::generate_context!())?;
```

## Frontend

```ts
import {
  check,
  install,
  requiresExit,
  configWarnings,
  type UpdateEvent,
} from 'tauri-plugin-delta-updater';

const info = await check();
if (!info.is_newer) return;

const result = await install((event: UpdateEvent) => {
  if (event.stage === 'downloading') {
    console.log(event.downloaded, event.total); // update the progress UI
  }
});
// updated / repair_required mean the replacement script already started:
// the plugin exits the process after a short delay.
if (requiresExit(result)) return;
```

`install()` downloads, verifies, merges and launches the swap. If the UI must
control when the process exits (save drafts, show a prompt), use `prepare()`
followed by `launch()`; `prepare` returns `{ kind: 'ready' }` and persists the
staged artifact across restarts.

Other exports: `launch`, `installFull`, `downloadFull`, `pending`,
`discardPending`, `installPending`, `diagnose`, `cancel`, `resetSafety`.

## Events and errors

Stage events arrive through a `Channel` (`{ stage: 'downloading', downloaded,
total }`, `applying`, `ready`, `deferred`, `fallback`, `repairing`, `safe_mode`,
`cancelled`, …). Commands throw `{ code, message, retryable, security }`; branch
on `code`, never on `message`.

The plugin also emits `delta-updater://startup`, `delta-updater://diagnosis`
and `delta-updater://unavailable`. They may fire before the webview starts
listening, so diagnostics pages should call `configWarnings()` and `diagnose()`
instead of relying on the events alone.

## More

Full integration, publishing, manifest format and troubleshooting:
[repository docs](https://github.com/zuoqiuning/tauri-delta-updater#readme).

## License

MIT
