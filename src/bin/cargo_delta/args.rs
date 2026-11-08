//! `cargo delta` 的命令行参数。

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tauri_delta_updater::release::ReleaseOptions;

#[derive(Parser)]
#[command(
    name = "cargo-delta",
    about = "配置与发布 tauri-delta-updater：init / migrate / release",
    long_about = None,
    after_help = r#"示例：
  cargo delta init --endpoint https://cdn.example.com/latest.json --pubkey RWQ...
  cargo delta init ... --apply
  cargo delta migrate                 # 输出 diff，不写文件
  cargo delta migrate --remove-official --apply
  cargo delta release --from old_binaries --to 0.3.0 \
    --app-binary target/release/app.exe --installer dist/app_0.3.0_x64-setup.exe \
    --base-url https://cdn.example.com/releases

init / migrate 默认只打印 diff；显式加 --apply 才落盘。
release 直接生成清单与补丁，--dry-run 只打印计划。"#
)]
pub(crate) struct Cli {
    /// 项目根目录（init / migrate 使用，默认当前目录）。
    #[arg(long, global = true)]
    pub(crate) project: Option<PathBuf>,

    /// 只打印将要执行的操作，不写文件（init / migrate 的默认行为）。
    #[arg(long, global = true, conflicts_with = "apply")]
    pub(crate) dry_run: bool,

    /// 实际写入文件（init / migrate）。
    #[arg(long, global = true)]
    pub(crate) apply: bool,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// 为当前项目写入 delta-updater 配置、capability 与依赖。
    Init {
        /// 清单地址（`latest.json`）。`--check` 时从项目现有配置读取，无需提供。
        #[arg(long, required_unless_present = "check")]
        endpoint: Option<String>,
        /// 固化在应用里的公钥（base64 或 `.pub` 内容）。`--check` 时无需提供。
        #[arg(long, required_unless_present = "check")]
        pubkey: Option<String>,
        /// 可选整包清单地址（`latest.bundle.json`）。
        #[arg(long)]
        bundle_endpoint: Option<String>,
        /// 已存在 `plugins.delta-updater` 时覆盖。
        #[arg(long)]
        force: bool,
        /// 写精确版本依赖（`"0.3.0"`）；默认为 `"0.3"`，允许同 minor 内补丁升级。
        #[arg(long)]
        registry: bool,
        /// 只检查配置是否完整，不打印 diff；缺失时以非零状态退出。
        #[arg(long)]
        check: bool,
    },
    /// 从 `plugins.updater` 迁移到 `plugins.delta-updater`。
    Migrate {
        /// 同时删除官方插件配置与 `updater:default` 权限。
        #[arg(long)]
        remove_official: bool,
        /// 已存在 `plugins.delta-updater` 时覆盖。
        #[arg(long)]
        force: bool,
    },
    /// 生成签名的更新清单、补丁与发布报告。
    Release(Box<ReleaseArgs>),
    /// 体检已发布的更新源：清单签名、安装包、补丁与补丁链。
    Doctor(DoctorArgs),
}

/// `cargo delta doctor` 的参数。
#[derive(clap::Args, Debug)]
pub(crate) struct DoctorArgs {
    /// 清单地址（`latest.json`）。
    #[arg(value_name = "URL")]
    pub(crate) endpoint: String,

    /// 受信任公钥（可重复）：base64 或 `.pub` 文件内容。
    #[arg(long = "pubkey", value_name = "KEY", required = true)]
    pub(crate) pubkey: Vec<String>,

    /// 可选：整包清单地址（`latest.bundle.json`），校验整包补丁与文件清单。
    #[arg(long = "bundle-url", value_name = "URL")]
    pub(crate) bundle_url: Option<String>,

    /// 下载安装包并校验 minisign 签名（较慢，但能证明全量回退真的可用）。
    #[arg(long)]
    pub(crate) deep: bool,

