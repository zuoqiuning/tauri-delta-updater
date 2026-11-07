//! `cargo delta`：把接入步骤压成一条命令，默认只输出 diff。

mod args;
mod project;

use args::{Cli, Command};
use project::{write_atomic, Change, Project};
use similar::TextDiff;

fn main() {
    let cli = Cli::parse_cargo();
    if let Err(err) = run(cli) {
        eprintln!("[ERROR] {err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let project = Project::discover(cli.project.as_deref())?;
    let is_init = matches!(cli.command, Command::Init { .. });
    let changes = match cli.command {
        Command::Init {
            endpoint,
            pubkey,
            bundle_endpoint,
            force,
            registry,
            check,
        } => {
            if check {
                if cli.apply {
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
                    print_next_steps(true);
                    return Ok(());
                }
                return Err(format!(
                    "接入配置不完整，缺少：\n  - {}",
                    missing.join("\n  - ")
                ));
            }
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
        } => project.migrate_changes(remove_official, force)?,
    };

    if changes.is_empty() {
        println!("配置已是最新，无需修改。");
    } else {
        for change in &changes {
            print_diff(change);
        }
        if cli.apply {
            for change in &changes {
                write_atomic(&change.path, &change.after)?;
                println!("已写入 {}", change.path.display());
            }
        } else {
            println!("\n未写入任何文件。确认 diff 后加 --apply 落盘。");
        }
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
    if !is_init {
        println!("  4. 官方插件配置默认保留；完全替代请运行 cargo delta migrate --remove-official --apply");
    }
}
