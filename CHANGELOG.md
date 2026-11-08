# Changelog

本文件记录 `tauri-delta-updater` 的版本变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### 修复

- `cargo delta init --check` 可无参数运行；缺少 capability 文件时生成合法的
  `identifier` / `windows`；`--check` 会拒绝过期插件依赖版本。
- `cargo delta init --check` 静态校验端点与公钥：非 `http(s)`、未显式允许的
  非回环明文 HTTP、占位符或截断的公钥直接失败，不再等到应用首次检查才暴露；
  多 capability 项目复用已有文件并提示实际写入目标。
- 明文 HTTP 检查覆盖 `signature_url` 与 `bundle_endpoint`，避免清单走 HTTPS、
  签名走 HTTP 的降级窗口。
- 插件配置 `proxy` / `headers` 自建客户端时恢复默认连接/读空闲超时
  （15s / 60s），弱网或黑洞端点不再无限挂起。
- 从官方 updater 迁移时，`windows.installMode` 与
  `dangerousInsecureTransportProtocol` 不再静默失效：启动警告会指出对应的
  `install_mode` / `allow_insecure_transport`（HTTPS 部署不产生噪音）。
- Unix 替换在 `cp` 中途失败时无条件用备份还原，半截目标不再被当旧版本启动。
- Windows 整包已 `committed` 后不再被误判为未完成替换；确认成功后清理备份与 journal。
- 全量下载/安装与增量路径共用 `UpdateLock`，并复用 `min_os` / 灰度门槛；
  修复未完成时返回 `repair_required`；`CheckOutcome::full_download()` 不再绕过
  `check()` 隐藏的全量地址。
- `signature_url` 真正用于主端点；`cargo delta doctor` 复用同一条 `.sig` 推导并
  默认镜像客户端的严格验签策略（新增 `--trust-policy` / `--allow-legacy`）。
- 全量安装包验签遵守 `allow_legacy_signatures`；多签时逐块校验 `tdu:v=` 版本绑定。
- 大文件验签、完整性校验与 WinVerifyTrust 改到 `spawn_blocking`。
- 脚本占位符守卫从 `debug_assert` 改为运行时错误；未知配置字段会在启动日志中提示。
- 全量 `.exe` 未显式配置 `installer_kind` 时按内容识别 Inno/NSIS，降低漏配导致弹向导的风险。
- pending 路径改用 canonicalize，阻止 `work_dir/../evil` 逃逸；`os_version` 缺失段补零。
- 日志/事件超过 2 MB 时轮转保留 `.1`；测试与 CI 清理噪声。

### 变更（公开 API）

- `CheckOutcome`、`PatchInfo`、`Rollout`、`BundleFile`、`BundlePatch`、
  `BundleManifest` 标记 `#[non_exhaustive]`：改用 serde / builder 构造，下游
  `match` 保留通配分支。
- `CheckOutcome` 新增结构化 `unsupported_os: Option<OsRequirement>`。
- `CheckOutcome` / `UpdateOutcome` / `UpdateResult` / `FullDownloadOutcome`
  标记 `#[must_use]`：忽略结果会触发编译警告。
- `FullDownloadOutcome` 新增 `Deferred` / `UnsupportedOs`；`FullDownloadDto`
  与前端类型同步。
- 新增公共入口：`default_signature_url`、`manifest_version_bindings`、
  `verify_file_signature_multi_with_legacy`、`OsRequirement`、`validate_public_key`、
  `default_client_builder`、`UpdateConfig::signature_url()`。
- 插件配置新增 `signature_url`：签名可独立托管，缺省仍按清单地址推导。
- 新增 `UpdateResult::requires_exit()`：`Updated` / `RepairRequired` 为 `true`，
  宿主不必再自行维护需要退出的变体列表。
- npm 绑定新增 `requiresExit(result)`，与 Rust 的 `requires_exit()` 对齐。
- 插件新增 `delta-updater:read` / `delta-updater:update` / `delta-updater:full`
  最小权限集（`default` 行为不变）。
- 文档拆分：README 保留快速接入，新增 `docs/USAGE.md`、`docs/PUBLISHING.md`、
  `docs/TROUBLESHOOTING.md`。

## [0.3.0] - 2026-11-08

### 兼容性

**0.3.0 是 MINOR 版本，包含以下破坏性变更**：

1. `InstallerKind` 新增 `Inno` 变体并标记 `#[non_exhaustive]`：下游 `match`
   需要保留 `_ => {}` 通配分支。
2. `UpdateConfig` 新增 `installer_kind` 字段并标记 `#[non_exhaustive]`：
   结构体字面量构造需改用 builder（推荐）或 serde。
3. `UpdateManifest` / `PlatformInfo` 标记 `#[non_exhaustive]`：外部字面量构造
   需改为按真实发布格式反序列化（或 builder）。
4. 安全默认收紧：`allow_legacy_signatures` / `allow_insecure_transport`
   默认值从 `true` 改为 `false`。使用第三方旧格式签名或非回环明文 HTTP 的
   宿主必须显式开启；本地回环 `http://127.0.0.1` 仍默认允许。

非破坏性新增：`install_with_mode` 签名不变，新增
`install_with_mode_and_kind`；`Diagnosis` 本就是 `#[non_exhaustive]`，新增
`installer_kind` 字段不影响已有匹配。

### 新增

- **Inno Setup 全量安装**：`InstallerKind::Inno` + `UpdateConfig::installer_kind`
  （插件配置 `"installer_kind": "inno"`）。参数映射：
  `Passive → /SILENT /NORESTART /SP-`、`Quiet → /VERYSILENT /NORESTART /SP-`、
  `BasicUi → 空`（Inno 默认 UI）。`.exe` 无法区分 NSIS 与 Inno，必须显式声明；
  配置与后缀明显矛盾时在安装阶段返回 `DeltaError::Config`。
  **边界**：该配置只被引擎 `install_full` 消费；官方 `tauri-plugin-updater`
  不支持 Inno，只会传 NSIS 参数。Inno 没有 `/R` 等价参数，静默安装后的重启
  必须由 `.iss` 的 `[Run]` 承担，且不能加 `skipifsilent`（其语义是“静默不执行”）。
- **一条命令发布**：`cargo delta release`，与 `generate` 共用同一实现
  （参数命名不同：基线用可重复的 `--from`，目录或单个可执行文件）。发布逻辑
  抽到库的 `release` 模块（`ReleaseOptions` / `run_release` / `run_release_blocking`）。
