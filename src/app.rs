use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontFamily, FontId, Frame, Id, Layout, Margin, Rect,
    Response, Rounding, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2,
};

use crate::api::{self, FeedPage, SearchParams};
use crate::downloads::{DownloadManager, DownloadNotice, DownloadState};
use crate::images::{ImageLoader, ImageSpec, Textures};
use crate::models::{
    sort_albums, Album, AlbumDetail, FormatFilter, LibraryEntry, Persisted, RuleField, SaveRule,
    SearchKind, SearchSort, WatchMeta,
};
use crate::store;
use crate::theme::{
    self, accent, accent_dim, Icon, ACCENTS, BG, BORDER, BORDER_SOFT, CARD, CARD_HOVER, DANGER,
    DIM, MUTED, PANEL, SIDEBAR, SUCCESS, TEXT, WARNING,
};
use crate::tray::{Tray, TrayAction};

const CARD_GAP: f32 = 16.0;
const MIN_CARD_WIDTH: f32 = 172.0;
const DETAIL_WIDTH: f32 = 414.0;

fn search_field_id() -> Id {
    Id::new("search-field")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Discover,
    Search,
    Watchlist,
    Library,
    Downloads,
    Settings,
}

struct SearchState {
    query: String,
    kind: SearchKind,
    formats: FormatFilter,
    results: Vec<Album>,
    /// Site-order results sorted for drawing. Pagination always reads `results`.
    display: Vec<Album>,
    total: Option<usize>,
    next_page: usize,
    requested_page: Option<usize>,
    loading: bool,
    done: bool,
    error: Option<String>,
    gen: u64,
    searched: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            kind: SearchKind::Blanket,
            formats: FormatFilter::default(),
            results: Vec::new(),
            display: Vec::new(),
            total: None,
            next_page: 0,
            requested_page: None,
            loading: false,
            done: false,
            error: None,
            gen: 0,
            searched: false,
        }
    }
}

struct DetailState {
    album: Album,
    loaded: Option<AlbumDetail>,
    loading: bool,
    error: Option<String>,
}

#[derive(Clone)]
struct RuleDraft {
    query: String,
    field: RuleField,
    folder: String,
}

impl Default for RuleDraft {
    fn default() -> Self {
        Self {
            query: String::new(),
            field: RuleField::Any,
            folder: String::new(),
        }
    }
}

enum NetEvent {
    Latest {
        gen: u64,
        page: usize,
        result: Result<FeedPage, String>,
    },
    Search {
        gen: u64,
        page: usize,
        result: Result<FeedPage, String>,
    },
    Detail {
        id: u64,
        result: Result<AlbumDetail, String>,
    },
}

#[derive(Clone)]
enum CardAction {
    Open(Album),
    ToggleSelect {
        id: u64,
        shift: bool,
    },
    Download(Album),
    Watch(Album),
    SetFolder {
        album: Album,
        folder: Option<String>,
    },
    OpenUrl(String),
    LoadMore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WatchFilter {
    All,
    Uncategorized,
    Folder(String),
}

impl Default for WatchFilter {
    fn default() -> Self {
        Self::All
    }
}

enum DetailAction {
    Close,
    Download(Album),
    ToggleWatch(Album),
    OpenUrl(String),
    Search { text: String, kind: SearchKind },
    Retry,
    Preview,
    WatchMetaChanged,
}

enum TaskAction {
    Pause(u64),
    Resume(u64),
    Cancel(u64),
    Retry(u64),
    Remove(u64),
    OpenFolder(u64),
    OpenUrl(String),
    OpenAlbum(Album),
    PlayInFoobar(PathBuf),
}

enum LibraryAction {
    Open(Album),
    OpenFolder(PathBuf),
    Remove(u64),
    PlayInFoobar(PathBuf),
}

struct Toast {
    text: String,
    created: Instant,
    tone: ToastTone,
}

#[derive(Clone, Copy)]
enum ToastTone {
    Info,
    Success,
    Error,
}

struct DownloadUiState {
    speeds: HashMap<u64, (u64, Instant, f32)>,
    cache_bytes: u64,
    cache_checked: Instant,
    manual_opened: HashSet<u64>,
}

/// Command palette action
#[derive(Clone, Debug, PartialEq, Eq)]
enum PaletteAction {
    Search,
    Library,
    Watchlist,
    Downloads,
    Settings,
    Refresh,
    PauseAll,
    ResumeAll,
    Quit,
}

pub struct App {
    persisted: Persisted,
    view: View,

    latest: Vec<Album>,
    latest_gen: u64,
    latest_next_page: usize,
    latest_requested_page: Option<usize>,
    latest_loading: bool,
    latest_done: bool,
    latest_error: Option<String>,
    latest_new_count: usize,
    new_baseline: u64,
    first_run: bool,

    search: SearchState,
    detail: Option<DetailState>,
    selection: HashSet<u64>,

    images: ImageLoader,
    textures: Textures,
    downloads: DownloadManager,
    dl_ui: DownloadUiState,
    tray: Option<Tray>,
    tray_attempted: bool,
    tray_hint_shown: bool,
    allow_exit: bool,
    rule_draft: Option<RuleDraft>,

    net_tx: Sender<NetEvent>,
    net_rx: Receiver<NetEvent>,

    toasts: Vec<Toast>,
    preview_cover: bool,
    focus_search: bool,
    search_save_draft: Option<String>,
    library_query: String,
    library_format_filter: Option<String>,
    library_year_filter: Option<u16>,
    watch_filter: WatchFilter,
    folder_draft: Option<String>,
    folder_rename: Option<(String, String)>,
    selection_anchor: Option<u64>,
    dirty: bool,
    last_state_save: Instant,
    had_active_downloads: bool,

    // Command palette
    palette_open: bool,
    palette_query: String,
    palette_selected: usize,
    palette_actions: Vec<(String, String, PaletteAction)>,

    // Keyboard shortcuts overlay
    show_shortcuts: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        store::ensure_dirs();
        let persisted = store::load_state();
        theme::configure(&cc.egui_ctx, persisted.settings.accent);
        let settings = persisted.settings.clone();
        let (net_tx, net_rx) = unbounded();
        let first_run = persisted.last_seen_id == 0;
        let downloads = DownloadManager::new(
            settings.max_concurrent,
            settings.download_dir.clone(),
            settings.auto_extract,
            settings.auto_retry,
            settings.max_retries,
        );
        let mut app = Self {
            persisted,
            view: View::Discover,
            latest: Vec::new(),
            latest_gen: 0,
            latest_next_page: 0,
            latest_requested_page: None,
            latest_loading: false,
            latest_done: false,
            latest_error: None,
            latest_new_count: 0,
            new_baseline: 0,
            first_run,
            search: SearchState::default(),
            detail: None,
            selection: HashSet::new(),
            images: ImageLoader::new(6),
            textures: Textures::default(),
            downloads,
            dl_ui: DownloadUiState {
                speeds: HashMap::new(),
                cache_bytes: store::cache_size(),
                cache_checked: Instant::now(),
                manual_opened: HashSet::new(),
            },
            tray: None,
            tray_attempted: false,
            tray_hint_shown: false,
            allow_exit: false,
            rule_draft: None,
            net_tx,
            net_rx,
            toasts: Vec::new(),
            preview_cover: false,
            focus_search: false,
            search_save_draft: None,
            library_query: String::new(),
            library_format_filter: None,
            library_year_filter: None,
            watch_filter: WatchFilter::All,
            folder_draft: None,
            folder_rename: None,
            selection_anchor: None,
            dirty: false,
            last_state_save: Instant::now(),
            had_active_downloads: false,

            // Command palette
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
            palette_actions: Vec::new(),

            // Keyboard shortcuts overlay
            show_shortcuts: false,
        };
        app.refresh_latest();
        app.sync_library_from_downloads();
        if app.downloads.restored_paused > 0 {
            let count = app.downloads.restored_paused;
            app.toast(
                format!(
                    "Restored {count} paused download{}. Resume them in Downloads.",
                    if count == 1 { "" } else { "s" }
                ),
                ToastTone::Info,
            );
        }
        app
    }

    fn ensure_tray(&mut self, ctx: &egui::Context) {
        if !self.persisted.settings.show_tray_icon {
            if self.tray.take().is_some() {
                self.tray_attempted = false;
            }
            return;
        }
        if self.tray.is_none() && !self.tray_attempted {
            self.tray_attempted = true;
            self.tray = Tray::new(ctx);
            if self.tray.is_none() {
                self.toast(
                    "The system tray icon could not be created.",
                    ToastTone::Error,
                );
            }
        }
    }

