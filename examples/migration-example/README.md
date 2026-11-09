# 官方 updater → delta-updater 前端迁移示例

- `before.ts`：`@tauri-apps/plugin-updater` 的 `check()` + `downloadAndInstall()` + `relaunch()`。
- `after.ts`：两种等价写法
  - `runCompat()`：保留官方调用形状，改用 `checkCompat()`；进度事件仍是
    `Started` / `Progress` / `Finished`。
  - `runNative()`：直接使用 `install()` 的阶段事件（`downloading` / `applying` /
    `ready` / `safe_mode`），适合需要展示更细状态或自定义退出时机的 UI。

## 迁移清单

1. `cargo delta migrate --apply`：配置、capability、依赖一次改完；
   CLI 会打印可直接粘贴的 `checkCompat()` 片段。
2. 前端依赖换成 `@tauri-apps/plugin-delta-updater`。
3. capability 至少包含 `delta-updater:default`；使用 `checkCompat()` 或
   `installFull()` 时再加 `delta-updater:full`。
4. `npm run build` 之前确认 `Channel` 传给了命令（本包函数已封装，不需要手动传）。
5. 更新成功后进程必须退出：`install()` 会自动退出；自己控制时用
   `requiresExit(result)` 判断。

## 行为差异

| 官方 updater | 本项目 |
|---|---|
| 只有全量安装包 | 优先补丁；`requires_full_download` 时再走全量 |
| `downloadAndInstall` 回调 | 阶段事件；`checkCompat()` 提供官方形状的兼容层 |
| 更新失败后的回滚由安装器负责 | 增量路径有确定性回滚（备份 + 启动确认）；全量仍由安装器负责 |
| `relaunch()` 由前端调用 | 插件在脚本启动后自动退出；`prepare()` + `launch()` 可自定义时机 |