- **发布源体检**：`cargo delta doctor <latest.json>`，校验清单签名与
  `tdu:v=` 版本绑定、安装包可下载性与体积、补丁 SHA-256、补丁链可达性，
  以及整包清单（`--bundle-url`）的补丁哈希与路径安全。`--deep` 额外下载
  安装包验签；`--json` 输出机器可读报告；任何错误退出码非零。
- **`generate --sha256sums`**：一条命令产出清单、签名、公钥、安装包与全部
  补丁的 `SHA256SUMS`，上传前即可核对 CDN 完整性。
- **`generate --legacy-patch`**：输出未压缩 `BSDIFF40`，供不认 zstd 容器的
  旧引擎客户端；补丁哈希与体积按未压缩文件计算。
- **回滚提示**：更新失败并恢复旧版本后，Windows 显示消息框、macOS/Linux
  发送桌面通知（独立进程/非阻塞，无交互桌面自动跳过）。默认开启，
  `UpdateConfig::rollback_notice(false)` 或插件配置 `rollback_notice` 关闭；
  直接调用 `PlatformUpdater` 生成脚本时默认不弹提示。
- **可观测性**：`Diagnosis::rollback_history` 暴露完整回滚链；替换脚本新增
  正向阶段事件（`download_completed` / `script_ready` / `replace_started` /
  `startup_confirmed`），进程退出后仍能定位卡点。

### 修复

- **跨代际升级不确认启动**：`confirm_startup_success_if_pending` 现在把旧引擎
  脚本残留识别为替换证据；新引擎首发仍应调用无条件确认，避免依赖兼容判据。
- **已准备好的待安装产物被误删**：清理会保留 `tdx_pending.json` 引用的路径；
  下载更高版本失败、清单为最新或灰度未命中时不再删除已就绪产物；新记录写入
  成功后才清除旧记录。
- **`cargo delta init` 依赖与检查**：默认写 `"0.3"`（同 minor 补丁可升级），
  `--registry` 写精确 `"0.3.0"`；`--check` 对 git 依赖只告警不失败；
  未知平台使用 `{{target}}` / `{{arch}}` 时直接报错，而不是展开为
  `unknown-unknown`。
- **平台边界与文案**：修正 Linux `min_os` 的“内核 release ≠ 发行版版本”说明；
  `dir_swap` 校验标志改名为 `_verify_failed`，避免返回码语义反转。
- **CI 发布**：插件发布对“npm 单独重试”幂等；Ubuntu 发布 job 安装 Tauri
  依赖；已发布的 crate 跳过重复发布。

### 升级注意

- Inno 宿主：配置 `installer_kind`，并确保全量路径走引擎 `install_full`
  （官方 updater 不会识别 Inno 参数）；`.iss` 的 `[Run]` 负责重启，不能加
  `skipifsilent`。
- 用结构体字面量构造 `UpdateConfig` / `UpdateManifest` / `PlatformInfo`
  的代码需要改用 builder 或 serde；这是本次 MINOR 的破坏性部分。
- 默认拒绝非回环明文 HTTP 与旧格式 minisign 签名；需要兼容的宿主显式开启
  对应开关，并确认这是有意放宽。
- 旧清单、旧补丁文件与已发布客户端不受影响；历史补丁仍需保留在 CDN 上。

## [0.2.1] - 2026-11-07

### 兼容性

**0.2.1 相对 0.2.0 无破坏性变更**：现有接入代码零改动即可升级。

- 新增的都是**可选 API**（`additional_public_keys` / `trust_policy` 等）或
  **新增可选配置项**（`channel`、`min_os`、`install_on_next_launch`）。
- `UpdateOutcome` / `UpdateResult` 新增了 `UnsupportedOs` 变体；两个枚举都标记了
  `#[non_exhaustive]`，下游已有的通配分支不受影响（按 `VERSIONING.md` 属于 PATCH）。
- `DeltaError` 新增 `ChainHopFailed` 变体（同样 `#[non_exhaustive]`）：错误码与
  安全分类委托给底层错误，`Display` 额外指出失败跳数。
- 清单新增字段（`min_os` / `notes_i18n`）全部可选，旧清单与旧客户端继续工作。

### 修复（接入与运行时）

- **`cargo delta init` 写出的插件依赖无法构建**：模板把版本硬编码为 `0.1`，
  而插件未发布、仓库版本已是 0.2.0。现在默认写可用的 git 依赖，`--registry`
  才写 `"<CARGO_PKG_VERSION>"`；并新增测试断言版本必须等于工具版本。
- **插件初始化失败会阻止宿主应用启动**：工作目录只读或配置缺失时，
  Tauri 会把插件 setup 错误升级为 `PluginInitialization` 并使 `App::build` 失败。
  现在统一降级为 `updater_unavailable` 状态，应用照常启动，命令返回稳定错误码。
- **整包不可用时错误地跳过单文件增量**：Linux / 非 `.app` 的 macOS 配置了
  `bundle_endpoint` 后会直接回退全量。现在先尝试单文件补丁，只有两者都不可用
  才回退，`check()` 同步报告 `has_patch`。
- **生成补丁可能 OOM 中止**：新增 4 GiB 工作集上限
  （`DeltaError::PatchWorkingSetTooLarge`），`generate` 跳过超限基线并写入报告，
  而不是在分配失败时 abort 整个发布进程。
- **Windows 提权被拒时可能拉起第二个实例**：回滚路径现在按 PID + 路径确认旧进程
  已退出，仍在运行时跳过重启。
- **macOS 全量安装包在切换前被删除**：删除时机移到启动确认之后，切换失败时
  仍可直接重试。
- **`cargo delta` 重排 `tauri.conf.json`**：启用 `serde_json/preserve_order`
  后只插入新键，不再产生整文件 diff。
- **清单与签名串行下载**：两者改为并行拉取，检查更新少一个 RTT。
- **替换脚本可能静默不启动**：启动改为绝对路径并先校验文件存在；
  缺失时返回明确错误，而不是 spawn 一个必然失败的 PowerShell。
- **"已是最新"的检查会删除已就绪的待安装产物**：清理原本在版本/灰度判断
  之前执行，`UpToDate` / `Deferred` 也会把上次 `prepare` 的脚本与产物删掉。
  现在只在真正要下载新产物时才清理，避免一次网络故障或灰度未命中就让
  已完成的更新白费。

