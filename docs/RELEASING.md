# 发布流程与门禁

> 版本语义与破坏性判定见 [`VERSIONING.md`](../VERSIONING.md)；本文件只描述
> **操作步骤、门禁与补救**。
>
> 任何 `cargo publish`、tag 推送、GitHub Release 都必须先获得项目所有者的
> 明确书面授权，且授权包含：目标版本号、发布内容摘要、发布时间。
> 授权记录由项目所有者私下维护，不入库。

---

> **操作步骤、门禁与补救**。三平台命令、命令规范与发布说明写作规范都在
> 本文件内；不再单独维护第二份发布文档。

---

## 1. 发布前置条件

### 1.1 版本号可用

1. 访问 `https://crates.io/api/v1/crates/tauri-delta-updater`，确认目标版本不存在；
2. `0.4.0` 已发布且不可覆盖；目标版本 `0.5.0` 必须尚未发布；
3. 插件、测试夹具（`tdx-testkit`）与 npm 包随核心同步发布。

### 1.2 元数据

`cargo publish` 会校验 `name` / `version` / `description` / `license` / `keywords`
等字段；`cargo publish --dry-run` 必须零错误、零警告。

### 1.3 打包内容

以下内容不得进入发布包：`target/`、本地夹具（大文件 / 临时 exe）、`.env`、
`*.msk` / `*.pub`（示例公钥除外）、`real_test/` / `manual_test/` / `mock_release/`、
`*.patch`、`latest.json`。

```bash
cargo package --list --allow-dirty
cargo package --list --manifest-path plugin/Cargo.toml --allow-dirty
cargo package --list --manifest-path testkit/Cargo.toml --allow-dirty
```

`plugin/dist-js` / `plugin/guest-js` 由 npm 包发布，**有意排除**在插件 crate
tarball 之外，不是遗漏。CI 会断言它们不出现。

### 1.4 门禁

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked --no-fail-fast
CARGO_TARGET_DIR=target-doctest cargo test --doc --all-features --locked
cargo check --no-default-features --locked
cargo check --features tracing --locked
# 本机 registry 可能被镜像替换；显式 --registry crates-io 才校验正式源
cargo publish --dry-run --locked --registry crates-io

cargo package --list --allow-dirty
cargo package --list --manifest-path plugin/Cargo.toml --allow-dirty

cargo clippy --manifest-path plugin/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path plugin/Cargo.toml
cd plugin && npm ci && npm run build && npm run check
npm pack --dry-run --json   # 必须包含 dist-js/index.js、index.d.ts、package.json、README.md
```

`npm pack --dry-run` 与 `cargo package --list` 是同一类检查：typecheck 通过
不代表 tarball 包含正确的文件。`files` 漏掉 `dist-js` 时 `npm publish` 仍会
成功，但安装者拿不到绑定；CI 已按发布产物断言。

**任何一项失败，禁止发布。**

版本一致性由 CI 的 `versions` job 检查五处：核心 crate、插件 crate、
测试夹具 crate（`tdx-testkit`）、`plugin/package.json`、`CHANGELOG.md` 第一个
数字版本条目（`## [Unreleased]` 不参与比较）。**发布前必须把 `Unreleased`
下的条目移到新版本小节**，否则发布说明会缺失这批变更。

> 插件的 `cargo publish --dry-run` 校验时会从注册表解析核心 crate（不是本地
> path 依赖）。核心未发布前它**必然失败**；预发布阶段只用 `cargo package --list`
> 校验内容，真正的插件 dry-run 放在核心索引可见之后（见 §2 第 3 步）。

### 1.5 PATCH 基线复核

PATCH 判定不能只看当前工作区。任何 PATCH 发布前，回到上一个已发布 tag 复核
判定依据：

```bash
# 取最近可达的已发布 tag，不要手写版本号
LAST_TAG=$(git describe --tags --abbrev=0)

# 新增变体所属枚举在“已发布版本”中是否已标记 #[non_exhaustive]
git show "$LAST_TAG:src/error.rs" | grep -n non_exhaustive
git show "$LAST_TAG:src/updater/outcome.rs" | grep -n non_exhaustive

# 删除或改名的 pub 项：这条的输出必须为空
git diff "$LAST_TAG"..HEAD -- 'src' | grep -E '^-pub (fn|struct|enum|trait|const|type)'
```

若上一个 tag 中枚举**没有** `#[non_exhaustive]`，本次新增变体就是破坏性变更，
必须升 MINOR——即使当前工作区看起来“已经标记了”。

---

## 2. 发布顺序（不可跳步）

发布前先用 `publish: false` 跑一次 `verify` job，确认门禁与版本检查在真实
Actions 环境里通过（需要仓库 Actions 权限；`publish` job 应被跳过，不触碰
crates.io）：

> **前置条件**：`gh workflow run` 读取的是**远程默认分支**上的 `release.yml`，
> 不是本地未推送的版本。本地改动必须先推送（需要所有者授权一次仅包含
> `.github/workflows/` 的推送），否则演练的是旧工作流，结论不能用于本次发布。
> `act` 只能覆盖 Linux 部分，不能替代。

