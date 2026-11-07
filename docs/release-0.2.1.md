# v0.2.1 — 跨重启待安装、多跳链、多密钥信任

发布日期：2026-11-07
对比基线：`v0.2.0`（2026-11-07，crates.io 已发布，不可覆盖）

> **发布状态**：项目所有者已授权发布 `0.2.1`（2026-11-07，PATCH）。

0.2.1 是 **PATCH**：`DeltaError` / `UpdateOutcome` / `UpdateResult` 在 0.2.0
已发布版本中均已标记 `#[non_exhaustive]`，新增 `ChainHopFailed` /
`UnsupportedOs` 变体不构成破坏性变更（复核方式见 `docs/发布.md` §1.5）。

## 新增

- **跨重启待安装**：`pending` / `discard_pending` / `install_pending` 命令与
  `install_on_next_launch`；暂存产物跨重启保留，下次启动离线校验并执行。
- **多跳补丁链**：最多 `MAX_DELTA_CHAIN_HOPS` 跳，链总成本超过安装包预算时
  视为无补丁；失败时 `DeltaError::ChainHopFailed` 指出第几跳并保留底层错误码。
- **多密钥信任**：`additional_pubkeys` / `TrustPolicy`（`any` / `all`）与
  `generate --secondary-private-key` 双签，支持无停机密钥轮换。
- **最低系统版本**：清单 `min_os`，Windows/macOS/Linux 版本探测；不满足时
  不返回下载地址（best-effort，不是安全边界）。
- **渠道与本地化**：`channel` 占位符、`notes_i18n` + `notes_for(locale)`、
  清单未知字段 `extra` 透传。
- **运行时增强**：请求头 `headers`、端点故障转移 `fallback_endpoints` /
  `fallback_policy`、Windows `install_mode`、`allow_insecure_transport`。

## 安装

- 核心 crate：https://crates.io/crates/tauri-delta-updater/0.2.1
- API 文档：https://docs.rs/tauri-delta-updater/0.2.1
- 插件 crate：https://crates.io/crates/tauri-plugin-delta-updater/0.2.1
  （首次发布）
- npm 前端绑定：`tauri-plugin-delta-updater@0.2.1`（首次发布）

插件是 crate + npm 双分发：crate 不含 `guest-js` / `dist-js`，需要类型化
绑定时单独安装 npm 包。

## 未验证项

以下路径在本轮**没有真机验证**，CI 只做了静态与合成脚本断言：

- macOS `.app` 目录切换的 Gatekeeper / 公证行为；
- Windows UAC 提权、`install_full` 的 NSIS/MSI 参数在真实安装器上的行为，
  以及 `install_and_exit` 与 `tauri-plugin-single-instance` 的锁释放；
- Linux 提权（`sudo -n`）在真实发行版上的行为与降权启动。

遇到问题请附 `tdx_update.log` 与 `tdx_events.jsonl` 开 issue。

## 授权

项目所有者于 2026-11-07 确认 `0.2.1` 发布：版本 `0.2.1`（核心 / 插件 / npm），
内容为跨重启待安装、多跳补丁链、多密钥信任、最低系统版本、渠道与本地化透传，
时间窗口 2026-11-07。
