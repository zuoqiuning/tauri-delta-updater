//! 项目文件定位、配置改写与原子写入。

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// 插件依赖名（与 `plugin/Cargo.toml` 的 package 名一致）。
const PLUGIN_DEP: &str = "tauri-plugin-delta-updater";

/// 一次待写入的文件变更（dry-run 时只打印 diff）。
pub(crate) struct Change {
    pub(crate) path: PathBuf,
    pub(crate) before: String,
    pub(crate) after: String,
}

impl Change {
    pub(crate) fn label(&self) -> String {
        self.path.display().to_string()
    }
}

pub(crate) struct Project {
    pub(crate) config_path: PathBuf,
    pub(crate) capability_path: PathBuf,
    pub(crate) cargo_toml: PathBuf,
}

impl Project {
    pub(crate) fn discover(root: Option<&Path>) -> Result<Self, String> {
        let root = match root {
            Some(root) => root.to_path_buf(),
            None => std::env::current_dir().map_err(|e| format!("无法读取当前目录: {e}"))?,
        };
        let tauri_dir = if root.join("src-tauri/tauri.conf.json").is_file() {
            root.join("src-tauri")
        } else if root.join("tauri.conf.json").is_file() {
            root
        } else {
            return Err(format!(
                "在 {} 找不到 src-tauri/tauri.conf.json；请在 Tauri 项目根目录运行",
                root.display()
            ));
        };

        let config_path = tauri_dir.join("tauri.conf.json");
        // Tauri 2 允许自定义 capability 文件名（main.json / desktop.json 等）。
        // 优先 default.json；否则复用唯一的已有文件；多个时优先 identifier=default。
        let capability_path = pick_capability(&tauri_dir.join("capabilities"));
        let cargo_toml = tauri_dir.join("Cargo.toml");
        if !cargo_toml.is_file() {
            return Err(format!(
                "找不到 {}；请在 Tauri 项目根目录运行",
                cargo_toml.display()
            ));
        }
        Ok(Self {
            config_path,
            capability_path,
            cargo_toml,
        })
    }

    pub(crate) fn init_changes(
        &self,
        endpoint: &str,
        pubkey: &str,
        bundle_endpoint: Option<&str>,
        force: bool,
        registry: bool,
    ) -> Result<Vec<Change>, String> {
        let with_bundle = self.config_with_delta(None, endpoint, pubkey, bundle_endpoint, force)?;
        let capability = self.capability_with_delta(true, false)?;
        let cargo = self.cargo_with_delta(registry)?;
        Ok(vec![with_bundle, capability, cargo]
            .into_iter()
            .filter(|change| change.before != change.after)
            .collect())
    }

    /// 检查三项接入是否完整；返回缺失项（空表示已完整）。
    pub(crate) fn init_status(&self) -> Result<Vec<String>, String> {
        let mut missing = Vec::new();

        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        // 插件在缺失 plugins.delta-updater 时会回退读取 plugins.updater；
        // --check 必须承认这条迁移路径，否则“保留官方配置”会被误判为未接入。
        let delta = root
            .pointer("/plugins/delta-updater")
            .or_else(|| root.pointer("/plugins/updater"))
            .and_then(Value::as_object);
        let config_ok = delta.is_some_and(|delta| {
            delta
                .get("endpoints")
                .and_then(Value::as_array)
                .is_some_and(|endpoints| !endpoints.is_empty())
                && delta
                    .get("pubkey")
                    .and_then(Value::as_str)
                    .is_some_and(|key| !key.trim().is_empty())
        });
        if !config_ok {
            missing
                .push("plugins.delta-updater / plugins.updater（endpoints + pubkey）".to_string());
        } else if let Some(delta) = delta {
            // 配置存在不代表可用：错误的 scheme、明文 HTTP 或占位符拼错
            // 都会在应用启动/首次检查时才暴露，CI 守卫应在这里拦下。
            let allow_insecure = delta
                .get("allow_insecure_transport")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(endpoints) = delta.get("endpoints").and_then(Value::as_array) {
                for (index, endpoint) in endpoints.iter().enumerate() {
                    match endpoint.as_str() {
                        Some(url) => {
                            if let Some(issue) = endpoint_issue(url, allow_insecure) {
                                missing.push(format!("endpoints[{index}]: {issue}"));
                            }
                        }
                        None => missing.push(format!("endpoints[{index}] 不是字符串")),
                    }
                }
            }
            if let Some(pubkey) = delta.get("pubkey").and_then(Value::as_str) {
                if let Some(issue) = pubkey_issue(pubkey) {
                    missing.push(format!("pubkey: {issue}"));
                }
            }
            missing.extend(config_value_issues(delta, allow_insecure));
        }

        let (_, capability) = read_json_or(&self.capability_path, json!({}))?;
        let engine_installer = self.uses_engine_installer()?;
        // Tauri 2 的 capability 必须有非空 identifier；缺它时 `tauri build`
        // 会在反序列化阶段失败，而不是权限不生效。
        let capability_ok = capability
            .get("identifier")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
            && capability
                .get("permissions")
                .and_then(Value::as_array)
                .is_some_and(|permissions| {
                    let has = |name: &str| permissions.iter().any(|v| v.as_str() == Some(name));
                    has("delta-updater:default") && (!engine_installer || has("delta-updater:full"))
                });
        if !capability_ok {
            // 报错必须点名实际使用的文件：多 capability 项目里写死
            // default.json 会把用户引到错误的文件。
            let label = self
                .config_path
                .parent()
                .and_then(|base| self.capability_path.strip_prefix(base).ok())
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| self.capability_path.display().to_string())
                .replace('\\', "/");
            let required = if engine_installer {
                "identifier + delta-updater:default + delta-updater:full"
            } else {
                "identifier + delta-updater:default"
            };
            missing.push(format!("{label}（{required}）"));
        }

