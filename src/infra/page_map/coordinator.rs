use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tokio::sync::Semaphore;

use crate::domain::archive::BookId;
use crate::domain::page_map::{BookPageMap, PageDescriptor, SourceRevision};
use crate::infra::archive::page_map::{
    FolderPageMapSlowOutcome, build_book_page_map_slow_from_folder_path,
};
#[cfg(feature = "rar")]
use crate::infra::archive::page_map::{
    RarPageMapSlowOutcome, build_book_page_map_slow_from_rar_path,
};
use crate::infra::archive::page_map::{
    ZipPageMapFastStatus, ZipPageMapSlowFailureReason, ZipPageMapSlowOutcome, ZipPageMapSlowReason,
    build_book_page_map_slow_from_zip_reader,
};
use crate::infra::archive::{
    BookSourceKind, book_source_kind, epub::build_book_page_map_slow_from_epub_path,
};
use crate::infra::cache::artifact_failure::{ArtifactFailureDiskCache, ArtifactKind};
use crate::infra::cache::page_map::PageMapDiskCache;
use crate::infra::page_map::build::{PageMapBuildStatus, assemble_zip_fast_page_map};

#[derive(Clone, Debug)]
pub struct PageMapStatus {
    pub book_id: BookId,
    pub source_revision: SourceRevision,
    pub failed: bool,
}

type PageMapStatusNotifier = Arc<dyn Fn(PageMapStatus) + Send + Sync>;

/// Page Map の生成結果を cache に反映し、stale と重複 complete を抑止する。
#[derive(Clone)]
pub struct PageMapCoordinator {
    generation: Arc<AtomicU64>,
    artifact_generation: Arc<AtomicU64>,
    artifact_gate: Arc<RwLock<()>>,
    artifact_failure_cache: Option<Arc<ArtifactFailureDiskCache>>,
    status_notifier: Option<PageMapStatusNotifier>,
    page_map_slow_states: Arc<Mutex<HashMap<PageMapTaskKey, PageMapSlowState>>>,
    page_map_complete_permit: Arc<Semaphore>,
}

impl PageMapCoordinator {
    pub fn new(
        generation: Arc<AtomicU64>,
        artifact_generation: Arc<AtomicU64>,
        artifact_gate: Arc<RwLock<()>>,
        artifact_failure_cache: Option<Arc<ArtifactFailureDiskCache>>,
        status_notifier: Option<PageMapStatusNotifier>,
    ) -> Self {
        Self {
            generation,
            artifact_generation,
            artifact_gate,
            artifact_failure_cache,
            status_notifier,
            page_map_slow_states: Arc::new(Mutex::new(HashMap::new())),
            page_map_complete_permit: Arc::new(Semaphore::new(1)),
        }
    }