### 新增

- 发布渠道与端点占位符：插件配置 `channel`（默认 `stable`），端点支持
  `{{channel}}`，与 `{{target}}` / `{{arch}}` / `{{current_version}}` 一起展开；
  非法渠道名在启动阶段报错。
- 多语言说明与自定义元数据：`generate --notes-i18n LOCALE=TEXT`（可重复）写入
  受签名保护的 `notes_i18n`，客户端用 `manifest.notes_for(locale)`
  （精确 → 语言前缀 → `notes`）；未识别的清单顶层字段保留在
  `UpdateManifest::extra` / `BundleManifest::extra`，`--existing` 发布时不会丢失。
- 最低系统版本门槛：`generate --min-os` 写入受签名保护的
  `platforms[].min_os`；`check()` 隐藏补丁与全量地址，`update()` 在**任何下载前**
  返回 `UpdateOutcome::UnsupportedOs`（含要求版本与本机版本）。本机版本探测
  失败时不阻断（未知 ≠ 不满足）；Windows 用 `RtlGetVersion`，macOS 用
  `sw_vers`，Linux 用内核 release。新增 `current_os_version()` 公开接口。
- 多密钥信任：`UpdateConfig::additional_public_keys` + `TrustPolicy`
  （`AnyOf` 默认 / `AllOf` 要求全部签名），插件配置 `additional_pubkeys` / `trust_policy`；
  `.sig` 与 `platforms[].signature` 支持多个 minisign 签名块，
  `cargo delta generate --secondary-private-key` 可双签清单与安装包，
  用于密钥轮换而不打断旧客户端。
- 多跳补丁链：没有直达补丁时按最多 3 跳依次应用（`0.1 → 0.2 → 0.3`），
  每一跳都用签名清单里的 `target_hash` 校验中间产物，再作为下一跳输入；
  链的总大小超过安装包 80% 时回退全量。`cargo delta generate` 自动保留
  能链到新版本的历史补丁并按 `MAX_DELTA_CHAIN_HOPS` 裁剪，清单不会无限膨胀。
  历史补丁文件必须继续留在 CDN 上，否则链会在下载阶段断掉。
- 插件新增 `prepare` / `launch` 命令与 `UpdateResult::Ready`：先下载合并、
  由前端决定退出时机（保存草稿、二次确认），再触发替换。
- 插件配置新增 `work_dir` / `wait_timeout_secs` / `confirm_timeout_secs` /
  `proxy`，无需写 Rust 即可覆盖运行时行为。
- 启动诊断（`diagnose().warnings()`）现在写入 `tdx_update.log` 并发出
  `delta-updater://diagnosis` 事件；重新发布 `delta-updater://unavailable`
  事件说明不可用原因。
- `DeltaUpdater::prepare_with_events`：可观察、可取消的 prepare 路径。
- `UpdateConfig::allow_legacy_signatures(bool)` 与插件配置
  `allow_legacy_signatures`：默认仍接受旧格式（兼容），安全要求高的宿主
  可设为 `false` 只接受 pre-hash 签名；新增 `verify_manifest_signature_with`。
- `cargo delta`：`--registry` 明确提示“插件尚未发布”，`--check` 失败时给出
  修复命令，接入后的下一步包含退出姿势/单实例/残留文件的真机检查清单。
- 多端点故障转移：`UpdateConfig::fallback_endpoints` / `FallbackPolicy`
  （默认 `transport`：连接失败、超时、5xx、429 才切换，404 不切换；
  验签失败永远终止）；插件 `endpoints` 数组的其余项正式作为备用端点。
- HTTP 请求头：`UpdateConfig::headers` 与插件 `headers`，覆盖清单、签名、
  补丁与全量安装包四条请求链路（仅限内置客户端）。
- Windows `install_mode`：`passive`（默认）/ `basicUi` / `quiet`，
  插件配置同名键；`quiet` 无法自行提权。
- 明文 HTTP 诊断：`Diagnosis.insecure_transport` 告警 +
  `allow_insecure_transport`（默认兼容，设 `false` 在构造期拒绝非回环 http）；
  `delta.check` 结构化事件也带上 `insecure_transport` 字段，便于监控层捕获。
- 跨重启待安装：`prepare` 未立即启动替换时把产物落盘为 `tdx_pending.json`，
  下次启动由 `pending_update()` / `install_pending()` 校验并执行。
  记录必须满足：目标版本严格高于当前版本、脚本与产物位于工作目录内、
  单文件产物重算 SHA-256 与 prepare 时的 `target_hash` 一致；启动时不联网。
  插件新增 `pending` / `discard_pending` / `install_pending` 命令与
  `install_on_next_launch` 配置（默认关闭，开启后启动时自动安装并退出进程）。
  `discard_pending()` 用于"服务端已有更高版本"时先丢弃旧记录，避免先装旧版本。
- 前端类型包：`plugin/guest-js` + `plugin/dist-js`（npm 名 `tauri-plugin-delta-updater`），
  提供 `check` / `install` / `prepare` / `launch` / `diagnose` 等函数与完整事件/结果类型；
  未配置 `NPM_TOKEN` 时发布流程自动跳过 npm。
- 核心 crate 与插件合并为一个 Cargo workspace：版本只在 `[workspace.package]` 声明一次，
  依赖图与 lockfile 统一，`cargo test` 等命令不再需要两套 manifest。
- CI：cargo 缓存、`--no-fail-fast`、独立依赖审计（每周 `cargo audit`）、
  核心/插件/前端包版本一致性检查；发布流程同时发布核心、插件与 npm 包。
- `SECURITY.md` 与 `CONTRIBUTING.md`。

### 变更

- `cargo delta init` 默认写 git 依赖（插件发布前唯一可直接构建的安装方式）；
  发布后可用 `--registry` 切换为版本依赖。
- 32 位目标下补丁线程栈由 512 MB 下调为 64 MB。

### 升级注意

- `UpdateResult` 新增 `Ready` 变体（`#[non_exhaustive]`，外部 `match` 保留通配分支即可）。
- `UpdateConfig` 新增公开字段 `allow_legacy_signatures` / `fallback_manifest_urls` /
  `fallback_policy` / `headers` / `install_mode` / `allow_insecure_transport`：
  用 builder 的调用方无需改动；用结构体字面量构造的调用方需要补齐这些字段。