        let cargo = fs::read_to_string(&self.cargo_toml)
            .map_err(|e| format!("读取 {} 失败: {e}", self.cargo_toml.display()))?;
        if let Some(stale) = dependency_is_stale(&cargo) {
            missing.push(format!(
                "src-tauri/Cargo.toml 的 tauri-plugin-delta-updater 版本 {stale:?} 已落后（当前工具 {}，建议写 \"{}\"）",
                env!("CARGO_PKG_VERSION"),
                minor_requirement()
            ));
        } else if !has_dependency(&cargo) {
            missing.push("src-tauri/Cargo.toml 的 tauri-plugin-delta-updater".to_string());
        }

        Ok(missing)
    }

    /// 选中的 capability 文件不是 `default.json` 时返回文件名，供 CLI 提示。
    pub(crate) fn non_default_capability(&self) -> Option<String> {
        let name = self.capability_path.file_name()?.to_str()?;
        (name != "default.json").then(|| name.to_string())
    }

    /// 是否保留了官方 updater 配置（双栈模式或作为配置回退来源）。
    pub(crate) fn uses_official_updater(&self) -> Result<bool, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        Ok(root.pointer("/plugins/updater").is_some())
    }

    /// 是否已写入本插件专用配置键（而非回退使用 `plugins.updater`）。
    pub(crate) fn has_delta_config(&self) -> Result<bool, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        Ok(root.pointer("/plugins/delta-updater").is_some())
    }

    /// 项目里是否存在 Inno Setup 脚本（`*.iss`）。
    ///
    /// Inno 与 NSIS 的安装包同为 `.exe`；漏配 `installer_kind` 时只能在
    /// 安装阶段静默按 NSIS 参数调用。接入时就提示比运行时踩坑便宜得多。
    pub(crate) fn looks_like_inno_setup(&self) -> bool {
        let tauri_dir = self.config_path.parent().unwrap_or(Path::new("."));
        let root = if tauri_dir.file_name().and_then(|name| name.to_str()) == Some("src-tauri") {
            tauri_dir.parent().unwrap_or(tauri_dir)
        } else {
            tauri_dir
        };
        walkdir::WalkDir::new(root)
            .max_depth(3)
            .into_iter()
            .filter_entry(|entry| {
                !matches!(
                    entry.file_name().to_str(),
                    Some("target" | "node_modules" | ".git")
                )
            })
            .filter_map(Result::ok)
            .any(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("iss"))
            })
    }

    /// 配置里是否已显式声明安装器类型。
    pub(crate) fn installer_kind_configured(&self) -> Result<bool, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        Ok(root
            .pointer("/plugins/delta-updater")
            .or_else(|| root.pointer("/plugins/updater"))
            .and_then(|config| config.get("installer_kind"))
            .is_some())
    }

    /// 是否配置了本引擎的全量安装器（而不是把全量路径交给官方 updater）。
    ///
    /// `install_mode` / `installer_kind` / `installer_args`（含官方
    /// `windows.installMode` / `windows.installerArgs`）只被本引擎的
    /// `install_full` 消费：出现任一字段说明前端需要 `delta-updater:full`。
    pub(crate) fn uses_engine_installer(&self) -> Result<bool, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        let Some(config) = root
            .pointer("/plugins/delta-updater")
            .or_else(|| root.pointer("/plugins/updater"))
            .and_then(Value::as_object)
        else {
            return Ok(false);
        };
        Ok(["install_mode", "installer_kind", "installer_args"]
            .iter()
            .any(|key| config.contains_key(*key))
            || config
                .get("windows")
                .and_then(Value::as_object)
                .is_some_and(|windows| {
                    windows.contains_key("installMode") || windows.contains_key("installerArgs")
                }))
    }

    /// 依赖是否仍是 git 形式；插件发布后应改为版本依赖。
    pub(crate) fn dependency_is_git(&self) -> Result<bool, String> {
        let cargo = fs::read_to_string(&self.cargo_toml)
            .map_err(|e| format!("读取 {} 失败: {e}", self.cargo_toml.display()))?;
        Ok(cargo.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with(PLUGIN_DEP) && trimmed.contains("git =")
        }))
    }

    pub(crate) fn migrate_changes(
        &self,
        remove_official: bool,
        force: bool,
    ) -> Result<Vec<Change>, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        let updater = root
            .get("plugins")
            .and_then(|plugins| plugins.get("updater"))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                "tauri.conf.json 里没有 plugins.updater，无法迁移；先配置官方 updater".to_string()
            })?;
        let endpoints = updater
            .get("endpoints")
            .cloned()
            .ok_or_else(|| "plugins.updater 缺少 endpoints".to_string())?;
        let pubkey = updater
            .get("pubkey")
            .and_then(Value::as_str)
            .ok_or_else(|| "plugins.updater 缺少 pubkey".to_string())?;
        let bundle = updater.get("bundle_endpoint").and_then(Value::as_str);

        let mut delta = Map::new();
        delta.insert("endpoints".into(), endpoints);
        delta.insert("pubkey".into(), json!(pubkey));
        if let Some(bundle) = bundle {
            delta.insert("bundle_endpoint".into(), json!(bundle));
        }
        let config = self.config_with_delta(Some(delta), "", "", None, force)?;
        let mut config = config;
        if remove_official {
            config.after = remove_plugin(&config.after, "updater")?;
        }
        let capability = self.capability_with_delta(true, remove_official)?;
        let cargo = self.cargo_with_delta(false)?;
        Ok([config, capability, cargo]
            .into_iter()
            .filter(|change| change.before != change.after)
            .collect())
    }

    fn config_with_delta(
        &self,
        delta: Option<Map<String, Value>>,
        endpoint: &str,
        pubkey: &str,
        bundle_endpoint: Option<&str>,
        force: bool,
    ) -> Result<Change, String> {
        let (before, mut root) = read_json_or(&self.config_path, json!({}))?;
        let plugins = root
            .as_object_mut()
            .ok_or_else(|| "tauri.conf.json 根节点必须是对象".to_string())?
            .entry("plugins")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| "tauri.conf.json 的 plugins 必须是对象".to_string())?;

        if plugins.contains_key("delta-updater") && !force {
            return Err("plugins.delta-updater 已存在；确认要覆盖时加 --force".into());
        }

        let delta = match delta {
            Some(delta) => delta,
            None => {
                let mut delta = Map::new();
                delta.insert("endpoints".into(), json!([endpoint]));
                delta.insert("pubkey".into(), json!(pubkey));
                if let Some(bundle) = bundle_endpoint {
                    delta.insert("bundle_endpoint".into(), json!(bundle));
                }
                delta
            }
        };
        plugins.insert("delta-updater".into(), Value::Object(delta));

        Ok(Change {
            path: self.config_path.clone(),
            before,
            after: render_json(&root),
        })
    }

    fn capability_with_delta(
        &self,
        add_delta: bool,
        remove_official: bool,
    ) -> Result<Change, String> {
        let (before, mut root) = read_json_or(&self.capability_path, default_capability())?;
        let object = root
            .as_object_mut()
            .ok_or_else(|| "capability 根节点必须是对象".to_string())?;
        // 已有文件也补齐 identifier：它是 Tauri 2 Capability 的必填字段，
        // 缺字段时 `tauri build` 直接失败；`windows` 只在新建文件时给出默认值，
        // 避免覆盖用户已有的 platform/webview 作用域配置。
        object
            .entry("identifier")
            .or_insert_with(|| json!("default"));
        let permissions = object
            .entry("permissions")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| "capability 的 permissions 必须是数组".to_string())?;

        if add_delta
            && !permissions
                .iter()
                .any(|v| v.as_str() == Some("delta-updater:default"))
        {
            permissions.push(json!("delta-updater:default"));
        }
        // 全量安装器只被本引擎消费：配置了相关字段时一并授予 `full` 集，
        // 否则前端调用 install_full / download_full 会被 capability 拦住。
        if add_delta
            && self.uses_engine_installer()?
            && !permissions
                .iter()
                .any(|v| v.as_str() == Some("delta-updater:full"))
        {
            permissions.push(json!("delta-updater:full"));
        }
        if remove_official {
            permissions.retain(|v| v.as_str() != Some("updater:default"));
        }

        Ok(Change {
            path: self.capability_path.clone(),
            before,
            after: render_json(&root),
        })
    }

    fn cargo_with_delta(&self, registry: bool) -> Result<Change, String> {
        let before = fs::read_to_string(&self.cargo_toml)
            .map_err(|e| format!("读取 {} 失败: {e}", self.cargo_toml.display()))?;
        let after = ensure_dependency(&before, registry);
        Ok(Change {
            path: self.cargo_toml.clone(),
            before,
            after,
        })
    }
}

