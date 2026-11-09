# 源码导航

按"我要改什么"定位文件，而不是从入口逐层读。修改前先读对应行的"边界"说明，避免把逻辑写到错误的层。

## 1. 功能 → 模块 → 文件 → 测试

| 想改的功能 | 主文件 | 关联文件 | 覆盖测试 |
| --- | --- | --- | --- |
| 检查更新（不下载） | `src/updater/flow.rs` (`check`) | `src/updater/fetch.rs`、`src/updater/outcome.rs` | `tests/e2e_delta.rs`、`tests/integration.rs` |
| 单文件增量（补丁 → 替换） | `src/updater/delta.rs` (`run_locked`) | `src/updater/flow.rs`、`src/core/diff.rs` | `tests/e2e_delta.rs`、`tests/integration.rs` |
| 补丁容器与并发预算（bsdiff / zstd / TDXB1） | `src/core/diff.rs`、`src/core/block_delta.rs` | `src/release/patches.rs`、`src/release/bundles.rs`、`src/bin/*/args.rs` | `src/core/diff.rs`、`src/core/block_delta.rs` 单测 |
| 跨重启待安装（pending） | `src/updater/pending.rs` | `src/updater/{delta,bundle_flow}.rs`、`src/core/artifacts.rs` | `src/updater/pending.rs` 单测、`tests/e2e_delta.rs` |
| 补丁规划（直达 / 多跳链 / 全量） | `src/updater/plan.rs` | `src/models/manifest.rs`、`src/release/patches.rs` | `src/updater/plan.rs` 单测、`src/release/patches.rs` 单测、`tests/e2e_delta.rs` |
| 系统版本门槛（`min_os`） | `src/platform/os_version.rs` | `src/updater/{flow,delta}.rs`、`src/models/manifest.rs` | `src/platform/os_version.rs` 单测、`tests/e2e_delta.rs` |
| 整包增量（多文件） | `src/updater/bundle_flow.rs` | `src/core/bundle/`、`src/platform/dir_swap.rs` | `tests/e2e_bundle.rs`、`tests/macos_bundle.rs` |
| 全量安装包下载与验签 | `src/updater/full.rs` | `src/core/downloader.rs`、`src/core/verify.rs` | `tests/e2e_full.rs`、`tests/integration.rs` |
| 全量安装执行（NSIS/MSI/AppImage） | `src/platform/full_install.rs` | `src/updater/full.rs`、`src/platform/windows.rs` | `tests/windows_script.rs`、`tests/unix_script.rs` |
| 清单格式与版本单调性 | `src/models/manifest.rs` | `src/updater/fetch.rs` | `tests/signature.rs`、`tests/integration.rs` |
| 灰度发布（百分比分桶） | `src/core/rollout.rs`、`src/updater/rollout.rs` | `src/models/manifest.rs`、`src/updater/{flow,delta}.rs` | `src/core/rollout.rs` 单测、`tests/e2e_delta.rs` |
| 清单/安装包签名校验 | `src/core/verify.rs` | `src/core/crypto.rs` | `tests/signature.rs` |
| 下载、断点续传、退避重试 | `src/core/downloader.rs` | `src/updater/fetch.rs`、`src/updater/full.rs` | `tests/integration.rs` |
| 文件替换、启动确认、回滚 | `src/platform/{windows,linux,macos,unix_script,dir_swap,shared}.rs` | `src/updater/staged.rs`、`src/core/startup.rs` | `tests/{windows_script,unix_script,macos_bundle}.rs` |
| 启动状态指针与健康检查 | `src/core/startup.rs`、`src/platform/shared.rs` | `src/updater/{flow,diagnosis}.rs` | `src/core/startup.rs`、`src/updater/mod.rs` 单测 |
| Windows 签名状态（Authenticode） | `src/platform/authenticode.rs` | `src/updater/diagnosis.rs`、`src/updater/delta.rs` | `src/platform/authenticode.rs` 单测 |
| 安全模式与失败计数 | `src/core/safety.rs` | `src/updater/flow.rs` | `src/core/safety.rs` 单测、`tests/integration.rs` |
| 环境诊断与启动 doctor | `src/updater/diagnosis.rs`（`warnings`） | `src/platform/target.rs`、`plugin/src/lib.rs` | `src/updater/mod.rs` 单测 |
| 本地验证闭环（无网络） | `src/updater/simulate.rs` | `src/core/diff.rs` | 文档示例（`cargo test --doc`） |
| 发布端（清单 / 补丁 / 报告 / 体检） | `src/release/`、`src/bin/cargo_delta/doctor.rs` | `src/bin/generate/`、`src/bin/cargo_delta/` | `tests/generate_cli.rs`、`tests/cargo_delta_cli.rs` |
| 接入脚手架（init/migrate） | `src/bin/cargo_delta/` | `serde_json`、`similar`（仅 generator feature） | `tests/cargo_delta_cli.rs` |
| Tauri 插件与命令 | `plugin/src/lib.rs` | `plugin/permissions/` | 插件 crate 内单测 |
| 对外 API 契约 | `src/lib.rs` | `tests/public_api.rs` | `cargo test --test public_api` |

