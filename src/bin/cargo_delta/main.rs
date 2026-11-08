//! `cargo delta`：把接入步骤压成一条命令，默认只输出 diff。

mod args;
mod doctor;
mod project;

use args::{Cli, Command};
use project::{write_atomic, Change, Project};
use similar::TextDiff;

fn main() {
    tauri_delta_updater::release::enable_utf8_console();
    let cli = Cli::parse_cargo();
    if let Err(err) = run(cli) {
        eprintln!("[ERROR] {err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let is_init = matches!(cli.command, Command::Init { .. });
    let is_migrate = matches!(&cli.command, Command::Migrate { .. });
    // 全局参数对 release/doctor 无意义：静默接受会让用户以为它生效了。
    if matches!(&cli.command, Command::Release(_) | Command::Doctor(_)) {
        if cli.apply {
            return Err("--apply 只用于 init / migrate；release 总是写文件，doctor 只读".into());
        }
        if cli.project.is_some() {
            return Err("--project 只用于 init / migrate".into());
        }
        if matches!(&cli.command, Command::Doctor(_)) && cli.dry_run {
            return Err("--dry-run 只用于 init / migrate / release".into());
        }
    }
    let apply = cli.apply;

    let changes = match cli.command {
        Command::Release(args) => {
            let mut options = (*args).into_options()?;
            // 全局 --dry-run 对 release 同样生效，语义与 generate 一致。
            options.dry_run = cli.dry_run;
            tauri_delta_updater::release::run_release_blocking(options)?;
            return Ok(());
        }
        Command::Doctor(args) => return doctor::run(args),
        Command::Init {
            endpoint,
            pubkey,
            bundle_endpoint,
            force,
            registry,
            check,
        } => {
            let project = Project::discover(cli.project.as_deref())?;
            if let Some(name) = project.non_default_capability() {
                println!(
                    "[WARN] 未找到 capabilities/default.json，改用 {name}；确认其 windows 覆盖主窗口。"
                );
            }
            if check {
                if apply {
                    return Err("--check 不能与 --apply 同时使用".into());
                }
                let missing = project.init_status()?;
                if missing.is_empty() {
                    println!("接入配置完整。");
                    if project.uses_official_updater()? {
                        println!("已检测到 plugins.updater：全量回退路径可用（双栈）。");
                    } else {
                        println!(
                            "未检测到 plugins.updater：全量回退由 install_full 负责（单栈）。"
                        );
                    }
                    if project.dependency_is_git()? {
                        println!(
                            "[WARN] 插件依赖仍是 git 形式；插件已发布，重新运行 init --apply 可切到版本依赖（退出码仍为 0）。"
                        );
                    }
                    print_next_steps(true);
                    return Ok(());
                }
                return Err(format!(
                    "接入配置未通过检查，发现以下问题：\n  - {}\n\n修复：cargo delta init --endpoint <URL> --pubkey <KEY> --apply --force",
                    missing.join("\n  - ")
                ));
            }
            // 非 --check 时 clap 已通过 required_unless_present 保证两者存在；
            // 这里仍给出明确错误，避免未来参数约束调整后出现 unwrap 崩溃。
            let endpoint = endpoint.ok_or("--endpoint 必填（--check 除外）")?;
            let pubkey = pubkey.ok_or("--pubkey 必填（--check 除外）")?;
            project.init_changes(
                endpoint.trim(),
                pubkey.trim(),
                bundle_endpoint.as_deref(),
                force,
                registry,
            )?
        }
        Command::Migrate {
            remove_official,
            force,
        } => {
            let project = Project::discover(cli.project.as_deref())?;
            project.migrate_changes(remove_official, force)?
        }
    };

    if changes.is_empty() {
        println!("配置已是最新，无需修改。");
    } else {
        for change in &changes {
            print_diff(change);
        }
        if apply {
            for change in &changes {
                write_atomic(&change.path, &change.after)?;
                println!("已写入 {}", change.path.display());
            }
        } else {
            println!("\n未写入任何文件。确认 diff 后加 --apply 落盘。");
        }
    }

    if is_migrate {
        println!("\n迁移注意：");
        println!(
            "  - fallback_policy 默认 transport：主端点 404 不会切到备用；需要旧行为请设 \"any\""
        );
        println!("  - 前端进度监听需从 callback 改为 Channel 事件（命令签名不同）");
    }

    print_next_steps(is_init);
    Ok(())
}

fn print_diff(change: &Change) {
    let label = change.label();
    let diff = TextDiff::from_lines(&change.before, &change.after);
    print!(
        "{}",
        diff.unified_diff()
            .header(&format!("a/{label}"), &format!("b/{label}"))
    );
}

fn print_next_steps(is_init: bool) {
    println!("\n下一步：");
    println!("  1. 在 src-tauri/src/lib.rs 的 Builder 上注册：");
    println!("     .plugin(tauri_plugin_delta_updater::init_from_config())");
    println!(
        "  2. install() 返回 Updated / RepairRequired 后退出进程，或直接用 install_and_exit()"
    );
    println!("  3. 需要覆盖 sidecar/resources 时，在 tauri.conf.json 配置 bundle_endpoint");
    println!("  4. 上线前在每个目标平台实测一次完整更新：");
    println!("     - Updated 后进程是否真的退出（脚本要等旧 PID 消失）");
    println!("     - 重启后是否被 tauri-plugin-single-instance 判定为第二实例");
    println!("     - 工作目录是否残留 .update.lock / tdx_update.ps1");
    if !is_init {
        println!("  5. 官方插件配置默认保留；完全替代请运行 cargo delta migrate --remove-official --apply");
    }
}
