use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// 客户端允许的最长补丁链跳数。
///
/// 发布端按同一上限保留历史补丁（`cargo delta generate` 会自动裁剪），
/// 两端必须一致：客户端放宽只会走上永远走不通的链，收紧则让已发布的历史
/// 补丁失效。
pub const MAX_DELTA_CHAIN_HOPS: usize = 3;

/// 完全兼容 Tauri 官方 Updater 插件的 latest.json 格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    /// 目标版本号。
    ///
    /// 必须与客户端上报的版本号格式一致（不带 `v` 前缀），
    /// 否则补丁永远匹配不上。
    pub version: String,
    /// 更新说明。
    #[serde(default)]
    pub notes: String,
    /// 发布时间。
    #[serde(default)]
    pub pub_date: String,
    /// 最低允许版本：低于此版本的客户端必须更新，不受灰度比例限制。
    ///
    /// 用于"坏版本已经推出，需要强制所有人离开"的场景。
    /// 旧清单没有该字段时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// Tauri 原生字段：各平台完整安装包的下载地址与签名
    #[serde(default)]
    pub platforms: HashMap<String, PlatformInfo>,
    /// 【扩展字段】增量补丁列表，按平台分组。
    ///
    /// 必须按平台分组：不同架构的安装包二进制完全不同，
    /// 若混在一个列表里，客户端可能取到其它架构的补丁，导致校验失败。
    #[serde(default, deserialize_with = "deserialize_patches")]
    pub patches: HashMap<String, Vec<PatchInfo>>,
    /// 多语言更新说明（locale → 文本），[`Self::notes`] 作为兜底。
    ///
    /// 受清单签名保护；旧客户端只读 `notes`，不会受影响。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub notes_i18n: HashMap<String, String>,
    /// 未识别的顶层字段，原样保留。
    ///
    /// 让宿主可以读取发布端的自定义元数据（渠道标记、发布页 URL 等），
    /// 也让 `generate --existing` 不会丢掉旧清单里的扩展字段。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl UpdateManifest {
    /// 根据当前版本号与目标平台查找匹配的增量补丁。
    ///
    /// 如果找不到（跨版本更新或没有对应补丁），返回 `None`，调用方应回退到全量下载。
    ///
    /// 只返回目标版本与清单版本一致的补丁：清单里可能残留其它平台上一轮发布的
    /// 补丁，直接使用会把应用更新到错误的版本。
    ///
    /// **只返回直达补丁（1 跳）。** 更新流程会自动串起最多
    /// [`MAX_DELTA_CHAIN_HOPS`] 跳的历史补丁（`0.1 -> 0.2 -> 0.3`）；
    /// 需要同样的能力时用 `check()` / `update()`，不要自行拼装。
    pub fn find_patch(&self, current_version: &str, platform: &str) -> Option<&PatchInfo> {
        self.patches
            .get(platform)?
            .iter()
            .find(|p| p.from_version == current_version && p.to_version == self.version)
    }

    /// 该清单是否为指定平台提供了增量补丁。
    pub fn has_platform_patches(&self, platform: &str) -> bool {
        self.patches.get(platform).is_some_and(|p| !p.is_empty())
    }

    /// 按语言取更新说明：精确匹配 → 语言前缀（`zh-CN` → `zh`）→ `notes` 兜底。
    ///
    /// 只**向下**匹配：`zh-CN` 能命中 `zh` 条目；`zh` 不会命中 `zh-CN` 条目。
    /// 未知语言（包括空字符串）一律回退到 [`Self::notes`]。
    pub fn notes_for(&self, locale: &str) -> &str {
        let locale = locale.trim();
        if let Some(text) = self.notes_i18n.get(locale) {
            return text;
        }
        let language = locale.split(['-', '_']).next().unwrap_or(locale);
        if let Some(text) = self.notes_i18n.get(language) {
            return text;
        }
        &self.notes
    }

    /// 清单版本是否严格高于给定版本。
    /// **这是防降级的核心判据**。签名只能证明"这份清单出自发布者之手"，
    /// 不能证明"它比当前版本新"——攻击者可以原样重放一份旧的、签名合法的
    /// 清单，把用户降级回含已知漏洞的版本。版本单调性必须由客户端自己判。
    ///
    /// 按语义化版本比较（字符串比较会把 `0.10.0` 判成低于 `0.9.0`）。
    /// 任一侧不是合法语义化版本时返回 [`crate::DeltaError::VersionInvalid`]。
    pub fn is_newer_than(&self, current_version: &str) -> crate::Result<bool> {
        let target = parse_version(&self.version, "清单版本")?;
        let current = parse_version(current_version, "当前版本")?;
        Ok(target > current)
    }

    /// 当前版本是否低于清单的 `min_version`。
    ///
    /// 低于门槛时调用方应忽略灰度比例，强制更新：
    /// 这是"坏版本已经推出"后的最后一道保险。
    /// 未设置 `min_version` 时返回 `false`；值非法时返回
    /// [`crate::DeltaError::VersionInvalid`]。
    pub fn is_below_min_version(&self, current_version: &str) -> crate::Result<bool> {
        let Some(min) = &self.min_version else {
            return Ok(false);
        };
        let min = parse_version(min, "min_version")?;
        let current = parse_version(current_version, "当前版本")?;
        Ok(current < min)
    }
}