```bash
gh workflow run release.yml -f version=0.5.0 -f publish=false
gh run watch
```

确认 `verify` 全绿、`publish` 显示 skipped 后，再执行正式发布。

**路径 A（本机执行）**：按下面的命令逐条执行。**路径 B（CI 执行）**：推送后
在仓库配置 `CARGO_REGISTRY_TOKEN` / `NPM_TOKEN` 与 `release` environment
审批人，然后 `gh workflow run release.yml -f version=0.5.0 -f publish=true`，
不要再执行下面的 cargo/npm 命令。两条路径都使用 annotated tag。

```bash
# 1. annotated tag（不要 lightweight tag）；release.yml 是 workflow_dispatch，
#    推 tag 不会触发发布
git tag -a v0.5.0 -m "tauri-delta-updater 0.5.0"
git push origin v0.5.0

# 2. 核心 0.5.0（不可撤销）
#    本机 registry 常被镜像替换，必须显式 --registry crates-io
cargo publish --registry crates-io --locked

# 3. 轮询索引：固定 sleep 在索引慢时不可靠
for i in $(seq 1 15); do
  if curl -sf -H "User-Agent: tauri-delta-updater-release-ci" \
    "https://crates.io/api/v1/crates/tauri-delta-updater/0.5.0" > /dev/null; then
    break
  fi
  sleep 20
done

# 4. 插件：先 dry-run 确认能按注册表版本构建，再发布
cargo publish --manifest-path plugin/Cargo.toml --registry crates-io --dry-run --locked
cargo publish --manifest-path plugin/Cargo.toml --registry crates-io --locked

# 4b. 测试夹具（不依赖核心，可与核心并行发布）
cargo publish --manifest-path testkit/Cargo.toml --registry crates-io --locked

# 5. npm 包（前端类型绑定；显式官方 registry，避免被本机镜像接管）
cd plugin && npm publish --access public --registry https://registry.npmjs.org

# 6. GitHub Release：发布说明必须含 §2.1 的验证边界段
#    直接用 CHANGELOG.md 会漏掉它；发布说明来自 docs/releases/v0.5.0.md
gh release create v0.5.0 \
  --title "v0.5.0 — 大文件增量、并行发布与可解释的诊断" \
  --notes-file docs/releases/v0.5.0.md \
  --latest
```

第 3 步轮询失败时**停止**，不要继续发插件：插件会因核心不可见而校验失败。
此时核心已发布、插件未发布，按 §3.2 处理。

### 2.1 Release 内容必须包含

- 版本号与发布日期；
- CHANGELOG 摘要；
- crates.io / docs.rs 链接；
- **验证边界声明**：macOS `.app` 的 Gatekeeper / 公证、Windows UAC 提权与
  single-instance 退出、Linux 提权路径均未真机验证；CI 只做静态与合成脚本断言；
- **真机验证记录**：按 [`docs/DEVICE_TESTS.md`](DEVICE_TESTS.md) 执行，结果写入发布说明；
  未执行的项必须标注，不能用 CI 绿替代；
- “本版本不可覆盖，后续版本须经授权”的声明。

### 2.2 发布说明

写作规范见本文 §6。每次发布必须从
[`releases/TEMPLATE.md`](releases/TEMPLATE.md) 复制出 `releases/vX.Y.Z.md`，
正文面向使用者；GitHub Release 与该文件保持一致。

---

## 3. 发布后验证与补救

### 3.1 发布后验证

```bash
# 核心可安装
mkdir /tmp/tdx-verify && cd /tmp/tdx-verify && cargo init --bin
cargo add tauri-delta-updater@0.5.0 && cargo build

# 插件可解析（需 Tauri 项目上下文）
cd examples/tauri-app/src-tauri && cargo check

# 线上桶确实是你刚发的版本（发布后验证的核心断言）
cargo delta doctor <manifest-url> --pubkey <public-key> --expect-version 0.5.0

# npm 可安装
cd /tmp && npm init -y && npm install tauri-plugin-delta-updater@0.5.0
```

另需确认 `https://docs.rs/tauri-delta-updater/0.5.0` 与
`https://docs.rs/tauri-plugin-delta-updater/0.5.0` 构建成功。

**发布状态快照**：归档本次的 `latest.json`、`latest.json.sig` 与
`release-report.json`，记录 `platforms[].rollout` / `platforms[].min_os` 的生效值。
只保留 CDN 上的当前清单会丢失历史状态，灰度和回滚排查都依赖这份快照。

### 3.2 部分失败

| 情形 | 处理 |
|---|---|
| 核心发布失败 | 什么都没发；修复后可用同一版本重试（版本未被占用） |
| 核心成功、插件失败 | 插件 job 可稍后单独重跑；**核心不因此 yank** |
| 插件成功、npm 失败 | npm 单独重跑，不影响 crate |
| 核心版本本身有严重缺陷 | `cargo yank --version <ver>` 并在 CHANGELOG 说明；随后发 PATCH 修复版 |

