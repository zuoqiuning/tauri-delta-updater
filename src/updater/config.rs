//! 增量更新配置。

use crate::core::verify::TrustPolicy;
use crate::error::DeltaError;
use crate::platform::full_install::{InstallMode, InstallerKind};
use crate::platform::target::current_platform;
use std::path::{Path, PathBuf};

/// 主清单不可用时，何时切换到备用端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FallbackPolicy {
    /// 连接失败、超时、5xx / 429 时切换。默认，与官方“镜像故障转移”语义一致；
    /// 404 等确定性错误不切换，避免把“清单还没发布”误判成需要换源。
    #[default]
    OnTransportError,
    /// 任何 HTTP / 传输失败都切换（含 404 / 403）。
    ///
    /// 适合“主端点 404 = 该通道未发布，备用端点有镜像”的部署；验签失败仍然终止，
    /// 绝不会因为换端点而绕过签名校验。
    ///
    /// # 代价
    ///
    /// 主端点返回 404 可能只是新版本尚未发布，而备用端点仍托管着**旧清单**。
    /// 选择本策略意味着接受“切到旧清单”的降级重放面（版本单调性仍会拒绝
    /// 低于当前版本的清单，但高于当前版本的旧清单无法识别）。默认策略
    /// 只在传输错误/5xx 时切换，正是为了避免这一点。
    OnAnyFailure,
}

impl FallbackPolicy {
    /// 该错误是否值得尝试下一个端点。
    pub(crate) fn should_fallback(self, err: &DeltaError) -> bool {
        match self {
            FallbackPolicy::OnTransportError => match err {
                DeltaError::Network(_) | DeltaError::Timeout(_) => true,
                DeltaError::HttpStatus(code) => *code >= 500 || *code == 429,
                _ => false,
            },
            FallbackPolicy::OnAnyFailure => matches!(
                err,
                DeltaError::Network(_)
                    | DeltaError::Timeout(_)
                    | DeltaError::HttpStatus(_)
                    | DeltaError::ResponseTooLarge { .. }
                    | DeltaError::InvalidResponse(_)
            ),
        }
    }
}

