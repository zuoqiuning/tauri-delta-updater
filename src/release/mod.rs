//! 发布期工具：生成签名的更新清单、补丁与发布报告。
//!
//! `generate` 与 `cargo delta release` 都只是本模块的薄入口。发布逻辑只有
//! 这一份实现，两个 CLI 不会各自演化出不一致的行为；需要自定义流程时也可
//! 直接调用 [`run_release`]。

use crate::{calculate_hash, normalize_version, PatchInfo, PlatformInfo, Rollout, UpdateManifest};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::collections::HashMap;
use std::path::PathBuf;

mod baseline;
mod bundles;
mod files;
mod keys;
mod patches;

use baseline::archive_baseline;
use bundles::generate_bundles;
use files::{infer_platform, manifest_names};
use keys::{derive_public_key, load_secret_key};
use patches::{generate_single_patches, retain_chain_patches, PatchContext};

/// 一次发布所需的全部输入。
///
/// 必填项用 [`ReleaseOptions::new`] 构造，其余字段按发布工具的默认值填充后
/// 按需覆盖；字段全部公开，便于调用方直接组装。
#[derive(Debug, Clone)]
pub struct ReleaseOptions {
    /// 补丁基线目录：扫描其中所有历史主程序。可多个，生成时逐个扫描。
    pub old_dirs: Vec<PathBuf>,
    /// 显式指定的历史主程序（不是安装包），与目录扫描结果合并。
    pub old_binaries: Vec<PathBuf>,
    /// 本次发布的主程序，补丁的产物。
    pub app_binary: PathBuf,
    /// 本次发布的完整安装包，用于 `platforms[].url` 与全量下载签名；
    /// 省略时退回 `app_binary`（仅适用于单文件直接分发）。
    pub installer: Option<PathBuf>,
    /// 目标版本号，可带 `v` 前缀。
    pub version: String,
    /// 补丁与安装包的下载前缀。
    pub base_url: String,
    /// 允许 `base_url` 使用明文 HTTP（默认拒绝：客户端默认也不接受）。
    pub allow_http: bool,
    /// minisign 私钥：文件路径、base64 内容或密钥原文。
    pub private_key: String,
    /// 私钥口令（加密私钥时必填）。
    pub password: Option<String>,
    /// 第二把私钥，用于密钥轮换期的双签。
    pub secondary_private_key: Option<String>,
    /// 第二把私钥的口令。
    pub secondary_password: Option<String>,
    /// 多语言更新说明，元素形如 `zh-CN=文本`。
    pub notes_i18n: Vec<String>,
    /// 目标平台的最低系统版本。
    pub min_os: Option<String>,
    /// 上一版清单，用于保留其它平台的条目与补丁链。
    pub existing: Option<PathBuf>,
    /// 目标平台标识（如 `windows-x86_64`）；省略时从文件名推断。
    pub platform: Option<String>,
    /// 灰度放量比例（0–100，100 为全量）。
    pub rollout: u8,
    /// 最低允许版本：低于它的客户端无视灰度强制更新。
    pub min_version: Option<String>,
    /// 发布通道（默认 `stable`）。
    pub channel: String,
    /// 更新说明。
    pub notes: String,
    /// 补丁体积占安装包的比例上限，超过则跳过。
    pub max_patch_ratio: f64,
    /// 补丁 zstd 压缩级别（1–22）。
    pub zstd_level: i32,
    /// 清单/签名/公钥的输出目录，默认当前目录。
    pub output_dir: Option<PathBuf>,
    /// 生成成功后把本次主程序归档为该目录下的下一版基线。
    pub archive_dir: Option<PathBuf>,
    /// 本次发布的应用目录（主程序 + sidecars + resources），启用整包补丁。
    pub new_app_dir: Option<PathBuf>,
    /// 历史版本应用目录（目录名必须包含版本号）。
    pub old_app_dirs: Vec<PathBuf>,
    /// 发布报告（JSON）的输出路径。
    pub report: Option<PathBuf>,
    /// 额外输出 `SHA256SUMS` 的路径。
    pub sha256sums: Option<PathBuf>,
    /// 生成未压缩的 `BSDIFF40` 补丁，供旧引擎客户端消费。
    pub legacy_patch: bool,
    /// 一个补丁都没生成时以错误退出。
    pub fail_on_empty: bool,
    /// 只打印将要执行的操作，不写文件。
    pub dry_run: bool,
}

