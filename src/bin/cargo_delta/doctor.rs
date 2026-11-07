//! 发布源体检：清单签名、安装包可下载性、补丁完整性与补丁链可达性。
//!
//! 体检在发布后、用户更新前运行：它证明"线上桶里真的有客户端需要的全部字节"，
//! 而不是等客户端下载失败才发现漏传了补丁或清单。

use crate::args::DoctorArgs;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri_delta_updater::{
    client_builder, manifest_version_binding, normalize_version, verify_file_signature_multi,
    verify_manifest_signature_multi, BundleFile, BundleManifest, PatchInfo, TrustPolicy,
    UpdateManifest, MAX_DELTA_CHAIN_HOPS,
};

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024 * 1024;
const MAX_BUNDLE_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_PATCH_BYTES: u64 = 1024 * 1024 * 1024;

/// 执行体检并决定退出码：有任何错误时返回 `Err`。
pub(crate) fn run(args: DoctorArgs) -> Result<(), String> {
    let worker = std::thread::Builder::new()
        .name("delta-doctor".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| format!("无法创建 tokio 运行时: {e}"))?;
            rt.block_on(check(args))
        })
        .map_err(|e| format!("无法创建体检线程: {e}"))?;
    let report = worker
        .join()
        .map_err(|_| "体检线程异常退出".to_string())??;
    report.print();
    if let Some(path) = &report.json_path {
        report.write_json(path)?;
    }
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "更新源体检未通过：{} 个错误，{} 个警告",
            report.errors.len(),
            report.warnings.len()
        ))
    }
}

struct Report {
    endpoint: String,
    version: Option<String>,
    checks: usize,
    errors: Vec<String>,
    warnings: Vec<String>,
    json_path: Option<PathBuf>,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    endpoint: &'a str,
    version: Option<&'a str>,
    checks: usize,
    errors: &'a [String],
    warnings: &'a [String],
    ok: bool,
}

impl Report {
    fn ok(&mut self, message: impl Into<String>) {
        self.checks += 1;
        println!("[OK] {}", message.into());
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.checks += 1;
        let message = message.into();
        println!("[WARN] {message}");
        self.warnings.push(message);
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.checks += 1;
        let message = message.into();
        println!("[FAIL] {message}");
        self.errors.push(message);
    }

    fn print(&self) {
        println!(
            "[{}] 体检完成：{} 项检查，{} 个错误，{} 个警告",
            if self.errors.is_empty() { "OK" } else { "FAIL" },
            self.checks,
            self.errors.len(),
            self.warnings.len()
        );
    }

    fn write_json(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&JsonReport {
            endpoint: &self.endpoint,
            version: self.version.as_deref(),
            checks: self.checks,
            errors: &self.errors,
            warnings: &self.warnings,
            ok: self.errors.is_empty(),
        })
        .map_err(|e| format!("序列化体检报告失败: {e}"))?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败: {e}"))?;
        }
        std::fs::write(path, json).map_err(|e| format!("写入体检报告失败: {e}"))?;
        println!("[OK] 体检报告: {}", path.display());
        Ok(())
    }
}