    fn handle_tray_actions(&mut self, ctx: &egui::Context) {
        let actions = self
            .tray
            .as_ref()
            .map(|tray| tray.poll())
            .unwrap_or_default();
        let mut show = false;
        for action in actions {
            match action {
                TrayAction::Show => show = true,
                TrayAction::PauseAll => self.downloads.pause_all(),
                TrayAction::ResumeAll => self.downloads.resume_all(),
                TrayAction::Quit => {
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if show {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.allow_exit || !self.persisted.settings.close_to_tray || self.tray.is_none() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        if !self.tray_hint_shown {
            self.tray_hint_shown = true;
            self.desktop_notify(
                "DST-Explorer is still running",
                "Use the tray icon to reopen the window or quit.",
            );
        }
    }

    fn desktop_notify(&self, summary: &str, body: &str) {
        if !self.persisted.settings.notify_on_complete {
            return;
        }
        let _ = notify_rust::Notification::new()
            .appname("DST-Explorer")
            .summary(summary)
            .body(body)
            .timeout(notify_rust::Timeout::Milliseconds(5000))
            .show();
    }

    fn refresh_latest(&mut self) {
        self.latest_gen = self.latest_gen.wrapping_add(1);
        self.latest.clear();
        self.latest_next_page = 0;
        self.latest_requested_page = None;
        self.latest_loading = false;
        self.latest_done = false;
        self.latest_error = None;
        self.latest_new_count = 0;
        self.selection.clear();
        self.selection_anchor = None;
        self.request_latest_page(0);
    }

    fn request_latest_page(&mut self, page: usize) {
        if self.latest_loading || self.latest_done {
            return;
        }
        self.latest_loading = true;
        self.latest_error = None;
        self.latest_requested_page = Some(page);
        let gen = self.latest_gen;
        let tx = self.net_tx.clone();
        let _ = thread::Builder::new()
            .name(format!("latest-{page}"))
            .spawn(move || {
                let client = api::page_client();
                let result = api::fetch_latest(&client, page);
                let _ = tx.send(NetEvent::Latest { gen, page, result });
            });
    }

    fn load_search_page(&mut self) {
        if self.search.loading || self.search.done {
            return;
        }
        let query = self.search.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let page = self.search.next_page;
        self.search.loading = true;
        self.search.requested_page = Some(page);
        self.search.error = None;
        let gen = self.search.gen;
        let params = SearchParams {
            text: query,
            kind: self.search.kind,
            formats: self.search.formats,
            page,
        };
        let tx = self.net_tx.clone();
        let _ = thread::Builder::new()
            .name(format!("search-{page}"))
            .spawn(move || {
                let client = api::page_client();
                let result = api::fetch_search(&client, &params);
                let _ = tx.send(NetEvent::Search { gen, page, result });
            });
    }

    fn submit_search(&mut self) {
        self.search.gen = self.search.gen.wrapping_add(1);
        self.search.results.clear();
        self.search.display.clear();
        self.search.total = None;
        self.search.next_page = 0;
        self.search.requested_page = None;
        self.search.loading = false;
        self.search.done = false;
        self.search.error = None;
        self.selection.clear();
        let query = self.search.query.trim().to_string();
        if query.is_empty() {
            self.search.searched = false;
            return;
        }
        self.search.searched = true;
        if self.persisted.record_search(
            &query,
            self.search.kind,
            self.search.formats,
            store::now_unix(),
        ) {
            self.dirty = true;
        }
        self.load_search_page();
    }

    fn rebuild_search_display(&mut self) {
        self.search.display = sort_albums(
            self.search.results.clone(),
            self.persisted.settings.search_sort,
        );
    }

    fn run_search(&mut self, text: String, kind: SearchKind, formats: FormatFilter) {
        self.view = View::Search;
        self.search.kind = kind;
        self.search.formats = formats;
        self.search.query = text;
        self.search_save_draft = None;
        self.submit_search();
    }

    fn search_for(&mut self, text: String, kind: SearchKind) {
        let formats = self.search.formats;
        self.run_search(text, kind, formats);
    }

    fn set_search_sort(&mut self, sort: SearchSort) {
        if self.persisted.settings.search_sort != sort {
            self.persisted.settings.search_sort = sort;
            self.rebuild_search_display();
            self.dirty = true;
        }
    }

    fn open_detail(&mut self, album: Album) {
        if let Some(detail) = &self.detail {
            if detail.album.id == album.id {
                return;
            }
        }
        let id = album.id;
        let fallback = album.clone();
        self.detail = Some(DetailState {
            album,
            loaded: None,
            loading: true,
            error: None,
        });
        self.preview_cover = false;
        let tx = self.net_tx.clone();
        let album_for_thread = fallback;
        let _ = thread::Builder::new()
            .name(format!("detail-{id}"))
            .spawn(move || {
                let client = api::page_client();
                let result = api::fetch_album(&client, &album_for_thread);
                let _ = tx.send(NetEvent::Detail { id, result });
            });
    }

    fn retry_detail(&mut self) {
        if let Some(detail) = self.detail.as_mut() {
            detail.loading = true;
            detail.error = None;
            let album = detail.album.clone();
            let id = album.id;
            let tx = self.net_tx.clone();
            let _ = thread::Builder::new()
                .name(format!("detail-{id}"))
                .spawn(move || {
                    let client = api::page_client();
                    let result = api::fetch_album(&client, &album);
                    let _ = tx.send(NetEvent::Detail { id, result });
                });
        }
    }

    fn poll_net(&mut self) {
        loop {
            let event = match self.net_rx.try_recv() {
                Ok(event) => event,
                Err(_) => break,
            };
            match event {
                NetEvent::Latest { gen, page, result } => {
                    if gen != self.latest_gen || self.latest_requested_page != Some(page) {
                        continue;
                    }
                    self.latest_loading = false;
                    self.latest_requested_page = None;
                    match result {
                        Ok(feed) => {
                            if page == 0 && self.first_run {
                                self.new_baseline = feed
                                    .albums
                                    .iter()
                                    .map(|a| a.id)
                                    .max()
                                    .unwrap_or(self.persisted.last_seen_id);
                                self.first_run = false;
                            }
                            if self.new_baseline == 0 {
                                self.new_baseline = self.persisted.last_seen_id;
                            }
                            let feed_was_empty = feed.albums.is_empty();
                            let mut seen: HashSet<u64> = self.latest.iter().map(|a| a.id).collect();
                            let mut fetched = Vec::new();
                            for mut album in feed.albums {
                                if seen.insert(album.id) {
                                    album.is_new = self.new_baseline > 0
                                        && album.id > self.new_baseline
                                        && self.persisted.settings.show_new_badges;
                                    self.latest.push(album.clone());
                                    fetched.push(album);
                                }
                            }
                            let auto_saved = self.persisted.apply_save_rules(&fetched);
                            if !auto_saved.is_empty() {
                                self.dirty = true;
                                let count = auto_saved.len();
                                self.toast(
                                    format!(
                                        "Auto-saved {count} release{} to your watchlist",
                                        if count == 1 { "" } else { "s" }
                                    ),
                                    ToastTone::Success,
                                );
                            }
                            self.latest_new_count = self.latest.iter().filter(|a| a.is_new).count();
                            self.latest_next_page = page + 1;
                            if feed_was_empty {
                                self.latest_done = true;
                            }
                            if page == 0 && self.latest.is_empty() {
                                self.latest_done = true;
                            }
                        }
                        Err(error) => {
                            if self.latest.is_empty() {
                                self.latest_error = Some(error);
                            } else {
                                self.latest_error = Some(error.clone());
                                self.toast(
                                    format!("Could not load more: {error}"),
                                    ToastTone::Error,
                                );
                            }
                        }
                    }
                }
                NetEvent::Search { gen, page, result } => {
                    if gen != self.search.gen || self.search.requested_page != Some(page) {
                        continue;
                    }
                    self.search.loading = false;
                    self.search.requested_page = None;
                    match result {
                        Ok(feed) => {
                            if let Some(total) = feed.total_results {
                                self.search.total = Some(total);
                            }
                            let page_was_empty = feed.albums.is_empty();
                            let mut seen: HashSet<u64> =
                                self.search.results.iter().map(|a| a.id).collect();
                            for album in feed.albums {
                                if seen.insert(album.id) {
                                    self.search.results.push(album);
                                }
                            }
                            self.search.next_page = page + 1;
                            if page == 0 {
                                self.search.searched = true;
                            }
                            let total_done = self
                                .search
                                .total
                                .map(|total| total == 0 || self.search.results.len() >= total)
                                .unwrap_or(false);
                            if page_was_empty
                                || (page == 0 && self.search.results.is_empty())
                                || total_done
                            {
                                self.search.done = true;
                            }
                            self.rebuild_search_display();
                        }
                        Err(error) => {
                            self.search.error = Some(error.clone());
                            if self.search.results.is_empty() {
                                self.search.done = false;
                            }
                        }
                    }
                }
                NetEvent::Detail { id, result } => {
                    if let Some(detail) = self.detail.as_mut() {
                        if detail.album.id != id {
                            continue;
                        }
                        detail.loading = false;
                        match result {
                            Ok(loaded) => {
                                detail.album = loaded.album.clone();
                                if detail.album.tags.is_empty() {
                                    detail.album.tags = loaded.tags.clone();
                                }
                                detail.loaded = Some(loaded);
                                detail.error = None;
                            }
                            Err(error) => {
                                detail.error = Some(error);
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let (search_shortcut, refresh, escape, download, palette, shortcuts_help, nav_keys) = ctx.input(|input| {
            (
                (input.modifiers.command && input.key_pressed(egui::Key::F))
                    || (!typing && input.key_pressed(egui::Key::Slash)),
                input.modifiers.command && input.key_pressed(egui::Key::R),
                input.key_pressed(egui::Key::Escape),
                input.modifiers.command && input.key_pressed(egui::Key::D),
                input.modifiers.command && input.key_pressed(egui::Key::P),
                (!typing && input.key_pressed(egui::Key::Slash))
                    || (!typing && input.modifiers.command && input.key_pressed(egui::Key::Slash)),
                [
                    input.key_pressed(egui::Key::Num1),
                    input.key_pressed(egui::Key::Num2),
                    input.key_pressed(egui::Key::Num3),
                    input.key_pressed(egui::Key::Num4),
                    input.key_pressed(egui::Key::Num5),
                    input.key_pressed(egui::Key::Num6),
                ],
            )
        });

        if search_shortcut {
            self.view = View::Search;
            self.focus_search = true;
        }
        if refresh {
            match self.view {
                View::Discover => self.refresh_latest(),
                View::Search => {
                    if !self.search.query.trim().is_empty() {
                        self.submit_search();
                    }
                }
                View::Watchlist => {
                    self.toast("Watchlist is already up to date.", ToastTone::Info);
                }
                View::Library => {
                    self.toast("Library reflects completed downloads.", ToastTone::Info);
                }
                View::Downloads | View::Settings => {}
            }
        }
        if escape {
            if self.preview_cover {
                self.preview_cover = false;
            } else if self.detail.is_some() {
                self.detail = None;
            } else if !self.selection.is_empty() {
                self.selection.clear();
                self.selection_anchor = None;
            } else if self.view == View::Search {
                ctx.memory_mut(|memory| memory.surrender_focus(search_field_id()));
            }
        }
        if download {
            if !self.selection.is_empty() {
                self.download_selected();
            } else if let Some(detail) = &self.detail {
                let album = detail.album.clone();
                self.queue_download(album);
            }
        }
        if palette {
            self.open_command_palette();
        }
        if shortcuts_help {
            self.show_shortcuts = !self.show_shortcuts;
        }
        for (index, pressed) in nav_keys.iter().enumerate() {
            if *pressed && !typing {
                match index {
                    0 => self.set_view(View::Discover),
                    1 => self.set_view(View::Search),
                    2 => self.set_view(View::Watchlist),
                    3 => self.set_view(View::Library),
                    4 => self.set_view(View::Downloads),
                    5 => self.set_view(View::Settings),
                    _ => {}
                }
            }
        }
        if self.focus_search {
            ctx.memory_mut(|memory| memory.request_focus(search_field_id()));
            self.focus_search = false;
        }
    }

    /// Open the command palette and build the action list.
    fn open_command_palette(&mut self) {
        self.palette_open = true;
        self.palette_query.clear();
        self.palette_selected = 0;
        self.palette_actions = vec![
            ("Search".to_string(), "Search the archive (Ctrl+F)".to_string(), PaletteAction::Search),
            ("Library".to_string(), "Browse downloaded albums".to_string(), PaletteAction::Library),
            ("Watchlist".to_string(), "Saved albums with folders and notes".to_string(), PaletteAction::Watchlist),
            ("Downloads".to_string(), "Active, paused, and finished downloads".to_string(), PaletteAction::Downloads),
            ("Settings".to_string(), "Configure the app".to_string(), PaletteAction::Settings),
            ("Refresh".to_string(), "Reload current view (Ctrl+R)".to_string(), PaletteAction::Refresh),
            ("Pause all downloads".to_string(), "Pause all active downloads".to_string(), PaletteAction::PauseAll),
            ("Resume all downloads".to_string(), "Resume all paused downloads".to_string(), PaletteAction::ResumeAll),
            ("Quit".to_string(), "Exit the application".to_string(), PaletteAction::Quit),
        ];
    }

    fn set_view(&mut self, view: View) {
        if self.view != view {
            self.selection.clear();
            self.selection_anchor = None;
            self.preview_cover = false;
            self.folder_draft = None;
            self.folder_rename = None;
            self.search_save_draft = None;
        }
        self.view = view;
        if view == View::Search && self.search.query.trim().is_empty() {
            self.focus_search = true;
        }
    }

    fn toast(&mut self, text: impl Into<String>, tone: ToastTone) {
        self.toasts.push(Toast {
            text: text.into(),
            created: Instant::now(),
            tone,
        });
        if self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
    }

    fn export_all(&mut self) {
        use serde_json::json;
        
        let export = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "settings": self.persisted.settings,
            "watchlist": self.persisted.watchlist,
            "watch_folders": self.persisted.watch_folders,
            "watch_meta": self.persisted.watch_meta,
            "library": self.persisted.library,
            "save_rules": self.persisted.save_rules,
        });
        
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Export DST-Explorer data")
            .add_filter("JSON", &["json"])
            .set_file_name(&format!("dst-explorer-export-{}.json", chrono::Local::now().format("%Y%m%d")))
            .save_file()
        {
            match std::fs::write(&path, serde_json::to_string_pretty(&export).unwrap()) {
                Ok(_) => self.toast(format!("Exported to {}", path.display()), ToastTone::Success),
                Err(e) => self.toast(format!("Export failed: {}", e), ToastTone::Error),
            }
        }
    }

    fn import_all(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Import DST-Explorer data")
            .add_filter("JSON", &["json"])
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    match serde_json::from_str::<serde_json::Value>(&content) {
                        Ok(import) => {
                            let mut imported_something = false;
                            
                            if let Some(settings) = import.get("settings") {
                                if let Ok(s) = serde_json::from_value::<crate::models::Settings>(settings.clone()) {
                                    self.persisted.settings = s;
                                    imported_something = true;
                                }
                            }
                            
                            if let Some(watchlist) = import.get("watchlist") {
                                if let Ok(w) = serde_json::from_value::<Vec<crate::models::Album>>(watchlist.clone()) {
                                    self.persisted.watchlist = w;
                                    imported_something = true;
                                }
                            }
                            
                            if let Some(watch_folders) = import.get("watch_folders") {
                                if let Ok(w) = serde_json::from_value::<Vec<String>>(watch_folders.clone()) {
                                    self.persisted.watch_folders = w;
                                    imported_something = true;
                                }
                            }
                            
                            if let Some(watch_meta) = import.get("watch_meta") {
                                if let Ok(w) = serde_json::from_value::<std::collections::HashMap<u64, crate::models::WatchMeta>>(watch_meta.clone()) {
                                    self.persisted.watch_meta = w;
                                    imported_something = true;
                                }
                            }
                            
                            if let Some(library) = import.get("library") {
                                if let Ok(l) = serde_json::from_value::<Vec<crate::models::LibraryEntry>>(library.clone()) {
                                    self.persisted.library = l;
                                    imported_something = true;
                                }
                            }
                            
                            if let Some(save_rules) = import.get("save_rules") {
                                if let Ok(r) = serde_json::from_value::<Vec<crate::models::SaveRule>>(save_rules.clone()) {
                                    self.persisted.save_rules = r;
                                    imported_something = true;
                                }
                            }
                            
                            if imported_something {
                                self.dirty = true;
                                self.toast(format!("Imported from {}", path.display()), ToastTone::Success);
                            } else {
                                self.toast("Import failed: no valid data found", ToastTone::Error);
                            }
                        }
                        Err(e) => self.toast(format!("Import failed: {}", e), ToastTone::Error),
                    }
                }
                Err(e) => self.toast(format!("Import failed: {}", e), ToastTone::Error),
            }
        }
    }

    fn queue_download(&mut self, album: Album) {
        let title = album.title.clone();
        if self.persisted.in_library(album.id) {
            self.toast(
                format!("“{title}” is already in your library. Downloading another copy."),
                ToastTone::Info,
            );
        } else {
            self.toast(format!("Queued “{title}”"), ToastTone::Info);
        }
        self.downloads.enqueue(album);
    }

    fn sync_library_from_downloads(&mut self) {
        let completed: Vec<(Album, PathBuf, Option<PathBuf>, Option<String>)> = self
            .downloads
            .tasks
            .iter()
            .filter(|task| task.state == DownloadState::Done)
            .filter_map(|task| {
                let path = task.output.clone()?;
                Some((
                    task.album.clone(),
                    path,
                    task.extracted.clone(),
                    task.warning.clone(),
                ))
            })
            .collect();
        let mut changed = false;
        for (album, path, extracted, warning) in completed {
            let known = self
                .persisted
                .library
                .iter()
                .any(|entry| entry.album.id == album.id && entry.path == path);
            if !known {
                changed |= self
                    .persisted
                    .record_download(album, path, extracted, warning);
            }
        }
        if changed {
            self.dirty = true;
        }
    }

    fn toggle_watch(&mut self, album: Album) {
        let title = album.title.clone();
        let added = self.persisted.toggle_watch(album);
        self.dirty = true;
        if added {
            self.toast(
                format!("Added “{title}” to your watchlist"),
                ToastTone::Success,
            );
        } else {
            self.toast(
                format!("Removed “{title}” from your watchlist"),
                ToastTone::Info,
            );
        }
    }

    fn toggle_select(&mut self, id: u64, shift: bool) {
        if shift {
            let albums = self.visible_albums();
            let target = albums.iter().position(|album| album.id == id);
            let anchor = self
                .selection_anchor
                .and_then(|anchor| albums.iter().position(|album| album.id == anchor));
            if let (Some(target), Some(anchor)) = (target, anchor) {
                let (start, end) = if anchor <= target {
                    (anchor, target)
                } else {
                    (target, anchor)
                };
                for album in &albums[start..=end] {
                    self.selection.insert(album.id);
                }
                if self.selection_anchor.is_none() {
                    self.selection_anchor = Some(id);
                }
            } else {
                self.selection.insert(id);
                self.selection_anchor = Some(id);
            }
            return;
        }
        if self.selection.remove(&id) {
            if self.selection_anchor == Some(id) {
                self.selection_anchor = None;
            }
        } else {
            self.selection.insert(id);
            self.selection_anchor = Some(id);
        }
    }

    fn watch_albums(&self) -> Vec<Album> {
        self.persisted
            .watchlist
            .iter()
            .filter(|album| match &self.watch_filter {
                WatchFilter::All => true,
                WatchFilter::Uncategorized => self
                    .persisted
                    .watch_folder_of(album.id)
                    .map(|folder| folder.trim().is_empty())
                    .unwrap_or(true),
                WatchFilter::Folder(name) => {
                    self.persisted.watch_folder_of(album.id) == Some(name.as_str())
                }
            })
            .cloned()
            .collect()
    }

    fn set_watch_filter(&mut self, filter: WatchFilter) {
        if self.watch_filter != filter {
            self.watch_filter = filter;
            self.selection.clear();
            self.selection_anchor = None;
        }
    }

    fn visible_albums(&self) -> Vec<Album> {
        match self.view {
            View::Discover => self.latest.clone(),
            View::Search => self.search.display.clone(),
            View::Watchlist => self.watch_albums(),
            View::Downloads | View::Library | View::Settings => Vec::new(),
        }
    }

    fn get_library_formats(&self) -> Vec<String> {
        let mut formats: Vec<String> = self
            .persisted
            .library
            .iter()
            .filter_map(|e| {
                let fmt = e.album.format.trim();
                if fmt.is_empty() {
                    None
                } else {
                    Some(fmt.to_string())
                }
            })
            .collect();
        formats.sort();
        formats.dedup();
        formats
    }

    fn get_library_years(&self) -> Vec<u16> {
        let mut years: Vec<u16> = self
            .persisted
            .library
            .iter()
            .filter_map(|e| {
                let year = e.album.year;
                if year == 0 {
                    None
                } else {
                    Some(year)
                }
            })
            .collect();
        years.sort();
        years.dedup();
        years.reverse(); // newest first
        years
    }

    fn find_album(&self, id: u64) -> Option<Album> {
        if let Some(album) = self.latest.iter().find(|a| a.id == id) {
            return Some(album.clone());
        }
        if let Some(album) = self.search.results.iter().find(|a| a.id == id) {
            return Some(album.clone());
        }
        if let Some(album) = self.persisted.watchlist.iter().find(|a| a.id == id) {
            return Some(album.clone());
        }
        if let Some(entry) = self.persisted.library.iter().find(|e| e.album.id == id) {
            return Some(entry.album.clone());
        }
        if let Some(detail) = &self.detail {
            if detail.album.id == id {
                return Some(detail.album.clone());
            }
        }
        None
    }

    fn download_selected(&mut self) {
        let albums: Vec<Album> = self
            .selection
            .iter()
            .filter_map(|id| self.find_album(*id))
            .collect();
        if albums.is_empty() {
            return;
        }
        let count = albums.len();
        for album in albums {
            self.downloads.enqueue(album);
        }
        self.toast(
            format!(
                "Queued {count} download{}",
                if count == 1 { "" } else { "s" }
            ),
            ToastTone::Success,
        );
        self.selection.clear();
        self.selection_anchor = None;
        self.view = View::Downloads;
    }

    fn request_images(&mut self, requests: Vec<ImageSpec>) {
        for spec in requests {
            self.images.request(spec);
        }
    }

    fn handle_card_actions(&mut self, actions: Vec<CardAction>) {
        for action in actions {
            match action {
                CardAction::Open(album) => self.open_detail(album),
                CardAction::ToggleSelect { id, shift } => self.toggle_select(id, shift),
                CardAction::Download(album) => self.queue_download(album),
                CardAction::Watch(album) => self.toggle_watch(album),
                CardAction::SetFolder { album, folder } => {
                    let title = album.title.clone();
                    if self.persisted.set_watch_folder(album.id, folder.clone()) {
                        self.dirty = true;
                        let label = folder.as_deref().unwrap_or("Uncategorized");
                        self.toast(format!("Moved “{title}” to {label}"), ToastTone::Info);
                    }
                }
                CardAction::OpenUrl(url) => {
                    let _ = webbrowser::open(&url);
                }
                CardAction::LoadMore => match self.view {
                    View::Discover => {
                        let page = self.latest_next_page;
                        self.request_latest_page(page);
                    }
                    View::Search => self.load_search_page(),
                    _ => {}
                },
            }
        }
    }

    fn handle_detail_actions(&mut self, actions: Vec<DetailAction>) {
        for action in actions {
            match action {
                DetailAction::Close => {
                    self.detail = None;
                    self.preview_cover = false;
                }
                DetailAction::Download(album) => self.queue_download(album),
                DetailAction::ToggleWatch(album) => self.toggle_watch(album),
                DetailAction::OpenUrl(url) => {
                    let _ = webbrowser::open(&url);
                }
                DetailAction::Search { text, kind } => self.search_for(text, kind),
                DetailAction::Retry => self.retry_detail(),
                DetailAction::Preview => self.preview_cover = true,
                DetailAction::WatchMetaChanged => self.dirty = true,
            }
        }
    }

    fn handle_task_actions(&mut self, actions: Vec<TaskAction>) {
        for action in actions {
            match action {
                TaskAction::Pause(id) => self.downloads.pause(id),
                TaskAction::Resume(id) => {
                    self.dl_ui.manual_opened.remove(&id);
                    self.downloads.resume(id);
                }
                TaskAction::Cancel(id) => self.downloads.cancel(id),
                TaskAction::Retry(id) => {
                    self.dl_ui.manual_opened.remove(&id);
                    self.downloads.retry(id);
                }
                TaskAction::Remove(id) => {
                    self.dl_ui.speeds.remove(&id);
                    self.dl_ui.manual_opened.remove(&id);
                    self.downloads.remove(id);
                }
                TaskAction::OpenFolder(id) => {
                    if let Some(task) = self.downloads.tasks.iter().find(|t| t.id == id) {
                        if let Some(folder) = &task.extracted {
                            store::open_path(folder);
                        } else if let Some(path) = &task.output {
                            store::reveal_path(path);
                        } else {
                            let dir = self.persisted.settings.download_dir.clone();
                            store::open_path(&dir);
                        }
                    }
                }
                TaskAction::OpenUrl(url) => {
                    let _ = webbrowser::open(&url);
                }
                TaskAction::OpenAlbum(album) => self.open_detail(album),
                TaskAction::PlayInFoobar(path) => self.play_in_foobar2000(&path),
            }
        }
    }

    fn play_in_foobar2000(&self, path: &Path) {
        if let Some(foobar_path) = &self.persisted.settings.foobar2000_path {
            if foobar_path.exists() {
                let _ = std::process::Command::new(foobar_path)
                    .arg("/add")
                    .arg(path)
                    .spawn();
            } else {
                eprintln!("foobar2000 not found at: {}", foobar_path.display());
            }
        }
    }

    fn check_manual_downloads(&mut self) {
        let mut newly_manual = Vec::new();
        for task in &self.downloads.tasks {
            if task.state == DownloadState::Manual && !self.dl_ui.manual_opened.contains(&task.id) {
                if let Some(url) = &task.manual_url {
                    newly_manual.push((task.id, url.clone(), task.album.title.clone()));
                }
            }
        }
        for (id, url, title) in newly_manual {
            self.dl_ui.manual_opened.insert(id);
            let _ = webbrowser::open(&url);
            self.toast(
                format!("“{title}” needs a browser. Opened the host page."),
                ToastTone::Info,
            );
        }
    }

    fn handle_download_notices(&mut self, notices: Vec<DownloadNotice>) {
        for notice in notices {
            match notice {
                DownloadNotice::Completed { title, output } => {
                    self.toast(format!("Downloaded “{title}”"), ToastTone::Success);
                    if self.persisted.settings.notify_on_complete {
                        let detail = output
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| format!("Saved {name}"))
                            .unwrap_or_else(|| "Download complete".to_string());
                        self.desktop_notify(&format!("Downloaded “{title}”"), &detail);
                    }
                }
                DownloadNotice::Failed { title, error } => {
                    self.toast(format!("Download failed: “{title}”"), ToastTone::Error);
                    self.desktop_notify(&format!("Download failed: “{title}”"), &error);
                }
            }
        }
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        let mut navigate = None;
        let cache_text = store::human_size(self.dl_ui.cache_bytes);
        let active = self.downloads.active_count();
        SidePanelLeft::show(ctx, self, &mut navigate, &cache_text, active);
        if let Some(view) = navigate {
            self.set_view(view);
        }
    }

    fn draw_detail_panel(&mut self, ctx: &egui::Context) {
        if self.detail.is_none() {
            return;
        }
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let screen_width = ctx.screen_rect().width();
        let compact = screen_width < 1000.0;
        let default_width = if screen_width < 820.0 {
            300.0
        } else if compact {
            320.0
        } else {
            DETAIL_WIDTH
        };
        let detail_margin = if compact { 16.0 } else { 22.0 };
        let album_id = self
            .detail
            .as_ref()
            .map(|detail| detail.album.id)
            .unwrap_or_default();
        let watched = self
            .persisted
            .watchlist
            .iter()
            .any(|album| album.id == album_id);
        let in_library = self.persisted.in_library(album_id);
        let persisted = &mut self.persisted;
        let watch_meta = if watched {
            Some(persisted.watch_meta.entry(album_id).or_default())
        } else {
            None
        };
        let watchlist = &persisted.watchlist;
        let folders = &persisted.watch_folders;
        {
            let detail = self.detail.as_mut().expect("checked");
            let textures = &mut self.textures;
            egui::SidePanel::right("detail-panel")
                .resizable(true)
                .default_width(default_width)
                .width_range(280.0..=520.0)
                .frame(Frame::none().fill(PANEL).inner_margin(Margin {
                    left: detail_margin,
                    right: detail_margin,
                    top: if compact { 14.0 } else { 18.0 },
                    bottom: 16.0,
                }))
                .show(ctx, |ui| {
                    actions = draw_detail(
                        ui,
                        detail,
                        textures,
                        watchlist,
                        in_library,
                        folders,
                        watch_meta,
                        &mut requests,
                    );
                });
        }
        self.request_images(requests);
        self.handle_detail_actions(actions);
    }

    fn draw_selection_bar(&mut self, ctx: &egui::Context) {
        if self.selection.is_empty() {
            return;
        }
        let count = self.selection.len();
        let mut download = false;
        let mut clear = false;
        let mut select_all = false;
        let narrow = ctx.available_rect().width() < 560.0;
        let pad = if narrow { 16.0 } else { 28.0 };
        egui::TopBottomPanel::bottom("selection-bar")
            .exact_height(if narrow { 96.0 } else { 66.0 })
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                    .inner_margin(Margin {
                        left: pad,
                        right: pad,
                        top: 10.0,
                        bottom: 10.0,
                    }),
            )
            .show(ctx, |ui| {
                let count_label = format!(
                    "{count} album{} selected",
                    if count == 1 { "" } else { "s" }
                );
                if narrow {
                    ui.horizontal(|ui| {
                        let dot = Rect::from_center_size(
                            pos2(ui.cursor().left() + 5.0, ui.cursor().center().y),
                            vec2(8.0, 8.0),
                        );
                        ui.painter().circle_filled(dot.center(), 4.0, accent());
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(&count_label)
                                .size(12.5)
                                .strong()
                                .color(TEXT),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Clear").size(12.0).color(MUTED),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                clear = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Select all loaded")
                                        .size(12.0)
                                        .color(DIM),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            select_all = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::primary_button(ui, &format!("Download {count}")).clicked() {
                                download = true;
                            }
                        });
                    });
                } else {
                    ui.horizontal_centered(|ui| {
                        let dot = Rect::from_center_size(
                            pos2(ui.cursor().left() + 5.0, ui.cursor().center().y),
                            vec2(8.0, 8.0),
                        );
                        ui.painter().circle_filled(dot.center(), 4.0, accent());
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(&count_label)
                                .size(13.5)
                                .strong()
                                .color(TEXT),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::primary_button(ui, &format!("Download {count}")).clicked() {
                                download = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Clear").size(12.5).color(MUTED),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                clear = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Select all loaded")
                                            .size(12.0)
                                            .color(DIM),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                select_all = true;
                            }
                        });
                    });
                }
            });
        if select_all {
            let ids: Vec<u64> = self.visible_albums().iter().map(|a| a.id).collect();
            for id in ids {
                self.selection.insert(id);
            }
            self.selection_anchor = None;
        }
        if download {
            self.download_selected();
        }
        if clear {
            self.selection.clear();
            self.selection_anchor = None;
        }
    }

    fn draw_central(&mut self, ctx: &egui::Context) {
        let compact = ctx.screen_rect().width() < 960.0;
        let (pad_left, pad_right, pad_top, pad_bottom) = if compact {
            (16.0, 16.0, 14.0, 14.0)
        } else {
            (28.0, 24.0, 20.0, 18.0)
        };
        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin {
                left: pad_left,
                right: pad_right,
                top: pad_top,
                bottom: pad_bottom,
            }))
            .show(ctx, |ui| match self.view {
                View::Discover => self.view_discover(ui),
                View::Search => self.view_search(ui),
                View::Watchlist => self.view_watchlist(ui),
                View::Library => self.view_library(ui),
                View::Downloads => self.view_downloads(ui),
                View::Settings => self.view_settings(ui, ctx),
            });
    }

    fn view_discover(&mut self, ui: &mut Ui) {
        let loaded = self.latest.len();
        view_header(
            ui,
            "LATEST RELEASES",
            "Fresh from the archive",
            "New doujin albums, updated as they are added to DoujinStyle.",
            |ui| {
                if theme::icon_button(ui, Icon::Refresh, 36.0, MUTED)
                    .on_hover_text("Refresh releases (Ctrl+R)")
                    .clicked()
                {
                    self.refresh_latest();
                }
                if loaded > 0 {
                    ui.label(
                        egui::RichText::new(format!("{loaded} loaded"))
                            .size(11.0)
                            .color(DIM),
                    );
                }
            },
        );
        ui.add_space(12.0);

        if self.latest_new_count > 0 && self.persisted.settings.show_new_badges {
            let text = if self.latest_new_count >= 20 {
                "20+ new releases since your last visit".to_string()
            } else {
                format!(
                    "{} new release{} since your last visit",
                    self.latest_new_count,
                    if self.latest_new_count == 1 { "" } else { "s" }
                )
            };
            Frame::none()
                .fill(accent_dim(22))
                .stroke(Stroke::new(1.0_f32, accent_dim(70)))
                .rounding(Rounding::same(10.0))
                .inner_margin(Margin {
                    left: 14.0,
                    right: 14.0,
                    top: 8.0,
                    bottom: 8.0,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        theme::paint_icon(
                            ui.painter(),
                            Rect::from_center_size(rect.center(), vec2(11.0, 11.0)),
                            Icon::Clock,
                            accent(),
                            1.4,
                        );
                        ui.label(egui::RichText::new(text).size(12.0).color(accent()));
                    });
                });
            ui.add_space(12.0);
        }

        if self.latest.is_empty() {
            if self.latest_loading {
                draw_loading(ui, "Loading the latest releases…");
            } else if let Some(error) = &self.latest_error {
                let mut retry = false;
                draw_empty(
                    ui,
                    Icon::Alert,
                    "Could not reach DoujinStyle",
                    error,
                    Some("Try again"),
                    &mut retry,
                );
                if retry {
                    self.refresh_latest();
                }
            } else {
                let mut refresh = false;
                draw_empty(
                    ui,
                    Icon::Compass,
                    "Nothing here yet",
                    "The archive returned no albums. Try refreshing.",
                    Some("Refresh"),
                    &mut refresh,
                );
                if refresh {
                    self.refresh_latest();
                }
            }
            return;
        }

        let footer = if self.latest_loading {
            GridFooter::Loading("Loading more releases…")
        } else if self.latest_error.is_some() {
            GridFooter::LoadMore("Try again")
        } else if self.latest_done {
            GridFooter::End("You have reached the beginning of the archive.")
        } else {
            GridFooter::LoadMore("Load more")
        };

        let mut requests = Vec::new();
        let (actions, near_bottom) = draw_grid(
            ui,
            &self.latest,
            &mut self.textures,
            &self.selection,
            &self.persisted.watchlist,
            &self.persisted.watch_meta,
            &self.persisted.watch_folders,
            &self.persisted.library,
            &mut requests,
            self.persisted.settings.show_new_badges,
            footer,
            &format!("latest-{}", self.latest_gen),
        );
        self.request_images(requests);
        let load_more = actions.iter().any(|a| matches!(a, CardAction::LoadMore));
        self.handle_card_actions(actions);
        let auto_load = near_bottom && self.latest_error.is_none();
        if load_more || auto_load {
            if !self.latest_loading && !self.latest_done {
                let page = self.latest_next_page;
                self.request_latest_page(page);
            }
        }
    }

    fn view_search(&mut self, ui: &mut Ui) {
        let mut run_search = false;
        Frame::none()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
            .rounding(Rounding::same(14.0))
            .inner_margin(Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let button_width = if ui.available_width() < 300.0 {
                        84.0
                    } else {
                        104.0
                    };
                    let field_width = (ui.available_width() - button_width - 10.0).max(80.0);
                    let response = draw_search_field(ui, &mut self.search.query, field_width);
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        run_search = true;
                    }
                    if ui
                        .add_sized(
                            [button_width, 38.0],
                            egui::Button::new(
                                egui::RichText::new("Search")
                                    .size(13.0)
                                    .strong()
                                    .color(Color32::from_rgb(14, 15, 21)),
                            )
                            .fill(accent())
                            .stroke(Stroke::NONE)
                            .rounding(Rounding::same(9.0)),
                        )
                        .clicked()
                    {
                        run_search = true;
                    }
                });
                ui.add_space(11.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("SEARCH BY")
                            .size(9.5)
                            .strong()
                            .color(DIM),
                    );
                    ui.add_space(2.0);
                    for kind in SearchKind::ALL {
                        if theme::chip(ui, kind.label(), self.search.kind == kind).clicked() {
                            self.search.kind = kind;
                        }
                    }
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new("FORMAT").size(9.5).strong().color(DIM));
                    ui.add_space(2.0);
                    let options = [
                        ("FLAC", self.search.formats.flac),
                        ("MP3", self.search.formats.mp3),
                        ("TTA", self.search.formats.tta),
                        ("Opus", self.search.formats.opus),
                        ("OGG", self.search.formats.ogg),
                    ];
                    for (label, active) in options {
                        if theme::chip(ui, label, active).clicked() {
                            toggle_format(&mut self.search.formats, label);
                        }
                    }
                });
            });
        ui.add_space(12.0);

        if run_search {
            self.submit_search();
        }

        let query = self.search.query.trim().to_string();
        let kind = self.search.kind;
        let formats = self.search.formats;
        let history: Vec<_> = self
            .persisted
            .search_history
            .iter()
            .take(8)
            .cloned()
            .collect();
        let saved: Vec<_> = self.persisted.saved_searches.clone();
        let mut apply_search: Option<(String, SearchKind, FormatFilter)> = None;
        let mut delete_saved: Option<String> = None;
        let mut clear_history = false;
        let mut open_save = false;
        let mut commit_save = false;
        let mut cancel_save = false;
        let mut new_sort: Option<SearchSort> = None;

        if !history.is_empty() || !saved.is_empty() || !query.is_empty() {
            Frame::none()
                .fill(CARD)
                .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                .rounding(Rounding::same(11.0))
                .inner_margin(Margin::symmetric(12.0, 9.0))
                .show(ui, |ui| {
                    if !history.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("RECENT").size(9.5).strong().color(DIM));
                            ui.add_space(2.0);
                            for entry in &history {
                                let response = theme::chip(ui, &ellipsize(&entry.text, 28), false);
                                if response
                                    .on_hover_text(format!(
                                        "{} · {}",
                                        entry.kind.label(),
                                        entry.formats.summary()
                                    ))
                                    .clicked()
                                {
                                    apply_search =
                                        Some((entry.text.clone(), entry.kind, entry.formats));
                                }
                            }
                            if theme::small_button(ui, "Clear", DIM).clicked() {
                                clear_history = true;
                            }
                        });
                    }
                    if !saved.is_empty() {
                        if !history.is_empty() {
                            ui.add_space(6.0);
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("SAVED").size(9.5).strong().color(DIM));
                            ui.add_space(2.0);
                            for entry in &saved {
                                ui.horizontal(|ui| {
                                    let response =
                                        theme::chip(ui, &ellipsize(&entry.name, 26), false);
                                    if response
                                        .on_hover_text(format!(
                                            "“{}” · {} · {}",
                                            entry.text,
                                            entry.kind.label(),
                                            entry.formats.summary()
                                        ))
                                        .clicked()
                                    {
                                        apply_search =
                                            Some((entry.text.clone(), entry.kind, entry.formats));
                                    }
                                    if theme::icon_button(ui, Icon::Close, 20.0, DIM)
                                        .on_hover_text("Delete saved search")
                                        .clicked()
                                    {
                                        delete_saved = Some(entry.name.clone());
                                    }
                                });
                            }
                        });
                    }
                    if !query.is_empty() {
                        if !history.is_empty() || !saved.is_empty() {
                            ui.add_space(6.0);
                        }
                        if theme::small_button(ui, "Save this search", MUTED).clicked() {
                            open_save = true;
                        }
                    }
                });
        }

        if open_save {
            self.search_save_draft = Some(query.clone());
        }
        if clear_history {
            if self.persisted.clear_search_history() {
                self.dirty = true;
                self.toast("Recent searches cleared.", ToastTone::Info);
            }
        }
        if let Some(name) = delete_saved {
            if self.persisted.remove_saved_search(&name) {
                self.dirty = true;
                self.toast(format!("Deleted saved search “{name}”."), ToastTone::Info);
            }
        }

        if let Some(draft) = self.search_save_draft.as_mut() {
            Frame::none()
                .fill(PANEL)
                .stroke(Stroke::new(1.0_f32, accent_dim(70)))
                .rounding(Rounding::same(11.0))
                .inner_margin(Margin::symmetric(12.0, 9.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("SAVE AS").size(9.5).strong().color(DIM));
                        let width = (ui.available_width() - 150.0).clamp(100.0, 360.0);
                        let response = ui.add_sized(
                            [width, 30.0],
                            TextEdit::singleline(draft)
                                .hint_text("Name this search")
                                .margin(Margin::symmetric(9.0, 6.0)),
                        );
                        if response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            commit_save = true;
                        }
                        if theme::primary_button_w(ui, "Save", 64.0).clicked() {
                            commit_save = true;
                        }
                        if theme::icon_button(ui, Icon::Close, 22.0, DIM)
                            .on_hover_text("Cancel")
                            .clicked()
                        {
                            cancel_save = true;
                        }
                    });
                });
            ui.add_space(4.0);
        }
        if commit_save {
            if let Some(draft) = self.search_save_draft.take() {
                let name = draft.trim();
                if name.is_empty() {
                    self.toast("Enter a name for this search.", ToastTone::Error);
                    self.search_save_draft = Some(draft);
                } else if self
                    .persisted
                    .save_search(name, &query, kind, formats, store::now_unix())
                {
                    self.dirty = true;
                    self.toast(format!("Saved search “{name}”."), ToastTone::Success);
                }
            }
        } else if cancel_save {
            self.search_save_draft = None;
        }

        if let Some((text, kind, formats)) = apply_search {
            self.run_search(text, kind, formats);
        }

        if self.search.searched {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("SORT").size(9.5).strong().color(DIM));
                ui.add_space(2.0);
                for sort in SearchSort::ALL {
                    if theme::chip(
                        ui,
                        sort.label(),
                        self.persisted.settings.search_sort == sort,
                    )
                    .clicked()
                    {
                        new_sort = Some(sort);
                    }
                }
            });
            if let Some(sort) = new_sort {
                self.set_search_sort(sort);
            }
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if self.search.searched {
                let title = if let Some(total) = self.search.total {
                    format!(
                        "{} result{} for “{}”",
                        format_number(total),
                        if total == 1 { "" } else { "s" },
                        self.search.query.trim()
                    )
                } else {
                    format!("Results for “{}”", self.search.query.trim())
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(title).size(14.0).strong().color(TEXT))
                        .truncate(),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}",
                        self.search.kind.label(),
                        self.search.formats.summary()
                    ))
                    .size(11.0)
                    .color(DIM),
                );
            } else {
                ui.label(
                    egui::RichText::new("Search the archive")
                        .size(15.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(
                        "Try a title, an artist, a tag like “Rock”, or the person who provided it.",
                    )
                    .size(11.5)
                    .color(MUTED),
                );
            }
        });
        ui.add_space(8.0);

        if self.search.searched && !self.search.results.is_empty() {
            ui.horizontal(|ui| {
                let count = self.search.display.len();
                if theme::secondary_button(ui, &format!("Download all {count}")).clicked() {
                    let albums: Vec<Album> = self.search.display.iter().cloned().collect();
                    for album in &albums {
                        self.downloads.enqueue(album.clone());
                    }
                    self.toast(
                        format!(
                            "Queued {count} download{}",
                            if count == 1 { "" } else { "s" }
                        ),
                        ToastTone::Success,
                    );
                    self.view = View::Downloads;
                }
            });
            ui.add_space(8.0);
        }

        if !self.search.searched {
            draw_empty(
                ui,
                Icon::Search,
                "Find your next album",
                "Use the field above to search across thousands of doujin releases. Press Enter to search.",
                None,
                &mut false,
            );
            return;
        }

        if self.search.results.is_empty() {
            if self.search.loading {
                draw_loading(ui, "Searching DoujinStyle…");
            } else if let Some(error) = &self.search.error {
                let mut retry = false;
                draw_empty(
                    ui,
                    Icon::Alert,
                    "Search failed",
                    error,
                    Some("Try again"),
                    &mut retry,
                );
                if retry {
                    self.submit_search();
                }
            } else {
                draw_empty(
                    ui,
                    Icon::Search,
                    "No albums found",
                    "Try a broader query, a different search type, or clear the format filters.",
                    None,
                    &mut false,
                );
            }
            return;
        }

        let footer = if self.search.loading {
            GridFooter::Loading("Loading more results…")
        } else if self.search.error.is_some() {
            GridFooter::LoadMore("Try again")
        } else if self.search.done {
            GridFooter::End("End of results.")
        } else {
            GridFooter::LoadMore("Load more")
        };

        let mut requests = Vec::new();
        let (actions, near_bottom) = draw_grid(
            ui,
            &self.search.display,
            &mut self.textures,
            &self.selection,
            &self.persisted.watchlist,
            &self.persisted.watch_meta,
            &self.persisted.watch_folders,
            &self.persisted.library,
            &mut requests,
            false,
            footer,
            &format!("search-{}", self.search.gen),
        );
        self.request_images(requests);
        let load_more = actions.iter().any(|a| matches!(a, CardAction::LoadMore));
        self.handle_card_actions(actions);
        let auto_load = near_bottom && self.search.error.is_none();
        if (load_more || auto_load) && !self.search.loading && !self.search.done {
            self.load_search_page();
        }
    }

    fn view_watchlist(&mut self, ui: &mut Ui) {
        let total = self.persisted.watchlist.len();
        let visible = self.watch_albums();
        let note_count = self
            .persisted
            .watchlist
            .iter()
            .filter(|album| !self.persisted.watch_note_of(album.id).trim().is_empty())
            .count();
        let plural = if total == 1 { "" } else { "s" };
        let subtitle = if total == 0 {
            "No albums saved yet".to_string()
        } else {
            let mut text = if visible.len() == total {
                format!("{total} album{plural} saved")
            } else {
                format!("{} of {total} album{plural} in this folder", visible.len())
            };
            if note_count > 0 {
                text.push_str(&format!(
                    " · {note_count} note{}",
                    if note_count == 1 { "" } else { "s" }
                ));
            }
            text
        };
        let mut add_folder = false;
        let download_albums = visible.clone();
        view_header(ui, "COLLECTION", "Your watchlist", &subtitle, |ui| {
            if !download_albums.is_empty() {
                let label = if download_albums.len() == total {
                    "Download all"
                } else {
                    "Download shown"
                };
                if theme::secondary_button(ui, label).clicked() {
                    let count = download_albums.len();
                    for album in &download_albums {
                        self.downloads.enqueue(album.clone());
                    }
                    self.toast(
                        format!(
                            "Queued {count} download{}",
                            if count == 1 { "" } else { "s" }
                        ),
                        ToastTone::Success,
                    );
                    self.view = View::Downloads;
                }
            }
            if theme::secondary_button(ui, "New folder").clicked() {
                add_folder = true;
            }
        });
        if add_folder {
            self.folder_draft = Some(String::new());
        }
        ui.add_space(10.0);

        let folders = self.persisted.watch_folders.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("FOLDERS").size(9.5).strong().color(DIM));
            if theme::chip(ui, "All", self.watch_filter == WatchFilter::All).clicked() {
                self.set_watch_filter(WatchFilter::All);
            }
            if theme::chip(
                ui,
                "Uncategorized",
                self.watch_filter == WatchFilter::Uncategorized,
            )
            .clicked()
            {
                self.set_watch_filter(WatchFilter::Uncategorized);
            }
            for folder in &folders {
                let selected = self.watch_filter == WatchFilter::Folder(folder.clone());
                if theme::chip(ui, folder, selected).clicked() {
                    self.set_watch_filter(WatchFilter::Folder(folder.clone()));
                }
            }
        });

        if let WatchFilter::Folder(name) = self.watch_filter.clone() {
            ui.horizontal_wrapped(|ui| {
                if theme::small_button(ui, "Rename folder", MUTED).clicked() {
                    self.folder_rename = Some((name.clone(), name.clone()));
                }
                if theme::small_button(ui, "Delete folder", DANGER).clicked() {
                    self.persisted.remove_watch_folder(&name);
                    self.set_watch_filter(WatchFilter::All);
                    self.folder_rename = None;
                    self.dirty = true;
                    self.toast(
                        format!("Deleted folder “{name}”. Albums were kept."),
                        ToastTone::Info,
                    );
                }
            });
        }

        if self.folder_draft.is_some() {
            let mut create = false;
            let mut cancel = false;
            {
                let draft = self.folder_draft.as_mut().expect("checked");
                Frame::none()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                    .rounding(Rounding::same(9.0))
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("NEW FOLDER")
                                    .size(9.5)
                                    .strong()
                                    .color(DIM),
                            );
                            let field = ui.add_sized(
                                [220.0, 26.0],
                                TextEdit::singleline(draft)
                                    .id(Id::new("new-watch-folder"))
                                    .hint_text("Folder name")
                                    .font(FontId::proportional(12.5)),
                            );
                            if field.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter))
                            {
                                create = true;
                            }
                            if theme::primary_button(ui, "Create").clicked() {
                                create = true;
                            }
                            if theme::small_button(ui, "Cancel", MUTED).clicked() {
                                cancel = true;
                            }
                        });
                    });
            }
            if create {
                let name = self.folder_draft.take().unwrap_or_default();
                if self.persisted.add_watch_folder(&name) {
                    let name = name.trim().to_string();
                    self.set_watch_filter(WatchFilter::Folder(name.clone()));
                    self.dirty = true;
                    self.toast(format!("Created folder “{name}”"), ToastTone::Success);
                } else {
                    self.toast("Folder name is empty or already exists.", ToastTone::Error);
                    self.folder_draft = Some(name);
                }
            } else if cancel {
                self.folder_draft = None;
            }
        }

        if let Some((old, _)) = self.folder_rename.clone() {
            let mut save = false;
            let mut cancel = false;
            {
                let (_, draft) = self.folder_rename.as_mut().expect("checked");
                Frame::none()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                    .rounding(Rounding::same(9.0))
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("RENAME “{old}”"))
                                    .size(9.5)
                                    .strong()
                                    .color(DIM),
                            );
                            let field = ui.add_sized(
                                [220.0, 26.0],
                                TextEdit::singleline(draft)
                                    .id(Id::new("rename-watch-folder"))
                                    .font(FontId::proportional(12.5)),
                            );
                            if field.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter))
                            {
                                save = true;
                            }
                            if theme::primary_button(ui, "Save").clicked() {
                                save = true;
                            }
                            if theme::small_button(ui, "Cancel", MUTED).clicked() {
                                cancel = true;
                            }
                        });
                    });
            }
            if save {
                let (old, new) = self.folder_rename.take().expect("checked");
                match self.persisted.rename_watch_folder(&old, &new) {
                    Ok(()) => {
                        if self.watch_filter == WatchFilter::Folder(old.clone()) {
                            self.set_watch_filter(WatchFilter::Folder(new.trim().to_string()));
                        }
                        self.dirty = true;
                        self.toast(
                            format!("Renamed folder “{old}” to “{}”", new.trim()),
                            ToastTone::Success,
                        );
                    }
                    Err(error) => {
                        self.toast(error, ToastTone::Error);
                        self.folder_rename = Some((old, new));
                    }
                }
            } else if cancel {
                self.folder_rename = None;
            }
        }

        ui.add_space(12.0);

        if total == 0 {
            draw_empty(
                ui,
                Icon::Star,
                "Your watchlist is empty",
                "Tap the star on any album card to keep it here for later.",
                None,
                &mut false,
            );
            return;
        }

        if visible.is_empty() {
            draw_empty(
                ui,
                Icon::Folder,
                "This folder is empty",
                "Right-click an album in your watchlist to move it into this folder.",
                None,
                &mut false,
            );
            return;
        }

        let mut requests = Vec::new();
        let (actions, _) = draw_grid(
            ui,
            &visible,
            &mut self.textures,
            &self.selection,
            &self.persisted.watchlist,
            &self.persisted.watch_meta,
            &self.persisted.watch_folders,
            &self.persisted.library,
            &mut requests,
            false,
            GridFooter::None,
            "watchlist",
        );
        self.request_images(requests);
        self.handle_card_actions(actions);
    }

    fn view_library(&mut self, ui: &mut Ui) {
        let count = self.persisted.library.len();
        let total: u64 = self.persisted.library.iter().map(|entry| entry.size).sum();
        let subtitle = format!(
            "{} album{} · {} on disk",
            count,
            if count == 1 { "" } else { "s" },
            store::human_size(total)
        );
        view_header(ui, "COLLECTION", "Your library", &subtitle, |ui| {
            if theme::secondary_button(ui, "Open downloads folder").clicked() {
                let dir = self.persisted.settings.download_dir.clone();
                let _ = std::fs::create_dir_all(&dir);
                store::open_path(&dir);
            }
        });
        ui.add_space(8.0);

        // Filter toolbar
        ui.horizontal(|ui| {
            // Format filter
            let formats = self.get_library_formats();
            let format_label = self.library_format_filter
                .as_ref()
                .map(|f| f.as_str())
                .unwrap_or("All formats");
            egui::ComboBox::from_id_salt("library_format_filter")
                .selected_text(format_label)
                .width(140.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.library_format_filter, None, "All formats");
                    for fmt in &formats {
                        ui.selectable_value(&mut self.library_format_filter, Some(fmt.clone()), fmt);
                    }
                });
            
            ui.add_space(8.0);
            
            // Year filter
            let years = self.get_library_years();
            let year_label = self.library_year_filter
                .map(|y| y.to_string())
                .unwrap_or_else(|| "All years".to_string());
            egui::ComboBox::from_id_salt("library_year_filter")
                .selected_text(year_label)
                .width(110.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.library_year_filter, None, "All years");
                    for year in &years {
                        ui.selectable_value(&mut self.library_year_filter, Some(*year), year.to_string());
                    }
                });

            ui.add_space(16.0);
            
            // Search box
            let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            theme::paint_icon(ui.painter(), rect, Icon::Search, DIM, 1.4);
            let width = (ui.available_width() - 36.0).max(60.0);
            ui.add_sized(
                [width, 24.0],
                TextEdit::singleline(&mut self.library_query)
                    .id(Id::new("library-filter"))
                    .frame(false)
                    .hint_text("Filter by title, artist or folder…")
                    .font(FontId::proportional(13.0)),
            );
            if !self.library_query.is_empty()
                && theme::small_button(ui, "Clear", DIM).clicked()
            {
                self.library_query.clear();
            }
            
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Clear all filters
                let has_filters = self.library_format_filter.is_some() 
                    || self.library_year_filter.is_some() 
                    || !self.library_query.is_empty();
                if has_filters && theme::small_button(ui, "Clear all", DIM).clicked() {
                    self.library_format_filter = None;
                    self.library_year_filter = None;
                    self.library_query.clear();
                }
            });
        });
        ui.add_space(10.0);

        if count == 0 {
            let mut browse = false;
            draw_empty(
                ui,
                Icon::Folder,
                "Your library is empty",
                "Finished downloads appear here automatically, with quick access to their folders.",
                Some("Browse releases"),
                &mut browse,
            );
            if browse {
                self.set_view(View::Discover);
            }
            return;
        }

        let query = self.library_query.trim().to_lowercase();
        let format_filter = &self.library_format_filter;
        let year_filter = self.library_year_filter;
        let visible: Vec<usize> = self
            .persisted
            .library
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                let matches_query = query.is_empty()
                    || entry.album.title.to_lowercase().contains(&query)
                    || entry.album.artist.to_lowercase().contains(&query)
                    || entry.path.to_string_lossy().to_lowercase().contains(&query);
                
                let matches_format = format_filter.as_ref().map_or(true, |f| {
                    entry.album.format.trim().eq_ignore_ascii_case(f)
                });
                
                let matches_year = year_filter.map_or(true, |y| {
                    entry.album.year == y
                });
                
                matches_query && matches_format && matches_year
            })
            .map(|(index, _)| index)
            .collect();

        if visible.is_empty() {
            draw_empty(
                ui,
                Icon::Search,
                "No matching albums",
                "Try a different title, artist or folder name.",
                None,
                &mut false,
            );
            return;
        }

        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let persisted = &self.persisted;
        let textures = &mut self.textures;
        ScrollArea::vertical()
            .id_salt("library-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in visible {
                    let entry = &persisted.library[index];
                    let missing = entry.missing();
                    let response = Frame::none()
                        .fill(CARD)
                        .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                        .rounding(Rounding::same(11.0))
                        .inner_margin(Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (cover, cover_response) =
                                    ui.allocate_exact_size(vec2(56.0, 56.0), Sense::click());
                                let key = format!("t{}", entry.album.id);
                                if let Some(texture) = textures.get(&key) {
                                    paint_texture_cover(
                                        ui.painter(),
                                        cover,
                                        texture,
                                        Rounding::same(8.0),
                                    );
                                } else {
                                    paint_cover_placeholder(
                                        ui.painter(),
                                        cover,
                                        &entry.album.title,
                                    );
                                    requests.push(ImageSpec {
                                        key: key.clone(),
                                        url: entry.album.thumb_url(),
                                        cache_path: store::thumb_cache_path(entry.album.id),
                                        max_dim: 220,
                                    });
                                }
                                if cover_response.clicked() {
                                    actions.push(LibraryAction::Open(entry.album.clone()));
                                }
                                ui.add_space(10.0);

                                let buttons_width = 70.0;
                                let text_width = (ui.available_width() - buttons_width).max(80.0);
                                ui.allocate_ui_with_layout(
                                    vec2(text_width, 56.0),
                                    Layout::top_down(Align::LEFT),
                                    |ui| {
                                        let title = elide_text(
                                            ui.painter(),
                                            &entry.album.title,
                                            13.0,
                                            text_width - 2.0,
                                        );
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    egui::RichText::new(title)
                                                        .size(13.0)
                                                        .strong()
                                                        .color(if missing { MUTED } else { TEXT }),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .clicked()
                                        {
                                            actions.push(LibraryAction::Open(entry.album.clone()));
                                        }
                                        let artist = if entry.album.artist.trim().is_empty() {
                                            "Unknown artist"
                                        } else {
                                            entry.album.artist.trim()
                                        };
                                        ui.label(
                                            egui::RichText::new(elide_text(
                                                ui.painter(),
                                                artist,
                                                11.0,
                                                text_width - 2.0,
                                            ))
                                            .size(11.0)
                                            .color(MUTED),
                                        );
                                        ui.add_space(2.0);
                                        ui.horizontal_wrapped(|ui| {
                                            let mut parts = Vec::new();
                                            if !entry.album.format.is_empty() {
                                                parts.push(entry.album.format.clone());
                                            }
                                            parts.push(store::human_size(entry.size));
                                            let when = format_relative_time(entry.completed_at);
                                            if !when.is_empty() {
                                                parts.push(when);
                                            }
                                            ui.label(
                                                egui::RichText::new(parts.join(" · "))
                                                    .size(10.0)
                                                    .color(DIM),
                                            );
                                            if missing {
                                                theme::badge(
                                                    ui,
                                                    "MISSING",
                                                    DANGER,
                                                    Color32::from_rgb(38, 23, 29),
                                                );
                                            } else if entry.warning.is_some() {
                                                theme::badge(
                                                    ui,
                                                    "WARNING",
                                                    WARNING,
                                                    Color32::from_rgb(38, 32, 20),
                                                );
                                            }
                                        });
                                    },
                                );

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if theme::icon_button(ui, Icon::Trash, 30.0, MUTED)
                                        .on_hover_text("Remove from library")
                                        .clicked()
                                    {
                                        actions.push(LibraryAction::Remove(entry.album.id));
                                    }
                                    if !entry.extracted.as_ref().map(|p| p.exists()).unwrap_or(false) && !entry.path.exists() {
                                        // Don't offer foobar if files don't exist
                                    } else if theme::icon_button(ui, Icon::Folder, 30.0, MUTED)
                                        .on_hover_text("Open folder")
                                        .clicked()
                                    {
                                        actions.push(LibraryAction::OpenFolder(entry.folder()));
                                    } else if theme::icon_button(ui, Icon::External, 30.0, MUTED)
                                        .on_hover_text("Play in foobar2000")
                                        .clicked()
                                    {
                                        actions.push(LibraryAction::PlayInFoobar(entry.folder()));
                                    }
                                });
                            });
                        })
                        .response;
                    response.on_hover_text(entry.path.display().to_string());
                    ui.add_space(8.0);
                }
            });
        self.request_images(requests);

        for action in actions {
            match action {
                LibraryAction::Open(album) => self.open_detail(album),
                LibraryAction::OpenFolder(path) => {
                    if path.exists() {
                        store::open_path(&path);
                    } else {
                        self.toast("That folder no longer exists.", ToastTone::Error);
                    }
                }
                LibraryAction::Remove(album_id) => {
                    let title = self
                        .persisted
                        .library
                        .iter()
                        .find(|entry| entry.album.id == album_id)
                        .map(|entry| entry.album.title.clone())
                        .unwrap_or_default();
                    self.persisted
                        .library
                        .retain(|entry| entry.album.id != album_id);
                    self.downloads.forget_done_album(album_id);
                    self.dirty = true;
                    self.toast(
                        format!("Removed “{title}” from your library"),
                        ToastTone::Info,
                    );
                }
                LibraryAction::PlayInFoobar(path) => self.play_in_foobar2000(&path),
            }
        }
    }

    fn view_downloads(&mut self, ui: &mut Ui) {
        let has_tasks = !self.downloads.tasks.is_empty();
        let active = self
            .downloads
            .tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.state,
                    DownloadState::Resolving
                        | DownloadState::Downloading
                        | DownloadState::Extracting
                )
            })
            .count();
        let paused = self.downloads.paused_count();
        let failed = self.downloads.failed_count();
        let mut parts = vec![
            format!("{active} active"),
            format!("{} queued", self.downloads.queued_count()),
        ];
        if paused > 0 {
            parts.push(format!("{paused} paused"));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        parts.push(format!("{} total", self.downloads.tasks.len()));
        let subtitle = parts.join(" · ");
        view_header(ui, "TRANSFERS", "Download queue", &subtitle, |ui| {
            let running = self.downloads.running_count();
            let paused = self.downloads.paused_count();
            let failed = self.downloads.failed_count();
            if paused > 0
                && theme::secondary_button(ui, &format!("Resume all ({paused})")).clicked()
            {
                self.downloads.resume_all();
            }
            if running > 0 && theme::secondary_button(ui, "Pause all").clicked() {
                self.downloads.pause_all();
            }
            if failed > 0
                && theme::secondary_button(ui, &format!("Retry failed ({failed})")).clicked()
            {
                self.downloads.retry_all_failed();
            }
            if has_tasks && theme::secondary_button(ui, "Clear finished").clicked() {
                self.downloads.clear_finished();
            }
            if theme::secondary_button(ui, "Open folder").clicked() {
                let dir = self.persisted.settings.download_dir.clone();
                let _ = std::fs::create_dir_all(&dir);
                store::open_path(&dir);
            }
        });
        ui.add_space(14.0);

        if self.downloads.tasks.is_empty() {
            draw_empty(
                ui,
                Icon::Download,
                "No downloads yet",
                "Pick an album and hit Download. Your queue, progress, and finished files will appear here.",
                None,
                &mut false,
            );
            return;
        }

        let tasks = &self.downloads.tasks;
        let speeds = &mut self.dl_ui.speeds;
        let now = Instant::now();
        for task in tasks {
            if task.state == DownloadState::Downloading {
                let downloaded = task.progress.downloaded();
                let entry = speeds.entry(task.id).or_insert((downloaded, now, 0.0));
                let elapsed = now.duration_since(entry.1).as_secs_f32();
                if elapsed >= 0.3 {
                    let instant = (downloaded.saturating_sub(entry.0)) as f32 / elapsed;
                    entry.2 = if entry.2 <= 0.1 {
                        instant
                    } else {
                        entry.2 * 0.62 + instant * 0.38
                    };
                    entry.0 = downloaded;
                    entry.1 = now;
                }
            }
        }

        let mut actions = Vec::new();
        ScrollArea::vertical()
            .id_salt("downloads-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in 0..tasks.len() {
                    let task = &tasks[index];
                    let speed = speeds.get(&task.id).map(|(_, _, s)| *s).unwrap_or(0.0);
                    task_row(ui, task, speed, &mut actions);
                    ui.add_space(10.0);
                }
            });
        self.handle_task_actions(actions);

        if self.downloads.running_count() > 0 {
            ui.ctx().request_repaint_after(Duration::from_millis(180));
        }
    }

    fn view_settings(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut changed = false;
        let mut rules_changed = false;
        let mut remove_rule: Option<usize> = None;
        let mut add_rule = false;
        let mut commit_rule = false;
        let mut cancel_rule = false;
        let mut draft = self.rule_draft.take();
        ui.label(
            egui::RichText::new("PREFERENCES")
                .size(10.5)
                .strong()
                .color(accent()),
        );
        ui.add_space(3.0);
        ui.label(
            egui::RichText::new("Settings")
                .font(FontId::new(27.0, FontFamily::Proportional))
                .strong()
                .color(TEXT),
        );
        ui.add_space(12.0);

        ScrollArea::vertical()
            .id_salt("settings-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                settings_card(ui, "Downloads", |ui| {
                    ui.label(
                        egui::RichText::new("Save location")
                            .size(12.0)
                            .color(MUTED),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        Frame::none()
                            .fill(BG)
                            .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin {
                                left: 10.0,
                                right: 10.0,
                                top: 7.0,
                                bottom: 7.0,
                            })
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            self.persisted.settings.download_dir.display().to_string(),
                                        )
                                        .size(11.5)
                                        .color(TEXT),
                                    )
                                    .truncate(),
                                );
                            });
                        if theme::small_button(ui, "Change…", TEXT).clicked() {
                            if let Some(folder) = rfd::FileDialog::new()
                                .set_title("Choose a download folder")
                                .set_directory(&self.persisted.settings.download_dir)
                                .pick_folder()
                            {
                                self.persisted.settings.download_dir = folder;
                                changed = true;
                            }
                        }
                        if theme::small_button(ui, "Open", TEXT).clicked() {
                            let dir = self.persisted.settings.download_dir.clone();
                            let _ = std::fs::create_dir_all(&dir);
                            store::open_path(&dir);
                        }
                    });
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Simultaneous downloads")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(
                                    self.persisted.settings.max_concurrent.to_string(),
                                )
                                .size(12.0)
                                .strong()
                                .color(accent()),
                            );
                            if ui
                                .add_sized(
                                    [170.0, 20.0],
                                    egui::Slider::new(
                                        &mut self.persisted.settings.max_concurrent,
                                        1..=8,
                                    )
                                    .show_value(false),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(12.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.auto_extract,
                            egui::RichText::new("Automatically extract ZIP archives")
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("foobar2000 path")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            Frame::none()
                                .fill(BG)
                                .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                                .rounding(Rounding::same(8.0))
                                .inner_margin(Margin {
                                    left: 10.0,
                                    right: 10.0,
                                    top: 7.0,
                                    bottom: 7.0,
                                })
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(
                                                self.persisted.settings.foobar2000_path
                                                    .as_ref()
                                                    .map(|p| p.display().to_string())
                                                    .unwrap_or_else(|| "Not set".to_string()),
                                            )
                                            .size(11.5)
                                            .color(TEXT),
                                        )
                                        .truncate(),
                                    );
                                });
                            if theme::small_button(ui, "Browse…", TEXT).clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("Select foobar2000.exe")
                                    .add_filter("Executable", &["exe"])
                                    .pick_file()
                                {
                                    self.persisted.settings.foobar2000_path = Some(path);
                                    changed = true;
                                }
                            }
                            if theme::small_button(ui, "Clear", TEXT).clicked() {
                                self.persisted.settings.foobar2000_path = None;
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.auto_retry,
                            egui::RichText::new("Auto-retry failed downloads (temporary errors)")
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Max retries")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.persisted.settings.max_retries)
                                    .range(0..=10)
                                    .speed(0.5)
                                    .suffix(" attempts"),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.show_new_badges,
                            egui::RichText::new("Show NEW badges for unseen releases")
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, "Auto-save rules", |ui| {
                    ui.label(
                        egui::RichText::new(
                            "New Discover releases that match an enabled rule are saved to your watchlist automatically.",
                        )
                        .size(11.5)
                        .color(MUTED),
                    );
                    ui.add_space(10.0);
                    if self.persisted.save_rules.is_empty() && draft.is_none() {
                        ui.label(
                            egui::RichText::new("No rules yet.")
                                .size(11.5)
                                .color(DIM),
                        );
                        ui.add_space(8.0);
                    }
                    for (index, rule) in self.persisted.save_rules.iter_mut().enumerate() {
                        Frame::none()
                            .fill(BG)
                            .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut rule.enabled, "")
                                        .on_hover_text("Enable or pause this rule")
                                        .changed()
                                    {
                                        rules_changed = true;
                                    }
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(&rule.query)
                                                .size(12.0)
                                                .strong()
                                                .color(TEXT),
                                        );
                                        let mut details = rule.field.label().to_string();
                                        if let Some(folder) = &rule.folder {
                                            details.push_str(&format!(" · folder: {folder}"));
                                        }
                                        ui.label(
                                            egui::RichText::new(details)
                                                .size(10.5)
                                                .color(DIM),
                                        );
                                    });
                                    ui.with_layout(
                                        Layout::right_to_left(Align::Center),
                                        |ui| {
                                            if theme::icon_button(ui, Icon::Trash, 24.0, DIM)
                                                .on_hover_text("Delete rule")
                                                .clicked()
                                            {
                                                remove_rule = Some(index);
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(6.0);
                    }

                    if let Some(draft) = draft.as_mut() {
                        Frame::none()
                            .fill(BG)
                            .stroke(Stroke::new(1.0_f32, accent_dim(70)))
                            .rounding(Rounding::same(9.0))
                            .inner_margin(Margin::symmetric(10.0, 9.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new("MATCH TEXT")
                                        .size(9.5)
                                        .strong()
                                        .color(DIM),
                                );
                                ui.add_space(3.0);
                                ui.add_sized(
                                    [ui.available_width().min(360.0), 30.0],
                                    TextEdit::singleline(&mut draft.query)
                                        .hint_text("Artist, title, or tag…")
                                        .margin(Margin::symmetric(9.0, 6.0)),
                                );
                                ui.add_space(7.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new("IN")
                                            .size(9.5)
                                            .strong()
                                            .color(DIM),
                                    );
                                    for field in RuleField::ALL {
                                        if theme::chip(
                                            ui,
                                            field.label(),
                                            draft.field == field,
                                        )
                                        .clicked()
                                        {
                                            draft.field = field;
                                        }
                                    }
                                });
                                ui.add_space(7.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new("FOLDER")
                                            .size(9.5)
                                            .strong()
                                            .color(DIM),
                                    );
                                    ui.add_sized(
                                        [180.0, 28.0],
                                        TextEdit::singleline(&mut draft.folder)
                                            .hint_text("Optional")
                                            .margin(Margin::symmetric(9.0, 5.0)),
                                    );
                                    if theme::primary_button_w(ui, "Add rule", 82.0).clicked() {
                                        commit_rule = true;
                                    }
                                    if theme::small_button(ui, "Cancel", DIM).clicked() {
                                        cancel_rule = true;
                                    }
                                });
                            });
                    } else if theme::small_button(ui, "Add rule", TEXT).clicked() {
                        add_rule = true;
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, "Appearance", |ui| {
                    ui.label(
                        egui::RichText::new("Accent color")
                            .size(12.0)
                            .color(MUTED),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        for (index, (name, color)) in ACCENTS.iter().enumerate() {
                            let selected = self.persisted.settings.accent == index;
                            let (rect, response) =
                                ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
                            if response.hovered() || selected {
                                ui.painter().circle_stroke(
                                    rect.center(),
                                    16.0,
                                    Stroke::new(2.0_f32, *color),
                                );
                            }
                            ui.painter().circle_filled(rect.center(), 11.0, *color);
                            if selected {
                                theme::paint_icon(
                                    ui.painter(),
                                    Rect::from_center_size(rect.center(), vec2(12.0, 12.0)),
                                    Icon::Check,
                                    Color32::from_rgb(16, 17, 23),
                                    1.8,
                                );
                            }
                            if response.clicked() {
                                self.persisted.settings.accent = index;
                                theme::configure(ctx, index);
                                changed = true;
                            }
                            response.on_hover_text(*name);
                        }
                    });
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Thumbnail memory budget")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} textures",
                                    self.persisted.settings.texture_budget
                                ))
                                .size(12.0)
                                .strong()
                                .color(accent()),
                            );
                            if ui
                                .add_sized(
                                    [190.0, 20.0],
                                    egui::Slider::new(
                                        &mut self.persisted.settings.texture_budget,
                                        200..=1600,
                                    )
                                    .step_by(50.0)
                                    .show_value(false),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                    });
                });

                ui.add_space(14.0);
                settings_card(ui, "Notifications & tray", |ui| {
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.notify_on_complete,
                            egui::RichText::new(
                                "Show desktop notifications when downloads finish or fail",
                            )
                            .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(6.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.show_tray_icon,
                            egui::RichText::new("Show a tray icon").size(12.5),
                        )
                        .changed()
                    {
                        if !self.persisted.settings.show_tray_icon {
                            self.persisted.settings.close_to_tray = false;
                        }
                        self.tray_attempted = false;
                        changed = true;
                    }
                    ui.add_space(6.0);
                    let tray_available = self.persisted.settings.show_tray_icon;
                    if ui
                        .add_enabled(
                            tray_available,
                            egui::Checkbox::new(
                                &mut self.persisted.settings.close_to_tray,
                                egui::RichText::new(
                                    "Closing the window keeps the app running in the tray",
                                )
                                .size(12.5),
                            ),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, "Storage", |ui| {
                    self.dl_ui.cache_bytes = store::cache_size();
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("Image cache")
                                    .size(12.5)
                                    .strong()
                                    .color(TEXT),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} stored on disk",
                                    store::human_size(self.dl_ui.cache_bytes)
                                ))
                                .size(11.5)
                                .color(MUTED),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::secondary_button(ui, "Clear cache").clicked() {
                                store::clear_cache();
                                self.textures = Textures::default();
                                self.images.clear_failed();
                                self.images.reset_requests();
                                self.dl_ui.cache_bytes = store::cache_size();
                                self.toast("Image cache cleared", ToastTone::Success);
                            }
                            if theme::secondary_button(ui, "Open data folder").clicked() {
                                store::open_path(&store::data_dir());
                            }
                        });
                    });
                });
                
                ui.add_space(14.0);
                settings_card(ui, "Import / Export", |ui| {
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, "Export all").clicked() {
                            self.export_all();
                        }
                        if theme::secondary_button(ui, "Import…").clicked() {
                            self.import_all();
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Exports/imports settings, watchlist, library, and auto-save rules as JSON.")
                            .size(10.5)
                            .color(DIM),
                    );
                });
                
                ui.add_space(14.0);
                settings_card(ui, "About", |ui| {
                    ui.label(
                        egui::RichText::new("DST-Explorer")
                            .size(15.0)
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        egui::RichText::new(format!("Version {} · unofficial desktop client", env!("CARGO_PKG_VERSION")))
                            .size(11.5)
                            .color(MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "Browse and download from DoujinStyle.com with fast previews, a virtualized grid, and a resumable download queue. All music, games, and artwork belong to their respective creators. Please support the original artists.",
                        )
                        .size(11.5)
                        .color(MUTED),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, "Open DoujinStyle").clicked() {
                            let _ = webbrowser::open("https://doujinstyle.com");
                        }
                        if theme::secondary_button(ui, "Show shortcuts").clicked() {
                            self.toast(
                                "Ctrl+F search · Ctrl+R refresh · Ctrl+D download · Esc close",
                                ToastTone::Info,
                            );
                        }
                    });
                });
                ui.add_space(20.0);
            });

        self.rule_draft = draft;
        if add_rule {
            self.rule_draft = Some(RuleDraft::default());
        }
        if cancel_rule {
            self.rule_draft = None;
        }
        if commit_rule {
            if let Some(draft) = self.rule_draft.take() {
                let rule = SaveRule {
                    query: draft.query.clone(),
                    field: draft.field,
                    folder: Some(draft.folder.clone()),
                    enabled: true,
                };
                match self.persisted.add_save_rule(rule) {
                    Ok(()) => {
                        rules_changed = true;
                        self.toast("Auto-save rule added.", ToastTone::Success);
                    }
                    Err(error) => {
                        self.toast(error, ToastTone::Error);
                        self.rule_draft = Some(draft);
                    }
                }
            }
        }
        if let Some(index) = remove_rule {
            if self.persisted.remove_save_rule(index) {
                rules_changed = true;
                self.toast("Auto-save rule deleted.", ToastTone::Info);
            }
        }
        if rules_changed {
            self.dirty = true;
        }

        if changed {
            self.dirty = true;
            self.downloads.configure(
                self.persisted.settings.max_concurrent,
                self.persisted.settings.download_dir.clone(),
                self.persisted.settings.auto_extract,
                self.persisted.settings.auto_retry,
                self.persisted.settings.max_retries,
            );
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|toast| toast.created.elapsed() < Duration::from_secs(5));
        if self.toasts.is_empty() {
            return;
        }
        let toasts: Vec<(String, ToastTone)> = self
            .toasts
            .iter()
            .map(|toast| (toast.text.clone(), toast.tone))
            .collect();
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, [-24.0, -92.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(360.0);
                for (text, tone) in toasts.into_iter().rev() {
                    let (dot, frame_stroke) = match tone {
                        ToastTone::Info => (accent(), accent_dim(55)),
                        ToastTone::Success => (SUCCESS, Color32::from_rgb(35, 67, 52)),
                        ToastTone::Error => (DANGER, Color32::from_rgb(76, 36, 44)),
                    };
                    Frame::none()
                        .fill(Color32::from_rgb(20, 21, 30))
                        .stroke(Stroke::new(1.0_f32, frame_stroke))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin {
                            left: 14.0,
                            right: 14.0,
                            top: 11.0,
                            bottom: 11.0,
                        })
                        .shadow(egui::epaint::Shadow {
                            offset: vec2(0.0, 6.0),
                            blur: 20.0,
                            spread: 0.0,
                            color: Color32::from_black_alpha(110),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(9.0, 9.0), Sense::hover());
                                ui.painter().circle_filled(rect.center(), 4.0, dot);
                                ui.add_space(2.0);
                                ui.label(egui::RichText::new(text).size(12.0).color(TEXT));
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn draw_preview(&mut self, ctx: &egui::Context) {
        if !self.preview_cover {
            return;
        }
        let album = match &self.detail {
            Some(detail) => detail.album.clone(),
            None => {
                self.preview_cover = false;
                return;
            }
        };
        let key = format!("c{}", album.id);
        let mut close = false;
        let screen = ctx.screen_rect();
        let max = (screen.width().min(screen.height()) - 120.0).clamp(240.0, 760.0);
        egui::Window::new("Cover art")
            .id(Id::new("cover-preview"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .title_bar(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&album.title)
                                .size(13.0)
                                .strong()
                                .color(TEXT),
                        )
                        .truncate(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, 28.0, MUTED).clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(8.0);
                if let Some(texture) = self.textures.get(&key) {
                    let size = fit_size(texture.size_vec2(), vec2(max, max));
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(texture.id(), size))
                            .rounding(Rounding::same(10.0)),
                    );
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(max * 0.7, max * 0.7), Sense::hover());
                    paint_cover_placeholder(ui.painter(), rect, &album.title);
                    ui.label(
                        egui::RichText::new("Loading cover…")
                            .size(11.0)
                            .color(MUTED),
                    );
                }
            });
        if close {
            self.preview_cover = false;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.preview_cover = false;
        }
    }

    /// Draw the command palette overlay (Ctrl+P).
    fn draw_command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette_open {
            return;
        }

        let screen = ctx.screen_rect();
        let panel_width = screen.width().min(520.0);
        let panel_height = (screen.height() * 0.65).min(560.0);

        egui::Window::new("Command palette")
            .id(Id::new("command-palette"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_size(vec2(panel_width, panel_height))
            .anchor(Align2::CENTER_CENTER, [0.0, -screen.height() * 0.08])
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .rounding(Rounding::same(12.0))
                    .inner_margin(Margin::same(16.0)),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Command palette")
                        .size(12.0)
                        .strong()
                        .color(DIM),
                );
                ui.add_space(8.0);

                // Search field
                let mut run_action = None;
                let mut cycle_down = false;
                let mut cycle_up = false;
                let field = TextEdit::singleline(&mut self.palette_query)
                    .hint_text("Open view, download, settings…")
                    .font(FontId::proportional(13.5))
                    .margin(Margin::symmetric(10.0, 8.0))
                    .id(Id::new("palette-field"));
                let response = ui.add_sized([panel_width - 32.0, 36.0], field);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    run_action = Some(self.palette_selected);
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                    cycle_down = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                    cycle_up = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.palette_open = false;
                }

                if cycle_down && !self.palette_actions.is_empty() {
                    self.palette_selected = (self.palette_selected + 1) % self.palette_actions.len();
                }
                if cycle_up && !self.palette_actions.is_empty() {
                    self.palette_selected =
                        (self.palette_selected + self.palette_actions.len() - 1)
                            % self.palette_actions.len();
                }

                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(panel_height * 0.55)
                    .show(ui, |ui| {
                        let mut matched = 0;
                        for (index, (title, subtitle, _action)) in
                            self.palette_actions.iter().enumerate()
                        {
                            let needle = self.palette_query.trim().to_lowercase();
                            let haystack = format!("{} {}", title, subtitle).to_lowercase();
                            if !needle.is_empty() && !haystack.contains(&needle) {
                                continue;
                            }
                            matched += 1;

                            let selected = index == self.palette_selected;
                            let bg = if selected {
                                Color32::from_rgba_unmultiplied(accent().r(), accent().g(), accent().b(), 20)
                            } else if response.hovered() || ui.rect_contains_pointer(ui.min_rect()) {
                                CARD_HOVER
                            } else {
                                CARD
                            };
                            let (rect, item_response) =
                                ui.allocate_exact_size(vec2(panel_width - 32.0, 44.0), Sense::click());
                            ui.painter().rect_filled(rect, Rounding::same(8.0), bg);

                            ui.painter().text(
                                pos2(rect.left() + 16.0, rect.center().y - 8.0),
                                Align2::LEFT_CENTER,
                                title,
                                FontId::proportional(13.5),
                                TEXT,
                            );
                            ui.painter().text(
                                pos2(rect.left() + 16.0, rect.center().y + 6.0),
                                Align2::LEFT_CENTER,
                                subtitle,
                                FontId::proportional(10.5),
                                MUTED,
                            );

                            if item_response.clicked() {
                                run_action = Some(index);
                            }
                        }
                        if matched == 0 {
                            ui.allocate_ui_with_layout(
                                vec2(panel_width - 32.0, 60.0),
                                Layout::top_down(Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new("No matching commands")
                                            .size(12.0)
                                            .color(DIM),
                                    );
                                },
                            );
                        }
                    });

                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Arrow keys to navigate · Enter to open · Esc to close")
                        .size(10.0)
                        .color(DIM),
                );

                if let Some(index) = run_action {
                    if let Some((_, _, action)) = self.palette_actions.get(index) {
                        self.execute_palette_action(ctx, action.clone());
                    }
                    self.palette_open = false;
                    self.palette_query.clear();
                    self.palette_selected = 0;
                }
            });

        ctx.request_repaint();
    }

    /// Execute a command palette action.
    fn execute_palette_action(&mut self, ctx: &egui::Context, action: PaletteAction) {
        match action {
            PaletteAction::Search => {
                self.view = View::Search;
                self.focus_search = true;
            }
            PaletteAction::Library => self.set_view(View::Library),
            PaletteAction::Watchlist => self.set_view(View::Watchlist),
            PaletteAction::Downloads => self.set_view(View::Downloads),
            PaletteAction::Settings => self.set_view(View::Settings),
            PaletteAction::Refresh => match self.view {
                View::Discover => self.refresh_latest(),
                View::Search => {
                    if !self.search.query.trim().is_empty() {
                        self.submit_search();
                    }
                }
                View::Watchlist => {
                    self.toast("Watchlist is already up to date.", ToastTone::Info);
                }
                View::Library => {
                    self.toast("Library reflects completed downloads.", ToastTone::Info);
                }
                View::Downloads | View::Settings => {}
            },
            PaletteAction::PauseAll => {
                self.downloads.pause_all();
                self.toast("All downloads paused.", ToastTone::Info);
            }
            PaletteAction::ResumeAll => {
                self.downloads.resume_all();
                self.toast("Downloads resumed.", ToastTone::Info);
            }
            PaletteAction::Quit => {
                self.allow_exit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Draw the keyboard shortcuts help overlay (? or Ctrl+/).
    fn draw_shortcuts_overlay(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts {
            return;
        }

        let screen = ctx.screen_rect();
        let width = screen.width().min(640.0);
        let height = (screen.height() * 0.75).min(700.0);

        egui::Window::new("Keyboard shortcuts")
            .id(Id::new("shortcuts-overlay"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_size(vec2(width, height))
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .rounding(Rounding::same(12.0))
                    .inner_margin(Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Keyboard shortcuts")
                        .size(14.0)
                        .strong()
                        .color(accent()),
                );
                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("Global")
                        .size(10.5)
                        .strong()
                        .color(DIM),
                );
                ui.add_space(4.0);
                shortcut_row(ui, "Ctrl+P", "Command palette");
                shortcut_row(ui, "Ctrl+F", "Search");
                shortcut_row(ui, "Ctrl+R", "Refresh current view");
                shortcut_row(ui, "Ctrl+D", "Download selected album(s)");
                shortcut_row(ui, "Esc", "Close detail / clear selection / lose focus");
                shortcut_row(ui, "?", "Show this shortcuts help");
                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("Navigation")
                        .size(10.5)
                        .strong()
                        .color(DIM),
                );
                ui.add_space(4.0);
                shortcut_row(ui, "1", "Discover / latest releases");
                shortcut_row(ui, "2", "Search");
                shortcut_row(ui, "3", "Watchlist");
                shortcut_row(ui, "4", "Library");
                shortcut_row(ui, "5", "Downloads");
                shortcut_row(ui, "6", "Settings");
                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("Selection (grid views)")
                        .size(10.5)
                        .strong()
                        .color(DIM),
                );
                ui.add_space(4.0);
                shortcut_row(ui, "Click", "Toggle album selection");
                shortcut_row(ui, "Shift+click", "Select range between anchor and target");
                shortcut_row(ui, "Ctrl+A", "Select all loaded albums");
                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("Detail panel")
                        .size(10.5)
                        .strong()
                        .color(DIM),
                );
                ui.add_space(4.0);
                shortcut_row(ui, "Enter", "Download album from detail panel");
                shortcut_row(ui, "Esc", "Close detail panel");

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::primary_button(ui, "Close (Esc)").clicked() {
                            self.show_shortcuts = false;
                        }
                    });
                });
            });

        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.show_shortcuts = false;
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.ensure_tray(ctx);
        self.handle_tray_actions(ctx);
        self.handle_close_request(ctx);
        self.textures.begin_frame();
        let loaded = self.images.pump(ctx, &mut self.textures, 10);
        if loaded > 0 {
            ctx.request_repaint();
        }
        self.downloads.configure(
            self.persisted.settings.max_concurrent,
            self.persisted.settings.download_dir.clone(),
            self.persisted.settings.auto_extract,
            self.persisted.settings.auto_retry,
            self.persisted.settings.max_retries,
        );
        let notices = self.downloads.poll();
        self.handle_download_notices(notices);
        
        // Update Windows taskbar progress
        #[cfg(all(windows, feature = "taskbar-progress"))]
        {
            use crate::taskbar::{set_taskbar_progress, set_taskbar_error, set_taskbar_paused, clear_taskbar};
            use crate::downloads::DownloadState;
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            
            let mut total_downloaded: u64 = 0;
            let mut total_size: u64 = 0;
            let mut has_active = false;
            let mut has_error = false;
            let mut has_paused = false;
            
            for task in self.downloads.tasks.iter() {
                match task.state {
                    DownloadState::Downloading | DownloadState::Resolving | DownloadState::Extracting | DownloadState::Retrying { .. } => {
                        has_active = true;
                        total_downloaded += task.progress.downloaded();
                        total_size += task.progress.total();
                    }
                    DownloadState::Failed => has_error = true,
                    DownloadState::Paused => has_paused = true,
                    _ => {}
                }
            }
            
            // Get HWND from the frame
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    let hwnd = h.hwnd.get() as *mut std::ffi::c_void;
                    
                    if has_error {
                        set_taskbar_error(hwnd);
                    } else if has_paused && !has_active {
                        set_taskbar_paused(hwnd);
                    } else if has_active && total_size > 0 {
                        set_taskbar_progress(hwnd, total_downloaded, total_size);
                    } else {
                        clear_taskbar(hwnd);
                    }
                }
            }
        }
        self.sync_library_from_downloads();
        self.poll_net();
        self.check_manual_downloads();
        self.handle_shortcuts(ctx);

        self.draw_sidebar(ctx);
        self.draw_detail_panel(ctx);
        self.draw_selection_bar(ctx);
        self.draw_central(ctx);
        self.draw_preview(ctx);
        self.draw_command_palette(ctx);
        self.draw_shortcuts_overlay(ctx);
        self.draw_toasts(ctx);

        self.images.pump(ctx, &mut self.textures, 4);
        self.textures.trim(self.persisted.settings.texture_budget);

        if self.dirty && self.last_state_save.elapsed() > Duration::from_millis(700) {
            store::save_state(&self.persisted);
            self.dirty = false;
            self.last_state_save = Instant::now();
        }
        if self.dl_ui.cache_checked.elapsed() > Duration::from_secs(20) {
            self.dl_ui.cache_bytes = store::cache_size();
            self.dl_ui.cache_checked = Instant::now();
        }

        // Notify when all downloads complete
        let has_active = self.downloads.running_count() > 0;
        if self.had_active_downloads && !has_active {
            if self.persisted.settings.notify_on_complete {
                self.toast("All downloads complete", ToastTone::Success);
                self.desktop_notify("All downloads complete", "All queued downloads have finished");
            }
        }
        self.had_active_downloads = has_active;

        if self.latest_loading
            || self.search.loading
            || self.detail.as_ref().map(|d| d.loading).unwrap_or(false)
            || has_active
        {
            ctx.request_repaint_after(Duration::from_millis(180));
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if self.dirty {
            store::save_state(&self.persisted);
            self.dirty = false;
        }
        self.downloads.persist();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(max_id) = self.latest.iter().map(|a| a.id).max() {
            if max_id > self.persisted.last_seen_id {
                self.persisted.last_seen_id = max_id;
            }
        }
        self.persisted.last_visit = store::now_unix();
        store::save_state(&self.persisted);
        self.downloads.persist();
    }
}