- `Diagnosis` 新增公开字段 `insecure_transport`：同样需要字面量调用方补齐。
- 破坏性变更：无。清单格式、签名默认语义与替换脚本行为保持不变。

## [0.2.0] - 2026-11-07

### 修复（三平台 CI 验证发现）

- **macOS 整包切换的暂存校验返回码写反**：有效暂存目录被判定为失败
  （更新被拒绝），被篡改的目录反而通过校验进入切换。`verify_staging`
  改为按 shell 返回码约定（`_fail=0` 通过、`_fail=1` 失败）。
- **Windows 回滚路径会继续走成功流程**：`Restart-Old` 里的 `Start-Process`
  对无效 PE 抛终止性错误后，PowerShell 5.1 会跳过 `catch` 中的 `exit`，
  脚本继续启动新版本、删除回滚原因并重写 state，60 秒后再二次回滚。
  `Start-Process` 现在自带 `try/catch`，整包脚本改用 `$failed` 标志在
  `catch` 外统一回滚。
- **Windows 8.3 短路径下备份还原写到错误位置**：CI 的 `%TEMP%` 是
  `C:\Users\RUNNER~1\...`，`Get-ChildItem` 返回长路径时用长度差算相对路径
  会算错。改用 `Directory.EnumerateFiles`，保证返回路径与备份根目录同形。
- **可写目录被瞬时占用误判为只读**：写探针文件名带 PID/序号/时间戳，
  失败后换名重试一次，避免杀毒软件/索引器短暂持有探针文件时返回
  `WorkDirNotWritable`。
- **macOS 目标无法编译**：平台层按 6 个参数分发整包更新，但 macOS 实现
  只接受 5 个；同时修正 `bundle_repair_needed` 在 macOS 分支的未使用参数。
- **Unix 平台专属 clippy**：把 Unix 启动替换脚本的 `launch_updater` 移到
  测试模块之前，消除 `items_after_test_module`（Windows 因 `cfg(unix)` 看不到该函数）。
- **CI 可观测性与依赖**：插件 Ubuntu job 安装 GTK/WebKitGTK 开发依赖；
  测试失败时把 libtest 失败详情块写成 annotation，无需管理员权限下载日志。

### 升级注意（相对 0.1.0）

0.2.0 是 MINOR 版本，因为新增了若干公开字段与枚举变体：

- `UpdateManifest` 新增 `min_version`；`PlatformInfo` 新增 `size` / `rollout`。
- `CheckOutcome` 新增 `rollout_percentage` / `deferred_by_rollout`；
  `UpdateConfig` 新增 `install_id`；`Diagnosis` 新增 `startup_state`。
- `UpdateOutcome` / `UpdateResult` / `UpdateEvent` 新增 `Deferred`。
  三者都是 `#[non_exhaustive]`，但按本项目发布策略仍按 MINOR 处理；
  外部 `match` 请保留通配分支。

兼容性：

- 旧清单没有 `size` / `rollout` / `min_version` 时分别按“未知收益比 / 全量 / 不设门槛”处理；
  旧客户端读取新清单会忽略未知字段。
- 现有签名、双端哈希、回滚与安全模式语义不变。
- 如果宿主用结构体字面量构造上述公开结构体，需要补齐新字段；
  通过构造器 / `serde` 读取的调用方无需改动。
- MSRV 仍为 1.88。

### 可观测性与发布端防错

- **结构化回滚事件**：替换 / 修复脚本把回滚写成 JSON Lines
  （`tdx_events.jsonl`，字段 `event` / `reason` / `time`），`Diagnosis` 新增
  `events_path` 与 `script_events`（最近 20 条，已解析为 `ScriptEvent`，坏行忽略）。
  宿主下次启动时可逐行解析完整回滚链路，不必再用正则读自由格式日志。
- **Unix 单文件回滚原因落盘**：`tdx_update.sh` 现在同时写 `tdx_rollback.txt`，
  `Diagnosis::last_rollback` 在 Linux / macOS 裸二进制路径上也可用。
- **`generate` 参数防错**：`--installer` 与 `--app-binary` 内容相同时直接失败，
  避免补丁基底与全量下载地址指向错误的文件。
- **官方端点占位符**：插件配置的 `endpoints` / `bundle_endpoint` 支持
  `{{target}}`、`{{arch}}`、`{{current_version}}` 自动展开；新增可选
  `bundle_type`（`nsis` / `msi` / `appimage` / `app`）用于展开
  `{{bundle_type}}`，未配置时启动阶段给出明确错误。
- **Unix 模板跨平台渲染测试**：`unix_script` 改为跨平台编译（仅 `launch_updater`
  留在 Unix），Windows CI 也能验证 sh 模板的占位符、回滚标记与事件流路径。
- **诊断摘要与 Linux 降权探测**：`Diagnosis::user_message()` 返回面向用户的
  一句话摘要；Linux 上新增 `privilege_tools_available`（是否找到 `runuser`/`su`）。
- **Authenticode 三态可观测**：`platform::authenticode` 新增 `TrustState`
  （`Trusted` / `NotTrusted` / `Inconclusive`），`Diagnosis::authenticode_check_skipped`
  区分"离线导致检查未得出结论"与"确实未签名"。拦截规则不变（`Inconclusive`
  仍按不可信处理），`authenticode` 字段语义与类型也不变。
- **清单写入安装包体积**：`generate` 把安装包字节数写入受清单签名保护的
  `platforms[].size`；新增 `CheckOutcome::patch_size_ratio()` /
  `patch_size_ratio_for()`，客户端可在下载前得到 `patch_size / size`
  的真实占比（**数值越低收益越大**）。旧清单没有 `size` 时返回 `None`，不产生猜测值。
- **签名保护的百分比灰度**：`generate --rollout <0-100>` 写入
  `platforms[].rollout`；客户端按 `sha256(install_id + version) % 100` 稳定分桶，
  未命中返回 `UpdateOutcome::Deferred` / `UpdateResult::Deferred` /
  `UpdateEvent::Deferred`，不请求补丁。安装 ID 持久化在 `tdx_install_id`，
  也可用 `UpdateConfig::install_id(...)` 注入；0 暂停、100 全量且不写字段。
- **启动 doctor**：`Diagnosis::warnings()` 返回可操作的配置缺口
  （工作目录不可写、环境不支持单文件增量且未配整包、Linux 缺少降权工具），
  插件注册时打印到启动日志。
