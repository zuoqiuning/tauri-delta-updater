# v0.2.0 — 完整更新引擎（首个三平台 CI 全绿版本）

发布日期：2026-11-07
对比基线：`v0.1.0`（2026-11-06，crates.io 已发布）

> **发布状态**：0.2.0 是本项目首个对外发布的 0.2.x 版本，包含三平台 CI
> 首轮发现并修复的 5 个平台缺陷（macOS 编译、目录切换校验、Windows 回滚与
> 短路径、写探针误判）。

0.2.0 把项目从“增量下载库”推进为**完整更新引擎**：原生二进制增量、整包
（sidecar/resources）更新、内置全量安装包回退、确定性启动确认、灰度与最低版本
控制，以及一条 `cargo delta` 接入命令，全部由同一套清单签名与回滚契约承担。

## 相对 0.1.0 的主要变化

### 1. Tauri 插件成为独立 crate

新增 `tauri-plugin-delta-updater`（`plugin/`）：命令 `check` / `install` /
`download_full` / `install_full` / `diagnose` / `cancel` / `reset_safety`，
阶段事件通过 `Channel` 推送，权限为 `delta-updater:default`。
`init_from_config()` 直接读取 `plugins.delta-updater`，并回退到官方
`plugins.updater` 配置，迁移时 `endpoints` / `pubkey` 原样可用。

### 2. 整包（多文件）增量

`core::bundle` 提供确定性归档（按路径排序、同文件树同字节）、整包清单
`latest.bundle.json` 与目录级补丁。应用前校验本地布局，解压到暂存目录并
逐文件哈希校验；Windows 逐文件替换与删除，macOS 整 `.app` 目录切换，
其它平台回退全量。中断的整包替换由修复脚本还原，不需要联网。

### 3. 内置全量安装包路径

`download_full()` 下载 `platforms[].url` 并用 mmap 验签，`install_full()`
执行平台安装器：Windows NSIS/MSI（提权、Tauri 参数约定）、macOS
`.app.tar.gz`（解包 + 目录切换）、Linux AppImage；`.deb` / `.rpm` 明确拒绝。
官方 `tauri-plugin-updater` 不再是全量回退的必需依赖。

### 4. 确定性启动握手与健康检查

替换脚本写 `staged`，新进程写 `booting`，`confirm_startup_success_with(|| ...)`
通过宿主健康检查后才写 `committed`。失败立即落盘 `health_check_failed` 并回滚，
不再等满确认超时；`Diagnosis::startup_state` 与 `last_rollback` 区分
`startup_not_started` / `startup_crashed` / `health_check_failed` / `startup_timeout`。

### 5. 签名保护的灰度与最低版本

`generate --rollout <0-100>` 写入 `platforms[].rollout`，客户端按
`sha256(install_id + version) % 100` 稳定分桶；未命中返回
`Deferred`，不下载补丁或整包。`generate --min-version` 写入受签名保护的最低版本，
低于门槛的客户端无视灰度强制更新。

### 6. `cargo delta` 接入脚手架

`cargo delta init` / `migrate` 默认只输出统一 diff，`--apply` 才原子写入
`tauri.conf.json`、`capabilities/default.json` 与 `src-tauri/Cargo.toml`；
`--check` 作为 CI 接入守卫，并提示当前是双栈（保留官方插件）还是单栈。

### 7. 可观测性与安全叙事

- 结构化回滚事件 `tdx_events.jsonl` + `Diagnosis::script_events`；
- `Diagnosis::warnings()` 启动 doctor；
- Authenticode 三态诊断（`TrustState` / `authenticode_check_skipped`）；
- README 安全矩阵直接引用官方 updater 的清单未签名问题（CVE-2026-95625）；
- 跨平台真实文件系统 CI smoke、独立 doctest job、Zucchini A/B 工具与
  Web bundle 威胁模型。

## 修复（三平台 CI 首轮发现）

- **macOS 无编译**：整包更新的平台层分发按 6 个参数调用，macOS 实现只有 5 个；
  同时对 `bundle_repair_needed` 的 macOS 分支补齐参数使用。
- **macOS 目录切换校验反转**：`verify_staging` 用 `_ok=1` 表示正常却直接
  `return`，与 shell 返回码约定相反——有效包被拒、篡改包反而放行。
  改为 `_fail=0/1`，并在 Git Bash 中真实执行两条路径验证。
- **Windows 回滚后继续执行成功流程**：`Start-Process` 对无效 PE 抛终止性错误时，
  PowerShell 5.1 会跳过 `catch` 中的 `exit`，脚本继续启动新版本、清掉回滚原因。
  `Start-Process` 现在自带 `try/catch`，整包脚本用 `$failed` 在 `catch` 外回滚。
- **Windows 8.3 短路径还原失败**：`%TEMP%` 为 `C:\Users\RUNNER~1\...` 时，
  用长度差计算相对路径会算错；改用 `Directory.EnumerateFiles`。
- **可写目录被瞬时占用误判为只读**：写探针改为唯一名字 + 失败重试一次。

## 升级注意

0.2.0 是 MINOR 版本（相对 crates.io 上的 0.1.0），新增了公开字段与枚举变体：

- `UpdateManifest.min_version`；`PlatformInfo.size` / `rollout`；
- `CheckOutcome.rollout_percentage` / `deferred_by_rollout`；
- `UpdateConfig.install_id`；`Diagnosis.startup_state`；
- `UpdateOutcome` / `UpdateResult` / `UpdateEvent` 新增 `Deferred`。

兼容性：

- 旧清单没有新字段时按“未知收益比 / 全量 / 不设门槛”处理；
- 旧客户端读取新清单会忽略未知字段；
- 签名、双端哈希、回滚与安全模式语义不变；
- 用结构体字面量构造公开结构体的宿主需要补齐新字段，其余调用方无需改动；
- MSRV 仍为 1.88。

## 验证

- 主 crate：166 passed / 1 ignored；插件：8 passed；
- clippy / fmt 全绿；Windows 12 项真实 PowerShell 脚本测试；
- 三平台 CI：10 个 job 全部通过（Windows / Ubuntu / macOS 测试、
  真实文件系统脚本、插件、doctest、MSRV、release build；运行记录见 Actions）；
- 真实 27 MB PE 相邻构建补丁占比 0.01%（`docs/bsdiff-baseline-2026-11-07.md`）。

## 已知限制

- macOS 生产验证需要 Apple Developer 签名与公证；CI 只覆盖目录切换脚本。
- Linux `.deb` / `.rpm` 不接管，交给包管理器；AppImage 提权路径需真机验证。
- Web bundle 热更新仅完成威胁模型，未实现；等外部评审。
- Zucchini 仅有独立 A/B 工具，尚未集成；需 CI 数据决策。

完整变更见 [CHANGELOG.md](../CHANGELOG.md) 的 `[0.2.0]` 条目。