- **不得**删除 crates.io 版本（不支持删除），**不得**复用版本号重新发布。
- `yank` 只是标记为不推荐，已有 `Cargo.lock` 仍可解析；因此 yank 只用于确认有
  严重问题的情况，且必须在 CHANGELOG 记录原因。
- tag 误推送：本地 `git tag -d <tag>`，远端删除需所有者授权；删除 tag 不会撤回
  crates.io 版本，两者独立。

---

## 4. 权限与 CI 边界

- **仅项目所有者**或其明确书面授权的协作者可以 `cargo publish`、推送 tag、
  创建 Release。授权必须包含目标版本号、内容摘要、发布时间。
- 授权记录由项目所有者私下维护，**不入库**；其存在性可由所有者确认。
- **不以 CI job 假装覆盖前提不存在的场景**：如果真实安装包、NOPASSWD 配置
  或签名环境不在仓库里，该 job 只会长期不运行或长期红叉，两者都比“在发布
  说明中明确标注未验证”更糟。
- CI **可以**：`cargo test` / `clippy` / `fmt --check` / `check` / `package --list` /
  `publish --dry-run`。
- CI **不得**：自动 `cargo publish`、自动创建或推送 tag、自动创建 Release。
  发布工作流只允许 `workflow_dispatch`，`publish` 默认 `false`，`publish` job
  绑定 `environment: release` 审批。
- 违反约束的发布视为无效：立即停止后续步骤、回滚 tag/Release（crates.io 只能
  yank）、在 CHANGELOG 记录、由所有者决定是否重发。

---

## 5. 当前状态快照（2026-11-09）

| 项 | 状态 |
|---|---|
| `Cargo.toml` 版本 | `0.5.0`（2026-11-09 已发布） |
| crates.io | 核心 `0.1.0` / `0.2.0` / `0.2.1` / `0.3.0` / `0.4.0` / `0.5.0`；插件 `0.2.1` / `0.3.0` / `0.4.0` / `0.5.0`；测试夹具 `tdx-testkit` 自 0.6.0 起随核心发布 |
| npm | `tauri-plugin-delta-updater@0.2.1` / `0.3.0` / `0.4.0` / `0.5.0` |
| git tag / Release | `v0.1.0`、`v0.2.0`、`v0.2.1`、`v0.3.0`、`v0.4.0`、`v0.5.0`（annotated + Release） |
| docs.rs | `0.4.0` / `0.5.0` 均未从 docs.rs API 查到构建记录，需人工确认 |
| 测试 | 发布门禁在 Actions `release.yml` 全绿（run 42638556781） |
| clippy / fmt | 两个 crate `-D warnings` 全绿 |
| MSRV | 1.88 |
| 下一版本前提 | 项目所有者明确书面授权（版本 + 内容摘要 + 时间）；`0.5.0` 已发布且不可覆盖 |

---

## 6. 发布说明写作规范

`CHANGELOG.md` 给维护者；`docs/releases/vX.Y.Z.md` 与 GitHub Release 给使用者。
同一版本两份文档视角不同，不能直接复制。

命名规则：`docs/releases/vX.Y.Z.md`，tag `vX.Y.Z`（annotated），
GitHub Release 标题为 `vX.Y.Z — <一句用户可感知的变化>`。

### 6.1 结构（必须）

```markdown
# vX.Y.Z — <一句用户可感知的变化>

发布日期：YYYY-MM-DD
对比基线：vA.B.C

## 升级注意
- <破坏性变更 / 默认行为变化 / 必须执行的迁移；没有则写“无”>

## 新增
- <用户能做什么、行为怎么变；必要时给配置示例>

## 修复
- <现象 → 修复后的行为>

## 验证边界
- <未真机验证的平台或场景；没有则写“无”>
```

### 6.2 写什么

- **行为变化**：更新前怎样、更新后怎样。
- **用户需要做什么**：改配置、换依赖、验证哪条路径。
- **可执行示例**：配置片段、命令、迁移步骤。
- **已知边界**：哪些平台/场景还没真机验证，不用 CI 绿替代结论。

### 6.3 不写什么

- commit、分支、文件路径、模块名、内部函数名。
- 测试数量、clippy/fmt、CI job 名、构建耗时。
- “重构 / 优化发布端 / 代码质量提升”这类维护者视角总结。
- 内部枚举属性（`#[non_exhaustive]`）、内部协议字段、实现细节。

### 6.4 发布检查清单

- [ ] 版本五处一致：核心、插件、测试夹具（`tdx-testkit`）、npm、CHANGELOG 顶部
- [ ] `docs/releases/vX.Y.Z.md` 已按模板填写，且没有内部语言
- [ ] annotated tag `vX.Y.Z` 已创建并推送
- [ ] `release.yml` 三个 job 全绿（或手动三步均成功）
- [ ] crates.io 核心/插件/测试夹具、npm 均可访问；docs.rs 已确认
- [ ] GitHub Release 正文与 `docs/releases/vX.Y.Z.md` 一致