// ---------------------------------------------------------------------------
// Free UI functions
// ---------------------------------------------------------------------------

fn toggle_format(formats: &mut FormatFilter, label: &str) {
    let slot = match label {
        "FLAC" => &mut formats.flac,
        "MP3" => &mut formats.mp3,
        "TTA" => &mut formats.tta,
        "Opus" => &mut formats.opus,
        "OGG" => &mut formats.ogg,
        _ => return,
    };
    *slot = !*slot;
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn format_number(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn format_relative_time(timestamp: u64) -> String {
    if timestamp == 0 {
        return String::new();
    }
    let now = store::now_unix();
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        86_400..=2_591_999 => format!("{}d ago", seconds / 86_400),
        _ => format!("{}mo ago", seconds / 2_592_000),
    }
}

fn draw_search_field(ui: &mut Ui, query: &mut String, width: f32) -> Response {
    Frame::none()
        .fill(BG)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(Rounding::same(9.0))
        .inner_margin(Margin {
            left: 11.0,
            right: 8.0,
            top: 5.0,
            bottom: 5.0,
        })
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(17.0, 17.0), Sense::hover());
                theme::paint_icon(ui.painter(), rect, Icon::Search, DIM, 1.5);
                let response = ui.add_sized(
                    [width, 26.0],
                    TextEdit::singleline(query)
                        .id(search_field_id())
                        .frame(false)
                        .hint_text("Search albums, artists, tags, providers…")
                        .font(FontId::proportional(13.5))
                        .margin(Margin::symmetric(4.0, 5.0)),
                );
                response
            })
            .inner
        })
        .inner
}

