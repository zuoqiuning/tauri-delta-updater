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
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use tauri_delta_updater::{
    apply_patch, calculate_hash, create_patch_with_level, detect_arch, normalize_version,
    PatchInfo, PatchScope, PlatformInfo, UpdateManifest,
};
use walkdir::WalkDir;

/// 补丁验证目录的计数器，保证同一进程内多次调用不冲突。
static VERIFY_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Parser)]
#[command(
    name = "generate",
    // 关闭 clap 自带的 --version：本工具用 --version 表示"发布版本号"
    disable_version_flag = true,
    about = "生成 Tauri 增量更新的 latest.json 与补丁",
    long_about = None
)]
struct Args {
    /// 历史版本**主程序**所在目录（不是安装包）。
    #[arg(long)]
    old_dir: PathBuf,

    /// 本次发布的主程序（补丁目标）。
    #[arg(long)]
    app_binary: PathBuf,

    /// 本次发布的完整安装包，用于 platforms.url 与安装包签名。
    ///
    /// 缺省时退回 --app-binary（仅适用于单文件直接分发）。
    #[arg(long)]
    installer: Option<PathBuf>,

    /// 目标版本号，可带 v 前缀（会被规范化掉）。
    #[arg(long)]
    version: String,

    /// 补丁与安装包的下载前缀。
    #[arg(long)]
    base_url: String,

    /// minisign 私钥：文件路径、base64 内容或密钥原文。
    #[arg(long, env = "TAURI_SIGNING_PRIVATE_KEY")]
    private_key: String,

    /// 私钥口令（加密私钥时必填）。
    #[arg(long, env = "TAURI_SIGNING_PRIVATE_KEY_PASSWORD")]
    password: Option<String>,

    /// 上一版 latest.json，用于保留其它平台的补丁与条目。
    #[arg(long)]
    existing: Option<PathBuf>,

    /// 目标平台标识（如 windows-x86_64）；缺省时从文件名推断。
    #[arg(long)]
    platform: Option<String>,

    /// 更新说明。
    #[arg(long, default_value = "")]
    notes: String,

    /// 补丁体积占安装包的比例上限，超过则跳过（0.0–1.0，默认 1.0 仅在无收益时跳过）。
    #[arg(long, default_value_t = 1.0)]
    max_patch_ratio: f64,

    /// 补丁 zstd 压缩级别（1–22，默认 19）。
    ///
    /// 小补丁的体积对级别不敏感；历史版本多、安装包大时可降到 15–17，
    /// 明显缩短 CI 压缩时间，客户端解压成本不变。
    #[arg(long, default_value_t = 19)]
    zstd_level: i32,

    /// latest.json / .sig / .pub 的输出目录，默认当前目录。
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// 只打印将要执行的操作，不写文件。
    #[arg(long)]
    dry_run: bool,
}

/// 从文件名提取版本号，支持 `Sereno_0.1.0_x64-setup.exe` / `Sereno-v0.1.2-arm64.exe` 等格式。
fn extract_version(file_name: &str) -> Option<String> {
    let re = Regex::new(r"[vV]?(\d+\.\d+\.\d+(?:-[\w.]+)?)").ok()?;
    re.captures(file_name)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// 从文件名推断操作系统标识。
fn infer_os(file_name: &str) -> Option<&'static str> {
    let lower = file_name.to_lowercase();
    if lower.ends_with(".exe") || lower.ends_with(".msi") {
        Some("windows")
    } else if lower.ends_with(".app.tar.gz") || lower.ends_with(".dmg") {
        Some("darwin")
    } else if lower.ends_with(".appimage") || lower.ends_with(".deb") || lower.ends_with(".rpm") {
        Some("linux")
    } else {
        None
    }
}

/// 从文件名推断完整平台标识。
///
/// 只推断架构与操作系统；无法识别时返回 `None`，由调用方要求显式 `--platform`，
/// 绝不默认成 `windows-x86_64`——那会静默生成跨平台/跨架构补丁。
fn infer_platform(file_name: &str) -> Option<String> {
    let os = infer_os(file_name)?;
    let arch = detect_arch(file_name);
    if arch == "unknown" {
        return None;
    }
    Some(format!("{os}-{arch}"))
}

