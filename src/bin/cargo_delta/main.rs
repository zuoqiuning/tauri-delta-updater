//! `cargo delta`：把接入步骤压成一条命令，默认只输出 diff。

mod args;
mod doctor;
mod project;

use args::{Cli, Command};
use project::{write_atomic, Change, Project};
use similar::TextDiff;
use std::path::{Path, PathBuf};

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
            // doctor --from-config 需要项目配置；其余 release/doctor 用法不接受它。
            let from_config = matches!(&cli.command, Command::Doctor(args) if args.from_config);
            if !from_config {
                return Err("--project 只用于 init / migrate / doctor --from-config".into());
            }
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
        Command::Doctor(args) => return doctor::run(args, cli.project.as_deref()),
        Command::Init {
            endpoint,
            pubkey,
            pubkey_file,
            generate_key,
            bundle_endpoint,
            force,
            registry,
            check,
        } => {
            let project = Project::discover(cli.project.as_deref())?;
            if project.looks_like_inno_setup() && !project.installer_kind_configured()? {
                println!(
                    "[WARN] 检测到 Inno Setup 脚本（.iss）：全量安装包需在 plugins.delta-updater 中设置 \"installer_kind\": \"inno\"，否则可能被按 NSIS 参数调用。"
                );
            }
            if let Some(name) = project.non_default_capability() {
                println!(
                    "[WARN] 未找到 capabilities/default.json，改用 {name}；确认其 windows 覆盖主窗口。"
                );
            }
            if check {
                if apply {
                    return Err("--check 不能与 --apply 同时使用".into());
                }
                if generate_key.is_some() {
                    return Err("--check 不能与 --generate-key 同时使用".into());
                }
                let missing = project.init_status()?;
                if missing.is_empty() {
                    println!("接入配置完整。");
                    if project.uses_official_updater()? {
                        if project.has_delta_config()? {
                            println!("已检测到 plugins.updater：全量回退路径可用（双栈）。");
                        } else {
                            println!(
                                "配置来源：plugins.updater（插件会自动回退读取，无需重命名）。"
                            );
                        }
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
            // 非 --check 时必须提供公钥；clap 不再拦截，以便在报错里直接给出
            // 生成密钥的命令（首次接入最常见的卡点）。
            let endpoint = endpoint.ok_or("--endpoint 必填（--check 除外）")?;
            let pubkey = match (pubkey, pubkey_file, generate_key) {
                (Some(key), None, None) => key.trim().to_string(),
                (None, Some(path), None) => read_pubkey_file(&path)?,
                (None, None, Some(path)) => generate_keypair(&path, apply)?,
                (None, None, None) => return Err(
                    "--pubkey、--pubkey-file 或 --generate-key 必须提供一个（--check 除外）。\n\
                         如果还没有密钥对，可以直接生成：\n  \
                         cargo delta init --endpoint <URL> --generate-key myapp.key --apply\n\
                         或者用 minisign 生成：minisign -G -p myapp.pub -s myapp.key\n\
                         私钥不要提交到 git。"
                        .into(),
                ),
                _ => return Err("--pubkey、--pubkey-file 与 --generate-key 只能提供一个".into()),
            };
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
        println!("  - 前端可改用兼容层，避免重写进度事件：");
        println!("      import {{ checkCompat }} from 'tauri-plugin-delta-updater';");
        println!("      const update = await checkCompat();");
        println!(
            "      if (update) await update.downloadAndInstall(/* Started/Progress/Finished */);"
        );
        println!("    需要自定义退出时机时再用 prepare() / launch()。");
    }

    print_next_steps(is_init);
    Ok(())
}

/// 读取公钥文件：原始 `.pub` 内容编码为 base64（官方配置写法）；
/// 已经是 base64 的文本原样使用。
fn read_pubkey_file(path: &Path) -> Result<String, String> {
    use base64::Engine as _;
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取公钥文件 {} 失败: {e}", path.display()))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("公钥文件 {} 为空", path.display()));
    }
    if trimmed.starts_with("untrusted comment:") {
        return Ok(base64::engine::general_purpose::STANDARD.encode(trimmed.as_bytes()));
    }
    Ok(trimmed.to_string())
}

/// 生成一对 minisign 密钥；返回写入配置的 base64 公钥。
///
/// 私钥只在 `--apply` 时落盘，且**永不打印内容**；Unix 下权限为 `0600`。
/// dry-run 仍会在内存中生成密钥，以便展示真实的配置 diff，但不写任何文件。
fn generate_keypair(path: &Path, apply: bool) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let mut public_os = path.as_os_str().to_os_string();
    public_os.push(".pub");
    let public_path = PathBuf::from(public_os);
    if path.exists() {
        return Err(format!("私钥文件已存在，拒绝覆盖: {}", path.display()));
    }
    if public_path.exists() {
        return Err(format!(
            "公钥文件已存在，拒绝覆盖: {}",
            public_path.display()
        ));
    }

    let keypair = minisign::KeyPair::generate_unencrypted_keypair()
        .map_err(|e| format!("生成密钥失败: {e}"))?;
    let secret = keypair
        .sk
        .to_box(None)
        .map_err(|e| format!("序列化私钥失败: {e}"))?
        .into_string();
    let public = keypair
        .pk
        .to_box()
        .map_err(|e| format!("序列化公钥失败: {e}"))?
        .to_string();

    if apply {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建密钥目录 {} 失败: {e}", parent.display()))?;
            }
        }
        write_private_key(path, &secret)?;
        if let Err(e) = std::fs::write(&public_path, &public) {
            // 公钥写失败时不要留下一个孤立私钥，避免用户误以为密钥已就绪。
            let _ = std::fs::remove_file(path);
            return Err(format!("写入公钥 {} 失败: {e}", public_path.display()));
        }
        println!(
            "[OK] 已生成密钥对：\n  私钥: {}\n  公钥: {}\n[WARN] 私钥未加密：请妥善保管并加入 .gitignore，不要提交到版本库。",
            path.display(),
            public_path.display()
        );
    } else {
        println!(
            "[DRY] 将生成密钥对: {} 与 {}（加 --apply 才会落盘）",
            path.display(),
            public_path.display()
        );
    }
    Ok(STANDARD.encode(public.as_bytes()))
}

/// 以只允许当前用户读写的方式创建私钥文件（Unix `0600`）。
fn write_private_key(path: &Path, secret: &str) -> Result<(), String> {
    use std::io::Write as _;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);
    let mut file = file.map_err(|e| format!("创建私钥文件 {} 失败: {e}", path.display()))?;
    file.write_all(secret.as_bytes())
        .map_err(|e| format!("写入私钥 {} 失败: {e}", path.display()))?;
    file.flush()
        .map_err(|e| format!("刷新私钥 {} 失败: {e}", path.display()))
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
