# 版本与发布策略

> 本文件是 `tauri-delta-updater` 的**唯一版本与发布权威**。
> 任何与本文冲突的文档、CI 配置、口头约定，以本文为准。
>
> 生效日期：2026-11-06
> 当前锁定版本：**0.1.0**

---

## ⛔ 最高约束（先读这一节）

**没有项目所有者（丘屿知宁 / zuoqiuning）的明确书面允许，禁止以下任何操作：**

1. **禁止** 修改 `Cargo.toml` 中的 `version` 字段；
2. **禁止** 执行 `cargo publish`（包括 `--dry-run` 之外的任何发布行为）；
3. **禁止** 创建、推送、删除任何 git tag；
4. **禁止** 创建、修改、删除任何 GitHub Release；
5. **禁止** 在 CI 中配置自动发布（`cargo publish`、`softprops/action-gh-release`、`gh release create` 等）；
6. **禁止** 以“修复”“补丁”“临时”为理由绕过本条约束。

**当前版本 0.1.0 已锁死。** 任何版本号变更、任何 crates.io 发布、任何 tag 推送，都必须由项目所有者显式授权，并在授权中明确：目标版本号、发布内容摘要、发布时间。授权形式为项目所有者在 issue / commit / 对话中的明确文字确认，不接受“默认同意”或“惯例推断”。

违反本条约束的提交与发布，视为无效，须回滚。

---

## 1. 版本语义定义

