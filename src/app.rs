use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontFamily, FontId, Frame, Id, Layout, Margin, Mesh,
    Pos2, Rect, Response, Rounding, ScrollArea, Sense, Shape, Stroke, TextEdit, Ui, Vec2,
};

use crate::api::{self, FeedPage, SearchParams};
use crate::audio::{self, AudioPlayer, PlaybackState};
use crate::downloads::{DownloadManager, DownloadNotice, DownloadState};
use crate::i18n::{negotiate_best_language, I18n, Language};
use crate::images::{ImageLoader, ImageSpec, Textures};
use crate::models::{
    is_lossless_format, sort_albums, Album, AlbumDetail, EffectsQuality, FormatFilter,
    LibraryEntry, LibrarySort, Persisted, RepeatMode, RuleField, SaveRule, SearchHistoryEntry,
    SearchKind, SearchSort, WatchMeta,
};
use crate::store;
use crate::theme::{
    self, accent, accent_dark, accent_dim, Icon, ACCENTS, BG, BORDER, BORDER_SOFT, CARD,
    CARD_HOVER, DANGER, DIM, MUTED, PANEL, SIDEBAR, SUCCESS, TEXT, WARNING,
};
use crate::tray::{Tray, TrayAction};

const CARD_GAP: f32 = 16.0;
const MIN_CARD_WIDTH: f32 = 172.0;
const DETAIL_WIDTH: f32 = 414.0;

/// Smoothly approach `target` using frame-rate independent easing.
fn approach(value: &mut f32, target: f32, dt: f32, speed: f32) -> f32 {
    let t = 1.0 - (-dt * speed).exp();
    *value += (target - *value) * t;
    if (*value - target).abs() < 0.001 {
        *value = target;
    }
    *value
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// Translucent version of a palette colour, used for the frosted-glass panels.
fn glass_fill(base: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
}

fn soft_shadow(offset: Vec2, blur: f32) -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset,
        blur,
        spread: 0.0,
        color: Color32::from_black_alpha(80),
    }
}

/// Whether the window currently has keyboard focus. Animation timers are
/// suspended while it does not, so a minimised or background window stays idle.
fn window_focused(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.viewport().focused.unwrap_or(true))
}

/// Radial glow made from a single triangle fan. No shaders, no blur passes.
fn paint_soft_glow(
    painter: &egui::Painter,
    center: Pos2,
    radius: Vec2,
    color: Color32,
    strength: f32,
) {
    if radius.x <= 0.0 || radius.y <= 0.0 || strength <= 0.0 {
        return;
    }
    let alpha = (strength.clamp(0.0, 1.0) * 255.0) as u8;
    if alpha == 0 {
        return;
    }
    const SEGMENTS: usize = 24;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(
        center,
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
    );
    for index in 0..SEGMENTS {
        let angle = index as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(
            center + vec2(angle.cos() * radius.x, angle.sin() * radius.y),
            Color32::TRANSPARENT,
        );
    }
    for index in 0..SEGMENTS {
        let a = 1 + index as u32;
        let b = 1 + ((index + 1) % SEGMENTS) as u32;
        mesh.add_triangle(0, a, b);
    }
    painter.add(Shape::mesh(mesh));
}

/// Slow, low-alpha aurora used behind empty states. Three drifting glows are
/// enough to suggest motion without a shader or a blur pass.
fn paint_aurora(painter: &egui::Painter, rect: Rect, time: f32) {
    let a = accent();
    let secondary = accent_dark();
    let t = time * 0.12;
    let blobs = [
        (0.24 + (t * 1.3).sin() * 0.08, 0.30, 0.55, a, 0.10),
        (
            0.74 + (t * 0.9 + 2.1).cos() * 0.10,
            0.62,
            0.62,
            secondary,
            0.12,
        ),
        (0.48 + (t * 0.6 + 4.0).sin() * 0.16, 0.18, 0.40, a, 0.07),
    ];
    for (fx, fy, radius, color, alpha) in blobs {
        paint_soft_glow(
            painter,
            pos2(
                rect.left() + rect.width() * fx,
                rect.top() + rect.height() * fy,
            ),
            vec2(rect.width() * radius, rect.height() * radius * 0.85),
            color,
            alpha,
        );
    }
}

/// Diagonal translucent band that appears when a card is hovered.
fn paint_light_sweep(painter: &egui::Painter, rect: Rect, strength: f32) {
    let alpha = (28.0 * strength.clamp(0.0, 1.0)) as u8;
    if alpha == 0 {
        return;
    }
    let width = rect.width() * 0.32;
    let left = rect.left() - rect.height() * 0.35;
    let mut mesh = Mesh::default();
    let color = Color32::from_white_alpha(alpha);
    mesh.colored_vertex(pos2(left, rect.top()), Color32::TRANSPARENT);
    mesh.colored_vertex(pos2(left + width, rect.top()), color);
    mesh.colored_vertex(
        pos2(left + width + rect.height() * 0.35, rect.bottom()),
        color,
    );
    mesh.colored_vertex(
        pos2(left + rect.height() * 0.35, rect.bottom()),
        Color32::TRANSPARENT,
    );
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.with_clip_rect(rect).add(Shape::mesh(mesh));
}

fn hash_time(seed: u64) -> u64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn random_pick(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    (hash_time(now ^ std::process::id() as u64) as usize) % len
}

