//! サムネイル用 in-RAM キャッシュ。
//!
//! `ThumbMemCache` はサムネイル専用の LRU を提供する。

use std::num::NonZeroUsize;

use lru::LruCache;
use parking_lot::Mutex;

use crate::domain::archive::BookId;
use crate::domain::thumbnail::Thumbnail;

// ── ThumbMemCache ─────────────────────────────────────────────────────────────

/// サムネイル専用 LRU。キーは (BookId, target_width)。
pub struct ThumbMemCache {
    inner: Mutex<LruCache<(BookId, u16), Thumbnail>>,
}

impl ThumbMemCache {
    pub fn new(capacity: usize) -> Self {
        let cap = match NonZeroUsize::new(capacity.max(1)) {
            Some(v) => v,
            None => NonZeroUsize::MIN,
        };
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn get(&self, id: &BookId, target_width: u16) -> Option<Thumbnail> {
        self.inner.lock().get(&(id.clone(), target_width)).cloned()
    }

    pub fn put(&self, id: BookId, target_width: u16, thumb: Thumbnail) {
        self.inner.lock().put((id, target_width), thumb);
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    pub fn remove_by_book_id(&self, id: &BookId) -> usize {
        let mut inner = self.inner.lock();
        let keys: Vec<(BookId, u16)> = inner
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
            let _ = inner.pop(&key);
        }
        removed
    }
}
