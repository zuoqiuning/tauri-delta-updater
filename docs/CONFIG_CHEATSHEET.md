# 配置速查与决策树

## 先判断你属于哪个场景

```text
需要替换 sidecar / resources，或目标是 macOS .app？
├─ 是 → 配置 bundle_endpoint（整包增量）
└─ 否 → 只配置 endpoints（单文件增量）
     └─ 前端需要调用 installFull()/downloadFull()，或使用 checkCompat()？
        ├─ 是 → capability 加 delta-updater:full，配置安装器相关字段
        └─ 否 → capability 只需 delta-updater:default

安装器是 Inno Setup？
└─ 是 → installer_kind: "inno"（否则会被按 NSIS 参数调用）

需要灰度放量？
└─ 发布端 --rollout / --min-version；客户端无需配置

多用户共享 per-machine 安装？
└─ lock_scope: "install_dir"（安装目录只读时自动退回 work_dir）
```

## 插件配置（`tauri.conf.json` → `plugins.delta-updater`）

| 字段 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `endpoints` | 是 | — | 清单地址数组；第一个为主端点，其余备用 |
| `pubkey` | 是 | — | 固化在应用里的公钥（base64 或 `.pub` 内容） |
| `additional_pubkeys` | 否 | `[]` | 轮换期的额外受信任公钥 |
| `trust_policy` | 否 | `any` | `any`（任一有效签名）/ `all`（全部签名） |
| `channel` | 否 | `stable` | 端点可用的 `{{channel}}` 占位符 |
| `bundle_endpoint` | 否 | — | 整包清单 `latest.bundle.json` |
| `signature_url` | 否 | 清单同名 `.sig` | 签名独立托管时使用 |
| `bundle_type` | 否 | — | 端点 `{{bundle_type}}` 的取值（nsis/msi/appimage/app） |
| `install_id` | 否 | 自动生成 | 灰度分桶的稳定安装 ID |
| `work_dir` | 否 | 用户缓存目录 | 补丁、脚本、日志所在处 |
| `wait_timeout_secs` | 否 | `60` | 等待旧进程退出 |
| `confirm_timeout_secs` | 否 | `60` | 等待新版本启动确认 |
| `proxy` | 否 | — | HTTP 代理（设置后使用内置客户端） |
| `headers` | 否 | `{}` | 附加请求头（CDN Token / UA） |
| `allow_legacy_signatures` | 否 | `false` | 只给第三方旧签名管线用 |
| `fallback_policy` | 否 | `transport` | 主端点失败后何时切备用 |
| `install_mode` | 否 | `passive` | Windows 安装器 UI（passive/basicUi/quiet） |
| `installer_kind` | 否 | 按后缀推断 | Inno 必须显式写 `inno` |
| `installer_args` | 否 | `[]` | 追加在固定参数之后的安装器参数 |
| `authenticode_policy` | 否 | `strict` | Windows 产物签名策略（strict/permissive/off） |
| `lock_scope` | 否 | `work_dir` | `install_dir` 增加跨用户锁 |
| `allow_insecure_transport` | 否 | `false` | 只用于本地/内网 HTTP 联调 |
| `install_on_next_launch` | 否 | `false` | 下次启动自动执行 pending |
| `rollback_notice` | 否 | `true` | 回滚后弹系统提示 |

官方 `tauri-plugin-updater` 的 `windows.installMode` / `windows.installerArgs`
会自动迁移；`dangerousInsecureTransportProtocol` 不会静默生效（需显式
`allow_insecure_transport`）。

## 库配置（`UpdateConfig`）额外字段

`client`、`executable_path`、`platform`、`current_version`、`auto_launch`、
`fallback_manifest_urls`、`rollback_notice`、`authenticode_policy`、`lock_scope`
等；`UpdateConfig::new(manifest_url, public_key)` 后按需链式覆盖。

## 权限集

| 权限 | 覆盖命令 | 何时授予 |
|---|---|---|
| `delta-updater:default` | 检查、增量安装、诊断、取消、安全模式、pending | 几乎所有宿主 |
| `delta-updater:read` | `check` / `diagnose` / `config_warnings` / `pending` | 只读面板 |
| `delta-updater:update` | `install` / `prepare` / `launch` / `cancel` / `reset_safety` / pending 操作 | 增量更新窗口 |
| `delta-updater:full` | `download_full` / `install_full` | 需要平台安装器兜底的窗口 |

## 发布端参数

| 参数 | 作用 |
|---|---|
| `--from` | 基线目录或单个主程序（`release` 与 `generate` 通用） |
| `--installer` | 全量安装包；省略时全量地址指向主程序 |
| `--new-app-dir` / `--old-app-dir` | 整包增量（sidecar/resources/.app） |
| `--rollout` | 灰度比例 0–100 |
| `--min-version` | 低于它的客户端无视灰度强制更新 |
| `--allow-http` | 显式允许明文 `base_url`（默认必须 HTTPS） |
| `--sha256sums` | 产出所有制品的校验和 |
