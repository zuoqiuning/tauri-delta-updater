//! 生成 `latest.json` 与 `latest.json.sig`。
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

use base64::{engine::general_purpose::STANDARD, Engine as _};
use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;
use tauri_delta_updater::{
    calculate_hash, normalize_version, PatchInfo, PlatformInfo, Rollout, UpdateManifest,
};

use args::Args;
use baseline::archive_baseline;
use bundles::generate_bundles;
use files::{infer_platform, manifest_names};
use keys::{derive_public_key, load_secret_key};
use patches::{generate_single_patches, PatchContext};

mod args;
mod baseline;
mod bundles;
mod files;
mod keys;
mod patches;

/// 单个基线的补丁生成结果。
#[derive(serde::Serialize)]
struct PatchRecord {
    from_version: String,
    to_version: String,
    patch_file: String,
    patch_size: Option<u64>,
    /// 补丁体积占安装包的比例；安装包大小未知时为 `None`。
    ratio: Option<f64>,
}

/// 被跳过的基线。
#[derive(serde::Serialize)]
struct SkipRecord {
    file: String,
    reason: String,
}

/// 发布报告：给 CI/运维看的机器可读结果。
#[derive(serde::Serialize)]
struct ReleaseReport {
    version: String,
    platform: String,
    installer: String,
    installer_size: u64,
    /// 单文件（主程序）补丁数量。
    patches: Vec<PatchRecord>,
    /// 整包补丁数量（未启用 `--new-app-dir` 时为 0）。
    bundle_patches: usize,
    skipped: Vec<SkipRecord>,
}

/// 让 Windows 控制台以 UTF-8 输出，否则中文信息在 GBK 代码页下会变成乱码。
#[cfg(windows)]
fn enable_utf8_console() {
    use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
    const CP_UTF8: u32 = 65001;
    unsafe {
        SetConsoleOutputCP(CP_UTF8);
        SetConsoleCP(CP_UTF8);
    }
}

#[cfg(not(windows))]
fn enable_utf8_console() {}

fn main() {
    enable_utf8_console();

    let args = Args::parse();
    if !(0.0..=1.0).contains(&args.max_patch_ratio) {
        eprintln!("[ERROR] --max-patch-ratio 必须在 0.0 到 1.0 之间");
        std::process::exit(2);
    }
    if !(1..=22).contains(&args.zstd_level) {
        eprintln!("[ERROR] --zstd-level 必须在 1 到 22 之间");
        std::process::exit(2);
    }
    if args.rollout > 100 {
        eprintln!("[ERROR] --rollout 必须在 0 到 100 之间");
        std::process::exit(2);
    }

    // 调试构建下 run() 的 Future 会比发行构建大得多（未优化时局部变量都留在状态机里），
    // Windows 主线程默认只有 1 MB 栈，嵌套的补丁/哈希/解压 Future 可能溢出。
    // 用独立线程运行并给足栈空间；panic 仍会打印到同一终端。
    let worker = std::thread::Builder::new()
        .name("generate".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("无法创建 tokio 运行时");
            match rt.block_on(run(args)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("[ERROR] {e}");
                    1
                }
            }
        })
        .expect("无法创建 generate 运行线程");
    let code = worker.join().unwrap_or(1);
    if code != 0 {
        std::process::exit(code);
    }
}