fn view_header(
    ui: &mut Ui,
    eyebrow: &str,
    title: &str,
    subtitle: &str,
    actions: impl FnOnce(&mut Ui),
) {
    if ui.available_width() < 560.0 {
        view_heading(ui, eyebrow, title, subtitle);
        ui.add_space(10.0);
        ui.horizontal_wrapped(actions);
    } else {
        ui.horizontal(|ui| {
            view_heading(ui, eyebrow, title, subtitle);
            ui.with_layout(Layout::right_to_left(Align::Center), actions);
        });
    }
}

fn view_heading(ui: &mut Ui, eyebrow: &str, title: &str, subtitle: &str) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(eyebrow)
                .size(10.5)
                .strong()
                .color(accent()),
        );
        ui.add_space(3.0);
        ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .font(FontId::new(27.0, FontFamily::Proportional))
                        .strong()
                        .color(TEXT),
                )
                .wrap(),
            );
        if !subtitle.is_empty() {
            ui.label(egui::RichText::new(subtitle).size(12.0).color(MUTED));
        }
    });
}

fn draw_loading(ui: &mut Ui, text: &str) {
    let height = ui.available_height().max(260.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        Layout::top_down(Align::Center),
        |ui| {
            ui.add_space((height * 0.24).min(150.0));
            ui.add(egui::Spinner::new().size(24.0).color(accent()));
            ui.add_space(12.0);
            ui.label(egui::RichText::new(text).size(12.5).color(MUTED));
        },
    );
}

