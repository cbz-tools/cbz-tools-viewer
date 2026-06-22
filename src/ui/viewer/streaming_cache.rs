use std::collections::{HashMap, HashSet};

use super::auto_spread_plan::AutoSpreadPlan;

/// cache / inflight / worker 状態を持たず、`current_page` / `total_pages` / `visible_pages`
/// だけで BG に流す Desired Sequence の順序を決める。
#[derive(Clone, Debug)]
pub(super) struct SimpleStreamingCachePolicy {
    current_page: u32,
    total_pages: u32,
    visible_pages: Vec<u32>,
}

impl SimpleStreamingCachePolicy {
    pub(super) fn new(current_page: u32, total_pages: u32, visible_pages: Vec<u32>) -> Self {
        Self {
            current_page,
            total_pages,
            visible_pages,
        }
    }

    pub(super) fn desired_sequence(&self) -> Vec<u32> {
        if self.total_pages == 0 {
            return Vec::new();
        }

        let mut desired = Vec::with_capacity(self.total_pages as usize);
        let mut seen = HashSet::new();
        let current_page = self.current_page.min(self.total_pages.saturating_sub(1));
        let forward_remaining = self.total_pages.saturating_sub(current_page + 1);
        let backward_remaining = current_page;
        let forward_burst = (forward_remaining / 6).clamp(1, 12);

        for page in self
            .visible_pages
            .iter()
            .copied()
            .filter(|page| *page < self.total_pages)
        {
            if seen.insert(page) {
                desired.push(page);
            }
        }

        if seen.insert(current_page) {
            desired.push(current_page);
        }

        let mut forward_offset = 1u32;
        let mut backward_offset = 1u32;
        while forward_offset <= forward_remaining || backward_offset <= backward_remaining {
            for _ in 0..forward_burst {
                if forward_offset > forward_remaining {
                    break;
                }
                let next = current_page + forward_offset;
                if seen.insert(next) {
                    desired.push(next);
                }
                forward_offset = forward_offset.saturating_add(1);
            }

            if backward_offset <= backward_remaining {
                let prev = current_page - backward_offset;
                if seen.insert(prev) {
                    desired.push(prev);
                }
                backward_offset = backward_offset.saturating_add(1);
            }

            if desired.len() as u32 >= self.total_pages {
                break;
            }
        }

        desired
    }
}

#[derive(Clone, Debug)]
struct StreamingDesiredRankState {
    cache_pages: HashSet<u32>,
    desired_pages: HashSet<u32>,
    desired_ranks: HashMap<u32, usize>,
    lowest_priority_rank: usize,
}

struct StreamingEvictContext<'a> {
    bg_cache_pages: &'a [u32],
    desired_pages: &'a HashSet<u32>,
    desired_ranks: &'a HashMap<u32, usize>,
    lowest_priority_rank: usize,
    visible_pages: &'a HashSet<u32>,
    protected_pages: &'a HashSet<u32>,
    desired_sequence: &'a [u32],
    min_rank_exclusive: Option<usize>,
}

fn build_streaming_desired_rank_state(
    desired_sequence: &[u32],
    bg_cache_pages: &[u32],
) -> StreamingDesiredRankState {
    let cache_pages = bg_cache_pages.iter().copied().collect::<HashSet<_>>();
    let desired_pages = desired_sequence.iter().copied().collect::<HashSet<_>>();
    let desired_ranks = desired_sequence
        .iter()
        .copied()
        .enumerate()
        .map(|(rank, page)| (page, rank))
        .collect();
    let lowest_priority_rank = desired_sequence.len();

    StreamingDesiredRankState {
        cache_pages,
        desired_pages,
        desired_ranks,
        lowest_priority_rank,
    }
}

fn build_streaming_evict_candidates(context: StreamingEvictContext<'_>) -> Vec<u32> {
    let StreamingEvictContext {
        bg_cache_pages,
        desired_pages,
        desired_ranks,
        lowest_priority_rank,
        visible_pages,
        protected_pages,
        desired_sequence,
        min_rank_exclusive,
    } = context;
    let mut evict_candidates = Vec::new();
    let mut seen = HashSet::new();

    for page in bg_cache_pages.iter().copied() {
        let page_rank = desired_ranks
            .get(&page)
            .copied()
            .unwrap_or(lowest_priority_rank);
        if visible_pages.contains(&page)
            || protected_pages.contains(&page)
            || desired_pages.contains(&page)
            || min_rank_exclusive.is_some_and(|rank| page_rank <= rank)
            || !seen.insert(page)
        {
            continue;
        }
        evict_candidates.push(page);
    }

    for page in desired_sequence.iter().rev().copied() {
        let page_rank = desired_ranks
            .get(&page)
            .copied()
            .unwrap_or(lowest_priority_rank);
        if visible_pages.contains(&page) || protected_pages.contains(&page) || !seen.insert(page) {
            continue;
        }
        if min_rank_exclusive.is_some_and(|rank| page_rank <= rank) {
            continue;
        }
        evict_candidates.push(page);
    }

    evict_candidates
}

