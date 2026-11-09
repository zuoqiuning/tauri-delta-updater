# 架构与失败恢复

本文是面向贡献者的精简设计文档：只描述**必须保持不变的结构与不变量**。
逐文件的“改什么 → 改哪里 → 哪个测试守护”见 [SOURCE_MAP.md](SOURCE_MAP.md)。

## 1. 定位与边界

`tauri-delta-updater` 是完整更新引擎，不是“只缩小下载”的透明层：

- 自己校验签名、规划补丁链、执行替换、处理回滚与修复；
- 增量不可用时回退全量安装器（`install_full`），或把 `{url, signature}` 交给宿主；
- 清单格式与官方 updater 兼容，但**清单本身必须签名**（防降级重放）。

明确不负责：安装器内部失败（NSIS/MSI/Inno 的退出码与回滚）、包管理器元数据同步、
签名/公证/UAC 的真机行为。

## 2. 模块与依赖方向

```text
core/       平台无关：差分、下载、校验、锁、安全、磁盘、启动状态、整包归档
platform/   平台实现：PowerShell / sh / .app 目录切换 / 安装器 / Authenticode
updater/    编排：DeltaUpdater、配置、结果类型、plan、pending、fetch
plugin/     Tauri 薄壳：命令、事件、权限；不复制引擎逻辑
release/    发布工具（generator feature）：补丁生成、清单签名、报告
```

依赖只允许向下：`plugin → updater → platform → core`。
`models/` 是数据结构，除 `platform::target` 的版本/平台命名外不应依赖运行环境。

## 3. 客户端主流程

```text
install()
  → run_entry: needs_repair → SafeMode → run_prepare
  → run_prepare: UpdateLock → run_locked
      → fetch_verified_manifest（并行拉清单+签名，验签，逐块校验 tdu:v=）
      → is_newer_than / min_os / rollout
      → cleanup_previous_updates_except（保留 pending 与进行中的备份）
      → 整包优先 → 单文件直达/多跳链 → 否则回退全量
      → 逐跳：下载 → patch_hash → 应用 → target_hash
      → verify_integrity + Authenticode
      → 生成替换脚本 → clear_failed_attempts → launch / pending
```

`check()` 复用同一套门槛判定，只拉清单不下载；`UpdateOutcome` / `UpdateResult`
的 `UnsupportedOs` / `Deferred` 与 `CheckOutcome.unsupported_os` 必须语义一致。

## 4. 不可破坏的不变量

1. **验签失败永远终止**：不因备用端点、全量回退或安全模式而继续。
2. **双端哈希**：补丁字节与其合并产物都被签名清单锚定；`VerifiedArtifact`
   在类型上阻止未验证字节进入替换流程。
3. **TOCTOU**：脚本在复制前后各复核一次暂存文件哈希；替换后再次校验目标。
4. **版本单调**：清单版本必须严格高于当前版本；旧清单重放只能得到 `UpToDate`。
5. **跨进程互斥**：增量与全量路径共用 `UpdateLock`；并发返回 `update_in_progress`。
6. **启动状态机**：`staged → booting → committed / failed / rolled_back`；
   `committed` 之后不得再按“未完成替换”回滚。
7. **安全模式只计确定性错误**：网络/超时/取消不计入；连续 3 次后冷却 6 小时。
8. **路径限定**：pending 的脚本与产物必须 canonicalize 后仍在工作目录内。
9. **清理不碰用户数据**：只删精确命名的引擎产物；失败计数与 pending 另有规则。

## 5. 失败恢复

| 失败点 | 恢复方式 |
|---|---|
| 下载/验签/哈希失败 | 删除半截文件，按 `is_retryable` 决定重试；确定性错误计入安全模式 |
| 旧进程未退出 | 脚本等待超时后中止，保留旧版本 |
| 复制/重命名失败 | 脚本还原 `.bak` 并重启旧版本；Unix 半截目标会被备份覆盖 |
| 新版本启动超时/崩溃/健康检查失败 | 脚本结束新进程、还原备份、写 `startup_timeout` / `startup_crashed` / `health_check_failed` |
| 整包替换中途崩溃 | 下一次启动由 `bundle_repair_needed` 生成修复脚本，先删新增文件再还原备份 |
| 已 committed 但备份未删 | 不回滚；启动确认会清理备份与 journal |
| 全量安装器失败 | 引擎只上报 `full_install_failed`；回滚由安装器负责 |

