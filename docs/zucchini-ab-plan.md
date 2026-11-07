# Zucchini A/B 验证计划

> 状态：**工具与 CI 入口已就绪，数据待 CI 产物**。Windows 本机没有 clang，
> 且上游 `prebuilt` 不提供 `x86_64-pc-windows-msvc` 库；CI 的 Ubuntu runner
> 用 `prebuilt` 即可运行，不需要 clang。手动触发 `.github/workflows/zucchini-ab.yml`。

## 为什么先做 A/B，不直接集成

`zucchini` 0.1.1 是 Chromium Zucchini 的绑定，`zucchini-sys` 需要从 Chromium
源码构建 C++ 静态库：

- 需要 clang；Windows 上通常还需要对应工具链与构建目录；
- 许可为 BSD-3-Clause，二进制分发必须附带 Chromium 的 LICENSE；
- 补丁头使用 32 位长度，文件上限 4 GiB；
- PE / ELF 之外的格式会退化为原始字节差分。

主引擎以“零额外系统依赖”为卖点，因此它不能进入默认依赖；只有在数据证明
收益稳定且显著时，才考虑 feature-gated 后端。

## 工具

[`tools/zucchini-ab`](../tools/zucchini-ab) 是独立 crate，不参与主 crate 构建：

```bash
# Linux/macOS：默认 prebuilt，无需 clang
cargo run --release --manifest-path tools/zucchini-ab/Cargo.toml -- old new

# Windows：--no-default-features 从源码构建（需要 clang）
cargo run --release --no-default-features \
  --manifest-path tools/zucchini-ab/Cargo.toml -- old.exe new.exe
```

CI 入口：GitHub Actions 手动触发 `Zucchini A/B`，在 Ubuntu 上生成
“仅版本字符串变化”与“一个模块重编译”两份真实 ELF，输出三列对比并上传
`zucchini-ab-<date>.md` artifact。

输出：

```text
new file:            20971520 bytes
bsdiff + zstd:        1234567 bytes (5.89%)
zucchini raw:          654321 bytes (3.12%)
zucchini + zstd:       345678 bytes (1.65%)
```

工具会先用 Zucchini apply 还原并断言逐字节一致，再打印体积。

## 采样要求

至少 5–10 对真实发布产物，覆盖：

1. **仅版本字符串变化**：同一构建参数，只改版本号；
2. **仅时间戳/构建元数据变化**：同代码，重新构建；
3. **一个模块重编译**：局部代码变化；
4. **跨编译器/链接器参数变化**：不同工具链或优化级别（这类最容易暴露字节级差分的弱点）；
5. 如有条件，加入 macOS 产物作为对照（Zucchini 对 Mach-O 只做原始字节差分，预期不会优于 bsdiff）。

每组记录：旧/新文件体积、bsdiff+zstd、Zucchini raw、Zucchini+zstd、apply 是否通过。

## 决策阈值（单模块重编译场景）

以 `Zucchini+zstd / bsdiff+zstd` 的补丁体积比为准：

| 比值 | 决策 |
|---|---|
| **< 50%** | 进入 feature-gated 后端阶段（默认关闭，`generate --zucchini` 启用） |
| **50%–80%** | 记录收益，但不集成：不足以补偿 Chromium 构建依赖与许可成本 |
| **> 80%** | 关闭该路径，资源转给跨平台真实文件系统验证 |

其他场景仍按下面的通用门槛判断。

## 进入 feature-gated 后端的门槛

- 在上述至少 4 类场景中，Zucchini+zstd **稳定小于** bsdiff+zstd；
- 没有一类场景出现“Zucchini 明显更大且无法回退”；
- `apply` 在全部样本上逐字节一致；
- 构建文档明确 clang/许可/4 GiB 边界；
- 默认关闭，`generate --zucchini` 显式启用，补丁头魔数区分后端。

在满足这些条件之前，Zucchini 只作为实验工具存在，不进入 `core::diff` 主路径。

## 当前结论

数据未采集，**不集成**。下一步是在有 clang 的机器上跑完采样，再把结果写入
`docs/zucchini-ab-<date>.md`，并与本计划的准入门槛逐条对照。
