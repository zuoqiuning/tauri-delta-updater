//! 补丁规划：直接补丁、多跳链与成本边界。
//!
//! 决策只在这里发生：直接命中是"1 跳链"的特例，调用方拿到的是**已排序的
//! 补丁序列**，不必再自己判断"有没有补丁、要不要全量、先打哪个"。

use crate::models::{PatchInfo, UpdateManifest};
use crate::platform::target::{normalize_version, EnvironmentReason};
use std::collections::{HashMap, VecDeque};

/// 组合“为什么不能走增量”的统一原因。
///
/// `check()` 与 `update()` 必须使用同一份逻辑：两边各写一遍分支，
/// 迟早会出现“检查说能增量、更新却回退全量”的不一致。
///
/// - 环境不适合原地替换时，环境原因优先于“整包不支持”；
/// - 有单文件补丁时不算回退（`None`）；
/// - 有链但超预算时报告 [`FallbackReason::ChainOverBudget`]；
/// - 否则整包不支持解释整包原因，未配置整包解释“没有匹配补丁”。
pub(super) fn fallback_reason(
    bundle_unsupported: Option<&'static str>,
    environment: Option<EnvironmentReason>,
    has_single_patch: bool,
    chain_over_budget: Option<(usize, u64, u64)>,
    from: &str,
    to: &str,
) -> Option<super::FallbackReason> {
    if let Some(reason) = environment {
        return Some(super::FallbackReason::EnvironmentUnsupported(reason));
    }
    if let Some((hops, total_size, installer_size)) = chain_over_budget {
        if !has_single_patch {
            return Some(super::FallbackReason::ChainOverBudget {
                hops,
                total_size,
                installer_size,
            });
        }
    }
    match (bundle_unsupported, has_single_patch) {
        (Some(detail), false) => Some(super::FallbackReason::EnvironmentUnsupported(
            EnvironmentReason::Other(detail),
        )),
        (_, true) => None,
        (None, false) => Some(super::FallbackReason::NoMatchingPatch {
            from: from.to_string(),
            to: to.to_string(),
        }),
    }
}

/// 补丁路径的规划结果。
///
/// 把“没有补丁”与“有链但太贵”分开：UI 可以据此提示
/// “跨版本更新，建议下载完整安装包”，而不是笼统地说“没有补丁”。
pub(crate) enum PlanOutcome<'a> {
    /// 直达补丁（1 跳）。
    Direct(&'a PatchInfo),
    /// 多跳链（至少 2 跳，已排序）。
    Chain(Vec<&'a PatchInfo>),
    /// 存在链，但总下载量超过全量安装包的收益阈值。
    OverBudget {
        /// 链的跳数。
        hops: usize,
        /// 链的总下载体积（字节）。
        total_size: u64,
        /// 全量安装包体积（字节）。
        installer_size: u64,
    },
    /// 没有可用路径（无补丁、跳数超限或版本不可解析）。
    Unreachable,
}

impl<'a> PlanOutcome<'a> {
    /// 可执行的补丁序列；`OverBudget` / `Unreachable` 为 `None`。
    pub(crate) fn patches(self) -> Option<Vec<&'a PatchInfo>> {
        match self {
            PlanOutcome::Direct(patch) => Some(vec![patch]),
            PlanOutcome::Chain(chain) => Some(chain),
            PlanOutcome::OverBudget { .. } | PlanOutcome::Unreachable => None,
        }
    }

    /// 超预算的 `(跳数, 总大小, 安装包大小)`；其它结果为 `None`。
    pub(crate) fn over_budget(&self) -> Option<(usize, u64, u64)> {
        match self {
            PlanOutcome::OverBudget {
                hops,
                total_size,
                installer_size,
            } => Some((*hops, *total_size, *installer_size)),
            _ => None,
        }
    }
}

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
/// 返回 [`PlanOutcome::Unreachable`] 表示没有可用路径（调用方回退全量）；
/// 返回 [`PlanOutcome::OverBudget`] 表示有链但成本不划算。
pub(crate) fn plan_single<'a>(
    manifest: &'a UpdateManifest,
    platform: &str,
    current: &str,
) -> PlanOutcome<'a> {
    let Some(patches) = manifest.patches.get(platform) else {
        return PlanOutcome::Unreachable;
    };
    let Some(chain) = plan_chain(patches, current, &manifest.version, MAX_CHAIN_HOPS) else {
        return PlanOutcome::Unreachable;
    };
    if chain.len() == 1 {
        return PlanOutcome::Direct(chain[0]);
    }
    let installer_size = manifest.platforms.get(platform).and_then(|p| p.size);
    match budget_violation(&chain, installer_size) {
        Some((total_size, installer_size)) => PlanOutcome::OverBudget {
            hops: chain.len(),
            total_size,
            installer_size,
        },
        None => PlanOutcome::Chain(chain),
    }
}