/// 增量更新的配置。
///
/// 除 `manifest_url` 与 `public_key` 外都有合理默认值，
/// 通常只需要设置这两项。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UpdateConfig {
    /// `latest.json` 的下载地址。
    ///
    /// [`Self::signature_url`] 默认由它推导；需要把签名独立托管时，
    /// 在构造后显式修改 `signature_url` 即可（留空会在构造阶段重新推导）。
    pub manifest_url: String,
    /// `latest.json.sig` 的下载地址。
    ///
    /// 默认按清单地址同名 + `.sig` 推导（保留查询参数）。
    /// 只对主端点生效：备用端点各自按自己的清单地址推导签名地址。
    pub signature_url: String,
    /// 用于校验清单签名的公钥。
    ///
    /// 必须是**固化在应用里**的公钥，不能从服务器下载。
    pub public_key: String,
    /// 额外的受信任公钥（密钥轮换 / 双签过渡）。
    ///
    /// 与 [`Self::public_key`] 组成信任集合；每个公钥都接受 base64 或
    /// `.pub` 原文。是否要求全部签名由 [`Self::trust_policy`] 决定。
    pub additional_public_keys: Vec<String>,
    /// 多钥信任策略，默认 [`TrustPolicy::AnyOf`]（任意一把有效签名即可）。
    pub trust_policy: TrustPolicy,
    /// 当前运行中的版本号，用于匹配补丁与拒绝降级。
    ///
    /// 必须是语义化版本（与 Tauri 的 `app.package_info().version` 一致）。
    /// 首尾空白与 `v` 前缀会在 [`DeltaUpdater::new`] 中规范化掉。
    pub current_version: String,
    /// 目标平台标识。默认使用当前构建的平台。
    pub platform: String,
    /// 被更新的可执行文件。
    ///
    /// 为 `None` 时自动使用当前进程的可执行文件。
    /// 当前进程的可执行文件无法确定时（例如某些沙箱环境），
    /// [`DeltaUpdater::new`] 会返回明确的错误，而不是退化成相对路径。
    pub executable_path: Option<PathBuf>,
    /// 工作目录，用于存放补丁、暂存文件、替换脚本与日志。
    ///
    /// 为 `None` 时使用用户缓存目录（见 [`crate::default_work_dir`]），
    /// **不再默认放在 exe 旁边**：系统级安装目录对普通用户只读，
    /// 默认放在那里会让更新在下载阶段就失败。
    pub work_dir: Option<PathBuf>,
    /// 合并完成后是否自动启动替换脚本。
    ///
    /// 默认 `true`。设为 `false` 时由宿主调用 [`Staged::launch`] 自行决定时机。
    pub auto_launch: bool,
    /// 等待旧进程退出的最长秒数。默认 60。
    ///
    /// 写入替换脚本（`ScriptTimings`）；启动慢或希望更快回滚时调整它。
    pub wait_timeout_secs: u32,
    /// 等待新版本写入启动成功标志的最长秒数。默认 60。
    ///
    /// 同样写入替换脚本。太大时坏版本会多"存活"一段时间，
    /// 太小时初始化较慢的正常版本会被误判回滚——按实际启动耗时留足余量。
    pub confirm_timeout_secs: u32,
    /// 灰度分桶使用的安装 ID。
    ///
    /// 为 `None` 时自动在工作目录生成并持久化（`tdx_install_id`），
    /// 同一安装在同一版本上的分桶结果稳定。
    /// 多租户或共享工作目录的宿主可显式传入自己的稳定 ID。
    pub install_id: Option<String>,
    /// 整包清单 `latest.bundle.json` 的下载地址。
    ///
    /// 设置后客户端优先走整包（多文件）增量：它覆盖 `externalBin` 与
    /// `resources`，而单文件补丁只能替换主程序。签名从同名 `.sig` 地址推导。
    ///
    /// 为 `None` 时保持原有行为：只使用 `latest.json` 里的主程序补丁。
    pub bundle_manifest_url: Option<String>,
    /// 注入自定义 HTTP 客户端，用于设置代理、UA、超时等。
    ///
    /// 为 `None` 时使用默认客户端。宿主若有统一的网络层，建议注入自己的；
    /// 自定义客户端使用 rustls 时，必须在创建前调用
    /// [`crate::install_crypto_provider`]。
    pub client: Option<reqwest::Client>,
    /// 是否接受旧版（legacy）minisign 签名，默认 `false`。
    ///
    /// 本项目发布端固定 minisign 0.7.9，产出的是 pre-hash 格式。
    /// 只有仍在使用第三方历史管线、需要兼容旧格式签名的宿主才应显式设为
    /// `true`；开启会接受非预哈希签名，属于签名校验行为放宽。
    pub allow_legacy_signatures: bool,
    /// 备用清单地址（主地址失败时按策略依次尝试）。
    ///
    /// 每个端点都有自己的 `.sig` 地址（同目录同名 + `.sig`）。
    pub fallback_manifest_urls: Vec<String>,
    /// 主清单失败后何时切换备用端点，默认只在传输错误/5xx 时切换。
    pub fallback_policy: FallbackPolicy,
    /// 附加到所有 HTTP 请求的请求头（私有 CDN Token、User-Agent 等）。
    ///
    /// 只能配合内置客户端使用；同时设置 [`Self::client`] 时由调用方自行配置请求头。
    ///
    /// # 注意
    ///
    /// 不要设置 `Host`、`Content-Length`、`Connection`、`Transfer-Encoding`
    /// 等传输层头：它们由 HTTP 客户端按连接和请求体自动生成，手动覆盖会
    /// 破坏连接复用、TLS SNI 或请求分帧。这里只应放业务头
    /// （`Authorization`、`X-Api-Key`、`User-Agent` 等）。
    pub headers: Vec<(String, String)>,
    /// Windows 全量安装器的交互模式，默认 `Passive`（与官方一致）。
    ///
    /// 其它平台忽略该字段。
    pub install_mode: InstallMode,
    /// 全量安装器类型。默认 `None`：按 `platforms[].url` 的后缀推断
    /// （`.exe` → NSIS、`.msi` → MSI、`.app.tar.gz` → macOS 包、
    /// `.AppImage` → AppImage）。
    ///
    /// **用 Inno Setup 打包的宿主必须显式设为
    /// [`InstallerKind::Inno`]**：Inno 与 NSIS 的产物都是 `.exe`，
    /// 后缀无法区分；误判为 NSIS 会传入 `/P /UPDATE /R`，Inno 不识别这些
    /// 参数，表现为弹完整 UI 或直接失败，且引擎侧没有任何错误。
    ///
    /// 该字段只被本引擎的全量安装路径（[`crate::DeltaUpdater::install_full`]）
    /// 消费。宿主若继续用官方 `tauri-plugin-updater` 执行全量安装，
    /// 这里配置什么都不会生效：官方插件只会按 NSIS 参数调用安装包。
    pub installer_kind: Option<InstallerKind>,
    /// 是否允许明文 HTTP 端点，默认 `false`。
    ///
    /// 默认拒绝非回环地址的 `http://`；本地开发/内网测试可显式设为 `true`。
    /// 无论取值如何，明文端点都会出现在 `diagnose().warnings()` 中。
    pub allow_insecure_transport: bool,
    /// 插件是否在下次启动时自动执行待安装记录，默认 `false`。
    ///
    /// 该字段只被 Tauri 插件消费；库本身不会自动安装，宿主需显式调用
    /// [`crate::DeltaUpdater::install_pending`]。
    pub install_on_next_launch: bool,
    /// 回滚后是否弹出系统提示，默认 `true`。
    ///
    /// 更新失败、旧版本重新启动后，替换脚本会显示一条最小提示：
    /// Windows 是消息框，macOS / Linux 是桌面通知。提示在独立进程里触发，
    /// 不阻塞替换脚本；无交互桌面（服务、计划任务）时自动跳过。
    ///
    /// 只作用于本库内置的更新流程；直接调用 [`crate::PlatformUpdater`]
    /// 生成脚本时不弹提示，由宿主自行掌控界面。
    pub rollback_notice: bool,
}