struct Confetti {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    size: f32,
    spin: f32,
    color: Color32,
}

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
    Rss {
        items: Vec<(String, String)>,
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
    Play(Album),
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
    Play { folder: PathBuf, source: PathBuf },
    WatchMetaChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QualityFilter {
    Any,
    Lossless,
    Lossy,
}

impl QualityFilter {
    fn label(self) -> &'static str {
        match self {
            Self::Any => "Any quality",
            Self::Lossless => "Lossless",
            Self::Lossy => "Lossy",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AvailabilityFilter {
    Any,
    New,
    InLibrary,
    Queued,
    NotOwned,
}

impl AvailabilityFilter {
    fn label(self) -> &'static str {
        match self {
            Self::Any => "All results",
            Self::New => "NEW only",
            Self::InLibrary => "In library",
            Self::Queued => "In queue",
            Self::NotOwned => "Not owned",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DlFilter {
    All,
    Active,
    Paused,
    Failed,
    Done,
}

impl DlFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Active => "Active",
            Self::Paused => "Paused",
            Self::Failed => "Failed",
            Self::Done => "Done",
        }
    }

    fn matches(self, state: DownloadState) -> bool {
        match self {
            Self::All => true,
            Self::Active => state.is_active(),
            Self::Paused => state == DownloadState::Paused,
            Self::Failed => matches!(
                state,
                DownloadState::Failed | DownloadState::Manual | DownloadState::Cancelled
            ),
            Self::Done => state == DownloadState::Done,
        }
    }

    const ALL: [Self; 5] = [
        Self::All,
        Self::Active,
        Self::Paused,
        Self::Failed,
        Self::Done,
    ];
}

enum TaskAction {
    Pause(u64),
    Resume(u64),
    Cancel(u64),
    Retry(u64),
    Remove(u64),
    Prioritize(u64),
    MoveUp(u64),
    MoveDown(u64),
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
    SendToMpd(PathBuf),
    PlayFolder { folder: PathBuf, source: PathBuf },
}

struct Toast {
    text: String,
    created: Instant,
    tone: ToastTone,
    action: Option<ToastAction>,
}

#[derive(Clone)]
enum ToastAction {
    OpenFolder(PathBuf),
    OpenUrl(String),
    RestoreWatch(Album, Option<WatchMeta>),
    RestoreLibrary(LibraryEntry),
    RestoreSearchHistory(Vec<SearchHistoryEntry>),
}

impl ToastAction {
    fn label(&self) -> &'static str {
        match self {
            Self::OpenFolder(_) | Self::OpenUrl(_) => "Open",
            Self::RestoreWatch(..) | Self::RestoreLibrary(_) | Self::RestoreSearchHistory(_) => {
                "Undo"
            }
        }
    }

    fn is_undo(&self) -> bool {
        !matches!(self, Self::OpenFolder(_) | Self::OpenUrl(_))
    }
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
    /// Formatted `cache_bytes`, recomputed only when the size changes.
    cache_text: String,
    cache_checked: Instant,
    manual_opened: HashSet<u64>,
}

/// Cached library filter/sort result. Rebuilt only when the library or the
/// filter controls change, so scrolling a large collection does not re-filter
/// and re-sort every frame.
struct LibraryViewCache {
    initialized: bool,
    rev: u64,
    query: String,
    format: Option<String>,
    year: Option<u16>,
    sort: LibrarySort,
    visible: Vec<usize>,
    formats: Vec<String>,
    years: Vec<u16>,
    total_bytes: u64,
}

impl Default for LibraryViewCache {
    fn default() -> Self {
        Self {
            initialized: false,
            rev: 0,
            query: String::new(),
            format: None,
            year: None,
            sort: LibrarySort::Newest,
            visible: Vec::new(),
            formats: Vec::new(),
            years: Vec::new(),
            total_bytes: 0,
        }
    }
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
    player: AudioPlayer,
    tray: Option<Tray>,
    tray_attempted: bool,
    tray_hint_shown: bool,
    allow_exit: bool,
    rule_draft: Option<RuleDraft>,
    i18n: I18n,

    net_tx: Sender<NetEvent>,
    net_rx: Receiver<NetEvent>,

    toasts: Vec<Toast>,
    preview_cover: bool,
    focus_search: bool,
    search_save_draft: Option<String>,
    library_query: String,
    library_format_filter: Option<String>,
    library_year_filter: Option<u16>,
    library_rev: u64,
    library_view: LibraryViewCache,
    search_regex: bool,
    search_quality: QualityFilter,
    search_availability: AvailabilityFilter,
    pending_bulk: Option<Vec<Album>>,
    stats_open: bool,
    notified_matches: HashSet<(String, u64)>,
    rss_draft: String,
    rss_checked: bool,
    dl_query: String,
    dl_filter: DlFilter,
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

    // Animation clock and transition state (0..=1)
    last_frame: Instant,
    anim_dt: f32,
    view_anim: f32,
    sidebar_anim: f32,
    player_anim: f32,
    detail_anim: f32,

    // Deferred clipboard write + easter egg state
    pending_clipboard: Option<String>,
    konami_progress: usize,
    confetti: Option<(Instant, Vec<Confetti>)>,
    resume_saved: Instant,
    tray_tooltip_at: Instant,
    scale_dragging: bool,
    scrub_fraction: Option<f32>,

    // Frame pacing diagnostics (`DST_FPS=1`)
    fps_enabled: bool,
    fps_frames: u32,
    fps_since: Instant,
    fps_value: f32,
    fps_frame_ms: f32,
}

impl App {
    fn effects_enabled(&self) -> bool {
        self.persisted.settings.effects != EffectsQuality::Off
    }

    fn effects_full(&self) -> bool {
        self.persisted.settings.effects == EffectsQuality::Full
    }

    /// Full effects with motion allowed (shimmer, aurora, equalizer, confetti).
    fn motion_enabled(&self) -> bool {
        self.effects_full() && !self.persisted.settings.reduce_motion
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        // The app owns the zoom factor (it mirrors the Settings slider and is
        // applied every frame), so egui must not fight it with its own
        // Ctrl+Plus/Minus handler.
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        store::ensure_dirs();
        let persisted = store::load_state();
        theme::configure(
            &cc.egui_ctx,
            persisted.settings.accent,
            persisted.settings.light_theme,
        );
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
        let cache_bytes = store::cache_size();
        let player = AudioPlayer::new(settings.audio_enabled, settings.audio_volume);
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
            images: ImageLoader::new(2),
            textures: Textures::default(),
            downloads,
            dl_ui: DownloadUiState {
                speeds: HashMap::new(),
                cache_bytes,
                cache_text: store::human_size(cache_bytes),
                cache_checked: Instant::now(),
                manual_opened: HashSet::new(),
            },
            player,
            tray: None,
            tray_attempted: false,
            tray_hint_shown: false,
            allow_exit: false,
            rule_draft: None,
            i18n: I18n::new(),
            net_tx,
            net_rx,
            toasts: Vec::new(),
            preview_cover: false,
            focus_search: false,
            search_save_draft: None,
            library_query: String::new(),
            library_format_filter: None,
            library_year_filter: None,
            library_rev: 0,
            library_view: LibraryViewCache::default(),
            search_regex: false,
            search_quality: QualityFilter::Any,
            search_availability: AvailabilityFilter::Any,
            pending_bulk: None,
            stats_open: false,
            notified_matches: HashSet::new(),
            rss_draft: String::new(),
            rss_checked: false,
            dl_query: String::new(),
            dl_filter: DlFilter::All,
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

            last_frame: Instant::now(),
            anim_dt: 0.016,
            view_anim: 1.0,
            sidebar_anim: if settings.sidebar_collapsed { 1.0 } else { 0.0 },
            player_anim: 0.0,
            detail_anim: 0.0,
            pending_clipboard: None,
            konami_progress: 0,
            confetti: None,
            resume_saved: Instant::now(),
            tray_tooltip_at: Instant::now(),
            scale_dragging: false,
            scrub_fraction: None,
            fps_enabled: std::env::var_os("DST_FPS").is_some(),
            fps_frames: 0,
            fps_since: Instant::now(),
            fps_value: 0.0,
            fps_frame_ms: 0.0,
        };
        app.refresh_latest();
        app.sync_library_from_downloads();
        // Apply language from settings, allowing an env override for testing.
        let lang = match std::env::var("DST_EXPLORER_LANG") {
            Ok(value) if !value.trim().is_empty() => negotiate_best_language(&[value]),
            _ => Language::from_locale(&app.persisted.settings.language),
        };
        app.i18n.set_language(lang);
        app.player.set_shuffle(app.persisted.settings.shuffle);
        app.player.set_repeat(app.persisted.settings.repeat_mode);
        if app.persisted.settings.remember_position {
            let folder = app.persisted.settings.last_track_folder.clone();
            let file = app.persisted.settings.last_track_path.clone();
            let position = app.persisted.settings.last_track_position;
            if let (Some(folder), Some(file)) = (folder, file) {
                if file.is_file() && folder.is_dir() {
                    let title = file
                        .file_stem()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| "last track".to_string());
                    if app.player.prepare_folder(&folder, &file, position).is_ok() {
                        app.toast(format!("Press Play to resume “{title}”."), ToastTone::Info);
                    }
                }
            }
        }
        if app.downloads.restored_paused > 0 {
            let count = app.downloads.restored_paused;
            if app.persisted.settings.resume_on_start {
                app.downloads.resume_all();
                app.toast(
                    format!(
                        "Resumed {count} download{} from the previous session.",
                        if count == 1 { "" } else { "s" }
                    ),
                    ToastTone::Success,
                );
            } else {
                app.toast(
                    format!(
                        "Restored {count} paused download{}. Resume them in Downloads.",
                        if count == 1 { "" } else { "s" }
                    ),
                    ToastTone::Info,
                );
            }
        }
        app
    }

    fn set_language(&mut self, lang: Language) {
        self.i18n.set_language(lang);
        self.persisted.settings.language = lang.locale_id().to_string();
        self.dirty = true;
    }

    fn t(&self, key: &str) -> String {
        self.i18n.t(key, None)
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
        let mut albums = self.search.results.clone();
        let quality = self.search_quality;
        if quality != QualityFilter::Any {
            albums.retain(|album| {
                if album.format.trim().is_empty() {
                    return false;
                }
                let lossless = is_lossless_format(&album.format);
                if quality == QualityFilter::Lossless {
                    lossless
                } else {
                    !lossless
                }
            });
        }
        let availability = self.search_availability;
        if availability != AvailabilityFilter::Any {
            let queued: HashSet<u64> = self
                .downloads
                .tasks
                .iter()
                .filter(|task| task.state.is_active() || task.state == DownloadState::Paused)
                .map(|task| task.album.id)
                .collect();
            albums.retain(|album| match availability {
                AvailabilityFilter::Any => true,
                AvailabilityFilter::New => album.is_new,
                AvailabilityFilter::InLibrary => self.persisted.in_library(album.id),
                AvailabilityFilter::Queued => queued.contains(&album.id),
                AvailabilityFilter::NotOwned => {
                    !self.persisted.in_library(album.id) && !queued.contains(&album.id)
                }
            });
        }
        if self.search_regex {
            let pattern = self.search.query.trim();
            if !pattern.is_empty() {
                if let Ok(regex) = regex::RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                {
                    albums.retain(|album| {
                        regex.is_match(&album.title)
                            || regex.is_match(&album.artist)
                            || album.tags.iter().any(|tag| regex.is_match(tag))
                    });
                }
            }
        }
        self.search.display = sort_albums(albums, self.persisted.settings.search_sort);
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
        self.detail_anim = 0.0;
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

    fn check_rss_feeds(&mut self) {
        let feeds = self.persisted.settings.rss_feeds.clone();
        if feeds.is_empty() {
            return;
        }
        let tx = self.net_tx.clone();
        let _ = thread::Builder::new()
            .name("rss-feeds".into())
            .spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .user_agent(crate::api::USER_AGENT)
                    .timeout(Duration::from_secs(20))
                    .connect_timeout(Duration::from_secs(10))
                    .build();
                let Ok(client) = client else {
                    return;
                };
                let mut items = Vec::new();
                for feed in feeds {
                    if let Ok(response) = client.get(&feed).send() {
                        if let Ok(text) = response.text() {
                            items.extend(parse_feed_items(&text));
                        }
                    }
                }
                let _ = tx.send(NetEvent::Rss { items });
            });
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
                            if page == 0
                                && self.persisted.settings.notify_saved_searches
                                && !self.persisted.saved_searches.is_empty()
                            {
                                let mut hits: Vec<(String, String)> = Vec::new();
                                for saved in &self.persisted.saved_searches {
                                    let needle = saved.text.trim().to_lowercase();
                                    if needle.is_empty() {
                                        continue;
                                    }
                                    for album in &fetched {
                                        let matched = match saved.kind {
                                            SearchKind::Artist => {
                                                album.artist.to_lowercase().contains(&needle)
                                            }
                                            SearchKind::Tags => album
                                                .tags
                                                .iter()
                                                .any(|tag| tag.to_lowercase().contains(&needle)),
                                            _ => {
                                                album.title.to_lowercase().contains(&needle)
                                                    || album.artist.to_lowercase().contains(&needle)
                                            }
                                        };
                                        if matched
                                            && self
                                                .notified_matches
                                                .insert((saved.name.clone(), album.id))
                                        {
                                            hits.push((saved.name.clone(), album.title.clone()));
                                        }
                                    }
                                }
                                for (name, title) in hits.into_iter().take(3) {
                                    self.toast(
                                        format!("“{name}”: new release “{title}”"),
                                        ToastTone::Success,
                                    );
                                    self.desktop_notify(
                                        &format!("Saved search “{name}” matched"),
                                        &title,
                                    );
                                }
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
                NetEvent::Rss { items } => {
                    let mut fresh = 0;
                    for (title, link) in items {
                        if self.persisted.rss_seen.iter().any(|seen| seen == &link) {
                            continue;
                        }
                        self.persisted.rss_seen.push(link);
                        if fresh < 3 {
                            self.toast(format!("New release: {title}"), ToastTone::Info);
                            self.desktop_notify("New release", &title);
                        }
                        fresh += 1;
                    }
                    if self.persisted.rss_seen.len() > 500 {
                        let excess = self.persisted.rss_seen.len() - 500;
                        self.persisted.rss_seen.drain(0..excess);
                    }
                    if fresh > 0 {
                        self.dirty = true;
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
        let (
            search_shortcut,
            refresh,
            escape,
            download,
            palette,
            shortcuts_help,
            nav_keys,
            fullscreen,
            pause_all,
            resume_all,
            cycle_theme,
            play_pause,
            next_track,
            prev_track,
            seek_forward,
            seek_back,
            volume_up,
            volume_down,
            toggle_light,
            zoom_in,
            zoom_out,
            zoom_reset,
        ) = ctx.input(|input| {
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
                input.key_pressed(egui::Key::F11),
                input.modifiers.alt && input.modifiers.command && input.key_pressed(egui::Key::P),
                input.modifiers.alt && input.modifiers.command && input.key_pressed(egui::Key::R),
                input.modifiers.alt && input.modifiers.command && input.key_pressed(egui::Key::T),
                !typing && input.key_pressed(egui::Key::Space),
                input.modifiers.command
                    && !input.modifiers.shift
                    && input.key_pressed(egui::Key::ArrowRight),
                input.modifiers.command
                    && !input.modifiers.shift
                    && input.key_pressed(egui::Key::ArrowLeft),
                input.modifiers.command
                    && input.modifiers.shift
                    && input.key_pressed(egui::Key::ArrowRight),
                input.modifiers.command
                    && input.modifiers.shift
                    && input.key_pressed(egui::Key::ArrowLeft),
                input.modifiers.command && input.key_pressed(egui::Key::ArrowUp),
                input.modifiers.command && input.key_pressed(egui::Key::ArrowDown),
                input.modifiers.alt && input.modifiers.command && input.key_pressed(egui::Key::L),
                // Ctrl+Shift+= (plus), Ctrl+Shift+- and Ctrl+0. Bare Ctrl+Plus
                // and Ctrl+Minus work too, matching the usual zoom shortcuts.
                input.modifiers.command
                    && (input.key_pressed(egui::Key::Plus)
                        || (input.modifiers.shift && input.key_pressed(egui::Key::Equals))),
                input.modifiers.command && input.key_pressed(egui::Key::Minus),
                input.modifiers.command && input.key_pressed(egui::Key::Num0),
            )
        });

        if search_shortcut {
            self.view = View::Search;
            self.focus_search = true;
        }
        let copy_links = ctx.input(|input| {
            input.modifiers.command && input.modifiers.shift && input.key_pressed(egui::Key::C)
        });
        if copy_links && !self.selection.is_empty() {
            let links: Vec<String> = self
                .visible_albums()
                .iter()
                .filter(|album| self.selection.contains(&album.id))
                .map(|album| album.page_url())
                .collect();
            if !links.is_empty() {
                self.pending_clipboard = Some(links.join("\n"));
            }
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
        if fullscreen {
            let is_fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fullscreen));
            if !is_fullscreen && self.detail.is_some() {
                self.preview_cover = true;
            }
        }
        if pause_all {
            self.downloads.pause_all();
            self.toast("All downloads paused.", ToastTone::Info);
        }
        if resume_all {
            self.downloads.resume_all();
            self.toast("Downloads resumed.", ToastTone::Info);
        }
        if cycle_theme {
            let next = (self.persisted.settings.accent + 1) % ACCENTS.len();
            self.persisted.settings.accent = next;
            theme::set_accent(next);
            self.dirty = true;
            self.toast(
                format!("Theme accent: {}", ACCENTS[next].0),
                ToastTone::Info,
            );
        }
        if toggle_light {
            self.persisted.settings.light_theme = !self.persisted.settings.light_theme;
            theme::configure(
                ctx,
                self.persisted.settings.accent,
                self.persisted.settings.light_theme,
            );
            self.dirty = true;
            self.toast(
                if self.persisted.settings.light_theme {
                    "Light theme enabled"
                } else {
                    "Dark theme enabled"
                },
                ToastTone::Info,
            );
        }
        if play_pause {
            self.player.toggle();
        }
        if next_track {
            self.player.next();
        }
        if prev_track {
            self.player.previous();
        }
        if seek_forward {
            self.player.seek_relative(10.0);
        }
        if seek_back {
            self.player.seek_relative(-10.0);
        }
        if volume_up || volume_down {
            let delta = if volume_up { 0.05 } else { -0.05 };
            let volume = (self.persisted.settings.audio_volume + delta).clamp(0.0, 1.0);
            self.persisted.settings.audio_volume = volume;
            self.player.set_volume(volume);
            self.dirty = true;
        }
        if zoom_in || zoom_out || zoom_reset {
            let step = if zoom_in {
                0.05
            } else if zoom_out {
                -0.05
            } else {
                0.0
            };
            let current = self.persisted.settings.ui_scale;
            let target = if zoom_reset { 1.0 } else { current + step };
            // Snap to the same 5% grid as the Settings slider and keep it in
            // sync with it; the per-frame `set_zoom_factor` applies the value.
            let scale = ((target.clamp(0.75, 1.75) * 100.0).round() / 100.0).clamp(0.75, 1.75);
            if (scale - current).abs() > f32::EPSILON {
                self.persisted.settings.ui_scale = scale;
                ctx.set_zoom_factor(scale);
                self.dirty = true;
            }
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
        self.check_konami(ctx);
    }

    /// Detect the classic ↑↑↓↓←→←→BA sequence.
    fn check_konami(&mut self, ctx: &egui::Context) {
        use egui::Key;
        const SEQUENCE: [Key; 10] = [
            Key::ArrowUp,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::B,
            Key::A,
        ];
        let pressed: Vec<Key> = ctx.input(|input| {
            SEQUENCE
                .iter()
                .copied()
                .filter(|key| input.key_pressed(*key))
                .collect()
        });
        for key in pressed {
            if key == SEQUENCE[self.konami_progress] {
                self.konami_progress += 1;
            } else if key == SEQUENCE[0] {
                self.konami_progress = 1;
            } else {
                self.konami_progress = 0;
            }
            if self.konami_progress >= SEQUENCE.len() {
                self.konami_progress = 0;
                self.trigger_konami();
            }
        }
    }

    fn trigger_konami(&mut self) {
        let next =
            (self.persisted.settings.accent + 1 + random_pick(ACCENTS.len())) % ACCENTS.len();
        self.persisted.settings.accent = next;
        theme::set_accent(next);
        self.dirty = true;
        self.toast(
            format!("Konami code! Accent changed to {}.", ACCENTS[next].0),
            ToastTone::Success,
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        let palette = [
            accent(),
            SUCCESS(),
            WARNING(),
            Color32::from_rgb(140, 170, 255),
            Color32::from_rgb(255, 120, 170),
        ];
        let mut particles = Vec::with_capacity(110);
        for index in 0..110u64 {
            let seed = hash_time(now ^ (index.wrapping_mul(0x9E37_79B9)));
            let unit = |shift: u64| ((seed >> shift) & 0xFFFF) as f32 / 65535.0;
            particles.push(Confetti {
                x: unit(0),
                y: unit(16) * 0.35,
                vx: (unit(32) - 0.5) * 320.0,
                vy: 40.0 + unit(48) * 220.0,
                size: 4.0 + unit(5) * 5.0,
                spin: unit(8) * std::f32::consts::TAU,
                color: palette[(seed as usize) % palette.len()],
            });
        }
        self.confetti = Some((Instant::now(), particles));
    }

    fn draw_confetti(&mut self, ctx: &egui::Context) {
        if !self.motion_enabled() {
            self.confetti = None;
            return;
        }
        let Some((start, particles)) = self.confetti.take() else {
            return;
        };
        let elapsed = start.elapsed().as_secs_f32();
        if elapsed > 2.6 {
            self.confetti = None;
            return;
        }
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::new("konami-confetti"),
        ));
        let alpha = ((1.0 - elapsed / 2.6) * 255.0).clamp(0.0, 255.0) as u8;
        for particle in &particles {
            let x = particle.x * screen.width() + particle.vx * elapsed;
            let y =
                particle.y * screen.height() + particle.vy * elapsed + 420.0 * elapsed * elapsed;
            if y > screen.height() + 24.0 {
                continue;
            }
            let spin = particle.spin + elapsed * 9.0;
            let color = Color32::from_rgba_unmultiplied(
                particle.color.r(),
                particle.color.g(),
                particle.color.b(),
                alpha,
            );
            let half = particle.size * 0.5;
            let (sin, cos) = (spin.sin(), spin.cos());
            let corners = [
                vec2(-half, -half * 0.55),
                vec2(half, -half * 0.55),
                vec2(half, half * 0.55),
                vec2(-half, half * 0.55),
            ];
            let points: Vec<egui::Pos2> = corners
                .iter()
                .map(|corner| {
                    pos2(
                        x + corner.x * cos - corner.y * sin,
                        y + corner.x * sin + corner.y * cos,
                    )
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        self.confetti = Some((start, particles));
        ctx.request_repaint();
    }

    /// Open the command palette and build the action list.
    fn open_command_palette(&mut self) {
        self.palette_open = true;
        self.palette_query.clear();
        self.palette_selected = 0;
        self.palette_actions = vec![
            (
                "Search".to_string(),
                "Search the archive (Ctrl+F)".to_string(),
                PaletteAction::Search,
            ),
            (
                "Library".to_string(),
                "Browse downloaded albums".to_string(),
                PaletteAction::Library,
            ),
            (
                "Watchlist".to_string(),
                "Saved albums with folders and notes".to_string(),
                PaletteAction::Watchlist,
            ),
            (
                "Downloads".to_string(),
                "Active, paused, and finished downloads".to_string(),
                PaletteAction::Downloads,
            ),
            (
                "Settings".to_string(),
                "Configure the app".to_string(),
                PaletteAction::Settings,
            ),
            (
                "Refresh".to_string(),
                "Reload current view (Ctrl+R)".to_string(),
                PaletteAction::Refresh,
            ),
            (
                "Pause all downloads".to_string(),
                "Pause all active downloads".to_string(),
                PaletteAction::PauseAll,
            ),
            (
                "Resume all downloads".to_string(),
                "Resume all paused downloads".to_string(),
                PaletteAction::ResumeAll,
            ),
            (
                "Quit".to_string(),
                "Exit the application".to_string(),
                PaletteAction::Quit,
            ),
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
            self.view_anim = 0.0;
        }
        self.view = view;
        if view == View::Search && self.search.query.trim().is_empty() {
            self.focus_search = true;
        }
    }

    fn toast(&mut self, text: impl Into<String>, tone: ToastTone) {
        self.push_toast(text.into(), tone, None);
    }

    fn toast_action(&mut self, text: impl Into<String>, tone: ToastTone, action: ToastAction) {
        self.push_toast(text.into(), tone, Some(action));
    }

    fn push_toast(&mut self, text: String, tone: ToastTone, action: Option<ToastAction>) {
        self.toasts.push(Toast {
            text,
            created: Instant::now(),
            tone,
            action,
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
            .set_file_name(&format!(
                "dst-explorer-export-{}.json",
                chrono::Local::now().format("%Y%m%d")
            ))
            .save_file()
        {
            match std::fs::write(&path, serde_json::to_string_pretty(&export).unwrap()) {
                Ok(_) => self.toast(
                    format!("Exported to {}", path.display()),
                    ToastTone::Success,
                ),
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
                Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(import) => {
                        let mut imported_something = false;

                        if let Some(settings) = import.get("settings") {
                            if let Ok(s) =
                                serde_json::from_value::<crate::models::Settings>(settings.clone())
                            {
                                self.persisted.settings = s;
                                imported_something = true;
                            }
                        }

                        if let Some(watchlist) = import.get("watchlist") {
                            if let Ok(w) = serde_json::from_value::<Vec<crate::models::Album>>(
                                watchlist.clone(),
                            ) {
                                self.persisted.watchlist = w;
                                imported_something = true;
                            }
                        }

                        if let Some(watch_folders) = import.get("watch_folders") {
                            if let Ok(w) =
                                serde_json::from_value::<Vec<String>>(watch_folders.clone())
                            {
                                self.persisted.watch_folders = w;
                                imported_something = true;
                            }
                        }

                        if let Some(watch_meta) = import.get("watch_meta") {
                            if let Ok(w) = serde_json::from_value::<
                                std::collections::HashMap<u64, crate::models::WatchMeta>,
                            >(watch_meta.clone())
                            {
                                self.persisted.watch_meta = w;
                                imported_something = true;
                            }
                        }

                        if let Some(library) = import.get("library") {
                            if let Ok(l) = serde_json::from_value::<Vec<crate::models::LibraryEntry>>(
                                library.clone(),
                            ) {
                                self.persisted.library = l;
                                self.library_rev = self.library_rev.wrapping_add(1);
                                imported_something = true;
                            }
                        }

                        if let Some(save_rules) = import.get("save_rules") {
                            if let Ok(r) = serde_json::from_value::<Vec<crate::models::SaveRule>>(
                                save_rules.clone(),
                            ) {
                                self.persisted.save_rules = r;
                                imported_something = true;
                            }
                        }

                        if imported_something {
                            self.dirty = true;
                            self.toast(
                                format!("Imported from {}", path.display()),
                                ToastTone::Success,
                            );
                        } else {
                            self.toast("Import failed: no valid data found", ToastTone::Error);
                        }
                    }
                    Err(e) => self.toast(format!("Import failed: {}", e), ToastTone::Error),
                },
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
            self.library_rev = self.library_rev.wrapping_add(1);
            self.dirty = true;
        }
    }

    fn toggle_watch(&mut self, album: Album) {
        let title = album.title.clone();
        let meta = self.persisted.watch_meta.get(&album.id).cloned();
        let added = self.persisted.toggle_watch(album.clone());
        self.dirty = true;
        if added {
            self.toast(
                format!("Added “{title}” to your watchlist"),
                ToastTone::Success,
            );
        } else {
            self.toast_action(
                format!("Removed “{title}” from your watchlist"),
                ToastTone::Info,
                ToastAction::RestoreWatch(album, meta),
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

    /// Indices of the watchlist entries matching the current folder filter.
    /// Indices avoid cloning albums on every frame; the grid looks them up in
    /// the original watchlist.
    fn watch_indices(&self) -> Vec<usize> {
        self.persisted
            .watchlist
            .iter()
            .enumerate()
            .filter(|(_, album)| match &self.watch_filter {
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
            .map(|(index, _)| index)
            .collect()
    }

    fn watch_albums(&self) -> Vec<Album> {
        self.watch_indices()
            .into_iter()
            .map(|index| self.persisted.watchlist[index].clone())
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

    /// Rebuild the cached library filter/sort when the library or one of the
    /// filter controls changes. The expensive part (lowercasing titles,
    /// comparing and sorting) then runs once instead of on every repaint.
    fn ensure_library_view(&mut self) {
        let sort = self.persisted.settings.library_sort;
        {
            let cache = &self.library_view;
            if cache.initialized
                && cache.rev == self.library_rev
                && cache.query == self.library_query
                && cache.format == self.library_format_filter
                && cache.year == self.library_year_filter
                && cache.sort == sort
            {
                return;
            }
        }

        let query = self.library_query.trim().to_lowercase();
        let format_filter = self.library_format_filter.as_deref();
        let year_filter = self.library_year_filter;
        let library = &self.persisted.library;
        let mut visible: Vec<usize> = library
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                let matches_query = query.is_empty()
                    || entry.album.title.to_lowercase().contains(&query)
                    || entry.album.artist.to_lowercase().contains(&query)
                    || entry.path.to_string_lossy().to_lowercase().contains(&query);

                let matches_format = format_filter
                    .map_or(true, |f| entry.album.format.trim().eq_ignore_ascii_case(f));

                let matches_year = year_filter.map_or(true, |y| entry.album.year == y);

                matches_query && matches_format && matches_year
            })
            .map(|(index, _)| index)
            .collect();

        // Decorate once instead of lowercasing inside the comparator.
        match sort {
            LibrarySort::Title => {
                visible.sort_by_cached_key(|&index| library[index].album.title.to_lowercase());
            }
            LibrarySort::Artist => {
                visible.sort_by_cached_key(|&index| library[index].album.artist.to_lowercase());
            }
            LibrarySort::Format => {
                visible.sort_by_cached_key(|&index| library[index].album.format.to_lowercase());
            }
            LibrarySort::Size => {
                visible.sort_by_cached_key(|&index| std::cmp::Reverse(library[index].size));
            }
            LibrarySort::Newest => {
                visible.sort_by_cached_key(|&index| std::cmp::Reverse(library[index].completed_at));
            }
        }

        let total_bytes = library.iter().map(|entry| entry.size).sum();
        let formats = self.get_library_formats();
        let years = self.get_library_years();
        let cache = &mut self.library_view;
        cache.initialized = true;
        cache.rev = self.library_rev;
        cache.query.clone_from(&self.library_query);
        cache.format.clone_from(&self.library_format_filter);
        cache.year = self.library_year_filter;
        cache.sort = sort;
        cache.visible = visible;
        cache.formats = formats;
        cache.years = years;
        cache.total_bytes = total_bytes;
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
        if self.persisted.settings.confirm_bulk && albums.len() > 1 {
            self.pending_bulk = Some(albums);
            return;
        }
        self.enqueue_bulk(albums);
    }

    fn enqueue_bulk(&mut self, albums: Vec<Album>) {
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
            if self.images.wants(&spec.key) {
                self.images.request(spec);
            }
        }
    }

    fn handle_card_actions(&mut self, actions: Vec<CardAction>) {
        for action in actions {
            match action {
                CardAction::Open(album) => self.open_detail(album),
                CardAction::ToggleSelect { id, shift } => self.toggle_select(id, shift),
                CardAction::Download(album) => self.queue_download(album),
                CardAction::Play(album) => {
                    if let Some(entry) = self
                        .persisted
                        .library
                        .iter()
                        .find(|entry| entry.album.id == album.id)
                    {
                        let folder = entry.folder();
                        let source = entry.path.clone();
                        self.play_folder(&folder, &source);
                    } else {
                        self.toast("Download this album before playing it.", ToastTone::Info);
                    }
                }
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
                DetailAction::Play { folder, source } => self.play_folder(&folder, &source),
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
                TaskAction::Prioritize(id) => self.downloads.prioritize(id),
                TaskAction::MoveUp(id) => self.downloads.move_up(id),
                TaskAction::MoveDown(id) => self.downloads.move_down(id),
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
                return;
            }
            eprintln!("foobar2000 not found at: {}", foobar_path.display());
        }
        // Fall back to the system's default handler so playback still works.
        store::open_path(path);
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
            self.toast_action(
                format!("“{title}” needs a browser. Opened the host page."),
                ToastTone::Info,
                ToastAction::OpenUrl(url),
            );
        }
    }

    fn handle_download_notices(&mut self, notices: Vec<DownloadNotice>) {
        for notice in notices {
            match notice {
                DownloadNotice::Completed { title, output } => {
                    let folder = output
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| output.clone());
                    self.toast_action(
                        format!("Downloaded “{title}”"),
                        ToastTone::Success,
                        ToastAction::OpenFolder(folder),
                    );
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
        let cache_text = self.dl_ui.cache_text.clone();
        let active = self.downloads.active_count();
        let compact = ctx.screen_rect().width() < 960.0;
        let target = if self.persisted.settings.sidebar_collapsed || compact {
            1.0
        } else {
            0.0
        };
        let anim = approach(&mut self.sidebar_anim, target, self.anim_dt, 16.0);
        if (anim - target).abs() > 0.001 {
            ctx.request_repaint();
        }
        SidePanelLeft::show(ctx, self, &mut navigate, &cache_text, active, anim);
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
        let anim = approach(&mut self.detail_anim, 1.0, self.anim_dt, 15.0);
        if anim < 1.0 {
            ctx.request_repaint();
        }
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
        let playable = self
            .persisted
            .library
            .iter()
            .find(|entry| entry.album.id == album_id)
            .map(|entry| (entry.folder(), entry.path.clone()));
        let effects = self.effects_enabled();
        let motion = self.motion_enabled();
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
                .frame(
                    Frame::none()
                        .fill(glass_fill(PANEL(), 246))
                        .shadow(soft_shadow(vec2(-4.0, 0.0), 18.0))
                        .inner_margin(Margin {
                            left: detail_margin,
                            right: detail_margin,
                            top: if compact { 14.0 } else { 18.0 },
                            bottom: 16.0,
                        }),
                )
                .show(ctx, |ui| {
                    ui.set_opacity(0.08 + 0.92 * anim);
                    let slide = (1.0 - ease_out(anim)) * 14.0;
                    if slide > 0.2 {
                        ui.add_space(slide);
                    }
                    actions = draw_detail(
                        ui,
                        detail,
                        textures,
                        watchlist,
                        in_library,
                        folders,
                        watch_meta,
                        playable,
                        &mut requests,
                        effects,
                        motion,
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
        let mut copy_links = false;
        let narrow = ctx.available_rect().width() < 560.0;
        let pad = if narrow { 16.0 } else { 28.0 };
        egui::TopBottomPanel::bottom("selection-bar")
            .exact_height(if narrow { 96.0 } else { 66.0 })
            .frame(
                Frame::none()
                    .fill(glass_fill(PANEL(), 246))
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                    .shadow(soft_shadow(vec2(0.0, -3.0), 16.0))
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
                                .color(TEXT()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Clear").size(12.0).color(MUTED()),
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
                                        .color(DIM()),
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
                            if theme::secondary_button(ui, "Copy links").clicked() {
                                copy_links = true;
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
                                .color(TEXT()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::primary_button(ui, &format!("Download {count}")).clicked() {
                                download = true;
                            }
                            if theme::secondary_button(ui, "Copy links").clicked() {
                                copy_links = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Clear").size(12.5).color(MUTED()),
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
                                            .color(DIM()),
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
        if copy_links {
            let links: Vec<String> = self
                .visible_albums()
                .iter()
                .filter(|album| self.selection.contains(&album.id))
                .map(|album| album.page_url())
                .collect();
            if links.is_empty() {
                self.toast("Nothing selected to copy.", ToastTone::Info);
            } else {
                self.pending_clipboard = Some(links.join("\n"));
            }
        }
        if clear {
            self.selection.clear();
            self.selection_anchor = None;
        }
    }

    fn draw_player_bar(&mut self, ctx: &egui::Context) {
        self.player.tick();
        let show = self.player.has_track() || self.player.error().is_some();
        let anim = approach(
            &mut self.player_anim,
            if show { 1.0 } else { 0.0 },
            self.anim_dt,
            14.0,
        );
        if anim < 0.01 {
            return;
        }
        if anim < 1.0 {
            ctx.request_repaint();
        }
        let mut stop = false;
        let album = self.player_album();
        let cover_key = album.as_ref().map(|album| format!("t{}", album.id));
        let cover_tex = cover_key
            .as_deref()
            .and_then(|key| self.textures.get(key).cloned());
        let cover_tint = cover_key.as_deref().and_then(|key| self.textures.tint(key));
        let mut need_cover = false;
        let mut open_album: Option<Album> = None;
        egui::TopBottomPanel::bottom("player-bar")
            .exact_height(92.0 * anim)
            .frame(
                Frame::none()
                    .fill(glass_fill(PANEL(), 244))
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                    .shadow(soft_shadow(vec2(0.0, -3.0), 16.0))
                    .inner_margin(Margin {
                        left: 18.0,
                        right: 18.0,
                        top: 8.0,
                        bottom: 8.0,
                    }),
            )
            .show(ctx, |ui| {
                if self.effects_enabled() {
                    if let Some(tint) = cover_tint {
                        let panel = ui.max_rect();
                        paint_soft_glow(
                            ui.painter(),
                            pos2(panel.center().x, panel.top() + panel.height() * 0.35),
                            vec2(panel.width() * 0.55, panel.height() * 1.4),
                            tint,
                            0.12,
                        );
                    }
                }
                ui.set_opacity(anim);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (cover_rect, cover_resp) =
                        ui.allocate_exact_size(vec2(64.0, 64.0), Sense::click());
                    let painter = ui.painter();
                    if let Some(texture) = &cover_tex {
                        paint_texture_cover(painter, cover_rect, texture, Rounding::same(7.0), 1.0);
                    } else if let Some(album) = &album {
                        let failed = self
                            .textures
                            .is_failed(cover_key.as_deref().unwrap_or_default());
                        let shimmer = !failed && self.motion_enabled();
                        paint_cover_placeholder(
                            painter,
                            cover_rect,
                            &album.title,
                            ui.input(|i| i.time) as f32,
                            shimmer,
                        );
                        need_cover = !failed;
                    } else {
                        painter.rect_filled(cover_rect, Rounding::same(7.0), CARD());
                    }
                    if cover_resp.hovered() {
                        painter.rect_filled(
                            cover_rect,
                            Rounding::same(7.0),
                            Color32::from_black_alpha(70),
                        );
                        theme::paint_icon(
                            painter,
                            Rect::from_center_size(cover_rect.center(), vec2(20.0, 20.0)),
                            Icon::External,
                            TEXT(),
                            1.6,
                        );
                    }
                    if cover_resp.clicked() {
                        if let Some(album) = &album {
                            open_album = Some(album.clone());
                        }
                    }
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(
                        vec2(160.0, 64.0),
                        Layout::top_down(Align::LEFT),
                        |ui| {
                            ui.add_space(6.0);
                            let title = self
                                .player
                                .track_title()
                                .map(|title| title.to_string())
                                .unwrap_or_else(|| "Audio".to_string());
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(title).size(12.5).strong().color(TEXT()),
                                )
                                .truncate(),
                            );
                            if let Some(error) = self.player.error() {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(error).size(10.0).color(DANGER()),
                                    )
                                    .truncate(),
                                );
                            } else {
                                let index = self.player.queue_index().unwrap_or(0) + 1;
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Track {index}/{}",
                                        self.player.queue_len()
                                    ))
                                    .size(10.0)
                                    .color(DIM()),
                                );
                            }
                        },
                    );
                    ui.add_space(4.0);
                    if theme::icon_button(ui, Icon::Previous, 32.0, MUTED()).clicked() {
                        self.player.previous();
                    }
                    let icon = if self.player.state() == PlaybackState::Playing {
                        Icon::Pause
                    } else {
                        Icon::Play
                    };
                    if theme::icon_button(ui, icon, 40.0, accent()).clicked() {
                        self.player.toggle();
                    }
                    if theme::icon_button(ui, Icon::Next, 32.0, MUTED()).clicked() {
                        self.player.next();
                    }
                    ui.add_space(4.0);
                    let shuffle_on = self.player.shuffle();
                    if theme::icon_button(
                        ui,
                        Icon::Shuffle,
                        28.0,
                        if shuffle_on { accent() } else { MUTED() },
                    )
                    .on_hover_text(if shuffle_on {
                        "Shuffle on"
                    } else {
                        "Shuffle off"
                    })
                    .clicked()
                    {
                        self.persisted.settings.shuffle = !shuffle_on;
                        self.player.set_shuffle(!shuffle_on);
                        self.dirty = true;
                    }
                    let repeat = self.player.repeat();
                    let repeat_icon = if repeat == RepeatMode::One {
                        Icon::RepeatOne
                    } else {
                        Icon::Repeat
                    };
                    if theme::icon_button(
                        ui,
                        repeat_icon,
                        28.0,
                        if repeat == RepeatMode::Off {
                            MUTED()
                        } else {
                            accent()
                        },
                    )
                    .on_hover_text(repeat.label())
                    .clicked()
                    {
                        let next = repeat.cycle();
                        self.persisted.settings.repeat_mode = next;
                        self.player.set_repeat(next);
                        self.dirty = true;
                    }
                    ui.add_space(8.0);

                    // The time label lives inside the right-hand cluster; keeping
                    // it there means it can never be overlapped by the volume
                    // slider, no matter how narrow the player bar gets.
                    let controls_width = 210.0;
                    let wave_width = (ui.available_width() - controls_width - 16.0).max(60.0);
                    let wave_height = 44.0;
                    let cursor = ui.cursor().left_top();
                    let wave_rect = Rect::from_min_size(cursor, vec2(wave_width, wave_height));
                    let response = ui.allocate_rect(wave_rect, Sense::click_and_drag());
                    let peaks = self
                        .player
                        .waveform()
                        .map(|waveform| waveform.peaks.clone())
                        .unwrap_or_default();
                    let duration = self.player.duration();
                    let pointer_fraction = (response.dragged() || response.clicked())
                        .then(|| response.interact_pointer_pos())
                        .flatten()
                        .map(|position| {
                            ((position.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0)
                        });

                    // While dragging, only move the visual playhead. The actual seek
                    // happens once on release, which avoids rebuilding the audio
                    // sink dozens of times per second and stops the slider jumping.
                    if response.dragged() {
                        self.scrub_fraction = pointer_fraction;
                    }
                    if response.drag_stopped() {
                        if let Some(fraction) = self.scrub_fraction.take() {
                            self.player.set_position_fraction(fraction);
                        }
                    }
                    if response.clicked() {
                        if let Some(fraction) = pointer_fraction {
                            self.player.set_position_fraction(fraction);
                            self.scrub_fraction = None;
                        }
                    }

                    let progress = self
                        .scrub_fraction
                        .unwrap_or_else(|| self.player.progress());
                    paint_waveform(ui.painter(), wave_rect, &peaks, progress);
                    let shown_position = self
                        .scrub_fraction
                        .map(|fraction| fraction * duration)
                        .unwrap_or_else(|| self.player.position());
                    // Keep redrawing while dragging / playing so the playhead stays smooth.
                    if (response.hovered()
                        || self.player.is_playing()
                        || self.scrub_fraction.is_some())
                        && window_focused(ctx)
                    {
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                    ui.add_space(6.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, 26.0, MUTED())
                            .on_hover_text("Stop")
                            .clicked()
                        {
                            stop = true;
                        }
                        if ui
                            .add_sized(
                                [64.0, 18.0],
                                egui::Slider::new(
                                    &mut self.persisted.settings.audio_volume,
                                    0.0..=1.0,
                                )
                                .show_value(false),
                            )
                            .changed()
                        {
                            self.player.set_volume(self.persisted.settings.audio_volume);
                            self.dirty = true;
                        }
                        ui.add_space(2.0);
                        let icon_rect = Rect::from_center_size(
                            pos2(ui.cursor().left() + 7.0, ui.cursor().center().y),
                            vec2(14.0, 14.0),
                        );
                        theme::paint_icon(ui.painter(), icon_rect, Icon::Volume, DIM(), 1.3);
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / {}",
                                track_time(shown_position),
                                track_time(duration)
                            ))
                            .size(10.0)
                            .color(DIM()),
                        );
                    });
                });
            });
        if need_cover {
            if let Some(album) = &album {
                self.request_images(vec![ImageSpec {
                    key: format!("t{}", album.id),
                    url: album.thumb_url(),
                    cache_path: store::thumb_cache_path(album.id),
                    max_dim: 288,
                }]);
            }
        }
        if let Some(album) = open_album {
            self.open_detail(album);
        }
        if stop {
            self.player.stop();
        }
        if self.player.is_playing() && window_focused(ctx) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Find the library album that owns the currently playing folder.
    fn player_album(&self) -> Option<Album> {
        let folder = self.player.queue_folder()?;
        self.persisted
            .library
            .iter()
            .find(|entry| entry.folder() == folder)
            .map(|entry| entry.album.clone())
    }

    /// Start playback for a library folder, reporting a toast on failure.
    fn play_folder(&mut self, folder: &Path, source: &Path) {
        if folder.is_dir() {
            match self.player.play_folder(folder) {
                Ok(count) => self.toast(
                    format!("Playing {count} track{}", if count == 1 { "" } else { "s" }),
                    ToastTone::Info,
                ),
                Err(error) => self.toast(error, ToastTone::Error),
            }
        } else if source.is_file() && audio::is_audio_file(source) {
            match self.player.play_file(source) {
                Ok(()) => self.toast("Playing preview", ToastTone::Info),
                Err(error) => self.toast(error, ToastTone::Error),
            }
        } else {
            self.toast("That folder no longer exists.", ToastTone::Error);
        }
    }

    fn draw_central(&mut self, ctx: &egui::Context) {
        let compact = ctx.screen_rect().width() < 960.0;
        let (pad_left, pad_right, pad_top, pad_bottom) = if compact {
            (16.0, 16.0, 14.0, 14.0)
        } else {
            (28.0, 24.0, 20.0, 18.0)
        };
        let anim = approach(&mut self.view_anim, 1.0, self.anim_dt, 16.0);
        let slide = (1.0 - ease_out(anim)) * 12.0;
        if anim < 1.0 {
            ctx.request_repaint();
        }
        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG()).inner_margin(Margin {
                left: pad_left,
                right: pad_right,
                top: pad_top,
                bottom: pad_bottom,
            }))
            .show(ctx, |ui| {
                ui.set_opacity(0.1 + 0.9 * anim);
                if slide > 0.2 {
                    ui.add_space(slide);
                }
                let empty = match self.view {
                    View::Discover => self.latest.is_empty(),
                    View::Search => self.search.searched && self.search.display.is_empty(),
                    _ => false,
                };
                if empty && self.motion_enabled() {
                    let rect = ui.max_rect();
                    let time = ui.input(|i| i.time) as f32;
                    paint_aurora(ui.painter(), rect, time);
                    if window_focused(ui.ctx()) {
                        ui.ctx().request_repaint_after(Duration::from_millis(33));
                    }
                }
                match self.view {
                    View::Discover => self.view_discover(ui),
                    View::Search => self.view_search(ui),
                    View::Watchlist => self.view_watchlist(ui),
                    View::Library => self.view_library(ui),
                    View::Downloads => self.view_downloads(ui),
                    View::Settings => self.view_settings(ui, ctx),
                }
            });
    }

    fn view_discover(&mut self, ui: &mut Ui) {
        let loaded = self.latest.len();
        view_header(
            ui,
            self.t("discover-eyebrow"),
            self.t("discover-title"),
            self.t("discover-desc"),
            |ui| {
                if theme::icon_button(ui, Icon::Refresh, 36.0, MUTED())
                    .on_hover_text(self.t("refresh-hover"))
                    .clicked()
                {
                    self.refresh_latest();
                }
                if loaded > 0 {
                    if theme::icon_button(ui, Icon::Dice, 32.0, MUTED())
                        .on_hover_text("Surprise me")
                        .clicked()
                    {
                        let album = self.latest[random_pick(self.latest.len())].clone();
                        self.open_detail(album);
                    }
                    ui.label(
                        egui::RichText::new(format!("{} {}", loaded, self.t("loaded")))
                            .size(11.0)
                            .color(DIM()),
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
        let effects = self.effects_enabled();
        let motion = self.motion_enabled();
        let (actions, near_bottom) = draw_grid(
            ui,
            &self.latest,
            None,
            &mut self.textures,
            &self.selection,
            &self.persisted.watchlist,
            &self.persisted.watch_meta,
            &self.persisted.watch_folders,
            &self.persisted.library,
            &mut requests,
            self.persisted.settings.show_new_badges,
            footer,
            self.persisted.settings.compact_grid,
            ("latest", self.latest_gen),
            self.player.queue_folder(),
            effects,
            motion,
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
            .fill(PANEL())
            .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
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
                                egui::RichText::new(self.t("search-button"))
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
                            .color(DIM()),
                    );
                    ui.add_space(2.0);
                    for kind in SearchKind::ALL {
                        if theme::chip(ui, kind.label(), self.search.kind == kind).clicked() {
                            self.search.kind = kind;
                        }
                    }
                    ui.add_space(18.0);
                    ui.label(
                        egui::RichText::new("FORMAT")
                            .size(9.5)
                            .strong()
                            .color(DIM()),
                    );
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
                .fill(CARD())
                .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                .rounding(Rounding::same(11.0))
                .inner_margin(Margin::symmetric(12.0, 9.0))
                .show(ui, |ui| {
                    if !history.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("RECENT")
                                    .size(9.5)
                                    .strong()
                                    .color(DIM()),
                            );
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
                            if theme::small_button(ui, "Clear", DIM()).clicked() {
                                clear_history = true;
                            }
                        });
                    }
                    if !saved.is_empty() {
                        if !history.is_empty() {
                            ui.add_space(6.0);
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("SAVED").size(9.5).strong().color(DIM()));
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
                                    if theme::icon_button(ui, Icon::Close, 20.0, DIM())
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
                        if theme::small_button(ui, "Save this search", MUTED()).clicked() {
                            open_save = true;
                        }
                    }
                });
        }

        if open_save {
            self.search_save_draft = Some(query.clone());
        }
        if clear_history {
            let snapshot = self.persisted.search_history.clone();
            if self.persisted.clear_search_history() {
                self.dirty = true;
                self.toast_action(
                    "Recent searches cleared.",
                    ToastTone::Info,
                    ToastAction::RestoreSearchHistory(snapshot),
                );
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
                .fill(PANEL())
                .stroke(Stroke::new(1.0_f32, accent_dim(70)))
                .rounding(Rounding::same(11.0))
                .inner_margin(Margin::symmetric(12.0, 9.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("SAVE AS")
                                .size(9.5)
                                .strong()
                                .color(DIM()),
                        );
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
                        if theme::icon_button(ui, Icon::Close, 22.0, DIM())
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
                ui.label(egui::RichText::new("SORT").size(9.5).strong().color(DIM()));
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

        if self.search.searched {
            let mut filters_changed = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("FILTER")
                        .size(9.5)
                        .strong()
                        .color(DIM()),
                );
                ui.add_space(2.0);
                if theme::chip(ui, "Regex", self.search_regex).clicked() {
                    self.search_regex = !self.search_regex;
                    filters_changed = true;
                }
                egui::ComboBox::from_id_salt("search-quality")
                    .selected_text(self.search_quality.label())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for value in [
                            QualityFilter::Any,
                            QualityFilter::Lossless,
                            QualityFilter::Lossy,
                        ] {
                            if ui
                                .selectable_value(&mut self.search_quality, value, value.label())
                                .changed()
                            {
                                filters_changed = true;
                            }
                        }
                    });
                egui::ComboBox::from_id_salt("search-availability")
                    .selected_text(self.search_availability.label())
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for value in [
                            AvailabilityFilter::Any,
                            AvailabilityFilter::New,
                            AvailabilityFilter::InLibrary,
                            AvailabilityFilter::Queued,
                            AvailabilityFilter::NotOwned,
                        ] {
                            if ui
                                .selectable_value(
                                    &mut self.search_availability,
                                    value,
                                    value.label(),
                                )
                                .changed()
                            {
                                filters_changed = true;
                            }
                        }
                    });
                if self.search_regex
                    && regex::RegexBuilder::new(self.search.query.trim())
                        .case_insensitive(true)
                        .build()
                        .is_err()
                {
                    ui.label(
                        egui::RichText::new("invalid pattern")
                            .size(10.0)
                            .color(DANGER()),
                    );
                }
            });
            if filters_changed {
                self.rebuild_search_display();
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
                    egui::Label::new(egui::RichText::new(title).size(14.0).strong().color(TEXT()))
                        .truncate(),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}",
                        self.search.kind.label(),
                        self.search.formats.summary()
                    ))
                    .size(11.0)
                    .color(DIM()),
                );
            } else {
                ui.label(
                    egui::RichText::new("Search the archive")
                        .size(15.0)
                        .strong()
                        .color(TEXT()),
                );
                ui.label(
                    egui::RichText::new(
                        "Try a title, an artist, a tag like “Rock”, or the person who provided it.",
                    )
                    .size(11.5)
                    .color(MUTED()),
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
                ui.add_space(6.0);
                if theme::secondary_button(ui, "Surprise me").clicked() {
                    let album = self.search.display[random_pick(self.search.display.len())].clone();
                    self.open_detail(album);
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
        let effects = self.effects_enabled();
        let motion = self.motion_enabled();
        let (actions, near_bottom) = draw_grid(
            ui,
            &self.search.display,
            None,
            &mut self.textures,
            &self.selection,
            &self.persisted.watchlist,
            &self.persisted.watch_meta,
            &self.persisted.watch_folders,
            &self.persisted.library,
            &mut requests,
            false,
            footer,
            self.persisted.settings.compact_grid,
            ("search", self.search.gen),
            self.player.queue_folder(),
            effects,
            motion,
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
        let indices = self.watch_indices();
        let shown = indices.len();
        let note_count = self
            .persisted
            .watchlist
            .iter()
            .filter(|album| !self.persisted.watch_note_of(album.id).trim().is_empty())
            .count();
        let subtitle = if total == 0 {
            self.t("watchlist-empty")
        } else {
            let mut text = if shown == total {
                format!("{total} {}", self.t("albums-word"))
            } else {
                format!("{shown} / {total} {}", self.t("albums-word"))
            };
            if note_count > 0 {
                text.push_str(&format!(" · {note_count} ✎"));
            }
            text
        };
        let mut add_folder = false;
        let mut download_shown = false;
        view_header(
            ui,
            self.t("collection"),
            self.t("watchlist-title"),
            subtitle,
            |ui| {
                if shown > 0 {
                    let label = if shown == total {
                        "Download all"
                    } else {
                        "Download shown"
                    };
                    if theme::secondary_button(ui, label).clicked() {
                        download_shown = true;
                    }
                }
                if theme::secondary_button(ui, "New folder").clicked() {
                    add_folder = true;
                }
            },
        );
        if download_shown {
            let albums: Vec<Album> = indices
                .iter()
                .map(|&index| self.persisted.watchlist[index].clone())
                .collect();
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
            self.view = View::Downloads;
        }
        if add_folder {
            self.folder_draft = Some(String::new());
        }
        ui.add_space(10.0);

        let mut select_all = false;
        let mut select_uncategorized = false;
        let mut select_folder: Option<usize> = None;
        {
            let folders = &self.persisted.watch_folders;
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("FOLDERS")
                        .size(9.5)
                        .strong()
                        .color(DIM()),
                );
                if theme::chip(ui, "All", self.watch_filter == WatchFilter::All).clicked() {
                    select_all = true;
                }
                if theme::chip(
                    ui,
                    "Uncategorized",
                    self.watch_filter == WatchFilter::Uncategorized,
                )
                .clicked()
                {
                    select_uncategorized = true;
                }
                for (index, folder) in folders.iter().enumerate() {
                    let selected = matches!(
                        &self.watch_filter,
                        WatchFilter::Folder(name) if name == folder
                    );
                    if theme::chip(ui, folder, selected).clicked() {
                        select_folder = Some(index);
                    }
                }
            });
        }
        if select_all {
            self.set_watch_filter(WatchFilter::All);
        } else if select_uncategorized {
            self.set_watch_filter(WatchFilter::Uncategorized);
        } else if let Some(index) = select_folder {
            let name = self.persisted.watch_folders[index].clone();
            self.set_watch_filter(WatchFilter::Folder(name));
        }

        if let WatchFilter::Folder(name) = self.watch_filter.clone() {
            ui.horizontal_wrapped(|ui| {
                if theme::small_button(ui, "Rename folder", MUTED()).clicked() {
                    self.folder_rename = Some((name.clone(), name.clone()));
                }
                if theme::small_button(ui, "Delete folder", DANGER()).clicked() {
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
                    .fill(PANEL())
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                    .rounding(Rounding::same(9.0))
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("NEW FOLDER")
                                    .size(9.5)
                                    .strong()
                                    .color(DIM()),
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
                            if theme::small_button(ui, "Cancel", MUTED()).clicked() {
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
                    .fill(PANEL())
                    .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                    .rounding(Rounding::same(9.0))
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("RENAME “{old}”"))
                                    .size(9.5)
                                    .strong()
                                    .color(DIM()),
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
                            if theme::small_button(ui, "Cancel", MUTED()).clicked() {
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

        if shown == 0 {
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
        let effects = self.effects_enabled();
        let motion = self.motion_enabled();
        let (actions, _) = draw_grid(
            ui,
            &self.persisted.watchlist,
            Some(indices.as_slice()),
            &mut self.textures,
            &self.selection,
            &self.persisted.watchlist,
            &self.persisted.watch_meta,
            &self.persisted.watch_folders,
            &self.persisted.library,
            &mut requests,
            false,
            GridFooter::None,
            self.persisted.settings.compact_grid,
            "watchlist",
            self.player.queue_folder(),
            effects,
            motion,
        );
        self.request_images(requests);
        self.handle_card_actions(actions);
    }

    fn view_library(&mut self, ui: &mut Ui) {
        self.ensure_library_view();
        let count = self.persisted.library.len();
        let total = self.library_view.total_bytes;
        let subtitle = format!(
            "{} {} · {}",
            count,
            self.t("albums-word"),
            store::human_size(total)
        );
        view_header(
            ui,
            self.t("collection"),
            self.t("library-title"),
            subtitle,
            |ui| {
                if theme::secondary_button(ui, "Statistics").clicked() {
                    self.stats_open = !self.stats_open;
                }
                if theme::secondary_button(ui, self.t("open-downloads-folder")).clicked() {
                    let dir = self.persisted.settings.download_dir.clone();
                    let _ = std::fs::create_dir_all(&dir);
                    store::open_path(&dir);
                }
            },
        );
        ui.add_space(8.0);

        if self.stats_open && count > 0 {
            let lossless = self
                .persisted
                .library
                .iter()
                .filter(|entry| is_lossless_format(&entry.album.format))
                .count();
            let lossy = count.saturating_sub(lossless);
            let missing = self
                .persisted
                .library
                .iter()
                .filter(|entry| entry.missing())
                .count();
            let average = if count > 0 { total / count as u64 } else { 0 };
            Frame::none()
                .fill(PANEL())
                .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                .rounding(Rounding::same(11.0))
                .inner_margin(Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let stat = |ui: &mut Ui, label: &str, value: String, color: Color32| {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(value).size(15.0).strong().color(color),
                                );
                                ui.label(egui::RichText::new(label).size(9.5).color(DIM()));
                            });
                            ui.add_space(18.0);
                        };
                        stat(ui, "ALBUMS", format_number(count), TEXT());
                        stat(ui, "TOTAL SIZE", store::human_size(total), TEXT());
                        stat(ui, "AVERAGE", store::human_size(average), TEXT());
                        stat(ui, "LOSSLESS", format_number(lossless), SUCCESS());
                        stat(ui, "LOSSY", format_number(lossy), MUTED());
                        stat(
                            ui,
                            "MISSING",
                            format_number(missing),
                            if missing > 0 { DANGER() } else { DIM() },
                        );
                    });
                });
            ui.add_space(8.0);
        }

        // Filter toolbar
        ui.horizontal(|ui| {
            // Format filter
            let formats = &self.library_view.formats;
            let format_label = self
                .library_format_filter
                .as_ref()
                .map(|f| f.as_str())
                .unwrap_or("All formats");
            egui::ComboBox::from_id_salt("library_format_filter")
                .selected_text(format_label)
                .width(140.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.library_format_filter, None, "All formats");
                    for fmt in formats {
                        ui.selectable_value(
                            &mut self.library_format_filter,
                            Some(fmt.clone()),
                            fmt,
                        );
                    }
                });

            ui.add_space(8.0);

            // Year filter
            let years = &self.library_view.years;
            let year_label = self
                .library_year_filter
                .map(|y| y.to_string())
                .unwrap_or_else(|| "All years".to_string());
            egui::ComboBox::from_id_salt("library_year_filter")
                .selected_text(year_label)
                .width(110.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.library_year_filter, None, "All years");
                    for year in years {
                        ui.selectable_value(
                            &mut self.library_year_filter,
                            Some(*year),
                            year.to_string(),
                        );
                    }
                });

            ui.add_space(8.0);
            egui::ComboBox::from_id_salt("library_sort")
                .selected_text(format!(
                    "Sort: {}",
                    self.persisted.settings.library_sort.label()
                ))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for sort in LibrarySort::ALL {
                        ui.selectable_value(
                            &mut self.persisted.settings.library_sort,
                            sort,
                            sort.label(),
                        );
                    }
                });

            ui.add_space(16.0);

            // Search box
            let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            theme::paint_icon(ui.painter(), rect, Icon::Search, DIM(), 1.4);
            let width = (ui.available_width() - 36.0).max(60.0);
            ui.add_sized(
                [width, 24.0],
                TextEdit::singleline(&mut self.library_query)
                    .id(Id::new("library-filter"))
                    .frame(false)
                    .hint_text("Filter by title, artist or folder…")
                    .font(FontId::proportional(13.0)),
            );
            if !self.library_query.is_empty() && theme::small_button(ui, "Clear", DIM()).clicked() {
                self.library_query.clear();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Clear all filters
                let has_filters = self.library_format_filter.is_some()
                    || self.library_year_filter.is_some()
                    || !self.library_query.is_empty();
                if has_filters && theme::small_button(ui, "Clear all", DIM()).clicked() {
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

        let visible = &self.library_view.visible;
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
                for &index in visible {
                    let entry = &persisted.library[index];
                    let missing = entry.missing();
                    let response = Frame::none()
                        .fill(CARD())
                        .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                        .rounding(Rounding::same(11.0))
                        .inner_margin(Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (cover, cover_response) =
                                    ui.allocate_exact_size(vec2(56.0, 56.0), Sense::click());
                                // Separate key from the grid (which uses "t{id}" at a
                                // larger size) so a small library thumbnail can't poison
                                // the shared texture cache with a blurry image.
                                let key = format!("tl{}", entry.album.id);
                                if let Some(texture) = textures.get(&key) {
                                    paint_texture_cover(
                                        ui.painter(),
                                        cover,
                                        texture,
                                        Rounding::same(8.0),
                                        1.0,
                                    );
                                } else {
                                    paint_cover_placeholder(
                                        ui.painter(),
                                        cover,
                                        &entry.album.title,
                                        ui.input(|i| i.time) as f32,
                                        false,
                                    );
                                    requests.push(ImageSpec {
                                        key: key.clone(),
                                        url: entry.album.thumb_url(),
                                        cache_path: store::thumb_cache_path(entry.album.id),
                                        max_dim: 160,
                                    });
                                }
                                if cover_response.clicked() {
                                    actions.push(LibraryAction::Open(entry.album.clone()));
                                }
                                ui.add_space(10.0);

                                let buttons_width = 105.0;
                                let text_width = (ui.available_width() - buttons_width).max(80.0);
                                ui.allocate_ui_with_layout(
                                    vec2(text_width, 56.0),
                                    Layout::top_down(Align::LEFT),
                                    |ui| {
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    egui::RichText::new(&entry.album.title)
                                                        .size(13.0)
                                                        .strong()
                                                        .color(if missing {
                                                            MUTED()
                                                        } else {
                                                            TEXT()
                                                        }),
                                                )
                                                .truncate()
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
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(artist)
                                                    .size(11.0)
                                                    .color(MUTED()),
                                            )
                                            .truncate(),
                                        );
                                        ui.add_space(2.0);
                                        ui.horizontal_wrapped(|ui| {
                                            let mut parts = Vec::new();
                                            if !entry.album.format.is_empty() {
                                                parts.push(entry.album.format.clone());
                                            }
                                            if self.persisted.settings.library_show_year
                                                && entry.album.year > 0
                                            {
                                                parts.push(entry.album.year.to_string());
                                            }
                                            parts.push(store::human_size(entry.size));
                                            let when = format_relative_time(entry.completed_at);
                                            if !when.is_empty() {
                                                parts.push(when);
                                            }
                                            if self.persisted.settings.library_show_tags
                                                && !entry.album.tags.is_empty()
                                            {
                                                parts.push(
                                                    entry
                                                        .album
                                                        .tags
                                                        .iter()
                                                        .take(3)
                                                        .cloned()
                                                        .collect::<Vec<_>>()
                                                        .join(", "),
                                                );
                                            }
                                            ui.label(
                                                egui::RichText::new(parts.join(" · "))
                                                    .size(10.0)
                                                    .color(DIM()),
                                            );
                                            if missing {
                                                theme::badge(
                                                    ui,
                                                    "MISSING",
                                                    DANGER(),
                                                    Color32::from_rgb(38, 23, 29),
                                                );
                                            } else if entry.warning.is_some() {
                                                theme::badge(
                                                    ui,
                                                    "WARNING",
                                                    WARNING(),
                                                    Color32::from_rgb(38, 32, 20),
                                                );
                                            }
                                        });
                                    },
                                );

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if theme::small_button(ui, "MPD", MUTED())
                                        .on_hover_text("Add this folder to the MPD queue")
                                        .clicked()
                                    {
                                        actions.push(LibraryAction::SendToMpd(entry.folder()));
                                    }
                                    if (entry
                                        .extracted
                                        .as_ref()
                                        .map(|p| p.exists())
                                        .unwrap_or(false)
                                        || entry.path.exists())
                                        && theme::icon_button(ui, Icon::Play, 30.0, accent())
                                            .on_hover_text("Play in DST-Explorer")
                                            .clicked()
                                    {
                                        actions.push(LibraryAction::PlayFolder {
                                            folder: entry.folder(),
                                            source: entry.path.clone(),
                                        });
                                    }
                                    if theme::icon_button(ui, Icon::Trash, 30.0, MUTED())
                                        .on_hover_text("Remove from library")
                                        .clicked()
                                    {
                                        actions.push(LibraryAction::Remove(entry.album.id));
                                    }
                                    if !entry
                                        .extracted
                                        .as_ref()
                                        .map(|p| p.exists())
                                        .unwrap_or(false)
                                        && !entry.path.exists()
                                    {
                                        // Don't offer foobar if files don't exist
                                    } else if theme::icon_button(ui, Icon::Folder, 30.0, MUTED())
                                        .on_hover_text("Open folder")
                                        .clicked()
                                    {
                                        actions.push(LibraryAction::OpenFolder(entry.folder()));
                                    } else if theme::icon_button(ui, Icon::External, 30.0, MUTED())
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
                    let removed = self
                        .persisted
                        .library
                        .iter()
                        .find(|entry| entry.album.id == album_id)
                        .cloned();
                    let title = removed
                        .as_ref()
                        .map(|entry| entry.album.title.clone())
                        .unwrap_or_default();
                    self.persisted
                        .library
                        .retain(|entry| entry.album.id != album_id);
                    self.library_rev = self.library_rev.wrapping_add(1);
                    self.downloads.forget_done_album(album_id);
                    self.dirty = true;
                    match removed {
                        Some(entry) => self.toast_action(
                            format!("Removed “{title}” from your library"),
                            ToastTone::Info,
                            ToastAction::RestoreLibrary(entry),
                        ),
                        None => self.toast(
                            format!("Removed “{title}” from your library"),
                            ToastTone::Info,
                        ),
                    }
                }
                LibraryAction::PlayInFoobar(path) => self.play_in_foobar2000(&path),
                LibraryAction::PlayFolder { folder, source } => self.play_folder(&folder, &source),
                LibraryAction::SendToMpd(path) => {
                    let host = self.persisted.settings.mpd_host.trim().to_string();
                    if host.is_empty() {
                        self.toast(
                            "Set your MPD host in Settings → Integrations first.",
                            ToastTone::Error,
                        );
                    } else {
                        let port = self.persisted.settings.mpd_port;
                        match mpd_command(&host, port, &format!("add \"{}\"", mpd_escape(&path))) {
                            Ok(_) => self.toast("Added to the MPD queue.", ToastTone::Success),
                            Err(error) => self.toast(format!("MPD: {error}"), ToastTone::Error),
                        }
                    }
                }
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
            format!("{active} {}", self.t("active-word")),
            format!("{} {}", self.downloads.queued_count(), self.t("queued")),
        ];
        if paused > 0 {
            parts.push(format!("{paused} {}", self.t("paused")));
        }
        if failed > 0 {
            parts.push(format!("{failed} {}", self.t("failed")));
        }
        parts.push(format!(
            "{} {}",
            self.downloads.tasks.len(),
            self.t("total-word")
        ));
        let subtitle = parts.join(" · ");
        view_header(
            ui,
            self.t("downloads-eyebrow"),
            self.t("downloads-title"),
            subtitle,
            |ui| {
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
            },
        );
        ui.add_space(14.0);

        if !self.downloads.tasks.is_empty() {
            let hosts = self.downloads.hosts();
            if !hosts.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("HOSTS").size(9.5).strong().color(DIM()));
                    ui.add_space(2.0);
                    for host in hosts {
                        let active = self.downloads.tasks.iter().any(|task| {
                            task.host.as_deref() == Some(host.as_str()) && task.state.is_active()
                        });
                        let paused = self.downloads.tasks.iter().any(|task| {
                            task.host.as_deref() == Some(host.as_str())
                                && task.state == DownloadState::Paused
                        });
                        let label = if paused && !active {
                            format!("{host} · paused")
                        } else {
                            host.clone()
                        };
                        if theme::chip(ui, &label, active).clicked() {
                            if paused && !active {
                                self.downloads.resume_host(&host);
                                self.toast(
                                    format!("Resumed downloads from {host}."),
                                    ToastTone::Info,
                                );
                            } else {
                                self.downloads.pause_host(&host);
                                self.toast(
                                    format!("Paused downloads from {host}."),
                                    ToastTone::Info,
                                );
                            }
                        }
                    }
                });
                ui.add_space(10.0);
            }
        }

        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("FILTER")
                    .size(9.5)
                    .strong()
                    .color(DIM()),
            );
            ui.add_space(2.0);
            for filter in DlFilter::ALL {
                if theme::chip(ui, filter.label(), self.dl_filter == filter).clicked() {
                    self.dl_filter = filter;
                }
            }
            ui.add_space(12.0);
            let width = (ui.available_width() - 40.0).max(120.0);
            ui.add_sized(
                [width, 24.0],
                TextEdit::singleline(&mut self.dl_query)
                    .id(Id::new("dl-filter"))
                    .frame(false)
                    .hint_text("Filter queue by title, artist or host…")
                    .font(FontId::proportional(12.0)),
            );
            if !self.dl_query.is_empty() && theme::small_button(ui, "Clear", DIM()).clicked() {
                self.dl_query.clear();
            }
        });
        ui.add_space(8.0);

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

        let query = self.dl_query.trim().to_lowercase();
        let filter = self.dl_filter;
        let tasks = &self.downloads.tasks;
        let visible: Vec<usize> = tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| {
                if !filter.matches(task.state) {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                task.album.title.to_lowercase().contains(&query)
                    || task.album.artist.to_lowercase().contains(&query)
                    || task
                        .host
                        .as_deref()
                        .map(|host| host.to_lowercase().contains(&query))
                        .unwrap_or(false)
            })
            .map(|(index, _)| index)
            .collect();
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

        if visible.is_empty() {
            draw_empty(
                ui,
                Icon::Search,
                "No downloads match this filter",
                "Clear the filter or pick another status.",
                None,
                &mut false,
            );
        } else {
            let mut actions = Vec::new();
            ScrollArea::vertical()
                .id_salt("downloads-list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for index in visible {
                        let task = &tasks[index];
                        let speed = speeds.get(&task.id).map(|(_, _, s)| *s).unwrap_or(0.0);
                        task_row(ui, task, speed, &mut actions);
                        ui.add_space(10.0);
                    }
                });
            self.handle_task_actions(actions);
        }

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
            egui::RichText::new(self.t("preferences"))
                .size(10.5)
                .strong()
                .color(accent()),
        );
        ui.add_space(3.0);
        ui.label(
            egui::RichText::new(self.t("settings"))
                .font(FontId::new(27.0, FontFamily::Proportional))
                .strong()
                .color(TEXT()),
        );
        ui.add_space(12.0);

        ScrollArea::vertical()
            .id_salt("settings-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                settings_card(ui, self.t("downloads"), |ui| {
                    ui.label(
                        egui::RichText::new(self.t("save-location"))
                            .size(12.0)
                            .color(MUTED()),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        Frame::none()
                            .fill(BG())
                            .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
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
                                        .color(TEXT()),
                                    )
                                    .truncate(),
                                );
                            });
                        if theme::small_button(ui, self.t("browse"), TEXT()).clicked() {
                            if let Some(folder) = rfd::FileDialog::new()
                                .set_title(self.t("choose-download-folder"))
                                .set_directory(&self.persisted.settings.download_dir)
                                .pick_folder()
                            {
                                self.persisted.settings.download_dir = folder;
                                changed = true;
                            }
                        }
                        if theme::small_button(ui, self.t("open"), TEXT()).clicked() {
                            let dir = self.persisted.settings.download_dir.clone();
                            let _ = std::fs::create_dir_all(&dir);
                            store::open_path(&dir);
                        }
                    });
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(self.t("simultaneous-downloads"))
                                .size(12.0)
                                .color(MUTED()),
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
                    let auto_extract_label = self.t("auto-extract");
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.auto_extract,
                            egui::RichText::new(auto_extract_label)
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(self.t("foobar2000-path"))
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            Frame::none()
                                .fill(BG())
                                .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
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
                                                    .unwrap_or_else(|| self.t("not-set").to_string()),
                                            )
                                            .size(11.5)
                                            .color(TEXT()),
                                        )
                                        .truncate(),
                                    );
                                });
                            if theme::small_button(ui, self.t("browse"), TEXT()).clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title(self.t("select-foobar2000"))
                                    .add_filter("Executable", &["exe"])
                                    .pick_file()
                                {
                                    self.persisted.settings.foobar2000_path = Some(path);
                                    changed = true;
                                }
                            }
                            if theme::small_button(ui, self.t("clear"), TEXT()).clicked() {
                                self.persisted.settings.foobar2000_path = None;
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    let auto_retry_label = self.t("auto-retry");
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.auto_retry,
                            egui::RichText::new(auto_retry_label)
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(self.t("max-retries"))
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let attempts = self.t("attempts");
                            ui.add(
                                egui::DragValue::new(&mut self.persisted.settings.max_retries)
                                    .range(0..=10)
                                    .speed(0.5)
                                    .suffix(attempts),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    let show_new_badges_label = self.t("show-new-badges");
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.show_new_badges,
                            egui::RichText::new(show_new_badges_label)
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, self.t("auto-save-rules"), |ui| {
                    ui.label(
                        egui::RichText::new(self.t("auto-save-rules-desc"))
                            .size(11.5)
                            .color(MUTED()),
                    );
                    ui.add_space(10.0);
                    if self.persisted.save_rules.is_empty() && draft.is_none() {
                        ui.label(
                            egui::RichText::new(self.t("no-rules-yet"))
                                .size(11.5)
                                .color(DIM()),
                        );
                        ui.add_space(8.0);
                    }
                    let delete_rule_label = self.t("delete-rule");
                    let enable_rule_label = self.t("enable-rule");
                    for (index, rule) in self.persisted.save_rules.iter_mut().enumerate() {
                        Frame::none()
                            .fill(BG())
                            .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut rule.enabled, "")
                                        .on_hover_text(&enable_rule_label)
                                        .changed()
                                    {
                                        rules_changed = true;
                                    }
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(&rule.query)
                                                .size(12.0)
                                                .strong()
                                                .color(TEXT()),
                                        );
                                        let mut details = rule.field.label().to_string();
                                        if let Some(folder) = &rule.folder {
                                            details.push_str(&format!(" · folder: {folder}"));
                                        }
                                        ui.label(
                                            egui::RichText::new(details)
                                                .size(10.5)
                                                .color(DIM()),
                                        );
                                    });
                                    ui.with_layout(
                                        Layout::right_to_left(Align::Center),
                                        |ui| {
                                            if theme::icon_button(ui, Icon::Trash, 24.0, DIM())
                                                .on_hover_text(&delete_rule_label)
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
                            .fill(BG())
                            .stroke(Stroke::new(1.0_f32, accent_dim(70)))
                            .rounding(Rounding::same(9.0))
                            .inner_margin(Margin::symmetric(10.0, 9.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(self.t("match-text"))
                                        .size(9.5)
                                        .strong()
                                        .color(DIM()),
                                );
                                ui.add_space(3.0);
                                ui.add_sized(
                                    [ui.available_width().min(360.0), 30.0],
                                    TextEdit::singleline(&mut draft.query)
                                        .hint_text(self.t("hint-artist-title-tag"))
                                        .margin(Margin::symmetric(9.0, 6.0)),
                                );
                                ui.add_space(7.0);
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(self.t("in-field"))
                                            .size(9.5)
                                            .strong()
                                            .color(DIM()),
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
                                        egui::RichText::new(self.t("folder-optional"))
                                            .size(9.5)
                                            .strong()
                                            .color(DIM()),
                                    );
                                    ui.add_sized(
                                        [180.0, 28.0],
                                        TextEdit::singleline(&mut draft.folder)
                                            .hint_text(self.t("optional"))
                                            .margin(Margin::symmetric(9.0, 5.0)),
                                    );
                                    if theme::primary_button_w(ui, self.t("add-rule"), 82.0).clicked() {
                                        commit_rule = true;
                                    }
                                    if theme::small_button(ui, self.t("cancel"), DIM()).clicked() {
                                        cancel_rule = true;
                                    }
                                });
                            });
                    } else if theme::small_button(ui, self.t("add-rule"), TEXT()).clicked() {
                        add_rule = true;
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, "Automation", |ui| {
                    let delete_source = "Delete source archive after extraction";
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.delete_source_after_extract,
                            egui::RichText::new(delete_source).size(12.5),
                        )
                        .on_hover_text("Removes the .zip after a successful extraction. The extracted folder is kept.")
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(4.0);
                    let resume = "Resume interrupted downloads on launch";
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.resume_on_start,
                            egui::RichText::new(resume).size(12.5),
                        )
                        .on_hover_text("Continue paused or interrupted downloads when the app starts.")
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Speed limit")
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(if self.persisted.settings.speed_limit_kbps == 0 {
                                    "Unlimited".to_string()
                                } else {
                                    format!("{} KB/s", self.persisted.settings.speed_limit_kbps)
                                })
                                .size(12.0)
                                .strong()
                                .color(accent()),
                            );
                            if ui
                                .add_sized(
                                    [190.0, 20.0],
                                    egui::Slider::new(
                                        &mut self.persisted.settings.speed_limit_kbps,
                                        0..=50_000,
                                    )
                                    .step_by(250.0)
                                    .show_value(false),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(10.0);
                    let scheduler = "Scheduled downloads";
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.scheduler_enabled,
                            egui::RichText::new(scheduler).size(12.5),
                        )
                        .on_hover_text("Only start queued downloads inside this time window.")
                        .changed()
                    {
                        changed = true;
                    }
                    if self.persisted.settings.scheduler_enabled {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Window")
                                    .size(11.5)
                                    .color(MUTED()),
                            );
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.persisted.settings.schedule_start_hour,
                                )
                                .range(0..=23)
                                .suffix(":00"),
                            );
                            ui.label(egui::RichText::new("→").size(11.5).color(DIM()));
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.persisted.settings.schedule_end_hour,
                                )
                                .range(0..=23)
                                .suffix(":00"),
                            );
                            ui.label(
                                egui::RichText::new(if self.downloads.within_schedule() {
                                    "Active now"
                                } else {
                                    "Waiting for the window"
                                })
                                .size(10.5)
                                .color(DIM()),
                            );
                        });
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, self.t("appearance"), |ui| {
                    ui.label(
                        egui::RichText::new(self.t("accent-color"))
                            .size(12.0)
                            .color(MUTED()),
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
                                theme::configure(
                                    ctx,
                                    index,
                                    self.persisted.settings.light_theme,
                                );
                                changed = true;
                            }
                            response.on_hover_text(*name);
                        }
                    });
                    ui.add_space(16.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.light_theme,
                            egui::RichText::new("Light theme (Ctrl+Alt+L)").size(12.5),
                        )
                        .changed()
                    {
                        theme::configure(
                            ctx,
                            self.persisted.settings.accent,
                            self.persisted.settings.light_theme,
                        );
                        changed = true;
                    }
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(self.t("thumbnail-budget"))
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {}",
                                    self.persisted.settings.texture_budget,
                                    self.t("textures")
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
                                        60..=400,
                                    )
                                    .step_by(20.0)
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
                            &mut self.persisted.settings.compact_grid,
                            egui::RichText::new("Compact grid (covers only)").size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .checkbox(
                                &mut self.persisted.settings.library_show_year,
                                egui::RichText::new("Show year in library").size(12.0),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if ui
                            .checkbox(
                                &mut self.persisted.settings.library_show_tags,
                                egui::RichText::new("Show tags in library").size(12.0),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("UI scale")
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:.0}%",
                                    self.persisted.settings.ui_scale * 100.0
                                ))
                                .size(12.0)
                                .strong()
                                .color(accent()),
                            );
                            let slider = ui.add_sized(
                                [190.0, 20.0],
                                egui::Slider::new(&mut self.persisted.settings.ui_scale, 0.75..=1.75)
                                    .step_by(0.05)
                                    .show_value(false),
                            );
                            if slider.drag_started() {
                                self.scale_dragging = true;
                            }
                            if slider.drag_stopped() {
                                self.scale_dragging = false;
                            }
                            if slider.changed() {
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("Visual effects")
                            .size(12.0)
                            .color(MUTED()),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        for mode in EffectsQuality::ALL {
                            if theme::chip(
                                ui,
                                mode.label(),
                                self.persisted.settings.effects == mode,
                            )
                            .clicked()
                            {
                                self.persisted.settings.effects = mode;
                                changed = true;
                            }
                        }
                    });
                    ui.add_space(6.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.reduce_motion,
                            egui::RichText::new("Reduce motion").size(12.5),
                        )
                        .on_hover_text(
                            "Disables shimmer, aurora, the playing equaliser and confetti.",
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.add_space(14.0);
                let notifications_tray_title = self.t("notifications-tray");
                settings_card(ui, notifications_tray_title, |ui| {
                    let notify_label = self.t("notify-on-complete");
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.notify_on_complete,
                            egui::RichText::new(notify_label)
                                .size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(6.0);
                    let tray_label = self.t("show-tray-icon");
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.show_tray_icon,
                            egui::RichText::new(tray_label).size(12.5),
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
                    let close_to_tray_label = self.t("close-to-tray");
                    if ui
                        .add_enabled(
                            tray_available,
                            egui::Checkbox::new(
                                &mut self.persisted.settings.close_to_tray,
                                egui::RichText::new(close_to_tray_label).size(12.5),
                            ),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("RSS feeds")
                            .size(12.5)
                            .strong()
                            .color(TEXT()),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Paste RSS or Atom feed URLs. New entries are announced in the app.",
                        )
                        .size(10.5)
                        .color(DIM()),
                    );
                    ui.add_space(6.0);
                    let mut remove_feed: Option<String> = None;
                    for feed in self.persisted.settings.rss_feeds.clone() {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(ellipsize(&feed, 52))
                                    .size(10.8)
                                    .color(MUTED()),
                            );
                            if theme::icon_button(ui, Icon::Close, 20.0, DIM())
                                .on_hover_text("Remove feed")
                                .clicked()
                            {
                                remove_feed = Some(feed.clone());
                            }
                        });
                    }
                    if let Some(feed) = remove_feed {
                        self.persisted.settings.rss_feeds.retain(|item| item != &feed);
                        changed = true;
                    }
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 110.0).max(80.0);
                        ui.add_sized(
                            [width, 26.0],
                            TextEdit::singleline(&mut self.rss_draft)
                                .hint_text("https://example.com/feed.xml"),
                        );
                        if theme::secondary_button(ui, "Add").clicked() {
                            let feed = self.rss_draft.trim().to_string();
                            if feed.starts_with("http")
                                && !self.persisted.settings.rss_feeds.contains(&feed)
                            {
                                self.persisted.settings.rss_feeds.push(feed);
                                self.rss_draft.clear();
                                changed = true;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, "Check feeds now").clicked() {
                            self.check_rss_feeds();
                            self.toast("Checking RSS feeds…", ToastTone::Info);
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} feed{}",
                                self.persisted.settings.rss_feeds.len(),
                                if self.persisted.settings.rss_feeds.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                }
                            ))
                            .size(10.0)
                            .color(DIM()),
                        );
                    });
                });

                ui.add_space(14.0);
                settings_card(ui, self.t("language"), |ui| {
                    ui.label(
                        egui::RichText::new(self.t("interface-language"))
                            .size(12.0)
                            .color(MUTED()),
                    );
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        for lang in Language::all() {
                            let selected = self.i18n.language() == lang;
                            let label = format!("{} {}", lang.flag(), lang.display_name());
                            let resp = ui.selectable_label(selected, label);
                            if resp.clicked() {
                                self.set_language(lang);
                                changed = true;
                            }
                        }
                    });
                });

                ui.add_space(14.0);
                settings_card(ui, self.t("storage"), |ui| {
                    // The size is refreshed in `update()` every 20 seconds; do not
                    // walk the cache directory on every frame.
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(self.t("image-cache"))
                                    .size(12.5)
                                    .strong()
                                    .color(TEXT()),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {}",
                                    self.dl_ui.cache_text,
                                    self.t("stored-on-disk")
                                ))
                                .size(11.5)
                                .color(MUTED()),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::secondary_button(ui, self.t("clear-cache")).clicked() {
                                store::clear_cache();
                                self.textures = Textures::default();
                                self.images.clear_failed();
                                self.images.reset_requests();
                                self.dl_ui.cache_bytes = store::cache_size();
                                self.dl_ui.cache_text = store::human_size(self.dl_ui.cache_bytes);
                                self.toast(self.t("cache-cleared"), ToastTone::Success);
                            }
                            if theme::secondary_button(ui, self.t("open-data-folder")).clicked() {
                                store::open_path(&store::data_dir());
                            }
                        });
                    });
                    ui.add_space(14.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.auto_backup,
                            egui::RichText::new("Automatic database backup (daily, keep 10)").size(12.5),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let last = store::last_backup()
                            .and_then(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().to_string())
                            })
                            .unwrap_or_else(|| "No backup yet".to_string());
                        ui.label(
                            egui::RichText::new(format!("Last backup: {last}"))
                                .size(10.5)
                                .color(DIM()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::secondary_button(ui, "Back up now").clicked() {
                                store::backup_state();
                                self.toast("Database backup created.", ToastTone::Success);
                            }
                            if theme::secondary_button(ui, "Open backups").clicked() {
                                let dir = store::data_dir().join("backups");
                                let _ = std::fs::create_dir_all(&dir);
                                store::open_path(&dir);
                            }
                        });
                    });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Library cleanup")
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::secondary_button(ui, "Open library").clicked() {
                                self.set_view(View::Library);
                            }
                            if theme::secondary_button(ui, "Remove missing entries").clicked() {
                                let before = self.persisted.library.len();
                                self.persisted.library.retain(|entry| !entry.missing());
                                let removed = before - self.persisted.library.len();
                                if removed > 0 {
                                    self.library_rev = self.library_rev.wrapping_add(1);
                                }
                                self.dirty = true;
                                self.toast(
                                    format!(
                                        "Removed {removed} missing library entr{}.",
                                        if removed == 1 { "y" } else { "ies" }
                                    ),
                                    ToastTone::Info,
                                );
                            }
                        });
                    });
                });

                ui.add_space(14.0);
                settings_card(ui, "Playback", |ui| {
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.audio_enabled,
                            egui::RichText::new("Built-in audio preview player").size(12.5),
                        )
                        .changed()
                    {
                        self.player
                            .set_enabled(self.persisted.settings.audio_enabled);
                        changed = true;
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Volume")
                                .size(12.0)
                                .color(MUTED()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:.0}%",
                                    self.persisted.settings.audio_volume * 100.0
                                ))
                                .size(12.0)
                                .strong()
                                .color(accent()),
                            );
                            if ui
                                .add_sized(
                                    [190.0, 20.0],
                                    egui::Slider::new(
                                        &mut self.persisted.settings.audio_volume,
                                        0.0..=1.0,
                                    )
                                    .show_value(false),
                                )
                                .changed()
                            {
                                self.player
                                    .set_volume(self.persisted.settings.audio_volume);
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Space: play/pause | Ctrl+Left/Right: previous/next | Ctrl+Shift+Left/Right: seek 10 s | Ctrl+Up/Down: volume | Ctrl+Shift+=/-: UI scale",
                        )
                        .size(10.5)
                        .color(DIM()),
                    );
                    ui.add_space(6.0);
                    if ui
                        .checkbox(
                            &mut self.persisted.settings.remember_position,
                            egui::RichText::new("Remember playback position and resume on launch")
                                .size(12.0),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.add_space(14.0);
                settings_card(ui, "Integrations", |ui| {
                    ui.label(
                        egui::RichText::new("MPD server")
                            .size(12.0)
                            .color(MUTED()),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [180.0, 26.0],
                            TextEdit::singleline(&mut self.persisted.settings.mpd_host)
                                .hint_text("hostname (empty = disabled)"),
                        );
                        ui.label("Port");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.persisted.settings.mpd_port)
                                    .range(1..=65_535),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        if theme::secondary_button(ui, "Test connection").clicked() {
                            let host = self.persisted.settings.mpd_host.trim().to_string();
                            if host.is_empty() {
                                self.toast("Enter an MPD host first.", ToastTone::Error);
                            } else {
                                match mpd_command(
                                    &host,
                                    self.persisted.settings.mpd_port,
                                    "status",
                                ) {
                                    Ok(_) => self.toast("MPD connection works.", ToastTone::Success),
                                    Err(error) => {
                                        self.toast(format!("MPD: {error}"), ToastTone::Error)
                                    }
                                }
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Use the MPD button in your library to add a folder to the player queue.",
                        )
                        .size(10.5)
                        .color(DIM()),
                    );
                });

                ui.add_space(14.0);
                settings_card(ui, self.t("import-export"), |ui| {
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, self.t("export-all")).clicked() {
                            self.export_all();
                        }
                        if theme::secondary_button(ui, self.t("import")).clicked() {
                            self.import_all();
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(self.t("import-export-desc"))
                            .size(10.5)
                            .color(DIM()),
                    );
                });

                ui.add_space(14.0);
                settings_card(ui, self.t("about"), |ui| {
                    ui.label(
                        egui::RichText::new("DST-Explorer")
                            .size(15.0)
                            .strong()
                            .color(TEXT()),
                    );
                    ui.label(
                        egui::RichText::new(format!("Version {} · {}", env!("CARGO_PKG_VERSION"), self.t("desktop-client")))
                            .size(11.5)
                            .color(MUTED()),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(self.t("about-desc"))
                            .size(11.5)
                            .color(MUTED()),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, self.t("open-doujinstyle")).clicked() {
                            let _ = webbrowser::open("https://doujinstyle.com");
                        }
                        if theme::secondary_button(ui, self.t("show-shortcuts")).clicked() {
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
                &self.persisted.settings.download_dir,
                self.persisted.settings.auto_extract,
                self.persisted.settings.auto_retry,
                self.persisted.settings.max_retries,
                self.persisted.settings.delete_source_after_extract,
                self.persisted.settings.speed_limit_kbps,
                self.persisted.settings.scheduler_enabled,
                self.persisted.settings.schedule_start_hour,
                self.persisted.settings.schedule_end_hour,
            );
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|toast| {
            let ttl = if toast.action.as_ref().is_some_and(|action| action.is_undo()) {
                8
            } else {
                5
            };
            toast.created.elapsed() < Duration::from_secs(ttl)
        });
        if self.toasts.is_empty() {
            return;
        }
        let toasts: Vec<(String, ToastTone, Option<ToastAction>, f32)> = self
            .toasts
            .iter()
            .map(|toast| {
                let age = toast.created.elapsed().as_secs_f32();
                let ttl = if toast.action.as_ref().is_some_and(|action| action.is_undo()) {
                    8.0
                } else {
                    5.0
                };
                let fade_in = (age / 0.18).min(1.0);
                let fade_out = ((ttl - age) / 0.35).clamp(0.0, 1.0);
                (
                    toast.text.clone(),
                    toast.tone,
                    toast.action.clone(),
                    fade_in.min(fade_out),
                )
            })
            .collect();
        let mut pending: Option<ToastAction> = None;
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, [-24.0, -92.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(360.0);
                for (text, tone, action, fade) in toasts.into_iter().rev() {
                    let (dot, frame_stroke) = match tone {
                        ToastTone::Info => (accent(), accent_dim(55)),
                        ToastTone::Success => (SUCCESS(), Color32::from_rgb(35, 67, 52)),
                        ToastTone::Error => (DANGER(), Color32::from_rgb(76, 36, 44)),
                    };
                    ui.set_opacity(fade);
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
                                ui.label(egui::RichText::new(text).size(12.0).color(TEXT()));
                                if let Some(action) = action {
                                    let label = action.label();
                                    if theme::small_button(ui, label, accent()).clicked() {
                                        pending = Some(action);
                                    }
                                }
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        if let Some(action) = pending {
            match action {
                ToastAction::OpenFolder(path) => store::open_path(&path),
                ToastAction::OpenUrl(url) => {
                    let _ = webbrowser::open(&url);
                }
                ToastAction::RestoreWatch(album, meta) => {
                    if !self
                        .persisted
                        .watchlist
                        .iter()
                        .any(|existing| existing.id == album.id)
                    {
                        self.persisted.watchlist.insert(0, album.clone());
                    }
                    if let Some(meta) = meta {
                        self.persisted.watch_meta.insert(album.id, meta);
                    }
                    self.dirty = true;
                    self.toast(
                        format!("Restored “{}” to your watchlist", album.title),
                        ToastTone::Success,
                    );
                }
                ToastAction::RestoreLibrary(entry) => {
                    if !self
                        .persisted
                        .library
                        .iter()
                        .any(|existing| existing.album.id == entry.album.id)
                    {
                        self.persisted.library.insert(0, entry.clone());
                        self.library_rev = self.library_rev.wrapping_add(1);
                    }
                    self.dirty = true;
                    self.toast(
                        format!("Restored “{}” to your library", entry.album.title),
                        ToastTone::Success,
                    );
                }
                ToastAction::RestoreSearchHistory(entries) => {
                    self.persisted.search_history = entries;
                    self.dirty = true;
                    self.toast("Recent searches restored.", ToastTone::Success);
                }
            }
        }
        if window_focused(ctx) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn draw_confirm_bulk(&mut self, ctx: &egui::Context) {
        let Some(albums) = self.pending_bulk.clone() else {
            return;
        };
        let count = albums.len();
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Confirm bulk download")
            .id(Id::new("confirm-bulk"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .title_bar(false)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                ui.label(
                    egui::RichText::new(format!("Download {count} albums?"))
                        .size(15.0)
                        .strong()
                        .color(TEXT()),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "They will all be added to the download queue. Large batches can take a long time and a lot of disk space.",
                    )
                    .size(11.5)
                    .color(MUTED()),
                );
                ui.add_space(10.0);
                for album in albums.iter().take(6) {
                    ui.label(
                        egui::RichText::new(format!("• {}", album.title))
                            .size(11.0)
                            .color(DIM()),
                    );
                }
                if count > 6 {
                    ui.label(
                        egui::RichText::new(format!("…and {} more", count - 6))
                            .size(11.0)
                            .color(DIM()),
                    );
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, &format!("Download {count}")).clicked() {
                        confirm = true;
                    }
                    if theme::small_button(ui, "Cancel", MUTED()).clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.pending_bulk = None;
            self.enqueue_bulk(albums);
        } else if cancel {
            self.pending_bulk = None;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.pending_bulk = None;
        }
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
                                .color(TEXT()),
                        )
                        .truncate(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, 28.0, MUTED()).clicked() {
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
                    paint_cover_placeholder(
                        ui.painter(),
                        rect,
                        &album.title,
                        ui.input(|i| i.time) as f32,
                        self.motion_enabled(),
                    );
                    ui.label(
                        egui::RichText::new("Loading cover…")
                            .size(11.0)
                            .color(MUTED()),
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
                    .fill(PANEL())
                    .stroke(Stroke::new(1.0_f32, BORDER()))
                    .rounding(Rounding::same(12.0))
                    .inner_margin(Margin::same(16.0)),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Command palette")
                        .size(12.0)
                        .strong()
                        .color(DIM()),
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
                    self.palette_selected =
                        (self.palette_selected + 1) % self.palette_actions.len();
                }
                if cycle_up && !self.palette_actions.is_empty() {
                    self.palette_selected = (self.palette_selected + self.palette_actions.len()
                        - 1)
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
                                Color32::from_rgba_unmultiplied(
                                    accent().r(),
                                    accent().g(),
                                    accent().b(),
                                    20,
                                )
                            } else if response.hovered() || ui.rect_contains_pointer(ui.min_rect())
                            {
                                CARD_HOVER()
                            } else {
                                CARD()
                            };
                            let (rect, item_response) = ui.allocate_exact_size(
                                vec2(panel_width - 32.0, 44.0),
                                Sense::click(),
                            );
                            ui.painter().rect_filled(rect, Rounding::same(8.0), bg);

                            ui.painter().text(
                                pos2(rect.left() + 16.0, rect.center().y - 8.0),
                                Align2::LEFT_CENTER,
                                title,
                                FontId::proportional(13.5),
                                TEXT(),
                            );
                            ui.painter().text(
                                pos2(rect.left() + 16.0, rect.center().y + 6.0),
                                Align2::LEFT_CENTER,
                                subtitle,
                                FontId::proportional(10.5),
                                MUTED(),
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
                                            .color(DIM()),
                                    );
                                },
                            );
                        }
                    });

                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Arrow keys to navigate · Enter to open · Esc to close")
                        .size(10.0)
                        .color(DIM()),
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
                    .fill(PANEL())
                    .stroke(Stroke::new(1.0_f32, BORDER()))
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
                        .color(DIM()),
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
                        .color(DIM()),
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
                        .color(DIM()),
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
                        .color(DIM()),
                );
                ui.add_space(4.0);
                shortcut_row(ui, "Enter", "Download album from detail panel");
                shortcut_row(ui, "Esc", "Close detail panel");
                ui.add_space(14.0);

                ui.label(
                    egui::RichText::new("Playback & theme")
                        .size(10.5)
                        .strong()
                        .color(DIM()),
                );
                ui.add_space(4.0);
                shortcut_row(ui, "Space", "Play / pause the built-in player");
                shortcut_row(ui, "Ctrl+Left / Ctrl+Right", "Previous / next track");
                shortcut_row(ui, "Ctrl+Shift+Left / Right", "Seek 10 seconds");
                shortcut_row(ui, "Ctrl+Up / Ctrl+Down", "Player volume");
                shortcut_row(ui, "Ctrl+Shift+= / Ctrl+Shift+-", "Zoom UI in / out");
                shortcut_row(ui, "Ctrl+0", "Reset UI scale");
                shortcut_row(ui, "Ctrl+Alt+L", "Toggle light / dark theme");
                shortcut_row(ui, "Ctrl+Alt+T", "Cycle accent colour");

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
    }
    fn draw_fps_overlay(&self, ctx: &egui::Context) {
        egui::Area::new(Id::new("fps-overlay"))
            .anchor(Align2::RIGHT_TOP, vec2(-10.0, 10.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                Frame::none()
                    .fill(Color32::from_black_alpha(165))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{:.0} FPS · {:.1} ms",
                                self.fps_value, self.fps_frame_ms
                            ))
                            .font(FontId::monospace(10.0))
                            .color(Color32::from_rgb(170, 240, 190)),
                        );
                    });
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let now = Instant::now();
        self.anim_dt = (now - self.last_frame).as_secs_f32().clamp(0.0, 0.1);
        self.last_frame = now;
        if self.fps_enabled {
            self.fps_frames += 1;
            let elapsed = self.fps_since.elapsed().as_secs_f32();
            if elapsed >= 0.5 {
                self.fps_value = self.fps_frames as f32 / elapsed;
                self.fps_frame_ms = 1000.0 / self.fps_value.max(0.1);
                self.fps_frames = 0;
                self.fps_since = Instant::now();
                eprintln!("fps: {:.1} ({:.1} ms)", self.fps_value, self.fps_frame_ms);
            }
        }
        if let Some(text) = self.pending_clipboard.take() {
            ctx.copy_text(text);
            self.toast("Copied to clipboard.", ToastTone::Success);
        }
        // Applying zoom while the slider is being dragged moves the widget under
        // the pointer, which made the value oscillate. Apply it once on release.
        if !self.scale_dragging {
            ctx.set_zoom_factor(self.persisted.settings.ui_scale.clamp(0.75, 1.75));
        }
        self.ensure_tray(ctx);
        self.handle_tray_actions(ctx);
        if self.tray.is_some() && self.tray_tooltip_at.elapsed() > Duration::from_secs(3) {
            let mut text = match (self.player.track_title(), self.player.state()) {
                (Some(title), PlaybackState::Playing) => format!("▶ {title} — DST-Explorer"),
                (Some(title), _) => format!("❚❚ {title} — DST-Explorer"),
                _ => "DST-Explorer".to_string(),
            };
            let active = self.downloads.active_count();
            if active > 0 {
                text.push_str(&format!(
                    " · {active} download{}",
                    if active == 1 { "" } else { "s" }
                ));
            }
            if let Some(tray) = &self.tray {
                tray.set_tooltip(&text);
            }
            self.tray_tooltip_at = Instant::now();
        }
        self.handle_close_request(ctx);
        self.textures.begin_frame();
        let loaded = self.images.pump(ctx, &mut self.textures, 10);
        if loaded > 0 {
            ctx.request_repaint();
        }
        self.downloads.configure(
            self.persisted.settings.max_concurrent,
            &self.persisted.settings.download_dir,
            self.persisted.settings.auto_extract,
            self.persisted.settings.auto_retry,
            self.persisted.settings.max_retries,
            self.persisted.settings.delete_source_after_extract,
            self.persisted.settings.speed_limit_kbps,
            self.persisted.settings.scheduler_enabled,
            self.persisted.settings.schedule_start_hour,
            self.persisted.settings.schedule_end_hour,
        );
        let notices = self.downloads.poll();
        self.handle_download_notices(notices);

        // Update Windows taskbar progress
        #[cfg(all(windows, feature = "taskbar-progress"))]
        {
            use crate::downloads::DownloadState;
            use crate::taskbar::{
                clear_taskbar, set_taskbar_error, set_taskbar_paused, set_taskbar_progress,
            };
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};

            let mut total_downloaded: u64 = 0;
            let mut total_size: u64 = 0;
            let mut has_active = false;
            let mut has_error = false;
            let mut has_paused = false;

            for task in self.downloads.tasks.iter() {
                match task.state {
                    DownloadState::Downloading
                    | DownloadState::Resolving
                    | DownloadState::Extracting
                    | DownloadState::Retrying { .. } => {
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
        if !self.rss_checked {
            self.rss_checked = true;
            self.check_rss_feeds();
        }

        self.draw_sidebar(ctx);
        self.draw_detail_panel(ctx);
        self.draw_player_bar(ctx);
        self.draw_selection_bar(ctx);
        self.draw_central(ctx);
        self.draw_confirm_bulk(ctx);
        self.draw_preview(ctx);
        self.draw_command_palette(ctx);
        self.draw_shortcuts_overlay(ctx);
        self.draw_toasts(ctx);
        self.draw_confetti(ctx);

        self.images.pump(ctx, &mut self.textures, 4);
        self.textures.trim(
            self.persisted.settings.texture_budget,
            crate::images::TEXTURE_BYTE_BUDGET,
        );

        // Remember the last played track so it can be resumed on the next launch.
        if self.persisted.settings.remember_position
            && self.resume_saved.elapsed() > Duration::from_secs(4)
        {
            if let (Some(folder), Some(file)) = (
                self.player.queue_folder().map(Path::to_path_buf),
                self.player.current_path().map(Path::to_path_buf),
            ) {
                self.persisted.settings.last_track_folder = Some(folder);
                self.persisted.settings.last_track_path = Some(file);
                self.persisted.settings.last_track_position = self.player.position();
                self.dirty = true;
                self.resume_saved = Instant::now();
            }
        }

        if self.dirty && self.last_state_save.elapsed() > Duration::from_millis(700) {
            store::save_state(&self.persisted);
            self.dirty = false;
            self.last_state_save = Instant::now();
        }
        if self.dl_ui.cache_checked.elapsed() > Duration::from_secs(20) {
            let bytes = store::cache_size();
            if bytes != self.dl_ui.cache_bytes {
                self.dl_ui.cache_bytes = bytes;
                self.dl_ui.cache_text = store::human_size(bytes);
            }
            self.dl_ui.cache_checked = Instant::now();
        }

        // Notify when all downloads complete
        let has_active = self.downloads.running_count() > 0;
        if self.had_active_downloads && !has_active {
            if self.persisted.settings.notify_on_complete {
                self.toast("All downloads complete", ToastTone::Success);
                self.desktop_notify(
                    "All downloads complete",
                    "All queued downloads have finished",
                );
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
        if self.fps_enabled {
            self.draw_fps_overlay(ctx);
            ctx.request_repaint_after(Duration::from_secs(1));
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

fn mpd_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Send a single command to an MPD server and collect its response lines.
fn mpd_command(host: &str, port: u16, command: &str) -> Result<Vec<String>, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {host}: {error}"))?;
    let mut stream = None;
    for addr in addrs {
        if let Ok(candidate) = TcpStream::connect_timeout(&addr, Duration::from_secs(4)) {
            stream = Some(candidate);
            break;
        }
    }
    let mut stream = stream.ok_or_else(|| format!("cannot connect to {host}:{port}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?; // greeting: OK MPD <version>

    stream
        .write_all(command.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|error| error.to_string())?;
    stream.flush().ok();

    let mut lines = Vec::new();
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?
            == 0
        {
            break;
        }
        let trimmed = line.trim_end().to_string();
        if let Some(ack) = trimmed.strip_prefix("ACK ") {
            return Err(ack.to_string());
        }
        if trimmed == "OK" {
            break;
        }
        lines.push(trimmed);
    }
    let _ = stream.write_all(b"close\n");
    Ok(lines)
}

fn parse_feed_items(text: &str) -> Vec<(String, String)> {
    let Ok(block_re) =
        regex::Regex::new(r"(?is)<item\b[^>]*>(.*?)</item>|<entry\b[^>]*>(.*?)</entry>")
    else {
        return Vec::new();
    };
    let Ok(title_re) = regex::Regex::new(r"(?is)<title\b[^>]*>(.*?)</title>") else {
        return Vec::new();
    };
    let Ok(link_re) = regex::Regex::new(r#"(?is)<link\b[^>]*href\s*=\s*"([^"]+)""#) else {
        return Vec::new();
    };
    let Ok(plain_link_re) = regex::Regex::new(r"(?is)<link\b[^>]*>(.*?)</link>") else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for captures in block_re.captures_iter(text) {
        let block = captures
            .get(1)
            .or_else(|| captures.get(2))
            .map(|m| m.as_str())
            .unwrap_or_default();
        let title = title_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| decode_entities(m.as_str().trim()))
            .unwrap_or_default();
        let link = link_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .or_else(|| {
                plain_link_re
                    .captures(block)
                    .and_then(|c| c.get(1))
                    .map(|m| decode_entities(m.as_str().trim()))
            })
            .unwrap_or_default();
        if !link.is_empty() {
            items.push((title, link));
        }
    }
    items
}

fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
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
        .fill(BG())
        .stroke(Stroke::new(1.0_f32, BORDER()))
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
                theme::paint_icon(ui.painter(), rect, Icon::Search, DIM(), 1.5);
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
    eyebrow: impl Into<String>,
    title: impl Into<String>,
    subtitle: impl Into<String>,
    actions: impl FnOnce(&mut Ui),
) {
    let eyebrow = eyebrow.into();
    let title = title.into();
    let subtitle = subtitle.into();
    if ui.available_width() < 560.0 {
        view_heading(ui, &eyebrow, &title, &subtitle);
        ui.add_space(10.0);
        ui.horizontal_wrapped(actions);
    } else {
        ui.horizontal(|ui| {
            view_heading(ui, &eyebrow, &title, &subtitle);
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
                    .color(TEXT()),
            )
            .wrap(),
        );
        if !subtitle.is_empty() {
            ui.label(egui::RichText::new(subtitle).size(12.0).color(MUTED()));
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
            ui.label(egui::RichText::new(text).size(12.5).color(MUTED()));
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
                        .color(TEXT()),
                )
                .wrap()
                .halign(Align::Center),
            );
            ui.add_space(5.0);
            ui.add(
                egui::Label::new(egui::RichText::new(body).size(12.0).color(MUTED()))
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
    label: impl Into<String>,
    badge: Option<usize>,
    compact: bool,
) -> bool {
    let label = label.into();
    let width = ui.available_width();
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
    let hovered = response.hovered() || ui.rect_contains_pointer(rect);
    if selected {
        ui.painter()
            .rect_filled(rect, Rounding::same(9.0), accent_dim(28));
        let bar = Rect::from_min_size(pos2(rect.left(), rect.center().y - 9.0), vec2(3.0, 18.0));
        ui.painter().rect_filled(bar, Rounding::same(2.0), accent());
    } else if hovered {
        ui.painter().rect_filled(rect, Rounding::same(9.0), CARD());
    }
    let color = if selected {
        accent()
    } else if hovered {
        TEXT()
    } else {
        MUTED()
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
                    if selected { accent() } else { MUTED() },
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
                        MUTED()
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
                    if selected { accent() } else { CARD_HOVER() },
                );
                ui.painter().galley(
                    badge_rect.center() - galley.size() * 0.5,
                    galley,
                    if selected {
                        Color32::from_rgb(14, 15, 21)
                    } else {
                        MUTED()
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
        anim: f32,
    ) {
        let compact_screen = ctx.screen_rect().width() < 960.0;
        let compact = compact_screen || anim > 0.5;
        let width = 218.0 + (72.0 - 218.0) * anim;
        let margin = 14.0 + (10.0 - 14.0) * anim;
        let header_top = 22.0 + (18.0 - 22.0) * anim;
        egui::SidePanel::left("sidebar")
            .exact_width(width)
            .resizable(false)
            .frame(Frame::none().fill(SIDEBAR()).inner_margin(Margin {
                left: margin,
                right: margin,
                top: header_top,
                bottom: 18.0,
            }))
            .show(ctx, |ui| {
                if compact {
                    ui.vertical_centered(|ui| {
                        app_logo(ui, 36.0);
                        if !compact_screen {
                            if theme::icon_button(ui, Icon::Menu, 28.0, DIM())
                                .on_hover_text("Expand sidebar")
                                .clicked()
                            {
                                app.persisted.settings.sidebar_collapsed = false;
                                app.dirty = true;
                            }
                        }
                    });
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
                                    .color(TEXT()),
                            );
                            ui.label(
                                egui::RichText::new("EXPLORER")
                                    .size(9.0)
                                    .strong()
                                    .color(accent()),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::icon_button(ui, Icon::Menu, 28.0, DIM())
                                .on_hover_text("Collapse sidebar")
                                .clicked()
                            {
                                app.persisted.settings.sidebar_collapsed = true;
                                app.dirty = true;
                            }
                        });
                    });
                }
                ui.add_space(if compact { 22.0 } else { 28.0 });
                if !compact {
                    ui.label(
                        egui::RichText::new(app.t("browse-section"))
                            .size(9.5)
                            .strong()
                            .color(DIM()),
                    );
                    ui.add_space(7.0);
                }
                let items = [
                    (View::Discover, Icon::Compass, app.t("nav-discover"), None),
                    (View::Search, Icon::Search, app.t("nav-search"), None),
                    (
                        View::Watchlist,
                        Icon::Star,
                        app.t("nav-watchlist"),
                        Some(app.persisted.watchlist.len()),
                    ),
                    (
                        View::Library,
                        Icon::Folder,
                        app.t("nav-library"),
                        Some(app.persisted.library.len()),
                    ),
                    (
                        View::Downloads,
                        Icon::Download,
                        app.t("nav-downloads"),
                        Some(active),
                    ),
                ];
                for (view, icon, label, badge) in items {
                    if nav_row(ui, app.view == view, icon, label, badge, compact) {
                        *navigate = Some(view);
                    }
                    ui.add_space(2.0);
                }

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if compact {
                        let cache = theme::icon_button(ui, Icon::Grid, 34.0, DIM());
                        cache.on_hover_text(format!("{cache_text} image cache"));
                        ui.add_space(8.0);
                        if nav_row(
                            ui,
                            app.view == View::Settings,
                            Icon::Settings,
                            app.t("nav-settings"),
                            None,
                            true,
                        ) {
                            *navigate = Some(View::Settings);
                        }
                        return;
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "v{} · {}",
                            env!("CARGO_PKG_VERSION"),
                            app.t("desktop-client")
                        ))
                        .size(9.5)
                        .color(DIM()),
                    );
                    ui.add_space(8.0);
                    if nav_row(
                        ui,
                        app.view == View::Settings,
                        Icon::Settings,
                        app.t("nav-settings"),
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
                        theme::paint_icon(ui.painter(), rect, Icon::Grid, DIM(), 1.2);
                        ui.label(
                            egui::RichText::new(format!("{} {}", cache_text, app.t("image-cache")))
                                .size(10.0)
                                .color(DIM()),
                        );
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let current = app.i18n.language();
                        egui::ComboBox::from_id_salt("sidebar-language")
                            .selected_text(format!("{} {}", current.flag(), current.display_name()))
                            .width(168.0)
                            .show_ui(ui, |ui| {
                                for lang in Language::all() {
                                    let label = format!("{} {}", lang.flag(), lang.display_name());
                                    if ui
                                        .selectable_label(app.i18n.language() == lang, label)
                                        .clicked()
                                    {
                                        app.set_language(lang);
                                    }
                                }
                            });
                    });
                    if active > 0 {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                            ui.painter().circle_filled(rect.center(), 3.0, accent());
                            ui.label(
                                egui::RichText::new(format!(
                                    "{active} {}",
                                    app.t("downloads-active")
                                ))
                                .size(10.0)
                                .color(MUTED()),
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
    // Optional index list, used by the watchlist to draw a filtered view
    // without cloning every album.
    indices: Option<&[usize]>,
    textures: &mut Textures,
    selected: &HashSet<u64>,
    watchlist: &[Album],
    watch_meta: &HashMap<u64, WatchMeta>,
    folders: &[String],
    library: &[LibraryEntry],
    requests: &mut Vec<ImageSpec>,
    show_new: bool,
    footer: GridFooter<'_>,
    compact: bool,
    id_salt: impl std::hash::Hash,
    playing_folder: Option<&Path>,
    effects: bool,
    motion: bool,
) -> (Vec<CardAction>, bool) {
    let avail = (ui.available_width() - 16.0).max(140.0);
    let min_card = if avail < 560.0 { 140.0 } else { MIN_CARD_WIDTH };
    let columns = (((avail + CARD_GAP) / (min_card + CARD_GAP)).floor() as usize).max(1);
    let card_width = ((avail - CARD_GAP * (columns.saturating_sub(1)) as f32) / columns as f32)
        .floor()
        .max(120.0);
    let card_height = if compact {
        card_width
    } else {
        card_width + 51.0
    };
    let layout = CardLayout {
        columns,
        card_width,
        card_height,
    };
    let card_rows = indices
        .map_or(albums.len(), |ids| ids.len())
        .div_ceil(columns);
    let has_footer = !matches!(footer, GridFooter::None);
    let total_rows = card_rows + usize::from(has_footer);
    let mut actions: Vec<CardAction> = Vec::new();
    let mut clickable_footer = false;

    // Build membership sets once per frame instead of scanning the watchlist and
    // library for every visible card. With a large collection this was the most
    // expensive part of scrolling.
    let watch_ids: HashSet<u64> = watchlist.iter().map(|album| album.id).collect();
    let library_ids: HashSet<u64> = library.iter().map(|entry| entry.album.id).collect();
    let playing_ids: HashSet<u64> = playing_folder
        .map(|folder| {
            library
                .iter()
                .filter(|entry| entry.folder() == folder)
                .map(|entry| entry.album.id)
                .collect()
        })
        .unwrap_or_default();

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
                            ui.label(egui::RichText::new(text).size(11.0).color(MUTED()));
                        }
                        GridFooter::LoadMore(text) => {
                            if theme::secondary_button(ui, text).clicked() {
                                clickable_footer = true;
                            }
                        }
                        GridFooter::End(text) => {
                            ui.label(egui::RichText::new(text).size(11.0).color(DIM()));
                        }
                        GridFooter::None => {}
                    });
                    continue;
                }
                ui.horizontal(|ui| {
                    for column in 0..layout.columns {
                        let index = row * layout.columns + column;
                        let count = indices.map_or(albums.len(), |ids| ids.len());
                        if index >= count {
                            break;
                        }
                        let album = match indices {
                            Some(ids) => &albums[ids[index]],
                            None => &albums[index],
                        };
                        let watched = watch_ids.contains(&album.id);
                        let downloaded = library_ids.contains(&album.id);
                        let playing = playing_ids.contains(&album.id);
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
                            playing,
                            current_folder,
                            folders,
                            requests,
                            show_new,
                            compact,
                            effects,
                            motion,
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
    playing: bool,
    current_folder: Option<&str>,
    folders: &[String],
    requests: &mut Vec<ImageSpec>,
    show_new: bool,
    compact: bool,
    effects: bool,
    motion: bool,
) -> Option<CardAction> {
    let (card_rect, response) =
        ui.allocate_exact_size(vec2(layout.card_width, layout.card_height), Sense::click());
    // The action buttons only exist while the card is hovered. Using the response alone
    // makes them vanish as soon as the pointer is over a button, which causes flicker.
    let hovered = response.hovered() || ui.rect_contains_pointer(card_rect);
    let hover_t = ui
        .ctx()
        .animate_bool_with_time(Id::new(("card-hover", album.id)), hovered, 0.13);
    // Visual lift: only the painting moves, the interaction rect stays put so the
    // hover state cannot flicker at the card edge.
    let lift = if motion { hover_t * 2.0 } else { 0.0 };
    let rect = card_rect.translate(vec2(0.0, -lift));
    let painter = ui.painter().clone();
    let key = format!("t{}", album.id);
    let tint = textures.tint(&key);

    // Ambient glow tinted by the cover colour, painted behind the card.
    if effects && (selected || hover_t > 0.01) {
        if let Some(tint) = tint {
            let strength = if selected { 0.30 } else { 0.20 * hover_t };
            paint_soft_glow(
                &painter,
                rect.center(),
                vec2(rect.width() * 0.72, rect.height() * 0.60),
                tint,
                strength,
            );
        }
    }

    let bg = if selected {
        Color32::from_rgb(25, 24, 38)
    } else {
        lerp_color(CARD(), CARD_HOVER(), hover_t)
    };
    if hover_t > 0.01 {
        painter.rect_filled(
            rect.translate(vec2(0.0, 3.0)).expand(1.5),
            Rounding::same(14.0),
            Color32::from_black_alpha((55.0 * hover_t) as u8),
        );
    }
    painter.rect_filled(rect, Rounding::same(12.0), bg);
    if selected {
        painter.rect_stroke(rect, Rounding::same(12.0), Stroke::new(1.0_f32, accent()));
    } else if hover_t > 0.01 {
        painter.rect_stroke(
            rect,
            Rounding::same(12.0),
            Stroke::new(1.0_f32, lerp_color(Color32::TRANSPARENT, BORDER(), hover_t)),
        );
    }

    let cover = Rect::from_min_size(
        rect.min + vec2(8.0, 8.0),
        vec2(rect.width() - 16.0, rect.width() - 16.0),
    );
    let mut action = None;

    // Pointer parallax inside the cover (full effects only).
    let parallax = if motion && hover_t > 0.01 {
        ui.input(|i| i.pointer.hover_pos())
            .map(|pos| {
                let rel =
                    (pos - cover.center()) / vec2(cover.width().max(1.0), cover.height().max(1.0));
                vec2(rel.x.clamp(-0.5, 0.5), rel.y.clamp(-0.5, 0.5)) * (2.0 * hover_t)
            })
            .unwrap_or(Vec2::ZERO)
    } else {
        Vec2::ZERO
    };

    if let Some((texture, alpha)) = textures.get_with_fade(&key) {
        paint_texture_cover_fx(
            &painter,
            cover,
            texture,
            Rounding::same(9.0),
            alpha,
            parallax,
            if motion { 1.0 + 0.06 * hover_t } else { 1.0 },
        );
        if alpha < 1.0 {
            ui.ctx().request_repaint();
        }
    } else if textures.is_failed(&key) {
        // A failed image never retries, so draw a static placeholder and stop
        // requesting repaints. This used to spin the whole app at ~12 FPS.
        paint_cover_placeholder(
            &painter,
            cover,
            &album.title,
            ui.input(|i| i.time) as f32,
            false,
        );
    } else {
        paint_cover_placeholder(
            &painter,
            cover,
            &album.title,
            ui.input(|i| i.time) as f32,
            motion,
        );
        ui.ctx().request_repaint_after(Duration::from_millis(80));
        requests.push(ImageSpec {
            key: key.clone(),
            url: album.thumb_url(),
            cache_path: store::thumb_cache_path(album.id),
            max_dim: 288,
        });
    }

    // Diagonal light sweep across the cover on hover.
    if motion && hover_t > 0.01 {
        paint_light_sweep(&painter, cover, hover_t);
    }

    // Subtle hover shade over the image.
    if hover_t > 0.01 {
        painter.rect_filled(
            cover,
            Rounding::same(9.0),
            Color32::from_rgba_unmultiplied(8, 8, 12, (36.0 * hover_t) as u8),
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
        if downloaded {
            let play_rect = Rect::from_center_size(cover.center(), vec2(48.0, 48.0));
            let play_response =
                ui.interact(play_rect, ui.id().with(("play", album.id)), Sense::click());
            let lit = play_response.hovered();
            painter.circle_filled(
                play_rect.center(),
                24.0,
                if lit {
                    accent()
                } else {
                    Color32::from_black_alpha(165)
                },
            );
            theme::paint_icon(
                &painter,
                Rect::from_center_size(play_rect.center(), vec2(22.0, 22.0)),
                Icon::Play,
                if lit {
                    Color32::from_rgb(14, 15, 21)
                } else {
                    TEXT()
                },
                1.8,
            );
            if play_response.clicked() {
                action = Some(CardAction::Play(album.clone()));
            }
        }
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
                TEXT()
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
                CARD_HOVER()
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
            if watched { accent() } else { TEXT() },
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
        painter.rect_filled(owned_rect, Rounding::same(6.0), SUCCESS());
        theme::paint_icon(
            &painter,
            Rect::from_center_size(owned_rect.center(), vec2(12.0, 12.0)),
            Icon::Check,
            Color32::from_rgb(14, 15, 21),
            2.0,
        );
    }

    // Animated equaliser for the album that is currently playing.
    if playing {
        let eq_rect = Rect::from_min_size(cover.left_bottom() + vec2(8.0, -84.0), vec2(34.0, 20.0));
        painter.rect_filled(eq_rect, Rounding::same(6.0), Color32::from_black_alpha(165));
        let time = ui.input(|i| i.time) as f32;
        for bar in 0..3 {
            let level = if motion {
                (time * 5.2 + bar as f32 * 1.9).sin() * 0.5 + 0.5
            } else {
                0.7
            };
            let height = 5.0 + level * 9.0;
            let x = eq_rect.left() + 9.0 + bar as f32 * 8.0;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x, eq_rect.center().y + 5.0 - height),
                    pos2(x + 4.0, eq_rect.center().y + 5.0),
                ),
                Rounding::same(1.5),
                accent(),
            );
        }
        if motion && window_focused(ui.ctx()) {
            ui.ctx().request_repaint_after(Duration::from_millis(90));
        }
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

    if !compact {
        let text_left = rect.left() + 10.0;
        let text_width = rect.width() - 20.0;
        let title_y = cover.bottom() + 9.0;
        let title_galley = painter.layout(
            album.title.clone(),
            FontId::new(12.8, FontFamily::Proportional),
            TEXT(),
            text_width,
        );
        let title_clip = Rect::from_min_size(pos2(text_left, title_y), vec2(text_width, 34.0));
        painter
            .with_clip_rect(title_clip)
            .galley(pos2(text_left, title_y), title_galley, TEXT());
        let artist = if album.artist.trim().is_empty() {
            "Unknown artist"
        } else {
            album.artist.trim()
        };
        let artist_text = elided_galley(&painter, artist, 10.8, MUTED(), text_width);
        painter.galley(pos2(text_left, cover.bottom() + 43.0), artist_text, MUTED());
    }

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
                        action = Some(CardAction::OpenUrl(format!(
                            "{}/?p=search&type=tags&result={}",
                            crate::models::SITE,
                            percent_encoding::percent_encode(
                                tag.as_bytes(),
                                percent_encoding::NON_ALPHANUMERIC
                            )
                        )));
                        ui.close_menu();
                    }
                }
            });
        }
        if !album.artist.trim().is_empty() {
            ui.menu_button("Download by artist", |ui| {
                if ui.button(album.artist.trim()).clicked() {
                    action = Some(CardAction::OpenUrl(format!(
                        "{}/?p=search&type=artist&result={}",
                        crate::models::SITE,
                        percent_encoding::percent_encode(
                            album.artist.trim().as_bytes(),
                            percent_encoding::NON_ALPHANUMERIC
                        )
                    )));
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
        if ui.button("Copy album link").clicked() {
            ui.ctx().copy_text(album.page_url());
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

/// Lay out a single line of text, truncating it with `…` in one pass.
///
/// The old implementation binary-searched the string and re-laid it out up to
/// ten times per call, which was measurable in the player bar every frame.
fn elided_galley(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: FontId::proportional(size),
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    painter.layout_job(job)
}

fn paint_texture_cover(
    painter: &egui::Painter,
    rect: Rect,
    texture: &egui::TextureHandle,
    rounding: Rounding,
    alpha: f32,
) {
    paint_texture_cover_fx(painter, rect, texture, rounding, alpha, Vec2::ZERO, 1.0);
}

/// Draw a cover with an optional UV zoom/shift, used for the card hover parallax.
fn paint_texture_cover_fx(
    painter: &egui::Painter,
    rect: Rect,
    texture: &egui::TextureHandle,
    rounding: Rounding,
    alpha: f32,
    offset: Vec2,
    zoom: f32,
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
    let uv = if zoom > 1.0 {
        let center = uv.center();
        let half = uv.size() * 0.5 / zoom;
        let shift = vec2(offset.x * half.x * 0.20, offset.y * half.y * 0.20);
        Rect::from_center_size(center + shift, half * 2.0)
    } else {
        uv
    };
    let mut shape = egui::epaint::RectShape::filled(
        rect,
        rounding,
        Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8),
    );
    shape.fill_texture_id = texture.id();
    shape.uv = uv;
    painter.add(shape);
}

fn paint_cover_placeholder(
    painter: &egui::Painter,
    rect: Rect,
    title: &str,
    time: f32,
    shimmer: bool,
) {
    painter.rect_filled(rect, Rounding::same(9.0), Color32::from_rgb(25, 26, 37));
    // Cheap animated shimmer band: no textures, no allocation beyond a rect.
    // Failed downloads use a static placeholder so they never request repaints.
    if shimmer {
        let phase = (time * 0.45).fract();
        let band_width = rect.width() * 0.34;
        let band_x = rect.left() - band_width + phase * (rect.width() + band_width * 2.0);
        let band = Rect::from_min_size(pos2(band_x, rect.top()), vec2(band_width, rect.height()));
        painter.with_clip_rect(rect).rect_filled(
            band,
            Rounding::same(9.0),
            Color32::from_white_alpha(13),
        );
    }
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
        CARD()
    };
    Frame::none()
        .fill(frame_fill)
        .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
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
                                .color(TEXT()),
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
                            .color(MUTED()),
                        )
                        .truncate(),
                    );
                    if let Some(host) = &task.host {
                        ui.add(
                            egui::Label::new(egui::RichText::new(host).size(9.5).color(DIM()))
                                .truncate(),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let state_color = match task.state {
                        DownloadState::Done => SUCCESS(),
                        DownloadState::Failed => DANGER(),
                        DownloadState::Manual => WARNING(),
                        DownloadState::Paused => WARNING(),
                        DownloadState::Cancelled => MUTED(),
                        _ => accent(),
                    };
                    ui.label(
                        egui::RichText::new(task.state.label())
                            .size(10.5)
                            .strong()
                            .color(state_color),
                    );
                    ui.add_space(6.0);
                    if task.state == DownloadState::Queued {
                        if theme::small_button(ui, "↓", MUTED()).clicked() {
                            actions.push(TaskAction::MoveDown(task.id));
                        }
                        if theme::small_button(ui, "↑", MUTED()).clicked() {
                            actions.push(TaskAction::MoveUp(task.id));
                        }
                        if theme::small_button(ui, "First", TEXT()).clicked() {
                            actions.push(TaskAction::Prioritize(task.id));
                        }
                    }
                    if task.state.is_terminal() {
                        if theme::icon_button(ui, Icon::Trash, 26.0, DIM())
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
                    ui.label(egui::RichText::new(text).size(10.5).color(MUTED()));
                    if task.state == DownloadState::Downloading && speed > 0.5 {
                        ui.label(
                            egui::RichText::new(format!("· {}/s", store::human_size(speed as u64)))
                                .size(10.5)
                                .color(DIM()),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if task.state == DownloadState::Paused {
                            if theme::small_button(ui, "Resume", TEXT()).clicked() {
                                actions.push(TaskAction::Resume(task.id));
                            }
                        } else if theme::small_button(ui, "Pause", MUTED()).clicked() {
                            actions.push(TaskAction::Pause(task.id));
                        }
                        if theme::small_button(ui, "Cancel", MUTED()).clicked() {
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
                                            .color(DIM()),
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
                        ui.label(egui::RichText::new(file_name).size(10.8).color(MUTED()));
                        if let Some(folder) = &task.extracted {
                            ui.label(
                                egui::RichText::new("· extracted")
                                    .size(10.0)
                                    .color(SUCCESS()),
                            );
                            let _ = folder;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::small_button(ui, "Show in folder", TEXT()).clicked() {
                                actions.push(TaskAction::OpenFolder(task.id));
                            }
                            if task.extracted.as_ref().map(|p| p.exists()).unwrap_or(false)
                                || task.output.as_ref().map(|p| p.exists()).unwrap_or(false)
                            {
                                if theme::small_button(ui, "Play in foobar", TEXT()).clicked() {
                                    let path = task
                                        .extracted
                                        .clone()
                                        .unwrap_or_else(|| task.output.clone().unwrap());
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
                            DANGER()
                        } else {
                            MUTED()
                        },
                    ));
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        if task.state == DownloadState::Manual {
                            if let Some(url) = &task.manual_url {
                                if theme::small_button(ui, "Open host page", WARNING()).clicked() {
                                    actions.push(TaskAction::OpenUrl(url.clone()));
                                }
                            }
                        }
                        if theme::small_button(ui, "Retry", TEXT()).clicked() {
                            actions.push(TaskAction::Retry(task.id));
                        }
                        if task.state == DownloadState::Manual {
                            if let Some(url) = &task.manual_url {
                                if theme::small_button(ui, "Copy link", MUTED()).clicked() {
                                    ui.ctx().copy_text(url.clone());
                                }
                            }
                        }
                    });
                }
                if let Some(warning) = &task.warning {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(warning).size(10.2).color(WARNING()));
                }
            }
        });
}