fn draw_empty(
    ui: &mut Ui,
    icon: Icon,
    title: &str,
    body: &str,
    action: Option<&str>,
    clicked: &mut bool,
) {
    let height = ui.available_height().max(280.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        Layout::top_down(Align::Center),
        |ui| {
            ui.add_space((height * 0.18).min(120.0));
            let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 32.0, accent_dim(18));
            theme::paint_icon(
                ui.painter(),
                Rect::from_center_size(rect.center(), vec2(28.0, 28.0)),
                icon,
                accent(),
                1.7,
            );
            ui.add_space(14.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .font(FontId::new(19.0, FontFamily::Proportional))
                        .strong()
                        .color(TEXT),
                )
                .wrap()
                .halign(Align::Center),
            );
            ui.add_space(5.0);
            ui.add(
                egui::Label::new(egui::RichText::new(body).size(12.0).color(MUTED))
                    .wrap()
                    .halign(Align::Center),
            );
            if let Some(label) = action {
                ui.add_space(14.0);
                if theme::primary_button(ui, label).clicked() {
                    *clicked = true;
                }
            }
        },
    );
}

fn nav_row(
    ui: &mut Ui,
    selected: bool,
    icon: Icon,
    label: &str,
    badge: Option<usize>,
    compact: bool,
) -> bool {
    let width = ui.available_width();
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
    let hovered = response.hovered() || ui.rect_contains_pointer(rect);
    if selected {
        ui.painter()
            .rect_filled(rect, Rounding::same(9.0), accent_dim(28));
        let bar = Rect::from_min_size(pos2(rect.left(), rect.center().y - 9.0), vec2(3.0, 18.0));
        ui.painter().rect_filled(bar, Rounding::same(2.0), accent());
    } else if hovered {
        ui.painter().rect_filled(rect, Rounding::same(9.0), CARD);
    }
    let color = if selected {
        accent()
    } else if hovered {
        TEXT
    } else {
        MUTED
    };
    let icon_center = if compact {
        rect.center()
    } else {
        pos2(rect.left() + 23.0, rect.center().y)
    };
    theme::paint_icon(
        ui.painter(),
        Rect::from_center_size(icon_center, vec2(17.0, 17.0)),
        icon,
        color,
        1.5,
    );
    if compact {
        if let Some(count) = badge {
            if count > 0 {
                ui.painter().circle_filled(
                    pos2(rect.right() - 9.0, rect.top() + 9.0),
                    3.5,
                    if selected { accent() } else { MUTED },
                );
            }
        }
        response = response.on_hover_text(label);
    } else {
        ui.painter().text(
            pos2(rect.left() + 40.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            color,
        );
        if let Some(count) = badge {
            if count > 0 {
                let text = if count > 99 {
                    "99+".to_string()
                } else {
                    count.to_string()
                };
                let galley = ui.painter().layout_no_wrap(
                    text,
                    FontId::proportional(10.0),
                    if selected {
                        Color32::from_rgb(14, 15, 21)
                    } else {
                        MUTED
                    },
                );
                let size = vec2(galley.size().x + 12.0, 18.0);
                let badge_rect = Rect::from_min_size(
                    pos2(rect.right() - size.x - 10.0, rect.center().y - size.y * 0.5),
                    size,
                );
                ui.painter().rect_filled(
                    badge_rect,
                    Rounding::same(5.0),
                    if selected { accent() } else { CARD_HOVER },
                );
                ui.painter().galley(
                    badge_rect.center() - galley.size() * 0.5,
                    galley,
                    if selected {
                        Color32::from_rgb(14, 15, 21)
                    } else {
                        MUTED
                    },
                );
            }
        }
    }
    response.clicked()
}

fn app_logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(10.0), accent());
    let bars = [(0.30, 0.58), (0.46, 0.76), (0.62, 0.48), (0.78, 0.66)];
    for (x, h) in bars {
        let bar = Rect::from_center_size(
            pos2(rect.left() + rect.width() * x, rect.center().y),
            vec2(3.0, rect.height() * h),
        );
        ui.painter()
            .rect_filled(bar, Rounding::same(1.5), Color32::from_rgb(17, 18, 25));
    }
}

