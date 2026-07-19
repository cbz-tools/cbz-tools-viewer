use std::path::Path;
use std::sync::Arc;

use crate::domain::archive::BookMeta;
use crate::domain::page_map::{BookPageMap, SourceRevision};
use crate::infra::archive::epub::{
    EpubImageReader, EpubPageMapFastOutcome, build_book_page_map_fast_from_epub_reader,
};
use crate::infra::archive::page_map::{
    FolderPageMapFastStatus, FolderPageMapSlowOutcome, build_book_page_map_fast_from_folder_path,
    build_book_page_map_slow_from_folder_path, build_zip_page_map_fast,
};
use crate::infra::archive::{BookSourceKind, book_source_kind};
use crate::infra::cache::artifact_failure::{ArtifactFailureDiskCache, ArtifactKind};
use crate::infra::cache::page_map::PageMapDiskCache;

#[derive(Clone, Debug)]
pub enum ViewerPageMapMode {
    Mapped(Arc<BookPageMap>),
    Unavailable,
}

/// SPAD target用のcache-only Page Map参照。生成・保存・cache修復は行わない。
pub fn try_load_existing_viewer_page_map_for_spad(path: &Path) -> Option<Arc<BookPageMap>> {
    if matches!(book_source_kind(path), BookSourceKind::Unsupported) {
        return None;
    }
    let metadata = std::fs::metadata(path).ok()?;
    let revision = SourceRevision::from_file_state(metadata.len(), metadata.modified().ok());
    let id = crate::domain::archive::BookId::from_path(path);
    let cache = open_existing_page_map_cache()?;
    cache
        .get_existing_page_map_for_revision(&id, &revision)
        .map(Arc::new)
}