/// 新建 capability 文件时的合法默认值。
///
/// Tauri 2 的 Capability 要求非空 `identifier`，`windows` 决定作用窗口；
/// 只写 `permissions` 会生成一个 `tauri build` 无法反序列化的文件。
fn default_capability() -> Value {
    json!({
        "$schema": "../gen/schemas/desktop-schema.json",
        "identifier": "default",
        "windows": ["main"],
        "permissions": []
    })
}

/// 选择要写入的 capability 文件。
///
/// 优先级：`default.json` → 唯一已有 JSON → identifier 为 `default` 的 JSON →
/// 新建 `default.json`。多 capability 项目写入其中一个时，调用方会打印提示，
/// 由用户确认该文件覆盖目标窗口。
fn pick_capability(dir: &Path) -> PathBuf {
    let default = dir.join("default.json");
    if default.is_file() {
        return default;
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    candidates.sort();
    if candidates.len() == 1 {
        return candidates.remove(0);
    }
    for path in &candidates {
        let identified_default = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| {
                value
                    .get("identifier")
                    .and_then(Value::as_str)
                    .map(|id| id == "default")
            })
            .unwrap_or(false);
        if identified_default {
            return path.clone();
        }
    }
    candidates.into_iter().next().unwrap_or(default)
}

/// 检查端点 URL 是否明显不可用；返回给用户看的具体问题。
///
/// 只做静态检查（scheme 与明文 HTTP 策略），不发请求：
/// `--check` 必须是可在离线 CI 中运行的守卫。
fn endpoint_issue(url: &str, allow_insecure: bool) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Some("地址为空".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return None;
    }
    if trimmed.contains("{{") {
        // 运行时由插件按平台/渠道展开，静态阶段无法评估明文策略。
        return None;
    }
    if lower.starts_with("http://") {
        if allow_insecure {
            return None;
        }
        let loopback = reqwest::Url::parse(trimmed)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string));
        return match loopback.as_deref() {
            Some("localhost" | "127.0.0.1" | "::1" | "[::1]") => None,
            _ => Some(format!(
                "明文 HTTP 且非回环地址（{trimmed}）；改用 HTTPS 或显式设置 allow_insecure_transport"
            )),
        };
    }
    Some(format!("不是 http(s) 地址: {trimmed}"))
}

