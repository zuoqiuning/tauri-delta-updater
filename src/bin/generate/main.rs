//! 生成 `latest.json` 与 `latest.json.sig`。
//!
//! 这是发布逻辑的薄入口：参数解析后交给库的
//! `release::run_release_blocking`，与 `cargo delta release` 共用同一份实现。
//!
//! 用法示例：
//!
//! ```text
//! generate \
//!   --old-dir old_binaries \
//!   --app-binary Sereno.exe \
//!   --installer Sereno_0.1.1_x64-setup.exe \
//!   --version 0.1.1 \
//!   --base-url https://cdn.example.com \
//!   --private-key "$TAURI_SIGNING_PRIVATE_KEY"
//! ```
//!
//! # 为什么主程序与安装包要分开
//!
//! 补丁的基底与产物是**安装后的主程序**，而 `platforms[].url` 与
//! `platforms[].signature` 指向**完整安装包**。Tauri 的 NSIS/MSI 安装包
//! 与安装后的 exe 不是同一份字节：把安装包当补丁基底，客户端在真实
//! 安装目录上应用时产物哈希必然不匹配。两个输入必须显式分开。
//!
//! `--installer` 缺省时退回 `--app-binary`（适用于"单文件可执行程序直接分发"
//! 的场景），并打印警告。

use args::Args;
use clap::Parser;
use tauri_delta_updater::release::{enable_utf8_console, run_release_blocking};

mod args;

fn main() {
    enable_utf8_console();
    if let Err(err) = run_release_blocking(Args::parse().into_options()) {
        eprintln!("[ERROR] {err}");
        std::process::exit(1);
    }
}