struct SidePanelLeft;

impl SidePanelLeft {
    fn show(
        ctx: &egui::Context,
        app: &mut App,
        navigate: &mut Option<View>,
        cache_text: &str,
        active: usize,
    ) {
        let compact = ctx.screen_rect().width() < 960.0;
        let width = if compact { 72.0 } else { 218.0 };
        let margin = if compact { 10.0 } else { 14.0 };
        egui::SidePanel::left("sidebar")
            .exact_width(width)
            .resizable(false)
            .frame(Frame::none().fill(SIDEBAR).inner_margin(Margin {
                left: margin,
                right: margin,
                top: if compact { 18.0 } else { 22.0 },
                bottom: 18.0,
            }))
            .show(ctx, |ui| {
                if compact {
                    ui.vertical_centered(|ui| app_logo(ui, 36.0));
                } else {
                    ui.horizontal(|ui| {
                        app_logo(ui, 36.0);
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.add_space(1.0);
                            ui.label(
                                egui::RichText::new("DOUJIN")
                                    .size(15.0)
                                    .strong()
                                    .color(TEXT),
                            );
                            ui.label(
                                egui::RichText::new("EXPLORER")
                                    .size(9.0)
                                    .strong()
                                    .color(accent()),
                            );
                        });
                    });
                }
                ui.add_space(if compact { 22.0 } else { 28.0 });
                if !compact {
                    ui.label(egui::RichText::new("BROWSE").size(9.5).strong().color(DIM));
                    ui.add_space(7.0);
                }
                let items = [
                    (View::Discover, Icon::Compass, "Discover", None),
                    (View::Search, Icon::Search, "Search", None),
                    (
                        View::Watchlist,
                        Icon::Star,
                        "Watchlist",
                        Some(app.persisted.watchlist.len()),
                    ),
                    (
                        View::Library,
                        Icon::Folder,
                        "Library",
                        Some(app.persisted.library.len()),
                    ),
                    (View::Downloads, Icon::Download, "Downloads", Some(active)),
                ];
                for (view, icon, label, badge) in items {
                    if nav_row(ui, app.view == view, icon, label, badge, compact) {
                        *navigate = Some(view);
                    }
                    ui.add_space(2.0);
                }

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if compact {
                        let cache = theme::icon_button(ui, Icon::Grid, 34.0, DIM);
                        cache.on_hover_text(format!("{cache_text} image cache"));
                        ui.add_space(8.0);
                        if nav_row(
                            ui,
                            app.view == View::Settings,
                            Icon::Settings,
                            "Settings",
                            None,
                            true,
                        ) {
                            *navigate = Some(View::Settings);
                        }
                        return;
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "v{} · unofficial desktop client",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .size(9.5)
                        .color(DIM),
                    );
                    ui.add_space(8.0);
                    if nav_row(
                        ui,
                        app.view == View::Settings,
                        Icon::Settings,
                        "Settings",
                        None,
                        false,
                    ) {
                        *navigate = Some(View::Settings);
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                        theme::paint_icon(ui.painter(), rect, Icon::Grid, DIM, 1.2);
                        ui.label(
                            egui::RichText::new(format!("{cache_text} image cache"))
                                .size(10.0)
                                .color(DIM),
                        );
                    });
                    if active > 0 {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                            ui.painter().circle_filled(rect.center(), 3.0, accent());
                            ui.label(
                                egui::RichText::new(format!("{active} downloads active"))
                                    .size(10.0)
                                    .color(MUTED),
                            );
                        });
                    }
                });
            });
    }
}

