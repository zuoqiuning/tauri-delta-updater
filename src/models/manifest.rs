use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

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
    /// Tauri 原生字段：各平台完整安装包的下载地址与签名
    #[serde(default)]
    pub platforms: HashMap<String, PlatformInfo>,
    /// 【扩展字段】增量补丁列表，按平台分组。
    ///
    /// 必须按平台分组：不同架构的安装包二进制完全不同，
    /// 若混在一个列表里，客户端可能取到其它架构的补丁，导致校验失败。
    #[serde(default, deserialize_with = "deserialize_patches")]
    pub patches: HashMap<String, Vec<PatchInfo>>,
}

impl UpdateManifest {
    /// 根据当前版本号与目标平台查找匹配的增量补丁。
    ///
    /// 如果找不到（跨版本更新或没有对应补丁），返回 `None`，调用方应回退到全量下载。
    ///
    /// 只返回目标版本与清单版本一致的补丁：清单里可能残留其它平台上一轮发布的
    /// 补丁，直接使用会把应用更新到错误的版本。
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

    /// 清单版本是否严格高于给定版本。
    ///
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
