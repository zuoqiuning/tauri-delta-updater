//! 插件注册与 `tauri.conf.json` 配置读取。

use tauri::{AppHandle, Runtime};
use tauri_delta_updater::UpdateConfig;

/// `tauri.conf.json` 里 `plugins.delta-updater` 的配置。
///
/// 字段与官方 updater 的配置兼容：`endpoints` + `pubkey` 可以直接复用，
/// 迁移时不必改配置。官方配置里的 `windows` / `dmg` 等字段会被忽略。
#[derive(Debug, serde::Deserialize)]
pub(crate) struct FileConfig {
    /// 清单地址。与官方一致用数组；一个插件实例只消费第一个。
    pub(crate) endpoints: Vec<String>,
    /// 固化在应用里的公钥（base64 或 `.pub` 文件内容）。
    pub(crate) pubkey: String,
    /// 可选：整包清单地址（`latest.bundle.json`）。
    #[serde(default)]
    pub(crate) bundle_endpoint: Option<String>,
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
    /// 可选：是否接受旧版 minisign 签名，默认 `true`。
    ///
    /// 本项目发布端产出 pre-hash 签名；设为 `false` 可拒绝第三方旧格式。
    #[serde(default)]
    pub(crate) allow_legacy_signatures: Option<bool>,
}

impl FileConfig {
    /// 转成库层配置。
    pub(crate) fn into_update_config(self, current_version: &str) -> Result<UpdateConfig, String> {
        let endpoint = self
            .endpoints
            .first()
            .ok_or_else(|| "plugins.delta-updater.endpoints 不能为空".to_string())?
            .clone();
        let bundle_type = self.bundle_type.as_deref().filter(|s| !s.trim().is_empty());

        let endpoint = expand_endpoint(&endpoint, current_version, bundle_type)?;
        let mut config = UpdateConfig::new(endpoint, self.pubkey).current_version(current_version);
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
        if let Some(proxy) = self.proxy.filter(|proxy| !proxy.trim().is_empty()) {
            let proxy = reqwest::Proxy::all(proxy.trim())
                .map_err(|err| format!("plugins.delta-updater.proxy 非法: {err}"))?;
            let client = tauri_delta_updater::client_builder()
                .proxy(proxy)
                .build()
                .map_err(|err| format!("构建 HTTP 客户端失败: {err}"))?;
            config = config.client(client);
        }
        if let Some(allow) = self.allow_legacy_signatures {
            config = config.allow_legacy_signatures(allow);
        }
        if let Some(bundle) = self.bundle_endpoint {
            config =
                config.bundle_manifest_url(expand_endpoint(&bundle, current_version, bundle_type)?);
        }
        Ok(config)
    }
}

/// 展开官方 updater 支持的端点占位符。
///
/// 支持 `{{target}}`（windows / darwin / linux）、`{{arch}}`（x86_64 / aarch64）、
/// `{{current_version}}` 与 `{{bundle_type}}`（需配置 `bundle_type`）。
/// 用了 `{{bundle_type}}` 却没配置时返回明确错误，避免请求带着字面量花括号发出。
fn expand_endpoint(
    url: &str,
    current_version: &str,
    bundle_type: Option<&str>,
) -> Result<String, String> {
    if url.contains("{{bundle_type}}") && bundle_type.is_none() {
        return Err(
            "endpoints 使用了 {{bundle_type}}，请在 plugins.delta-updater.bundle_type \
             指定安装包类型（nsis / msi / appimage / app）"
                .into(),
        );
    }
    let platform = tauri_delta_updater::current_platform();
    let (target, arch) = platform.split_once('-').unwrap_or((platform, ""));
    let mut expanded = url
        .replace("{{target}}", target)
        .replace("{{arch}}", arch)
        .replace("{{current_version}}", current_version);
    if let Some(kind) = bundle_type {
        expanded = expanded.replace("{{bundle_type}}", kind);
    }
    Ok(expanded)
}

/// 从应用配置读取并构造更新器配置。
pub(crate) fn read_file_config<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<UpdateConfig, Box<dyn std::error::Error>> {
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
    let version = app.package_info().version.to_string();
    let config = file
        .into_update_config(&version)
        .map_err(std::io::Error::other)?;
    Ok(config)
}
