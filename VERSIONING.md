# 版本语义与发布约束

> 本文件是 `tauri-delta-updater` **版本语义的唯一权威**；发布门禁、操作步骤与
> 补救流程见 [`docs/RELEASING.md`](docs/RELEASING.md)。两者若有冲突：版本判定以本文件为准，
> 操作步骤以 `docs/RELEASING.md` 为准。
>
> 生效日期：2026-11-06；最后更新：2026-11-09

---

## ⛔ 最高约束

**没有项目所有者的明确书面授权，禁止以下任何操作：**

1. 修改 `Cargo.toml` 中的 `version` 字段；
2. 执行 `cargo publish`（`--dry-run` 之外任何发布行为）；
3. 创建、推送、删除任何 git tag；
4. 创建、修改、删除任何 GitHub Release；
5. 在 CI 中配置自动发布（任何由 tag push、merge 或定时任务触发的发布）；
   只允许 `workflow_dispatch` 手动触发，且 `publish` 默认 `false`、`publish` job
   绑定 `environment: release` 审批；
6. 以“修复”“补丁”“临时”为理由绕过本条约束。

授权必须明确三要素：**目标版本号、发布内容摘要、发布时间**。授权形式为项目
所有者在 issue / commit / 对话中的明确文字确认；默认同意、惯例推断均无效。
授权记录由项目所有者私下维护，不入库。

`0.3.0` 已于 2026-11-08 发布，`0.4.0` 已于 2026-11-09 发布，均不可覆盖；
`0.4.0` 经所有者授权并按发布门禁走完 verify → core → plugin → npm。

---

## 1. 版本语义

