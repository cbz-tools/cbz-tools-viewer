//! 遅延読み込みするディレクトリ専用のナビゲーションツリー。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::Duration,
};

use eframe::egui;

use crate::{
    ui::{library::compare_name_natural, theme},
    util::path_eq::normalize_path_for_selection,
};

const MAX_RENDER_DEPTH: usize = 64;
const PENDING_REPAINT_INTERVAL: Duration = Duration::from_millis(150);
const TREE_INDENT_STEP: f32 = 14.0;
const TREE_CONTENT_PADDING: f32 = 4.0;
const TREE_ACTIVE_BAR_WIDTH: f32 = 4.0;
const TREE_LABEL_GAP: f32 = 4.0;

#[derive(Default, Clone)]
struct FolderNode {
    children: Vec<PathBuf>,
    loaded: bool,
    loading: bool,
    error: Option<String>,
}

struct PendingScan {
    generation: u64,
    cancelled: Arc<AtomicBool>,
}

struct ScanResult {
    generation: u64,
    path: PathBuf,
    result: Result<Vec<PathBuf>, String>,
}

/// Runtime-only tree cache. `LibraryState::current_dir` remains the sole current directory.
pub struct FolderTreeState {
    initialized: bool,
    generation: u64,
    /// Drive roots obtained only at Tree initialization or explicit Reload.
    roots: Vec<PathBuf>,
    nodes: HashMap<PathBuf, FolderNode>,
    expanded: HashSet<PathBuf>,
    user_expanded: HashSet<PathBuf>,
    user_collapsed: HashSet<PathBuf>,
    /// Cache invalidation metadata only; `LibraryState::current_dir` remains authoritative.
    last_observed_current_key: Option<String>,
    /// One-shot request kept until the active row has actually been rendered.
    scroll_to_active: bool,
    pending: HashMap<PathBuf, PendingScan>,
    /// A clicked row waits for its background directory scan to succeed before
    /// the app is allowed to change LibraryState.current_dir.
    pending_navigation: Option<PathBuf>,
    navigation_ready: Option<PathBuf>,
    result_tx: Sender<ScanResult>,
    result_rx: Receiver<ScanResult>,
}

impl Default for FolderTreeState {
    fn default() -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        Self {
            initialized: false,
            generation: 0,
            roots: Vec::new(),
            nodes: HashMap::new(),
            expanded: HashSet::new(),
            user_expanded: HashSet::new(),
            user_collapsed: HashSet::new(),
            last_observed_current_key: None,
            scroll_to_active: false,
            pending: HashMap::new(),
            pending_navigation: None,
            navigation_ready: None,
            result_tx,
            result_rx,
        }
    }
}

impl Drop for FolderTreeState {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}

impl FolderTreeState {
    fn initialize_if_needed(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        self.roots = drive_roots();
        for root in &self.roots {
            self.nodes.entry(root.clone()).or_default();
        }
    }

    fn reload(&mut self, current_dir: Option<&Path>) {
        self.cancel_pending();
        self.generation = self.generation.wrapping_add(1);
        self.roots.clear();
        self.nodes.clear();
        self.expanded.clear();
        self.user_expanded.clear();
        self.user_collapsed.clear();
        self.last_observed_current_key = None;
        self.scroll_to_active = false;
        self.pending_navigation = None;
        self.navigation_ready = None;
        self.initialized = false;
        self.initialize_if_needed();
        self.sync_current_dir(current_dir);
    }

    fn cancel_pending(&mut self) {
        for scan in self.pending.values() {
            scan.cancelled.store(true, Ordering::Relaxed);
        }
        self.pending.clear();
        self.pending_navigation = None;
        for node in self.nodes.values_mut() {
            node.loading = false;
        }
    }