async fn run(args: Args) -> Result<(), String> {
    println!("[INFO] 开始生成更新清单...");

    // 输入存在性在最早处验证：拼错路径时不应该静默产出一份"没有补丁"的清单
    if args.old_dir.is_none() && args.old_binary.is_empty() && args.old_app_dirs.is_empty() {
        return Err("必须提供 --old-dir、--old-binary 或 --old-app-dir 作为补丁基线".into());
    }
    if let Some(dir) = &args.old_dir {
        if !dir.is_dir() {
            return Err(format!("--old-dir 不存在或不是目录: {}", dir.display()));
        }
    }
    for path in &args.old_binary {
        if !path.is_file() {
            return Err(format!("--old-binary 不存在: {}", path.display()));
        }
    }
    if !args.app_binary.is_file() {
        return Err(format!(
            "--app-binary 不存在: {}",
            args.app_binary.display()
        ));
    }

    // 输出目录与补丁目录：
    // - 清单/签名/公钥始终写 --output-dir（默认当前目录）；
    // - 补丁沿用旧行为写在 --old-dir 下；没有 --old-dir 时落到输出目录。
    let output_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let patch_dir = args.old_dir.clone().unwrap_or_else(|| output_dir.clone());

    if args.new_app_dir.is_some() && args.old_app_dirs.is_empty() {
        return Err("--new-app-dir 需要至少一个 --old-app-dir 作为整包基线".into());
    }
    if let Some(dir) = &args.new_app_dir {
        if !dir.is_dir() {
            return Err(format!("--new-app-dir 不存在或不是目录: {}", dir.display()));
        }
    }
    for dir in &args.old_app_dirs {
        if !dir.is_dir() {
            return Err(format!("--old-app-dir 不存在或不是目录: {}", dir.display()));
        }
    }
    if let Some(installer) = &args.installer {
        if !installer.is_file() {
            return Err(format!("--installer 不存在: {}", installer.display()));
        }
    }
    if let Some(existing) = &args.existing {
        if !existing.is_file() {
            return Err(format!("--existing 不存在: {}", existing.display()));
        }
    }

    // 版本号规范化：去掉开头的 `v`。
    // 发布流程里版本号常来自 Git tag（`v0.1.1`），
    // 而客户端上报的是 `CARGO_PKG_VERSION`（`0.1.1`），
    // 不统一的话补丁永远匹配不上。
    let new_version = normalize_version(&args.version);
    semver::Version::parse(new_version)
        .map_err(|e| format!("版本号 {new_version:?} 不是合法的语义化版本: {e}"))?;
    let min_version = match args.min_version.as_deref() {
        Some(raw) => {
            let normalized = normalize_version(raw);
            semver::Version::parse(normalized)
                .map_err(|e| format!("--min-version {normalized:?} 不是合法的语义化版本: {e}"))?;
            Some(normalized.to_string())
        }
        None => None,
    };

    // 通道名会进入文件名，只允许安全字符，避免 `../latest` 这类路径穿越。
    let channel = args.channel.trim().to_lowercase();
    if channel.is_empty()
        || !channel
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "--channel {:?} 非法：只允许小写字母、数字、- 与 _",
            args.channel
        ));
    }
    let (manifest_file, bundle_file) = manifest_names(&channel);

    let secret_key = load_secret_key(
        &args.private_key,
        args.password.as_deref().filter(|p| !p.is_empty()),
    )?;
    let public_key = derive_public_key(&secret_key)?;

    let installer = args.installer.as_deref().unwrap_or(&args.app_binary);
    if args.installer.is_none() {
        println!(
            "[WARN] 未提供 --installer：全量下载地址将指向主程序而不是安装包；\
             仅适用于单文件直接分发的应用"
        );
    }

    let installer_name = installer
        .file_name()
        .ok_or("安装包文件名无效")?
        .to_string_lossy()
        .to_string();
    let app_name = args
        .app_binary
        .file_name()
        .ok_or("主程序文件名无效")?
        .to_string_lossy()
        .to_string();

    // 平台标识：显式指定 > 安装包推断 > 主程序推断
    // 绝不默认成 windows-x86_64：那会静默生成跨平台补丁。
    let platform_key = match &args.platform {
        Some(p) => p.clone(),
        None => infer_platform(&installer_name)
            .or_else(|| infer_platform(&app_name))
            .ok_or_else(|| {
                format!(
                    "无法从文件名推断平台（{installer_name} / {app_name}），请显式指定 --platform"
                )
            })?,
    };
    let target_os = platform_key
        .split_once('-')
        .map(|(os, _)| os)
        .ok_or_else(|| format!("平台标识 {platform_key:?} 不是 os-arch 形式"))?;

    if let Some(inferred) = infer_platform(&installer_name) {
        if inferred != platform_key {
            println!("[WARN] 安装包文件名推断的平台 ({inferred}) 与目标平台 ({platform_key}) 不一致，以目标平台为准");
        }
    }

    let new_hash = calculate_hash(&args.app_binary)
        .await
        .map_err(|e| format!("计算主程序哈希失败: {e}"))?;

    // 安装包与主程序必须分开：补丁基底是安装后的主程序，全量地址指向安装包。
    // 两者内容相同几乎一定是用错了参数；静默生成会让客户端拿到错误基底。
    if args.installer.is_some() {
        let installer_hash = calculate_hash(installer)
            .await
            .map_err(|e| format!("计算安装包哈希失败: {e}"))?;
        if installer_hash == new_hash {
            return Err(
                "--installer 与 --app-binary 的内容相同：安装包必须是完整的安装程序，\
                 不能指向主程序本身（否则补丁基底与全量下载地址都会错）"
                    .into(),
            );
        }
    }

    let mut manifest = match &args.existing {
        Some(path) if path.exists() => {
            let content =
                std::fs::read_to_string(path).map_err(|e| format!("读取旧清单失败: {e}"))?;
            serde_json::from_str(&content).map_err(|e| format!("解析旧清单失败: {e}"))?
        }
        _ => UpdateManifest {
            version: new_version.to_string(),
            notes: String::new(),
            pub_date: chrono::Utc::now().to_rfc3339(),
            min_version: None,
            platforms: HashMap::new(),
            patches: HashMap::new(),
        },
    };

    // 最低版本按本次发布显式设置；省略即清除上一轮的门槛。
    manifest.min_version = min_version;

    // 平台条目始终指向本次发布的新安装包（其它平台条目保留）。
    // 安装包体积写进清单（受清单签名保护），客户端据此计算补丁的真实收益比。
    let installer_size = std::fs::metadata(installer).map(|m| m.len()).unwrap_or(0);
    manifest.platforms.insert(
        platform_key.clone(),
        PlatformInfo {
            signature: String::new(),
            url: format!("{}/{installer_name}", args.base_url.trim_end_matches('/')),
            size: (installer_size > 0).then_some(installer_size),
            // 100 即全量：不写字段，让旧客户端与旧清单保持字节级兼容。
            rollout: (args.rollout < 100).then_some(Rollout {
                percentage: args.rollout,
            }),
        },
    );

    // 生成补丁：只处理与本次发布同平台的旧**主程序**
    let target_arch = platform_key
        .split_once('-')
        .map(|(_, a)| a)
        .unwrap_or("unknown");
    let (platform_patches, mut skipped) = generate_single_patches(&PatchContext {
        app_binary: &args.app_binary,
        old_dir: args.old_dir.as_deref(),
        old_binary: &args.old_binary,
        new_version,
        target_os,
        target_arch,
        new_hash: &new_hash,
        installer_size,
        patch_dir: &patch_dir,
        base_url: &args.base_url,
        zstd_level: args.zstd_level,
        max_patch_ratio: args.max_patch_ratio,
        dry_run: args.dry_run,
    })
    .await?;

    if args.dry_run {
        println!(
            "[DRY] 平台 {platform_key}：可用补丁 {} 个，未写任何文件",
            platform_patches.len()
        );
        return Ok(());
    }

    // CI 闸门：没有补丁通常是基线没准备好或平台键写错了，
    // 静默发布一份"只能全量下载"的清单会让问题拖到用户侧才暴露。
    // 整包模式下单个补丁为空是正常的，闸门由 generate_bundles 负责。
    if args.fail_on_empty && args.new_app_dir.is_none() && platform_patches.is_empty() {
        return Err(format!(
            "平台 {platform_key} 未生成任何补丁：检查 --old-dir / --old-binary 是否包含与目标平台一致的旧主程序"
        ));
    }

    // 报告以最终清单中的补丁为准（去重后的结果），避免报告与实际发布不一致。
    let patch_records: Vec<PatchRecord> = platform_patches
        .iter()
        .map(|p| PatchRecord {
            from_version: p.from_version.clone(),
            to_version: p.to_version.clone(),
            patch_file: format!("{}_to_{}.patch", p.from_version, p.to_version),
            patch_size: p.patch_size,
            ratio: p
                .patch_size
                .filter(|_| installer_size > 0)
                .map(|size| size as f64 / installer_size as f64),
        })
        .collect();

    // 整包补丁（多文件）：与单文件补丁独立，失败不影响已生成的清单内容。
    let bundle_count = generate_bundles(
        &args,
        new_version,
        &platform_key,
        &output_dir,
        &patch_dir,
        &bundle_file,
        &secret_key,
    )
    .await?;

    // 只保留目标版本与本次发布一致的补丁。
    // 上一轮遗留的补丁指向旧版本，既无法使用，也会让清单不断膨胀。
    let mut all_patches: HashMap<String, Vec<PatchInfo>> = manifest
        .patches
        .clone()
        .into_iter()
        .filter_map(|(platform, patches)| {
            let kept: Vec<PatchInfo> = patches
                .into_iter()
                .filter(|p| p.to_version == new_version)
                .collect();
            (!kept.is_empty()).then_some((platform, kept))
        })
        .collect();

    all_patches.insert(platform_key.clone(), platform_patches);
    manifest.patches = all_patches;

    manifest.version = new_version.to_string();
    manifest.pub_date = chrono::Utc::now().to_rfc3339();
    manifest.notes = args.notes.clone();

    // 安装包本身的签名，供 Tauri 官方 updater 校验全量下载
    let installer_bytes = std::fs::read(installer).map_err(|e| format!("读取安装包失败: {e}"))?;
    let installer_signature = minisign::sign(
        None,
        &secret_key,
        std::io::Cursor::new(&installer_bytes),
        None,
        None,
    )
    .map_err(|e| format!("签名安装包失败: {e}"))?
    .to_string();

    if let Some(entry) = manifest.platforms.get_mut(&platform_key) {
        entry.signature = STANDARD.encode(installer_signature.as_bytes());
    }

    // 清单签名覆盖最终内容（含安装包签名），因此必须在所有字段就位后再签名。
    // trusted comment 写入版本绑定（tdu:v=<version>）：用于发现发布端
    // "把 A 版本的签名贴到 B 版本清单"的配置错误；防降级由客户端
    // 的版本单调性比较（is_newer_than）负责。
    let json_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("序列化清单失败: {e}"))?;
    let signature_text = minisign::sign(
        None,
        &secret_key,
        std::io::Cursor::new(&json_bytes),
        Some(&format!("tdu:v={new_version}")),
        None,
    )
    .map_err(|e| format!("签名失败: {e}"))?
    .to_string();

    std::fs::create_dir_all(&output_dir).map_err(|e| format!("创建输出目录失败: {e}"))?;
    std::fs::write(output_dir.join(&manifest_file), &json_bytes)
        .map_err(|e| format!("写入 {manifest_file} 失败: {e}"))?;
    std::fs::write(
        output_dir.join(format!("{manifest_file}.sig")),
        &signature_text,
    )
    .map_err(|e| format!("写入 {manifest_file}.sig 失败: {e}"))?;

    let public_key_text = public_key
        .to_box()
        .map_err(|e| format!("导出公钥失败: {e}"))?
        .to_string();
    // 加警告注释：这个文件只供运维查看，客户端绝不能从服务器读取公钥，
    // 否则攻击者可同时替换清单与公钥。保持 untrusted comment 前缀以兼容解析。
    let public_key_text = public_key_text.replacen(
        "untrusted comment:",
        "untrusted comment: DO NOT TRUST this file fetched from a server; clients must embed the public key. Original comment:",
        1,
    );
    std::fs::write(
        output_dir.join(format!("{manifest_file}.pub")),
        public_key_text,
    )
    .map_err(|e| format!("写入 {manifest_file}.pub 失败: {e}"))?;

    let patch_count = manifest
        .patches
        .get(&platform_key)
        .map(|p| p.len())
        .unwrap_or(0);
    let skipped_count = skipped.len();

    if let Some(path) = &args.report {
        let report = ReleaseReport {
            version: new_version.to_string(),
            platform: platform_key.clone(),
            installer: installer_name.clone(),
            installer_size,
            patches: patch_records,
            bundle_patches: bundle_count,
            skipped: std::mem::take(&mut skipped),
        };
        let json =
            serde_json::to_vec_pretty(&report).map_err(|e| format!("序列化发布报告失败: {e}"))?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败: {e}"))?;
        }
        std::fs::write(path, json).map_err(|e| format!("写入发布报告失败: {e}"))?;
        println!("[OK] 发布报告: {}", path.display());
    }

    if let Some(dir) = &args.archive_dir {
        archive_baseline(dir, &args.app_binary, new_version, &platform_key)?;
    }

    println!(
        "[OK] {manifest_file} + {manifest_file}.sig 生成成功。通道: {channel}，平台: {platform_key}，补丁 {patch_count} 个，跳过 {skipped_count} 个"
    );
    Ok(())
}
