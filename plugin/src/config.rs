//! 插件注册与 `tauri.conf.json` 配置读取。

use std::collections::HashMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tauri::{AppHandle, Runtime};
use tauri_delta_updater::{FallbackPolicy, InstallMode, InstallerKind, TrustPolicy, UpdateConfig};

/// `tauri.conf.json` 里 `plugins.delta-updater` 的配置。
///
/// 字段与官方 updater 的配置兼容：`endpoints` + `pubkey` 可以直接复用，
/// 迁移时不必改配置。官方配置里的 `windows.installMode` /
/// `windows.installerArgs` 会自动迁移到对应的顶层字段；其它官方字段
/// （`dmg` 等）会被忽略。`dangerousInsecureTransportProtocol` 不会自动开启
/// 明文传输，启动警告会提示改用 `allow_insecure_transport`。
#[derive(Debug, serde::Deserialize)]
pub(crate) struct FileConfig {
    /// 清单地址。与官方一致用数组：第一个是主端点，其余作为备用端点。
    pub(crate) endpoints: Vec<String>,
    /// 固化在应用里的公钥（base64 或 `.pub` 文件内容）。
    pub(crate) pubkey: String,
    /// 可选：额外的受信任公钥（密钥轮换 / 双签过渡）。
    #[serde(default)]
    pub(crate) additional_pubkeys: Option<Vec<String>>,
    /// 可选：多钥信任策略（`any` 默认 / `all` 要求全部签名）。
    #[serde(default)]
    pub(crate) trust_policy: Option<String>,
    /// 可选：发布渠道，默认 `stable`；端点可用 `{{channel}}` 占位符。
    #[serde(default)]
    pub(crate) channel: Option<String>,
    /// 可选：整包清单地址（`latest.bundle.json`）。
    #[serde(default)]
    pub(crate) bundle_endpoint: Option<String>,
    /// 可选：主清单签名的独立地址；缺省按清单地址同名 + `.sig` 推导。
    ///
    /// 只对主端点生效；备用端点各自按自己的清单地址推导。
    #[serde(default)]
    pub(crate) signature_url: Option<String>,
    /// 可选：安装包类型，用于展开端点里的 `{{bundle_type}}`。
    ///
    /// 官方配置没有这个字段；在 URL 里使用该占位符时必须提供，
    /// 取值按发布形态填写（`nsis` / `msi` / `appimage` / `app`）。
    #[serde(default)]
    pub(crate) bundle_type: Option<String>,
    /// 可选：灰度分桶使用的稳定安装 ID；缺省由库自动生成并持久化。
    #[serde(default)]
    pub(crate) install_id: Option<String>,
    /// 可选：覆盖工作目录（补丁、脚本、日志与标志文件所在处）。
    #[serde(default)]
    pub(crate) work_dir: Option<String>,
    /// 可选：等待旧进程退出的秒数（默认 60）。
    #[serde(default)]
    pub(crate) wait_timeout_secs: Option<u32>,
    /// 可选：等待新版本确认启动的秒数（默认 60）。
    #[serde(default)]
    pub(crate) confirm_timeout_secs: Option<u32>,
    /// 可选：HTTP 代理（如 `http://127.0.0.1:7890`），用于企业网络。
    #[serde(default)]
    pub(crate) proxy: Option<String>,
    /// 可选：是否接受旧版 minisign 签名，默认 `false`。
    ///
    /// 本项目发布端产出 pre-hash 签名；只有第三方历史管线才需要设为 `true`。
    #[serde(default)]
    pub(crate) allow_legacy_signatures: Option<bool>,
    /// 可选：主端点失败后何时切换备用端点（`transport` 默认 / `any`）。
    #[serde(default)]
    pub(crate) fallback_policy: Option<String>,
    /// 可选：附加到所有请求的 HTTP 头（私有 CDN Token、定制 UA）。
    #[serde(default)]
    pub(crate) headers: Option<HashMap<String, String>>,
    /// 可选：Windows 安装器模式（`passive` / `basicUi` / `quiet`）。
    #[serde(default)]
    pub(crate) install_mode: Option<String>,
    /// 可选：全量安装器类型。用 Inno Setup 打包时必须设为 `inno`；
    /// `nsis` / `msi` 可省略（按文件后缀推断）。
    #[serde(default)]
    pub(crate) installer_kind: Option<String>,
    /// 可选：Windows 安装器额外命令行参数，追加在固定静默/重启参数之后。
    ///
    /// 官方配置的 `windows.installerArgs` 会自动迁移到本字段。
    #[serde(default)]
    pub(crate) installer_args: Option<Vec<String>>,
    /// 可选：是否允许明文 HTTP 端点，默认 `false`（仅本地开发/内网测试可开启）。
    #[serde(default)]
    pub(crate) allow_insecure_transport: Option<bool>,
    /// 可选：下次启动时自动执行 `prepare` 落盘的待安装记录，默认 `false`。
    #[serde(default)]
    pub(crate) install_on_next_launch: Option<bool>,
    /// 可选：回滚后是否弹出系统提示，默认 `true`。
    #[serde(default)]
    pub(crate) rollback_notice: Option<bool>,
    /// 未识别的字段：保留但不生效，启动时写日志提示，避免拼写错误静默失效。
    #[serde(flatten)]
    pub(crate) extra: HashMap<String, serde_json::Value>,
}