测试布局：`tests/common/mod.rs` 是共用的 HTTP 服务与签名夹具；`e2e_delta` / `e2e_bundle` / `e2e_full`
按场景拆分端到端测试；`integration.rs` 覆盖底层能力；`*_script.rs` 真机执行生成的脚本
（`unix_script` 与 `macos_bundle` 仅在对应平台运行）。

## 2. 依赖方向

```
bin/generate、bin/cargo_delta ─→ release ─┐
                                          ├→ core（diff / bundle / verify / downloader / safety …）
updater ──────────────────────────────────┤        ↑                 ↑
  │           └→ platform ────────────────┘
  ├→ models（清单）
  └→ session（事件与会话） ─→ updater（仅公开 API）
plugin ─→ updater（公开 API）
```

- `core/` 不依赖 `updater/` 与 `platform/`，可单独测试。
- `platform/` 只依赖 `core/`，不感知清单与下载。
- `updater/` 各自文件之间只通过 `super::` 的公开项协作；流程方向为
  `flow → delta | bundle_flow → fetch | full`，反向依赖不存在。
- 已知例外：`models/manifest.rs` 引用 `platform/target` 的平台键（历史上如此，
  改动面大，暂不调整；新增代码不要继续加深这条依赖）。

## 3. 目录职责边界

| 目录 | 只负责 | 不要放 |
| --- | --- | --- |
| `core/` | 与平台、UI、流程无关的算法与 IO | 平台判断、事件回调、更新阶段编排 |
| `models/` | 清单数据结构与版本规则 | 网络、文件写入 |
| `platform/` | 脚本生成、进程启动、签名状态、目录切换 | 清单解析、补丁下载 |
| `updater/` | 编排与对宿主可见的结果类型 | 具体平台的命令行参数（放 `platform/`） |
| `release/` | 发布期清单、补丁与报告生成（仅 `generator` feature） | 运行时更新逻辑（那在 `updater/`） |
| `plugin/` | Tauri 命令、权限、事件桥接、前端类型包（`guest-js` → `dist-js`） | 更新算法本身 |
| `tests/` | 跨模块行为契约 | 单个私有函数的细节（放源码内单测） |

## 4. 拆分记录（`updater.rs` 2348 行 → `updater/`）

| 文件 | 行数 | 变更原因 |
| --- | --- | --- |
| `mod.rs` | 708 | 模块导航、`DeltaUpdater` 构造、共享常量与下载公共段、结果类型单测 |
| `flow.rs` | 583 | 对外入口（`check` / `update` / `install` / `prepare` / `session`）、修复与安全模式编排 |
| `delta.rs` | 294 | 单文件补丁路径：规划 → 下载 → 逐跳合并 → 校验 → 生成脚本 / 待安装记录 |
| `bundle_flow.rs` | 204 | 整包路径与整包清单解析 |
| `full.rs` | 275 | 全量安装包下载、验签与安装执行 |
| `outcome.rs` | 392 | 检查/更新结果与回退原因（对宿主的契约） |
| `staged.rs` | 252 | `Staged*` 产物与全量安装包信息 |
| `config.rs` | 440 | `UpdateConfig`、端点策略/请求头/安装模式与签名地址推导 |
| `fetch.rs` | 174 | 清单与签名下载（多端点、重试、体积上限、验签） |
| `diagnosis.rs` | 307 | 诊断快照、回滚原因读取 |
| `pending.rs` | 336 | 跨重启待安装：落盘、校验、执行 |
| `plan.rs` | 311 | 补丁规划：直达 / 多跳链 / 成本边界 |
| `rollout.rs` | 50 | 灰度分桶 |
| `simulate.rs` | 95 | 本地验证闭环 |