impl UpdateConfig {
    /// 以清单地址与公钥创建配置，其余项自动填充。
    pub fn new(manifest_url: impl Into<String>, public_key: impl Into<String>) -> Self {
        let manifest_url = manifest_url.into();

        Self {
            // 默认与清单同目录同名的 .sig 文件
            signature_url: default_signature_url(&manifest_url),
            manifest_url,
            public_key: public_key.into(),
            additional_public_keys: Vec::new(),
            trust_policy: TrustPolicy::default(),
            current_version: String::new(),
            platform: current_platform().to_string(),
            executable_path: None,
            work_dir: None,
            auto_launch: true,
            wait_timeout_secs: 60,
            confirm_timeout_secs: 60,
            install_id: None,
            bundle_manifest_url: None,
            client: None,
            allow_legacy_signatures: false,
            fallback_manifest_urls: Vec::new(),
            fallback_policy: FallbackPolicy::default(),
            headers: Vec::new(),
            install_mode: InstallMode::default(),
            installer_kind: None,
            allow_insecure_transport: false,
            install_on_next_launch: false,
            rollback_notice: true,
        }
    }

    /// 设置当前版本号。
    pub fn current_version(mut self, version: impl Into<String>) -> Self {
        self.current_version = version.into();
        self
    }

    /// 设置主清单的签名地址。
    ///
    /// 缺省按 [`Self::manifest_url`] 同名 + `.sig` 推导（保留查询参数）；
    /// 显式留空会在 [`crate::DeltaUpdater::new`] 中重新推导。
    /// 只对主端点生效：备用端点各自按自己的清单地址推导。
    pub fn signature_url(mut self, url: impl Into<String>) -> Self {
        self.signature_url = url.into();
        self
    }

    /// 追加受信任公钥（密钥轮换期的旧钥或新钥）。
    pub fn additional_public_keys(mut self, keys: Vec<String>) -> Self {
        self.additional_public_keys = keys;
        self
    }

    /// 设置多钥信任策略（默认任意一把有效签名即可）。
    pub fn trust_policy(mut self, policy: TrustPolicy) -> Self {
        self.trust_policy = policy;
        self
    }