/// 公钥格式是否可用；返回给用户看的具体问题。
fn pubkey_issue(input: &str) -> Option<String> {
    tauri_delta_updater::validate_public_key(input)
        .err()
        .map(|err| err.to_string())
}

/// 配置字段的类型、枚举与占位符依赖校验。
///
/// 接受范围与插件启动时的反序列化保持一致；这里只做保守校验，
/// 目的是让 CI 在接入阶段就发现写错的值，而不是等应用启动后
/// 变成“更新器不可用”。
fn config_value_issues(delta: &Map<String, Value>, allow_insecure: bool) -> Vec<String> {
    let mut issues = Vec::new();

    for key in ["work_dir", "install_id"] {
        match delta.get(key) {
            None => {}
            Some(Value::String(text)) if text.trim().is_empty() => {
                issues.push(format!("{key} 不能为空"))
            }
            Some(Value::String(_)) => {}
            Some(_) => issues.push(format!("{key} 必须是字符串")),
        }
    }

    for (key, what) in [
        ("signature_url", "签名地址"),
        ("bundle_endpoint", "整包清单地址"),
    ] {
        match delta.get(key) {
            None => {}
            Some(Value::String(url)) => {
                if let Some(issue) = endpoint_issue(url, allow_insecure) {
                    issues.push(format!("{key}: {issue}"));
                }
            }
            Some(_) => issues.push(format!("{key} 必须是字符串（{what}）")),
        }
    }

    match delta.get("additional_pubkeys") {
        None => {}
        Some(Value::Array(keys)) => {
            for (index, key) in keys.iter().enumerate() {
                match key.as_str() {
                    Some(key) => {
                        if let Some(issue) = pubkey_issue(key) {
                            issues.push(format!("additional_pubkeys[{index}]: {issue}"));
                        }
                    }
                    None => issues.push(format!("additional_pubkeys[{index}] 不是字符串")),
                }
            }
        }
        Some(_) => issues.push("additional_pubkeys 必须是字符串数组".to_string()),
    }

    for key in [
        "allow_legacy_signatures",
        "allow_insecure_transport",
        "install_on_next_launch",
        "rollback_notice",
        "dangerousInsecureTransportProtocol",
    ] {
        if delta.get(key).is_some_and(|value| !value.is_boolean()) {
            issues.push(format!("{key} 必须是布尔值"));
        }
    }

    for key in ["wait_timeout_secs", "confirm_timeout_secs"] {
        match delta.get(key) {
            None => {}
            Some(value) => match value.as_u64() {
                Some(0) => issues.push(format!("{key} 必须大于 0")),
                Some(_) => {}
                None => issues.push(format!("{key} 必须是正整数")),
            },
        }
    }

    if let Some(value) = delta.get("headers") {
        match value.as_object() {
            Some(headers) => {
                for (name, value) in headers {
                    if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                        issues.push(format!("headers.{name} 不是合法的请求头名"));
                    }
                    if value.as_str().is_none() {
                        issues.push(format!("headers.{name} 必须是字符串"));
                    }
                }
            }
            None => issues.push("headers 必须是对象（字符串到字符串）".to_string()),
        }
    }

    if let Some(value) = delta.get("proxy") {
        match value.as_str() {
            Some(proxy) if !proxy.trim().is_empty() => {
                if let Err(err) = reqwest::Proxy::all(proxy.trim()) {
                    issues.push(format!("proxy 非法: {err}"));
                }
            }
            Some(_) => {}
            None => issues.push("proxy 必须是字符串".to_string()),
        }
    }

    if let Some(value) = delta.get("installer_args") {
        issues.extend(string_array_issues(value, "installer_args"));
    }

    check_enum(
        delta,
        "trust_policy",
        &["any", "any_of", "all", "all_of"],
        &mut issues,
    );
    check_enum(
        delta,
        "fallback_policy",
        &["transport", "on_transport_error", "any", "on_any_failure"],
        &mut issues,
    );
    check_enum(
        delta,
        "install_mode",
        &["passive", "basicui", "basic_ui", "basic", "quiet", "silent"],
        &mut issues,
    );
    check_enum(
        delta,
        "installer_kind",
        &["inno", "nsis", "msi"],
        &mut issues,
    );
    check_enum(
        delta,
        "authenticode_policy",
        &["strict", "permissive", "off", "none"],
        &mut issues,
    );
    check_enum(
        delta,
        "lock_scope",
        &["work_dir", "workdir", "install_dir", "installdir"],
        &mut issues,
    );

    if let Some(value) = delta.get("channel") {
        match value.as_str() {
            // 插件启动时会先转小写再校验，这里必须接受同样的写法。
            Some(channel) if channel.trim().is_empty() => {}
            Some(channel)
                if channel.trim().to_ascii_lowercase().chars().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
                }) => {}
            Some(channel) => issues.push(format!(
                "channel 非法: {channel:?}（只允许字母、数字、- 与 _）"
            )),
            None => issues.push("channel 必须是字符串".to_string()),
        }
    }

    match delta.get("bundle_type") {
        None => {}
        Some(Value::String(text)) if text.trim().is_empty() => {
            issues.push("bundle_type 不能为空".to_string())
        }
        Some(Value::String(_)) => {}
        Some(_) => issues.push("bundle_type 必须是字符串".to_string()),
    }

    // 官方 windows 嵌套字段：插件会自动迁移，类型错误同样应在 CI 拦下。
    if let Some(windows) = delta.get("windows") {
        match windows.as_object() {
            Some(windows) => {
                if let Some(value) = windows.get("installMode") {
                    match value.as_str() {
                        Some(mode) => {
                            let normalized = mode.trim().to_ascii_lowercase();
                            if !["passive", "basicui", "basic_ui", "basic", "quiet", "silent"]
                                .contains(&normalized.as_str())
                            {
                                issues.push(format!("windows.installMode 非法: {mode:?}"));
                            }
                        }
                        None => issues.push("windows.installMode 必须是字符串".to_string()),
                    }
                }
                if let Some(value) = windows.get("installerArgs") {
                    issues.extend(string_array_issues(value, "windows.installerArgs"));
                }
            }
            None => issues.push("windows 必须是对象".to_string()),
        }
    }

    // {{bundle_type}} 依赖显式的 bundle_type；否则运行时请求会带着花括号。
    let has_bundle_type = delta
        .get("bundle_type")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty());
    let mut urls: Vec<&str> = Vec::new();
    if let Some(endpoints) = delta.get("endpoints").and_then(Value::as_array) {
        urls.extend(endpoints.iter().filter_map(Value::as_str));
    }
    for key in ["signature_url", "bundle_endpoint"] {
        if let Some(url) = delta.get(key).and_then(Value::as_str) {
            urls.push(url);
        }
    }
    if !has_bundle_type && urls.iter().any(|url| url.contains("{{bundle_type}}")) {
        issues.push(
            "端点使用 {{bundle_type}}，但缺少 bundle_type 配置（nsis / msi / appimage / app）"
                .to_string(),
        );
    }

    issues
}

