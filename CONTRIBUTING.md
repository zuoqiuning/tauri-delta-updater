# Contributing

Thanks for taking the time to improve the engine. This project cares more about
correctness of the update path than about feature count — a patch that applies
99% of the time is a bug.

## Before you start

- Read `docs/SOURCE_MAP.md` (Chinese) — it maps "what you want to change" to files
  and the tests that guard them.
- Before touching `Cargo.toml`, the plugin version, or CI: version semantics are
  in `VERSIONING.md` and the release gate/checklist is in `docs/RELEASING.md`.
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

# frontend type package (source in plugin/guest-js, output in plugin/dist-js)
cd plugin
npm ci
npm run check   # tsc --noEmit
npm run build   # regenerate dist-js; CI fails if it drifts from sources
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
- Installer semantics (NSIS / MSI / Inno flags, silent-mode behavior) must be
  checked against the vendor's official documentation, never inferred from
  another installer. A review suggestion in this project once mapped NSIS `/R`
  to Inno `skipifsilent`; Inno's docs say `skipifsilent` means "do not run this
  entry in silent mode" — the opposite of `/R`. The lesson is that
  cross-installer analogies fail, not that the implementer forgot to read the
  docs.
- Do not add error handling for cases that cannot happen. Do add it at
  boundaries (HTTP responses, archive bytes, user-supplied paths).
- `minisign` is pinned to 0.7.9 and `generate/keys.rs::is_unencrypted_payload`
  relies on its payload layout. Before upgrading, run
  `cargo test --all-features distinguishes_encrypted_and_plain_key_payloads`
  and confirm encrypted/plain keys are still classified correctly.
- The project is maintained by a single maintainer; issue/PR response time is
  not guaranteed. Security reports follow `SECURITY.md`.

## 安全修改脚本模板

模板（`src/platform/windows/*.ps1`、`unix/*.sh`）与 Rust 编排层通过
**行为契约**耦合，改模板前先确认这四组输入/输出：

| 契约 | 输入 | 期望输出 |
|---|---|---|
| 替换 | 旧进程 PID、目标路径、暂存产物哈希 | 目标被替换且哈希一致；旧进程未退出时等待超时后回滚 |
| 回滚 | 备份路径、失败原因 | 原文件恢复、旧版本被重新拉起、`ROLLBACK_FLAG` 与 `tdx_events.jsonl` 记录原因 |
| 启动确认 | 成功标志路径、确认超时 | 看到标志后删除备份；超时写 `startup_timeout` 并回滚 |
| 整包 | journal、备份目录、文件清单 | 先加后删可逆；中途失败按 journal 反向恢复 |

对应 fixture 是 `tests/windows_script.rs` 与 `tests/unix_script.rs`：
它们真实执行生成的脚本并断言文件状态，不依赖 PowerShell/Rust 内部实现。
修改模板后至少跑：

```bash
cargo test --test windows_script --test unix_script --all-features
```

不要为了“少写几行”把回滚分支合并成“尽力而为”：脚本是宿主退出后唯一
能恢复文件的代码，失败路径必须有确定性出口。

## Commit / PR

- Small, focused commits; explain *why* in the message (the "what" is in the diff).
- When you add or remove tests, update the test counts wherever they are stated
  (README, CHANGELOG, release notes).
- PRs should state: affected platform(s), how it was verified, and any new
  fallback / failure path.
- CI must be green, including the plugin crate and the version-consistency job.
- Do not include generated line-ending changes or build artifacts.
