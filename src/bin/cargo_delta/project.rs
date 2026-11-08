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
        let capability_path = tauri_dir.join("capabilities/default.json");
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
        let config_ok = root
            .pointer("/plugins/delta-updater")
            .and_then(Value::as_object)
            .is_some_and(|delta| {
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
            missing.push("plugins.delta-updater（endpoints + pubkey）".to_string());
        }

        let (_, capability) = read_json_or(&self.capability_path, json!({}))?;
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
                    permissions
                        .iter()
                        .any(|v| v.as_str() == Some("delta-updater:default"))
                });
        if !capability_ok {
            missing.push(
                "capabilities/default.json（identifier + delta-updater:default）".to_string(),
            );
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

    /// 是否保留了官方 updater（双栈模式：本引擎增量 + 官方全量回退）。
    pub(crate) fn uses_official_updater(&self) -> Result<bool, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        Ok(root.pointer("/plugins/updater").is_some())
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
/// 默认写 `"0.3"`（允许同 minor 内 patch 升级）；`--registry` 写精确版本
/// `"0.3.0"`，用于必须与工具版本一致的宿主。
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
/// 默认只约束到 minor（`"0.3"`），让宿主拿到同系列补丁；`registry` 写精确
/// 版本，适合要求"依赖版本必须等于发布工具版本"的流水线。
fn dependency_line(registry: bool) -> String {
    if registry {
        format!("{PLUGIN_DEP} = \"{}\"", env!("CARGO_PKG_VERSION"))
    } else {
        format!("{PLUGIN_DEP} = \"{}\"", minor_requirement())
    }
}

/// 从工具版本推导 minor 约束（`0.3.0` → `0.3`）。
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
    use super::{dependency_is_stale, ensure_dependency, has_dependency, minor_requirement};

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