/// 官方 `tauri-plugin-updater` 的字段：本插件有意忽略，不应报警告。
///
/// `windows` 是官方嵌套配置的载体；`windows.installMode` /
/// `windows.installerArgs` 会被自动迁移到顶层字段。顶层的 `installerArgs`
/// 不是官方写法，会按未知字段提示改用 `installer_args`。
const OFFICIAL_KEYS: &[&str] = &[
    "windows",
    "linux",
    "darwin",
    "dmg",
    "nsis",
    "appimage",
    "dangerousInsecureTransportProtocol",
];

impl FileConfig {
    /// 未知字段的启动警告（已排除官方 updater 的合法字段）。
    ///
    /// 官方字段本身不报错；`dangerousInsecureTransportProtocol` 的语义是
    /// “允许明文传输”，不能静默继承，因此单独提示迁移时必须显式确认。
    /// 排序保证日志稳定，便于测试与 diff。
    pub(crate) fn unknown_field_warnings(&self) -> Vec<String> {
        let mut keys: Vec<&str> = self
            .extra
            .keys()
            .map(String::as_str)
            .filter(|key| !OFFICIAL_KEYS.contains(key))
            .collect();
        keys.sort_unstable();
        let mut warnings: Vec<String> = keys
            .into_iter()
            .map(|key| {
                format!(
                    "plugins.delta-updater 含未知字段 {key:?}：该配置不会生效（检查拼写，字段表见 README）"
                )
            })
            .collect();

        // 官方 dangerousInsecureTransportProtocol → 本插件 allow_insecure_transport。
        // 只在端点确实使用明文 HTTP 时提示：HTTPS 部署不受影响，
        // 不应因为迁移时原样保留了该字段就产生噪音。
        let uses_http = self
            .endpoints
            .iter()
            .any(|url| url.trim().to_ascii_lowercase().starts_with("http://"));
        let insecure = self
            .extra
            .get("dangerousInsecureTransportProtocol")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if insecure && self.allow_insecure_transport.is_none() && uses_http {
            warnings.push(
                "plugins.delta-updater.dangerousInsecureTransportProtocol 不会生效：请显式设置 allow_insecure_transport: true".into(),
            );
        }

        warnings
    }