async fn check(args: DoctorArgs) -> Result<Report, String> {
    let mut report = Report {
        endpoint: args.endpoint.clone(),
        version: None,
        checks: 0,
        errors: Vec::new(),
        warnings: Vec::new(),
        json_path: args.json.clone(),
    };

    if !is_http_url(&args.endpoint) {
        report.fail(format!("清单地址必须是 http(s) URL: {}", args.endpoint));
        return Ok(report);
    }
    if args.endpoint.starts_with("http://") && !is_loopback(&args.endpoint) {
        report.warn(format!(
            "清单使用明文 HTTP: {}（生产环境应使用 HTTPS）",
            args.endpoint
        ));
    }

    let client = build_client(&args)?;

    let Some(manifest_bytes) = fetch_or_fail(
        &client,
        &args.endpoint,
        MAX_MANIFEST_BYTES,
        "清单",
        &mut report,
    )
    .await
    else {
        return Ok(report);
    };
    report.ok(format!(
        "清单可下载（{} 字节）: {}",
        manifest_bytes.len(),
        args.endpoint
    ));

    let manifest: UpdateManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => {
            report.ok("清单 JSON 结构可解析");
            manifest
        }
        Err(err) => {
            report.fail(format!("清单 JSON 解析失败: {err}"));
            return Ok(report);
        }
    };
    report.version = Some(manifest.version.clone());

    if semver::Version::parse(normalize_version(&manifest.version)).is_ok() {
        report.ok(format!("目标版本 {} 是合法语义化版本", manifest.version));
    } else {
        report.fail(format!(
            "目标版本 {:?} 不是合法语义化版本",
            manifest.version
        ));
    }
    if let Some(min_version) = &manifest.min_version {
        if semver::Version::parse(normalize_version(min_version)).is_ok() {
            report.ok(format!("最低版本 {min_version} 是合法语义化版本"));
        } else {
            report.fail(format!("min_version {min_version:?} 不是合法语义化版本"));
        }
    }

    let keys: Vec<&str> = args.pubkey.iter().map(String::as_str).collect();
    check_manifest_signature(
        &client,
        &args.endpoint,
        &manifest_bytes,
        &manifest,
        &keys,
        &mut report,
    )
    .await;

    check_platforms(&client, &manifest, &keys, args.deep, &mut report).await;
    check_patches(&client, &manifest, &mut report).await;

    if let Some(bundle_url) = &args.bundle_url {
        check_bundle(&client, &manifest, bundle_url, &keys, &mut report).await;
    }

    Ok(report)
}

fn build_client(args: &DoctorArgs) -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    for raw in &args.headers {
        let (name, value) = raw
            .split_once('=')
            .ok_or_else(|| format!("--header 必须是 NAME=VALUE 形式: {raw}"))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|e| format!("请求头名称非法（{name}）: {e}"))?;
        let value = HeaderValue::from_str(value.trim())
            .map_err(|e| format!("请求头 {name} 的值非法: {e}"))?;
        headers.insert(name, value);
    }
    client_builder()
        .timeout(Duration::from_secs(args.timeout))
        .default_headers(headers)
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

async fn check_manifest_signature(
    client: &reqwest::Client,
    endpoint: &str,
    manifest_bytes: &[u8],
    manifest: &UpdateManifest,
    keys: &[&str],
    report: &mut Report,
) {
    let sig_url = format!("{endpoint}.sig");
    let Some(signature_bytes) =
        fetch_or_fail(client, &sig_url, MAX_SIGNATURE_BYTES, "清单签名", report).await
    else {
        return;
    };
    let signature = match String::from_utf8(signature_bytes) {
        Ok(text) => text,
        Err(_) => {
            report.fail(format!("清单签名不是 UTF-8 文本: {sig_url}"));
            return;
        }
    };

    match verify_manifest_signature_multi(
        manifest_bytes,
        &signature,
        keys,
        TrustPolicy::AnyOf,
        true,
    ) {
        Ok(()) => report.ok("清单签名有效（任一受信任公钥）"),
        Err(err) => report.fail(format!("清单签名校验失败: {err}")),
    }

    match manifest_version_binding(&signature) {
        Some(bound) if normalize_version(&bound) == manifest.version => {
            report.ok(format!("签名版本绑定一致（tdu:v={bound}）"));
        }
        Some(bound) => report.fail(format!(
            "签名绑定版本 {bound:?} 与清单版本 {:?} 不一致：签名可能贴错了清单",
            manifest.version
        )),
        None => report.warn("签名没有版本绑定（tdu:v=…），无法发现版本与清单漂移"),
    }
}