impl ReleaseOptions {
    /// 只填四个必填项，其余取发布工具的默认值。
    ///
    /// 默认值：`stable` 通道、全量灰度（100）、补丁压缩级别 19、
    /// 仅在补丁没有收益时跳过。
    pub fn new(
        app_binary: impl Into<PathBuf>,
        version: impl Into<String>,
        base_url: impl Into<String>,
        private_key: impl Into<String>,
    ) -> Self {
        Self {
            old_dirs: Vec::new(),
            old_binaries: Vec::new(),
            app_binary: app_binary.into(),
            installer: None,
            version: version.into(),
            base_url: base_url.into(),
            allow_http: false,
            private_key: private_key.into(),
            password: None,
            secondary_private_key: None,
            secondary_password: None,
            notes_i18n: Vec::new(),
            min_os: None,
            existing: None,
            platform: None,
            rollout: 100,
            min_version: None,
            channel: "stable".to_string(),
            notes: String::new(),
            max_patch_ratio: 1.0,
            zstd_level: 19,
            output_dir: None,
            archive_dir: None,
            new_app_dir: None,
            old_app_dirs: Vec::new(),
            report: None,
            sha256sums: None,
            legacy_patch: false,
            fail_on_empty: false,
            dry_run: false,
        }
    }
}

/// 发布成功后可供调用方展示或断言的摘要。
#[derive(Debug, Clone)]
pub struct ReleaseOutputs {
    /// 本次发布的版本号（已规范化）。
    pub version: String,
    /// 本次发布的平台标识。
    pub platform: String,
    /// 清单文件名（如 `latest.json`）。
    pub manifest_file: String,
    /// 该平台的单文件补丁数量。
    pub patch_count: usize,
    /// 被跳过的基线数量。
    pub skipped_count: usize,
    /// 整包补丁数量（未启用整包时为 0）。
    pub bundle_count: usize,
}

/// 让 Windows 控制台以 UTF-8 输出，否则中文日志在 GBK 代码页下会乱码。
pub fn enable_utf8_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
        const CP_UTF8: u32 = 65001;
        unsafe {
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }
    }
}

/// 在独立线程里执行 [`run_release`]。
///
/// 调试构建下异步状态机会保留大量局部变量，Windows 主线程默认 1 MB 栈
/// 会被嵌套的补丁/哈希/解压调用撑爆，因此两个 CLI 都通过本函数运行。
pub fn run_release_blocking(options: ReleaseOptions) -> Result<ReleaseOutputs, String> {
    let worker = std::thread::Builder::new()
        .name("delta-release".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| format!("无法创建 tokio 运行时: {e}"))?;
            rt.block_on(run_release(options))
        })
        .map_err(|e| format!("无法创建发布线程: {e}"))?;
    worker.join().map_err(|_| "发布线程异常退出".to_string())?
}

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
    /// 整包补丁文件名（相对补丁目录）。
    bundle_patch_files: Vec<String>,
    skipped: Vec<SkipRecord>,
}

/// 用一把或多把私钥签名；多钥时按 minisign 块拼接（客户端支持多块验签）。
fn sign_with_keys(
    keys: &[minisign::SecretKey],
    content: &[u8],
    trusted_comment: Option<&str>,
) -> Result<String, String> {
    let mut blocks = Vec::with_capacity(keys.len());
    for key in keys {
        let signature = minisign::sign(
            None,
            key,
            std::io::Cursor::new(content),
            trusted_comment,
            None,
        )
        .map_err(|e| format!("签名失败: {e}"))?;
        blocks.push(signature.to_string());
    }
    Ok(blocks.join("\n"))
}