    /// 覆盖目标平台标识。默认使用当前构建的平台，通常无需设置。
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = platform.into();
        self
    }

    /// 覆盖被更新的可执行文件路径。
    pub fn executable_path(mut self, path: impl AsRef<Path>) -> Self {
        self.executable_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// 覆盖工作目录。
    ///
    /// 必须是用户可写目录。自定义后，启动确认要调用
    /// [`DeltaUpdater::confirm_startup_success`]（实例方法），
    /// 而不是静态的 [`PlatformUpdater::confirm_startup_success`]。
    pub fn work_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.work_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// 关闭自动启动替换脚本，改由宿主调用 [`Staged::launch`]。
    pub fn auto_launch(mut self, auto: bool) -> Self {
        self.auto_launch = auto;
        self
    }

    /// 设置等待旧进程退出的超时（秒）。
    pub fn wait_timeout_secs(mut self, secs: u32) -> Self {
        self.wait_timeout_secs = secs;
        self
    }

    /// 设置等待新版本启动确认的超时（秒）。
    pub fn confirm_timeout_secs(mut self, secs: u32) -> Self {
        self.confirm_timeout_secs = secs;
        self
    }

    /// 覆盖灰度分桶使用的安装 ID（默认自动生成并持久化）。
    pub fn install_id(mut self, id: impl Into<String>) -> Self {
        self.install_id = Some(id.into());
        self
    }

    /// 启用整包（多文件）增量，地址指向 `latest.bundle.json`。
    ///
    /// 签名地址自动推导为同目录同名 `.sig`。发布端用
    /// `generate --new-app-dir … --old-app-dir …` 生成该清单。
    pub fn bundle_manifest_url(mut self, url: impl Into<String>) -> Self {
        self.bundle_manifest_url = Some(url.into());
        self
    }

    /// 注入自定义 HTTP 客户端（代理、UA、超时等）。
    pub fn client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// 是否接受旧版（legacy）minisign 签名，默认 `false`。
    ///
    /// 仅第三方历史管线需要显式开启；本项目的发布端始终产出 pre-hash 签名。
    /// 这是签名校验行为放宽，开启前确认上游确实使用旧格式。
    pub fn allow_legacy_signatures(mut self, allow: bool) -> Self {
        self.allow_legacy_signatures = allow;
        self
    }

    /// 设置备用清单端点（主端点失败时按策略尝试）。
    pub fn fallback_endpoints(mut self, urls: Vec<String>) -> Self {
        self.fallback_manifest_urls = urls;
        self
    }

    /// 设置端点切换策略，默认仅在传输错误/5xx 时切换。
    pub fn fallback_policy(mut self, policy: FallbackPolicy) -> Self {
        self.fallback_policy = policy;
        self
    }

    /// 设置附加请求头（私有 CDN Token、定制 UA 等）。
    pub fn headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    /// 设置 Windows 全量安装器交互模式。
    pub fn install_mode(mut self, mode: InstallMode) -> Self {
        self.install_mode = mode;
        self
    }

    /// 显式声明全量安装器类型。
    ///
    /// 用 Inno Setup 打包的宿主必须设为 [`InstallerKind::Inno`]；NSIS / MSI
    /// 可省略（按文件后缀推断）。配置与后缀明显矛盾时（如 `.msi` 配 Inno），
    /// 安装阶段会返回 [`crate::DeltaError::Config`]。
    pub fn installer_kind(mut self, kind: InstallerKind) -> Self {
        self.installer_kind = Some(kind);
        self
    }

    /// 是否允许明文 HTTP 端点（默认拒绝；仅本地开发/内网测试可临时开启）。
    pub fn allow_insecure_transport(mut self, allow: bool) -> Self {
        self.allow_insecure_transport = allow;
        self
    }

    /// 插件是否在下次启动时自动执行待安装记录（默认 `false`）。
    pub fn install_on_next_launch(mut self, auto: bool) -> Self {
        self.install_on_next_launch = auto;
        self
    }

    /// 回滚后是否弹出系统提示（默认 `true`）。
    ///
    /// 关闭后更新失败仍然写回滚原因与事件，只是不再打扰用户。
    pub fn rollback_notice(mut self, notice: bool) -> Self {
        self.rollback_notice = notice;
        self
    }

    /// 主清单、签名地址、备用端点或整包清单中第一个明文 HTTP 地址。
    ///
    /// 签名与整包清单同样承载信任材料：只检查 manifest_url 会留下
    /// “清单走 HTTPS、签名走 HTTP”的降级窗口。
    pub(crate) fn insecure_endpoint(&self) -> Option<&str> {
        std::iter::once(self.manifest_url.as_str())
            .chain(std::iter::once(self.signature_url.as_str()))
            .chain(self.fallback_manifest_urls.iter().map(String::as_str))
            .chain(self.bundle_manifest_url.iter().map(String::as_str))
            .find(|url| endpoint_is_insecure(url))
    }

    /// 任一更新端点使用明文 HTTP（且非回环）。
    pub(crate) fn uses_insecure_transport(&self) -> bool {
        self.insecure_endpoint().is_some()
    }

    /// 完整的受信任公钥集合（主公钥 + 额外公钥）。
    pub(crate) fn trusted_keys(&self) -> Vec<&str> {
        std::iter::once(self.public_key.as_str())
            .chain(self.additional_public_keys.iter().map(|key| key.as_str()))
            .collect()
    }
}