/// 判断旧目录里的文件是否是"主程序产物"（不是安装包、补丁、元数据）。
fn is_app_binary_candidate(file_name: &str) -> bool {
    let lower = file_name.to_lowercase();
    const SKIP: &[&str] = &[
        ".patch",
        ".json",
        ".sig",
        ".txt",
        ".md",
        ".log",
        ".msi",
        ".dmg",
        ".pkg",
        ".deb",
        ".rpm",
        ".appimage",
        ".tar",
        ".gz",
        ".xz",
        ".zip",
        ".7z",
        ".bz2",
    ];
    !SKIP.iter().any(|ext| lower.ends_with(ext))
}

/// 读取私钥。
///
/// 支持三种来源，兼容本地开发与 CI 两种用法：
/// - 指向密钥文件的路径
/// - base64 编码的密钥内容（Tauri CI 中 `TAURI_SIGNING_PRIVATE_KEY` 的格式）
/// - 密钥箱文本或原始二进制密钥
///
/// 注意：绝不能调用 `into_secret_key(None)` —— 它会在无 TTY 的环境下
/// 交互式索要口令，导致 CI 流程挂起。这里始终显式传入口令。
fn load_secret_key(source: &str, password: Option<&str>) -> Result<minisign::SecretKey, String> {
    // 1. 路径指向真实文件
    let path = Path::new(source);
    if path.is_file() {
        let raw = std::fs::read(path).map_err(|e| format!("读取私钥失败: {e}"))?;
        return parse_secret_key(&raw, password);
    }

    // 2. base64 编码的内容（CI 常用形式）
    if let Ok(decoded) = STANDARD.decode(source.trim()) {
        if let Ok(key) = parse_secret_key(&decoded, password) {
            return Ok(key);
        }
    }

    // 3. 直接就是密钥内容
    parse_secret_key(source.as_bytes(), password)
}

/// 从原始字节还原私钥。
///
/// 字节可能是 minisign 密钥箱文本（第二行是 base64 负载），
/// 也可能是 158 字节的原始密钥。
fn parse_secret_key(raw: &[u8], password: Option<&str>) -> Result<minisign::SecretKey, String> {
    if let Ok(text) = std::str::from_utf8(raw) {
        if let Some(payload) = base64_payload(text) {
            // 有口令时按密钥箱解密，错误口令会被直接拒绝
            if let Some(pwd) = password {
                return minisign::SecretKeyBox::from_string(text)
                    .map_err(|e| format!("私钥格式错误: {e}"))?
                    .into_secret_key(Some(pwd.to_string()))
                    .map_err(|e| format!("私钥口令错误或私钥损坏: {e}"));
            }

            // 未加密的私钥：负载即原始密钥字节
            if is_unencrypted_payload(&payload) {
                return minisign::SecretKey::from_bytes(&payload)
                    .map_err(|e| format!("无法解析私钥: {e}"));
            }

            // 加密私钥但未提供口令：明确报错，而不是退回原始字节
            // （原始字节解析会得到一个错误的密钥，签名结果无法通过校验）
            return Err("私钥已加密，请提供口令".to_string());
        }
    }

    // 原始二进制私钥
    minisign::SecretKey::from_bytes(raw).map_err(|e| format!("无法解析私钥: {e}"))
}

/// 提取密钥箱文本中的 base64 负载。
fn base64_payload(text: &str) -> Option<Vec<u8>> {
    let line = text.lines().nth(1)?.trim();
    if line.is_empty() {
        return None;
    }
    STANDARD.decode(line).ok()
}