拆分按"变更原因"而非行数：改下载只动 `fetch.rs`/`full.rs`，改替换只动 `platform/`，
改 UI 结果只动 `outcome.rs`。公共路径全部通过 `pub use` 保持，`tests/public_api.rs`
在编译期守住这些路径。

第二轮拆分：

| 原文件 | 拆分后 | 变更原因 |
| --- | --- | --- |
| `core/bundle.rs`（1069 行） | `bundle/mod.rs`（公共导出与测试）、`bundle/archive/{mod,format}.rs`（归档与格式原语）、`bundle/patch.rs`（补丁与清单） | 归档 IO 与补丁逻辑互不相关，客户端/发布端各自只看一侧 |
| `bin/generate.rs`（965 行） | 发布实现 `src/release/`（`mod.rs` 编排 + `files/keys/patches/bundles/baseline.rs`）；CLI 只留 `src/bin/generate/` 与 `src/bin/cargo_delta/` 的薄入口 | 发布逻辑与命令行解耦，`generate` 与 `cargo delta release` 共用一份实现 |

两个发布入口都通过 `release::run_release_blocking` 在独立线程（16 MB 栈）上运行：
调试构建的 Future 比发行构建大得多，Windows 默认 1 MB 主线程栈在嵌套的
补丁/哈希/解压调用下会溢出。

脚本模板外置（`include_str!`，行为与占位符不变）：

| 原位置 | 现文件 |
| --- | --- |
| `platform/windows.rs` 三个 `r#"…"#` 模板 | `platform/windows/{updater,bundle,repair}.ps1` |
| `platform/unix_script.rs` 主模板 | `platform/unix/updater.sh` |
| `platform/dir_swap.rs` 两个模板 | `platform/unix/{dir_swap,repair}.sh` |

## 5. 开发与验证

```bash
cargo fmt
cargo clippy -j 1 --all-targets --all-features -- -D warnings
cargo test -j 1 --all-features
cargo test -j 1 --test public_api          # 公共 API 路径守卫
cargo check -j 1 -p tauri-plugin-delta-updater

# 前端类型包（构建产物入库，CI 校验可重新生成）
cd plugin && npm ci && npm run build
```

低内存机器（并行 rustc 报 `os error 1455`）统一用 `-j 1`；这不是代码问题。
macOS / Linux 专属代码依赖 CI 矩阵验证，本机只能跑平台无关测试。

## 6. 后续维护建议

**必须守住**

- 新增公共符号时同步更新 `tests/public_api.rs`；删除/改名先让守卫失败，再改代码。
- 单文件超过约 600 行、或同时承担两类变更原因时再拆，先看调用边界，不要按行数切。
- 新增平台行为进 `platform/`，新增流程分支进 `updater/flow.rs`，不要在 `core/` 里写平台特例。
- 脚本模板渲染后会执行 `shared::assert_rendered`（运行时检查，release 同样生效）：改模板时漏替换占位符会直接返回错误，不会写出无法执行的脚本。

**建议下一步**

- 增加开发命令入口（`justfile` 或 `docs/开发与验证.md`），把上面的验证命令固化。

**暂不改**

- `models → platform/target` 的依赖倒置（改动面大、收益有限）。
- `platform/mod.rs` 的 `cfg` 聚合方式、`docs/设计文档.md` 的单文件长度。
- npm/TS 前端包、benchmark 与 fuzz（当前发布形态不需要）。