/// 解析语义化版本，失败时返回可读错误。
fn parse_version(value: &str, what: &str) -> crate::Result<semver::Version> {
    semver::Version::parse(value.trim()).map_err(|e| {
        crate::error::DeltaError::VersionInvalid(format!(
            "{what} {value:?} 不是合法的语义化版本: {e}"
        ))
    })
}

/// 兼容两种补丁格式：
/// - 新格式（按平台分组）：`{ "windows-x86_64": [ ... ] }`
/// - 旧格式（扁平列表）：`[ ... ]`
///
/// 旧格式无法区分平台，归入当前运行平台——这正是读它的客户端所需要的；
/// 其它平台的客户端自然取不到，只会安静地回退全量下载。
fn deserialize_patches<'de, D>(deserializer: D) -> Result<HashMap<String, Vec<PatchInfo>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Patches {
        Grouped(HashMap<String, Vec<PatchInfo>>),
        Flat(Vec<PatchInfo>),
    }

    match Patches::deserialize(deserializer)? {
        Patches::Grouped(map) => Ok(map),
        Patches::Flat(list) => Ok(HashMap::from([(
            crate::platform::target::current_platform().to_string(),
            list,
        )])),
    }
}

/// 单个平台的完整安装包信息（兼容 Tauri 官方格式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformInfo {
    /// 安装包的 minisign 签名（base64）。
    ///
    /// 供 Tauri 官方 updater 校验全量下载的安装包。
    pub signature: String,
    /// 完整安装包的下载地址。
    pub url: String,
    /// 完整安装包体积（字节）。
    ///
    /// 发布端写入，受清单签名保护；旧清单没有这个字段时为 `None`。
    /// 客户端据此计算补丁的真实收益比（`patch_size / size`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 该平台要求的最低系统版本（如 `10.0.19045` / `13.0` / `5.15`）。
    ///
    /// 受清单签名保护。客户端无法确定本机版本时**不阻断**（宁可放行也不误伤）；
    /// 能确定且低于该值时拒绝下载，并返回 `UpdateOutcome::UnsupportedOs`。
    ///
    /// # 平台语义
    ///
    /// - Windows：`major.minor.build`（经 `RtlGetVersion` 探测）；
    /// - macOS：`sw_vers -productVersion`（如 `13.4`）；
    /// - Linux：**内核 release**（如 `5.15.0`），不是发行版版本；写 `"24.04"`
    ///   之类的发行版号没有意义，Linux 上建议不要使用本字段。
    ///
    /// 这是**尽力而为的约束，不是安全边界**：探测失败（命令缺失、权限受限、
    /// 内核接口不可读）时更新会继续。不要用它替代平台自身的兼容性检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_os: Option<String>,
    /// 可选灰度配置；缺省表示全量发布（旧清单天然全量）。
    ///
    /// 由 `generate --rollout <0-100>` 写入，受清单签名保护。
    /// 客户端按稳定哈希分桶，未命中的安装会得到 `Deferred` 而不是开始下载。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<Rollout>,
}

/// 渐进发布（灰度）配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollout {
    /// 放量比例（0–100）。0 暂停发布，100 全量；中间值按安装 ID 分桶。
    pub percentage: u8,
}

/// 单个增量补丁的描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchInfo {
    /// 补丁的起始版本号（客户端当前版本）。
    pub from_version: String,
    /// 补丁的目标版本号（必须与清单的 `version` 一致）。
    pub to_version: String,
    /// 补丁文件的下载地址。
    pub patch_url: String,
    /// 补丁文件的 SHA-256 十六进制哈希。
    pub patch_hash: String,
    /// 合并产物的 SHA-256 十六进制哈希。
    pub target_hash: String,
    /// 补丁文件大小（字节）。
    ///
    /// 可选：让客户端在"检查更新"阶段就能告知用户需要下载多少数据，
    /// 而不必等真正开始下载才知道。旧清单没有这个字段时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_size: Option<u64>,
    /// 【扩展字段】补丁覆盖范围说明。
    ///
    /// 默认（`None`）表示补丁覆盖整个应用产物。本库的补丁基底与产物都是
    /// **主可执行文件**，使用 `externalBin`（sidecar）或 `resources` 的应用
    /// 需要发布端显式标注，客户端据此提示"本次增量不覆盖附加资源"。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PatchScope>,
}

/// 补丁覆盖范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PatchScope {
    /// 仅覆盖主可执行文件（本库的默认行为）。
    MainExecutableOnly,
    /// 覆盖完整应用产物（需要发布端生成整包补丁）。
    FullApp,
}

