//! 补丁规划：直接补丁、多跳链与成本边界。
//!
//! 决策只在这里发生：直接命中是"1 跳链"的特例，调用方拿到的是**已排序的
//! 补丁序列**，不必再自己判断"有没有补丁、要不要全量、先打哪个"。

use crate::models::{PatchInfo, UpdateManifest};
use crate::platform::target::normalize_version;
use std::collections::{HashMap, VecDeque};

/// 链式补丁最多串几跳。
///
/// 每多一跳就多一次下载、合并与哈希校验：收益递减，失败面线性增长。
pub(crate) const MAX_CHAIN_HOPS: usize = crate::models::MAX_DELTA_CHAIN_HOPS;

/// 链式补丁总大小超过安装包的这一比例时，全量下载更划算。
///
/// 只约束多跳链；直达补丁保持原有行为，避免已经可用的补丁被成本规则误杀。
pub(crate) const MAX_CHAIN_BUDGET_RATIO: f64 = 0.8;

/// 为清单规划一条到达目标版本的单文件补丁路径。
///
/// 返回 `None` 表示没有可用路径（调用方回退全量）。
pub(crate) fn plan_single<'a>(
    manifest: &'a UpdateManifest,
    platform: &str,
    current: &str,
) -> Option<Vec<&'a PatchInfo>> {
    let patches = manifest.patches.get(platform)?;
    let chain = plan_chain(patches, current, &manifest.version, MAX_CHAIN_HOPS)?;
    let installer_size = manifest.platforms.get(platform).and_then(|p| p.size);
    within_budget(&chain, installer_size).then_some(chain)
}

/// 在补丁列表里找一条从 `current` 到 `target` 的最短路径。
///
/// 每一跳都必须严格版本递增且落在 `(current, target]` 内，否则链可能先降级
/// 再升级，或跨过目标版本。直接补丁天然优先（最短路径深度 1）。
pub(crate) fn plan_chain<'a>(
    patches: &'a [PatchInfo],
    current: &str,
    target: &str,
    max_hops: usize,
) -> Option<Vec<&'a PatchInfo>> {
    let current = semver::Version::parse(normalize_version(current)).ok()?;
    let target = semver::Version::parse(normalize_version(target)).ok()?;
    if current >= target || max_hops == 0 {
        return None;
    }

    // 先过滤出位于 (current, target] 区间内的递增边，避免 BFS 反复解析版本号。
    let mut edges: Vec<(semver::Version, semver::Version, usize)> = Vec::new();
    for (index, patch) in patches.iter().enumerate() {
        let (Ok(from), Ok(to)) = (
            semver::Version::parse(normalize_version(&patch.from_version)),
            semver::Version::parse(normalize_version(&patch.to_version)),
        ) else {
            continue;
        };
        if from < current || from >= to || to > target {
            continue;
        }
        edges.push((from, to, index));
    }

    // 版本号在清单里的写法可能带 `v` 前缀，邻接表统一用规范化字符串做键。
    let mut adjacency: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (from, to, index) in &edges {
        adjacency
            .entry(from.to_string())
            .or_default()
            .push((to.to_string(), *index));
    }

    let start = current.to_string();
    let goal = target.to_string();
    let mut visited: HashMap<String, Option<(String, usize)>> = HashMap::new();
    visited.insert(start.clone(), None);
    let mut queue = VecDeque::from([(start.clone(), 0usize)]);

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_hops {
            continue;
        }
        let Some(next_edges) = adjacency.get(&node) else {
            continue;
        };
        for (next, index) in next_edges {
            if visited.contains_key(next) {
                continue;
            }
            visited.insert(next.clone(), Some((node.clone(), *index)));
            if next == &goal {
                return Some(reconstruct(&visited, &goal, patches));
            }
            queue.push_back((next.clone(), depth + 1));
        }
    }

    None
}

/// 从终点沿 BFS 前驱回溯成补丁序列。
fn reconstruct<'a>(
    visited: &HashMap<String, Option<(String, usize)>>,
    goal: &str,
    patches: &'a [PatchInfo],
) -> Vec<&'a PatchInfo> {
    let mut chain = Vec::new();
    let mut node = goal.to_string();
    while let Some(Some((previous, index))) = visited.get(&node) {
        chain.push(&patches[*index]);
        node = previous.clone();
    }
    chain.reverse();
    chain
}

/// 多跳链的总字节数是否仍然比全量安装包划算。
///
/// 大小未知（旧清单没有 `patch_size` / `size`）时不做限制：无法比较就不猜。
pub(crate) fn within_budget(chain: &[&PatchInfo], installer_size: Option<u64>) -> bool {
    if chain.len() < 2 {
        return true;
    }
    let Some(installer_size) = installer_size.filter(|size| *size > 0) else {
        return true;
    };
    let mut total = 0u64;
    for patch in chain {
        let Some(size) = patch.patch_size else {
            return true;
        };
        total = total.saturating_add(size);
    }
    (total as f64) <= (installer_size as f64) * MAX_CHAIN_BUDGET_RATIO
}

#[cfg(test)]
mod tests {
    use super::{plan_chain, plan_single, within_budget, MAX_CHAIN_HOPS};
    use crate::models::{
        PatchInfo, PatchScope, PlatformInfo, UpdateManifest, MAX_DELTA_CHAIN_HOPS,
    };

    fn patch(from: &str, to: &str, size: Option<u64>) -> PatchInfo {
        PatchInfo {
            from_version: from.into(),
            to_version: to.into(),
            patch_url: format!("https://cdn.example.com/{from}_to_{to}.patch"),
            patch_hash: "patch".into(),
            target_hash: "target".into(),
            patch_size: size,
            scope: Some(PatchScope::MainExecutableOnly),
        }
    }

