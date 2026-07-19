//! BG RGBA Cache を供給元とする future 専用 GPU warmup cache。
//! 表示済み texture は持たず、未来候補の保持と rank 改善だけを担う。
use std::collections::{HashMap, VecDeque};

use eframe::egui;

use super::gpu_texture_history::{
    GpuTextureHit, GpuTextureHitKind, GpuTextureHitSource, GpuTextureKey,
};
use super::working_set::{DisplayRequirement, GpuTextureEntrySnapshot, page_render_signature_rank};

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GpuWarmupCacheSnapshot {
    pub(super) current_bytes: usize,
    pub(super) max_bytes: usize,
    pub(super) entry_count: usize,
    pub(super) insert_count: u64,
    pub(super) evict_count: u64,
    pub(super) hit_count: u64,
    pub(super) promotion_count: u64,
    pub(super) unused_evict_count: u64,
    pub(super) clear_count: u64,
    pub(super) last_clear_reason: Option<&'static str>,
}

struct CachedGpuTexture {
    texture: egui::TextureHandle,
    estimated_bytes: usize,
}

pub(super) struct GpuWarmupCache {
    entries: HashMap<GpuTextureKey, CachedGpuTexture>,
    lru: VecDeque<GpuTextureKey>,
    current_bytes: usize,
    max_bytes: usize,
    insert_count: u64,
    evict_count: u64,
    hit_count: u64,
    promotion_count: u64,
    unused_evict_count: u64,
    clear_count: u64,
    last_clear_reason: Option<&'static str>,
}