fn worst_evictable_rank(
    bg_cache_pages: &[u32],
    desired_ranks: &HashMap<u32, usize>,
    visible_pages: &HashSet<u32>,
    protected_pages: &HashSet<u32>,
    lowest_priority_rank: usize,
) -> Option<usize> {
    bg_cache_pages
        .iter()
        .copied()
        .filter(|page| !visible_pages.contains(page) && !protected_pages.contains(page))
        .map(|page| {
            desired_ranks
                .get(&page)
                .copied()
                .unwrap_or(lowest_priority_rank)
        })
        .max()
}

/// AUTO 表示では unit ベースの順序を展開して、physical page の Desired Sequence を返す。
/// cache / inflight / dispatch 状態は見ない。
pub(super) fn desired_auto_streaming_sequence<I>(
    plan: &AutoSpreadPlan,
    current_physical_page: u32,
    total_pages: u32,
    visible_pages: I,
) -> Vec<u32>
where
    I: IntoIterator<Item = u32>,
{
    if total_pages == 0 {
        return Vec::new();
    }

    let mut desired = Vec::with_capacity(total_pages as usize);
    let mut seen = HashSet::new();
    let current_physical_page = current_physical_page.min(total_pages.saturating_sub(1));

    append_unique_physical_pages(&mut desired, &mut seen, visible_pages, total_pages);
    append_auto_unit_pages(
        plan,
        &mut desired,
        &mut seen,
        current_physical_page,
        total_pages,
    );

    let current_unit_anchor = plan
        .anchor_for_logical_page(current_physical_page)
        .unwrap_or(current_physical_page);
    let mut forward_units = Vec::new();
    let mut next_unit_anchor = current_unit_anchor;
    while let Some(next_anchor) = plan.next_anchor(next_unit_anchor) {
        forward_units.push(next_anchor);
        next_unit_anchor = next_anchor;
    }

    let mut backward_units = Vec::new();
    let mut previous_unit_anchor = current_unit_anchor;
    while let Some(previous_anchor) = plan.previous_anchor(previous_unit_anchor) {
        backward_units.push(previous_anchor);
        previous_unit_anchor = previous_anchor;
    }

    let forward_burst = ((forward_units.len() as u32) / 6).clamp(1, 12) as usize;
    let mut forward_index = 0usize;
    let mut backward_index = 0usize;
    while forward_index < forward_units.len() || backward_index < backward_units.len() {
        for _ in 0..forward_burst {
            let Some(unit_anchor) = forward_units.get(forward_index).copied() else {
                break;
            };
            append_auto_unit_pages(plan, &mut desired, &mut seen, unit_anchor, total_pages);
            forward_index += 1;
        }

        if let Some(unit_anchor) = backward_units.get(backward_index).copied() {
            append_auto_unit_pages(plan, &mut desired, &mut seen, unit_anchor, total_pages);
            backward_index += 1;
        }

        if desired.len() as u32 >= total_pages {
            break;
        }
    }

    desired
}

fn append_unique_physical_pages<I>(
    desired: &mut Vec<u32>,
    seen: &mut HashSet<u32>,
    physical_pages: I,
    total_pages: u32,
) where
    I: IntoIterator<Item = u32>,
{
    for page in physical_pages
        .into_iter()
        .filter(|page| *page < total_pages)
    {
        if seen.insert(page) {
            desired.push(page);
        }
    }
}

fn append_auto_unit_pages(
    plan: &AutoSpreadPlan,
    desired: &mut Vec<u32>,
    seen: &mut HashSet<u32>,
    logical_page: u32,
    total_pages: u32,
) {
    if let Some((first_page, second_page)) = plan.pages_for_logical_page(logical_page) {
        append_unique_physical_pages(
            desired,
            seen,
            std::iter::once(first_page).chain(second_page),
            total_pages,
        );
    } else if logical_page < total_pages && seen.insert(logical_page) {
        desired.push(logical_page);
    }
}

/// Planner が返す dispatch / eviction の実行計画。
/// manager はこの結果を実行するだけで、優先順位は再計算しない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamingCacheStopReason {
    NoWorkerCapacity,
    CacheLimitUnavailable,
    CacheNotFullDispatch,
    PriorityImprovementDispatch,
    CacheFullNoPriorityImprovement,
    NoDispatchablePages,
}