/// 判断密钥负载是否为未加密的原始私钥。
///
/// 依据 minisign 密钥格式（参考 minisign 0.7.x `SecretKey::from_bytes` 的布局）：
/// 负载依次为 sig_alg(2) + kdf_alg(2) + chk_alg(2) + kdf_salt(32) +
/// opslimit(8) + memlimit(8) + keynum(8) + sk(64) + chk(32)，
/// 其中末尾 32 字节是校验和。**未加密**密钥的校验和字段在 `to_box` 前恒为零，
/// 加密时会被 scrypt 流异或为非零值——以此区分两种密钥。
///
/// 注意：这是对 minisign crate 内部布局的经验性依赖。
/// 升级 `minisign` 依赖时必须回归验证：
/// 用 `KeyPair::generate_unencrypted_keypair` 与 `generate_encrypted_keypair`
/// 各生成一份密钥，确认本函数判定结果不变。
fn is_unencrypted_payload(payload: &[u8]) -> bool {
    const CHK_BYTES: usize = 32;
    payload.len() > CHK_BYTES && payload[payload.len() - CHK_BYTES..].iter().all(|b| *b == 0)
}

/// 从私钥导出公钥，供客户端校验签名。
fn derive_public_key(sk: &minisign::SecretKey) -> Result<minisign::PublicKey, String> {
    minisign::PublicKey::from_secret_key(sk).map_err(|e| format!("无法导出公钥: {e}"))
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

    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run(args)) {
        eprintln!("[ERROR] {e}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    println!("[INFO] 开始生成更新清单...");

    // 输入存在性在最早处验证：拼错路径时不应该静默产出一份"没有补丁"的清单
    if !args.old_dir.is_dir() {
        return Err(format!(
            "--old-dir 不存在或不是目录: {}",
            args.old_dir.display()
        ));
    }
    if !args.app_binary.is_file() {
        return Err(format!(
            "--app-binary 不存在: {}",
            args.app_binary.display()
        ));
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
            platforms: HashMap::new(),
            patches: HashMap::new(),
        },
    };

    // 平台条目始终指向本次发布的新安装包（其它平台条目保留）。
    manifest.platforms.insert(
        platform_key.clone(),
        PlatformInfo {
            signature: String::new(),
            url: format!("{}/{installer_name}", args.base_url.trim_end_matches('/')),
        },
    );

    // 生成补丁：只处理与本次发布同平台的旧**主程序**
    let mut platform_patches: Vec<PatchInfo> = Vec::new();
    let installer_size = std::fs::metadata(installer).map(|m| m.len()).unwrap_or(0);

    if args.old_dir.exists() {
        for entry in WalkDir::new(&args.old_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if !is_app_binary_candidate(&file_name) {
                continue;
            }

            // 文件是本次发布的主程序本身时跳过
            if let (Ok(a), Ok(b)) = (
                std::fs::canonicalize(path),
                std::fs::canonicalize(&args.app_binary),
            ) {
                if a == b {
                    continue;
                }
            }

            // 架构不同的主程序不能与新版本互为补丁
            let old_arch = detect_arch(&file_name);
            let target_arch = platform_key
                .split_once('-')
                .map(|(_, a)| a)
                .unwrap_or("unknown");
            if old_arch == "unknown" || old_arch != target_arch {
                println!("[SKIP] 跳过架构不明或不同的旧主程序: {file_name}");
                continue;
            }
            // 有平台信息时还必须与目标操作系统一致
            if let Some(old_os) = infer_os(&file_name) {
                if old_os != target_os {
                    println!("[SKIP] 跳过其它平台的旧主程序: {file_name}");
                    continue;
                }
            }

            let old_version = match extract_version(&file_name) {
                Some(v) => normalize_version(&v).to_string(),
                None => {
                    println!("[WARN] 跳过无法解析版本的文件: {file_name}");
                    continue;
                }
            };
            if old_version == new_version {
                continue;
            }

            let patch_name = format!("{old_version}_to_{new_version}.patch");
            let patch_path = args.old_dir.join(&patch_name);

            if args.dry_run {
                println!("[DRY] 将生成补丁: {old_version} -> {new_version}（{file_name}）");
                continue;
            }

            println!("[INFO] 生成补丁: {old_version} -> {new_version}");
            create_patch_with_level(path, &args.app_binary, &patch_path, args.zstd_level)
                .await
                .map_err(|e| format!("生成补丁失败: {e}"))?;
            let patch_hash = calculate_hash(&patch_path)
                .await
                .map_err(|e| format!("计算补丁哈希失败: {e}"))?;
            let patch_size = std::fs::metadata(&patch_path).map(|m| m.len()).ok();

            // 劣质补丁跳过：补丁接近全量时，下载补丁反而更慢
            if let Some(size) = patch_size {
                if installer_size > 0
                    && (size as f64) > (installer_size as f64) * args.max_patch_ratio
                {
                    println!(
                        "[SKIP] 补丁 {patch_name} 体积 {size} 超过安装包的 {:.0}%，已跳过",
                        args.max_patch_ratio * 100.0
                    );
                    let _ = std::fs::remove_file(&patch_path);
                    continue;
                }
            }

            // 发布端验证：把补丁真实应用一遍，证明它能重建出新版本主程序。
            // "a manifest never describes a patch nobody has proven
            // reconstructs the release"——坏补丁在发布流水线失败，
            // 而不是在用户侧表现为"更新→回滚"循环。apply_patch 内部
            // 会校验产物哈希 == target_hash，任何不符都会报错。
            let verify_dir = std::env::temp_dir().join(format!(
                "tdx-verify-{}-{}",
                std::process::id(),
                VERIFY_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&verify_dir)
                .map_err(|e| format!("创建补丁验证目录失败: {e}"))?;
            let verify_out = verify_dir.join("reconstructed");
            let verify_result =
                apply_patch(path, &patch_path, &verify_out, &patch_hash, &new_hash).await;
            let _ = std::fs::remove_file(&verify_out);
            let _ = std::fs::remove_dir_all(&verify_dir);
            verify_result.map_err(|e| {
                format!("补丁 {patch_name} 无法从旧主程序重建出新版本（发布中止）: {e}")
            })?;
            println!("[OK] 补丁验证通过: {patch_name}（{patch_size:?} 字节）");

            platform_patches.push(PatchInfo {
                from_version: old_version,
                to_version: new_version.to_string(),
                patch_url: format!("{}/{patch_name}", args.base_url.trim_end_matches('/')),
                patch_hash,
                target_hash: new_hash.clone(),
                patch_size,
                // 补丁基底与产物都是主程序，显式标注覆盖范围。
                // 使用 sidecar/resources 的应用据此提示"本次增量不覆盖附加资源"。
                scope: Some(PatchScope::MainExecutableOnly),
            });
        }
    }

    // 同一 from_version 只保留最新的一份补丁（按语义化版本排序）
    platform_patches.sort_by(|a, b| {
        semver::Version::parse(&a.from_version)
            .ok()
            .cmp(&semver::Version::parse(&b.from_version).ok())
    });
    platform_patches.dedup_by(|a, b| a.from_version == b.from_version);

    if args.dry_run {
        println!(
            "[DRY] 平台 {platform_key}：可用补丁 {} 个，未写任何文件",
            platform_patches.len()
        );
        return Ok(());
    }

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

    let output_dir = args.output_dir.unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&output_dir).map_err(|e| format!("创建输出目录失败: {e}"))?;
    std::fs::write(output_dir.join("latest.json"), &json_bytes)
        .map_err(|e| format!("写入 latest.json 失败: {e}"))?;
    std::fs::write(output_dir.join("latest.json.sig"), &signature_text)
        .map_err(|e| format!("写入 latest.json.sig 失败: {e}"))?;

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
    std::fs::write(output_dir.join("latest.json.pub"), public_key_text)
        .map_err(|e| format!("写入 latest.json.pub 失败: {e}"))?;

    println!(
        "[OK] latest.json + latest.json.sig 生成成功。平台: {platform_key}，补丁总数: {}",
        manifest
            .patches
            .get(&platform_key)
            .map(|p| p.len())
            .unwrap_or(0)
    );
    Ok(())
}