async fn check_platforms(
    client: &reqwest::Client,
    manifest: &UpdateManifest,
    keys: &[&str],
    deep: bool,
    report: &mut Report,
) {
    if manifest.platforms.is_empty() {
        report.fail("清单没有任何 platforms 条目：客户端拿不到全量下载地址");
    }

    for (platform, info) in &manifest.platforms {
        if !is_http_url(&info.url) {
            report.fail(format!(
                "平台 {platform} 的安装包地址不是 http(s) URL: {}",
                info.url
            ));
            continue;
        }
        if info.signature.trim().is_empty() {
            report.fail(format!("平台 {platform} 的安装包缺少 signature"));
        }

        match remote_size(client, &info.url).await {
            Ok(Some(size)) => match info.size {
                Some(expected) if size != expected => report.fail(format!(
                    "平台 {platform} 安装包体积 {size} 与清单声明的 {expected} 不一致"
                )),
                Some(_) => report.ok(format!("平台 {platform} 安装包可下载，体积与清单一致")),
                None => report.ok(format!(
                    "平台 {platform} 安装包可下载（{size} 字节；清单未声明体积）"
                )),
            },
            Ok(None) => report.warn(format!(
                "平台 {platform} 的服务器未返回安装包长度，无法核对清单里的 size"
            )),
            Err(err) => report.fail(format!("平台 {platform} 安装包不可用: {err}")),
        }

        if deep && !info.signature.trim().is_empty() {
            match download_temp(client, &info.url).await {
                Ok(path) => {
                    let verified = verify_file_signature_multi(
                        &path,
                        &info.signature,
                        keys,
                        TrustPolicy::AnyOf,
                    );
                    let _ = std::fs::remove_file(&path);
                    match verified {
                        Ok(()) => report.ok(format!("平台 {platform} 安装包签名有效")),
                        Err(err) => {
                            report.fail(format!("平台 {platform} 安装包签名校验失败: {err}"))
                        }
                    }
                }
                Err(err) => report.fail(format!("平台 {platform} 安装包下载失败: {err}")),
            }
        }
    }
}

async fn check_patches(client: &reqwest::Client, manifest: &UpdateManifest, report: &mut Report) {
    if manifest.patches.is_empty() {
        report.warn("清单没有任何增量补丁：所有客户端只能下载完整安装包");
        return;
    }

    for (platform, patches) in &manifest.patches {
        if !manifest.platforms.contains_key(platform) {
            report.warn(format!("补丁分组 {platform} 没有对应的安装包条目"));
        }
        if patches.is_empty() {
            report.warn(format!("平台 {platform} 的补丁列表为空"));
            continue;
        }

        let mut seen = HashSet::new();
        for patch in patches {
            if !is_http_url(&patch.patch_url) {
                report.fail(format!(
                    "补丁 {:?} -> {:?} 的地址不是 http(s) URL: {}",
                    patch.from_version, patch.to_version, patch.patch_url
                ));
                continue;
            }
            if !is_sha256_hex(&patch.patch_hash) || !is_sha256_hex(&patch.target_hash) {
                report.fail(format!(
                    "补丁 {:?} -> {:?} 的哈希不是 64 位十六进制",
                    patch.from_version, patch.to_version
                ));
                continue;
            }
            let from = semver::Version::parse(normalize_version(&patch.from_version));
            let to = semver::Version::parse(normalize_version(&patch.to_version));
            match (from, to) {
                (Ok(from), Ok(to)) if from < to => {}
                _ => {
                    report.fail(format!(
                        "补丁版本区间非法: {:?} -> {:?}",
                        patch.from_version, patch.to_version
                    ));
                    continue;
                }
            }
            if !seen.insert((patch.from_version.clone(), patch.to_version.clone())) {
                report.fail(format!(
                    "平台 {platform} 存在重复补丁: {} -> {}",
                    patch.from_version, patch.to_version
                ));
            }

            match remote_hash(client, &patch.patch_url).await {
                Ok((actual_hash, actual_size)) => {
                    if actual_hash != patch.patch_hash.to_lowercase() {
                        report.fail(format!(
                            "补丁 {} -> {} 的 SHA-256 与清单不一致（线上文件可能被替换或漏传）",
                            patch.from_version, patch.to_version
                        ));
                    } else if patch.patch_size.is_some_and(|size| size != actual_size) {
                        report.fail(format!(
                            "补丁 {} -> {} 的体积 {actual_size} 与清单声明的 {} 不一致",
                            patch.from_version,
                            patch.to_version,
                            patch.patch_size.unwrap_or_default()
                        ));
                    } else {
                        report.ok(format!(
                            "补丁 {} -> {} 可下载且哈希匹配（{actual_size} 字节）",
                            patch.from_version, patch.to_version
                        ));
                    }
                }
                Err(err) => report.fail(format!(
                    "补丁 {} -> {} 不可用: {err}",
                    patch.from_version, patch.to_version
                )),
            }
        }

        let unreachable = unreachable_start_versions(patches, &manifest.version);
        if !unreachable.is_empty() {
            report.warn(format!(
                "平台 {platform} 的补丁无法在 {} 跳内到达 {}：{}（客户端用不到，建议清理）",
                MAX_DELTA_CHAIN_HOPS,
                manifest.version,
                unreachable.join(", ")
            ));
        }
        if !patches
            .iter()
            .any(|patch| normalize_version(&patch.to_version) == manifest.version)
        {
            report.warn(format!(
                "平台 {platform} 没有直达 {} 的补丁：所有客户端都会走全量下载",
                manifest.version
            ));
        }
    }
}

