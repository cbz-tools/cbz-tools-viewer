//! サムネイル用 in-RAM キャッシュ。
//!
//! `ThumbMemCache` はサムネイル専用の LRU を提供する。

use lru::LruCache;
use parking_lot::Mutex;

use crate::domain::archive::BookId;
use crate::domain::thumbnail::Thumbnail;

// ── ThumbMemCache ─────────────────────────────────────────────────────────────

/// サムネイル専用 LRU。キーは (BookId, target_width)。
pub struct ThumbMemCache {
    inner: Mutex<ThumbMemCacheInner>,
}

struct ThumbMemCacheInner {
    entries: LruCache<(BookId, u16), Thumbnail>,
    current_bytes: usize,
    max_bytes: usize,
}

impl ThumbMemCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(ThumbMemCacheInner {
                entries: LruCache::unbounded(),
                current_bytes: 0,
                max_bytes: max_bytes.max(1),
            }),
        }
    }

    pub fn get(&self, id: &BookId, target_width: u16) -> Option<Thumbnail> {
        self.inner
            .lock()
            .entries
            .get(&(id.clone(), target_width))
            .cloned()
    }

    pub fn put(&self, id: BookId, target_width: u16, thumb: Thumbnail) {
        let mut inner = self.inner.lock();
        let key = (id, target_width);
        if let Some(old) = inner.entries.pop(&key) {
            inner.current_bytes = inner.current_bytes.saturating_sub(old.pixels.len());
        }
        inner.current_bytes = inner.current_bytes.saturating_add(thumb.pixels.len());
        inner.entries.put(key, thumb);
        while inner.current_bytes > inner.max_bytes {
            let Some((_evicted_key, evicted_thumb)) = inner.entries.pop_lru() else {
                break;
            };
            inner.current_bytes = inner
                .current_bytes
                .saturating_sub(evicted_thumb.pixels.len());
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.entries.clear();
        inner.current_bytes = 0;
    }

    pub fn remove_by_book_id(&self, id: &BookId) -> usize {
        let mut inner = self.inner.lock();
        let keys: Vec<(BookId, u16)> = inner
            .entries
            .iter()
            .filter_map(|(key, _)| {
                if key.0 == *id {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();
        let removed = keys.len();
        for key in keys {
            if let Some(old) = inner.entries.pop(&key) {
                inner.current_bytes = inner.current_bytes.saturating_sub(old.pixels.len());
            }
        }
        removed
    }
}