/// Viewer 起動時に Page Map を cache / FAST で確定する。
/// ここで unavailable なら後から Complete へ切り替えない。
pub fn bootstrap_viewer_page_map(entry: &BookMeta, map_make_skip: bool) -> ViewerPageMapMode {
    let revision = source_revision_for_entry(entry);
    let source_kind = book_source_kind(entry.path.as_ref());
    let cache = open_page_map_cache();
    let failure_cache = open_artifact_failure_cache();
    if cache.is_none() {
        tracing::debug!(
            path = %entry.path.display(),
            "viewer page map bootstrap cache unavailable"
        );
    }

    if source_kind != BookSourceKind::Epub {
        if let Some(cache) = cache.as_ref() {
            if let Some(page_map) = cache.get_page_map_for_revision(&entry.id, &revision) {
                tracing::debug!(
                    path = %entry.path.display(),
                    page_count = page_map.page_count(),
                    "viewer page map bootstrap cache hit"
                );
                clear_page_map_failure(failure_cache.as_ref(), entry, &revision);
                return ViewerPageMapMode::Mapped(Arc::new(page_map));
            }
        }
    }

    if failure_cache.as_ref().is_some_and(|failure_cache| {
        failure_cache.has_failure_for_revision(&entry.id, &revision, ArtifactKind::PageMap)
    }) {
        tracing::debug!(
            path = %entry.path.display(),
            source_revision = ?revision,
            "viewer page map bootstrap skipped by failure cache"
        );
        return ViewerPageMapMode::Unavailable;
    }

    if map_make_skip && source_kind != BookSourceKind::Epub {
        tracing::debug!(
            path = %entry.path.display(),
            "viewer page map bootstrap skipped fast generation"
        );
        return ViewerPageMapMode::Unavailable;
    }

    match source_kind {
        BookSourceKind::Zip => {
            let fast = build_zip_page_map_fast(entry.path.as_ref(), revision.clone());
            if !matches!(
                fast.status,
                crate::infra::page_map::build::PageMapBuildStatus::Ready
            ) {
                if matches!(
                    fast.status,
                    crate::infra::page_map::build::PageMapBuildStatus::Failed(_)
                ) {
                    mark_page_map_failure(failure_cache.as_ref(), entry, &revision);
                }
                tracing::debug!(
                    path = %entry.path.display(),
                    status = ?fast.status,
                    "viewer page map bootstrap zip fast unavailable"
                );
                return ViewerPageMapMode::Unavailable;
            }
            let Some(page_map) = fast.page_map else {
                tracing::debug!(
                    path = %entry.path.display(),
                    "viewer page map bootstrap zip fast missing page map"
                );
                return ViewerPageMapMode::Unavailable;
            };

            if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
                tracing::debug!(
                    path = %entry.path.display(),
                    "viewer page map bootstrap zip source changed before save"
                );
                return ViewerPageMapMode::Unavailable;
            }

            clear_page_map_failure(failure_cache.as_ref(), entry, &revision);

            if let Some(cache) = cache.as_ref() {
                let page_map_bytes = page_map.encode_cache_bytes();
                if cache
                    .put_page_map_bytes_for_revision(&entry.id, &revision, &page_map_bytes)
                    .is_err()
                {
                    tracing::debug!(
                        path = %entry.path.display(),
                        "viewer page map bootstrap zip cache save failed"
                    );
                }
            }

            if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
                tracing::debug!(
                    path = %entry.path.display(),
                    "viewer page map bootstrap zip source changed after save"
                );
                return ViewerPageMapMode::Unavailable;
            }

            tracing::debug!(
                path = %entry.path.display(),
                page_count = page_map.page_count(),
                "viewer page map bootstrap zip fast mapped"
            );
            ViewerPageMapMode::Mapped(Arc::new(page_map))
        }
        BookSourceKind::Rar => {
            tracing::debug!(
                path = %entry.path.display(),
                "viewer page map bootstrap unavailable for rar/cbr cache miss"
            );
            ViewerPageMapMode::Unavailable
        }
        BookSourceKind::Folder => {
            let fast =
                build_book_page_map_fast_from_folder_path(entry.path.as_ref(), revision.clone());
            match fast.status {
                FolderPageMapFastStatus::Ready => {
                    let Some(page_map) = fast.page_map else {
                        tracing::debug!(
                            path = %entry.path.display(),
                            "viewer page map bootstrap folder fast missing page map"
                        );
                        return ViewerPageMapMode::Unavailable;
                    };

                    if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
                        tracing::debug!(
                            path = %entry.path.display(),
                            "viewer page map bootstrap folder source changed before save"
                        );
                        return ViewerPageMapMode::Unavailable;
                    }

                    clear_page_map_failure(failure_cache.as_ref(), entry, &revision);

                    if let Some(cache) = cache.as_ref() {
                        let page_map_bytes = page_map.encode_cache_bytes();
                        if cache
                            .put_page_map_bytes_for_revision(&entry.id, &revision, &page_map_bytes)
                            .is_err()
                        {
                            tracing::debug!(
                                path = %entry.path.display(),
                                "viewer page map bootstrap folder cache save failed"
                            );
                        }
                    }

                    if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
                        tracing::debug!(
                            path = %entry.path.display(),
                            "viewer page map bootstrap folder source changed after save"
                        );
                        return ViewerPageMapMode::Unavailable;
                    }

                    tracing::debug!(
                        path = %entry.path.display(),
                        page_count = page_map.page_count(),
                        "viewer page map bootstrap folder fast mapped"
                    );
                    ViewerPageMapMode::Mapped(Arc::new(page_map))
                }
                FolderPageMapFastStatus::RequiresComplete => {
                    tracing::debug!(
                        path = %entry.path.display(),
                        "viewer page map bootstrap folder fast requires complete"
                    );
                    match build_book_page_map_slow_from_folder_path(
                        entry.path.as_ref(),
                        revision.clone(),
                    ) {
                        FolderPageMapSlowOutcome::Success(page_map) => {
                            if current_source_revision(entry.path.as_ref())
                                != Some(source_revision_for_entry(entry))
                            {
                                tracing::debug!(
                                    path = %entry.path.display(),
                                    "viewer page map bootstrap folder source changed before slow save"
                                );
                                return ViewerPageMapMode::Unavailable;
                            }

                            clear_page_map_failure(failure_cache.as_ref(), entry, &revision);

                            if let Some(cache) = cache.as_ref() {
                                let page_map_bytes = page_map.encode_cache_bytes();
                                if cache
                                    .put_page_map_bytes_for_revision(
                                        &entry.id,
                                        &source_revision_for_entry(entry),
                                        &page_map_bytes,
                                    )
                                    .is_err()
                                {
                                    tracing::debug!(
                                        path = %entry.path.display(),
                                        "viewer page map bootstrap folder slow cache save failed"
                                    );
                                }
                            }

                            if current_source_revision(entry.path.as_ref())
                                != Some(source_revision_for_entry(entry))
                            {
                                tracing::debug!(
                                    path = %entry.path.display(),
                                    "viewer page map bootstrap folder source changed after slow save"
                                );
                                return ViewerPageMapMode::Unavailable;
                            }

                            tracing::debug!(
                                path = %entry.path.display(),
                                page_count = page_map.page_count(),
                                "viewer page map bootstrap folder slow mapped"
                            );
                            ViewerPageMapMode::Mapped(Arc::new(page_map))
                        }
                        FolderPageMapSlowOutcome::Failure(failure) => {
                            tracing::debug!(
                                path = %entry.path.display(),
                                page_index = ?failure.page_index,
                                entry_index = ?failure.entry_index,
                                reason = ?failure.reason,
                                "viewer page map bootstrap folder slow unavailable"
                            );
                            mark_page_map_failure(failure_cache.as_ref(), entry, &revision);
                            ViewerPageMapMode::Unavailable
                        }
                    }
                }
                FolderPageMapFastStatus::Failed => {
                    tracing::debug!(
                        path = %entry.path.display(),
                        "viewer page map bootstrap folder fast unavailable"
                    );
                    mark_page_map_failure(failure_cache.as_ref(), entry, &revision);
                    ViewerPageMapMode::Unavailable
                }
            }
        }
        BookSourceKind::Unsupported => {
            tracing::debug!(
                path = %entry.path.display(),
                "viewer page map bootstrap unavailable for unsupported source"
            );
            ViewerPageMapMode::Unavailable
        }
        BookSourceKind::Epub => {
            let reader = match EpubImageReader::open(entry.path.as_ref()) {
                Ok(reader) => reader,
                Err(e) => {
                    tracing::debug!(
                        path = %entry.path.display(),
                        "viewer page map bootstrap epub reader unavailable: {e:#}"
                    );
                    return ViewerPageMapMode::Unavailable;
                }
            };

            if let Some(cache) = cache.as_ref() {
                if let Some(page_map) = cache.get_page_map_for_revision(&entry.id, &revision) {
                    if page_map.page_count() == reader.page_count() as usize {
                        tracing::debug!(
                            path = %entry.path.display(),
                            page_count = page_map.page_count(),
                            "viewer page map bootstrap epub cache hit"
                        );
                        clear_page_map_failure(failure_cache.as_ref(), entry, &revision);
                        return ViewerPageMapMode::Mapped(Arc::new(page_map));
                    }

                    tracing::debug!(
                        path = %entry.path.display(),
                        cached_page_count = page_map.page_count(),
                        reader_page_count = reader.page_count(),
                        "viewer page map bootstrap epub cache page count mismatch"
                    );
                }
            }

            if map_make_skip {
                tracing::debug!(
                    path = %entry.path.display(),
                    "viewer page map bootstrap skipped epub fast generation"
                );
                return ViewerPageMapMode::Unavailable;
            }

            let fast = build_book_page_map_fast_from_epub_reader(&reader, revision.clone());
            match fast {
                EpubPageMapFastOutcome::Ready(page_map) => {
                    if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
                        tracing::debug!(
                            path = %entry.path.display(),
                            "viewer page map bootstrap epub source changed before save"
                        );
                        return ViewerPageMapMode::Unavailable;
                    }

                    clear_page_map_failure(failure_cache.as_ref(), entry, &revision);

                    if let Some(cache) = cache.as_ref() {
                        let page_map_bytes = page_map.encode_cache_bytes();
                        if cache
                            .put_page_map_bytes_for_revision(&entry.id, &revision, &page_map_bytes)
                            .is_err()
                        {
                            tracing::debug!(
                                path = %entry.path.display(),
                                "viewer page map bootstrap epub cache save failed"
                            );
                        }
                    }

                    if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
                        tracing::debug!(
                            path = %entry.path.display(),
                            "viewer page map bootstrap epub source changed after save"
                        );
                        return ViewerPageMapMode::Unavailable;
                    }

                    tracing::debug!(
                        path = %entry.path.display(),
                        page_count = page_map.page_count(),
                        "viewer page map bootstrap epub fast mapped"
                    );
                    ViewerPageMapMode::Mapped(Arc::new(page_map))
                }
                EpubPageMapFastOutcome::RequiresComplete => {
                    // Viewer は EPUB の complete / slow Page Map を作らない。
                    // FAST が通らなければ reader を優先して unavailable にする。
                    tracing::debug!(
                        path = %entry.path.display(),
                        "viewer page map bootstrap epub fast requires complete"
                    );
                    ViewerPageMapMode::Unavailable
                }
            }
        }
    }
}