    /// 转成库层配置。
    pub(crate) fn into_update_config(self, current_version: &str) -> Result<UpdateConfig, String> {
        // 官方 windows.installMode / windows.installerArgs 与本插件语义一致，
        // 直接迁移；顶层同名字段优先。类型错误在启动阶段报错，避免安装器
        // 收到意料之外的参数。
        let official_windows = self
            .extra
            .get("windows")
            .and_then(serde_json::Value::as_object);
        let official_install_mode = match official_windows.and_then(|w| w.get("installMode")) {
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| {
                        "plugins.delta-updater.windows.installMode 必须是字符串".to_string()
                    })?
                    .to_string(),
            ),
            None => None,
        };
        let official_installer_args = match official_windows.and_then(|w| w.get("installerArgs")) {
            Some(value) => {
                let array = value.as_array().ok_or_else(|| {
                    "plugins.delta-updater.windows.installerArgs 必须是字符串数组".to_string()
                })?;
                Some(
                    array
                        .iter()
                        .map(|item| {
                            item.as_str().map(str::to_string).ok_or_else(|| {
                                "plugins.delta-updater.windows.installerArgs 必须是字符串数组"
                                    .to_string()
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                )
            }
            None => None,
        };
        let install_mode = self.install_mode.or(official_install_mode);
        let installer_args = self.installer_args.or(official_installer_args);

        let mut endpoints = self.endpoints.into_iter();
        let primary = endpoints
            .next()
            .ok_or_else(|| "plugins.delta-updater.endpoints 不能为空".to_string())?;
        let bundle_type = self.bundle_type.as_deref().filter(|s| !s.trim().is_empty());
        let channel = match self.channel.as_deref().map(str::trim) {
            Some(channel) if !channel.is_empty() => {
                let channel = channel.to_ascii_lowercase();
                if !channel
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
                {
                    return Err(format!(
                        "plugins.delta-updater.channel 非法: {channel}（只允许小写字母、数字、- 与 _）"
                    ));
                }
                channel
            }
            _ => "stable".to_string(),
        };

        let primary = expand_endpoint(&primary, current_version, bundle_type, &channel)?;
        let fallbacks = endpoints
            .map(|url| expand_endpoint(&url, current_version, bundle_type, &channel))
            .collect::<Result<Vec<_>, _>>()?;
        let mut config = UpdateConfig::new(primary, self.pubkey).current_version(current_version);
        if let Some(url) = self.signature_url.filter(|url| !url.trim().is_empty()) {
            config = config.signature_url(expand_endpoint(
                url.trim(),
                current_version,
                bundle_type,
                &channel,
            )?);
        }
        if let Some(keys) = self.additional_pubkeys {
            config = config.additional_public_keys(keys);
        }
        if let Some(policy) = self.trust_policy {
            let policy = match policy.trim().to_ascii_lowercase().as_str() {
                "any" | "any_of" => TrustPolicy::AnyOf,
                "all" | "all_of" => TrustPolicy::AllOf,
                other => {
                    return Err(format!(
                        "plugins.delta-updater.trust_policy 非法: {other}（any / all）"
                    ))
                }
            };
            config = config.trust_policy(policy);
        }
        if !fallbacks.is_empty() {
            config = config.fallback_endpoints(fallbacks);
        }
        if let Some(id) = self.install_id.filter(|id| !id.trim().is_empty()) {
            config = config.install_id(id);
        }
        if let Some(dir) = self.work_dir.filter(|dir| !dir.trim().is_empty()) {
            config = config.work_dir(dir.trim());
        }
        if let Some(secs) = self.wait_timeout_secs {
            config = config.wait_timeout_secs(secs);
        }
        if let Some(secs) = self.confirm_timeout_secs {
            config = config.confirm_timeout_secs(secs);
        }
        if let Some(policy) = self.fallback_policy {
            let policy = match policy.trim().to_ascii_lowercase().as_str() {
                "transport" | "on_transport_error" => FallbackPolicy::OnTransportError,
                "any" | "on_any_failure" => FallbackPolicy::OnAnyFailure,
                other => {
                    return Err(format!(
                        "plugins.delta-updater.fallback_policy 非法: {other}（transport / any）"
                    ))
                }
            };
            config = config.fallback_policy(policy);
        }
        if let Some(mode) = install_mode {
            let mode = match mode.trim().to_ascii_lowercase().as_str() {
                "passive" => InstallMode::Passive,
                "basicui" | "basic_ui" | "basic" => InstallMode::BasicUi,
                "quiet" | "silent" => InstallMode::Quiet,
                other => {
                    return Err(format!(
                    "plugins.delta-updater.install_mode 非法: {other}（passive / basicUi / quiet）"
                ))
                }
            };
            config = config.install_mode(mode);
        }
        if let Some(kind) = self.installer_kind {
            let kind = match kind.trim().to_ascii_lowercase().as_str() {
                "inno" => InstallerKind::Inno,
                "nsis" => InstallerKind::Nsis,
                "msi" => InstallerKind::Msi,
                other => {
                    return Err(format!(
                        "plugins.delta-updater.installer_kind 非法: {other}（inno / nsis / msi）"
                    ))
                }
            };
            config = config.installer_kind(kind);
        }
        if let Some(args) = installer_args {
            config = config.installer_args(args);
        }
        if let Some(allow) = self.allow_insecure_transport {
            config = config.allow_insecure_transport(allow);
        }
        if let Some(auto) = self.install_on_next_launch {
            config = config.install_on_next_launch(auto);
        }
        if let Some(notice) = self.rollback_notice {
            config = config.rollback_notice(notice);
        }
        if let Some(allow) = self.allow_legacy_signatures {
            config = config.allow_legacy_signatures(allow);
        }

        // 代理与附加请求头都需要自定义客户端；两者合并构建，
        // 避免出现"只生效了一个"的静默问题。
        let proxy = self.proxy.filter(|proxy| !proxy.trim().is_empty());
        let headers = self.headers.unwrap_or_default();
        if proxy.is_some() || !headers.is_empty() {
            let mut builder = tauri_delta_updater::default_client_builder();
            if let Some(proxy) = proxy {
                let proxy = reqwest::Proxy::all(proxy.trim())
                    .map_err(|err| format!("plugins.delta-updater.proxy 非法: {err}"))?;
                builder = builder.proxy(proxy);
            }
            if !headers.is_empty() {
                let mut map = HeaderMap::new();
                for (name, value) in headers {
                    let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
                        format!("plugins.delta-updater.headers 名称非法 {name:?}: {err}")
                    })?;
                    let header_value = HeaderValue::from_str(&value).map_err(|err| {
                        format!("plugins.delta-updater.headers 值非法 {value:?}: {err}")
                    })?;
                    map.insert(header_name, header_value);
                }
                builder = builder.default_headers(map);
            }
            let client = builder
                .build()
                .map_err(|err| format!("构建 HTTP 客户端失败: {err}"))?;
            config = config.client(client);
        }
        if let Some(bundle) = self.bundle_endpoint {
            config = config.bundle_manifest_url(expand_endpoint(
                &bundle,
                current_version,
                bundle_type,
                &channel,
            )?);
        }
        Ok(config)
    }
}

