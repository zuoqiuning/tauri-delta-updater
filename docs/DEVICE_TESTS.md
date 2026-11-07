# 真机验证清单（发布前）

CI 覆盖静态断言与合成脚本；以下路径必须在真实机器上跑一次，结果写入对应版本的
`docs/release-<version>.md`。未执行的项必须标注为"未验证"，不能用 CI 绿替代。

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

## Linux

- [ ] `.deb` 安装到 `/usr/bin` 后更新：确认 `pkexec` / `sudo -n` 行为，失败时回滚。
- [ ] AppImage：直接执行 `.AppImage` → 更新 → 确认本体被替换、`$APPIMAGE` 路径正确。
- [ ] 普通用户对只读目录更新：应中止并保留旧版本，不产生半成品。

## 记录

每个平台记录：OS 版本、安装方式、结果（通过 / 失败）、失败时的 `tdx_update.log`
与 `tdx_events.jsonl`。三项平台全部执行或明确标注未验证后，才可创建 Release。