- **三态启动指针与健康检查**：替换脚本写 `staged`，新进程写 `booting`，
  `confirm_startup_success_with(|| ...)` 通过健康检查后才写 `committed`；
  失败立即落盘 `health_check_failed` 并回滚，不再等满确认超时。
  `Diagnosis::startup_state` 暴露当前指针，`last_rollback` 区分
  `startup_not_started` / `startup_crashed` / `health_check_failed` / `startup_timeout`。
- **回滚原因不再被覆盖**：`Restart-Old` / `restart_old` 保留已经写入的具体原因，
  不再用笼统的 `update_failed` 覆盖 `startup_timeout` / `health_check_failed`。
- **最低版本强制更新**：清单新增受签名保护的 `min_version`，
  `generate --min-version` 写入；低于门槛的客户端无视 `rollout` 比例强制更新。
- **`cargo delta init/migrate`**：独立的 `cargo-delta` 子命令，把配置、capability
  与 `src-tauri/Cargo.toml` 依赖一次写好；默认只输出统一 diff，`--apply` 才落盘。
  `migrate` 默认保留官方插件作为全量回退，`--remove-official` 可完全替代。
  `init --check` 供 CI 做接入守卫，缺失项以非零状态退出；
  通过时额外提示当前是双栈（保留 `plugins.updater`）还是单栈。
- **CI doctest 隔离**：doctest 拆成独立 job 并使用独立 `CARGO_TARGET_DIR`，
  避免与测试/构建产物混用后出现 `found staticlib instead of rlib`；
  日志作为 artifact 上传。本机隔离重跑仍受页面文件限制（3/22，`os error 1455`），
  最终以 CI 结果为准。
- **跨平台真实文件系统 smoke**：新增 `real-fs-smoke` job，在 Ubuntu/macOS
  真实执行 `unix_script` / `macos_bundle` 替换脚本，与 Windows 的 12 项
  PowerShell 真实执行测试对称；签名/公证仍不在覆盖范围。
- **bsdiff 真实基线**：27 MB PE 相邻构建的补丁占比 0.01%，
  并给出“待采集场景 × 预期范围”矩阵与 `old_binaries/` 积累到 5 对后的重采样路径。
- **Zucchini A/B CI 入口**：`.github/workflows/zucchini-ab.yml`（手动触发，Ubuntu + prebuilt），
  生成“仅版本串变化 / 一个模块重编译”两份真实 ELF，上传
  `zucchini-ab-<date>.md` artifact；本地 Windows 用 `--no-default-features` 从源码构建。
- **验证记录**：本轮实际执行的测试、未验证的平台与复现命令已随发布记录归档；
  doctest 结果以 CI 独立 job 为准。
- **测试补齐**：语义化版本边界（`0.10.0 > 0.9.0`、预发布）、清单签名地址的
  查询参数、全量安装包后缀推断（带 token / 无扩展名）均有单测；
  新增灰度分桶单测与"0% 灰度不下载补丁"的端到端测试。
- **Zucchini A/B 工具**：独立的 `tools/zucchini-ab` crate（不进入主依赖），
  真实 PE/ELF 上对比 bsdiff+zstd / Zucchini raw / Zucchini+zstd；
  采集门槛与数据记录格式见 `docs/proposals/zucchini-ab-plan.md`。
- **Web bundle 威胁模型**：`docs/proposals/web-bundle-threat-model.md` 定义准入条件，
  在签名覆盖 URL、路径穿越、原子激活与独立状态机评审通过前不写替换代码；
  新增 8 个 open questions、Review log 与可直接粘贴的 issue 草稿；
  并加入与 `tauri-plugin-hot-update` 的逐项对照，明确“不再以三态回滚作为差异化卖点”。
- **CI**：插件 job 扩展到 Windows + Ubuntu 矩阵（示例应用只在 Windows 检查，
  Linux 构建需要额外的 webkit2gtk 系统依赖）。

### 整包（多文件）更新引擎

- **确定性整包容器**：`core::bundle` 按路径排序构建归档，同一文件树永远产出
  同一份字节；构建与提取都流式处理，不把整包读进内存。
- **文件清单校验**：`BundleFile { path, size, sha256 }` 描述整包布局；
  应用前先校验本地安装目录，任何缺失或改动都会拒绝应用（回退全量）。
- **生成/应用 API**：`create_bundle_patch` / `apply_bundle_patch` 完成
  "目录 → 归档 → 差分 → 提取暂存"闭环，补丁与重建产物都经过双端哈希校验，
  并拒绝绝对路径、`..` 与盘符等路径穿越。

### 客户端整包更新

- **`UpdateConfig::bundle_manifest_url(...)`**：指向已签名的 `latest.bundle.json`。
  配置后 `check()` / `update()` / `install()` 优先走整包，覆盖主程序、sidecar 与
  resources；签名地址自动推导为同名 `.sig`。
- **`UpdateOutcome::Bundled { staged: StagedBundle }`**：`UpdateResult` 映射为
  `Updated`；`StagedBundle` 提供暂存目录、文件清单、脚本路径与幂等的 `launch()`。
- **多文件替换脚本（Windows）**：逐文件校验暂存哈希、备份旧内容、复制变化文件、
  删除新版本已移除的文件；任一文件失败或启动确认超时都会整体回滚，并写入
  `event=delta.rollback.triggered`。
- **macOS `.app` 目录切换**：以整个 `.app` 为单位备份与换位，避免逐文件改写
  破坏代码签名；切换前对已签名包执行 `codesign --verify --deep --strict`，
  新版本确认启动时删除备份，未确认则下次整体回滚。应用目录不可写时不做任何
  文件操作，旧版本继续运行。
- **整包归档 v2**：条目额外携带类型与 Unix 权限位，符号链接与可执行位可原样
  重建（macOS `.app` 的框架链接与主程序可执行位是签名/启动的前提）。
- **安全回退**：整包清单缺失、版本不一致或没有当前版本的补丁时回退全量下载；
  不支持的平台返回 `EnvironmentUnsupported`，不会只替换主程序留下不匹配的
  sidecar/resources。

### 发布端

- **基线自动滚动**：`--archive-dir` 在发布成功后把当前主程序归档为下一版基线
  （文件名已含版本与架构则原样保留，否则补全成 `App_0.1.1_x86_64.exe`），
  不再需要手工整理 `old_binaries/`。
- **显式基线**：`--old-binary` 可重复指定，与 `--old-dir` 合并；显式文件的
  版本/架构/平台校验失败会直接报错，而不是静默跳过。
