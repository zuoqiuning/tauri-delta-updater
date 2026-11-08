# 真机验证清单（发布前）

CI 覆盖静态断言与合成脚本；以下路径必须在真实机器上跑一次，结果写入对应版本的
`docs/releases/v<version>.md`。未执行的项必须标注为"未验证"，不能用 CI 绿替代。

## macOS

```bash
codesign -dv --verbose=4 /Applications/MyApp.app
spctl -a -vv /Applications/MyApp.app
```

- [ ] 用已签名 + 公证的构建完整更新一次（`.app` 目录切换）。
- [ ] 重启后版本号变化，`spctl -a` 仍返回 accepted。
- [ ] 篡改补丁导致失败时能回滚到旧版本并正常启动。

## Windows

- [ ] 普通用户安装 → 更新 → UAC 弹窗 → 允许 → 重启后是新版本。
- [ ] UAC 拒绝 → 旧版本被重新拉起（不是应用消失）。
- [ ] 与 `tauri-plugin-single-instance` 同时使用：更新后重启不被判为第二实例。
- [ ] 安装目录含空格与特殊字符（如 `C:\Program Files\My App (x86)`）。
- [ ] Inno Setup 安装包（配置 `installer_kind: "inno"`）：**先确认全量更新调用的是引擎
      `install_full`，不是官方 `tauri-plugin-updater`**（官方插件只会传 NSIS 参数，
      Inno 会弹完整向导）；`Passive` 与 `Quiet` 各跑一次，确认没有
      "This will install…" 起始确认框、安装过程中系统不会自动重启。
- [ ] 静默更新完成后应用按 `.iss` 的 `[Run]` 段启动（或按产品约定明确不启动）；
      `skipifsilent` 会跳过静默模式下的启动，添加前先确认这是预期行为。
- [ ] 从旧 NSIS 版本升级到 Inno：`Apps & Features` 里只有一条卸载记录，卸载仍可用
      （旧 ARP 清理由 `.iss` 负责，不是引擎职责）。

## Linux

- [ ] `.deb` 安装到 `/usr/bin` 后走**单文件增量**（实验性）：确认 `pkexec` / `sudo -n`
      行为，失败时回滚；同时确认 `dpkg -l` 仍报告旧版本（包管理器元数据不更新）。
- [ ] 全量安装包为 `.deb` / `.rpm` 时必须被拒绝（`full_install_failed`），提示走包管理器。
- [ ] AppImage：直接执行 `.AppImage` → 更新 → 确认本体被替换、`$APPIMAGE` 路径正确。
- [ ] 普通用户对只读目录更新：应中止并保留旧版本，不产生半成品。

## 失败恢复（手工，需要真实超时窗口）

- [ ] 默认 60 秒启动确认超时：发布一个启动后 70 秒才调用确认（或故意不确认）的新版本，
      确认脚本在超时后写 `startup_timeout`、还原 `.bak` 并重启旧版本；
      `diagnose().startup_state` 为 `rolled_back`，`last_rollback` 为 `startup_timeout`。
- [ ] 健康检查失败立即回滚：`confirm_startup_success_with(|| Err(...))` 后
      `last_rollback` 为 `health_check_failed`，不需要等满 60 秒。

## 安装器语义对照（跨安装器不可类比）

改 NSIS / MSI / Inno 参数或 `.iss` 时，逐条对照官方文档，不要凭另一个安装器的直觉：

| 意图 | NSIS（Tauri 模板） | Inno Setup |
|---|---|---|
| 显示进度但不交互 | `/P` | `/SILENT` |
| 完全无界面 | `/S` | `/VERYSILENT` |
| 装完启动应用 | `/R` | `[Run]` 段，且**不能**加 `skipifsilent` |
| 这是更新而非首次安装 | `/UPDATE` | `.iss` 读取旧 ARP / `UsePreviousAppDir`（无命令行参数） |
| 关闭起始确认框 | 模板行为 | `/SP-` |
| 不自动重启系统 | — | `/NORESTART` |

- `skipifsilent` 的语义是“静默模式不执行”，与 NSIS `/R` 正好相反；加错会导致
  静默更新完成后应用不启动。
- MSI 使用 `/passive` / `/qb` / `/qn` 与 `AUTOLAUNCHAPP=True`，与上面两套都不同。

## 记录

每个平台记录：OS 版本、安装方式、结果（通过 / 失败）、失败时的 `tdx_update.log`
与 `tdx_events.jsonl`。三项平台全部执行或明确标注未验证后，才可创建 Release。