    /// 机器可读报告（JSON）的输出路径。
    #[arg(long, value_name = "PATH")]
    pub(crate) json: Option<PathBuf>,

    /// 单次请求超时（秒，默认 30）。
    #[arg(long, default_value_t = 30)]
    pub(crate) timeout: u64,

    /// 附加请求头（可重复）：`--header Name=Value`，用于私有 CDN Token。
    #[arg(long = "header", value_name = "NAME=VALUE")]
    pub(crate) headers: Vec<String>,

    /// 多钥信任策略：`any`（默认，任意一把有效签名）或 `all`（必须全部签名）。
    #[arg(long = "trust-policy", value_name = "any|all", default_value = "any")]
    pub(crate) trust_policy: String,

    /// 接受旧版（legacy）minisign 签名。
    ///
    /// 客户端默认拒绝；开启后体检结果可能与真实客户端不一致。
    #[arg(long = "allow-legacy")]
    pub(crate) allow_legacy: bool,
}

/// `cargo delta release` 的参数：与 `generate` 等价，基线用 `--from` 统一表达。
#[derive(clap::Args, Debug)]
pub(crate) struct ReleaseArgs {
    /// 补丁基线（可重复）：目录会扫描其中所有历史主程序，文件则必须是可识别的主程序。
    #[arg(long = "from", value_name = "PATH")]
    pub(crate) from: Vec<PathBuf>,

    /// 目标版本号，可带 v 前缀。
    #[arg(long = "to", value_name = "VERSION")]
    pub(crate) to: String,

    /// 本次发布的主程序（补丁目标）。
    #[arg(long = "app-binary", value_name = "PATH")]
    pub(crate) app_binary: PathBuf,

    /// 本次发布的完整安装包，用于全量下载地址与签名；省略时退回主程序。
    #[arg(long, value_name = "PATH")]
    pub(crate) installer: Option<PathBuf>,

    /// 补丁与安装包的下载前缀。
    #[arg(long = "base-url", value_name = "URL")]
    pub(crate) base_url: String,

    /// minisign 私钥：文件路径、base64 内容或密钥原文。
    #[arg(long = "private-key", env = "TAURI_SIGNING_PRIVATE_KEY")]
    pub(crate) private_key: String,

    /// 私钥口令（加密私钥时必填）。
    #[arg(long, env = "TAURI_SIGNING_PRIVATE_KEY_PASSWORD")]
    pub(crate) password: Option<String>,

    /// 第二把私钥，用于密钥轮换期的双签。
    #[arg(long = "secondary-private-key", value_name = "PATH")]
    pub(crate) secondary_private_key: Option<String>,

    /// 第二把私钥的口令。
    #[arg(long = "secondary-password")]
    pub(crate) secondary_password: Option<String>,

    /// 多语言更新说明（可重复）：`--notes-i18n zh-CN=本次修复了 ...`。
    #[arg(long = "notes-i18n", value_name = "LOCALE=TEXT")]
    pub(crate) notes_i18n: Vec<String>,

    /// 目标平台的最低系统版本。
    #[arg(long = "min-os", value_name = "VERSION")]
    pub(crate) min_os: Option<String>,

    /// 上一版清单，用于保留其它平台的条目与补丁链。
    #[arg(long, value_name = "PATH")]
    pub(crate) existing: Option<PathBuf>,

    /// 目标平台标识（如 windows-x86_64）；缺省时从文件名推断。
    #[arg(long)]
    pub(crate) platform: Option<String>,

    /// 灰度放量比例（0–100，默认 100 = 全量）。
    #[arg(long, default_value_t = 100)]
    pub(crate) rollout: u8,

    /// 最低允许版本：低于它的客户端无视灰度强制更新。
    #[arg(long = "min-version")]
    pub(crate) min_version: Option<String>,

    /// 发布通道（默认 stable）。
    #[arg(long, default_value = "stable")]
    pub(crate) channel: String,

