//! BG RGBA Cache を供給元とする future 専用 GPU L1 の計画器。
//! 毎 frame 理想集合へ完全同期せず、空きがある間は追加し、
//! 満杯時は現在保持中の worst rank より近い candidate だけを順位改善として置換する。
//! 過去 page は History の責務。

use std::collections::HashMap;

use super::state::RgbaCacheKey;
use super::working_set::{
    page_render_signature_rank, DisplayRequirement, GpuTextureEntrySnapshot,
    PageRenderSignatureKey, RenderSignature,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RgbaReadyEntrySnapshot {
    pub(super) key: RgbaCacheKey,
    pub(super) page: u32,
    pub(super) bytes: usize,
    pub(super) signature: RenderSignature,
    pub(super) source: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GpuWarmupCandidateSnapshot {
    pub(super) rgba_key: RgbaCacheKey,
    pub(super) key: PageRenderSignatureKey,
    pub(super) page: u32,
    pub(super) bytes: usize,
    pub(super) distance: u32,
    pub(super) requirement: DisplayRequirement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GpuWarmupEvictCandidate {
    pub(super) key: PageRenderSignatureKey,
    pub(super) page: u32,
    pub(super) bytes: usize,
    pub(super) reason: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GpuWarmupUploadCandidate {
    pub(super) rgba_key: RgbaCacheKey,
    pub(super) key: PageRenderSignatureKey,
    pub(super) page: u32,
    pub(super) bytes: usize,
    pub(super) distance: u32,
    pub(super) requirement: DisplayRequirement,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GpuWarmupPlanSnapshot {
    pub(super) visible_end: Option<u32>,
    pub(super) l2_ready_count: usize,
    pub(super) future_candidate_count: usize,
    pub(super) warm_count: usize,
    pub(super) warm_bytes: usize,
    pub(super) free_bytes: usize,
    pub(super) best_missing_page: Option<u32>,
    pub(super) worst_warm_page: Option<u32>,
    pub(super) replacement_needed: usize,
    pub(super) replacement_count: usize,
    pub(super) stale_evict_count: usize,
    pub(super) upload_count: usize,
    pub(super) pending_uploads: usize,
    pub(super) upload_page: Option<u32>,
    pub(super) upload_bytes: usize,
    pub(super) upload_mode: Option<&'static str>,
    pub(super) evict_count: usize,
    pub(super) idle_reason: Option<&'static str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct GpuWarmupPlan {
    summary: GpuWarmupPlanSnapshot,
    pub(super) stale_evict_candidates: Vec<GpuWarmupEvictCandidate>,
    pub(super) replacement_evict_candidates: Vec<GpuWarmupEvictCandidate>,
    pub(super) upload_candidate: Option<GpuWarmupUploadCandidate>,
}

impl GpuWarmupPlan {
    pub(super) fn summary(&self) -> GpuWarmupPlanSnapshot {
        self.summary
    }
}

pub(super) fn best_ready_candidate(
    page: u32,
    requirement: DisplayRequirement,
    ready_entries: &[RgbaReadyEntrySnapshot],
) -> Option<RgbaReadyEntrySnapshot> {
    let mut best: Option<((u64, u32, u32, usize), RgbaReadyEntrySnapshot)> = None;
    for candidate in ready_entries {
        let Some(rank) = page_render_signature_rank(
            candidate.page,
            candidate.signature,
            page,
            requirement,
            candidate.bytes,
        ) else {
            continue;
        };
        if best.as_ref().is_none_or(|(prev, _)| rank < *prev) {
            best = Some((rank, candidate.clone()));
        }
    }
    best.map(|(_, candidate)| candidate)
}

pub(super) fn resolve_future_candidate(
    page: u32,
    distance: u32,
    requirement: DisplayRequirement,
    ready_entries: &[RgbaReadyEntrySnapshot],
) -> Option<GpuWarmupCandidateSnapshot> {
    let ready = best_ready_candidate(page, requirement, ready_entries)?;
    Some(GpuWarmupCandidateSnapshot {
        rgba_key: ready.key.clone(),
        key: PageRenderSignatureKey {
            page,
            render_signature: ready.signature,
        },
        page,
        bytes: ready.bytes,
        distance,
        requirement,
    })
}

fn entry_satisfies_requirement(
    entry: &GpuTextureEntrySnapshot,
    page: u32,
    requirement: DisplayRequirement,
) -> bool {
    page_render_signature_rank(
        entry.page,
        entry.key.render_signature,
        page,
        requirement,
        entry.bytes,
    )
    .is_some()
}

fn candidate_is_satisfied(
    candidate: &GpuWarmupCandidateSnapshot,
    warm_entries: &[GpuTextureEntrySnapshot],
    history_entries: &[GpuTextureEntrySnapshot],
) -> bool {
    warm_entries
        .iter()
        .any(|entry| entry_satisfies_requirement(entry, candidate.page, candidate.requirement))
        || history_entries
            .iter()
            .any(|entry| entry_satisfies_requirement(entry, candidate.page, candidate.requirement))
}

pub(super) fn plan_gpu_warmup(
    visible_end: u32,
    l2_ready_count: usize,
    warm_capacity_bytes: usize,
    candidates: &[GpuWarmupCandidateSnapshot],
    current_warm_entries: &[GpuTextureEntrySnapshot],
    history_entries: &[GpuTextureEntrySnapshot],
    desired_future_requirements: &HashMap<u32, DisplayRequirement>,
) -> GpuWarmupPlan {
    let mut ordered_candidates = candidates.to_vec();
    ordered_candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.distance,
            candidate.page,
            candidate.key.render_signature.target_w,
            candidate.key.render_signature.target_h,
            candidate.bytes,
        )
    });

    let mut stale_evict_candidates = Vec::new();
    let mut retained_warm_entries = Vec::new();
    for entry in current_warm_entries {
        if entry.page <= visible_end {
            stale_evict_candidates.push(GpuWarmupEvictCandidate {
                key: entry.key,
                page: entry.page,
                bytes: entry.bytes,
                reason: "past",
            });
            continue;
        }

        let Some(requirement) = desired_future_requirements.get(&entry.page).copied() else {
            stale_evict_candidates.push(GpuWarmupEvictCandidate {
                key: entry.key,
                page: entry.page,
                bytes: entry.bytes,
                reason: "outside_desired_future",
            });
            continue;
        };

        if entry_satisfies_requirement(entry, entry.page, requirement) {
            retained_warm_entries.push(*entry);
        } else {
            stale_evict_candidates.push(GpuWarmupEvictCandidate {
                key: entry.key,
                page: entry.page,
                bytes: entry.bytes,
                reason: "requirement_mismatch",
            });
        }
    }

    let retained_warm_bytes = retained_warm_entries
        .iter()
        .fold(0usize, |acc, entry| acc.saturating_add(entry.bytes));
    let free_bytes = warm_capacity_bytes.saturating_sub(retained_warm_bytes);
    let worst_warm_page = retained_warm_entries.iter().map(|entry| entry.page).max();

    let mut upload_candidate = None;
    let mut replacement_evict_candidates = Vec::new();
    let mut best_missing_page = None;
    let mut replacement_needed = 0usize;
    let mut upload_mode = None;
    let mut had_oversized_candidate = false;

    for candidate in &ordered_candidates {
        if candidate_is_satisfied(candidate, &retained_warm_entries, history_entries) {
            continue;
        }
        if candidate.bytes > warm_capacity_bytes {
            had_oversized_candidate = true;
            continue;
        }

        best_missing_page = Some(candidate.page);
        if candidate.bytes <= free_bytes {
            upload_mode = Some("free-space");
            upload_candidate = Some(GpuWarmupUploadCandidate {
                rgba_key: candidate.rgba_key.clone(),
                key: candidate.key,
                page: candidate.page,
                bytes: candidate.bytes,
                distance: candidate.distance,
                requirement: candidate.requirement,
            });
            break;
        }

        let needed = candidate.bytes.saturating_sub(free_bytes);
        let mut replaceable = retained_warm_entries
            .iter()
            .filter(|entry| {
                entry.page > candidate.page
                    || (entry.page == candidate.page
                        && !entry_satisfies_requirement(
                            entry,
                            candidate.page,
                            candidate.requirement,
                        ))
            })
            .copied()
            .collect::<Vec<_>>();
        replaceable.sort_unstable_by_key(|entry| {
            (
                std::cmp::Reverse(entry.page),
                std::cmp::Reverse(entry.bytes),
                std::cmp::Reverse(entry.key.render_signature.target_w),
                std::cmp::Reverse(entry.key.render_signature.target_h),
            )
        });
        let replaceable_bytes = replaceable
            .iter()
            .fold(0usize, |acc, entry| acc.saturating_add(entry.bytes));
        if replaceable_bytes < needed {
            replacement_needed = needed;
            break;
        }

        let mut freed = 0usize;
        for entry in replaceable {
            if freed >= needed {
                break;
            }
            freed = freed.saturating_add(entry.bytes);
            replacement_evict_candidates.push(GpuWarmupEvictCandidate {
                key: entry.key,
                page: entry.page,
                bytes: entry.bytes,
                reason: "rank_replacement",
            });
        }
        replacement_needed = needed;
        upload_mode = Some("rank-replacement");
        upload_candidate = Some(GpuWarmupUploadCandidate {
            rgba_key: candidate.rgba_key.clone(),
            key: candidate.key,
            page: candidate.page,
            bytes: candidate.bytes,
            distance: candidate.distance,
            requirement: candidate.requirement,
        });
        break;
    }

    let upload_count = usize::from(upload_candidate.is_some());
    let pending_uploads = upload_candidate
        .as_ref()
        .map(|candidate| {
            ordered_candidates
                .iter()
                .skip_while(|entry| entry.page <= candidate.page)
                .filter(|entry| {
                    !candidate_is_satisfied(entry, &retained_warm_entries, history_entries)
                })
                .count()
        })
        .unwrap_or(0);
    let evict_count = stale_evict_candidates.len() + replacement_evict_candidates.len();

    let idle_reason = if upload_count == 0 && evict_count == 0 {
        if ordered_candidates.is_empty() {
            Some("no-l2-ready")
        } else if had_oversized_candidate && best_missing_page.is_none() {
            Some("no-suitable-entry")
        } else if best_missing_page.is_some() {
            Some("no-rank-improvement")
        } else {
            Some("already-cached")
        }
    } else {
        None
    };

    let summary = GpuWarmupPlanSnapshot {
        visible_end: Some(visible_end),
        l2_ready_count,
        future_candidate_count: ordered_candidates.len(),
        warm_count: retained_warm_entries.len(),
        warm_bytes: retained_warm_bytes,
        free_bytes,
        best_missing_page,
        worst_warm_page,
        replacement_needed,
        replacement_count: replacement_evict_candidates.len(),
        stale_evict_count: stale_evict_candidates.len(),
        upload_count,
        pending_uploads,
        upload_page: upload_candidate.as_ref().map(|candidate| candidate.page),
        upload_bytes: upload_candidate
            .as_ref()
            .map(|candidate| candidate.bytes)
            .unwrap_or(0),
        upload_mode,
        evict_count,
        idle_reason,
    };

    GpuWarmupPlan {
        summary,
        stale_evict_candidates,
        replacement_evict_candidates,
        upload_candidate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::app_settings::ViewerQuality;

    fn make_signature(width: u32, height: u32) -> RenderSignature {
        RenderSignature::from_decode_request(ViewerQuality::Balanced, width, height, 4096)
    }

    fn make_entry(page: u32, width: u32, height: u32) -> GpuTextureEntrySnapshot {
        GpuTextureEntrySnapshot {
            key: PageRenderSignatureKey {
                page,
                render_signature: make_signature(width, height),
            },
            page,
            bytes: 256,
        }
    }

    #[test]
    fn resolve_future_candidate_requires_ready_entry() {
        let ready_entries = vec![RgbaReadyEntrySnapshot {
            key: RgbaCacheKey {
                page: 10,
                render_signature: make_signature(1200, 600),
            },
            page: 10,
            bytes: 256,
            signature: make_signature(1200, 600),
            source: "ready",
        }];
        let requirement =
            DisplayRequirement::from_display_request(ViewerQuality::Balanced, 1100, 550, 4096);
        assert!(resolve_future_candidate(10, 5, requirement, &ready_entries).is_some());
        assert!(resolve_future_candidate(10, 5, requirement, &[]).is_none());
    }

    #[test]
    fn warm_entries_survive_when_still_desired_even_without_ready_supply() {
        let warm_entries = vec![make_entry(10, 1000, 500)];
        let mut desired_future_requirements = HashMap::new();
        desired_future_requirements.insert(
            10,
            DisplayRequirement::from_display_request(ViewerQuality::Balanced, 1000, 500, 4096),
        );

        let plan = plan_gpu_warmup(
            5,
            0,
            1024,
            &[],
            &warm_entries,
            &[],
            &desired_future_requirements,
        );

        assert_eq!(plan.summary().stale_evict_count, 0);
        assert_eq!(plan.summary().warm_count, 1);
        assert!(plan.stale_evict_candidates.is_empty());
    }

    #[test]
    fn warm_entries_outside_desired_future_are_evicted() {
        let warm_entries = vec![make_entry(10, 1000, 500)];
        let desired_future_requirements = HashMap::new();

        let plan = plan_gpu_warmup(
            5,
            0,
            1024,
            &[],
            &warm_entries,
            &[],
            &desired_future_requirements,
        );

        assert_eq!(plan.summary().stale_evict_count, 1);
        assert_eq!(
            plan.stale_evict_candidates[0].reason,
            "outside_desired_future"
        );
        assert_eq!(plan.summary().warm_count, 0);
    }
}