impl GpuWarmupCache {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            current_bytes: 0,
            max_bytes,
            insert_count: 0,
            evict_count: 0,
            hit_count: 0,
            promotion_count: 0,
            unused_evict_count: 0,
            clear_count: 0,
            last_clear_reason: None,
        }
    }

    pub(super) fn insert(
        &mut self,
        key: GpuTextureKey,
        texture: egui::TextureHandle,
        estimated_bytes: usize,
    ) -> bool {
        if estimated_bytes > self.max_bytes {
            tracing::trace!(
                page = key.page,
                current_mb = %format_bytes_mb(self.current_bytes),
                max_mb = %format_bytes_mb(self.max_bytes),
                entries = self.entries.len(),
                reason = "estimated_bytes_gt_max",
                "gpu-warmup-skip"
            );
            return false;
        }

        let replaced = self.entries.remove(&key);
        if let Some(old) = replaced.as_ref() {
            self.current_bytes = self.current_bytes.saturating_sub(old.estimated_bytes);
            self.lru.retain(|existing| existing != &key);
        }

        self.current_bytes = self.current_bytes.saturating_add(estimated_bytes);
        self.lru.push_back(key);
        self.entries.insert(
            key,
            CachedGpuTexture {
                texture,
                estimated_bytes,
            },
        );
        self.insert_count = self.insert_count.saturating_add(1);
        self.evict_to_budget();

        tracing::trace!(
            page = key.page,
            current_mb = %format_bytes_mb(self.current_bytes),
            max_mb = %format_bytes_mb(self.max_bytes),
            entries = self.entries.len(),
            source = "warmup",
            reason = if replaced.is_some() { "replace" } else { "insert" },
            "gpu-warmup-insert"
        );
        true
    }

    pub(super) fn clear(&mut self, reason: &'static str) {
        let had_entries = !self.entries.is_empty();
        self.entries.clear();
        self.lru.clear();
        self.current_bytes = 0;
        self.last_clear_reason = Some(reason);
        if had_entries {
            self.clear_count = self.clear_count.saturating_add(1);
            tracing::trace!(
                current_mb = %format_bytes_mb(self.current_bytes),
                max_mb = %format_bytes_mb(self.max_bytes),
                entries = self.entries.len(),
                reason = reason,
                "gpu-warmup-clear"
            );
        }
    }

    pub(super) fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    pub(super) fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[cfg(debug_assertions)]
    pub(super) fn snapshot(&self) -> GpuWarmupCacheSnapshot {
        GpuWarmupCacheSnapshot {
            current_bytes: self.current_bytes,
            max_bytes: self.max_bytes,
            entry_count: self.entries.len(),
            insert_count: self.insert_count,
            evict_count: self.evict_count,
            hit_count: self.hit_count,
            promotion_count: self.promotion_count,
            unused_evict_count: self.unused_evict_count,
            clear_count: self.clear_count,
            last_clear_reason: self.last_clear_reason,
        }
    }

    pub(super) fn entry_snapshots(&self) -> Vec<GpuTextureEntrySnapshot> {
        self.lru
            .iter()
            .filter_map(|key| {
                self.entries.get(key).map(|entry| GpuTextureEntrySnapshot {
                    key: *key,
                    page: key.page,
                    bytes: entry.estimated_bytes,
                })
            })
            .collect()
    }

    pub(super) fn record_hit(&mut self) {
        self.hit_count = self.hit_count.saturating_add(1);
    }

    pub(super) fn peek_suitable(
        &self,
        page: u32,
        requirement: DisplayRequirement,
    ) -> Option<GpuTextureHit> {
        let key = self.best_suitable_candidate(page, requirement)?;
        let entry = self.entries.get(&key)?;
        let [texture_width, texture_height] = entry.texture.size();
        let hit_kind = if key.render_signature.target_w == requirement.required_w
            && key.render_signature.target_h == requirement.required_h
        {
            GpuTextureHitKind::Exact
        } else {
            GpuTextureHitKind::Suitable
        };
        Some(GpuTextureHit {
            texture: entry.texture.clone(),
            key,
            hit_kind,
            source: GpuTextureHitSource::Warmup,
            estimated_bytes: entry.estimated_bytes,
            texture_width,
            texture_height,
        })
    }

    pub(super) fn promote_to_history(
        &mut self,
        key: &GpuTextureKey,
    ) -> Option<(egui::TextureHandle, usize)> {
        self.lru.retain(|existing| existing != key);
        let old = self.entries.remove(key)?;
        self.current_bytes = self.current_bytes.saturating_sub(old.estimated_bytes);
        self.promotion_count = self.promotion_count.saturating_add(1);
        tracing::trace!(
            page = key.page,
            current_mb = %format_bytes_mb(self.current_bytes),
            max_mb = %format_bytes_mb(self.max_bytes),
            entries = self.entries.len(),
            source = "warmup",
            reason = "display_commit",
            "gpu-warmup-promote"
        );
        Some((old.texture, old.estimated_bytes))
    }

    pub(super) fn remove_without_promotion(
        &mut self,
        key: &GpuTextureKey,
    ) -> Option<(egui::TextureHandle, usize)> {
        self.lru.retain(|existing| existing != key);
        let old = self.entries.remove(key)?;
        self.current_bytes = self.current_bytes.saturating_sub(old.estimated_bytes);
        Some((old.texture, old.estimated_bytes))
    }

    fn evict_to_budget(&mut self) {
        while self.current_bytes > self.max_bytes {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            let Some(old) = self.entries.remove(&key) else {
                continue;
            };
            self.current_bytes = self.current_bytes.saturating_sub(old.estimated_bytes);
            self.evict_count = self.evict_count.saturating_add(1);
            self.unused_evict_count = self.unused_evict_count.saturating_add(1);
            tracing::trace!(
                page = key.page,
                current_mb = %format_bytes_mb(self.current_bytes),
                max_mb = %format_bytes_mb(self.max_bytes),
                entries = self.entries.len(),
                source = "warmup",
                reason = "capacity",
                "gpu-warmup-evict"
            );
        }
    }

    fn best_suitable_candidate(
        &self,
        page: u32,
        requirement: DisplayRequirement,
    ) -> Option<GpuTextureKey> {
        let mut best: Option<((u64, u32, u32, usize), GpuTextureKey)> = None;
        for (candidate, entry) in &self.entries {
            let Some(rank) = page_render_signature_rank(
                candidate.page,
                candidate.render_signature,
                page,
                requirement,
                entry.estimated_bytes,
            ) else {
                continue;
            };
            if best.as_ref().is_none_or(|(prev, _)| rank < *prev) {
                best = Some((rank, *candidate));
            }
        }
        best.map(|(_, key)| key)
    }
}

fn format_bytes_mb(bytes: usize) -> String {
    const MB: u128 = 1024 * 1024;
    let mb_x10 = ((bytes as u128) * 10 + MB / 2) / MB;
    let whole = mb_x10 / 10;
    let frac = mb_x10 % 10;
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}