/// 与 [`sign_with_keys`] 相同，但从文件流式读取内容。
///
/// 安装包可能有几百 MB：`fs::read` 会让发布进程额外驻留同等内存，
/// 在低内存 CI 上可能 OOM。minisign 接受任意 `Read`，直接传 `File` 即可。
fn sign_file_with_keys(
    keys: &[minisign::SecretKey],
    path: &std::path::Path,
    trusted_comment: Option<&str>,
) -> Result<String, String> {
    let mut blocks = Vec::with_capacity(keys.len());
    for key in keys {
        let file = std::fs::File::open(path).map_err(|e| format!("读取安装包失败: {e}"))?;
        let signature = minisign::sign(None, key, file, trusted_comment, None)
            .map_err(|e| format!("签名失败: {e}"))?;
        blocks.push(signature.to_string());
    }
    Ok(blocks.join("\n"))
}

/// 发布阶段共享的、已规范化并校验过的上下文。
struct ReleaseContext {
    output_dir: PathBuf,
    patch_dir: PathBuf,
    new_version: String,
    min_version: Option<String>,
    notes_i18n: HashMap<String, String>,
    channel: String,
    manifest_file: String,
    bundle_file: String,
    signing_keys: Vec<minisign::SecretKey>,
    installer: PathBuf,
    installer_name: String,
    platform_key: String,
    target_os: String,
    target_arch: String,
    new_hash: String,
    installer_size: u64,
}