- **发布报告**：`--report` 输出 JSON（每个基线的补丁体积、占比、跳过原因），
  便于 CI 摘要与发布审计。
- **空补丁闸门**：`--fail-on-empty` 让"一个补丁都没生成"直接失败，
  防止静默发布一份只能全量下载的清单。
- **发布通道与灰度**：`generate --channel beta` 输出 `latest-beta.json` /
  `latest-beta.bundle.json`（stable 保持旧文件名）。客户端把 `manifest_url`
  指向对应文件即可参与灰度；验证后按 stable 再发一次即完成放量。
  通道名会进入文件名，因此只允许小写字母、数字、`-`、`_`。
- **整包发布模式**：`--new-app-dir` + `--old-app-dir`（可重复，目录名带版本）
  生成目录级补丁，并输出独立签名的 `latest.bundle.json`（含 base/target 文件清单）；
  旧客户端继续使用 `latest.json`，互不影响。macOS `.app` 的版本号可以写在父目录
  （`v0.1.1/MyApp.app`），不必给 bundle 改名。

### 全量路径（第 2 阶段：安装执行）

- **`DeltaUpdater::install_full()` / `install_full_with_events()`**：下载 + 验签后
  直接执行平台安装器并返回 `Updated`（调用方需立即退出进程）。
- **Windows 安装器执行**：与官方 updater 的约定一致——NSIS 用被动模式
  ` /P /UPDATE /R`（`/UPDATE` 标记"这是更新"，`/R` 安装后重启应用），
  MSI 用 `/passive /promptrestart AUTOLAUNCHAPP=True`；提权交给安装器自己的
  manifest（`ShellExecuteW open`），per-user 安装不会无谓弹 UAC，
  用户取消提权时同步报错 `full_install_failed`；未知扩展名一律拒绝执行。
- **macOS `.app.tar.gz`**：系统 `tar` 解包 → 定位 `.app` → 复用整包目录切换脚本
  （`codesign --verify`、备份换位、启动确认、失败回滚）。
- **Linux AppImage**：复用单文件替换脚本（备份、覆盖、`chmod +x`、启动确认、回滚），
  哈希来自验签后的安装包字节。
- `.deb/.rpm` 与跨平台类型的安装包会被明确拒绝（`full_install_failed`），
  不尝试用错误的执行器运行。

- **启动确认清理安装包**：新版本确认启动成功后删除工作目录里的全量安装包，
  避免几十到几百 MB 长期占用用户缓存；用户自己的文件不受影响。

### 全量路径（第 1 阶段：下载与验签）

- **`DeltaUpdater::download_full()`**：从已验签清单取 `platforms[].url`，
  下载到工作目录（`tdx_full_installer_<version><扩展名>`），
  再用 `verify_file_signature`（只读 mmap，避免整包读入内存）校验
  `platforms[].signature`。版本不高于当前版本时返回 `UpToDate`，不发起下载。
- **`download_full_with_events()`**：发出 `Checking` / `DownloadingFull` 事件，
  支持 `CancelToken` 取消；失败时删除安装包。
- **下载前磁盘预检**：拿到 `Content-Length` 后先查剩余空间，避免下到一半才失败。
- 安装包体积上限 4 GB，防止被劫持的服务端灌盘。

### 安全

- **Windows Authenticode 退化拦截**：应用补丁后、生成脚本前用 `WinVerifyTrust`
  比较原版本与产物的签名状态。**原版本可信而产物不可信**时返回
  `DeltaError::AuthenticodeUnsigned`（错误码 `authenticode_unsigned`，
  归类为安全阻断），避免在企业环境（AppLocker/WDAC）替换后应用直接无法启动。
  未签名的开发/便携构建不受影响；整包更新检查暂存树里的主程序。
- **诊断可见**：`Diagnosis::authenticode` 暴露原版本签名状态（非 Windows 为 `None`）。

### 平台覆盖

- **Linux AppImage 支持原地增量**：不再无条件回退全量。运行时用
  `$APPIMAGE` 定位 `.AppImage` 本体作为替换目标（`current_exe` 只是
  squashfs 挂载点），复用单文件流程：校验、备份、覆盖、`chmod +x`、重启。
  只有拿不到本体路径或所在目录不可写时才回退，并在 `fallback_reason` 里
  说明原因。发布端把 `.AppImage` 作为补丁基底即可。
- **AppImage 的进程等待不走 `/proc/<pid>/exe`**：该路径指向挂载点，
  与本体路径永远不相等，贸然沿用会立刻判定旧进程退出并提前替换。
  脚本新增 `CHECK_EXE_LINK` 开关，AppImage 下按 PID 等待。

### 修复与稳健性

- **整包崩溃恢复备份不再被清理**：`cleanup_previous_updates` 曾删除
  `tdx_bundle_backup`，让“已替换一半”的安装目录失去唯一可回滚的旧内容。
  现在只由替换脚本在还原后清理。
- **Windows 单文件脚本日志改用 `File.AppendAllText`**：与整包脚本一致，
  避免 `Add-Content` 在文件刚创建/被占用时静默丢行。
- **启动目录修正**：新版本进程的工作目录改为可执行文件所在目录，
  不再继承用户缓存目录。
- **macOS 提前回退**：运行在 App Translocation 或应用目录不可写时，
  在下载前返回 `EnvironmentUnsupported`，不会先提示“已更新”再失败。
- **整包磁盘预检**：下载前按补丁 + 旧归档 + 新归档 + 暂存目录的体积做
  保守检查，避免下完补丁才发现空间不足并累计进安全模式。
- **macOS 备份隐藏命名**：`.<App>.app.tdx-backup`，减少 Finder 干扰与撞名风险。
- **回滚原因可见**：脚本整体回滚时把 `update_failed` / `startup_timeout`
  写入 `tdx_rollback.txt`，`Diagnosis::last_rollback` 直接读取，
  不再出现“提示更新成功、重启后版本没变且没有线索”。新版本确认启动后自动清除。
- **中断后自动修复**：检测到上次整包替换未完成（备份或新增文件日志仍在）时，
  `update()` 不再发起网络请求，直接生成修复脚本并返回
  `UpdateOutcome::Repair` / `UpdateResult::RepairRequired`。脚本删除上次新增的文件、
  还原备份并重启旧版本；`Diagnosis::needs_repair` / `CheckOutcome::needs_repair`
  可让 UI 在启动时提示“需要先完成修复”。
