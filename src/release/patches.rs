//! 单文件补丁生成：扫描基线、生成、验证并汇总。

use super::files::{extract_version, infer_os, is_app_binary_candidate};
use super::SkipRecord;
use crate::{
    apply_patch, calculate_hash, create_patch_block_delta, create_patch_uncompressed,
    create_patch_with_level, detect_arch, normalize_version, DeltaError, PatchInfo, PatchScope,
    MAX_DELTA_CHAIN_HOPS,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use walkdir::WalkDir;

/// 补丁验证目录的计数器，保证同一进程内多次调用不冲突。
static VERIFY_COUNTER: AtomicU32 = AtomicU32::new(0);

/// 生成单文件补丁所需的发布上下文。
#[derive(Clone, Copy)]
pub(crate) struct PatchContext<'a> {
    pub(crate) app_binary: &'a Path,
    pub(crate) old_dirs: &'a [PathBuf],
    pub(crate) old_binaries: &'a [PathBuf],
    pub(crate) new_version: &'a str,
    pub(crate) target_os: &'a str,
    pub(crate) target_arch: &'a str,
    pub(crate) new_hash: &'a str,
    pub(crate) installer_size: u64,
    pub(crate) patch_dir: &'a Path,
    pub(crate) base_url: &'a str,
    pub(crate) zstd_level: i32,
    /// 生成未压缩补丁，兼容不认 zstd 容器的旧客户端。
    pub(crate) legacy_patch: bool,
    /// bsdiff 工作集超限时退回分块差分（TDXB1）。
    pub(crate) large_file_delta: bool,
    pub(crate) max_patch_ratio: f64,
    pub(crate) dry_run: bool,
}

/// 单个基线的处理结果：要么产出补丁，要么记录跳过原因。
struct CandidateOutcome {
    patch: Option<PatchInfo>,
    skip: Option<SkipRecord>,
}

impl CandidateOutcome {
    fn ignored() -> Self {
        Self {
            patch: None,
            skip: None,
        }
    }

    fn skipped(file: String, reason: String) -> Self {
        Self {
            patch: None,
            skip: Some(SkipRecord { file, reason }),
        }
    }

    fn patch(patch: PatchInfo) -> Self {
        Self {
            patch: Some(patch),
            skip: None,
        }
    }
}

