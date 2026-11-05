//! TLS 加密后端初始化。
//!
//! 本库的 TLS 后端固定为 `ring`（而不是 `aws-lc-sys`）：
//! 后者需要 cmake + C 工具链，在部分 Windows 环境会导致构建长时间挂起。
//!
//! 但 `rustls` 要求进程内显式安装一个加密后端，否则创建 HTTP 客户端会直接 panic。
//! 这里在每个入口函数里懒安装，**不在库加载期做全局构造**——
//! 库不应该在宿主进程启动时抢先安装进程级后端，否则宿主想用 aws-lc 时
//! 会发现默认后端已被占用。

use std::sync::Once;

static INSTALL: Once = Once::new();

/// 安装进程级 TLS 加密后端。
///
/// 重复调用是安全的：若宿主或其它库已经安装过后端，这里不会覆盖它。
///
/// # 什么时候需要手动调用
///
/// 本库的 TLS 后端固定为 `ring`。使用 `reqwest` 的 `rustls-no-provider` 特性时，
/// **创建 `reqwest::Client` 之前必须先安装一个加密后端**，否则会 panic。
///
/// - 通过 [`crate::DeltaUpdater`] 使用时**无需关心**，它在每次请求前自动安装。
/// - 自己构造 `reqwest::Client` 时，请在创建前调用一次本函数：
///
/// ```no_run
/// tauri_delta_updater::install_crypto_provider();
/// let client = reqwest::Client::builder()
///     .proxy(reqwest::Proxy::all("http://127.0.0.1:7890")?)
///     .user_agent("MyApp/1.0")
///     .timeout(std::time::Duration::from_secs(30))
///     .build()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn install_crypto_provider() {
    INSTALL.call_once(|| {
        // 失败说明已有后端被安装，忽略即可
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// 构建一个已安装好加密后端的 `reqwest::Client` 构建器。
///
/// 宿主想注入自定义客户端（代理/UA/超时）时，用它代替
/// `reqwest::Client::builder()`，可以省掉手动初始化这一行，
/// 也不会忘记调用 [`install_crypto_provider`] 而在 `.build()` 时 panic。
///
/// ```
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = tauri_delta_updater::client_builder()
///     .user_agent("MyApp/1.0")
///     .timeout(std::time::Duration::from_secs(30))
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub fn client_builder() -> reqwest::ClientBuilder {
    install_crypto_provider();
    reqwest::Client::builder()
}
