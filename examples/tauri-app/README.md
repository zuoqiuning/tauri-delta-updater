# tauri-app 示例

最小可运行的 Tauri 2 应用，演示 `tauri-plugin-delta-updater` 的接入：

- `src-tauri/tauri.conf.json`：插件配置（`endpoints` + `pubkey`）。
- `src-tauri/capabilities/default.json`：`delta-updater:default`
  （只走增量；需要 `installFull()` / `checkCompat()` 时再加
  `delta-updater:full`）。
- `src/`：前端调用 `check()` / `install()`。

## 运行

```bash
cd examples/tauri-app
npm install
npm run tauri dev
```

示例没有配置真实的发布源，`check()` 会失败；把 `endpoints` 与 `pubkey` 换成
自己的 CDN 与公钥后即可联调。

## 重要：依赖路径

`src-tauri/Cargo.toml` 使用的是仓库内 `path = "../../../plugin"` 依赖，
**只用于本仓库开发/测试**。复制到真实项目后必须改为 crates.io 版本：

```toml
tauri-plugin-delta-updater = "0.3"
# 只有宿主需要直接调用库时才加：
# tauri-delta-updater = "0.3"
```

保留 `path` 会让 CI（没有本仓库目录）解析失败。