    fn drain_results(&mut self) {
        loop {
            match self.result_rx.try_recv() {
                Ok(result) => {
                    self.apply_result(result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    for (path, _) in self.pending.drain() {
                        if let Some(node) = self.nodes.get_mut(&path) {
                            node.loading = false;
                            node.error = Some("Directory scan worker disconnected".to_owned());
                        }
                    }
                    break;
                }
            }
        }
    }

    /// Returns false for stale, cancelled, or superseded scan results.
    fn apply_result(&mut self, result: ScanResult) -> bool {
        let accepted = result.generation == self.generation
            && self.pending.get(&result.path).is_some_and(|scan| {
                scan.generation == result.generation && !scan.cancelled.load(Ordering::Relaxed)
            });
        if !accepted {
            return false;
        }

        self.pending.remove(&result.path);
        let navigation_requested = self
            .pending_navigation
            .as_deref()
            .is_some_and(|path| paths_match(path, &result.path));
        if navigation_requested {
            self.pending_navigation = None;
        }
        let parent_for_retry = result.path.parent().map(PathBuf::from);
        let succeeded = result.result.is_ok();
        match result.result {
            Ok(children) => {
                for child in &children {
                    self.nodes.entry(child.clone()).or_default();
                }
                let node = self.nodes.entry(result.path.clone()).or_default();
                node.loading = false;
                node.children = children;
                node.loaded = true;
                node.error = None;
            }
            Err(error) => {
                let node = self.nodes.entry(result.path.clone()).or_default();
                let had_cached_children = node.loaded;
                node.loading = false;
                if !had_cached_children {
                    node.children.clear();
                    node.loaded = false;
                }
                node.error = Some(error);
            }
        }
        if navigation_requested {
            if succeeded {
                self.navigation_ready = Some(result.path);
            } else if let Some(parent) = parent_for_retry {
                self.refresh_node(parent);
            }
        }
        true
    }

    fn sync_current_dir(&mut self, current_dir: Option<&Path>) {
        let Some(current_dir) = current_dir else {
            // Leaving the Library path also supersedes any pending Tree click.
            self.pending_navigation = None;
            self.navigation_ready = None;
            self.last_observed_current_key = None;
            self.scroll_to_active = false;
            self.user_collapsed.clear();
            return;
        };
        let current_key = normalize_path_for_selection(current_dir);
        if self.last_observed_current_key.as_deref() != Some(current_key.as_str()) {
            // A non-Tree navigation supersedes any background validation started
            // by an older Tree click. Its scan result may still update that node's
            // cache, but it must not later change LibraryState.current_dir.
            self.pending_navigation = None;
            self.navigation_ready = None;
            self.user_collapsed.clear();
            self.last_observed_current_key = Some(current_key);
            self.scroll_to_active = true;
        }
        let chain = ancestor_chain(current_dir);
        let Some(root) = self
            .roots
            .iter()
            .find(|root| path_is_within_root(current_dir, root))
        else {
            return;
        };
        let Some(root_index) = chain.iter().position(|path| paths_match(path, root)) else {
            return;
        };
        let chain = &chain[root_index..];
        for path in chain {
            self.nodes.entry(path.clone()).or_default();
        }
        let Some((current, ancestors)) = chain.split_last() else {
            return;
        };
        for ancestor in ancestors {
            if self.user_collapsed.contains(ancestor) {
                break;
            }
            self.expanded.insert(ancestor.clone());
            self.ensure_scan(ancestor.clone());
        }
        let active_path_revealed = ancestors
            .iter()
            .all(|ancestor| !self.user_collapsed.contains(ancestor));
        if active_path_revealed && self.user_expanded.contains(current) {
            self.expanded.insert(current.clone());
            self.ensure_scan(current.clone());
        } else {
            self.expanded.remove(current);
        }
    }

    fn ensure_scan(&mut self, path: PathBuf) {
        self.start_scan(path, false);
    }

    fn refresh_node(&mut self, path: PathBuf) {
        self.nodes.entry(path.clone()).or_default();
        self.start_scan(path, true);
    }

    fn start_scan(&mut self, path: PathBuf, force: bool) {
        let should_scan = self
            .nodes
            .get(&path)
            .is_some_and(|node| force || (!node.loaded && !node.loading && node.error.is_none()));
        if !should_scan || self.pending.contains_key(&path) {
            return;
        }

        let generation = self.generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.nodes.entry(path.clone()).or_default().loading = true;
        self.nodes.entry(path.clone()).or_default().error = None;
        self.pending.insert(
            path.clone(),
            PendingScan {
                generation,
                cancelled: Arc::clone(&cancelled),
            },
        );
        let result_tx = self.result_tx.clone();
        std::thread::spawn(move || {
            let result = if cancelled.load(Ordering::Relaxed) {
                return;
            } else {
                read_immediate_child_dirs(&path)
            };
            if !cancelled.load(Ordering::Relaxed) {
                let _ = result_tx.send(ScanResult {
                    generation,
                    path,
                    result,
                });
            }
        });
    }

    /// Refreshes only the active Library directory's direct children. Existing
    /// cache, expansion state, and scroll state are deliberately preserved.
    pub fn refresh_active_node(&mut self, current_dir: Option<&Path>) {
        if let Some(path) = current_dir {
            self.refresh_node(path.to_path_buf());
        }
    }

    fn request_navigation(&mut self, path: PathBuf) {
        self.navigation_ready = None;
        self.pending_navigation = Some(path.clone());
        self.refresh_node(path);
    }
}

/// Draws the Tree tab and returns a directory navigation request when a label is clicked.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut FolderTreeState,
    current_dir: Option<&Path>,
) -> Option<PathBuf> {
    state.initialize_if_needed();
    state.drain_results();
    state.sync_current_dir(current_dir);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Directories").color(theme::TEXT_SUBTLE));
        if ui
            .small_button("Reload")
            .on_hover_text("Reload Tree")
            .clicked()
        {
            state.reload(current_dir);
        }
    });
    ui.separator();

    let active_normalized = current_dir.map(normalize_path_for_selection);
    let mut roots = state.roots.clone();
    roots.sort_by(|left, right| compare_paths(left, right));

    // Allocate the remaining body explicitly so the non-floating scrollbar reserves
    // space inside this clipped rectangle rather than overpainting tree rows.
    let body_rect = ui.available_rect_before_wrap();
    let parent_clip = ui.clip_rect();
    let (_, body_rect) = ui.allocate_space(body_rect.size());
    let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body_rect));
    body_ui.set_clip_rect(parent_clip.intersect(body_rect));
    body_ui.style_mut().spacing.scroll.floating = false;
    egui::ScrollArea::vertical()
        .id_salt("folder_tree_scroll")
        .auto_shrink([false, false])
        .show(&mut body_ui, |tree_ui| {
            for root in roots {
                show_node(tree_ui, state, &root, &active_normalized, 0);
            }
        });

    if !state.pending.is_empty() {
        ui.ctx().request_repaint_after(PENDING_REPAINT_INTERVAL);
    }
    state.navigation_ready.take()
}