本项目遵循 [语义化版本 2.0.0](https://semver.org/lang/zh-CN/)，版本号格式为 `MAJOR.MINOR.PATCH`。

### 1.1 0.x 阶段的特殊约定

`0.x.y` 阶段的语义化版本**不承诺 API 稳定**，但本项目**主动收窄** 0.x 的自由度，采用以下约定，使下游可以安全依赖：

| 版本位 | 本项目含义 | 典型变更 |
|---|---|---|
| `MAJOR`（当前恒为 0） | 不适用 | — |
| `MINOR` | **破坏性变更** | 公开 API 签名变更、`UpdateOutcome` 变体增删、`generate` CLI 参数不兼容、默认行为变更（如工作目录、退出语义）、MSRV 提升 |
| `PATCH` | **非破坏性变更** | 新增可选 API、新增可选配置项、内部实现优化、文档修正、测试补充、错误信息改进、默认值不变的行为修复 |

**关键承诺**：
- `0.1.x` 系列内，**任何不破坏现有调用方的变更**都走 `PATCH`；
- **只有破坏性变更**才升 `MINOR`（即 `0.2.0`）；
- 任何 `PATCH` 发布都必须能通过“**现有接入代码零改动即可升级**”的验证。

### 1.2 破坏性变更的判定清单

以下任一情况属于破坏性变更，**必须升 MINOR**：

- 删除或重命名任何 `pub` 项（函数、结构体、枚举、变体、字段、trait）；
- 修改任何 `pub` 函数的签名（参数类型、返回类型、泛型约束）；
- 给 `#[non_exhaustive]` 枚举新增变体（调用方的 `match` 可能不再穷尽）；
- 修改任何公开结构体的字段可见性或类型；
- 修改 `generate` CLI 的现有参数名、参数顺序、默认值语义；
- 修改默认工作目录、默认退出行为、默认回退语义；
- 提升 `rust-version`（MSRV）；
- 修改清单格式的**必需字段**或语义（新增可选字段不算）；
- 修改签名校验、哈希校验的**失败行为**（从放行改为拒绝，或反之）；
- 修改 `Cargo.toml` 中的 `features` 默认集合（例如把 `generator` 从默认开启改为默认关闭，或反之）。

以下情况**不**属于破坏性变更，走 PATCH：

- 新增 `pub` 函数、结构体、枚举变体（前提是枚举标记了 `#[non_exhaustive]`）；
- 新增可选配置项（`UpdateConfig` 的 builder 方法）；
- 新增 feature（默认关闭）；
- 内部实现优化、性能提升、日志改进；
- 修复错误行为，且修复后与文档描述一致；
- 文档、示例、测试的增删改。

### 1.3 预发布与构建元数据

- 预发布版本使用 `0.2.0-alpha.1`、`0.2.0-beta.1` 形式，**不得**发布到 crates.io 作为正式版本（如需分发，使用 git tag 或本地构建）。
- 构建元数据（`+build.1`）不用于本项目，避免与 crates.io 版本解析冲突。

---

## 2. 当前版本与路线图

### 2.1 当前锁定版本：0.1.0

`0.1.0` 是**首个公开版本**，包含：

- 零配置入口 `DeltaUpdater::auto(url, pubkey, version)`；
- 环境诊断 `DeltaUpdater::diagnose()`；
- 本地验证闭环 `simulate_update(old, new, work_dir)`；
- 可选结构化事件（`tracing` feature）；
- 发布端补丁自验证（默认行为）；
- 发布端 `--zstd-level` 与 `create_patch_with_level`；
- 端到端路径断言（增量命中时全量 URL 未被请求）；
- 与官方 updater / `tauri-plugin-updater-delta` 的接入对比文档。

### 2.2 已规划但未发布的变更

以下变更**不属于 0.1.0**，须在获得授权后按对应版本发布：

| 变更 | 目标版本 | 类型 | 前置条件 |
|---|---|---|---|
| Tauri 插件封装（`app.delta_updater()`） | `0.2.0` | 破坏性（新增 `tauri` 依赖、新增公开 API 形态） | 独立 crate 设计完成、权限/命令脚手架就绪 |
| 独立 `tauri-delta-core` crate 拆分 | `0.2.0` | 破坏性（公开 API 路径迁移） | workspace 拆分方案通过评审 |
| PE 感知差分实验（`--pe-normalize`） | 待定 | 非破坏性（新增可选 flag） | 真实产物基准测试证明收益 |
| 并行目录 / 原子切换 | 待定 | 可能破坏性（`Staged` 生命周期重设计） | Windows 边界验证（独占锁、跨卷、杀软） |
| macOS tar-level 增量路径 | 待定 | 非破坏性（新增可选后端） | 真实签名/公证应用验证 |

**触发条件**：上表中“待定”的项，只有在项目所有者明确启动时才进入排期。不得以“路线图已列出”为由自行实施。

---

## 3. 发布到 crates.io 的前置条件

### 3.1 crate 名称与版本可用性

- crate 名称 `tauri-delta-updater` 已在 crates.io 确认**未被占用**（2026-11-06 核实）。
- 版本 `0.1.0` **从未发布过**，可作为首次发布版本。
- **发布前必须再次核实**：访问 `https://crates.io/crates/tauri-delta-updater`，若返回 404，说明名称仍可用；若返回页面，说明已被发布，**立即停止发布**并报告项目所有者。

### 3.2 `Cargo.toml` 元数据清单

`cargo publish` 会校验以下字段。缺失或格式错误会导致发布失败或 crate 页面显示不完整：

| 字段 | 要求 | 本项目现状 | 检查命令 |
|---|---|---|---|
| `name` | 与 crates.io 名称一致 | `tauri-delta-updater` | `cargo metadata --no-deps` |
| `version` | 符合 SemVer，且未被发布过 | `0.1.0` | 同上 |
| `description` | 非空，≤ 200 字符 | 已有 | `cargo publish --dry-run` |
| `license` | SPDX 标识符 | `MIT` | 同上 |
| `license-file` | 与 `license` 二选一 | 使用 `license`，无需此项 | — |
| `repository` | 建议填写 | 已有 | — |
| `readme` | 建议显式指向 | 建议加 `readme = "README.md"` | — |
| `keywords` | ≤ 5 个，每个 ≤ 20 字符 | 已有，确认数量 | `cargo publish --dry-run` |
| `categories` | 可选，建议填写 | 建议加 `development-tools`、`os::windows` | — |
| `rust-version` | MSRV，须准确 | `1.88`（已核实为 `reqwest 0.13 → url → icu_*` 的真实下限） | `cargo msrv verify`（如安装） |
| `edition` | 当前 `2021` | 已有 | — |

**强制要求**：`cargo publish --dry-run` 必须零错误、零警告（crates.io 的元数据警告也要处理），才能进入正式发布。

### 3.3 打包内容检查

`cargo publish` 会打包 `Cargo.toml` 声明的文件集合。以下内容**不得**进入发布包：

- `target/` 目录；
- 本地测试夹具（`tests/fixtures/` 中的大文件、临时 exe）；
- `.env`、密钥文件、`*.msk`、`*.pub`（除非明确作为示例公钥）；
- `real_test/`、`manual_test/`、`mock_release/` 等本地目录；
- `.github/` 中的 CI 配置（crates.io 不需要，但可保留，不影响）；
- 任何 `*.patch`、`latest.json` 等生成产物。

**检查命令**：

```bash
cargo package --list --allow-dirty
```

逐行审查输出，确认无上述内容。如有，在 `Cargo.toml` 中添加 `exclude = [...]`。

### 3.4 构建与测试门禁

正式发布前，以下命令必须全部通过（与 CI 一致）：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc --all-features
cargo check --no-default-features
cargo check --features tracing
cargo package --allow-dirty
```

**任何一项失败，禁止发布。**

---

## 4. 发布流程（分步，不可跳步）

> 以下流程仅在获得项目所有者**明确授权**后执行。授权中必须包含目标版本号与发布内容摘要。

### 4.1 发布前准备

```bash
# 1. 确认工作区干净（无未提交改动）
git status

# 2. 确认版本号与 CHANGELOG 一致
grep '^version' Cargo.toml
head -50 CHANGELOG.md

# 3. 确认 crate 名称在 crates.io 仍可用
#    浏览器访问 https://crates.io/crates/tauri-delta-updater，应为 404

# 4. 运行完整门禁
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc --all-features
cargo check --no-default-features
```

### 4.2 提交与打 tag

```bash
# 1. 提交所有改动（commit message 使用 Conventional Commits）
git add -A
git commit -m "chore: release 0.1.0"

# 2. 创建 annotated tag（不是 lightweight tag）
git tag -a v0.1.0 -m "tauri-delta-updater 0.1.0"

# 3. 推送提交与 tag
git push origin main
git push origin v0.1.0
```

**注意**：tag 推送后，CI 中的 `release.yml` **不得**自动触发 `cargo publish`。当前 `release.yml` 若包含自动发布逻辑，必须在发布前改为手动触发（`workflow_dispatch`），并加审批门禁。

### 4.3 发布到 crates.io

```bash
# 1. dry-run：验证元数据、打包内容、构建
cargo publish --dry-run

# 2. 逐行审查 dry-run 输出：
#    - 打包文件列表是否只包含预期文件
#    - 是否有元数据警告
#    - 是否有编译警告

# 3. 正式发布（不可撤销）
cargo publish
```

**`cargo publish` 成功后，0.1.0 永久存在于 crates.io，不可删除。** 如有问题，只能 `cargo yank --version 0.1.0`（标记为不推荐，但依赖仍可解析），并发布修复版本。

### 4.4 发布后验证

```bash
# 1. 确认 crate 页面可访问
#    https://crates.io/crates/tauri-delta-updater

# 2. 确认 docs.rs 构建成功（通常几分钟内）
#    https://docs.rs/tauri-delta-updater

# 3. 在一个临时项目中验证可安装
mkdir /tmp/tdx-verify && cd /tmp/tdx-verify
cargo init --bin
cargo add tauri-delta-updater@0.1.0
cargo build
```

### 4.5 创建 GitHub Release

```bash
gh release create v0.1.0 \
  --title "v0.1.0 — 首个公开版本" \
  --notes-file CHANGELOG.md \
  --latest
```

Release 内容必须包含：
- 版本号与发布日期；
- CHANGELOG 摘要（Added / Security / Reliability / DX）；
- crates.io 链接与 docs.rs 链接；
- 明确的“本版本锁死，后续版本须经授权”声明。

---

## 5. 发布失败与补救

### 5.1 `cargo publish` 失败的常见原因

| 错误                           | 原因                    | 处理                                       |
| ------------------------------ | ----------------------- | ------------------------------------------ |
| `crate version already exists` | 版本已发布              | 停止，报告项目所有者，不得自行改版本号重试 |
| `missing or empty metadata`    | `Cargo.toml` 元数据缺失 | 补齐元数据后重新 `--dry-run`               |
| `failed to verify package`     | 打包内容构建失败        | 检查 `Cargo.toml` 的 `exclude`、依赖路径   |
| `network error`                | 网络问题                | 重试；若持续失败，报告项目所有者           |

### 5.2 发布后的补救

- **不得** 删除 crates.io 版本（crates.io 不支持删除）。
- **不得** 复用同一版本号重新发布。
- 如需撤回，使用 `cargo yank --version <ver>`，并在 CHANGELOG 中说明撤回原因。
- 修复后发布新的 PATCH 版本（例如 `0.1.1`），并在 CHANGELOG 中引用被撤回的版本。

### 5.3 tag 误推送的补救

```bash
# 删除本地 tag
git tag -d v0.1.0

# 删除远端 tag（须获得项目所有者授权）
git push origin :refs/tags/v0.1.0
```

**注意**：若 crates.io 已发布对应版本，删除 tag 不会撤回 crates.io 版本。两者独立。

---

## 6. 权限与审批

### 6.1 谁可以发布

- **仅项目所有者**（丘屿知宁 / zuoqiuning）或其**明确书面授权**的协作者可以执行 `cargo publish`、推送 tag、创建 Release。
- 授权必须包含：目标版本号、发布内容摘要、授权有效期。
- 授权形式：issue 评论、commit message、对话中的明确文字确认。**口头授权、默认同意、惯例推断均无效。**

### 6.2 CI 的权限边界

- CI **可以** 运行 `cargo test`、`cargo clippy`、`cargo fmt --check`、`cargo check`、`cargo package --list`。
- CI **不得** 自动执行 `cargo publish`。
- CI **不得** 自动创建或推送 tag。
- CI **不得** 自动创建 GitHub Release。
- 若 CI 配置中包含上述自动行为，**必须** 改为 `workflow_dispatch` 手动触发，并加 environment protection rule（需要人工审批）。

### 6.3 违反约束的处理

任何违反第 0 节约束的发布行为，按以下顺序处理：

1. 立即停止后续发布；
2. 回滚 tag 与 Release（若 crates.io 已发布，只能 yank）；
3. 在 CHANGELOG 中记录违规发布与撤回原因；
4. 由项目所有者决定是否重新发布。

---

## 7. 版本号变更的操作清单

当项目所有者授权发布新版本时，按以下清单执行：

- [ ] 确认授权包含目标版本号与内容摘要；
- [ ] 确认 crates.io 上目标版本号未被占用；
- [ ] 更新 `Cargo.toml` 的 `version`；
- [ ] 运行 `cargo update -p tauri-delta-updater` 同步 `Cargo.lock`；
- [ ] 在 `CHANGELOG.md` 顶部新增该版本条目（按 Added / Changed / Fixed / Security / Reliability / DX 分类）；
- [ ] 更新 `README.md` 中的版本引用（依赖示例、兼容性表）；
- [ ] 更新 `VERSIONING.md` 的“当前锁定版本”；
- [ ] 运行第 3.4 节的全部门禁命令；
- [ ] 运行 `cargo publish --dry-run` 并逐行审查；
- [ ] 执行第 4.2–4.5 节的发布流程；
- [ ] 发布后运行第 4.4 节的验证。

---

## 8. 当前状态快照（2026-11-06）

| 项                 | 状态                                   |
| ------------------ | -------------------------------------- |
| `Cargo.toml` 版本  | `0.1.0`                                |
| crates.io 状态     | 未发布                                 |
| git tag            | `v0.1.0`                               |
| GitHub Release     | `v0.1.0`                               |
| 测试               | 71 个集成/单元测试 + 11 个文档测试全绿 |
| clippy             | 零告警                                 |
| MSRV               | 1.88                                   |
| 下次允许发布的前提 | 项目所有者明确书面授权                 |

---

**本文件自 2026-11-06 起生效。任何修改须经项目所有者批准。**

**再次强调：没有项目所有者的允许，不能推送新版本、不能发布、不能打 tag。0.1.0 已锁死。**
