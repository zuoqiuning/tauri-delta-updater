# 发布文档规范（所有版本通用）

> 适用于**引擎自身发版**：crates.io 核心/插件、npm、GitHub Release。
> 应用更新清单、补丁与 CDN 的发布属于宿主仓库，不适用本规范。
> 发布授权、门禁与失败补救见 [RELEASING.md](RELEASING.md)。

## 1. 每次发布必须产出的文档

| 文件 | 视角 | 内容要求 |
|---|---|---|
| `CHANGELOG.md` 顶部 `## [X.Y.Z]` | 维护者 | 技术完整：兼容性、API 变化、内部判定 |
| `docs/releases/vX.Y.Z.md` | 使用者 | 从 [TEMPLATE.md](releases/TEMPLATE.md) 复制，写行为变化与升级动作 |
| GitHub Release 正文 | 使用者 | 与 `docs/releases/vX.Y.Z.md` 完全一致 |

命名规则：语义化版本；文件 `docs/releases/vX.Y.Z.md`；tag `vX.Y.Z`（annotated）。
GitHub Release 标题使用 `vX.Y.Z — <一句用户可感知的变化>`。

## 2. 三平台同步发布（通用命令）

推荐入口是 GitHub Actions 的 `release.yml`（`workflow_dispatch`），它按顺序发布
core → plugin → npm，任一步失败会停在可重试位置：

```bash
gh workflow run release.yml -f version=<version> -f publish=true
gh run watch --workflow release.yml
```

前置条件：版本四处一致、annotated tag 已推送、仓库已配置
`CARGO_REGISTRY_TOKEN` / `NPM_TOKEN`、`release` environment 审批人已就绪。

workflow 发布内容：

1. `tauri-delta-updater@<version>`（crates.io）
2. `tauri-plugin-delta-updater@<version>`（crates.io，等待核心索引可见）
3. `tauri-plugin-delta-updater@<version>`（npm）
4. GitHub Release **不由该 workflow 创建**，用发布说明单独创建：

```bash
gh release create v<version> \
  --title "v<version> — <一句用户可感知的变化>" \
  --notes-file docs/releases/v<version>.md --latest
```

本机手动等价命令（没有 Actions 权限时）：

```bash
git push origin main
git push origin v<version>
cargo publish --registry crates-io --locked
# 等 crates.io 上核心版本可见，再发插件
cargo publish --manifest-path plugin/Cargo.toml --registry crates-io --locked
npm publish --access public --registry https://registry.npmjs.org
gh release create v<version> \
  --title "v<version> — <一句用户可感知的变化>" \
  --notes-file docs/releases/v<version>.md --latest
```

命令规范：

- **cargo 必须带 `--registry crates-io`**：本机常配置镜像/源替换，不带它可能只更新
  镜像索引而不会上传 crates.io。
- **npm 必须显式官方 registry**：本机 `NPM_CONFIG_REGISTRY` 可能指向镜像；
  `npm publish --registry https://registry.npmjs.org` 才是正式发布。
- **先核心后插件**：插件校验会从注册表解析核心版本，核心不可见时插件必然失败。
- **npm 放最后**：crate 失败时不要让前端包指向一个不存在的 Rust 版本。
- **tag 在发布前推送**：annotated tag，版本一旦发布不可覆盖；不要移动或删除 tag
  后重发同一版本。
- 只重发 npm 时，重跑 `publish-npm` job 或单独执行 `npm publish`，不要重复
  `cargo publish`（版本已存在会被拒绝）。

发布后验证：

```bash
curl -sf https://crates.io/api/v1/crates/tauri-delta-updater/<version>
curl -sf https://crates.io/api/v1/crates/tauri-plugin-delta-updater/<version>
npm view tauri-plugin-delta-updater@<version> version --registry https://registry.npmjs.org
```

## 3. 发布说明写作规范（使用者视角）

`CHANGELOG.md` 给维护者看；`docs/releases/vX.Y.Z.md` 和 GitHub Release 给使用者看。
同一个版本，两份文档的视角不同，不能直接复制。

### 3.1 结构（必须）

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

### 3.2 写什么

- 写**行为变化**：更新前怎样、更新后怎样。
- 写**用户需要做什么**：改配置、换依赖、验证哪条路径。
- 写**可执行的示例**：配置片段、命令、迁移步骤。
- 写**已知边界**：哪些平台/场景还没真机验证。
- 必要的 API 名称只放在“升级注意”或配置示例里。

### 3.3 不写什么

- 不写 commit、分支、文件路径、模块名、内部函数名。
- 不写测试数量、clippy/fmt、CI job 名、构建耗时。
- 不写“重构 / 优化发布端 / 代码质量提升”这类维护者视角的总结。
- 不写内部枚举属性（如 `#[non_exhaustive]`）、内部协议字段、实现细节。
- 不把 CHANGELOG 的兼容性段落原样搬过来。

### 3.4 改写方法

1. 从 `CHANGELOG.md` 提取事实：新增、修复、破坏性变更。
2. 把“改了什么代码”改写为“用户会看到什么变化”。
3. 破坏性变更、默认值变化、必须执行的迁移放到最前。
4. 删除 commit、测试数量、模块名和内部实现描述。
5. 补上验证边界，不用 CI 绿替代真机结论。
6. 全文以“一屏能读完”为准，细节留在文档里。

### 3.5 正反例

反例（维护者语言）：

```text
- 重构 release 模块，generate 与 cargo delta release 共用实现。
- 231 项测试通过，clippy 全绿。
- UpdateConfig 标记 #[non_exhaustive]。
```

正例（使用者语言）：

```text
- 发布工具收拢为一条命令：`cargo delta release ...`，不再需要手写校验和脚本。
- 用结构体字面量构造 `UpdateConfig` 的代码需改用 builder；通过 builder/serde
  配置的调用方不受影响。
```

## 4. 发布检查清单

- [ ] 版本四处一致：核心、插件、npm、CHANGELOG 顶部
- [ ] `docs/releases/vX.Y.Z.md` 已按模板填写，且没有内部语言
- [ ] annotated tag `vX.Y.Z` 已创建并推送
- [ ] `release.yml` 三个 job 全绿（或手动三步均成功）
- [ ] crates.io 核心/插件、npm、docs.rs 均可访问
- [ ] GitHub Release 正文与 `docs/releases/vX.Y.Z.md` 一致