fn show_node(
    ui: &mut egui::Ui,
    state: &mut FolderTreeState,
    path: &Path,
    active_normalized: &Option<String>,
    depth: usize,
) {
    if depth > MAX_RENDER_DEPTH {
        return;
    }
    let Some(node) = state.nodes.get(path).cloned() else {
        return;
    };
    let expanded = state.expanded.contains(path);
    let is_active = active_normalized
        .as_ref()
        .is_some_and(|active| *active == normalize_path_for_selection(path));
    let label = path_label(path);

    let row_width = ui.available_width().max(0.0);
    let (_, row_rect) = ui.allocate_space(egui::vec2(row_width, theme::CONTROL_HEIGHT));
    let indent = depth as f32 * TREE_INDENT_STEP;
    let arrow_rect = egui::Rect::from_min_size(
        row_rect.min + egui::vec2(indent, 0.0),
        egui::vec2(22.0, theme::CONTROL_HEIGHT),
    );
    let arrow_response = ui.interact(
        arrow_rect,
        ui.id().with(("folder_tree_arrow", path)),
        egui::Sense::click(),
    );
    let arrow = if expanded { "▼" } else { "▶" };
    let content_start_x = arrow_rect.max.x + TREE_CONTENT_PADDING;
    let status_width = if node.loading || node.error.is_some() {
        20.0
    } else {
        0.0
    };
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(content_start_x, row_rect.min.y),
        egui::pos2(
            (row_rect.max.x - status_width).max(content_start_x),
            row_rect.max.y,
        ),
    );
    let label_response = ui
        .interact(
            label_rect,
            ui.id().with(("folder_tree_label", path)),
            egui::Sense::click(),
        )
        .on_hover_text(path.to_string_lossy());
    if is_active && state.scroll_to_active {
        label_response.scroll_to_me(Some(egui::Align::Center));
        state.scroll_to_active = false;
    }
    paint_tree_row_state(ui, label_rect, is_active, label_response.hovered());
    ui.painter().text(
        arrow_rect.center(),
        egui::Align2::CENTER_CENTER,
        arrow,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );
    if arrow_response.clicked() {
        if expanded {
            state.expanded.remove(path);
            state.user_expanded.remove(path);
            state.user_collapsed.insert(path.to_path_buf());
        } else {
            state.expanded.insert(path.to_path_buf());
            state.user_expanded.insert(path.to_path_buf());
            state.user_collapsed.remove(path);
            state.ensure_scan(path.to_path_buf());
        }
    } else if label_response.clicked() {
        state.request_navigation(path.to_path_buf());
    }

    let text_rect = egui::Rect::from_min_max(
        egui::pos2(
            label_rect.min.x + TREE_ACTIVE_BAR_WIDTH + TREE_LABEL_GAP,
            row_rect.min.y,
        ),
        egui::pos2(
            label_rect
                .max
                .x
                .max(label_rect.min.x + TREE_ACTIVE_BAR_WIDTH + TREE_LABEL_GAP),
            row_rect.max.y,
        ),
    );
    ui.painter().with_clip_rect(text_rect).text(
        text_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(theme::FONT_SIZE_BODY),
        theme::TEXT_MAIN,
    );
    if node.loading {
        let spinner_rect = egui::Rect::from_min_size(
            egui::pos2(row_rect.max.x - 16.0, row_rect.min.y + 6.0),
            egui::vec2(12.0, 12.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(spinner_rect),
            |spinner_ui| {
                spinner_ui.add(egui::Spinner::new().size(12.0));
            },
        );
    } else if let Some(error) = node.error.as_deref() {
        ui.painter().text(
            egui::pos2(row_rect.max.x - 10.0, row_rect.center().y),
            egui::Align2::CENTER_CENTER,
            "!",
            egui::FontId::proportional(theme::FONT_SIZE_BODY),
            theme::DELETE_RED,
        );
        label_response.clone().on_hover_text(error);
    }

    if expanded && node.loaded {
        for child in node.children {
            show_node(ui, state, &child, active_normalized, depth + 1);
        }
    }
}