fn open_page_map_cache() -> Option<PageMapDiskCache> {
    PageMapDiskCache::open(PageMapDiskCache::default_root())
        .or_else(|_| {
            PageMapDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("page_maps"),
            )
        })
        .ok()
}

fn open_existing_page_map_cache() -> Option<PageMapDiskCache> {
    let default_root = PageMapDiskCache::default_root();
    PageMapDiskCache::open_existing(default_root)
        .ok()
        .or_else(|| {
            PageMapDiskCache::open_existing(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("page_maps"),
            )
            .ok()
        })
}

fn open_artifact_failure_cache() -> Option<ArtifactFailureDiskCache> {
    ArtifactFailureDiskCache::open(ArtifactFailureDiskCache::default_root())
        .or_else(|_| {
            ArtifactFailureDiskCache::open(
                std::env::temp_dir()
                    .join(crate::app_identity::app_data_dir())
                    .join("artifact_failures"),
            )
        })
        .ok()
}

fn mark_page_map_failure(
    cache: Option<&ArtifactFailureDiskCache>,
    entry: &BookMeta,
    revision: &SourceRevision,
) {
    if current_source_revision(entry.path.as_ref()) != Some(revision.clone()) {
        return;
    }
    if let Some(cache) = cache {
        match cache.mark_failure_for_revision(&entry.id, revision, ArtifactKind::PageMap) {
            Ok(true) => {
                tracing::debug!(
                    path = %entry.path.display(),
                    source_revision = ?revision,
                    "viewer page map terminal failure cached"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    path = %entry.path.display(),
                    error = %error,
                    "viewer page map failure cache save failed"
                );
            }
        }
    }
}

fn clear_page_map_failure(
    cache: Option<&ArtifactFailureDiskCache>,
    entry: &BookMeta,
    revision: &SourceRevision,
) {
    if let Some(cache) = cache {
        match cache.clear_failure_for_revision(&entry.id, revision, ArtifactKind::PageMap) {
            Ok(true) => {
                tracing::debug!(
                    path = %entry.path.display(),
                    source_revision = ?revision,
                    "viewer page map failure cache cleared after success"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    path = %entry.path.display(),
                    error = %error,
                    "viewer page map failure cache clear failed"
                );
            }
        }
    }
}

fn source_revision_for_entry(entry: &BookMeta) -> SourceRevision {
    SourceRevision::from_file_state(entry.size, Some(entry.modified))
}

fn current_source_revision(path: &Path) -> Option<SourceRevision> {
    let meta = std::fs::metadata(path).ok()?;
    Some(SourceRevision::from_file_state(
        meta.len(),
        meta.modified().ok(),
    ))
}
