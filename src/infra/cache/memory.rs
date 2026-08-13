//! サムネイル用 in-RAM キャッシュ。
//!
//! `ThumbMemCache` はサムネイル専用の LRU を提供する。

use lru::LruCache;
use parking_lot::Mutex;

use crate::domain::archive::BookId;
use crate::domain::page_map::SourceRevision;
use crate::domain::thumbnail::Thumbnail;

// ── ThumbMemCache ─────────────────────────────────────────────────────────────

/// サムネイル専用 LRU。キーは (BookId, target_width)。
pub struct ThumbMemCache {
    inner: Mutex<ThumbMemCacheInner>,
}

struct ThumbMemCacheInner {
    entries: LruCache<(BookId, u16), CachedThumbnail>,
    current_bytes: usize,
    max_bytes: usize,
}

struct CachedThumbnail {
    thumbnail: Thumbnail,
    revision: Option<SourceRevision>,
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
            .map(|entry| entry.thumbnail.clone())
    }

    pub fn get_for_revision(
        &self,
        id: &BookId,
        target_width: u16,
        revision: &SourceRevision,
    ) -> Option<Thumbnail> {
        self.inner
            .lock()
            .entries
            .get(&(id.clone(), target_width))
            .filter(|entry| entry.revision.as_ref() == Some(revision))
            .map(|entry| entry.thumbnail.clone())
    }

    pub fn put(&self, id: BookId, target_width: u16, thumb: Thumbnail) {
        self.put_with_revision(id, target_width, thumb, None);
    }

    pub fn put_for_revision(
        &self,
        id: BookId,
        target_width: u16,
        thumb: Thumbnail,
        revision: SourceRevision,
    ) {
        self.put_with_revision(id, target_width, thumb, Some(revision));
    }

    fn put_with_revision(
        &self,
        id: BookId,
        target_width: u16,
        thumb: Thumbnail,
        revision: Option<SourceRevision>,
    ) {
        let mut inner = self.inner.lock();
        let key = (id, target_width);
        if let Some(old) = inner.entries.pop(&key) {
            inner.current_bytes = inner
                .current_bytes
                .saturating_sub(old.thumbnail.pixels.len());
        }
        inner.current_bytes = inner.current_bytes.saturating_add(thumb.pixels.len());
        inner.entries.put(
            key,
            CachedThumbnail {
                thumbnail: thumb,
                revision,
            },
        );
        while inner.current_bytes > inner.max_bytes {
            let Some((_evicted_key, evicted_thumb)) = inner.entries.pop_lru() else {
                break;
            };
            inner.current_bytes = inner
                .current_bytes
                .saturating_sub(evicted_thumb.thumbnail.pixels.len());
        }
    }

    /// Remove revision-tagged entries for a source that are not the current revision.
    /// Untagged normal thumbnails remain governed by the existing cache behavior.
    pub fn prune_revisions_except(&self, id: &BookId, revision: &SourceRevision) -> usize {
        let mut inner = self.inner.lock();
        let keys: Vec<(BookId, u16)> = inner
            .entries
            .iter()
            .filter(|(key, entry)| {
                key.0 == *id
                    && entry
                        .revision
                        .as_ref()
                        .is_some_and(|cached| cached != revision)
            })
            .map(|(key, _)| key.clone())
            .collect();
        let removed = keys.len();
        for key in keys {
            if let Some(old) = inner.entries.pop(&key) {
                inner.current_bytes = inner
                    .current_bytes
                    .saturating_sub(old.thumbnail.pixels.len());
            }
        }
        removed
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
                inner.current_bytes = inner
                    .current_bytes
                    .saturating_sub(old.thumbnail.pixels.len());
            }
        }
        removed
    }
}
