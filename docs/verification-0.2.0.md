# 0.2.0 验证记录（2026-11-07）

> 目的：记录**实际跑过什么、没跑什么**，供外部审查核对。
> 分两部分：本机 Windows 验证，以及三平台 GitHub Actions 验证。

## 环境

| 项 | 值 |
|---|---|
| 本机 | Windows x86_64，Rust stable（MSRV 1.88 由 CI 单独验证） |
| 本机构建 | `cargo test -j 1 --all-targets --all-features --locked`（默认并行度下 rustc-LLVM OOM，降并行后 166 项全通过） |
| CI | 发布提交的三平台 CI 全绿（10 个 job，运行记录见 Actions 页面对应提交） |
| 代码状态 | `v0.2.0` 发布提交 |

## 三平台 CI 结果

| Job | 结果 |
|---|---|
| Test on windows-latest / ubuntu-latest / macos-latest | 通过 |
| Real filesystem swap (ubuntu-latest / macos-latest) | 通过（真实执行 `.sh` 与 `.app` 切换脚本） |
| Plugin crate (windows-latest / ubuntu-latest) | 通过（Ubuntu 需 GTK + WebKitGTK 开发依赖） |
| Doctests (isolated target) | 通过（独立 `CARGO_TARGET_DIR`） |
| MSRV (1.88) | 通过 |
| Release build | 通过 |

### 三平台真实脚本执行证据

| 平台 | 真实脚本执行测试 | CI job | 结果 |
|---|---|---|---|
| Windows | `windows_script` 12 项（特殊字符路径、崩溃恢复、回滚、超时、篡改拒绝） | `Test on windows-latest` | 通过 |
| macOS | `macos_bundle` 3 项 + `unix_script`（目录切换、修复还原、篡改拒绝） | `Real filesystem swap (macos-latest)` | 通过 |
| Linux | `unix_script`（替换、启动标志、备份清理、AppImage/pkexec 静态断言） | `Real filesystem swap (ubuntu-latest)` | 通过 |

**0.2.0 的跨平台承诺首次由真实执行验证**；此前 macOS/Linux 只有编译检查或未运行。

## 本机已执行

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 通过 |
| 主 crate 全部测试目标 | **166 passed / 1 ignored**（连续 3 次全量重跑） |
| 插件 `cargo test` | **8 passed** |
| 插件 `cargo clippy --all-targets -- -D warnings` | 通过 |
| `cargo test --test windows_script` | **12 passed**，真实执行 PowerShell 替换脚本（另单独重复 5 次） |
| `cargo test --test cargo_delta_cli` | **4 passed**（init/migrate dry-run、apply、--check） |
| 跨目标 clippy（Linux / macOS） | 通过（`--target x86_64-unknown-linux-gnu` / `x86_64-apple-darwin`；C 依赖用空实现编译器绕过，只验证 Rust 代码） |

主 crate 明细：

| 套件 | 通过 | 忽略 |
|---|---|---|
| lib 单测 | 57 | 0 |
| `cargo-delta` 单测 | 2 | 0 |
| `cargo_delta_cli` | 4 | 0 |
| `e2e_bundle` | 7 | 0 |
| `e2e_delta` | 21 | 1 |
| `e2e_full` | 3 | 0 |
| `generate_cli` | 20 | 0 |
| `integration` | 31 | 0 |
| `public_api` | 1 | 0 |
| `signature` | 7 | 0 |
| `tracing` | 1 | 0 |
| `windows_script` | 12 | 0 |

真实产物基线：[bsdiff-baseline-2026-11-07.md](bsdiff-baseline-2026-11-07.md)
（27 MB PE，补丁 1,584 字节，0.01%）。

## CI 首轮发现并修复的问题

三平台 CI 第一次真正运行时暴露了 5 个本机无法发现（或间歇出现）的问题：

| 问题 | 触发条件 | 修复 |
|---|---|---|
| macOS 目标无法编译 | 平台层 6 参数调用 macOS 5 参数实现 | 对齐调用签名 |
| macOS 暂存校验返回码写反 | `_ok=1` 直接 `return`，与 shell 约定相反 | 改为 `_fail=0/1`，Git Bash 真实执行验证两条路径 |
| Windows 回滚后继续走成功流程 | `Start-Process` 抛终止性错误时 PowerShell 5.1 跳过 `exit` | `Start-Process` 自带 `try/catch`；整包脚本用 `$failed` 在 `catch` 外回滚 |
| Windows 8.3 短路径还原失败 | `TEMP` 为 `C:\Users\RUNNER~1\...` | `Directory.EnumerateFiles` 保证路径同形 |
| 可写目录被误判只读 | 探针文件并发/被瞬时占用 | 唯一探针名 + 失败重试一次 |

## 未执行 / 未验证

| 项 | 原因 | 下一步 |
|---|---|---|
| macOS 已签名 `.app` 目录切换 + 公证 | CI 执行的是未签名夹具；签名/公证需要 Apple Developer 环境 | 发布级真机验证 |
| Linux AppImage 提权/降权 | CI runner 非 root，只覆盖非提权路径 | Ubuntu 真机验证 |
| Zucchini A/B | 本地无 clang/cmake/ninja；workflow 需手动触发 | `.github/workflows/zucchini-ab.yml` + [zucchini-ab-plan.md](zucchini-ab-plan.md) |
| Web bundle 热更新 | 威胁模型未评审，未实现 | [web-bundle-threat-model.md](web-bundle-threat-model.md) |
| 本机 doctest | 页面文件不足（`os error 1455`） | 以 CI 独立 doctest job 为准（已通过） |

## 复现命令

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --locked
cargo test --manifest-path plugin/Cargo.toml

# 独立 target 目录跑 doctest（CI 同款隔离）
CARGO_TARGET_DIR=target-doctest cargo test --doc --all-features --locked

# 跨目标 clippy（需要对应 std target；C 依赖需可用的交叉工具链）
cargo clippy --target x86_64-unknown-linux-gnu --all-targets --all-features -- -D warnings
cargo clippy --target x86_64-apple-darwin --all-targets --all-features -- -D warnings
# 上述交叉检查需要可编译 C 依赖；无交叉工具链时可只为 Rust 代码提供空实现编译器

# 真实产物基线
cargo run --release --example simulate_update -- \
  test_run/old.exe test_run/new.exe target/delta-sim
```

## 结论

- Windows 单文件增量、整包替换、回滚、灰度、最低版本、`cargo delta` 工具均有可复现的测试结果。
- macOS 的目录切换脚本与 Linux 的替换脚本在 CI 上**真实执行**（不再只是字符串断言）；
  但签名/公证与 Linux 提权仍属于环境限制，不是发布级验证。
- doctest 以 CI 独立 job 为准，本机不声称通过。