脚本日志：`tdx_update.log`；结构化事件：`tdx_events.jsonl`；
下一次启动通过 `diagnose()` 读取 `startup_state` / `last_rollback` / `rollback_history`。

## 6. 平台差异

- **Windows**：PowerShell 5.1 + UTF-8 BOM；NSIS/MSI/Inno 参数不同；`explorer.exe`
  降权重启；Authenticode 退化阻断；`.exe` 未显式配置时按内容识别 Inno/NSIS。
- **macOS**：`.app` 以目录为单位切换（保留签名与符号链接）；切换前
  `codesign --verify --deep --strict`；App Translocation 回退。
- **Linux**：裸二进制与 AppImage 原地替换；`sudo -n` → `pkexec` 提权后用
  `runuser` / `su` 降权；`.deb` / `.rpm` 全量安装包明确拒绝。

## 7. 发布端

`generate` 与 `cargo delta release` 共用 `release::run_release`，按五阶段执行：

1. `prepare_context`：校验参数、规范化版本/通道、加载密钥、计算哈希；
2. `prepare_manifest`：读取旧清单并写入本次平台条目；
3. 补丁生成：单文件补丁逐个真实应用验证；
4. `finalize_manifest`：合并历史补丁链、签名安装包与清单；
5. `write_outputs`：写清单/签名/公钥/SHA256SUMS/报告/归档。

- 每个补丁真实应用并校验产物哈希，坏补丁在 CI 失败；
- 清单签名覆盖最终内容，`tdu:v=` 写入 trusted comment；
- 整包补丁写入独立签名的 `latest.bundle.json`；
- `doctor` 在发布后验证线上桶（签名、体积、哈希、链可达性）。
- 互斥锁默认只覆盖用户工作目录；`lock_scope = "install_dir"` 会在安装目录
  额外加一把跨用户锁（目录只读时退回并告警）。

### 7.1 补丁容器

| 容器 | 产出 | 应用条件 |
|---|---|---|
| `BSDIFF40`（未压缩） | `--legacy-patch` | 所有版本 |
| zstd(`BSDIFF40`) 默认 | `generate` / `cargo delta release` | 0.2 起（认 zstd 容器） |
| `TDXB1` 分块差分 | `--large-file-delta` 且 bsdiff 工作集超限 | 0.5.0 起 |

`apply_patch` 只读前 5 字节判断容器类型，三种格式共用同一套
"补丁哈希 → 双端哈希"校验；未知格式仍按 bsdiff 处理，旧清单无需迁移。

`TDXB1` 的可选性来自兼容性：它只有 0.5.0 起的客户端能应用，发布端必须
显式开启 `--large-file-delta`。容器内的 `PATCH` 指令用于"分块匹配失败、
但两侧 COPY 能定位出对应旧内容"的缺口，在这些缺口上再做一次有界
（≤4 MiB）的 bsdiff，从而在低内存前提下接近常规补丁体积。

### 7.2 并发预算

补丁任务按**预估工作集**加权限流（单位 256 MiB，默认总预算 4 GiB，
`--patch-memory-budget-mb` / `TDX_PATCH_MEMORY_BUDGET_MB`），
CPU 并发另由 `--jobs` / `TDX_PATCH_JOBS` 封顶。单文件与整包补丁都按基线
并行生成，`join_all` 保序，报告与清单输出与串行版本一致。
`TDXB1` 的索引内存约为 `size / 8 KiB × 24 B`，与文件规模线性但系数很低。

## 8. 安全边界（摘要）

签名与哈希保证“内容出自发布者且未被替换”；不保证发布者本身可信。
可写工作目录的本地攻击者可以伪造成功标志或 pending 记录，因此工作目录必须
属于当前用户且权限合理。`min_os`、灰度与 Authenticode 检查是**兼容性/可用性
约束**，不是密码学边界。完整威胁矩阵见 README 的 “Security boundaries”。