async fn check_bundle(
    client: &reqwest::Client,
    manifest: &UpdateManifest,
    bundle_url: &str,
    keys: &[&str],
    report: &mut Report,
) {
    if !is_http_url(bundle_url) {
        report.fail(format!("整包清单地址不是 http(s) URL: {bundle_url}"));
        return;
    }
    let Some(bytes) = fetch_or_fail(
        client,
        bundle_url,
        MAX_BUNDLE_MANIFEST_BYTES,
        "整包清单",
        report,
    )
    .await
    else {
        return;
    };
    let bundle: BundleManifest = match serde_json::from_slice(&bytes) {
        Ok(bundle) => bundle,
        Err(err) => {
            report.fail(format!("整包清单 JSON 解析失败: {err}"));
            return;
        }
    };

    let sig_url = format!("{bundle_url}.sig");
    let Some(sig_bytes) = fetch_or_fail(
        client,
        &sig_url,
        MAX_SIGNATURE_BYTES,
        "整包清单签名",
        report,
    )
    .await
    else {
        return;
    };
    match String::from_utf8(sig_bytes) {
        Ok(signature) => match verify_manifest_signature_multi(
            &bytes,
            &signature,
            keys,
            TrustPolicy::AnyOf,
            true,
        ) {
            Ok(()) => report.ok("整包清单签名有效"),
            Err(err) => report.fail(format!("整包清单签名校验失败: {err}")),
        },
        Err(_) => report.fail(format!("整包清单签名不是 UTF-8 文本: {sig_url}")),
    }

    if normalize_version(&bundle.version) != manifest.version {
        report.fail(format!(
            "整包清单版本 {:?} 与主清单 {:?} 不一致",
            bundle.version, manifest.version
        ));
    } else {
        report.ok(format!("整包清单版本与主清单一致（{}）", bundle.version));
    }
    if !manifest.platforms.contains_key(&bundle.platform) {
        report.warn(format!(
            "整包清单平台 {} 在主清单里没有安装包条目",
            bundle.platform
        ));
    }

    let mut seen = HashSet::new();
    for patch in &bundle.bundles {
        if !seen.insert(patch.from_version.clone()) {
            report.fail(format!("整包清单存在重复基线版本: {}", patch.from_version));
        }
        if normalize_version(&patch.to_version) != bundle.version {
            report.fail(format!(
                "整包补丁 {} -> {} 的目标版本与清单 {} 不一致",
                patch.from_version, patch.to_version, bundle.version
            ));
        }
        if !is_http_url(&patch.patch_url) {
            report.fail(format!(
                "整包补丁 {} 的地址不是 http(s) URL: {}",
                patch.from_version, patch.patch_url
            ));
            continue;
        }
        if !is_sha256_hex(&patch.patch_hash) || !is_sha256_hex(&patch.target_hash) {
            report.fail(format!(
                "整包补丁 {} 的哈希不是 64 位十六进制",
                patch.from_version
            ));
        }
        check_bundle_files(&patch.base_files, &patch.from_version, "base_files", report);
        check_bundle_files(
            &patch.target_files,
            &patch.from_version,
            "target_files",
            report,
        );

        match remote_hash(client, &patch.patch_url).await {
            Ok((actual_hash, actual_size)) => {
                if actual_hash != patch.patch_hash.to_lowercase() {
                    report.fail(format!(
                        "整包补丁 {} 的 SHA-256 与清单不一致",
                        patch.from_version
                    ));
                } else if patch.patch_size.is_some_and(|size| size != actual_size) {
                    report.fail(format!(
                        "整包补丁 {} 的体积 {actual_size} 与清单声明的 {} 不一致",
                        patch.from_version,
                        patch.patch_size.unwrap_or_default()
                    ));
                } else {
                    report.ok(format!(
                        "整包补丁 {} 可下载且哈希匹配（{actual_size} 字节）",
                        patch.from_version
                    ));
                }
            }
            Err(err) => report.fail(format!("整包补丁 {} 不可用: {err}", patch.from_version)),
        }
    }
}