#[cfg(test)]
mod tests {
    use super::UpdateManifest;

    fn manifest(version: &str) -> UpdateManifest {
        UpdateManifest {
            version: version.to_string(),
            notes: String::new(),
            pub_date: String::new(),
            min_version: None,
            platforms: Default::default(),
            patches: Default::default(),
            notes_i18n: Default::default(),
            extra: Default::default(),
        }
    }

    /// 防降级依赖语义化版本比较，而不是字符串比较。
    #[test]
    fn compares_minor_versions_numerically() {
        assert!(manifest("0.10.0").is_newer_than("0.9.0").unwrap());
        assert!(!manifest("0.9.0").is_newer_than("0.10.0").unwrap());
        assert!(manifest("1.10.0").is_newer_than("1.9.0").unwrap());
    }

    /// 预发布版本低于同号正式版，不能被当成"更新到正式版"。
    #[test]
    fn prerelease_is_lower_than_release() {
        assert!(manifest("1.0.0").is_newer_than("1.0.0-alpha").unwrap());
        assert!(!manifest("1.0.0-alpha").is_newer_than("1.0.0").unwrap());
        assert!(manifest("1.0.0").is_newer_than("1.0.0-beta.2").unwrap());
    }

    /// 多语言说明：精确匹配 → 语言前缀 → `notes` 兜底。
    #[test]
    fn localized_notes_fall_back_to_language_then_default() {
        let mut m = manifest("1.2.3");
        m.notes = "default".into();
        m.notes_i18n.insert("zh-CN".into(), "中文".into());
        m.notes_i18n.insert("ja".into(), "日本語".into());

        assert_eq!(m.notes_for("zh-CN"), "中文");
        assert_eq!(m.notes_for("zh-TW"), "default", "没有 zh 前缀时回落默认");
        assert_eq!(m.notes_for("ja-JP"), "日本語", "语言前缀匹配");
        assert_eq!(m.notes_for("fr"), "default");
        assert_eq!(m.notes_for(""), "default");
    }

    /// 未识别的顶层字段必须原样保留（宿主自定义元数据）。
    #[test]
    fn unknown_top_level_fields_are_preserved() {
        let parsed: UpdateManifest = serde_json::from_value(serde_json::json!({
            "version": "1.2.3",
            "notes": "hi",
            "channel": "beta",
            "release_page": "https://example.com/releases/1.2.3"
        }))
        .unwrap();
        assert_eq!(parsed.extra["channel"], "beta");

        let round_trip = serde_json::to_value(&parsed).unwrap();
        assert_eq!(round_trip["channel"], "beta");
        assert_eq!(
            round_trip["release_page"],
            "https://example.com/releases/1.2.3"
        );
    }

    /// 灰度字段是可选的：旧清单没有它时按全量发布处理。
    #[test]
    fn rollout_is_optional_and_parsed() {
        let without: UpdateManifest = serde_json::from_value(serde_json::json!({
            "version": "1.2.3",
            "platforms": {
                "windows-x86_64": { "signature": "sig", "url": "https://cdn/app.exe" }
            }
        }))
        .unwrap();
        assert!(without.platforms["windows-x86_64"].rollout.is_none());

        let with: UpdateManifest = serde_json::from_value(serde_json::json!({
            "version": "1.2.3",
            "platforms": {
                "windows-x86_64": {
                    "signature": "sig",
                    "url": "https://cdn/app.exe",
                    "rollout": { "percentage": 20 }
                }
            }
        }))
        .unwrap();
        assert_eq!(
            with.platforms["windows-x86_64"].rollout.unwrap().percentage,
            20
        );
    }

    /// `min_version` 用于强制离开坏版本：低于门槛时必须返回 true。
    #[test]
    fn detects_versions_below_minimum() {
        let mut m = manifest("2.0.0");
        m.min_version = Some("1.5.0".into());
        assert!(m.is_below_min_version("1.4.9").unwrap());
        assert!(!m.is_below_min_version("1.5.0").unwrap());
        assert!(!m.is_below_min_version("1.6.0").unwrap());

        // 未设置门槛时永远不强制。
        assert!(!manifest("2.0.0").is_below_min_version("0.0.1").unwrap());

        // 非法门槛必须报错，不能静默当成 0。
        m.min_version = Some("not-a-version".into());
        assert!(matches!(
            m.is_below_min_version("1.0.0").unwrap_err(),
            crate::DeltaError::VersionInvalid(_)
        ));
    }

    /// 非法版本号必须给出明确的 `VersionInvalid`，而不是静默按 0 处理。
    #[test]
    fn rejects_invalid_versions() {
        let err = manifest("1.0").is_newer_than("0.1.0").unwrap_err();
        assert!(matches!(err, crate::DeltaError::VersionInvalid(_)));
        let err = manifest("1.0.0")
            .is_newer_than("not-a-version")
            .unwrap_err();
        assert!(matches!(err, crate::DeltaError::VersionInvalid(_)));
    }
}