- **跨进程续传**：半截补丁不再在下次更新时被清理，而是用清单里已签名的
  `patch_hash` 写入身份记录（`tdx_update.patch.id`）。身份一致才按 `Range`
  续传，换补丁/换版本或没有身份记录时丢弃重下，绝不把两份补丁拼在一起；
  记录超过 7 天（`RESUME_MAX_AGE_SECS`）后连同补丁一起清理。
- **清单与签名请求也会重试**：此前只有补丁下载有退避重试，清单拉取遇到一次
  502/超时就直接失败。现在小文件请求复用同一套 2s→16s 退避（含抖动），
  确定性错误（4xx、超限、非法内容）仍然立即返回。
- **传输中断不再丢进度**：响应体比声明的 `Content-Length` 短时保留半截文件，
  重试直接带 `Range` 续传；只有服务端多发字节（超出预期）才删除重下。
- **测试盲区修复**：本地 MockServer 按 `Range: bytes=` 精确大小写匹配，
  而 reqwest/hyper 发送的是小写头，导致 **206 续传分支实际上从未被覆盖**。
  改为大小写不敏感并新增"半途断流 → 续传完成"的端到端断言。
- **替换过程记录新增文件**：整包脚本把新增路径写入 `tdx_bundle_journal.txt`，
  崩溃回滚与修复都能删除旧版本本来没有的文件。

### Tauri 插件（`tauri-plugin-delta-updater`）

- **独立插件 crate**（`plugin/`）：只做注册命令、事件桥接与启动确认，不复制任何更新逻辑。
  主库保持框架无关，不启用插件的宿主不会引入 `tauri`。
- **命令**：`check` / `install` / `download_full` / `install_full` / `diagnose` / `cancel` / `reset_safety`，
  权限标识随包提供（`delta-updater:default`），前端用
  `invoke('plugin:delta-updater|install', { onEvent })` 即可。
- **零 Rust 配置**：`init_from_config()` 从 `tauri.conf.json` 的
  `plugins.delta-updater`（`endpoints` / `pubkey` / `bundle_endpoint`）读取；
  找不到时回退到 `plugins.updater`，从官方插件迁移不必改配置。
- **全量命令**：`download_full` 让回退路径也不再依赖官方插件下载；
  `install_full` 下载验签后直接启动平台安装器（Windows 走管理员权限的 NSIS/MSI），
  随后插件自动退出进程。
- **事件桥接**：`UpdateEvent` 通过 Tauri `Channel` 逐阶段推到前端，
  不需要在后端写 `emit`；产物就绪后插件自动退出进程（先返回结果，500ms 后退出）。
- **启动确认前移**：插件的 `setup` 在窗口创建前调用
  `confirm_startup_success_if_pending()`，并发出 `delta-updater://startup` 事件，
  宿主不会因为忘了调用而 60 秒后被回滚。
- **错误契约**：命令错误统一为 `{ code, message, retryable, security }`，
  前端按 `code` 分支，不必匹配中文文案。

### 更新会话（可观察、可取消）

- **`DeltaUpdater::session()`**：返回 `UpdateSession`，按阶段发出
  `Checking` / `Downloading` / `Applying` / `Ready` / `UpToDate` / `Fallback` /
  `SafeMode` / `Cancelled` 事件，事件可按 `stage` 序列化给前端。
- **`CancelToken`**：可从任意线程触发取消。下载阶段立即中止并删除半截补丁，
  合并阶段在下一个检查点生效；取消返回 `DeltaError::Cancelled`，
  **不计入安全模式**，也不留下暂存产物。
- **Future 现在满足 `Send`**：进度/事件回调加上 `Send` 约束，
  `install_with_progress` / `update_with_progress` 的返回值可直接放进
  Tauri 的 async command（此前内部 trait object 未带 `Send`，Future 不是 `Send`）。

### 开发者体验

- **等待时长配置真正生效**：`wait_timeout_secs` / `confirm_timeout_secs` 此前只是
  配置项，脚本里仍写死 60 秒。现在通过 `ScriptTimings` 传给所有脚本
  （单文件、整包、修复），0 值在构造阶段直接拒绝；新增 Windows 端到端断言。
- **稳定错误码**：`DeltaError::code()` 返回 `snake_case` 字符串（如
  `signature_invalid` / `update_in_progress`），前端与日志按码分支；
  文案改动不再破坏调用方的判断。新增单测保证码唯一且合法。
- **默认路径也支持进度**：`install_with_progress(|downloaded, total| ...)` 返回
  `UpdateResult`，不必为了进度条掉到高级 API。
- **启动确认可无条件调用**：`PlatformUpdater::confirm_startup_success_if_pending()`
  只在确实发生过替换时才写标志，普通启动是零写入空操作——避免"忘了确认导致 60 秒后回滚"。
- **`host_version!()` 宏**：非 Tauri 宿主直接拿到调用方 crate 的版本号，
  不会误用库自身的 `CARGO_PKG_VERSION`。
- **类型化全量回退**：`CheckOutcome::full_download()` / `full_download_for(platform)`
  返回 `FullDownload { version, url, signature }`，直接交给官方 updater
  或自定义安装流程，不必再从清单里手工挑字段。
- **`check()` 的原因不再靠猜**：没有匹配补丁时 `fallback_reason` 也会填
  `NoMatchingPatch{from,to}`，与 `update()` 的回退原因保持一致。
- **`install()` 语义收紧**：返回 `Updated` 时替换脚本一定已启动；
  即使配置了 `auto_launch(false)` 也会补上启动，避免"看起来更新了却没有替换"。
- **接入错误提前暴露**：`manifest_url` / `public_key` 留空、`current_version`
  非语义化版本时，构造阶段直接报出具体字段；版本号自动去掉 `v`/`V` 前缀与首尾空白，
  避免"清单里明明有补丁却永远匹配不到"。
- **诊断结论一步到位**：`Diagnosis::can_use_delta()` 直接回答"当前环境能不能走增量"，
  UI 诊断页不必自己拼 `fallback_reason` / `safe_mode` / 可写性三个条件。
- **前后端 JSON 契约统一**：`UpdateResult` 与 `FallbackReason` 都按 `snake_case`
  序列化，Tauri command 可直接返回给前端按 `kind` 分支
  （`{"kind":"requires_full_download","reason":{"no_matching_patch":{...}}}`）。