fn check_bundle_files(files: &[BundleFile], version: &str, field: &str, report: &mut Report) {
    for file in files {
        if !is_safe_bundle_path(&file.path) {
            report.fail(format!(
                "整包补丁 {version} 的 {field} 含不安全路径: {:?}",
                file.path
            ));
        }
        if !is_sha256_hex(&file.sha256) {
            report.fail(format!(
                "整包补丁 {version} 的 {field} 条目 {:?} 哈希非法",
                file.path
            ));
        }
    }
}

/// 从目标版本反向搜索，返回无法在跳数上限内到达目标的起始版本。
fn unreachable_start_versions(patches: &[PatchInfo], target: &str) -> Vec<String> {
    let Ok(target) = semver::Version::parse(normalize_version(target)) else {
        return Vec::new();
    };

    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for patch in patches {
        let (Ok(from), Ok(to)) = (
            semver::Version::parse(normalize_version(&patch.from_version)),
            semver::Version::parse(normalize_version(&patch.to_version)),
        ) else {
            continue;
        };
        if from >= to || to > target {
            continue;
        }
        reverse
            .entry(to.to_string())
            .or_default()
            .push(from.to_string());
    }

    let mut reachable = HashSet::new();
    let mut queue = VecDeque::from([(target.to_string(), 0usize)]);
    while let Some((node, depth)) = queue.pop_front() {
        if depth >= MAX_DELTA_CHAIN_HOPS {
            continue;
        }
        let Some(predecessors) = reverse.get(&node) else {
            continue;
        };
        for from in predecessors {
            if reachable.insert(from.clone()) {
                queue.push_back((from.clone(), depth + 1));
            }
        }
    }

    let mut unreachable: Vec<String> = patches
        .iter()
        .map(|patch| normalize_version(&patch.from_version).to_string())
        .filter(|from| !reachable.contains(from))
        .collect();
    unreachable.sort();
    unreachable.dedup();
    unreachable
}

async fn fetch_or_fail(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    what: &str,
    report: &mut Report,
) -> Option<Vec<u8>> {
    match fetch_bytes(client, url, limit).await {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            report.fail(format!("{what}不可用: {err}"));
            None
        }
    }
}

async fn fetch_bytes(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} 返回 HTTP {}", resp.status()));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > limit {
            return Err(format!("{url} 体积 {len} 超过上限 {limit}"));
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取 {url} 失败: {e}"))?;
    if bytes.len() > limit {
        return Err(format!("{url} 体积 {} 超过上限 {limit}", bytes.len()));
    }
    Ok(bytes.to_vec())
}