/// 为每个可用基线生成并验证补丁，返回去重后的补丁信息与跳过记录。
///
/// 基线之间互不依赖，因此并行生成；内存与 CPU 由 `core::diff` 的预算限流，
/// `join_all` 保序，补丁与跳过记录的顺序与串行版本完全一致。
pub(crate) async fn generate_single_patches(
    ctx: &PatchContext<'_>,
) -> Result<(Vec<PatchInfo>, Vec<SkipRecord>), String> {
    let PatchContext {
        app_binary,
        old_dirs,
        old_binaries,
        new_version,
        target_os,
        target_arch,
        ..
    } = *ctx;

    let mut skipped: Vec<SkipRecord> = Vec::new();

    // 基线来源：基线目录扫描 + 显式基线文件。后者校验失败会直接报错。
    let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
    for dir in old_dirs {
        for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                candidates.push((path.to_path_buf(), false));
            }
        }
    }
    for path in old_binaries {
        candidates.push((path.clone(), true));
    }

    // 过滤放在并发之前：报错顺序、日志顺序都与输入顺序一致。
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut valid: Vec<(PathBuf, String)> = Vec::new();

    for (path, explicit) in candidates {
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // 目录扫描时过滤非主程序；显式路径必须是主程序，否则直接报错。
        if !is_app_binary_candidate(&file_name) {
            if explicit {
                return Err(format!(
                    "--old-binary 不是主程序（像是安装包或元数据）: {file_name}"
                ));
            }
            continue;
        }

        // 同一文件只处理一次（目录扫描与显式文件可能重叠）
        if let Ok(canonical) = std::fs::canonicalize(&path) {
            if !seen.insert(canonical) {
                continue;
            }
        }

        // 文件是本次发布的主程序本身时跳过
        if let (Ok(a), Ok(b)) = (
            std::fs::canonicalize(&path),
            std::fs::canonicalize(app_binary),
        ) {
            if a == b {
                if explicit {
                    return Err("--old-binary 不能指向本次发布的主程序本身".into());
                }
                continue;
            }
        }

        // 架构不同的主程序不能与新版本互为补丁
        let old_arch = detect_arch(&file_name);
        if old_arch == "unknown" || old_arch != target_arch {
            let reason = format!("架构不明，或与目标架构 {target_arch} 不同");
            if explicit {
                return Err(format!("--old-binary {file_name} 不可用: {reason}"));
            }
            println!("[SKIP] 跳过 {file_name}: {reason}");
            skipped.push(SkipRecord {
                file: file_name,
                reason,
            });
            continue;
        }
        // 有平台信息时还必须与目标操作系统一致
        if let Some(old_os) = infer_os(&file_name) {
            if old_os != target_os {
                let reason = format!("操作系统 {old_os} 与目标 {target_os} 不同");
                if explicit {
                    return Err(format!("--old-binary {file_name} 不可用: {reason}"));
                }
                println!("[SKIP] 跳过 {file_name}: {reason}");
                skipped.push(SkipRecord {
                    file: file_name,
                    reason,
                });
                continue;
            }
        }

        let old_version = match extract_version(&file_name) {
            Some(v) => normalize_version(&v).to_string(),
            None => {
                let reason = "文件名里没有可解析的语义化版本".to_string();
                if explicit {
                    return Err(format!("--old-binary {file_name} 不可用: {reason}"));
                }
                println!("[WARN] 跳过 {file_name}: {reason}");
                skipped.push(SkipRecord {
                    file: file_name,
                    reason,
                });
                continue;
            }
        };
        if old_version == new_version {
            if explicit {
                return Err(format!(
                    "--old-binary {file_name} 的版本与 --version ({new_version}) 相同"
                ));
            }
            continue;
        }

        valid.push((path, old_version));
    }

    let outcomes = futures_util::future::join_all(
        valid
            .iter()
            .map(|(path, old_version)| process_candidate(ctx, path, old_version)),
    )
    .await;

    let mut platform_patches: Vec<PatchInfo> = Vec::new();
    for outcome in outcomes {
        let outcome = outcome?;
        if let Some(patch) = outcome.patch {
            platform_patches.push(patch);
        }
        if let Some(skip) = outcome.skip {
            skipped.push(skip);
        }
    }

    // 同一 from_version 只保留最新的一份补丁（按语义化版本排序）
    platform_patches.sort_by(|a, b| {
        semver::Version::parse(&a.from_version)
            .ok()
            .cmp(&semver::Version::parse(&b.from_version).ok())
    });
    platform_patches.dedup_by(|a, b| a.from_version == b.from_version);

    Ok((platform_patches, skipped))
}

