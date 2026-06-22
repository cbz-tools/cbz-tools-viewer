//! 表示済み texture の短期履歴。
//! warmup cache と分け、現在表示に近い再利用だけを担う。
use std::collections::{HashMap, VecDeque};

use eframe::egui;

use super::working_set::{
    page_render_signature_rank, DisplayRequirement, GpuTextureEntrySnapshot, PageRenderSignatureKey,
};

pub(super) type GpuTextureKey = PageRenderSignatureKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GpuTextureHitKind {
    Exact,
    Suitable,
}

impl GpuTextureHitKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Suitable => "suitable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GpuTextureHitSource {
    History,
    Warmup,
}

impl GpuTextureHitSource {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Warmup => "warmup",
        }
    }
}

#[derive(Clone)]
pub(super) struct GpuTextureHit {
    pub(super) texture: egui::TextureHandle,
    pub(super) key: GpuTextureKey,
    pub(super) hit_kind: GpuTextureHitKind,
    pub(super) source: GpuTextureHitSource,
    pub(super) estimated_bytes: usize,
    pub(super) texture_width: usize,
    pub(super) texture_height: usize,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GpuTextureHistorySnapshot {
    pub(super) current_bytes: usize,
    pub(super) max_bytes: usize,
    pub(super) entry_count: usize,
    pub(super) insert_count: u64,
    pub(super) evict_count: u64,
    pub(super) full_hit_count: u64,
    pub(super) miss_count: u64,
    pub(super) partial_spread_hit_count: u64,
    pub(super) partial_reuse_count: u64,
    pub(super) clear_count: u64,
    pub(super) last_clear_reason: Option<&'static str>,
}

struct CachedGpuTexture {
    texture: egui::TextureHandle,
    estimated_bytes: usize,
}

pub(super) struct GpuTextureHistory {
    entries: HashMap<GpuTextureKey, CachedGpuTexture>,
    lru: VecDeque<GpuTextureKey>,
    current_bytes: usize,
    max_bytes: usize,
    insert_count: u64,
    evict_count: u64,
    full_hit_count: u64,
    miss_count: u64,
    partial_spread_hit_count: u64,
    partial_reuse_count: u64,
    clear_count: u64,
    last_clear_reason: Option<&'static str>,
}

impl GpuTextureHistory {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            current_bytes: 0,
            max_bytes,
            insert_count: 0,
            evict_count: 0,
            full_hit_count: 0,
            miss_count: 0,
            partial_spread_hit_count: 0,
            partial_reuse_count: 0,
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
                "gpu-history-skip-oversized"
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

        let event_reason = if replaced.is_some() {
            "replace"
        } else {
            "insert"
        };
        let event_name = if replaced.is_some() {
            "gpu-history-replace"
        } else {
            "gpu-history-insert"
        };
        tracing::trace!(
            page = key.page,
            current_mb = %format_bytes_mb(self.current_bytes),
            max_mb = %format_bytes_mb(self.max_bytes),
            entries = self.entries.len(),
            reason = event_reason,
            "{event_name}"
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
                "gpu-history-clear"
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
    pub(super) fn snapshot(&self) -> GpuTextureHistorySnapshot {
        GpuTextureHistorySnapshot {
            current_bytes: self.current_bytes,
            max_bytes: self.max_bytes,
            entry_count: self.entries.len(),
            insert_count: self.insert_count,
            evict_count: self.evict_count,
            full_hit_count: self.full_hit_count,
            miss_count: self.miss_count,
            partial_spread_hit_count: self.partial_spread_hit_count,
            partial_reuse_count: self.partial_reuse_count,
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
        self.full_hit_count = self.full_hit_count.saturating_add(1);
    }

    pub(super) fn record_miss(&mut self) {
        self.miss_count = self.miss_count.saturating_add(1);
    }

    pub(super) fn record_partial_spread_hit(&mut self) {
        self.partial_spread_hit_count = self.partial_spread_hit_count.saturating_add(1);
    }

    pub(super) fn record_partial_reuse(&mut self) {
        self.partial_reuse_count = self.partial_reuse_count.saturating_add(1);
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
            source: GpuTextureHitSource::History,
            estimated_bytes: entry.estimated_bytes,
            texture_width,
            texture_height,
        })
    }

    pub(super) fn touch(&mut self, key: &GpuTextureKey) -> bool {
        if !self.entries.contains_key(key) {
            return false;
        }
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(*key);
        true
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
            tracing::trace!(
                page = key.page,
                current_mb = %format_bytes_mb(self.current_bytes),
                max_mb = %format_bytes_mb(self.max_bytes),
                entries = self.entries.len(),
                reason = "capacity",
                "gpu-history-evict"
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