/// 枚举字段校验（大小写不敏感，空值不算缺失）。
fn check_enum(delta: &Map<String, Value>, key: &str, allowed: &[&str], issues: &mut Vec<String>) {
    let Some(value) = delta.get(key) else {
        return;
    };
    match value.as_str() {
        Some(text) if allowed.contains(&text.trim().to_ascii_lowercase().as_str()) => {}
        Some(text) => issues.push(format!(
            "{key} 非法: {text:?}（可选: {}）",
            allowed.join(" / ")
        )),
        None => issues.push(format!("{key} 必须是字符串")),
    }
}

/// 字符串数组校验（含 NUL 检查）。
fn string_array_issues(value: &Value, label: &str) -> Vec<String> {
    match value.as_array() {
        Some(items) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item.as_str() {
                Some(arg) if arg.contains('\0') => Some(format!("{label}[{index}] 不能包含 NUL")),
                Some(_) => None,
                None => Some(format!("{label}[{index}] 不是字符串")),
            })
            .collect(),
        None => vec![format!("{label} 必须是字符串数组")],
    }
}

fn read_json_or(path: &Path, default: Value) -> Result<(String, Value), String> {
    if !path.is_file() {
        return Ok((String::new(), default));
    }
    let text =
        fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    let value =
        serde_json::from_str(&text).map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;
    Ok((text, value))
}

fn render_json(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON 必须可序列化");
    text.push('\n');
    text
}

/// 删除 `plugins.<name>`，保留其余配置。
fn remove_plugin(config: &str, name: &str) -> Result<String, String> {
    let mut root: Value = serde_json::from_str(config).map_err(|e| e.to_string())?;
    if let Some(plugins) = root.get_mut("plugins").and_then(Value::as_object_mut) {
        plugins.remove(name);
    }
    Ok(render_json(&root))
}