fn paint_tree_row_state(ui: &egui::Ui, content_rect: egui::Rect, active: bool, hovered: bool) {
    if active {
        ui.painter().rect_filled(
            content_rect,
            egui::CornerRadius::same(4),
            theme::SIDEBAR_SELECTED_BG,
        );
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                content_rect.min,
                egui::pos2(
                    content_rect.min.x + TREE_ACTIVE_BAR_WIDTH,
                    content_rect.max.y,
                ),
            ),
            0,
            theme::ACCENT_ACTIVE,
        );
        ui.painter().rect_stroke(
            content_rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        ui.painter().rect_filled(
            content_rect,
            egui::CornerRadius::same(4),
            theme::SIDEBAR_HOVER_BG,
        );
        ui.painter().rect_stroke(
            content_rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(3.0, theme::HOVER_BORDER),
            egui::StrokeKind::Inside,
        );
    }
}

fn read_immediate_child_dirs(path: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    let mut children = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            (metadata.is_dir() && !metadata_has_hidden_attribute(&metadata)).then(|| entry.path())
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| compare_paths(left, right));
    Ok(children)
}

fn compare_paths(left: &Path, right: &Path) -> std::cmp::Ordering {
    let left_name = left
        .file_name()
        .unwrap_or(left.as_os_str())
        .to_string_lossy();
    let right_name = right
        .file_name()
        .unwrap_or(right.as_os_str())
        .to_string_lossy();
    compare_name_natural(&left_name, &right_name)
}

#[cfg(windows)]
fn metadata_has_hidden_attribute(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    file_attributes_are_hidden(metadata.file_attributes())
}

#[cfg(windows)]
fn file_attributes_are_hidden(attributes: u32) -> bool {
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN;

    attributes & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn metadata_has_hidden_attribute(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn paths_match(left: &Path, right: &Path) -> bool {
    normalize_path_for_selection(left) == normalize_path_for_selection(right)
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    let path_key = normalize_path_for_selection(path);
    let root_key = normalize_path_for_selection(root);
    path_key == root_key
        || path_key.strip_prefix(&root_key).is_some_and(|remaining| {
            root_key.ends_with('\\')
                || root_key.ends_with('/')
                || remaining.starts_with('\\')
                || remaining.starts_with('/')
        })
}

fn ancestor_chain(path: &Path) -> Vec<PathBuf> {
    let mut chain = path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    chain.reverse();
    chain.dedup();
    chain
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn drive_roots() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

    // SAFETY: `GetLogicalDrives` has no arguments and only returns a drive-bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", (b'A' + bit as u8) as char)))
        .collect()
}

#[cfg(not(windows))]
fn drive_roots() -> Vec<PathBuf> {
    vec![PathBuf::from(std::path::MAIN_SEPARATOR_STR)]
}