    fn manifest(
        version: &str,
        patches: Vec<PatchInfo>,
        installer_size: Option<u64>,
    ) -> UpdateManifest {
        let mut platforms = std::collections::HashMap::new();
        platforms.insert(
            "windows-x86_64".to_string(),
            PlatformInfo {
                signature: "sig".into(),
                url: "https://cdn.example.com/app.exe".into(),
                size: installer_size,
                rollout: None,
            },
        );
        let mut grouped = std::collections::HashMap::new();
        grouped.insert("windows-x86_64".to_string(), patches);
        UpdateManifest {
            version: version.into(),
            notes: String::new(),
            pub_date: String::new(),
            min_version: None,
            platforms,
            patches: grouped,
        }
    }

    /// 直达补丁必须优先于任何链：少一次下载、少一次合并。
    #[test]
    fn direct_patch_wins_over_chain() {
        let patches = vec![
            patch("0.1.0", "0.3.0", Some(1_000)),
            patch("0.1.0", "0.2.0", Some(200)),
            patch("0.2.0", "0.3.0", Some(200)),
        ];
        let chain = plan_chain(&patches, "0.1.0", "0.3.0", 3).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].from_version, "0.1.0");
        assert_eq!(chain[0].to_version, "0.3.0");
    }

    /// 没有直达补丁时才串链，顺序必须从当前版本开始逐跳递增。
    #[test]
    fn multi_hop_chain_is_ordered() {
        let patches = vec![
            patch("0.2.0", "0.3.0", Some(300)),
            patch("0.1.0", "0.2.0", Some(200)),
        ];
        let chain = plan_chain(&patches, "0.1.0", "0.3.0", 3).unwrap();
        let hops: Vec<(&str, &str)> = chain
            .iter()
            .map(|p| (p.from_version.as_str(), p.to_version.as_str()))
            .collect();
        assert_eq!(hops, vec![("0.1.0", "0.2.0"), ("0.2.0", "0.3.0")]);
    }

    /// 越过目标版本的补丁必须被忽略，否则会先升级到 0.4 再"回退"到 0.3。
    #[test]
    fn ignores_edges_outside_target_window() {
        let patches = vec![patch("0.2.0", "0.4.0", Some(100))];
        assert!(plan_chain(&patches, "0.2.0", "0.3.0", 3).is_none());
    }

    /// 跳数上限必须生效：超出上限的链等于没有补丁。
    #[test]
    fn respects_hop_limit() {
        let patches = vec![
            patch("0.1.0", "0.2.0", None),
            patch("0.2.0", "0.3.0", None),
            patch("0.3.0", "0.4.0", None),
        ];
        assert_eq!(
            plan_chain(&patches, "0.1.0", "0.4.0", MAX_CHAIN_HOPS)
                .unwrap()
                .len(),
            MAX_CHAIN_HOPS
        );
        assert!(plan_chain(&patches, "0.1.0", "0.4.0", 2).is_none());
        assert_eq!(MAX_CHAIN_HOPS, MAX_DELTA_CHAIN_HOPS);
    }

    /// 非递增（同版本或降级）的边不能参与链。
    #[test]
    fn ignores_non_increasing_edges() {
        let patches = vec![
            patch("0.1.0", "0.1.0", None),
            patch("0.2.0", "0.1.0", None),
            patch("0.1.0", "0.2.0", None),
        ];
        let chain = plan_chain(&patches, "0.1.0", "0.2.0", 3).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].to_version, "0.2.0");
    }

    /// 成本边界只约束链：直达补丁即使接近全量也保持原有行为。
    #[test]
    fn budget_applies_only_to_chains() {
        let direct = [patch("0.1.0", "0.2.0", Some(900))];
        let chain = [
            patch("0.1.0", "0.2.0", Some(450)),
            patch("0.2.0", "0.3.0", Some(450)),
        ];
        assert!(within_budget(
            &direct.iter().collect::<Vec<_>>(),
            Some(1_000)
        ));
        assert!(
            !within_budget(&chain.iter().collect::<Vec<_>>(), Some(1_000)),
            "两条合计 90% 不划算"
        );
        assert!(
            within_budget(&chain.iter().collect::<Vec<_>>(), Some(2_000)),
            "45% 仍然划算"
        );
    }

    /// 大小未知时不做猜测：旧清单必须继续可用。
    #[test]
    fn unknown_sizes_do_not_block_chains() {
        let chain = [patch("0.1.0", "0.2.0", None), patch("0.2.0", "0.3.0", None)];
        assert!(within_budget(
            &chain.iter().collect::<Vec<_>>(),
            Some(1_000)
        ));
        assert!(within_budget(&chain.iter().collect::<Vec<_>>(), None));
    }

    /// `plan_single` 把清单、平台与成本判断合成一次决策。
    #[test]
    fn plan_single_combines_manifest_and_budget() {
        let m = manifest(
            "0.3.0",
            vec![
                patch("0.1.0", "0.2.0", Some(450)),
                patch("0.2.0", "0.3.0", Some(450)),
            ],
            Some(1_000),
        );
        assert!(
            plan_single(&m, "windows-x86_64", "0.1.0").is_none(),
            "超过预算的链应回退全量"
        );

        let m = manifest(
            "0.3.0",
            vec![
                patch("0.1.0", "0.2.0", Some(200)),
                patch("0.2.0", "0.3.0", Some(200)),
            ],
            Some(1_000),
        );
        assert_eq!(plan_single(&m, "windows-x86_64", "0.1.0").unwrap().len(), 2);
        assert!(plan_single(&m, "darwin-aarch64", "0.1.0").is_none());
    }
}