/// 读取远端文件大小：优先 HEAD；CDN 不返回长度时用单字节 Range 取总长。
async fn remote_size(client: &reqwest::Client, url: &str) -> Result<Option<u64>, String> {
    let resp = client
        .head(url)
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if resp.status().is_success() {
        // 0 字节对安装包没有意义：某些服务器/代理会把 HEAD 的长度报成 0，
        // 继续用 Range 探测，避免把正常文件误判成"体积不一致"。
        if let Some(len) = resp.content_length().filter(|len| *len > 0) {
            return Ok(Some(len));
        }
    } else if resp.status() != reqwest::StatusCode::METHOD_NOT_ALLOWED {
        return Err(format!("{url} 返回 HTTP {}", resp.status()));
    }

    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        // `Content-Range: bytes 0-0/12345` 的末段就是总长度
        if let Some(total) = resp
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|value| value.trim().parse().ok())
        {
            return Ok(Some(total));
        }
    }
    // 服务器忽略了 Range 但返回了完整响应的长度：同样可用。
    if resp.status().is_success() {
        return Ok(resp.content_length().filter(|len| *len > 0));
    }
    Err(format!("{url} 返回 HTTP {}", resp.status()))
}

/// 流式下载并计算 SHA-256，避免把大补丁整体读进内存。
async fn remote_hash(client: &reqwest::Client, url: &str) -> Result<(String, u64), String> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} 返回 HTTP {}", resp.status()));
    }

    let mut hasher = Sha256::new();
    let mut total = 0u64;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("读取 {url} 失败: {e}"))?
    {
        total += chunk.len() as u64;
        if total > MAX_PATCH_BYTES {
            return Err(format!("{url} 超过 {MAX_PATCH_BYTES} 字节上限"));
        }
        hasher.update(&chunk);
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

async fn download_temp(client: &reqwest::Client, url: &str) -> Result<PathBuf, String> {
    use tokio::io::AsyncWriteExt;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!("tdx-doctor-{}-{stamp}.bin", std::process::id()));

    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} 返回 HTTP {}", resp.status()));
    }

    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("读取 {url} 失败: {e}"))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入临时文件失败: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("写入临时文件失败: {e}"))?;
    Ok(path)
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

fn is_loopback(url: &str) -> bool {
    url.strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| {
            host.starts_with("127.0.0.1")
                || host.starts_with("localhost")
                || host.starts_with("[::1]")
        })
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 整包文件条目必须是安全的相对路径：拒绝绝对路径、盘符与 `..` 穿越。
fn is_safe_bundle_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return false;
    }
    let mut segments = path.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty() || first.contains(':') {
        return false;
    }
    path.split('/')
        .all(|segment| !matches!(segment, "" | "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::{is_http_url, is_safe_bundle_path, is_sha256_hex, unreachable_start_versions};
    use tauri_delta_updater::{PatchInfo, PatchScope};

    fn patch(from: &str, to: &str) -> PatchInfo {
        PatchInfo {
            from_version: from.into(),
            to_version: to.into(),
            patch_url: format!("https://cdn.example.com/{from}_to_{to}.patch"),
            patch_hash: "a".repeat(64),
            target_hash: "b".repeat(64),
            patch_size: None,
            scope: Some(PatchScope::MainExecutableOnly),
        }
    }

    /// 链前缀可达；孤立补丁与超过跳数上限的起点必须被指出。
    #[test]
    fn reports_unreachable_chain_starts() {
        let patches = vec![
            patch("0.1.0", "0.2.0"),
            patch("0.2.0", "0.3.0"),
            patch("0.3.0", "0.4.0"),
            patch("0.4.0", "0.5.0"),
            patch("0.0.9", "0.0.10"),
        ];
        assert_eq!(
            unreachable_start_versions(&patches, "0.5.0"),
            vec!["0.0.9".to_string(), "0.1.0".to_string()],
            "0.1.0 是第 4 跳起点，必须报告"
        );
    }

    #[test]
    fn validates_paths_and_hashes() {
        assert!(is_http_url("https://cdn.example.com/latest.json"));
        assert!(!is_http_url("ftp://x"));
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(!is_sha256_hex(&"z".repeat(64)));
        assert!(is_safe_bundle_path("resources/app.asar"));
        assert!(!is_safe_bundle_path("../escape"));
        assert!(!is_safe_bundle_path("/etc/passwd"));
        assert!(!is_safe_bundle_path("C:/Windows/system32"));
        assert!(!is_safe_bundle_path("a//b"));
        assert!(!is_safe_bundle_path("a\\b"));
    }
}
