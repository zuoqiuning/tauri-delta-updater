# tauri-plugin-delta-updater

Typed frontend bindings for the Tauri v2 delta updater. The engine, commands and
permissions live in the Rust crate
[`tauri-plugin-delta-updater`](https://crates.io/crates/tauri-plugin-delta-updater).

## Install

```bash
npm install tauri-plugin-delta-updater
```

Add the Rust side and read the integration guide in the
[repository README](https://github.com/zuoqiuning/tauri-delta-updater).

## Usage

```ts
import { check, install, discardPending } from 'tauri-plugin-delta-updater';
```

Stage events arrive through a `Channel`; the full command list, event contract
and error codes are in the repository README.

## License

MIT