/// Policy の順序と現在の cache / inflight / budget 状態を突き合わせた結果。
/// decode や worker 実行は持たず、manager に渡す実行候補だけを返す。
#[derive(Clone, Debug, Default)]
pub(super) struct StreamingCachePlan {
    pub(super) dispatch_pages: Vec<u32>,
    pub(super) evict_candidates: Vec<u32>,
    pub(super) stop_reason: Option<StreamingCacheStopReason>,
}

/// BG completion を現在の Desired Sequence で再評価する結果。
/// Planner は insert / eviction を実行せず、admit / drop の判断だけを返す。
/// drop は恒久失敗ではなく、現在順位で採用しないだけを表す。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamingCompletionDropReason {
    TooLargeForBgRgba,
    NotInDesiredSequence,
    NoPriorityImprovement,
    InsufficientLowPriorityCapacity,
}

#[derive(Clone, Debug, Default)]
pub(super) struct StreamingCompletionAdmissionPlan {
    pub(super) admit: bool,
    pub(super) drop_reason: Option<StreamingCompletionDropReason>,
    pub(super) evict_candidates: Vec<u32>,
    pub(super) completed_rank: Option<usize>,
    pub(super) worst_evictable_rank: Option<usize>,
}

/// BG completion の現在状態を Policy 順で再評価する入力。
/// Planner は cache / inflight の実行を持たず、現在の Desired Sequence との差だけを見る。
pub(super) struct StreamingCompletionAdmissionInput<'a> {
    pub(super) desired_sequence: &'a [u32],
    pub(super) cache_pages: &'a [u32],
    pub(super) page_eviction_bytes: &'a HashMap<u32, usize>,
    pub(super) cache_current_bytes: usize,
    pub(super) cache_max_bytes: usize,
    pub(super) completed_page: u32,
    pub(super) completed_entry_bytes: usize,
    pub(super) visible_pages: &'a HashSet<u32>,
    pub(super) protected_pages: &'a HashSet<u32>,
}

/// Policy の順序と現在状態から、何を dispatch するか・何を残すかだけを決める。
/// worker の実行や RGBA cache の保持責務は持たない。
pub(super) struct StreamingCachePlanner;