/// 阶段一：校验参数、规范化版本/通道、加载密钥并计算哈希。
///
/// 所有会失败的环境/参数问题都应在这里返回，后面的阶段只做生成与写盘。
async fn prepare_context(options: &ReleaseOptions) -> Result<ReleaseContext, String> {
    validate(options)?;

    // 输出目录与补丁目录：
    // - 清单/签名/公钥始终写 --output-dir（默认当前目录）；
    // - 只有一个基线目录时沿用旧行为写在它下面，便于它同时充当归档目录；
    //   多个基线目录时无法确定归属，统一写输出目录。
    let output_dir = options
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let patch_dir = match options.old_dirs.as_slice() {
        [single] => single.clone(),
        _ => output_dir.clone(),
    };

    let new_version = normalize_version(&options.version);
    semver::Version::parse(new_version)
        .map_err(|e| format!("版本号 {new_version:?} 不是合法的语义化版本: {e}"))?;
    let min_version = match options.min_version.as_deref() {
        Some(raw) => {
            let normalized = normalize_version(raw);
            semver::Version::parse(normalized)
                .map_err(|e| format!("--min-version {normalized:?} 不是合法的语义化版本: {e}"))?;
            Some(normalized.to_string())
        }
        None => None,
    };
    if let Some(min_os) = &options.min_os {
        if min_os
            .split('.')
            .any(|part| !part.starts_with(|c: char| c.is_ascii_digit()))
        {
            return Err(format!(
                "--min-os 必须是点分数字版本（如 10.0.19045 / 13.0 / 5.15）: {min_os}"
            ));
        }
    }

    let mut notes_i18n = HashMap::new();
    for entry in &options.notes_i18n {
        let (locale, text) = entry.split_once('=').ok_or_else(|| {
            format!("--notes-i18n 必须是 LOCALE=TEXT 形式（如 zh-CN=本次修复了...）：{entry}")
        })?;
        let locale = locale.trim();
        if locale.is_empty() {
            return Err(format!("--notes-i18n 的语言标记不能为空: {entry}"));
        }
        notes_i18n.insert(locale.to_string(), text.to_string());
    }

    // 通道名会进入文件名，只允许安全字符，避免 `../latest` 这类路径穿越。
    let channel = options.channel.trim().to_lowercase();
    if channel.is_empty()
        || !channel
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "--channel {:?} 非法：只允许小写字母、数字、- 与 _",
            options.channel
        ));
    }
    let (manifest_file, bundle_file) = manifest_names(&channel);

    let secret_key = load_secret_key(
        &options.private_key,
        options.password.as_deref().filter(|p| !p.is_empty()),
    )?;
    let secondary_key = match &options.secondary_private_key {
        Some(path) => Some(load_secret_key(
            path,
            options
                .secondary_password
                .as_deref()
                .filter(|p| !p.is_empty()),
        )?),
        None => None,
    };
    let mut signing_keys = vec![secret_key];
    if let Some(secondary) = secondary_key {
        signing_keys.push(secondary);
    }
    let public_key = derive_public_key(&signing_keys[0])?;
    if let Some(secondary_public) = signing_keys.get(1).map(derive_public_key).transpose()? {
        if secondary_public
            .to_box()
            .map_err(|e| e.to_string())?
            .to_string()
            == public_key.to_box().map_err(|e| e.to_string())?.to_string()
        {
            println!("[WARN] --secondary-private-key 与主私钥是同一把密钥，双签没有意义");
        }
    }

    let installer = options
        .installer
        .clone()
        .unwrap_or_else(|| options.app_binary.clone());
    if options.installer.is_none() {
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
    let app_name = options
        .app_binary
        .file_name()
        .ok_or("主程序文件名无效")?
        .to_string_lossy()
        .to_string();

    // 平台标识：显式指定 > 安装包推断 > 主程序推断
    // 绝不默认成 windows-x86_64：那会静默生成跨平台补丁。
    let platform_key = match &options.platform {
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
        .ok_or_else(|| format!("平台标识 {platform_key:?} 不是 os-arch 形式"))?
        .to_string();
    let target_arch = platform_key
        .split_once('-')
        .map(|(_, arch)| arch)
        .unwrap_or("unknown")
        .to_string();

    if let Some(inferred) = infer_platform(&installer_name) {
        if inferred != platform_key {
            println!("[WARN] 安装包文件名推断的平台 ({inferred}) 与目标平台 ({platform_key}) 不一致，以目标平台为准");
        }
    }

    let new_hash = calculate_hash(&options.app_binary)
        .await
        .map_err(|e| format!("计算主程序哈希失败: {e}"))?;

    // 安装包与主程序必须分开：补丁基底是安装后的主程序，全量地址指向安装包。
    // 两者内容相同几乎一定是用错了参数；静默生成会让客户端拿到错误基底。
    if options.installer.is_some() {
        let installer_hash = calculate_hash(&installer)
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
    let installer_size = std::fs::metadata(&installer).map(|m| m.len()).unwrap_or(0);

    Ok(ReleaseContext {
        output_dir,
        patch_dir,
        new_version: new_version.to_string(),
        min_version,
        notes_i18n,
        channel,
        manifest_file,
        bundle_file,
        signing_keys,
        installer,
        installer_name,
        platform_key,
        target_os,
        target_arch,
        new_hash,
        installer_size,
    })
}

/// 阶段二：从旧清单（可选）构造本次清单的骨架。
///
/// 只填版本/说明/平台条目；补丁与签名在后续阶段合并。
fn prepare_manifest(
    options: &ReleaseOptions,
    ctx: &ReleaseContext,
) -> Result<UpdateManifest, String> {
    let mut manifest = match &options.existing {
        Some(path) if path.exists() => {
            let content =
                std::fs::read_to_string(path).map_err(|e| format!("读取旧清单失败: {e}"))?;
            serde_json::from_str(&content).map_err(|e| format!("解析旧清单失败: {e}"))?
        }
        _ => UpdateManifest {
            version: ctx.new_version.clone(),
            notes: String::new(),
            pub_date: chrono::Utc::now().to_rfc3339(),
            min_version: None,
            platforms: HashMap::new(),
            patches: HashMap::new(),
            notes_i18n: HashMap::new(),
            extra: Default::default(),
        },
    };

    // 最低版本按本次发布显式设置；省略即清除上一轮的门槛。
    manifest.min_version = ctx.min_version.clone();

    // 平台条目始终指向本次发布的新安装包（其它平台条目保留）。
    // 安装包体积写进清单（受清单签名保护），客户端据此计算补丁的真实收益比。
    manifest.platforms.insert(
        ctx.platform_key.clone(),
        PlatformInfo {
            signature: String::new(),
            url: format!(
                "{}/{}",
                options.base_url.trim_end_matches('/'),
                ctx.installer_name
            ),
            size: (ctx.installer_size > 0).then_some(ctx.installer_size),
            min_os: options.min_os.clone(),
            // 100 即全量：不写字段，让旧客户端与旧清单保持字节级兼容。
            rollout: (options.rollout < 100).then_some(Rollout {
                percentage: options.rollout,
            }),
        },
    );
    Ok(manifest)
}

/// 生成签名的更新清单、补丁与发布报告。
///
/// 会对每个基线真实应用一遍补丁并校验产物哈希：坏补丁在发布端失败，
/// 而不是在用户侧表现为"更新 → 回滚"循环。
pub async fn run_release(options: ReleaseOptions) -> Result<ReleaseOutputs, String> {
    println!("[INFO] 开始生成更新清单...");

    // 阶段一：校验参数 + 规范化 + 加载密钥 + 哈希。
    let ctx = prepare_context(&options).await?;
    // 阶段二：旧清单骨架。
    let mut manifest = prepare_manifest(&options, &ctx)?;

    // 阶段三：单文件补丁（已包含逐补丁真实应用验证）。
    let (platform_patches, skipped) = generate_single_patches(&PatchContext {
        app_binary: &options.app_binary,
        old_dirs: &options.old_dirs,
        old_binaries: &options.old_binaries,
        new_version: &ctx.new_version,
        target_os: &ctx.target_os,
        target_arch: &ctx.target_arch,
        new_hash: &ctx.new_hash,
        installer_size: ctx.installer_size,
        patch_dir: &ctx.patch_dir,
        base_url: &options.base_url,
        zstd_level: options.zstd_level,
        legacy_patch: options.legacy_patch,
        max_patch_ratio: options.max_patch_ratio,
        dry_run: options.dry_run,
    })
    .await?;

    if let Some(outputs) = preflight_patches(&options, &ctx, &platform_patches, &skipped)? {
        return Ok(outputs);
    }

    // 阶段四：整包补丁（多文件）。与单文件补丁独立，失败不影响已生成的清单内容。
    let bundle_patch_paths = generate_bundles(
        &options,
        &ctx.new_version,
        &ctx.platform_key,
        &ctx.output_dir,
        &ctx.patch_dir,
        &ctx.bundle_file,
        &ctx.signing_keys,
    )
    .await?;

    // 阶段五：合并补丁链、签名并写盘。
    let finalized = finalize_manifest(&options, &ctx, &mut manifest, platform_patches)?;
    write_outputs(&options, &ctx, finalized, bundle_patch_paths, skipped).await
}

/// dry-run 短路与“零补丁”CI 闸门。
///
/// 返回 `Some` 表示无需继续写盘（dry-run）；`Err` 表示发布应中止。
fn preflight_patches(
    options: &ReleaseOptions,
    ctx: &ReleaseContext,
    patches: &[PatchInfo],
    skipped: &[SkipRecord],
) -> Result<Option<ReleaseOutputs>, String> {
    if options.dry_run {
        println!(
            "[DRY] 平台 {}：可用补丁 {} 个，未写任何文件",
            ctx.platform_key,
            patches.len()
        );
        return Ok(Some(ReleaseOutputs {
            version: ctx.new_version.clone(),
            platform: ctx.platform_key.clone(),
            manifest_file: ctx.manifest_file.clone(),
            patch_count: patches.len(),
            skipped_count: skipped.len(),
            bundle_count: 0,
        }));
    }

    // CI 闸门：没有补丁通常是基线没准备好或平台键写错了，
    // 静默发布一份"只能全量下载"的清单会让问题拖到用户侧才暴露。
    // 整包模式下单个补丁为空是正常的，闸门由 generate_bundles 负责。
    if options.fail_on_empty && options.new_app_dir.is_none() && patches.is_empty() {
        return Err(format!(
            "平台 {} 未生成任何补丁：检查基线目录与文件是否包含与目标平台一致的旧主程序",
            ctx.platform_key
        ));
    }
    Ok(None)
}

/// 已签名的清单与报告数据（写入阶段只读）。
struct FinalizedRelease {
    patch_records: Vec<PatchRecord>,
    json_bytes: Vec<u8>,
    signature_text: String,
    patch_count: usize,
}

/// 阶段四：合并补丁链、写入安装包签名并签清单。
fn finalize_manifest(
    options: &ReleaseOptions,
    ctx: &ReleaseContext,
    manifest: &mut UpdateManifest,
    platform_patches: Vec<PatchInfo>,
) -> Result<FinalizedRelease, String> {
    // 报告以本次实际生成的补丁为准；随后才合并历史链。
    let patch_records: Vec<PatchRecord> = platform_patches
        .iter()
        .map(|p| PatchRecord {
            from_version: p.from_version.clone(),
            to_version: p.to_version.clone(),
            patch_file: format!("{}_to_{}.patch", p.from_version, p.to_version),
            patch_size: p.patch_size,
            ratio: p
                .patch_size
                .filter(|_| ctx.installer_size > 0)
                .map(|size| size as f64 / ctx.installer_size as f64),
        })
        .collect();

    // 保留仍能链到新版本的旧补丁：跨多个版本的客户端可以逐跳升级，
    // 而不必下载全量。超出跳数上限或与目标无关的补丁会被裁掉，
    // 清单不会随发布次数无限膨胀。
    let mut all_patches: HashMap<String, Vec<PatchInfo>> = manifest.patches.clone();
    all_patches
        .entry(ctx.platform_key.clone())
        .or_default()
        .extend(platform_patches);
    manifest.patches = retain_chain_patches(all_patches, &ctx.new_version);

    manifest.version = ctx.new_version.clone();
    manifest.pub_date = chrono::Utc::now().to_rfc3339();
    manifest.notes = options.notes.clone();
    manifest.notes_i18n = ctx.notes_i18n.clone();

    // 安装包本身的签名，供 Tauri 官方 updater 校验全量下载。
    // 双签时把多个 minisign 块拼接：客户端支持多块验签。
    // 流式读取：几百 MB 的安装包不能整个读进发布进程内存。
    let installer_signature = sign_file_with_keys(&ctx.signing_keys, &ctx.installer, None)?;
    if let Some(entry) = manifest.platforms.get_mut(&ctx.platform_key) {
        entry.signature = STANDARD.encode(installer_signature.as_bytes());
    }

    // 清单签名覆盖最终内容（含安装包签名），因此必须在所有字段就位后再签名。
    // trusted comment 写入版本绑定（tdu:v=<version>）：用于发现发布端
    // "把 A 版本的签名贴到 B 版本清单"的配置错误；防降级由客户端
    // 的版本单调性比较（is_newer_than）负责。
    let json_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("序列化清单失败: {e}"))?;
    let signature_text = sign_with_keys(
        &ctx.signing_keys,
        &json_bytes,
        Some(&format!("tdu:v={}", ctx.new_version)),
    )?;

    let patch_count = manifest
        .patches
        .get(&ctx.platform_key)
        .map(|patches| patches.len())
        .unwrap_or(0);
    Ok(FinalizedRelease {
        patch_records,
        json_bytes,
        signature_text,
        patch_count,
    })
}

/// 阶段五：写清单/签名/公钥/SHA256SUMS/报告/归档。
async fn write_outputs(
    options: &ReleaseOptions,
    ctx: &ReleaseContext,
    finalized: FinalizedRelease,
    bundle_patch_paths: Vec<PathBuf>,
    mut skipped: Vec<SkipRecord>,
) -> Result<ReleaseOutputs, String> {
    let FinalizedRelease {
        patch_records,
        json_bytes,
        signature_text,
        patch_count,
    } = finalized;
    let bundle_count = bundle_patch_paths.len();
    let manifest_file = &ctx.manifest_file;

    std::fs::create_dir_all(&ctx.output_dir).map_err(|e| format!("创建输出目录失败: {e}"))?;
    std::fs::write(ctx.output_dir.join(manifest_file), &json_bytes)
        .map_err(|e| format!("写入 {manifest_file} 失败: {e}"))?;
    std::fs::write(
        ctx.output_dir.join(format!("{manifest_file}.sig")),
        &signature_text,
    )
    .map_err(|e| format!("写入 {manifest_file}.sig 失败: {e}"))?;

    // 公钥文件只供运维查看；客户端绝不能从服务器读取公钥，
    // 否则攻击者可同时替换清单与公钥。保持 untrusted comment 前缀以兼容解析。
    let mut public_key_texts = Vec::with_capacity(ctx.signing_keys.len());
    for key in &ctx.signing_keys {
        let text = derive_public_key(key)
            .map_err(|e| format!("导出公钥失败: {e}"))?
            .to_box()
            .map_err(|e| format!("导出公钥失败: {e}"))?
            .to_string();
        public_key_texts.push(text.replacen(
            "untrusted comment:",
            "untrusted comment: DO NOT TRUST this file fetched from a server; clients must embed the public key. Original comment:",
            1,
        ));
    }
    std::fs::write(
        ctx.output_dir.join(format!("{manifest_file}.pub")),
        public_key_texts.join("\n"),
    )
    .map_err(|e| format!("写入 {manifest_file}.pub 失败: {e}"))?;

    // 运维核对 CDN 是否齐全：一条命令产出清单、签名、公钥、安装包与补丁的校验和。
    if let Some(path) = &options.sha256sums {
        let mut files = vec![
            ctx.output_dir.join(manifest_file),
            ctx.output_dir.join(format!("{manifest_file}.sig")),
            ctx.output_dir.join(format!("{manifest_file}.pub")),
            ctx.installer.clone(),
        ];
        files.extend(
            patch_records
                .iter()
                .map(|record| ctx.patch_dir.join(&record.patch_file)),
        );
        // 整包补丁也是 CDN 上的独立制品；漏传同样只会在客户端失败。
        files.extend(bundle_patch_paths.iter().cloned());
        write_sha256sums(path, &files).await?;
        println!("[OK] SHA256SUMS: {}", path.display());
    }

    let skipped_count = skipped.len();

    if let Some(path) = &options.report {
        let report = ReleaseReport {
            version: ctx.new_version.clone(),
            platform: ctx.platform_key.clone(),
            installer: ctx.installer_name.clone(),
            installer_size: ctx.installer_size,
            patches: patch_records,
            bundle_patches: bundle_count,
            bundle_patch_files: bundle_patch_paths
                .iter()
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default()
                })
                .collect(),
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

    if let Some(dir) = &options.archive_dir {
        archive_baseline(
            dir,
            &options.app_binary,
            &ctx.new_version,
            &ctx.platform_key,
        )?;
    }

    println!(
        "[OK] {manifest_file} + {manifest_file}.sig 生成成功。通道: {}，平台: {}，补丁 {patch_count} 个，跳过 {skipped_count} 个",
        ctx.channel, ctx.platform_key
    );
    Ok(ReleaseOutputs {
        version: ctx.new_version.clone(),
        platform: ctx.platform_key.clone(),
        manifest_file: ctx.manifest_file.clone(),
        patch_count,
        skipped_count,
        bundle_count,
    })
}
/// 参数范围与输入存在性的统一校验：两个 CLI 共用，避免薄入口各写一遍。
fn validate(options: &ReleaseOptions) -> Result<(), String> {
    // 发布源是客户端的信任入口：明文 HTTP 默认会被客户端拒绝，
    // 必须在生成清单之前失败，而不是等用户更新时才发现。
    let base_url = options.base_url.trim();
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|e| format!("base_url 不是合法 URL（{base_url}）: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("base_url 必须是带主机名的 http(s) URL: {base_url}"));
    }
    if parsed.scheme() == "http" && !options.allow_http {
        return Err(format!(
            "base_url 使用明文 HTTP（{base_url}），客户端默认会拒绝；如确认要使用请加 --allow-http"
        ));
    }

    if !(0.0..=1.0).contains(&options.max_patch_ratio) {
        return Err("--max-patch-ratio 必须在 0.0 到 1.0 之间".into());
    }
    if !(1..=22).contains(&options.zstd_level) {
        return Err("--zstd-level 必须在 1 到 22 之间".into());
    }
    if options.rollout > 100 {
        return Err("--rollout 必须在 0 到 100 之间".into());
    }

    // 输入存在性在最早处验证：拼错路径时不应该静默产出一份"没有补丁"的清单。
    if options.old_dirs.is_empty()
        && options.old_binaries.is_empty()
        && options.old_app_dirs.is_empty()
    {
        return Err("必须提供补丁基线目录（--old-dir / --from）或显式基线文件".into());
    }
    for dir in &options.old_dirs {
        if !dir.is_dir() {
            return Err(format!("补丁基线目录不存在或不是目录: {}", dir.display()));
        }
    }
    for path in &options.old_binaries {
        if !path.is_file() {
            return Err(format!("基线文件不存在: {}", path.display()));
        }
    }
    if !options.app_binary.is_file() {
        return Err(format!(
            "本次发布的主程序不存在: {}",
            options.app_binary.display()
        ));
    }
    if options.new_app_dir.is_some() && options.old_app_dirs.is_empty() {
        return Err("--new-app-dir 需要至少一个 --old-app-dir 作为整包基线".into());
    }
    if let Some(dir) = &options.new_app_dir {
        if !dir.is_dir() {
            return Err(format!("--new-app-dir 不存在或不是目录: {}", dir.display()));
        }
    }
    for dir in &options.old_app_dirs {
        if !dir.is_dir() {
            return Err(format!("--old-app-dir 不存在或不是目录: {}", dir.display()));
        }
    }
    if let Some(installer) = &options.installer {
        if !installer.is_file() {
            return Err(format!("--installer 不存在: {}", installer.display()));
        }
    }
    if let Some(existing) = &options.existing {
        if !existing.is_file() {
            return Err(format!("--existing 不存在: {}", existing.display()));
        }
    }
    Ok(())
}

/// 写 `SHA256SUMS`：每行 `哈希  文件名`，不存在的产物直接跳过。
async fn write_sha256sums(path: &std::path::Path, files: &[PathBuf]) -> Result<(), String> {
    use std::fmt::Write as _;

    let mut text = String::new();
    for file in files {
        if !file.is_file() {
            continue;
        }
        let hash = calculate_hash(file)
            .await
            .map_err(|e| format!("计算 {} 的哈希失败: {e}", file.display()))?;
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact");
        writeln!(text, "{hash}  {name}").map_err(|_| "格式化 SHA256SUMS 失败".to_string())?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 {} 失败: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}