    /// 同一 task の slow / complete を 1 回に絞る。persistable でない revision は予約しない。
    pub fn reserve_page_map_slow(&self, key: &PageMapTaskKey) -> bool {
        if !key.source_revision.is_persistable() {
            return false;
        }
        let mut guard = match self.page_map_slow_states.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!("page map slow state mutex poisoned");
                return false;
            }
        };
        let state = guard
            .entry(key.clone())
            .or_insert(PageMapSlowState::NotStarted);
        match state {
            PageMapSlowState::NotStarted => {
                *state = PageMapSlowState::QueuedOrRunning;
                true
            }
            PageMapSlowState::QueuedOrRunning
            | PageMapSlowState::Succeeded
            | PageMapSlowState::Failed
            | PageMapSlowState::Stale => false,
        }
    }

    pub fn reserve_page_map_complete_request(&self, request: &PageMapCompleteRequest) -> bool {
        self.reserve_page_map_slow(&PageMapTaskKey::from_request(request))
    }

    pub fn clear_all(&self) {
        if let Ok(mut guard) = self.page_map_slow_states.lock() {
            guard.clear();
        }
    }

    pub fn remove_by_book_id(&self, id: &BookId) {
        if let Ok(mut guard) = self.page_map_slow_states.lock() {
            guard.retain(|key, _| &key.book_id != id);
        }
    }

    pub fn record_page_map_terminal_failure(&self, id: &BookId, revision: &SourceRevision) {
        self.mark_page_map_failure(id, revision);
    }

    /// FAST 結果を保存する。Complete が必要なら 1 回だけ slow 側へ引き渡す。
    pub async fn complete_fast(&self, request: PageMapFastPersistRequest) {
        let request_for_log = (
            request.book_id.clone(),
            Arc::clone(&request.source_path),
            request.source_revision.clone(),
            request.task_generation,
            request.task_artifact_generation,
        );
        let coordinator = self.clone();
        match tokio::task::spawn_blocking(move || coordinator.complete_fast_blocking(request)).await
        {
            Ok(PageMapFastPersistOutcome::Saved | PageMapFastPersistOutcome::Failed) => {}
            Ok(PageMapFastPersistOutcome::RequiresComplete(request)) => {
                if self.reserve_page_map_complete_request(&request) {
                    self.complete(request).await;
                }
            }
            Err(join_error) => {
                let (
                    book_id,
                    source_path,
                    source_revision,
                    task_generation,
                    task_artifact_generation,
                ) = request_for_log;
                tracing::error!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    source_revision = ?source_revision,
                    generation = task_generation,
                    artifact_generation = task_artifact_generation,
                    join_error = %join_error,
                    "page-map fast worker join error"
                );
            }
        }
    }

    pub async fn complete_ready(&self, request: PageMapReadyPersistRequest) {
        let request_for_log = (
            request.book_id.clone(),
            Arc::clone(&request.source_path),
            request.source_revision.clone(),
            request.task_generation,
            request.task_artifact_generation,
        );
        let coordinator = self.clone();
        match tokio::task::spawn_blocking(move || coordinator.complete_ready_blocking(request))
            .await
        {
            Ok(()) => {}
            Err(join_error) => {
                let (
                    book_id,
                    source_path,
                    source_revision,
                    task_generation,
                    task_artifact_generation,
                ) = request_for_log;
                tracing::error!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    source_revision = ?source_revision,
                    generation = task_generation,
                    artifact_generation = task_artifact_generation,
                    join_error = %join_error,
                    "page-map ready persist worker join error"
                );
            }
        }
    }

    pub async fn complete(&self, request: PageMapCompleteRequest) {
        let request_key = PageMapTaskKey::from_task(
            request.book_id.clone(),
            request.source_revision.clone(),
            request.task_generation,
            request.task_artifact_generation,
        );
        let book_id = request.book_id.clone();
        let source_path = request.source_path.clone();
        let source_revision = request.source_revision.clone();
        let task_generation = request.task_generation;
        let task_artifact_generation = request.task_artifact_generation;
        let permit = match Arc::clone(&self.page_map_complete_permit)
            .acquire_owned()
            .await
        {
            Ok(permit) => permit,
            Err(e) => {
                tracing::debug!(
                    id = %request.book_id.0.to_hex(),
                    error = %e,
                    "page-map slow skipped because permit acquisition failed"
                );
                let _ = self.finish_page_map_slow_state(
                    &PageMapTaskKey::from_task(
                        request.book_id.clone(),
                        request.source_revision.clone(),
                        request.task_generation,
                        request.task_artifact_generation,
                    ),
                    PageMapSlowState::Failed,
                );
                return;
            }
        };

        let coordinator = self.clone();
        match tokio::task::spawn_blocking(move || {
            coordinator.complete_blocking(request, permit);
        })
        .await
        {
            Ok(()) => {}
            Err(join_error) => {
                tracing::error!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    source_revision = ?source_revision,
                    generation = task_generation,
                    artifact_generation = task_artifact_generation,
                    join_error = %join_error,
                    "page-map slow worker join error"
                );
                let _ = self.finish_page_map_slow_state(&request_key, PageMapSlowState::Failed);
            }
        }
    }

    fn complete_fast_blocking(
        &self,
        request: PageMapFastPersistRequest,
    ) -> PageMapFastPersistOutcome {
        let PageMapFastPersistRequest {
            book_id,
            source_path,
            source_revision,
            task_generation,
            task_artifact_generation,
            page_count,
            fast_lane_status,
            fast_lane_pages,
            page_map_cache,
        } = request;
        if !source_revision.is_persistable() {
            tracing::debug!(
                id = %book_id.0.to_hex(),
                path = %source_path.display(),
                "page-map fast skipped because source revision is not persistable"
            );
            return PageMapFastPersistOutcome::Failed;
        }
        if let Some(stale_reason) = self.page_map_stale_reason(
            task_generation,
            task_artifact_generation,
            &source_path,
            &source_revision,
        ) {
            self.log_page_map_stale(&book_id, &source_path, stale_reason);
            return PageMapFastPersistOutcome::Failed;
        }

        let started = Instant::now();
        let assembled = assemble_zip_fast_page_map(
            source_revision.clone(),
            page_count,
            fast_lane_status,
            fast_lane_pages,
        );
        match assembled.status {
            PageMapBuildStatus::Ready => {
                let Some(page_map) = assembled.page_map else {
                    tracing::debug!(
                        id = %book_id.0.to_hex(),
                        path = %source_path.display(),
                        "page-map fast ready without page map"
                    );
                    return PageMapFastPersistOutcome::Failed;
                };

                if let Some(stale_reason) = self.page_map_stale_reason(
                    task_generation,
                    task_artifact_generation,
                    &source_path,
                    &source_revision,
                ) {
                    self.log_page_map_stale(&book_id, &source_path, stale_reason);
                    return PageMapFastPersistOutcome::Failed;
                }

                let _gate = self.artifact_gate.read();
                if let Some(stale_reason) = self.page_map_stale_reason(
                    task_generation,
                    task_artifact_generation,
                    &source_path,
                    &source_revision,
                ) {
                    self.log_page_map_stale(&book_id, &source_path, stale_reason);
                    return PageMapFastPersistOutcome::Failed;
                }

                let page_map_page_count = page_map.page_count();
                self.clear_page_map_failure(&book_id, &source_revision);
                let page_map_bytes = page_map.encode_cache_bytes();
                let persist_outcome = match page_map_cache.put_page_map_bytes_for_revision(
                    &book_id,
                    &source_revision,
                    &page_map_bytes,
                ) {
                    Ok(()) => {
                        tracing::debug!(
                            id = %book_id.0.to_hex(),
                            path = %source_path.display(),
                            page_count = page_count,
                            page_map_pages = page_map_page_count,
                            elapsed_ms = started.elapsed().as_millis(),
                            "page-map fast complete"
                        );
                        PageMapFastPersistOutcome::Saved
                    }
                    Err(e) => {
                        tracing::debug!(
                            id = %book_id.0.to_hex(),
                            path = %source_path.display(),
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %e,
                            "page-map cache save failed"
                        );
                        PageMapFastPersistOutcome::Failed
                    }
                };
                persist_outcome
            }
            PageMapBuildStatus::RequiresComplete(reason) => {
                tracing::debug!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    reason = ?reason,
                    "page-map fast requires complete"
                );
                PageMapFastPersistOutcome::RequiresComplete(PageMapCompleteRequest {
                    book_id,
                    source_path,
                    source_revision,
                    task_generation,
                    task_artifact_generation,
                    page_count: Some(page_count),
                    reason: Some(reason),
                    page_map_cache,
                })
            }
            PageMapBuildStatus::Failed(reason) => {
                tracing::debug!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    reason = ?reason,
                    "page-map fast failed"
                );
                self.mark_page_map_failure(&book_id, &source_revision);
                PageMapFastPersistOutcome::Failed
            }
        }
    }

    fn complete_ready_blocking(&self, request: PageMapReadyPersistRequest) {
        let PageMapReadyPersistRequest {
            book_id,
            source_path,
            source_revision,
            task_generation,
            task_artifact_generation,
            page_map,
            page_map_cache,
        } = request;
        if !source_revision.is_persistable() {
            tracing::debug!(
                id = %book_id.0.to_hex(),
                path = %source_path.display(),
                "page-map ready persist skipped because source revision is not persistable"
            );
            return;
        }

        let started = Instant::now();
        if let Some(stale_reason) = self.page_map_stale_reason(
            task_generation,
            task_artifact_generation,
            &source_path,
            &source_revision,
        ) {
            self.log_page_map_stale(&book_id, &source_path, stale_reason);
            return;
        }

        let _gate = self.artifact_gate.read();
        if let Some(stale_reason) = self.page_map_stale_reason(
            task_generation,
            task_artifact_generation,
            &source_path,
            &source_revision,
        ) {
            self.log_page_map_stale(&book_id, &source_path, stale_reason);
            return;
        }

        let page_map_page_count = page_map.page_count();
        self.clear_page_map_failure(&book_id, &source_revision);
        let page_map_bytes = page_map.encode_cache_bytes();
        match page_map_cache.put_page_map_bytes_for_revision(
            &book_id,
            &source_revision,
            &page_map_bytes,
        ) {
            Ok(()) => {
                tracing::debug!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    page_map_pages = page_map_page_count,
                    elapsed_ms = started.elapsed().as_millis(),
                    "page-map ready persist complete"
                );
            }
            Err(e) => {
                tracing::debug!(
                    id = %book_id.0.to_hex(),
                    path = %source_path.display(),
                    elapsed_ms = started.elapsed().as_millis(),
                    error = %e,
                    "page-map cache save failed"
                );
            }
        };
    }

    fn complete_blocking(
        &self,
        request: PageMapCompleteRequest,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let source_revision = request.source_revision.clone();
        let key = PageMapTaskKey::from_task(
            request.book_id.clone(),
            source_revision.clone(),
            request.task_generation,
            request.task_artifact_generation,
        );
        if !source_revision.is_persistable() {
            tracing::debug!(
                id = %request.book_id.0.to_hex(),
                path = %request.source_path.display(),
                "page-map slow skipped because source revision is not persistable"
            );
            let _ = self.finish_page_map_slow_state(&key, PageMapSlowState::Failed);
            return;
        }

        let started = Instant::now();
        let page_count = request.page_count.unwrap_or(0);
        let source_kind = book_source_kind(&request.source_path);
        tracing::debug!(
            id = %request.book_id.0.to_hex(),
            path = %request.source_path.display(),
            page_count = page_count,
            source_revision = ?source_revision,
            reason = ?request.reason,
            "page-map slow start"
        );

        match source_kind {
            BookSourceKind::Folder => {
                match build_book_page_map_slow_from_folder_path(
                    &request.source_path,
                    source_revision.clone(),
                ) {
                    FolderPageMapSlowOutcome::Success(page_map) => {
                        debug_assert!(!page_map.is_empty());
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }

                        let _gate = self.artifact_gate.read();
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }

                        let page_map_page_count = page_map.page_count();
                        let page_map_bytes = page_map.encode_cache_bytes();
                        match request.page_map_cache.put_page_map_bytes_for_revision(
                            &request.book_id,
                            &source_revision,
                            &page_map_bytes,
                        ) {
                            Ok(()) => {
                                let _ = self
                                    .finish_page_map_slow_state(&key, PageMapSlowState::Succeeded);
                                tracing::debug!(
                                    id = %request.book_id.0.to_hex(),
                                    path = %request.source_path.display(),
                                    page_map_pages = page_map_page_count,
                                    elapsed_ms = started.elapsed().as_millis(),
                                    "page-map slow complete"
                                );
                            }
                            Err(e) => {
                                tracing::debug!(
                                    id = %request.book_id.0.to_hex(),
                                    path = %request.source_path.display(),
                                    elapsed_ms = started.elapsed().as_millis(),
                                    error = %e,
                                    "page-map cache save failed"
                                );
                                let _ =
                                    self.finish_page_map_slow_state(&key, PageMapSlowState::Failed);
                            }
                        };
                    }
                    FolderPageMapSlowOutcome::Failure(failure) => {
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }
                        tracing::debug!(
                            id = %request.book_id.0.to_hex(),
                            path = %request.source_path.display(),
                            page_index = ?failure.page_index,
                            entry_index = ?failure.entry_index,
                            reason = ?failure.reason,
                            elapsed_ms = started.elapsed().as_millis(),
                            "page-map slow failed"
                        );
                        self.finish_page_map_terminal_failure(&request, &key);
                    }
                }
                return;
            }
            #[cfg(feature = "rar")]
            BookSourceKind::Rar => {
                match build_book_page_map_slow_from_rar_path(
                    &request.source_path,
                    source_revision.clone(),
                ) {
                    RarPageMapSlowOutcome::Success(page_map) => {
                        debug_assert!(!page_map.is_empty());
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }

                        let _gate = self.artifact_gate.read();
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }

                        let page_map_page_count = page_map.page_count();
                        let page_map_bytes = page_map.encode_cache_bytes();
                        match request.page_map_cache.put_page_map_bytes_for_revision(
                            &request.book_id,
                            &source_revision,
                            &page_map_bytes,
                        ) {
                            Ok(()) => {
                                let _ = self
                                    .finish_page_map_slow_state(&key, PageMapSlowState::Succeeded);
                                tracing::debug!(
                                    id = %request.book_id.0.to_hex(),
                                    path = %request.source_path.display(),
                                    page_map_pages = page_map_page_count,
                                    elapsed_ms = started.elapsed().as_millis(),
                                    "page-map slow complete"
                                );
                            }
                            Err(e) => {
                                tracing::debug!(
                                    id = %request.book_id.0.to_hex(),
                                    path = %request.source_path.display(),
                                    elapsed_ms = started.elapsed().as_millis(),
                                    error = %e,
                                    "page-map cache save failed"
                                );
                                let _ =
                                    self.finish_page_map_slow_state(&key, PageMapSlowState::Failed);
                            }
                        };
                    }
                    RarPageMapSlowOutcome::Failure(failure) => {
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }
                        tracing::debug!(
                            id = %request.book_id.0.to_hex(),
                            path = %request.source_path.display(),
                            page_index = ?failure.page_index,
                            entry_index = ?failure.entry_index,
                            reason = ?failure.reason,
                            elapsed_ms = started.elapsed().as_millis(),
                            "page-map slow failed"
                        );
                        self.finish_page_map_terminal_failure(&request, &key);
                    }
                }
                return;
            }
            #[cfg(not(feature = "rar"))]
            BookSourceKind::Rar => {}
            BookSourceKind::Epub => {
                match build_book_page_map_slow_from_epub_path(
                    &request.source_path,
                    source_revision.clone(),
                ) {
                    Ok(page_map) => {
                        debug_assert!(!page_map.is_empty());
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }

                        let _gate = self.artifact_gate.read();
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }

                        let page_map_page_count = page_map.page_count();
                        let page_map_bytes = page_map.encode_cache_bytes();
                        match request.page_map_cache.put_page_map_bytes_for_revision(
                            &request.book_id,
                            &source_revision,
                            &page_map_bytes,
                        ) {
                            Ok(()) => {
                                let _ = self
                                    .finish_page_map_slow_state(&key, PageMapSlowState::Succeeded);
                                tracing::debug!(
                                    id = %request.book_id.0.to_hex(),
                                    path = %request.source_path.display(),
                                    page_count = page_count,
                                    page_map_pages = page_map_page_count,
                                    elapsed_ms = started.elapsed().as_millis(),
                                    "page-map slow complete"
                                );
                            }
                            Err(e) => {
                                tracing::debug!(
                                    id = %request.book_id.0.to_hex(),
                                    path = %request.source_path.display(),
                                    elapsed_ms = started.elapsed().as_millis(),
                                    error = %e,
                                    "page-map cache save failed"
                                );
                                let _ =
                                    self.finish_page_map_slow_state(&key, PageMapSlowState::Failed);
                            }
                        };
                    }
                    Err(failure) => {
                        if let Some(stale_reason) =
                            self.page_map_stale_reason_from_slow_request(&request)
                        {
                            self.finish_page_map_stale(&request, &key, stale_reason);
                            return;
                        }
                        tracing::debug!(
                            id = %request.book_id.0.to_hex(),
                            path = %request.source_path.display(),
                            page_index = ?failure.page_index,
                            image_path = ?failure.image_path,
                            elapsed_ms = started.elapsed().as_millis(),
                            "page-map slow failed"
                        );
                        self.finish_page_map_terminal_failure(&request, &key);
                    }
                }
                return;
            }
            BookSourceKind::Zip | BookSourceKind::Unsupported => {}
        }

        let reader = match crate::infra::archive::zip::ZipReader::open(&request.source_path) {
            Ok(reader) => reader,
            Err(_) => {
                tracing::debug!(
                    id = %request.book_id.0.to_hex(),
                    path = %request.source_path.display(),
                    reason = ?ZipPageMapSlowFailureReason::ZipOpenError,
                    "page-map slow failed"
                );
                self.finish_page_map_terminal_failure(&request, &key);
                return;
            }
        };
        let page_count = request.page_count.unwrap_or_else(|| reader.page_count());
        let outcome = build_book_page_map_slow_from_zip_reader(&reader, source_revision.clone());
        match outcome {
            ZipPageMapSlowOutcome::Success(page_map) => {
                debug_assert!(!page_map.is_empty());
                if let Some(stale_reason) = self.page_map_stale_reason_from_slow_request(&request) {
                    self.finish_page_map_stale(&request, &key, stale_reason);
                    return;
                }

                let _gate = self.artifact_gate.read();
                if let Some(stale_reason) = self.page_map_stale_reason_from_slow_request(&request) {
                    self.finish_page_map_stale(&request, &key, stale_reason);
                    return;
                }

                let page_map_page_count = page_map.page_count();
                let page_map_bytes = page_map.encode_cache_bytes();
                match request.page_map_cache.put_page_map_bytes_for_revision(
                    &request.book_id,
                    &source_revision,
                    &page_map_bytes,
                ) {
                    Ok(()) => {
                        let _ = self.finish_page_map_slow_state(&key, PageMapSlowState::Succeeded);
                        tracing::debug!(
                            id = %request.book_id.0.to_hex(),
                            path = %request.source_path.display(),
                            page_count = page_count,
                            page_map_pages = page_map_page_count,
                            elapsed_ms = started.elapsed().as_millis(),
                            "page-map slow complete"
                        );
                    }
                    Err(e) => {
                        tracing::debug!(
                            id = %request.book_id.0.to_hex(),
                            path = %request.source_path.display(),
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %e,
                            "page-map cache save failed"
                        );
                        let _ = self.finish_page_map_slow_state(&key, PageMapSlowState::Failed);
                    }
                };
            }
            ZipPageMapSlowOutcome::Failure(failure) => {
                if let Some(stale_reason) = self.page_map_stale_reason_from_slow_request(&request) {
                    self.finish_page_map_stale(&request, &key, stale_reason);
                    return;
                }
                tracing::debug!(
                    id = %request.book_id.0.to_hex(),
                    path = %request.source_path.display(),
                    page_index = ?failure.page_index,
                    entry_index = ?failure.entry_index,
                    reason = ?failure.reason,
                    elapsed_ms = started.elapsed().as_millis(),
                    "page-map slow failed"
                );
                self.finish_page_map_terminal_failure(&request, &key);
            }
        }
    }

    fn finish_page_map_terminal_failure(
        &self,
        request: &PageMapCompleteRequest,
        key: &PageMapTaskKey,
    ) {
        self.mark_page_map_failure(&request.book_id, &request.source_revision);
        let _ = self.finish_page_map_slow_state(key, PageMapSlowState::Failed);
    }

    fn mark_page_map_failure(&self, id: &BookId, revision: &SourceRevision) {
        if let Some(cache) = self.artifact_failure_cache.as_ref() {
            match cache.mark_failure_for_revision(id, revision, ArtifactKind::PageMap) {
                Ok(true) => {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        source_revision = ?revision,
                        "page-map terminal failure cached"
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        source_revision = ?revision,
                        error = %error,
                        "page-map failure cache save failed"
                    );
                }
            }
        }
        self.notify_status(id, revision, true);
    }

    fn clear_page_map_failure(&self, id: &BookId, revision: &SourceRevision) {
        if let Some(cache) = self.artifact_failure_cache.as_ref() {
            match cache.clear_failure_for_revision(id, revision, ArtifactKind::PageMap) {
                Ok(true) => {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        source_revision = ?revision,
                        "page-map failure cache cleared after success"
                    );
                    self.notify_status(id, revision, false);
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::debug!(
                        id = %id.0.to_hex(),
                        source_revision = ?revision,
                        error = %error,
                        "page-map failure cache clear failed"
                    );
                }
            }
        }
    }

    fn notify_status(&self, id: &BookId, revision: &SourceRevision, failed: bool) {
        if let Some(notifier) = self.status_notifier.as_ref() {
            notifier(PageMapStatus {
                book_id: id.clone(),
                source_revision: revision.clone(),
                failed,
            });
        }
    }

    fn finish_page_map_slow_state(
        &self,
        key: &PageMapTaskKey,
        next_state: PageMapSlowState,
    ) -> bool {
        let mut guard = match self.page_map_slow_states.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::error!("page map slow state mutex poisoned");
                return false;
            }
        };
        let Some(state) = guard.get_mut(key) else {
            return false;
        };
        if *state != PageMapSlowState::QueuedOrRunning {
            return false;
        }
        if next_state == PageMapSlowState::Succeeded {
            self.clear_page_map_failure(&key.book_id, &key.source_revision);
        }
        *state = next_state;
        true
    }

    fn page_map_stale_reason(
        &self,
        task_generation: u64,
        task_artifact_generation: u64,
        source_path: &Path,
        source_revision: &SourceRevision,
    ) -> Option<PageMapStaleReason> {
        if self.generation.load(Ordering::Relaxed) != task_generation {
            return Some(PageMapStaleReason::GlobalGenerationChanged);
        }
        if self.artifact_generation.load(Ordering::Relaxed) != task_artifact_generation {
            return Some(PageMapStaleReason::ArtifactGenerationChanged);
        }
        if !source_path.exists() {
            return Some(PageMapStaleReason::SourceDeleted);
        }
        let Ok(meta) = std::fs::metadata(source_path) else {
            return Some(PageMapStaleReason::SourceChanged);
        };
        let Some((expected_size, expected_modified_nanos)) = source_revision.persistable_key()
        else {
            return Some(PageMapStaleReason::SourceChanged);
        };
        if meta.len() != expected_size {
            return Some(PageMapStaleReason::SourceChanged);
        }
        let current_modified_nanos = meta
            .modified()
            .ok()
            .map(|time| system_time_to_i64_nanos(Some(time)))
            .unwrap_or(0);
        if current_modified_nanos != expected_modified_nanos {
            return Some(PageMapStaleReason::SourceChanged);
        }
        None
    }

    fn page_map_stale_reason_from_slow_request(
        &self,
        request: &PageMapCompleteRequest,
    ) -> Option<PageMapStaleReason> {
        self.page_map_stale_reason(
            request.task_generation,
            request.task_artifact_generation,
            &request.source_path,
            &request.source_revision,
        )
    }

    fn log_page_map_stale(
        &self,
        request_id: &BookId,
        source_path: &Path,
        reason: PageMapStaleReason,
    ) {
        tracing::debug!(
            id = %request_id.0.to_hex(),
            path = %source_path.display(),
            reason = ?reason,
            "page-map stale"
        );
    }

    fn finish_page_map_stale(
        &self,
        request: &PageMapCompleteRequest,
        key: &PageMapTaskKey,
        reason: PageMapStaleReason,
    ) {
        self.log_page_map_stale(&request.book_id, &request.source_path, reason);
        let _ = self.finish_page_map_slow_state(key, PageMapSlowState::Stale);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PageMapTaskKey {
    pub book_id: BookId,
    pub source_revision: SourceRevision,
    pub task_generation: u64,
    pub task_artifact_generation: u64,
}

impl PageMapTaskKey {
    pub fn from_task(
        book_id: BookId,
        source_revision: SourceRevision,
        task_generation: u64,
        task_artifact_generation: u64,
    ) -> Self {
        Self {
            book_id,
            source_revision,
            task_generation,
            task_artifact_generation,
        }
    }

    pub fn from_request(request: &PageMapCompleteRequest) -> Self {
        Self::from_task(
            request.book_id.clone(),
            request.source_revision.clone(),
            request.task_generation,
            request.task_artifact_generation,
        )
    }
}

/// slow / complete の進行状態。成功/失敗/stale を記録して重複起動を避ける。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageMapSlowState {
    NotStarted,
    QueuedOrRunning,
    Succeeded,
    Failed,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageMapStaleReason {
    GlobalGenerationChanged,
    ArtifactGenerationChanged,
    SourceChanged,
    SourceDeleted,
}

#[derive(Debug)]
pub struct PageMapFastPersistRequest {
    pub book_id: BookId,
    pub source_path: Arc<Path>,
    pub source_revision: SourceRevision,
    pub task_generation: u64,
    pub task_artifact_generation: u64,
    pub page_count: u32,
    pub fast_lane_status: ZipPageMapFastStatus,
    pub fast_lane_pages: Vec<PageDescriptor>,
    pub page_map_cache: Arc<PageMapDiskCache>,
}

enum PageMapFastPersistOutcome {
    Saved,
    RequiresComplete(PageMapCompleteRequest),
    Failed,
}

#[derive(Debug)]
pub struct PageMapReadyPersistRequest {
    pub book_id: BookId,
    pub source_path: Arc<Path>,
    pub source_revision: SourceRevision,
    pub task_generation: u64,
    pub task_artifact_generation: u64,
    pub page_map: BookPageMap,
    pub page_map_cache: Arc<PageMapDiskCache>,
}

#[derive(Clone, Debug)]
pub struct PageMapCompleteRequest {
    pub book_id: BookId,
    pub source_path: Arc<Path>,
    pub source_revision: SourceRevision,
    pub task_generation: u64,
    pub task_artifact_generation: u64,
    pub page_count: Option<u32>,
    pub reason: Option<ZipPageMapSlowReason>,
    pub page_map_cache: Arc<PageMapDiskCache>,
}

fn system_time_to_i64_nanos(time: Option<SystemTime>) -> i64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