#[derive(Clone, Copy)]
enum GridFooter<'a> {
    None,
    Loading(&'a str),
    LoadMore(&'a str),
    End(&'a str),
}

struct CardLayout {
    columns: usize,
    card_width: f32,
    card_height: f32,
}

fn draw_grid(
    ui: &mut Ui,
    albums: &[Album],
    textures: &mut Textures,
    selected: &HashSet<u64>,
    watchlist: &[Album],
    watch_meta: &HashMap<u64, WatchMeta>,
    folders: &[String],
    library: &[LibraryEntry],
    requests: &mut Vec<ImageSpec>,
    show_new: bool,
    footer: GridFooter<'_>,
    id_salt: &str,
) -> (Vec<CardAction>, bool) {
    let avail = (ui.available_width() - 16.0).max(140.0);
    let min_card = if avail < 560.0 { 140.0 } else { MIN_CARD_WIDTH };
    let columns = (((avail + CARD_GAP) / (min_card + CARD_GAP)).floor() as usize).max(1);
    let card_width = ((avail - CARD_GAP * (columns.saturating_sub(1)) as f32) / columns as f32)
        .floor()
        .max(120.0);
    let card_height = card_width + 51.0;
    let layout = CardLayout {
        columns,
        card_width,
        card_height,
    };
    let card_rows = albums.len().div_ceil(columns);
    let has_footer = !matches!(footer, GridFooter::None);
    let total_rows = card_rows + usize::from(has_footer);
    let mut actions: Vec<CardAction> = Vec::new();
    let mut clickable_footer = false;

    let output = ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
        .show_rows(ui, card_height, total_rows, |ui, row_range| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for row in row_range {
                if row >= card_rows {
                    ui.add_space(22.0);
                    ui.vertical_centered(|ui| match footer {
                        GridFooter::Loading(text) => {
                            ui.add(egui::Spinner::new().size(18.0).color(accent()));
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(text).size(11.0).color(MUTED));
                        }
                        GridFooter::LoadMore(text) => {
                            if theme::secondary_button(ui, text).clicked() {
                                clickable_footer = true;
                            }
                        }
                        GridFooter::End(text) => {
                            ui.label(egui::RichText::new(text).size(11.0).color(DIM));
                        }
                        GridFooter::None => {}
                    });
                    continue;
                }
                ui.horizontal(|ui| {
                    for column in 0..layout.columns {
                        let index = row * layout.columns + column;
                        if index >= albums.len() {
                            break;
                        }
                        let album = &albums[index];
                        let watched = watchlist.iter().any(|a| a.id == album.id);
                        let downloaded = library.iter().any(|e| e.album.id == album.id);
                        let current_folder = watch_meta
                            .get(&album.id)
                            .and_then(|meta| meta.folder.as_deref());
                        if let Some(action) = draw_card(
                            ui,
                            album,
                            &layout,
                            textures,
                            selected.contains(&album.id),
                            watched,
                            downloaded,
                            current_folder,
                            folders,
                            requests,
                            show_new,
                        ) {
                            actions.push(action);
                        }
                    }
                });
            }
        });

    if clickable_footer {
        actions.push(CardAction::LoadMore);
    }
    let near_bottom =
        output.state.offset.y + output.inner_rect.height() + 520.0 >= output.content_size.y;
    (actions, near_bottom)
}

#[allow(clippy::too_many_arguments)]
fn draw_card(
    ui: &mut Ui,
    album: &Album,
    layout: &CardLayout,
    textures: &mut Textures,
    selected: bool,
    watched: bool,
    downloaded: bool,
    current_folder: Option<&str>,
    folders: &[String],
    requests: &mut Vec<ImageSpec>,
    show_new: bool,
) -> Option<CardAction> {
    let (rect, response) =
        ui.allocate_exact_size(vec2(layout.card_width, layout.card_height), Sense::click());
    // The action buttons only exist while the card is hovered. Using the response alone
    // makes them vanish as soon as the pointer is over a button, which causes flicker.
    let hovered = response.hovered() || ui.rect_contains_pointer(rect);
    let painter = ui.painter().clone();
    let bg = if selected {
        Color32::from_rgb(25, 24, 38)
    } else if hovered {
        CARD_HOVER
    } else {
        CARD
    };
    painter.rect_filled(rect, Rounding::same(12.0), bg);
    if selected {
        painter.rect_stroke(rect, Rounding::same(12.0), Stroke::new(1.0_f32, accent()));
    } else if hovered {
        painter.rect_stroke(rect, Rounding::same(12.0), Stroke::new(1.0_f32, BORDER));
    }

    let cover = Rect::from_min_size(
        rect.min + vec2(8.0, 8.0),
        vec2(rect.width() - 16.0, rect.width() - 16.0),
    );
    let key = format!("t{}", album.id);
    let mut action = None;

    if let Some(texture) = textures.get(&key) {
        paint_texture_cover(&painter, cover, texture, Rounding::same(9.0));
    } else {
        paint_cover_placeholder(&painter, cover, &album.title);
        requests.push(ImageSpec {
            key: key.clone(),
            url: album.thumb_url(),
            cache_path: store::thumb_cache_path(album.id),
            max_dim: 420,
        });
    }

    // Subtle hover shade over the image.
    if hovered {
        painter.rect_filled(
            cover,
            Rounding::same(9.0),
            Color32::from_rgba_unmultiplied(8, 8, 12, 36),
        );
    }

    // Selection control (always visible when selected, subtle on hover).
    let check_size = 22.0;
    let check_rect = Rect::from_min_size(cover.min + vec2(8.0, 8.0), vec2(check_size, check_size));
    let check_response = ui.interact(
        check_rect,
        ui.id().with(("select", album.id)),
        Sense::click(),
    );
    if selected || check_response.hovered() {
        painter.rect_filled(
            check_rect,
            Rounding::same(6.0),
            if selected {
                accent()
            } else {
                Color32::from_black_alpha(120)
            },
        );
        painter.rect_stroke(
            check_rect,
            Rounding::same(6.0),
            Stroke::new(
                1.0_f32,
                if selected {
                    accent()
                } else {
                    Color32::from_white_alpha(70)
                },
            ),
        );
        if selected {
            theme::paint_icon(
                &painter,
                Rect::from_center_size(check_rect.center(), vec2(13.0, 13.0)),
                Icon::Check,
                Color32::from_rgb(14, 15, 21),
                2.0,
            );
        }
    }
    if check_response.clicked() {
        let shift = ui.input(|input| input.modifiers.shift);
        action = Some(CardAction::ToggleSelect {
            id: album.id,
            shift,
        });
    }

    // Hover actions.
    if hovered {
        let download_rect =
            Rect::from_min_size(cover.right_top() + vec2(-38.0, 8.0), vec2(30.0, 30.0));
        let download_response = ui.interact(
            download_rect,
            ui.id().with(("download", album.id)),
            Sense::click(),
        );
        painter.rect_filled(
            download_rect,
            Rounding::same(8.0),
            if download_response.hovered() {
                accent()
            } else {
                Color32::from_black_alpha(145)
            },
        );
        theme::paint_icon(
            &painter,
            Rect::from_center_size(download_rect.center(), vec2(14.0, 14.0)),
            Icon::Download,
            if download_response.hovered() {
                Color32::from_rgb(14, 15, 21)
            } else {
                TEXT
            },
            1.6,
        );
        if download_response.clicked() {
            action = Some(CardAction::Download(album.clone()));
        }

        let watch_rect =
            Rect::from_min_size(cover.right_top() + vec2(-74.0, 8.0), vec2(30.0, 30.0));
        let watch_response = ui.interact(
            watch_rect,
            ui.id().with(("watch", album.id)),
            Sense::click(),
        );
        painter.rect_filled(
            watch_rect,
            Rounding::same(8.0),
            if watch_response.hovered() {
                CARD_HOVER
            } else {
                Color32::from_black_alpha(145)
            },
        );
        theme::paint_icon(
            &painter,
            Rect::from_center_size(watch_rect.center(), vec2(14.0, 14.0)),
            if watched {
                Icon::StarFilled
            } else {
                Icon::Star
            },
            if watched { accent() } else { TEXT },
            1.5,
        );
        if watch_response.clicked() {
            action = Some(CardAction::Watch(album.clone()));
        }
    } else if watched {
        let star_rect = Rect::from_min_size(cover.right_top() + vec2(-28.0, 8.0), vec2(20.0, 20.0));
        painter.rect_filled(
            star_rect,
            Rounding::same(6.0),
            Color32::from_black_alpha(130),
        );
        theme::paint_icon(
            &painter,
            Rect::from_center_size(star_rect.center(), vec2(12.0, 12.0)),
            Icon::StarFilled,
            accent(),
            1.2,
        );
    }

    if downloaded {
        let owned_rect =
            Rect::from_min_size(cover.left_bottom() + vec2(8.0, -56.0), vec2(20.0, 20.0));
        painter.rect_filled(owned_rect, Rounding::same(6.0), SUCCESS);
        theme::paint_icon(
            &painter,
            Rect::from_center_size(owned_rect.center(), vec2(12.0, 12.0)),
            Icon::Check,
            Color32::from_rgb(14, 15, 21),
            2.0,
        );
    }

    if show_new && album.is_new {
        let galley = painter.layout_no_wrap(
            "NEW".to_string(),
            FontId::proportional(9.0),
            Color32::from_rgb(14, 15, 21),
        );
        let badge_rect = Rect::from_min_size(
            cover.left_bottom() + vec2(8.0, -30.0),
            vec2(galley.size().x + 14.0, 20.0),
        );
        painter.rect_filled(badge_rect, Rounding::same(5.0), accent());
        painter.galley(
            pos2(
                badge_rect.left() + 7.0,
                badge_rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            Color32::from_rgb(14, 15, 21),
        );
    }

    if !album.format.is_empty() {
        let galley = painter.layout_no_wrap(
            album.format.clone(),
            FontId::proportional(9.5),
            Color32::from_rgb(235, 235, 245),
        );
        let badge_rect = Rect::from_min_size(
            pos2(
                cover.right() - galley.size().x - 24.0,
                cover.bottom() - 28.0,
            ),
            vec2(galley.size().x + 16.0, 19.0),
        );
        painter.rect_filled(
            badge_rect,
            Rounding::same(5.0),
            Color32::from_black_alpha(160),
        );
        painter.galley(
            pos2(
                badge_rect.left() + 8.0,
                badge_rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            Color32::from_rgb(235, 235, 245),
        );
    }

    let text_left = rect.left() + 10.0;
    let text_width = rect.width() - 20.0;
    let title_y = cover.bottom() + 9.0;
    let title_galley = painter.layout(
        album.title.clone(),
        FontId::new(12.8, FontFamily::Proportional),
        TEXT,
        text_width,
    );
    let title_clip = Rect::from_min_size(pos2(text_left, title_y), vec2(text_width, 34.0));
    painter
        .with_clip_rect(title_clip)
        .galley(pos2(text_left, title_y), title_galley, TEXT);
    let artist = if album.artist.trim().is_empty() {
        "Unknown artist"
    } else {
        album.artist.trim()
    };
    let artist_text = elide_text(&painter, artist, 10.8, text_width);
    painter.text(
        pos2(text_left, cover.bottom() + 43.0),
        Align2::LEFT_TOP,
        artist_text,
        FontId::proportional(10.8),
        MUTED,
    );

    response.context_menu(|ui| {
        if ui.button("Open details").clicked() {
            action = Some(CardAction::Open(album.clone()));
            ui.close_menu();
        }
        if ui.button("Download album").clicked() {
            action = Some(CardAction::Download(album.clone()));
            ui.close_menu();
        }
        ui.separator();
        if !album.tags.is_empty() {
            ui.menu_button("Download by tag", |ui| {
                for tag in &album.tags {
                    if ui.button(tag).clicked() {
                        action = Some(CardAction::OpenUrl(format!("{}/?p=search&type=tags&result={}", crate::models::SITE, percent_encoding::percent_encode(tag.as_bytes(), percent_encoding::NON_ALPHANUMERIC))));
                        ui.close_menu();
                    }
                }
            });
        }
        if !album.artist.trim().is_empty() {
            ui.menu_button("Download by artist", |ui| {
                if ui.button(album.artist.trim()).clicked() {
                    action = Some(CardAction::OpenUrl(format!("{}/?p=search&type=artist&result={}", crate::models::SITE, percent_encoding::percent_encode(album.artist.trim().as_bytes(), percent_encoding::NON_ALPHANUMERIC))));
                    ui.close_menu();
                }
            });
        }
        ui.separator();
        if ui
            .button(if watched {
                "Remove from watchlist"
            } else {
                "Add to watchlist"
            })
            .clicked()
        {
            action = Some(CardAction::Watch(album.clone()));
            ui.close_menu();
        }
        if watched {
            if ui.button("Edit watchlist note…").clicked() {
                action = Some(CardAction::Open(album.clone()));
                ui.close_menu();
            }
            ui.menu_button("Watchlist folder", |ui| {
                if ui
                    .selectable_label(current_folder.is_none(), "Uncategorized")
                    .clicked()
                {
                    action = Some(CardAction::SetFolder {
                        album: album.clone(),
                        folder: None,
                    });
                    ui.close_menu();
                }
                for folder in folders {
                    if ui
                        .selectable_label(current_folder == Some(folder.as_str()), folder)
                        .clicked()
                    {
                        action = Some(CardAction::SetFolder {
                            album: album.clone(),
                            folder: Some(folder.clone()),
                        });
                        ui.close_menu();
                    }
                }
            });
        }
        ui.separator();
        if ui.button("Copy title").clicked() {
            ui.ctx().copy_text(album.title.clone());
            ui.close_menu();
        }
        if ui.button("Open album page in browser").clicked() {
            action = Some(CardAction::OpenUrl(album.page_url()));
            ui.close_menu();
        }
    });

    if action.is_none() && response.clicked() {
        action = Some(CardAction::Open(album.clone()));
    }
    action
}

fn elide_text(painter: &egui::Painter, text: &str, size: f32, max_width: f32) -> String {
    let full = painter.layout_no_wrap(text.to_string(), FontId::proportional(size), MUTED);
    if full.size().x <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        let width = painter
            .layout_no_wrap(candidate.clone(), FontId::proportional(size), MUTED)
            .size()
            .x;
        if width <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    chars[..low].iter().collect::<String>() + "…"
}

fn paint_texture_cover(
    painter: &egui::Painter,
    rect: Rect,
    texture: &egui::TextureHandle,
    rounding: Rounding,
) {
    let tex_size = texture.size_vec2();
    if tex_size.x <= 0.0 || tex_size.y <= 0.0 {
        return;
    }
    let tex_aspect = tex_size.x / tex_size.y;
    let rect_aspect = rect.width() / rect.height().max(1.0);
    let uv = if tex_aspect > rect_aspect {
        let visible = rect_aspect / tex_aspect;
        Rect::from_min_max(
            pos2((1.0 - visible) * 0.5, 0.0),
            pos2((1.0 + visible) * 0.5, 1.0),
        )
    } else {
        let visible = tex_aspect / rect_aspect;
        Rect::from_min_max(
            pos2(0.0, (1.0 - visible) * 0.5),
            pos2(1.0, (1.0 + visible) * 0.5),
        )
    };
    let mut shape = egui::epaint::RectShape::filled(rect, rounding, Color32::WHITE);
    shape.fill_texture_id = texture.id();
    shape.uv = uv;
    painter.add(shape);
}

fn paint_cover_placeholder(painter: &egui::Painter, rect: Rect, title: &str) {
    painter.rect_filled(rect, Rounding::same(9.0), Color32::from_rgb(25, 26, 37));
    let stripe = Rect::from_min_size(rect.left_top(), vec2(rect.width(), 2.0));
    painter.rect_filled(stripe, Rounding::same(1.0), accent_dim(80));
    let initial = title
        .chars()
        .find(|c| !c.is_whitespace())
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".to_string());
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::new(
            (rect.width() * 0.28).clamp(22.0, 54.0),
            FontFamily::Proportional,
        ),
        accent_dim(150),
    );
}

fn fit_size(source: Vec2, bounds: Vec2) -> Vec2 {
    if source.x <= 0.0 || source.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.x / source.x).min(bounds.y / source.y);
    source * scale
}