/// 端点是否使用明文 HTTP 且不是回环地址。
pub(crate) fn endpoint_is_insecure(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    !matches!(
        parsed.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

/// 由清单地址推导签名地址。
///
/// 在路径末尾追加 `.sig`，并**保留查询参数**：
/// `https://cdn/latest.json?token=abc` 应得到
/// `https://cdn/latest.json.sig?token=abc`，
/// 而不是把 `.sig` 追加到 token 后面。
pub fn default_signature_url(manifest_url: &str) -> String {
    match manifest_url.find('?') {
        Some(pos) => {
            let (base, query) = manifest_url.split_at(pos);
            format!("{base}.sig{query}")
        }
        None => format!("{manifest_url}.sig"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str) -> UpdateConfig {
        UpdateConfig::new(url, "PK").current_version("0.1.0")
    }

    fn updater(config: UpdateConfig) -> crate::error::Result<crate::updater::DeltaUpdater> {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"app").unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let config = config.executable_path(&exe).work_dir(&work);
        crate::updater::DeltaUpdater::new(config)
    }

    #[test]
    fn fallback_policy_classifies_errors() {
        use FallbackPolicy::*;
        assert!(OnTransportError.should_fallback(&DeltaError::Network("x".into())));
        assert!(OnTransportError.should_fallback(&DeltaError::Timeout("x".into())));
        assert!(OnTransportError.should_fallback(&DeltaError::HttpStatus(502)));
        assert!(OnTransportError.should_fallback(&DeltaError::HttpStatus(429)));
        assert!(!OnTransportError.should_fallback(&DeltaError::HttpStatus(404)));
        assert!(!OnTransportError.should_fallback(&DeltaError::SignatureInvalid("x".into())));

        assert!(OnAnyFailure.should_fallback(&DeltaError::HttpStatus(404)));
        assert!(!OnAnyFailure.should_fallback(&DeltaError::ManifestInvalid("x".into())));
    }

    #[test]
    fn insecure_http_is_detected_but_loopback_is_allowed() {
        assert!(endpoint_is_insecure("http://cdn.example.com/latest.json"));
        assert!(!endpoint_is_insecure("https://cdn.example.com/latest.json"));
        assert!(!endpoint_is_insecure("http://127.0.0.1:8080/latest.json"));
        assert!(!endpoint_is_insecure("http://localhost:8080/latest.json"));
        assert!(!endpoint_is_insecure("not a url"));
    }

    #[test]
    fn insecure_transport_can_be_rejected() {
        let err =
            updater(config("http://cdn.example.com/latest.json").allow_insecure_transport(false))
                .unwrap_err();
        assert!(
            matches!(err, DeltaError::Config(_)),
            "非回环 http 必须被拒绝: {err:?}"
        );

        updater(config("http://127.0.0.1:8080/latest.json").allow_insecure_transport(false))
            .expect("回环 http 仍应允许（本地调试）");
    }

    #[test]
    fn invalid_headers_are_rejected() {
        let err = updater(
            config("https://cdn.example.com/latest.json")
                .headers(vec![("bad name".into(), "x".into())]),
        )
        .unwrap_err();
        assert!(matches!(err, DeltaError::Config(_)), "{err:?}");
    }

    #[test]
    fn headers_with_custom_client_are_rejected() {
        let client = crate::core::crypto::client_builder().build().unwrap();
        let err = updater(
            config("https://cdn.example.com/latest.json")
                .client(client)
                .headers(vec![("X-Token".into(), "secret".into())]),
        )
        .unwrap_err();
        assert!(matches!(err, DeltaError::Config(_)), "{err:?}");
    }

    /// `.sig` 必须插在路径末尾、查询参数之前；带 token 的 CDN 最容易在这里出错。
    #[test]
    fn signature_url_keeps_query_parameters() {
        assert_eq!(
            super::default_signature_url("https://cdn.example.com/latest.json"),
            "https://cdn.example.com/latest.json.sig"
        );
        assert_eq!(
            super::default_signature_url("https://cdn.example.com/latest.json?token=abc&v=2"),
            "https://cdn.example.com/latest.json.sig?token=abc&v=2"
        );
    }

    /// `UpdateConfig::new` 必须立即推导出正确的签名地址；builder 可覆盖它。
    #[test]
    fn config_derives_signature_url() {
        let config = UpdateConfig::new("https://cdn.example.com/latest.json?token=abc", "PK");
        assert_eq!(
            config.signature_url,
            "https://cdn.example.com/latest.json.sig?token=abc"
        );

        let config = config.signature_url("https://sig.example.com/latest.json.sig");
        assert_eq!(
            config.signature_url, "https://sig.example.com/latest.json.sig",
            "独立签名地址必须覆盖推导值"
        );
    }
}
