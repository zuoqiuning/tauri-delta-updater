# Changelog

本文件记录 `tauri-delta-updater` 的版本变更。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

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