fn task_row(
    ui: &mut Ui,
    task: &crate::downloads::DownloadTask,
    speed: f32,
    actions: &mut Vec<TaskAction>,
) {
    let frame_fill = if matches!(task.state, DownloadState::Failed) {
        Color32::from_rgb(27, 20, 25)
    } else if matches!(task.state, DownloadState::Manual) {
        Color32::from_rgb(28, 25, 20)
    } else {
        CARD
    };
    Frame::none()
        .fill(frame_fill)
        .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin {
            left: 16.0,
            right: 16.0,
            top: 13.0,
            bottom: 12.0,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let title_response = ui.add(
                        egui::Label::new(
                            egui::RichText::new(&task.album.title)
                                .size(13.4)
                                .strong()
                                .color(TEXT),
                        )
                        .truncate()
                        .sense(Sense::click()),
                    );
                    if title_response.clicked() {
                        actions.push(TaskAction::OpenAlbum(task.album.clone()));
                    }
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(if task.album.artist.trim().is_empty() {
                                "Unknown artist"
                            } else {
                                task.album.artist.trim()
                            })
                            .size(11.0)
                            .color(MUTED),
                        )
                        .truncate(),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let state_color = match task.state {
                        DownloadState::Done => SUCCESS,
                        DownloadState::Failed => DANGER,
                        DownloadState::Manual => WARNING,
                        DownloadState::Paused => WARNING,
                        DownloadState::Cancelled => MUTED,
                        _ => accent(),
                    };
                    ui.label(
                        egui::RichText::new(task.state.label())
                            .size(10.5)
                            .strong()
                            .color(state_color),
                    );
                    ui.add_space(6.0);
                    if task.state.is_terminal() {
                        if theme::icon_button(ui, Icon::Trash, 26.0, DIM)
                            .on_hover_text("Remove from list")
                            .clicked()
                        {
                            actions.push(TaskAction::Remove(task.id));
                        }
                    }
                });
            });

            if task.state.is_active() || task.state == DownloadState::Paused {
                ui.add_space(9.0);
                theme::progress_bar(ui, task.fraction(), ui.available_width(), 6.0);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let downloaded = task.progress.downloaded();
                    let total = task.progress.total();
                    let text = if total > 0 {
                        format!(
                            "{} / {}",
                            store::human_size(downloaded),
                            store::human_size(total)
                        )
                    } else {
                        store::human_size(downloaded)
                    };
                    ui.label(egui::RichText::new(text).size(10.5).color(MUTED));
                    if task.state == DownloadState::Downloading && speed > 0.5 {
                        ui.label(
                            egui::RichText::new(format!("· {}/s", store::human_size(speed as u64)))
                                .size(10.5)
                                .color(DIM),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if task.state == DownloadState::Paused {
                            if theme::small_button(ui, "Resume", TEXT).clicked() {
                                actions.push(TaskAction::Resume(task.id));
                            }
                        } else if theme::small_button(ui, "Pause", MUTED).clicked() {
                            actions.push(TaskAction::Pause(task.id));
                        }
                        if theme::small_button(ui, "Cancel", MUTED).clicked() {
                            actions.push(TaskAction::Cancel(task.id));
                        }
                        if task.state == DownloadState::Downloading {
                            let eta = if task.progress.total() > 0 && speed > 1.0 {
                                let remaining = task
                                    .progress
                                    .total()
                                    .saturating_sub(task.progress.downloaded())
                                    as f32
                                    / speed;
                                Some(remaining)
                            } else {
                                None
                            };
                            if let Some(seconds) = eta {
                                if seconds < 360_000.0 {
                                    ui.label(
                                        egui::RichText::new(format_eta(seconds))
                                            .size(10.0)
                                            .color(DIM),
                                    );
                                }
                            }
                        }
                    });
                });
            } else {
                ui.add_space(7.0);
                if task.state == DownloadState::Done {
                    let file_name = task
                        .output
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Download complete".into());
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(file_name).size(10.8).color(MUTED));
                        if let Some(folder) = &task.extracted {
                            ui.label(egui::RichText::new("· extracted").size(10.0).color(SUCCESS));
                            let _ = folder;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::small_button(ui, "Show in folder", TEXT).clicked() {
                                actions.push(TaskAction::OpenFolder(task.id));
                            }
                            if task.extracted.as_ref().map(|p| p.exists()).unwrap_or(false) || task.output.as_ref().map(|p| p.exists()).unwrap_or(false) {
                                if theme::small_button(ui, "Play in foobar", TEXT).clicked() {
                                    let path = task.extracted.clone().unwrap_or_else(|| task.output.clone().unwrap());
                                    actions.push(TaskAction::PlayInFoobar(path));
                                }
                            }
                        });
                    });
                } else {
                    let message = task
                        .error
                        .clone()
                        .unwrap_or_else(|| "This download stopped.".into());
                    ui.label(egui::RichText::new(message).size(10.8).color(
                        if task.state == DownloadState::Failed {
                            DANGER
                        } else {
                            MUTED
                        },
                    ));
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        if task.state == DownloadState::Manual {
                            if let Some(url) = &task.manual_url {
                                if theme::small_button(ui, "Open host page", WARNING).clicked() {
                                    actions.push(TaskAction::OpenUrl(url.clone()));
                                }
                            }
                        }
                        if theme::small_button(ui, "Retry", TEXT).clicked() {
                            actions.push(TaskAction::Retry(task.id));
                        }
                        if task.state == DownloadState::Manual {
                            if let Some(url) = &task.manual_url {
                                if theme::small_button(ui, "Copy link", MUTED).clicked() {
                                    ui.ctx().copy_text(url.clone());
                                }
                            }
                        }
                    });
                }
                if let Some(warning) = &task.warning {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(warning).size(10.2).color(WARNING));
                }
            }
        });
}

fn format_eta(seconds: f32) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 60.0 {
        format!("{}s left", seconds.round() as u64)
    } else if seconds < 3600.0 {
        format!("{}m left", (seconds / 60.0).round() as u64)
    } else {
        format!("{}h left", (seconds / 3600.0).round() as u64)
    }
}

fn settings_card(ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui)) {
    ui.label(egui::RichText::new(title).size(12.5).strong().color(TEXT));
    ui.add_space(6.0);
    Frame::none()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::same(16.0))
        .show(ui, contents);
}

fn draw_detail(
    ui: &mut Ui,
    detail: &mut DetailState,
    textures: &mut Textures,
    watchlist: &[Album],
    in_library: bool,
    folders: &[String],
    watch_meta: Option<&mut WatchMeta>,
    requests: &mut Vec<ImageSpec>,
) -> Vec<DetailAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("ALBUM")
                .size(9.5)
                .strong()
                .color(accent()),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::icon_button(ui, Icon::Close, 30.0, MUTED)
                .on_hover_text("Close (Esc)")
                .clicked()
            {
                actions.push(DetailAction::Close);
            }
        });
    });
    ui.add_space(5.0);
    ui.separator();
    ui.add_space(8.0);

    let album = detail
        .loaded
        .as_ref()
        .map(|loaded| loaded.album.clone())
        .unwrap_or_else(|| detail.album.clone());
    let watched = watchlist.iter().any(|a| a.id == album.id);
    let cover_key = format!("c{}", album.id);

    ScrollArea::vertical()
        .id_salt("detail-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Keep the cover at a sane size and centered, even if the panel is wide.
            let available = ui.available_width();
            let cover_side = available.min(340.0);
            let (row_rect, _) = ui.allocate_exact_size(vec2(available, cover_side), Sense::hover());
            let cover_rect =
                Rect::from_center_size(row_rect.center(), vec2(cover_side, cover_side));
            let cover_response = ui.interact(
                cover_rect,
                ui.id().with(("detail-cover", album.id)),
                Sense::click(),
            );
            if let Some(texture) = textures.get(&cover_key) {
                paint_texture_cover(ui.painter(), cover_rect, texture, Rounding::same(12.0));
            } else {
                paint_cover_placeholder(ui.painter(), cover_rect, &album.title);
                requests.push(ImageSpec {
                    key: cover_key.clone(),
                    url: album.cover_url(),
                    cache_path: store::cover_cache_path(album.id),
                    max_dim: 900,
                });
            }
            if cover_response.hovered() {
                ui.painter().rect_stroke(
                    cover_rect,
                    Rounding::same(12.0),
                    Stroke::new(1.0_f32, accent_dim(90)),
                );
            }
            if cover_response.clicked() {
                actions.push(DetailAction::Preview);
            }
            ui.add_space(14.0);

            ui.add(
                egui::Label::new(
                    egui::RichText::new(&album.title)
                        .font(FontId::new(21.0, FontFamily::Proportional))
                        .strong()
                        .color(TEXT),
                )
                .wrap(),
            );
            ui.add_space(3.0);
            if !album.artist.trim().is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(album.artist.trim())
                            .size(12.2)
                            .color(MUTED),
                    )
                    .wrap(),
                );
            }
            ui.add_space(9.0);
            ui.horizontal_wrapped(|ui| {
                if !album.format.is_empty() {
                    theme::badge(ui, &album.format, accent(), accent_dim(30));
                }
                if let Some(loaded) = &detail.loaded {
                    if !loaded.date_added.is_empty() {
                        theme::badge(ui, &loaded.date_added, MUTED, CARD);
                    }
                }
                if album.is_new {
                    theme::badge(ui, "NEW", Color32::from_rgb(14, 15, 21), accent());
                }
                if in_library {
                    theme::badge(ui, "IN LIBRARY", SUCCESS, Color32::from_rgb(20, 38, 30));
                }
            });
            ui.add_space(13.0);

            ui.horizontal(|ui| {
                let button_width = (ui.available_width() - 78.0).max(140.0);
                if theme::primary_button_w(
                    ui,
                    if in_library {
                        "Download again"
                    } else {
                        "Download album"
                    },
                    button_width,
                )
                .clicked()
                {
                    actions.push(DetailAction::Download(album.clone()));
                }
                if theme::icon_button(
                    ui,
                    if watched {
                        Icon::StarFilled
                    } else {
                        Icon::Star
                    },
                    36.0,
                    if watched { accent() } else { MUTED },
                )
                .on_hover_text(if watched {
                    "Remove from watchlist"
                } else {
                    "Add to watchlist"
                })
                .clicked()
                {
                    actions.push(DetailAction::ToggleWatch(album.clone()));
                }
                if theme::icon_button(ui, Icon::External, 36.0, MUTED)
                    .on_hover_text("Open album page in browser")
                    .clicked()
                {
                    actions.push(DetailAction::OpenUrl(album.page_url()));
                }
            });
            // Download by tag/artist
            if !album.tags.is_empty() || !album.artist.trim().is_empty() {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if !album.tags.is_empty() {
                        ui.menu_button("Download by tag", |ui| {
                            for tag in &album.tags {
                                if ui.button(tag).clicked() {
                                    actions.push(DetailAction::Search { text: tag.clone(), kind: crate::models::SearchKind::Tags });
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if !album.artist.trim().is_empty() {
                        ui.menu_button("Download by artist", |ui| {
                            if ui.button(album.artist.trim()).clicked() {
                                actions.push(DetailAction::Search { text: album.artist.trim().to_string(), kind: crate::models::SearchKind::Artist });
                                ui.close_menu();
                            }
                        });
                    }
                });
            }

            if let Some(meta) = watch_meta {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(9.0);
                ui.label(
                    egui::RichText::new("WATCHLIST")
                        .size(9.5)
                        .strong()
                        .color(DIM),
                );
                ui.add_space(3.0);
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Folder").size(11.0).color(MUTED));
                    let selected = meta
                        .folder
                        .clone()
                        .unwrap_or_else(|| "Uncategorized".to_string());
                    egui::ComboBox::from_id_salt("watch-folder-combo")
                        .selected_text(selected)
                        .width((ui.available_width() - 10.0).max(100.0))
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(meta.folder.is_none(), "Uncategorized")
                                .clicked()
                            {
                                meta.folder = None;
                                changed = true;
                            }
                            for folder in folders {
                                if ui
                                    .selectable_label(
                                        meta.folder.as_deref() == Some(folder.as_str()),
                                        folder,
                                    )
                                    .clicked()
                                {
                                    meta.folder = Some(folder.clone());
                                    changed = true;
                                }
                            }
                        });
                });
                ui.add_space(6.0);
                let note = ui.add(
                    TextEdit::multiline(&mut meta.note)
                        .id(Id::new(("watch-note", album.id)))
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("Private note…")
                        .font(FontId::proportional(11.5)),
                );
                if note.changed() {
                    changed = true;
                }
                if changed {
                    actions.push(DetailAction::WatchMetaChanged);
                }
            }

            if detail.loading {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(15.0).color(accent()));
                    ui.label(
                        egui::RichText::new("Loading details…")
                            .size(11.0)
                            .color(MUTED),
                    );
                });
            }
            if let Some(error) = &detail.error {
                ui.add_space(10.0);
                Frame::none()
                    .fill(Color32::from_rgb(31, 21, 26))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(72, 36, 46)))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(error).size(10.8).color(DANGER));
                        if theme::small_button(ui, "Retry", TEXT).clicked() {
                            actions.push(DetailAction::Retry);
                        }
                    });
            }

            if let Some(loaded) = &detail.loaded {
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(egui::RichText::new("DETAILS").size(9.5).strong().color(DIM));
                ui.add_space(6.0);
                egui::Grid::new("detail-meta-grid")
                    .num_columns(2)
                    .spacing([18.0, 7.0])
                    .show(ui, |ui| {
                        meta_row(
                            ui,
                            "Format",
                            if loaded.format.is_empty() {
                                "—"
                            } else {
                                &loaded.format
                            },
                        );
                        meta_row(
                            ui,
                            "Released",
                            if loaded.date_added.is_empty() {
                                "—"
                            } else {
                                &loaded.date_added
                            },
                        );
                        meta_row(
                            ui,
                            "Downloads",
                            if loaded.download_count.is_empty() {
                                "—"
                            } else {
                                &loaded.download_count
                            },
                        );
                        meta_row(
                            ui,
                            "Rating",
                            if loaded.rating.is_empty() {
                                "—"
                            } else {
                                &loaded.rating
                            },
                        );
                        if let Some(provider) = &loaded.provider {
                            ui.label(egui::RichText::new("Provider").size(11.0).color(DIM));
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(provider).size(11.5).color(accent()),
                                    )
                                    .sense(Sense::click()),
                                )
                                .clicked()
                            {
                                actions.push(DetailAction::Search {
                                    text: provider.clone(),
                                    kind: SearchKind::Credit,
                                });
                            }
                            ui.end_row();
                        }
                    });

                if !loaded.artists.is_empty() {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("ARTISTS").size(9.5).strong().color(DIM));
                    ui.add_space(5.0);
                    ui.horizontal_wrapped(|ui| {
                        for artist in &loaded.artists {
                            if theme::chip(ui, artist, false).clicked() {
                                actions.push(DetailAction::Search {
                                    text: artist.clone(),
                                    kind: SearchKind::Artist,
                                });
                            }
                        }
                    });
                }

                if !loaded.tags.is_empty() {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("TAGS").size(9.5).strong().color(DIM));
                    ui.add_space(5.0);
                    ui.horizontal_wrapped(|ui| {
                        for tag in &loaded.tags {
                            if theme::chip(ui, tag, false).clicked() {
                                actions.push(DetailAction::Search {
                                    text: tag.clone(),
                                    kind: SearchKind::Tags,
                                });
                            }
                        }
                    });
                }

                let xfd = loaded.xfd.clone().or_else(|| album.xfd.clone());
                if let Some(xfd) = xfd {
                    ui.add_space(14.0);
                    if theme::secondary_button(ui, "Play crossfade").clicked() {
                        actions.push(DetailAction::OpenUrl(xfd));
                    }
                }
            } else if !album.tags.is_empty() {
                ui.add_space(14.0);
                ui.horizontal_wrapped(|ui| {
                    for tag in &album.tags {
                        if theme::chip(ui, tag, false).clicked() {
                            actions.push(DetailAction::Search {
                                text: tag.clone(),
                                kind: SearchKind::Tags,
                            });
                        }
                    }
                });
            }

            ui.add_space(16.0);
            if theme::secondary_button(ui, "Open album page").clicked() {
                actions.push(DetailAction::OpenUrl(album.page_url()));
            }
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(format!("DoujinStyle album #{}", album.id))
                    .size(9.5)
                    .color(DIM),
            );
            ui.add_space(20.0);
        });

    actions
}

fn meta_row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).size(11.0).color(DIM));
    ui.label(egui::RichText::new(value).size(11.5).color(TEXT));
    ui.end_row();
}

fn shortcut_row(ui: &mut Ui, key: &str, description: &str) {
    ui.horizontal(|ui| {
        let galley = ui
            .painter()
            .layout_no_wrap(key.to_string(), FontId::proportional(11.0), DIM);
        let key_width = galley.size().x + 16.0;
        let (rect, _) = ui.allocate_exact_size(vec2(key_width, 22.0), Sense::hover());
        ui.painter().rect_filled(
            rect,
            Rounding::same(4.0),
            Color32::from_rgb(25, 26, 37),
        );
        ui.painter().rect_stroke(
            rect,
            Rounding::same(4.0),
            Stroke::new(1.0_f32, BORDER_SOFT),
        );
        ui.painter().galley(
            rect.center() - galley.size() * 0.5,
            galley,
            MUTED,
        );
        ui.add_space(12.0);
        ui.label(egui::RichText::new(description).size(11.5).color(TEXT));
    });
    ui.add_space(2.0);
}