fn track_time(seconds: f32) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_string();
    }
    let total = seconds.round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Draw a compact waveform with elapsed bars highlighted in the accent colour.
fn paint_waveform(painter: &egui::Painter, rect: Rect, peaks: &[f32], progress: f32) {
    painter.rect_filled(rect, Rounding::same(7.0), CARD());
    painter.rect_stroke(
        rect,
        Rounding::same(7.0),
        Stroke::new(1.0_f32, BORDER_SOFT()),
    );
    let progress = progress.clamp(0.0, 1.0);
    let play_x = rect.left() + rect.width() * progress;
    let bars = if peaks.is_empty() {
        64
    } else {
        peaks.len().min((rect.width() / 3.0).max(16.0) as usize)
    };
    let gap = 1.0f32;
    let bar_width = ((rect.width() - 8.0) / bars as f32 - gap).max(1.0);
    let mid = rect.center().y;
    let max_height = rect.height() - 10.0;
    for index in 0..bars {
        let amplitude = if peaks.is_empty() {
            0.16
        } else {
            let position = index * peaks.len() / bars;
            peaks[position.min(peaks.len() - 1)]
        };
        let height = (amplitude * max_height).max(1.5);
        let x = rect.left() + 4.0 + index as f32 * (bar_width + gap);
        let color = if x <= play_x {
            accent()
        } else {
            MUTED().gamma_multiply(0.45)
        };
        painter.rect_filled(
            Rect::from_center_size(pos2(x + bar_width * 0.5, mid), vec2(bar_width, height)),
            Rounding::same(1.0),
            color,
        );
    }
    painter.line_segment(
        [
            pos2(play_x, rect.top() + 3.0),
            pos2(play_x, rect.bottom() - 3.0),
        ],
        Stroke::new(1.5_f32, accent()),
    );
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

fn settings_card(ui: &mut Ui, title: impl Into<String>, contents: impl FnOnce(&mut Ui)) {
    ui.label(
        egui::RichText::new(title.into())
            .size(12.5)
            .strong()
            .color(TEXT()),
    );
    ui.add_space(6.0);
    Frame::none()
        .fill(PANEL())
        .stroke(Stroke::new(1.0_f32, BORDER_SOFT()))
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
    playable: Option<(PathBuf, PathBuf)>,
    requests: &mut Vec<ImageSpec>,
    effects: bool,
    motion: bool,
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
            if theme::icon_button(ui, Icon::Close, 30.0, MUTED())
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

    // Ambient glow tinted by the loaded cover colour, behind the panel content.
    if effects {
        if let Some(tint) = textures.tint(&cover_key) {
            let panel = ui.max_rect();
            paint_soft_glow(
                ui.painter(),
                pos2(panel.center().x, panel.top() + panel.width() * 0.42),
                vec2(panel.width() * 0.95, panel.width() * 0.72),
                tint,
                0.18,
            );
        }
    }

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
            if let Some((texture, alpha)) = textures.get_with_fade(&cover_key) {
                paint_texture_cover(
                    ui.painter(),
                    cover_rect,
                    texture,
                    Rounding::same(12.0),
                    alpha,
                );
                if alpha < 1.0 {
                    ui.ctx().request_repaint();
                }
            } else if textures.is_failed(&cover_key) {
                paint_cover_placeholder(
                    ui.painter(),
                    cover_rect,
                    &album.title,
                    ui.input(|i| i.time) as f32,
                    false,
                );
            } else {
                paint_cover_placeholder(
                    ui.painter(),
                    cover_rect,
                    &album.title,
                    ui.input(|i| i.time) as f32,
                    motion,
                );
                ui.ctx().request_repaint_after(Duration::from_millis(80));
                requests.push(ImageSpec {
                    key: cover_key.clone(),
                    url: album.cover_url(),
                    cache_path: store::cover_cache_path(album.id),
                    max_dim: 640,
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
                        .color(TEXT()),
                )
                .wrap(),
            );
            ui.add_space(3.0);
            if !album.artist.trim().is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(album.artist.trim())
                            .size(12.2)
                            .color(MUTED()),
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
                        theme::badge(ui, &loaded.date_added, MUTED(), CARD());
                    }
                }
                if album.is_new {
                    theme::badge(ui, "NEW", Color32::from_rgb(14, 15, 21), accent());
                }
                if in_library {
                    theme::badge(ui, "IN LIBRARY", SUCCESS(), Color32::from_rgb(20, 38, 30));
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
                    if watched { accent() } else { MUTED() },
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
                if theme::icon_button(ui, Icon::External, 36.0, MUTED())
                    .on_hover_text("Open album page in browser")
                    .clicked()
                {
                    actions.push(DetailAction::OpenUrl(album.page_url()));
                }
                if let Some((folder, source)) = &playable {
                    if theme::icon_button(ui, Icon::Play, 36.0, accent())
                        .on_hover_text("Play in DST-Explorer")
                        .clicked()
                    {
                        actions.push(DetailAction::Play {
                            folder: folder.clone(),
                            source: source.clone(),
                        });
                    }
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
                                    actions.push(DetailAction::Search {
                                        text: tag.clone(),
                                        kind: crate::models::SearchKind::Tags,
                                    });
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if !album.artist.trim().is_empty() {
                        ui.menu_button("Download by artist", |ui| {
                            if ui.button(album.artist.trim()).clicked() {
                                actions.push(DetailAction::Search {
                                    text: album.artist.trim().to_string(),
                                    kind: crate::models::SearchKind::Artist,
                                });
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
                        .color(DIM()),
                );
                ui.add_space(3.0);
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Folder").size(11.0).color(MUTED()));
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
                            .color(MUTED()),
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
                        ui.label(egui::RichText::new(error).size(10.8).color(DANGER()));
                        if theme::small_button(ui, "Retry", TEXT()).clicked() {
                            actions.push(DetailAction::Retry);
                        }
                    });
            }

            if let Some(loaded) = &detail.loaded {
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("DETAILS")
                        .size(9.5)
                        .strong()
                        .color(DIM()),
                );
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
                            ui.label(egui::RichText::new("Provider").size(11.0).color(DIM()));
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
                    ui.label(
                        egui::RichText::new("ARTISTS")
                            .size(9.5)
                            .strong()
                            .color(DIM()),
                    );
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
                    ui.label(egui::RichText::new("TAGS").size(9.5).strong().color(DIM()));
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
                    .color(DIM()),
            );
            ui.add_space(20.0);
        });

    actions
}