impl StreamingCachePlanner {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan(
        desired_sequence: Vec<u32>,
        bg_cache_pages: &[u32],
        cache_current_bytes: usize,
        cache_max_bytes: usize,
        cache_saturated: bool,
        candidate_limit: usize,
        inflight_pages: &HashSet<u32>,
        too_large_pages: &HashSet<u32>,
        visible_pages: &HashSet<u32>,
        protected_pages: &HashSet<u32>,
        worker_capacity: usize,
    ) -> StreamingCachePlan {
        let rank_state = build_streaming_desired_rank_state(&desired_sequence, bg_cache_pages);

        if worker_capacity == 0 {
            return StreamingCachePlan {
                stop_reason: Some(StreamingCacheStopReason::NoWorkerCapacity),
                ..StreamingCachePlan::default()
            };
        }

        if cache_max_bytes == 0 {
            return StreamingCachePlan {
                stop_reason: Some(StreamingCacheStopReason::CacheLimitUnavailable),
                ..StreamingCachePlan::default()
            };
        }

        let cache_is_full = cache_saturated || cache_current_bytes >= cache_max_bytes;
        let mut dispatch_pages = Vec::new();

        let should_dispatch = |page: u32| {
            !rank_state.cache_pages.contains(&page)
                && !inflight_pages.contains(&page)
                && !too_large_pages.contains(&page)
                && !visible_pages.contains(&page)
                && !protected_pages.contains(&page)
        };

        if !cache_is_full {
            for page in desired_sequence.iter().copied() {
                if dispatch_pages.len() >= candidate_limit {
                    break;
                }
                if !should_dispatch(page) {
                    continue;
                }
                dispatch_pages.push(page);
            }

            let evict_candidates = build_streaming_evict_candidates(StreamingEvictContext {
                bg_cache_pages,
                desired_pages: &rank_state.desired_pages,
                desired_ranks: &rank_state.desired_ranks,
                lowest_priority_rank: rank_state.lowest_priority_rank,
                visible_pages,
                protected_pages,
                desired_sequence: &desired_sequence,
                min_rank_exclusive: None,
            });
            let stop_reason = if dispatch_pages.is_empty() {
                Some(StreamingCacheStopReason::NoDispatchablePages)
            } else {
                Some(StreamingCacheStopReason::CacheNotFullDispatch)
            };

            return StreamingCachePlan {
                dispatch_pages,
                evict_candidates,
                stop_reason,
            };
        }

        let worst_evictable_rank = worst_evictable_rank(
            bg_cache_pages,
            &rank_state.desired_ranks,
            visible_pages,
            protected_pages,
            rank_state.lowest_priority_rank,
        );
        let evict_candidates = build_streaming_evict_candidates(StreamingEvictContext {
            bg_cache_pages,
            desired_pages: &rank_state.desired_pages,
            desired_ranks: &rank_state.desired_ranks,
            lowest_priority_rank: rank_state.lowest_priority_rank,
            visible_pages,
            protected_pages,
            desired_sequence: &desired_sequence,
            min_rank_exclusive: None,
        });

        if let Some(worst_rank) = worst_evictable_rank {
            for page in desired_sequence.iter().copied() {
                if dispatch_pages.len() >= candidate_limit {
                    break;
                }
                if !should_dispatch(page) {
                    continue;
                }
                let page_rank = rank_state
                    .desired_ranks
                    .get(&page)
                    .copied()
                    .unwrap_or(rank_state.lowest_priority_rank);
                if page_rank >= worst_rank {
                    break;
                }
                dispatch_pages.push(page);
            }
        }

        let stop_reason = if dispatch_pages.is_empty() {
            Some(StreamingCacheStopReason::CacheFullNoPriorityImprovement)
        } else {
            Some(StreamingCacheStopReason::PriorityImprovementDispatch)
        };

        StreamingCachePlan {
            dispatch_pages,
            evict_candidates,
            stop_reason,
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// BG completion を現在の Desired Sequence で再評価する。
    /// Planner は insert / eviction を実行せず、drop は恒久失敗ではない。
    pub(super) fn plan_completion_admission(
        input: StreamingCompletionAdmissionInput<'_>,
    ) -> StreamingCompletionAdmissionPlan {
        let rank_state =
            build_streaming_desired_rank_state(input.desired_sequence, input.cache_pages);
        let completed_rank = rank_state.desired_ranks.get(&input.completed_page).copied();
        let worst_evictable_rank = worst_evictable_rank(
            input.cache_pages,
            &rank_state.desired_ranks,
            input.visible_pages,
            input.protected_pages,
            rank_state.lowest_priority_rank,
        );
        let evict_candidates = build_streaming_evict_candidates(StreamingEvictContext {
            bg_cache_pages: input.cache_pages,
            desired_pages: &rank_state.desired_pages,
            desired_ranks: &rank_state.desired_ranks,
            lowest_priority_rank: rank_state.lowest_priority_rank,
            visible_pages: input.visible_pages,
            protected_pages: input.protected_pages,
            desired_sequence: input.desired_sequence,
            min_rank_exclusive: completed_rank,
        });
        let mut selected_evict_candidates = Vec::new();
        let mut selected_evict_bytes = 0usize;
        let mut admit = false;
        let mut drop_reason = None;

        if input.completed_entry_bytes > input.cache_max_bytes {
            drop_reason = Some(StreamingCompletionDropReason::TooLargeForBgRgba);
        } else if let Some(completed_rank) = completed_rank {
            if input
                .cache_current_bytes
                .saturating_add(input.completed_entry_bytes)
                <= input.cache_max_bytes
            {
                admit = true;
            } else {
                for page in evict_candidates.iter().copied() {
                    let page_bytes = input.page_eviction_bytes.get(&page).copied().unwrap_or(0);
                    if page_bytes == 0 {
                        continue;
                    }
                    selected_evict_candidates.push(page);
                    selected_evict_bytes = selected_evict_bytes.saturating_add(page_bytes);
                    if input
                        .cache_current_bytes
                        .saturating_add(input.completed_entry_bytes)
                        .saturating_sub(selected_evict_bytes)
                        <= input.cache_max_bytes
                    {
                        break;
                    }
                }
                let enough_low_priority_capacity = selected_evict_bytes > 0
                    && input
                        .cache_current_bytes
                        .saturating_add(input.completed_entry_bytes)
                        .saturating_sub(selected_evict_bytes)
                        <= input.cache_max_bytes;
                let has_priority_improvement =
                    worst_evictable_rank.is_some_and(|worst_rank| completed_rank < worst_rank);
                admit = enough_low_priority_capacity && has_priority_improvement;
                if !admit {
                    drop_reason = Some(if !has_priority_improvement {
                        if selected_evict_bytes == 0 {
                            StreamingCompletionDropReason::InsufficientLowPriorityCapacity
                        } else {
                            StreamingCompletionDropReason::NoPriorityImprovement
                        }
                    } else {
                        StreamingCompletionDropReason::InsufficientLowPriorityCapacity
                    });
                }
            }
        } else {
            drop_reason = Some(StreamingCompletionDropReason::NotInDesiredSequence);
        }

        StreamingCompletionAdmissionPlan {
            admit,
            drop_reason,
            evict_candidates: if admit {
                if input
                    .cache_current_bytes
                    .saturating_add(input.completed_entry_bytes)
                    <= input.cache_max_bytes
                {
                    Vec::new()
                } else {
                    selected_evict_candidates
                }
            } else {
                Vec::new()
            },
            completed_rank,
            worst_evictable_rank,
        }
    }
}