    /// 更新说明。
    #[arg(long, default_value = "")]
    pub(crate) notes: String,

    /// 补丁体积占安装包的比例上限，超过则跳过（0.0–1.0，默认 1.0）。
    #[arg(long = "max-patch-ratio", default_value_t = 1.0)]
    pub(crate) max_patch_ratio: f64,

    /// 补丁 zstd 压缩级别（1–22，默认 19）。
    #[arg(long = "zstd-level", default_value_t = 19)]
    pub(crate) zstd_level: i32,

    /// 清单/签名/公钥的输出目录，默认当前目录。
    #[arg(long = "output-dir")]
    pub(crate) output_dir: Option<PathBuf>,

    /// 生成成功后把本次主程序归档为该目录下的下一版基线。
    #[arg(long = "archive-dir")]
    pub(crate) archive_dir: Option<PathBuf>,

    /// 本次发布的应用目录（主程序 + sidecars + resources），启用整包补丁。
    #[arg(long = "new-app-dir")]
    pub(crate) new_app_dir: Option<PathBuf>,

    /// 历史版本应用目录（可重复，目录名必须包含版本号）。
    #[arg(long = "old-app-dir", value_name = "DIR")]
    pub(crate) old_app_dirs: Vec<PathBuf>,

    /// 发布报告（JSON）的输出路径。
    #[arg(long)]
    pub(crate) report: Option<PathBuf>,

    /// 额外输出 `SHA256SUMS` 的路径。
    #[arg(long, value_name = "PATH")]
    pub(crate) sha256sums: Option<PathBuf>,

    /// 生成未压缩的 `BSDIFF40` 补丁，供旧引擎客户端消费。
    #[arg(long = "legacy-patch")]
    pub(crate) legacy_patch: bool,

    /// 一个补丁都没生成时以非零状态退出。
    #[arg(long = "fail-on-empty")]
    pub(crate) fail_on_empty: bool,
}

impl ReleaseArgs {
    /// 映射为库的发布选项；`--from` 按实际类型拆成基线目录或基线文件。
    pub(crate) fn into_options(self) -> Result<ReleaseOptions, String> {
        let mut old_dirs = Vec::new();
        let mut old_binaries = Vec::new();
        for path in self.from {
            if path.is_dir() {
                old_dirs.push(path);
            } else if path.is_file() {
                old_binaries.push(path);
            } else {
                return Err(format!("--from 不存在: {}", path.display()));
            }
        }

        let mut options =
            ReleaseOptions::new(self.app_binary, self.to, self.base_url, self.private_key);
        options.old_dirs = old_dirs;
        options.old_binaries = old_binaries;
        options.installer = self.installer;
        options.password = self.password;
        options.secondary_private_key = self.secondary_private_key;
        options.secondary_password = self.secondary_password;
        options.notes_i18n = self.notes_i18n;
        options.min_os = self.min_os;
        options.existing = self.existing;
        options.platform = self.platform;
        options.rollout = self.rollout;
        options.min_version = self.min_version;
        options.channel = self.channel;
        options.notes = self.notes;
        options.max_patch_ratio = self.max_patch_ratio;
        options.zstd_level = self.zstd_level;
        options.output_dir = self.output_dir;
        options.archive_dir = self.archive_dir;
        options.new_app_dir = self.new_app_dir;
        options.old_app_dirs = self.old_app_dirs;
        options.report = self.report;
        options.sha256sums = self.sha256sums;
        options.legacy_patch = self.legacy_patch;
        options.fail_on_empty = self.fail_on_empty;
        Ok(options)
    }
}

impl Cli {
    /// Cargo 以 `cargo-delta delta <subcommand>` 调用子命令；直接运行时没有前缀。
    pub(crate) fn parse_cargo() -> Self {
        let mut args: Vec<String> = std::env::args().collect();
        if args.get(1).is_some_and(|arg| arg == "delta") {
            args.remove(1);
        }
        Self::parse_from(args)
    }
}