/// 在 `[dependencies]` 下补齐插件依赖；已存在时不改动。
///
/// 默认写 `"0.4"`（允许同 minor 内 patch 升级）；`--registry` 写精确版本
/// `"0.4.0"`，用于必须与工具版本一致的宿主。
fn ensure_dependency(text: &str, registry: bool) -> String {
    if has_dependency(text) {
        return text.to_string();
    }

    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<&str> = text.lines().collect();
    let dependency = dependency_line(registry);
    let insert_at = lines
        .iter()
        .position(|line| line.trim() == "[dependencies]")
        .map(|index| index + 1);
    match insert_at {
        Some(index) => lines.insert(index, &dependency),
        None => {
            lines.push("");
            lines.push("[dependencies]");
            lines.push(&dependency);
        }
    }

    let mut rendered = lines.join(nl);
    if text.ends_with('\n') || text.is_empty() {
        rendered.push_str(nl);
    }
    rendered
}

/// 插件依赖声明。
///
/// 默认只约束到 minor（`"0.4"`），让宿主拿到同系列补丁；`registry` 写精确
/// 版本，适合要求"依赖版本必须等于发布工具版本"的流水线。
fn dependency_line(registry: bool) -> String {
    if registry {
        format!("{PLUGIN_DEP} = \"{}\"", env!("CARGO_PKG_VERSION"))
    } else {
        format!("{PLUGIN_DEP} = \"{}\"", minor_requirement())
    }
}

/// 从工具版本推导 minor 约束（`0.4.0` → `0.4`）。
fn minor_requirement() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        _ => version.to_string(),
    }
}

fn has_dependency(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(PLUGIN_DEP) {
            return false;
        }
        let rest = trimmed[PLUGIN_DEP.len()..].trim_start();
        // 覆盖 `= { ... }`、`= "1.2.3"` 与 `= { path = ... }` 三种写法
        rest.starts_with('=') && rest[1..].trim_start().starts_with(['{', '"', '\''])
    })
}

/// 依赖声明的版本要求。
enum DeclaredVersion {
    /// 显式的版本约束（可能带 `^`/`~`/`=` 前缀）。
    Requirement(String),
    /// `path` / `git` 等无法与发布版本比较的写法。
    Unverifiable,
}

/// 从 Cargo.toml 取出本插件依赖的版本要求。
fn declared_version(text: &str) -> Option<DeclaredVersion> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(PLUGIN_DEP) {
            continue;
        }
        let rest = trimmed[PLUGIN_DEP.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('{') {
            return Some(match inline_table_version(rest) {
                Some(version) => DeclaredVersion::Requirement(version),
                None => DeclaredVersion::Unverifiable,
            });
        }
        let quote = rest.chars().next()?;
        if !matches!(quote, '"' | '\'') {
            continue;
        }
        let end = rest[1..].find(quote)? + 1;
        return Some(DeclaredVersion::Requirement(rest[1..end].to_string()));
    }
    None
}

