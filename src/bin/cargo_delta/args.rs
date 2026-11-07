//! `cargo delta` 的命令行参数。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "cargo-delta",
    about = "配置 tauri-delta-updater：init / migrate（默认 dry-run）",
    long_about = None,
    after_help = r#"示例：
  cargo delta init --endpoint https://cdn.example.com/latest.json --pubkey RWQ...
  cargo delta init ... --apply
  cargo delta migrate                 # 输出 diff，不写文件
  cargo delta migrate --remove-official --apply

默认只打印 diff；显式加 --apply 才落盘。"#
)]
pub(crate) struct Cli {
    /// 项目根目录（默认当前目录）。
    #[arg(long, global = true)]
    pub(crate) project: Option<PathBuf>,

    /// 只打印 diff，不写文件（默认行为）。
    #[arg(long, global = true, conflicts_with = "apply")]
    pub(crate) dry_run: bool,

    /// 实际写入文件。
    #[arg(long, global = true)]
    pub(crate) apply: bool,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// 为当前项目写入 delta-updater 配置、capability 与依赖。
    Init {
        /// 清单地址（`latest.json`）。
        #[arg(long)]
        endpoint: String,
        /// 固化在应用里的公钥（base64 或 `.pub` 内容）。
        #[arg(long)]
        pubkey: String,
        /// 可选整包清单地址（`latest.bundle.json`）。
        #[arg(long)]
        bundle_endpoint: Option<String>,
        /// 已存在 `plugins.delta-updater` 时覆盖。
        #[arg(long)]
        force: bool,
        /// 保留兼容：插件已发布，默认即写注册表版本依赖。
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