/// 客户端能应用的补丁容器；`None` 表示历史清单，必须继续支持。
///
/// 未来新增容器时，旧客户端会在这里跳过它并回退全量，而不是下载后失败。
fn patch_algorithm_supported(patch: &PatchInfo) -> bool {
    match patch.algorithm.as_deref() {
        None | Some("bsdiff40") | Some("zstd-bsdiff40") | Some("tdxb1") => true,
        Some(_) => false,
    }
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
        // 不认识的容器格式不能进入链：下载后才发现无法应用，只会变成一次
        // "更新失败 → 回滚"。跳过它，让规划回退到全量或其它路径。
        if !patch_algorithm_supported(patch) {
            continue;
        }
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
#[cfg(test)]
pub(crate) fn within_budget(chain: &[&PatchInfo], installer_size: Option<u64>) -> bool {
    budget_violation(chain, installer_size).is_none()
}

/// 超预算时返回 `(链总大小, 安装包大小)`；未知大小或仍然划算时为 `None`。
pub(crate) fn budget_violation(
    chain: &[&PatchInfo],
    installer_size: Option<u64>,
) -> Option<(u64, u64)> {
    if chain.len() < 2 {
        return None;
    }
    let installer_size = installer_size.filter(|size| *size > 0)?;
    let mut total = 0u64;
    for patch in chain {
        total = total.saturating_add(patch.patch_size?);
    }
    let too_expensive = (total as f64) > (installer_size as f64) * MAX_CHAIN_BUDGET_RATIO;
    too_expensive.then_some((total, installer_size))
}

#[cfg(test)]
mod tests {
    use super::{
        fallback_reason, plan_chain, plan_single, within_budget, PlanOutcome, MAX_CHAIN_HOPS,
    };
    use crate::models::{
        PatchInfo, PatchScope, PlatformInfo, UpdateManifest, MAX_DELTA_CHAIN_HOPS,
    };
    use crate::platform::target::{normalize_version, EnvironmentReason};
    use crate::updater::FallbackReason;
    use proptest::prelude::*;

    /// 回退原因的组合必须与 check/update 共用同一语义。
    #[test]
    fn fallback_reason_prefers_environment_and_single_patch() {
        assert_eq!(
            fallback_reason(
                Some("bundle"),
                Some(EnvironmentReason::MacOsBundle),
                false,
                None,
                "1.0",
                "2.0"
            ),
            Some(FallbackReason::EnvironmentUnsupported(
                EnvironmentReason::MacOsBundle
            ))
        );
        assert_eq!(
            fallback_reason(Some("bundle"), None, true, None, "1.0", "2.0"),
            None,
            "单文件补丁可用时不算回退"
        );
        assert_eq!(
            fallback_reason(Some("bundle"), None, false, None, "1.0", "2.0"),
            Some(FallbackReason::EnvironmentUnsupported(
                EnvironmentReason::Other("bundle")
            ))
        );
        assert_eq!(
            fallback_reason(None, None, false, Some((2, 900, 1000)), "1.0", "2.0"),
            Some(FallbackReason::ChainOverBudget {
                hops: 2,
                total_size: 900,
                installer_size: 1000,
            }),
            "超预算的链必须与“没有补丁”区分"
        );
        assert_eq!(
            fallback_reason(None, None, false, None, "1.0", "2.0"),
            Some(FallbackReason::NoMatchingPatch {
                from: "1.0".into(),
                to: "2.0".into(),
            })
        );
    }

    /// 不认识的补丁容器必须在规划阶段跳过，而不是下载后才发现无法应用。
    #[test]
    fn skips_patches_with_unknown_algorithm() {
        let mut unknown = patch("0.1.0", "0.2.0", None);
        unknown.algorithm = Some("future-format".into());
        let unknown_manifest = manifest("0.2.0", vec![unknown.clone()], Some(1000));
        assert!(
            matches!(
                plan_single(&unknown_manifest, "windows-x86_64", "0.1.0"),
                PlanOutcome::Unreachable
            ),
            "未知容器必须让规划退回全量"
        );

        // 已知算法（含缺省的旧清单）仍然可规划
        unknown.algorithm = Some("tdxb1".into());
        let known_manifest = manifest("0.2.0", vec![unknown], Some(1000));
        assert!(matches!(
            plan_single(&known_manifest, "windows-x86_64", "0.1.0"),
            PlanOutcome::Direct(_)
        ));
    }

    fn patch(from: &str, to: &str, size: Option<u64>) -> PatchInfo {
        PatchInfo {
            from_version: from.into(),
            to_version: to.into(),
            patch_url: format!("https://cdn.example.com/{from}_to_{to}.patch"),
            patch_hash: "patch".into(),
            target_hash: "target".into(),
            patch_size: size,
            scope: Some(PatchScope::MainExecutableOnly),
            algorithm: None,
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
                min_os: None,
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
            notes_i18n: Default::default(),
            extra: Default::default(),
        }
    }

    // 随机版本边 + 随机跳数上限：规划结果必须始终是一条合法链。
    //
    // 不变量：
    // - 链非空且不超过上限；
    // - 第一跳从当前版本出发，最后一跳到达目标版本；
    // - 相邻两跳首尾相接，且版本严格递增；
    // - 存在直达边时优先返回 1 跳；
    // - 跳数上限为 0 时永远没有链。
    proptest! {
        #[test]
        fn planned_chain_is_well_formed(
            edges in prop::collection::vec((0u16..6, 0u16..6), 0..12),
            start in 0u16..5,
            target in 1u16..6,
            max_hops in 0usize..4,
        ) {
            let patches: Vec<PatchInfo> = edges
                .iter()
                .filter(|(from, to)| from < to)
                .map(|(from, to)| {
                    patch(
                        &format!("0.0.{from}"),
                        &format!("0.0.{to}"),
                        None,
                    )
                })
                .collect();
            let current = format!("0.0.{start}");
            let goal = format!("0.0.{target}");

            let chain = plan_chain(&patches, &current, &goal, max_hops);

            if max_hops == 0 {
                prop_assert!(chain.is_none());
                return Ok(());
            }

            let has_direct = patches.iter().any(|patch| {
                patch.from_version == current && patch.to_version == goal
            });
            if has_direct {
                prop_assert!(chain.as_ref().is_some_and(|chain| chain.len() == 1));
            }

            if let Some(chain) = chain {
                prop_assert!(!chain.is_empty());
                prop_assert!(chain.len() <= max_hops);
                prop_assert_eq!(&chain.first().unwrap().from_version, &current);
                prop_assert_eq!(&chain.last().unwrap().to_version, &goal);
                for window in chain.windows(2) {
                    prop_assert_eq!(&window[0].to_version, &window[1].from_version);
                }
                let mut previous = semver::Version::parse(normalize_version(&current)).unwrap();
                for hop in &chain {
                    let next = semver::Version::parse(normalize_version(&hop.to_version)).unwrap();
                    prop_assert!(next > previous);
                    previous = next;
                }
            }
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
        match plan_single(&m, "windows-x86_64", "0.1.0") {
            PlanOutcome::OverBudget {
                hops,
                total_size,
                installer_size,
            } => {
                assert_eq!(hops, 2);
                assert_eq!(total_size, 900);
                assert_eq!(installer_size, 1_000);
            }
            _ => panic!("超过预算的链必须报告 OverBudget"),
        }

        let m = manifest(
            "0.3.0",
            vec![
                patch("0.1.0", "0.2.0", Some(200)),
                patch("0.2.0", "0.3.0", Some(200)),
            ],
            Some(1_000),
        );
        let chain = plan_single(&m, "windows-x86_64", "0.1.0")
            .patches()
            .expect("划算的链必须可执行");
        assert_eq!(chain.len(), 2);
        assert!(matches!(
            plan_single(&m, "darwin-aarch64", "0.1.0"),
            PlanOutcome::Unreachable
        ));

        let direct = manifest(
            "0.2.0",
            vec![patch("0.1.0", "0.2.0", Some(900))],
            Some(1_000),
        );
        assert!(matches!(
            plan_single(&direct, "windows-x86_64", "0.1.0"),
            PlanOutcome::Direct(_)
        ));
    }
}