本项目遵循 [语义化版本 2.0.0](https://semver.org/lang/zh-CN/)，格式为 `MAJOR.MINOR.PATCH`。

### 1.1 0.x 阶段的特殊约定

`0.x.y` **不承诺 API 稳定**，但本项目主动收窄 0.x 的自由度：

| 版本位 | 本项目含义 | 典型变更 |
|---|---|---|
| `MAJOR`（当前恒为 0） | 不适用 | — |
| `MINOR` | **破坏性变更** | 公开 API 签名变更、未标记 `#[non_exhaustive]` 的枚举增删变体、`generate` CLI 参数不兼容、默认行为变更、MSRV 提升 |
| `PATCH` | **非破坏性变更** | 新增可选 API、新增可选配置项、内部优化、文档修正、错误信息改进、默认值不变的行为修复 |

**关键承诺**：

- `0.2.x` 系列内，任何不破坏现有调用方的变更都走 `PATCH`；
- 只有破坏性变更才升 `MINOR`（例如 `0.2.0` → `0.3.0`）；
- 任何 `PATCH` 发布都必须通过“现有接入代码零改动即可升级”的验证。

### 1.2 破坏性判定清单

以下任一情况属于破坏性变更，**必须升 MINOR**：

- 删除或重命名任何 `pub` 项（函数、结构体、枚举、变体、字段、trait）；
- 修改任何 `pub` 函数的签名（参数类型、返回类型、泛型约束）；
- 给**未标记 `#[non_exhaustive]`** 的枚举新增变体（下游 `match` 会不再穷尽）；
- 修改任何公开结构体的字段可见性或类型；
- 修改 `generate` CLI 的现有参数名、参数顺序、默认值语义；
- 修改默认工作目录、默认退出行为、默认回退语义；
- 提升 `rust-version`（MSRV）；
- 修改清单格式的**必需字段**或语义（新增可选字段不算）；
- 修改签名校验、哈希校验的**失败行为**（从放行改为拒绝，或反之）；
- 修改 `Cargo.toml` 中的 `features` 默认集合。

以下**不**属于破坏性变更，走 PATCH：

- 新增 `pub` 函数、结构体、枚举变体（前提是枚举已标记 `#[non_exhaustive]`）；
- 新增可选配置项（`UpdateConfig` 的 builder 方法）；
- 新增 feature（默认关闭）；
- 内部实现优化、性能提升、日志改进；
- 修复错误行为，且修复后与文档描述一致；
- 文档、示例、测试的增删改。

**当前基线**：`DeltaError` / `UpdateOutcome` / `UpdateResult` 在 `0.2.0` 已发布版本
中均已标记 `#[non_exhaustive]`（`git show v0.2.0:src/error.rs`、`...:outcome.rs`
可复核）。因此 `0.2.1` 新增的 `ChainHopFailed` / `UnsupportedOs` 属于 PATCH。

### 1.3 预发布

- 预发布使用 `0.2.0-alpha.1` / `0.2.0-beta.1` 形式，**不得**作为正式版本发布到
  crates.io；需要分发时用 git tag 或本地构建。
- 构建元数据（`+build.1`）不使用，避免与 crates.io 版本解析冲突。

### 1.4 历史偏离（0.2.1）与 0.3.0 修正

`0.2.1` 给 `UpdateConfig` / `UpdateManifest` / `PlatformInfo` 新增了可选字段。
当时这三个结构体未标记 `#[non_exhaustive]`，严格按 §1.2 属于破坏性变更；
因新增字段都带 serde 默认值、不影响 builder 路径，按 PATCH 发布。

`0.3.0` 已给这三个结构体补上 `#[non_exhaustive]`，并把
`allow_legacy_signatures` / `allow_insecure_transport` 默认值收紧为 `false`。
此后新增公开字段不再破坏下游；代价是外部字面量构造需要改为 serde 反序列化
或 builder。新增公开结构体字段不再按 PATCH 处理。

### 1.5 0.4.0 的破坏性判定

`0.4.0` 按 MINOR 发布，破坏性变更集中在已有变体的形状与默认权限：

- `UpdateOutcome::SafeMode` / `UpdateResult::SafeMode` 从单元变体改为结构体变体，
  新增 `reason: SafeModeReason`；
- `FallbackReason::EnvironmentUnsupported(&'static str)` 改为携带
  `EnvironmentReason`；
- `DeltaError::FullInstallFailed(String)` 改为
  `{ kind: InstallFailureKind, message }`；
- `DeltaError` / `UpdateOutcome` / `UpdateResult` / `FallbackReason` 均已标记
  `#[non_exhaustive]`，新增变体本身不构成破坏；
- `delta-updater:default` 收窄为 read + update，全量安装命令需要显式
  `delta-updater:full`（默认行为变化）；
- `ReleaseOptions` 新增公开字段 `allow_http`（字面量构造需改用
  `ReleaseOptions::new` 后逐字段覆盖）。

新增 `authenticode_policy` / `lock_scope` 等可选配置不单独构成破坏性变更。

---

## 2. 当前版本状态

### 2.1 已发布与待发布

| 制品 | 状态 |
|---|---|
| 核心 crate | `0.1.0`（2026-11-06）、`0.2.0`、`0.2.1`（2026-11-07）、`0.3.0`（2026-11-08）、`0.4.0`（2026-11-09）已发布 |
| 插件 crate | `0.2.1`（2026-11-07）、`0.3.0`（2026-11-08）、`0.4.0`（2026-11-09）已发布 |
| npm 包 | `0.2.1`（2026-11-07）、`0.3.0`（2026-11-08）、`0.4.0`（2026-11-09）已发布 |
| git tag / Release | `v0.1.0`、`v0.2.0`、`v0.2.1`、`v0.3.0`、`v0.4.0`（annotated） |

`0.2.1` 相对 `0.2.0` 的新增面（全部为可选 API，见 `CHANGELOG.md`）：

- 请求头 `headers`、端点故障转移 `fallback_endpoints` / `fallback_policy`；
- Windows 安装模式 `install_mode`、明文 HTTP 边界 `allow_insecure_transport`；
- 跨重启待安装（`pending` / `discard_pending` / `install_pending`、
  `install_on_next_launch`）；
- 多跳补丁链（`MAX_DELTA_CHAIN_HOPS`、成本决策、`DeltaError::ChainHopFailed`）；
- 多密钥信任（`additional_public_keys` / `TrustPolicy` / 双签工具）；
- 最低系统版本 `min_os`、渠道占位符 `channel`、多语言说明 `notes_i18n`、
  自定义字段透传 `extra`。

### 2.2 未排期候选

| 变更 | 类型 | 前置条件 |
|---|---|---|
| 独立 `tauri-delta-core` crate 拆分 | 破坏性（公开 API 路径迁移） | workspace 拆分方案通过评审 |
| PE 感知差分（Zucchini / `--pe-normalize`） | 非破坏性（新增可选后端） | 真实产物基准测试证明收益 |
| 并行目录 / 原子切换 | 可能破坏性（`Staged` 生命周期重设计） | Windows 独占锁、跨卷、杀软边界验证 |
| macOS tar-level 增量路径 | 非破坏性（新增可选后端） | 真实签名/公证应用验证 |
| 收紧安全默认值（`allow_legacy_signatures` / `allow_insecure_transport` 默认 `false`） | 破坏性（签名与传输行为变更） | 已在 0.3.0 完成，附迁移说明 |
| 给 `UpdateConfig` / `UpdateManifest` / `PlatformInfo` 补 `#[non_exhaustive]` | 破坏性（字面量构造需改 serde / builder） | 已在 0.3.0 完成；见 §1.4 |

上表只有项目所有者明确启动时才进入排期；列出不等于默许实施。