/// 处理一个已通过预检的基线：生成、校验体积、回放验证。
async fn process_candidate(
    ctx: &PatchContext<'_>,
    path: &Path,
    old_version: &str,
) -> Result<CandidateOutcome, String> {
    let PatchContext {
        app_binary,
        new_version,
        new_hash,
        installer_size,
        patch_dir,
        base_url,
        zstd_level,
        legacy_patch,
        large_file_delta,
        max_patch_ratio,
        dry_run,
        ..
    } = *ctx;
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let patch_name = format!("{old_version}_to_{new_version}.patch");
    let patch_path = patch_dir.join(&patch_name);

    if dry_run {
        println!("[DRY] 将生成补丁: {old_version} -> {new_version}（{file_name}）");
        return Ok(CandidateOutcome::ignored());
    }

    println!("[INFO] 生成补丁: {old_version} -> {new_version}");
    let created = if legacy_patch {
        create_patch_uncompressed(path, app_binary, &patch_path).await
    } else {
        create_patch_with_level(path, app_binary, &patch_path, zstd_level).await
    };
    if let Err(err) = created {
        match err {
            // 体积/内存超出 bsdiff 适用范围：分块差分可以绕开 17n 的内存模型，
            // 但只有发布者显式开启时才使用（旧客户端不认 TDXB1）。
            DeltaError::IncrementalNotApplicable { .. }
            | DeltaError::PatchWorkingSetTooLarge { .. }
                if large_file_delta && !legacy_patch =>
            {
                println!("[INFO] {file_name} 超出 bsdiff 适用范围，改用分块差分");
                if let Err(fallback) =
                    create_patch_block_delta(path, app_binary, &patch_path, zstd_level).await
                {
                    let reason = fallback.to_string();
                    println!("[SKIP] {file_name}: {reason}");
                    return Ok(CandidateOutcome::skipped(file_name, reason));
                }
            }
            // 未开启分块差分时跳过该基线，走全量分发，
            // 而不是让发布流水线在分配失败时直接 abort。
            DeltaError::IncrementalNotApplicable { .. }
            | DeltaError::PatchWorkingSetTooLarge { .. } => {
                let reason = err.to_string();
                println!("[SKIP] {file_name}: {reason}");
                return Ok(CandidateOutcome::skipped(file_name, reason));
            }
            other => return Err(format!("生成补丁失败: {other}")),
        }
    }
    let patch_hash = calculate_hash(&patch_path)
        .await
        .map_err(|e| format!("计算补丁哈希失败: {e}"))?;
    let patch_size = std::fs::metadata(&patch_path).map(|m| m.len()).ok();

    // 劣质补丁跳过：补丁接近全量时，下载补丁反而更慢
    if let Some(size) = patch_size {
        if installer_size > 0 && (size as f64) > (installer_size as f64) * max_patch_ratio {
            let reason = format!(
                "补丁体积 {size} 超过安装包的 {:.0}%",
                max_patch_ratio * 100.0
            );
            println!("[SKIP] {patch_name}: {reason}，已跳过");
            let _ = std::fs::remove_file(&patch_path);
            return Ok(CandidateOutcome::skipped(file_name, reason));
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
    std::fs::create_dir_all(&verify_dir).map_err(|e| format!("创建补丁验证目录失败: {e}"))?;
    let verify_out = verify_dir.join("reconstructed");
    let verify_result = apply_patch(path, &patch_path, &verify_out, &patch_hash, new_hash).await;
    let _ = std::fs::remove_file(&verify_out);
    let _ = std::fs::remove_dir_all(&verify_dir);
    verify_result.map_err(|e| {
        format!(
            "补丁 {patch_name} 无法从旧主程序重建出新版本（发布中止 / patch verification failed）: {e}\n建议：\n  1. 检查 --old-dir / --from 的基线是否与目标架构一致\n  2. 检查 --max-patch-ratio 是否过于严格\n  3. 发布后运行 `cargo delta doctor` 体检线上桶"
        )
    })?;
    println!("[OK] 补丁验证通过: {patch_name}（{patch_size:?} 字节）");

    Ok(CandidateOutcome::patch(PatchInfo {
        from_version: old_version.to_string(),
        to_version: new_version.to_string(),
        patch_url: format!("{}/{patch_name}", base_url.trim_end_matches('/')),
        patch_hash,
        target_hash: new_hash.to_string(),
        patch_size,
        // 补丁基底与产物都是主程序，显式标注覆盖范围。
        // 使用 sidecar/resources 的应用据此提示"本次增量不覆盖附加资源"。
        scope: Some(PatchScope::MainExecutableOnly),
    }))
}

/// 保留仍能链到目标版本的补丁，并按跳数上限裁剪。
///
/// 每轮发布都会为每个可用基线重新生成"直达新版本"的补丁；这里额外保留
/// 历史补丁，让跨多个版本的客户端可以 `0.1 -> 0.2 -> 0.3` 逐跳升级，
/// 而不必下载全量安装包。超过 [`MAX_DELTA_CHAIN_HOPS`] 的补丁用不上，
/// 必须裁掉，否则清单会随发布次数无限膨胀。
///
/// 保留的是**元数据**：旧补丁文件必须继续留在 CDN 上，否则链会在下载阶段
/// 断掉。发布流水线不应删除历史补丁。
pub(crate) fn retain_chain_patches(
    mut grouped: HashMap<String, Vec<PatchInfo>>,
    new_version: &str,
) -> HashMap<String, Vec<PatchInfo>> {
    let Ok(target) = semver::Version::parse(normalize_version(new_version)) else {
        return grouped;
    };

    for patches in grouped.values_mut() {
        // 去重：同一 (from,to) 只保留最后写入的那份（本轮生成的新补丁）。
        let mut latest: HashMap<(String, String), PatchInfo> = HashMap::new();
        for patch in patches.drain(..) {
            let (Ok(from), Ok(to)) = (
                semver::Version::parse(normalize_version(&patch.from_version)),
                semver::Version::parse(normalize_version(&patch.to_version)),
            ) else {
                continue;
            };
            if from >= to || to > target {
                continue;
            }
            latest.insert((from.to_string(), to.to_string()), patch);
        }
        let edges: Vec<PatchInfo> = latest.into_values().collect();

        // 反向 BFS：从目标版本出发，标出 `MAX_DELTA_CHAIN_HOPS` 跳内可达的起点。
        let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
        for patch in &edges {
            reverse
                .entry(normalize_version(&patch.to_version).to_string())
                .or_default()
                .push(normalize_version(&patch.from_version).to_string());
        }
        let mut keep: HashSet<String> = HashSet::new();
        let mut queue = VecDeque::from([(target.to_string(), 0usize)]);
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= MAX_DELTA_CHAIN_HOPS {
                continue;
            }
            let Some(predecessors) = reverse.get(&node) else {
                continue;
            };
            for from in predecessors {
                if keep.insert(from.clone()) {
                    queue.push_back((from.clone(), depth + 1));
                }
            }
        }

        // 只保留"从该版本出发一定能走到目标"的边。
        let mut kept: Vec<PatchInfo> = edges
            .into_iter()
            .filter(|patch| keep.contains(normalize_version(&patch.from_version)))
            .collect();
        // 排序保证清单稳定，避免每次发布都产生无意义的 diff。
        kept.sort_by(|a, b| {
            let from_a = semver::Version::parse(normalize_version(&a.from_version)).ok();
            let from_b = semver::Version::parse(normalize_version(&b.from_version)).ok();
            from_a.cmp(&from_b).then_with(|| {
                let to_a = semver::Version::parse(normalize_version(&a.to_version)).ok();
                let to_b = semver::Version::parse(normalize_version(&b.to_version)).ok();
                to_a.cmp(&to_b)
            })
        });
        *patches = kept;
    }

    grouped.retain(|_, patches| !patches.is_empty());
    grouped
}

#[cfg(test)]
mod tests {
    use super::retain_chain_patches;
    use crate::{PatchInfo, PatchScope};
    use std::collections::HashMap;

    fn patch(from: &str, to: &str, url: &str) -> PatchInfo {
        PatchInfo {
            from_version: from.into(),
            to_version: to.into(),
            patch_url: url.into(),
            patch_hash: "patch".into(),
            target_hash: "target".into(),
            patch_size: None,
            scope: Some(PatchScope::MainExecutableOnly),
        }
    }

    fn group(patches: Vec<PatchInfo>) -> HashMap<String, Vec<PatchInfo>> {
        let mut grouped = HashMap::new();
        grouped.insert("windows-x86_64".to_string(), patches);
        grouped
    }

    fn hops(grouped: &HashMap<String, Vec<PatchInfo>>) -> Vec<(String, String)> {
        grouped["windows-x86_64"]
            .iter()
            .map(|p| (p.from_version.clone(), p.to_version.clone()))
            .collect()
    }

    /// 链前缀必须保留，孤立的历史补丁必须清掉。
    #[test]
    fn keeps_chainable_prefix_and_drops_orphans() {
        let grouped = retain_chain_patches(
            group(vec![
                patch("0.1.0", "0.2.0", "old.patch"),
                patch("0.0.9", "0.0.10", "orphan.patch"),
                patch("0.2.0", "0.3.0", "new.patch"),
            ]),
            "0.3.0",
        );
        assert_eq!(
            hops(&grouped),
            vec![
                ("0.1.0".to_string(), "0.2.0".to_string()),
                ("0.2.0".to_string(), "0.3.0".to_string()),
            ]
        );
    }

    /// 超过跳数上限的链前缀必须裁掉：客户端永远不会用到。
    #[test]
    fn drops_prefixes_beyond_hop_limit() {
        let grouped = retain_chain_patches(
            group(vec![
                patch("0.1.0", "0.2.0", "a.patch"),
                patch("0.2.0", "0.3.0", "b.patch"),
                patch("0.3.0", "0.4.0", "c.patch"),
                patch("0.4.0", "0.5.0", "d.patch"),
            ]),
            "0.5.0",
        );
        assert_eq!(
            hops(&grouped),
            vec![
                ("0.2.0".to_string(), "0.3.0".to_string()),
                ("0.3.0".to_string(), "0.4.0".to_string()),
                ("0.4.0".to_string(), "0.5.0".to_string()),
            ],
            "0.1→0.2 是第 4 跳，必须裁掉"
        );
    }

    /// 同一对版本在旧清单与本轮生成里都存在时，本轮生成的新补丁必须胜出。
    #[test]
    fn new_patch_overwrites_same_edge() {
        let grouped = retain_chain_patches(
            group(vec![
                patch("0.2.0", "0.3.0", "old.patch"),
                patch("0.2.0", "0.3.0", "new.patch"),
            ]),
            "0.3.0",
        );
        assert_eq!(grouped["windows-x86_64"][0].patch_url, "new.patch");
    }
}