fn meta_row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).size(11.0).color(DIM()));
    ui.label(egui::RichText::new(value).size(11.5).color(TEXT()));
    ui.end_row();
}

fn shortcut_row(ui: &mut Ui, key: &str, description: &str) {
    ui.horizontal(|ui| {
        let galley =
            ui.painter()
                .layout_no_wrap(key.to_string(), FontId::proportional(11.0), DIM());
        let key_width = galley.size().x + 16.0;
        let (rect, _) = ui.allocate_exact_size(vec2(key_width, 22.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(25, 26, 37));
        ui.painter().rect_stroke(
            rect,
            Rounding::same(4.0),
            Stroke::new(1.0_f32, BORDER_SOFT()),
        );
        ui.painter()
            .galley(rect.center() - galley.size() * 0.5, galley, MUTED());
        ui.add_space(12.0);
        ui.label(egui::RichText::new(description).size(11.5).color(TEXT()));
    });
    ui.add_space(2.0);
}

#[cfg(test)]
mod tests {
    use super::parse_feed_items;

    #[test]
    fn rss_feed_items_are_parsed() {
        let xml = r#"<?xml version="1.0"?>
        <rss><channel>
          <item><title>First &amp; best</title><link>https://example.com/1</link></item>
          <item><title>Second</title><link>https://example.com/2</link></item>
        </channel></rss>"#;
        let items = parse_feed_items(xml);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "First & best");
        assert_eq!(items[0].1, "https://example.com/1");
        assert_eq!(items[1].1, "https://example.com/2");
    }

    #[test]
    fn atom_links_with_href_are_parsed() {
        let xml = r#"<feed>
          <entry><title>Atom entry</title><link href="https://example.com/atom"/></entry>
        </feed>"#;
        let items = parse_feed_items(xml);
        assert_eq!(
            items,
            vec![(
                "Atom entry".to_string(),
                "https://example.com/atom".to_string()
            )]
        );
    }
}
