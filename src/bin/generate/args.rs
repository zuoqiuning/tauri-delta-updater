//! 命令行参数。

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "generate",
    // 关闭 clap 自带的 --version：本工具用 --version 表示"发布版本号"
    disable_version_flag = true,
    about = "生成 Tauri 增量更新的 latest.json 与补丁",
    long_about = None,
    after_help = r#"示例（私钥从 TAURI_SIGNING_PRIVATE_KEY 读取）：
  generate --old-dir ./old_binaries --app-binary target/release/app.exe \
           --installer dist/app_0.2.0_x64-setup.exe --version 0.2.0 \
           --base-url https://cdn.example.com/releases/0.2.0 \
           --archive-dir ./old_binaries --report release-report.json
产物：latest.json、latest.json.sig 与补丁文件。
--archive-dir 会把本次主程序归档为下一次发布的基线；--report 输出机器可读的补丁结果。"#
)]
pub(crate) struct Args {
    /// 历史版本**主程序**所在目录（不是安装包）。
    ///
    /// 与 `--old-binary` 至少提供一个；两者可以同时使用。
    #[arg(long)]
    pub(crate) old_dir: Option<PathBuf>,

    /// 额外指定一个历史主程序（可重复），与 `--old-dir` 合并。
    ///
    /// 适合 CI 里产物分散在不同目录的场景；显式指定的文件校验失败会直接报错。
    #[arg(long = "old-binary", value_name = "PATH")]
    pub(crate) old_binary: Vec<PathBuf>,

    /// 本次发布的主程序（补丁目标）。
    #[arg(long)]
    pub(crate) app_binary: PathBuf,

    /// 本次发布的完整安装包，用于 platforms.url 与安装包签名。
    ///
    /// 缺省时退回 --app-binary（仅适用于单文件直接分发）。
    #[arg(long)]
    pub(crate) installer: Option<PathBuf>,

    /// 目标版本号，可带 v 前缀（会被规范化掉）。
    #[arg(long)]
    pub(crate) version: String,

    /// 补丁与安装包的下载前缀。
    #[arg(long)]
    pub(crate) base_url: String,

    /// minisign 私钥：文件路径、base64 内容或密钥原文。
    #[arg(long, env = "TAURI_SIGNING_PRIVATE_KEY")]
    pub(crate) private_key: String,

    /// 私钥口令（加密私钥时必填）。
    #[arg(long, env = "TAURI_SIGNING_PRIVATE_KEY_PASSWORD")]
    pub(crate) password: Option<String>,

    /// 第二把 minisign 私钥：用于密钥轮换期的双签（清单与安装包都会双签）。
    ///
    /// 客户端需同时固化新旧两把公钥（`trust_policy = any` 即可）。
    #[arg(long, value_name = "PATH")]
    pub(crate) secondary_private_key: Option<String>,

    /// 第二把私钥的口令。
    #[arg(long)]
    pub(crate) secondary_password: Option<String>,

    /// 上一版 latest.json，用于保留其它平台的补丁与条目。
    #[arg(long)]
    pub(crate) existing: Option<PathBuf>,

    /// 目标平台标识（如 windows-x86_64）；缺省时从文件名推断。
    #[arg(long)]
    pub(crate) platform: Option<String>,

    /// 灰度放量比例（0–100，默认 100 = 全量）。
    ///
    /// 小于 100 时写入受清单签名保护的 `platforms[].rollout`，
    /// 客户端按稳定哈希分桶；0 表示暂停发布。
    #[arg(long, default_value_t = 100)]
    pub(crate) rollout: u8,

    /// 最低允许版本：低于它的客户端无视灰度比例强制更新。
    ///
    /// 用于把用户从已知坏版本上拉走；省略表示本次发布不设门槛。
    #[arg(long)]
    pub(crate) min_version: Option<String>,

    /// 发布通道（默认 `stable`）。
    ///
    /// 非 stable 时产物命名为 `latest-<channel>.json` / `latest-<channel>.bundle.json`，
    /// 客户端把 manifest_url 指向对应文件即可：灰度就是"先发 beta，再把它当成 stable 发布"。
    #[arg(long, default_value = "stable")]
    pub(crate) channel: String,

    /// 更新说明。
    #[arg(long, default_value = "")]
    pub(crate) notes: String,

    /// 补丁体积占安装包的比例上限，超过则跳过（0.0–1.0，默认 1.0 仅在无收益时跳过）。
    #[arg(long, default_value_t = 1.0)]
    pub(crate) max_patch_ratio: f64,

    /// 补丁 zstd 压缩级别（1–22，默认 19）。
    ///
    /// 小补丁的体积对级别不敏感；历史版本多、安装包大时可降到 15–17，
    /// 明显缩短 CI 压缩时间，客户端解压成本不变。
    #[arg(long, default_value_t = 19)]
    pub(crate) zstd_level: i32,

    /// latest.json / .sig / .pub 的输出目录，默认当前目录。
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,

    /// 生成成功后，把本次主程序归档到该目录作为下一版基线。
    ///
    /// 文件名已含版本与架构时原样保留，否则补成 `{名称}_{版本}_{架构}{后缀}`。
    /// 下一次发布时直接把这个目录当作 `--old-dir`，基线就会自动滚动。
    #[arg(long)]
    pub(crate) archive_dir: Option<PathBuf>,

    /// 本次发布的应用目录（主程序 + sidecars + resources）。
    ///
    /// 提供后额外生成整包补丁与已签名的 `latest.bundle.json`（含完整文件清单），
    /// 适用于需要覆盖 sidecar/resources 的应用。
    #[arg(long)]
    pub(crate) new_app_dir: Option<PathBuf>,

    /// 历史版本应用目录（可重复）。目录名必须包含版本号（如 `v0.1.0`）。
    #[arg(long = "old-app-dir", value_name = "DIR")]
    pub(crate) old_app_dirs: Vec<PathBuf>,

    /// 发布报告（JSON）的输出路径：每个基线的补丁体积、占比与跳过原因。
    #[arg(long)]
    pub(crate) report: Option<PathBuf>,

    /// 一个补丁都没生成时以非零状态退出（防止 CI 静默发布空清单）。
    #[arg(long)]
    pub(crate) fail_on_empty: bool,

    /// 只打印将要执行的操作，不写文件。
    #[arg(long)]
    pub(crate) dry_run: bool,
}