- README 补充首次接入检查清单、完整 Tauri `setup` + command 接入片段、发布端构建/密钥/旧版本归档步骤与 GitHub Actions 示例。

## [0.1.0] - 2026-11-06

首个公开版本。面向 Tauri v2 的增量更新引擎：只传输差异，失败路径优先。

### 新增

- **一行接入**：`DeltaUpdater::new(UpdateConfig::new(url, pubkey).current_version(...))`
  完成拉取清单 → 验签 → 版本检查 → 匹配补丁 → 下载 → 双端哈希校验 → 合并 → 启动替换。
- **分阶段接入**：`check()` 只检查、`prepare()` 只下载合并、`Staged::launch()`
  由宿主决定替换时机；`update_and_exit()` 一步到位。
- **零配置入口**：`DeltaUpdater::auto(url, pubkey, version)`，
  自动推导平台键、用户缓存工作目录与默认 HTTP 客户端。
- **本地验证闭环**：`simulate_update(old, new, work_dir)` 在无网络、无签名密钥的情况下
  跑完“生成补丁 → 应用 → 校验”，返回补丁体积、压缩比与哈希。
- **环境诊断**：`DeltaUpdater::diagnose()` 返回工作目录、可写性、平台、
  适用边界、安全模式状态与日志路径。
- **发布端 CLI**（`generator` feature）：`--old-dir` / `--app-binary` / `--installer`
  / `--version` / `--base-url` / `--private-key` / `--platform` / `--notes`
  / `--max-patch-ratio` / `--zstd-level` / `--existing` / `--output-dir` / `--dry-run`。
- **补丁覆盖范围标注**：清单中的 `scope: "main_executable_only"`，
  让 sidecar/resources 用户能在 UI 层提示，而不是靠回滚循环发现。

### 安全

- **降级/重放防护（三层，默认全部开启）**：
  1. 清单本身被 minisign 签名，攻击者无法把清单指向旧安装包而不破坏签名；
  2. 验签后按语义化版本强制比较 `manifest.version` 与当前版本，不更高即 `UpToDate`；
  3. trusted comment 的版本绑定用于发现“签名与清单版本不一致”的发布端错误。
  三层不依赖任何可选开关。
- **双端哈希校验**：补丁下载形态与合并产物分别校验 SHA-256；
  替换脚本在复制前/后再校验一次产物哈希，封住校验到执行的 TOCTOU 窗口。
- **`VerifiedArtifact` 类型级防护**：未经验证的产物在类型上无法进入替换流程。
- **提权后降权启动**：Windows 经 `explorer.exe` 以登录用户拉起真正的 PE
  （只传目标路径一个参数）；Linux 优先 `runuser`，缺失时退回 `su`，
  两者都缺时记录日志并以当前身份启动；替换后的文件所有权交还原用户。
- **清理只删本项目产物**：不再按 `.bak`/`.old` 扩展名批量删除用户文件；
  历史脚本 `sereno_update.*` 仍在显式清理范围内，旧日志刻意保留用于排查。
- **公钥必须内置**：`latest.json.pub` 仅给运维查看，第一行带 `DO NOT TRUST` 警告。

### 可靠性

- **默认工作目录改为用户缓存**（`%LOCALAPPDATA%` / `~/Library/Caches` / `$XDG_CACHE_HOME`）：
  `Program Files`、`/usr/bin` 等只读安装目录不再在下载阶段失败；
  构造阶段用探针文件验证真实可写性，失败返回 `WorkDirNotWritable`。
- **替换失败统一收口**：提权被拒、备份/复制/哈希失败、启动超时都会
  还原备份并重新拉起旧版本，应用不会“消失”。
- **崩溃恢复**：上一轮若在“exe → `.bak`”之后被打断，下一次脚本先还原备份；
  目标仍存在时绝不误还原。
- **进程等待/结束按 PID + 路径**：Windows 校验 PID 对应的可执行文件路径；
  Linux 额外用 `/proc/<pid>/exe` 校验，避免 PID 回收和同名误杀。
- **安全模式带 6 小时冷却**：达到阈值后暂停增量，冷却结束自动放行一次；
  旧单行计数文件按“冷却已过期”处理；`reset_safety()` 可手动退出。
- **断点续传严格依赖 206**：服务端忽略 `Range` 返回 200 时丢弃残留重下；
  长度校验防止截断文件被当成完整补丁。
- **资源边界**：清单/签名 10 MB 上限；补丁体积上限从签名的 `patch_size` 推导；
  解压改为流式落盘 + 映射，并设 `2 × 旧文件 + 128 MB` 上限。
- **重试退避带抖动**：避免 CDN 抖动时整批客户端同步重连。

### 开发者体验

- **默认路径面向 UI**：`install()` / `install_and_exit()` 返回
  `UpdateResult::{Updated, UpToDate, RequiresFullDownload, SafeMode}`；
  开发者不需要处理补丁 URL、哈希或暂存文件。需要完全控制时仍用 `update()`。
- **检查结果面向 UI**：`CheckOutcome` 增加 `is_delta` 与 `download_size`，
  并提供 `UpdateCheck` 别名。
- **错误可机读**：`UpdateInProgress`、`SafeMode`、`WorkDirNotWritable`、
  `ManifestInvalid`、`ResponseTooLarge` 等独立变体；`FallbackReason` 为枚举。
- **可选 `tracing`**：`tracing` feature 发出 `delta.check`、
  `delta.patch.matched`、`delta.patch.rejected`、`delta.fallback`、
  `delta.safety_mode.entered`、`delta.safety_mode.cooldown_active` 等结构化事件；
  回滚事件以 `event=delta.rollback.triggered` 写入脚本日志（进程已退出，库无法观测）。
- **CLI 易用性**：clap 具名参数、`--help`、`--dry-run`、环境变量读取私钥；
  跨平台生成补丁（Windows 安装包、Linux/macOS 裸二进制）。
- **文档**：README 提供 5 分钟本地验证与竞品接入对比；设计文档覆盖
  威胁模型、适用边界、CI 覆盖与环境限制。

### 工程

- 三平台 CI 矩阵、clippy `-D warnings`、`--no-default-features` 构建、
  MSRV 1.88 job；发布端校验 tag 与 Cargo 版本一致。
- `generator` feature 默认关闭：库使用者不编译 `clap` / `minisign` / `walkdir`。
- `fs2` → `fs4`；移除加载期 `#[ctor]`，TLS 后端在入口懒安装。