/// 展开官方 updater 支持的端点占位符。
///
/// 支持 `{{target}}`（windows / darwin / linux）、`{{arch}}`（x86_64 / aarch64）、
/// `{{current_version}}`、`{{channel}}`（默认 `stable`）与 `{{bundle_type}}`
/// （需配置 `bundle_type`）。用了 `{{bundle_type}}` 却没配置时返回明确错误，
/// 避免请求带着字面量花括号发出。
fn expand_endpoint(
    url: &str,
    current_version: &str,
    bundle_type: Option<&str>,
    channel: &str,
) -> Result<String, String> {
    if url.contains("{{bundle_type}}") && bundle_type.is_none() {
        return Err(
            "endpoints 使用了 {{bundle_type}}，请在 plugins.delta-updater.bundle_type \
             指定安装包类型（nsis / msi / appimage / app）"
                .into(),
        );
    }
    let platform = tauri_delta_updater::current_platform();
    expand_with_platform(url, platform, current_version, bundle_type, channel)
}

/// 展开占位符，平台作为参数传入以便测试 unknown 组合。
fn expand_with_platform(
    url: &str,
    platform: &str,
    current_version: &str,
    bundle_type: Option<&str>,
    channel: &str,
) -> Result<String, String> {
    let needs_platform = url.contains("{{target}}") || url.contains("{{arch}}");
    if needs_platform && platform.contains("unknown") {
        return Err(format!(
            "无法识别当前平台（{platform}），不能展开 {{{{target}}}} / {{{{arch}}}}；\
             请在 endpoints 中直接写具体平台路径"
        ));
    }
    let (target, arch) = platform.split_once('-').unwrap_or((platform, ""));
    let mut expanded = url
        .replace("{{target}}", target)
        .replace("{{arch}}", arch)
        .replace("{{current_version}}", current_version)
        .replace("{{channel}}", channel);
    if let Some(kind) = bundle_type {
        expanded = expanded.replace("{{bundle_type}}", kind);
    }
    Ok(expanded)
}

/// 从应用配置读取并构造更新器配置。
///
/// 同时返回未知字段的警告：由启动流程写入日志并发事件，
/// 避免 `bundle_endpont` 这类拼写错误静默失效。
pub(crate) fn read_file_config<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<(UpdateConfig, Vec<String>), Box<dyn std::error::Error>> {
    let plugins = &app.config().plugins.0;
    let raw = plugins
        .get("delta-updater")
        .or_else(|| plugins.get("updater"))
        .ok_or_else(|| {
            std::io::Error::other(
                "tauri.conf.json 缺少 plugins.delta-updater（或 plugins.updater）配置",
            )
        })?;
    let file: FileConfig = serde_json::from_value(raw.clone())
        .map_err(|e| std::io::Error::other(format!("plugins.delta-updater 配置非法: {e}")))?;
    let warnings = file.unknown_field_warnings();
    let version = app.package_info().version.to_string();
    let config = file
        .into_update_config(&version)
        .map_err(std::io::Error::other)?;
    Ok((config, warnings))
}

#[cfg(test)]
mod tests {
    use super::expand_with_platform;

    #[test]
    fn expands_known_platform_placeholders() {
        let url = expand_with_platform(
            "https://cdn/{{target}}/{{arch}}/{{channel}}/{{current_version}}.json",
            "windows-x86_64",
            "1.2.3",
            None,
            "beta",
        )
        .unwrap();
        assert_eq!(url, "https://cdn/windows/x86_64/beta/1.2.3.json");
    }

    /// `{{target}}` / `{{arch}}` 在未知平台上必须报错，而不是展开成 `unknown`。
    /// 没有用到平台占位符时，未知平台不影响更新。
    #[test]
    fn rejects_unknown_platform_only_when_placeholder_is_used() {
        assert!(expand_with_platform(
            "https://cdn/latest.json",
            "unknown-unknown",
            "1.0.0",
            None,
            "stable"
        )
        .is_ok());

        let err = expand_with_platform(
            "https://cdn/{{target}}.json",
            "unknown-unknown",
            "1.0.0",
            None,
            "stable",
        )
        .unwrap_err();
        assert!(err.contains("无法识别当前平台"), "{err}");
    }
}
