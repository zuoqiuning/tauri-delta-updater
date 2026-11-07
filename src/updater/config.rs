//! 增量更新配置。

use crate::platform::target::current_platform;
use std::path::{Path, PathBuf};

/// 增量更新的配置。
///
/// 除 `manifest_url` 与 `public_key` 外都有合理默认值，
/// 通常只需要设置这两项。
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// `latest.json` 的下载地址。
    ///
    /// # 注意
    ///
    /// [`Self::signature_url`] 在 [`Self::new`] 里由它推导。
    /// **构造之后再手动修改本字段，必须同步修改 `signature_url`**，
    /// 否则签名会从旧地址拉取，导致验签拿到不匹配的文件。
    pub manifest_url: String,
    /// `latest.json.sig` 的下载地址。
    pub signature_url: String,
    /// 用于校验清单签名的公钥。
    ///
    /// 必须是**固化在应用里**的公钥，不能从服务器下载。
    pub public_key: String,
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
        }
    }

    /// 设置当前版本号。
    pub fn current_version(mut self, version: impl Into<String>) -> Self {
        self.current_version = version.into();
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
}

/// 由清单地址推导签名地址。
///
/// 在路径末尾追加 `.sig`，并**保留查询参数**：
/// `https://cdn/latest.json?token=abc` 应得到
/// `https://cdn/latest.json.sig?token=abc`，
/// 而不是把 `.sig` 追加到 token 后面。
pub(crate) fn default_signature_url(manifest_url: &str) -> String {
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
    use super::{default_signature_url, UpdateConfig};

    /// `.sig` 必须插在路径末尾、查询参数之前；带 token 的 CDN 最容易在这里出错。
    #[test]
    fn signature_url_keeps_query_parameters() {
        assert_eq!(
            default_signature_url("https://cdn.example.com/latest.json"),
            "https://cdn.example.com/latest.json.sig"
        );
        assert_eq!(
            default_signature_url("https://cdn.example.com/latest.json?token=abc&v=2"),
            "https://cdn.example.com/latest.json.sig?token=abc&v=2"
        );
    }

    /// `UpdateConfig::new` 必须立即推导出正确的签名地址。
    #[test]
    fn config_derives_signature_url() {
        let config = UpdateConfig::new("https://cdn.example.com/latest.json?token=abc", "PK");
        assert_eq!(
            config.signature_url,
            "https://cdn.example.com/latest.json.sig?token=abc"
        );
    }
}