/// 从内联表 `{ version = "0.3", ... }` 中取出引号内容。
fn inline_table_version(table: &str) -> Option<String> {
    let pos = table.find("version")?;
    let after = table[pos + "version".len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let quote = after.chars().next()?;
    if !matches!(quote, '"' | '\'') {
        return None;
    }
    let end = after[1..].find(quote)? + 1;
    Some(after[1..end].to_string())
}

/// 依赖版本是否明显落后于当前工具版本（返回原始写法）。
///
/// 只做 major.minor 级别的粗判：`"0.1"` / `"0.2"` 这类旧约束会让宿主
/// 长期停在旧插件上，而 `init --check` 却保持绿色。`>=` 这类无上界的
/// 约束与 `path`/`git` 依赖无法判断，交给 `dependency_is_git` 单独警告。
fn dependency_is_stale(text: &str) -> Option<String> {
    let DeclaredVersion::Requirement(raw) = declared_version(text)? else {
        return None;
    };
    let raw = raw.trim();
    if raw == "*" || raw.starts_with(">=") {
        return None;
    }
    let numeric = raw.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = numeric.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = match parts.next() {
        Some(minor) => minor.parse().ok()?,
        None => 0,
    };
    let mut tool = env!("CARGO_PKG_VERSION").split('.');
    let tool_major: u64 = tool.next()?.parse().ok()?;
    let tool_minor: u64 = tool.next()?.parse().ok()?;
    (major < tool_major || (major == tool_major && minor < tool_minor)).then(|| raw.to_string())
}

/// 先写临时文件再替换，避免中断留下半份配置。
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 {} 失败: {e}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let tmp = path.with_file_name(format!(".{file_name}.tdx-tmp"));
    fs::write(&tmp, contents).map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
    if fs::rename(&tmp, path).is_err() {
        fs::copy(&tmp, path).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        config_value_issues, dependency_is_stale, endpoint_issue, ensure_dependency,
        has_dependency, minor_requirement, pick_capability, pubkey_issue,
    };
    use serde_json::json;

    /// 端点静态校验必须在离线 CI 中拦下错 scheme 与非回环明文 HTTP。
    #[test]
    fn endpoint_issue_catches_unusable_addresses() {
        assert!(endpoint_issue("https://cdn.example.com/latest.json", false).is_none());
        assert!(endpoint_issue("http://127.0.0.1:8080/latest.json", false).is_none());
        assert!(endpoint_issue("http://localhost/latest.json", false).is_none());
        assert!(
            endpoint_issue("http://cdn.example.com/latest.json", true).is_none(),
            "显式允许明文传输时必须放行"
        );

        let issue = endpoint_issue("http://cdn.example.com/latest.json", false)
            .expect("非回环明文 HTTP 必须报错");
        assert!(issue.contains("allow_insecure_transport"), "{issue}");
        assert!(endpoint_issue("ftp://cdn.example.com/latest.json", false).is_some());
        assert!(endpoint_issue("  ", false).is_some());
        assert!(
            endpoint_issue("http://{{host}}/latest.json", false).is_none(),
            "含运行时占位符时交给插件展开，静态阶段不误判"
        );
    }

    /// 公钥占位符与截断必须在接入阶段暴露。
    #[test]
    fn pubkey_issue_rejects_placeholders() {
        assert!(pubkey_issue("PK").is_some());
        assert!(pubkey_issue("BASE64_PUBLIC_KEY").is_some());
        assert!(pubkey_issue("   ").is_some());
    }

    /// 合法配置不得被 schema 校验误报。
    #[test]
    fn accepts_valid_config_values() {
        let value = json!({
            "endpoints": ["https://cdn.example.com/{{channel}}/latest.json"],
            "trust_policy": "all_of",
            "fallback_policy": "on_any_failure",
            "install_mode": "basicUi",
            "installer_kind": "inno",
            "channel": "Beta",
            "bundle_type": "nsis",
            "allow_legacy_signatures": false,
            "allow_insecure_transport": false,
            "install_on_next_launch": true,
            "rollback_notice": false,
            "wait_timeout_secs": 20,
            "confirm_timeout_secs": 120,
            "work_dir": "/tmp/tdx",
            "install_id": "host-id",
            "signature_url": "http://127.0.0.1:8080/latest.json.sig",
            "bundle_endpoint": "https://cdn.example.com/latest.bundle.json",
            "headers": { "X-Auth": "token" },
            "proxy": "http://127.0.0.1:7890",
            "installer_args": ["/NORESTART", "/LANG=en"],
            "windows": {
                "installMode": "quiet",
                "installerArgs": ["/LANG=en"]
            }
        });
        let map = value.as_object().unwrap().clone();
        assert_eq!(config_value_issues(&map, false), Vec::<String>::new());
    }

    /// 类型、枚举、代理与占位符依赖写错时，必须一次列清楚。
    #[test]
    fn reports_invalid_config_values() {
        let value = json!({
            "endpoints": ["https://cdn.example.com/{{bundle_type}}/latest.json"],
            "trust_policy": "majority",
            "fallback_policy": "sometimes",
            "install_mode": "silentish",
            "installer_kind": "innosetup",
            "authenticode_policy": "relaxed",
            "lock_scope": "machine",
            "channel": "be ta",
            "proxy": "http://[",
            "headers": { "X-Auth": 1, "Bad Header": "x" },
            "additional_pubkeys": ["PK"],
            "installer_args": ["bad\0arg"],
            "wait_timeout_secs": 0,
            "windows": {
                "installMode": "weird",
                "installerArgs": [42]
            }
        });
        let map = value.as_object().unwrap().clone();
        let issues = config_value_issues(&map, false);
        for expected in [
            "trust_policy",
            "fallback_policy",
            "install_mode",
            "installer_kind",
            "authenticode_policy",
            "lock_scope",
            "channel",
            "proxy",
            "headers.X-Auth",
            "请求头名",
            "additional_pubkeys[0]",
            "installer_args[0]",
            "wait_timeout_secs",
            "bundle_type",
            "windows.installMode",
            "windows.installerArgs[0]",
        ] {
            assert!(
                issues.iter().any(|issue| issue.contains(expected)),
                "缺少 {expected} 的错误: {issues:?}"
            );
        }
    }

    /// 插件 `FileConfig` 的每个字段都必须被 `init --check` 覆盖，
    /// 或明确属于“由其它检查/内部逻辑处理”的例外。
    ///
    /// 插件新增配置项而 CLI 守卫没跟上时，这条测试会失败，
    /// 避免“配置写了却没人校验”的长期漂移。
    #[test]
    fn config_schema_covers_plugin_fields() {
        const NOT_VALIDATED_HERE: &[&str] = &["endpoints", "pubkey", "extra"];
        const VALIDATED: &[&str] = &[
            "additional_pubkeys",
            "trust_policy",
            "channel",
            "signature_url",
            "bundle_endpoint",
            "bundle_type",
            "install_id",
            "work_dir",
            "wait_timeout_secs",
            "confirm_timeout_secs",
            "proxy",
            "allow_legacy_signatures",
            "fallback_policy",
            "headers",
            "install_mode",
            "installer_kind",
            "authenticode_policy",
            "lock_scope",
            "installer_args",
            "allow_insecure_transport",
            "install_on_next_launch",
            "rollback_notice",
        ];

        let source = include_str!("../../../plugin/src/config.rs");
        let start = source
            .find("pub(crate) struct FileConfig {")
            .expect("插件必须定义 FileConfig");
        let body = &source[start..start + source[start..].find("\n}").expect("结构体必须闭合")];
        let fields: Vec<&str> = body
            .lines()
            .skip(1) // 第一行是结构体声明本身
            .filter_map(|line| {
                let line = line.trim();
                let rest = line.strip_prefix("pub(crate) ")?;
                let name = rest.split(':').next()?.trim();
                (!name.is_empty()).then_some(name)
            })
            .collect();
        assert!(!fields.is_empty(), "必须解析出插件配置字段");

        for field in &fields {
            assert!(
                VALIDATED.contains(field) || NOT_VALIDATED_HERE.contains(field),
                "插件配置字段 {field} 没有被 init --check 覆盖；请补校验或加入例外"
            );
        }
        for key in VALIDATED {
            assert!(
                fields.contains(key),
                "校验列表包含插件不存在的字段 {key}，请同步清理"
            );
        }
    }

    /// 多 capability 项目必须复用已有文件，而不是另建 `default.json`。
    #[test]
    fn pick_capability_prefers_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            pick_capability(dir.path()),
            dir.path().join("default.json"),
            "没有任何文件时新建 default.json"
        );

        std::fs::write(dir.path().join("main.json"), r#"{"identifier":"main"}"#).unwrap();
        assert_eq!(
            pick_capability(dir.path()),
            dir.path().join("main.json"),
            "唯一已有文件必须被复用"
        );

        std::fs::write(
            dir.path().join("default.json"),
            r#"{"identifier":"default"}"#,
        )
        .unwrap();
        assert_eq!(
            pick_capability(dir.path()),
            dir.path().join("default.json"),
            "default.json 优先"
        );
    }

    /// 没有 default.json 时按 identifier 选择，避免写到无关的 capability。
    #[test]
    fn pick_capability_matches_identifier() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), r#"{"identifier":"other"}"#).unwrap();
        std::fs::write(dir.path().join("b.json"), r#"{"identifier":"default"}"#).unwrap();
        assert_eq!(pick_capability(dir.path()), dir.path().join("b.json"));
    }

    /// 依赖版本落后必须在 --check 中暴露；当前/更新版本与 path/git 依赖不得误报。
    #[test]
    fn detects_stale_dependency_version() {
        // 0.x 阶段：0.1 一定落后于当前工具，路径依赖无法判断。
        assert_eq!(
            dependency_is_stale("[dependencies]\ntauri-plugin-delta-updater = \"0.1\"\n"),
            Some("0.1".to_string())
        );
        assert_eq!(
            dependency_is_stale(
                "[dependencies]\ntauri-plugin-delta-updater = { version = \"0.1\", features = [\"x\"] }\n"
            ),
            Some("0.1".to_string())
        );
        assert!(
            dependency_is_stale(&format!(
                "[dependencies]\ntauri-plugin-delta-updater = \"{}\"\n",
                minor_requirement()
            ))
            .is_none(),
            "工具自身写的 minor 约束必须被接受"
        );
        assert!(
            dependency_is_stale(
                "[dependencies]\ntauri-plugin-delta-updater = { path = \"../plugin\" }\n"
            )
            .is_none(),
            "path 依赖没有版本号，不能误判为落后"
        );
        assert!(
            dependency_is_stale("[dependencies]\ntauri-plugin-delta-updater = \">=0.1\"\n")
                .is_none(),
            "无上界的版本约束允许升级，不能误判"
        );
    }

    /// 依赖插入必须保留原有内容与换行风格。
    #[test]
    fn inserts_dependency_after_table_header() {
        let text = "[dependencies]\nserde = \"1\"\n";
        let out = ensure_dependency(text, false);
        assert!(out.contains("tauri-plugin-delta-updater"));
        assert!(out.contains("serde = \"1\""));
        assert!(out.ends_with('\n'));
        assert!(has_dependency(&out));
    }

    /// 已经存在（包括 path / git / 版本号）时不能重复插入，也不能误判缺失。
    #[test]
    fn skips_existing_dependency() {
        for line in [
            "tauri-plugin-delta-updater = { path = \"../plugin\" }",
            "tauri-plugin-delta-updater = { git = \"https://example.com/repo\" }",
            "tauri-plugin-delta-updater = \"0.3.0\"",
        ] {
            let text = format!("[dependencies]\n{line}\n");
            assert_eq!(
                ensure_dependency(&text, false),
                text,
                "不得重复插入: {line}"
            );
            assert!(has_dependency(&text), "必须识别已有依赖: {line}");
        }
    }

    /// 默认写 minor 约束，`--registry` 写精确版本。
    #[test]
    fn dependency_source_is_valid_cargo_syntax() {
        let default = ensure_dependency("[dependencies]\n", false);
        assert_eq!(
            default.trim_end(),
            format!(
                "[dependencies]\ntauri-plugin-delta-updater = \"{}\"",
                minor_requirement()
            ),
            "默认必须只约束到 minor"
        );

        let exact = ensure_dependency("[dependencies]\n", true);
        assert_eq!(
            exact.trim_end(),
            format!(
                "[dependencies]\ntauri-plugin-delta-updater = \"{}\"",
                env!("CARGO_PKG_VERSION")
            ),
            "--registry 必须写精确版本"
        );
    }
}
