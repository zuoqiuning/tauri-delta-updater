//! 项目文件定位、配置改写与原子写入。

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

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
    ) -> Result<Vec<Change>, String> {
        let with_bundle = self.config_with_delta(None, endpoint, pubkey, bundle_endpoint, force)?;
        let capability = self.capability_with_delta(true, false)?;
        let cargo = self.cargo_with_delta()?;
        Ok(vec![with_bundle, capability, cargo]
            .into_iter()
            .filter(|change| change.before != change.after)
            .collect())
    }

    /// 检查三项接入是否完整；返回缺失项（空表示已完整）。
    pub(crate) fn init_status(&self) -> Result<Vec<&'static str>, String> {
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
            missing.push("plugins.delta-updater（endpoints + pubkey）");
        }

        let (_, capability) = read_json_or(&self.capability_path, json!({}))?;
        let capability_ok = capability
            .get("permissions")
            .and_then(Value::as_array)
            .is_some_and(|permissions| {
                permissions
                    .iter()
                    .any(|v| v.as_str() == Some("delta-updater:default"))
            });
        if !capability_ok {
            missing.push("capabilities/default.json 的 delta-updater:default");
        }

        let cargo = fs::read_to_string(&self.cargo_toml)
            .map_err(|e| format!("读取 {} 失败: {e}", self.cargo_toml.display()))?;
        if !has_dependency(&cargo) {
            missing.push("src-tauri/Cargo.toml 的 tauri-plugin-delta-updater");
        }

        Ok(missing)
    }

    /// 是否保留了官方 updater（双栈模式：本引擎增量 + 官方全量回退）。
    pub(crate) fn uses_official_updater(&self) -> Result<bool, String> {
        let (_, root) = read_json_or(&self.config_path, json!({}))?;
        Ok(root.pointer("/plugins/updater").is_some())
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
        let cargo = self.cargo_with_delta()?;
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
        let (before, mut root) = read_json_or(&self.capability_path, json!({ "permissions": [] }))?;
        let permissions = root
            .as_object_mut()
            .ok_or_else(|| "capability 根节点必须是对象".to_string())?
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

    fn cargo_with_delta(&self) -> Result<Change, String> {
        let before = fs::read_to_string(&self.cargo_toml)
            .map_err(|e| format!("读取 {} 失败: {e}", self.cargo_toml.display()))?;
        let after = ensure_dependency(&before);
        Ok(Change {
            path: self.cargo_toml.clone(),
            before,
            after,
        })
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
fn ensure_dependency(text: &str) -> String {
    if has_dependency(text) {
        return text.to_string();
    }

    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<&str> = text.lines().collect();
    let insert_at = lines
        .iter()
        .position(|line| line.trim() == "[dependencies]")
        .map(|index| index + 1);
    match insert_at {
        Some(index) => lines.insert(index, "tauri-plugin-delta-updater = \"0.1\""),
        None => {
            lines.push("");
            lines.push("[dependencies]");
            lines.push("tauri-plugin-delta-updater = \"0.1\"");
        }
    }

    let mut rendered = lines.join(nl);
    if text.ends_with('\n') || text.is_empty() {
        rendered.push_str(nl);
    }
    rendered
}

fn has_dependency(text: &str) -> bool {
    const NAME: &str = "tauri-plugin-delta-updater";
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(NAME) {
            return false;
        }
        let rest = trimmed[NAME.len()..].trim_start();
        rest.starts_with('=') || rest.starts_with('.')
    })
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
    use super::ensure_dependency;

    /// 依赖插入必须保留原有内容与换行风格。
    #[test]
    fn inserts_dependency_after_table_header() {
        let text = "[dependencies]\nserde = \"1\"\n";
        let out = ensure_dependency(text);
        assert!(out.contains("tauri-plugin-delta-updater = \"0.1\""));
        assert!(out.contains("serde = \"1\""));
        assert!(out.ends_with('\n'));
    }

    /// 已经存在（包括 path 依赖）时不能重复插入。
    #[test]
    fn skips_existing_dependency() {
        let text = "[dependencies]\ntauri-plugin-delta-updater = { path = \"../plugin\" }\n";
        assert_eq!(ensure_dependency(text), text);
    }
}
