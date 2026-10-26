use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const SITE: &str = "https://doujinstyle.com";
pub const STATE_VERSION: u32 = 4;
pub const SEARCH_HISTORY_LIMIT: usize = 24;
pub const SAVED_SEARCH_LIMIT: usize = 40;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Album {
    pub id: u64,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub xfd: Option<String>,
    #[serde(default)]
    pub is_new: bool,
}

impl Album {
    pub fn thumb_url(&self) -> String {
        format!("{SITE}/thumbs/{}.jpg", self.id)
    }

    pub fn cover_url(&self) -> String {
        format!("{SITE}/covers/{}.jpg", self.id)
    }

    pub fn page_url(&self) -> String {
        format!("{SITE}/?p=page&type=1&id={}", self.id)
    }
}

#[derive(Clone, Debug)]
pub struct AlbumDetail {
    pub album: Album,
    pub artists: Vec<String>,
    pub tags: Vec<String>,
    pub xfd: Option<String>,
    pub format: String,
    pub date_added: String,
    pub download_count: String,
    pub rating: String,
    pub provider: Option<String>,
}

impl AlbumDetail {
    pub fn from_album(album: Album) -> Self {
        let artists = if album.artist.trim().is_empty() {
            Vec::new()
        } else {
            vec![album.artist.clone()]
        };
        Self {
            artists,
            tags: album.tags.clone(),
            xfd: album.xfd.clone(),
            format: album.format.clone(),
            date_added: String::new(),
            download_count: String::new(),
            rating: String::new(),
            provider: None,
            album,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Blanket,
    Artist,
    Tags,
    Credit,
}

impl Default for SearchKind {
    fn default() -> Self {
        Self::Blanket
    }
}

impl SearchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blanket => "blanket",
            Self::Artist => "artist",
            Self::Tags => "tags",
            Self::Credit => "credit",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Blanket => "Everything",
            Self::Artist => "Artist",
            Self::Tags => "Tag",
            Self::Credit => "Provider",
        }
    }

    pub const ALL: [Self; 4] = [Self::Blanket, Self::Artist, Self::Tags, Self::Credit];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatFilter {
    pub flac: bool,
    pub mp3: bool,
    pub tta: bool,
    pub opus: bool,
    pub ogg: bool,
}

impl Default for FormatFilter {
    fn default() -> Self {
        Self {
            flac: false,
            mp3: false,
            tta: false,
            opus: false,
            ogg: false,
        }
    }
}

impl FormatFilter {
    pub fn summary(&self) -> String {
        let mut out = Vec::new();
        if self.flac {
            out.push("FLAC");
        }
        if self.mp3 {
            out.push("MP3");
        }
        if self.tta {
            out.push("TTA");
        }
        if self.opus {
            out.push("Opus");
        }
        if self.ogg {
            out.push("OGG");
        }
        if out.is_empty() {
            "Any format".to_string()
        } else {
            out.join(" + ")
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    /// Keep the order returned by DoujinStyle.
    #[default]
    Default,
    /// Highest album id first (newest uploads first).
    Newest,
    Title,
    Artist,
    Format,
}

impl SearchSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Newest => "Newest",
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Format => "Format",
        }
    }

    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Newest,
        Self::Title,
        Self::Artist,
        Self::Format,
    ];
}

/// A query the user actually ran, kept so it can be replayed with one click.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchHistoryEntry {
    pub text: String,
    pub kind: SearchKind,
    pub formats: FormatFilter,
    pub at: u64,
}

/// A named query pinned by the user.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSearch {
    pub name: String,
    pub text: String,
    pub kind: SearchKind,
    pub formats: FormatFilter,
    pub saved_at: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleField {
    #[default]
    Any,
    Title,
    Artist,
    Tag,
}

impl RuleField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Any field",
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Tag => "Tag",
        }
    }

    pub const ALL: [Self; 4] = [Self::Any, Self::Title, Self::Artist, Self::Tag];
}

/// An auto-save rule applied to releases as they arrive from Discover.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveRule {
    pub query: String,
    pub field: RuleField,
    /// Optional watchlist folder for albums caught by this rule.
    pub folder: Option<String>,
    pub enabled: bool,
}

impl SaveRule {
    pub fn matches(&self, album: &Album) -> bool {
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            return false;
        }
        let title = album.title.to_lowercase();
        let artist = album.artist.to_lowercase();
        match self.field {
            RuleField::Any => {
                title.contains(&needle)
                    || artist.contains(&needle)
                    || album
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&needle))
            }
            RuleField::Title => title.contains(&needle),
            RuleField::Artist => artist.contains(&needle),
            RuleField::Tag => album
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(&needle)),
        }
    }
}

/// Sort a copy of the search results for display. The unsorted list is kept for
/// pagination and de-duplication, so switching sorts never drops a page.
pub fn sort_albums(albums: Vec<Album>, sort: SearchSort) -> Vec<Album> {
    let mut out = albums;
    match sort {
        SearchSort::Default => {}
        SearchSort::Newest => out.sort_by(|a, b| b.id.cmp(&a.id)),
        SearchSort::Title => out.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.artist.to_lowercase().cmp(&b.artist.to_lowercase()))
                .then_with(|| b.id.cmp(&a.id))
        }),
        SearchSort::Artist => out.sort_by(|a, b| {
            a.artist
                .to_lowercase()
                .cmp(&b.artist.to_lowercase())
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| b.id.cmp(&a.id))
        }),
        SearchSort::Format => out.sort_by(|a, b| {
            let a_format = a.format.trim().to_lowercase();
            let b_format = b.format.trim().to_lowercase();
            a_format
                .is_empty()
                .cmp(&b_format.is_empty())
                .then_with(|| a_format.cmp(&b_format))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        }),
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    pub max_concurrent: usize,
    pub auto_extract: bool,
    pub accent: usize,
    pub show_new_badges: bool,
    pub confirm_bulk: bool,
    pub texture_budget: usize,
    pub search_sort: SearchSort,
    pub notify_on_complete: bool,
    pub show_tray_icon: bool,
    pub close_to_tray: bool,
    pub foobar2000_path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("DoujinStyle");
        Self {
            download_dir,
            max_concurrent: 3,
            auto_extract: true,
            accent: 0,
            show_new_badges: true,
            confirm_bulk: false,
            texture_budget: 700,
            search_sort: SearchSort::Default,
            notify_on_complete: true,
            show_tray_icon: true,
            close_to_tray: false,
            foobar2000_path: None,
        }
    }
}

/// A finished download that belongs to the user's local collection.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryEntry {
    pub album: Album,
    /// The downloaded archive on disk.
    pub path: PathBuf,
    /// The folder created by auto-extraction, when available.
    pub extracted: Option<PathBuf>,
    pub size: u64,
    pub completed_at: u64,
    pub warning: Option<String>,
}

impl LibraryEntry {
    pub fn folder(&self) -> PathBuf {
        if let Some(extracted) = &self.extracted {
            if extracted.exists() {
                return extracted.clone();
            }
        }
        self.path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.path.clone())
    }

    pub fn missing(&self) -> bool {
        !self.path.exists()
            && self
                .extracted
                .as_ref()
                .map(|path| !path.exists())
                .unwrap_or(true)
    }
}

/// Extra watchlist data kept beside the album list, so the on-disk shape stays
/// compatible with older state files.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchMeta {
    pub folder: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    /// Schema version used for migrations. Files written by older builds have
    /// no version field and therefore deserialize as 0.
    pub version: u32,
    pub settings: Settings,
    pub watchlist: Vec<Album>,
    pub watch_folders: Vec<String>,
    pub watch_meta: HashMap<u64, WatchMeta>,
    pub library: Vec<LibraryEntry>,
    pub search_history: Vec<SearchHistoryEntry>,
    pub saved_searches: Vec<SavedSearch>,
    pub save_rules: Vec<SaveRule>,
    pub last_seen_id: u64,
    pub last_visit: u64,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            settings: Settings::default(),
            watchlist: Vec::new(),
            watch_folders: Vec::new(),
            watch_meta: HashMap::new(),
            library: Vec::new(),
            search_history: Vec::new(),
            saved_searches: Vec::new(),
            save_rules: Vec::new(),
            last_seen_id: 0,
            last_visit: 0,
        }
    }
}

impl Persisted {
    pub fn toggle_watch(&mut self, album: Album) -> bool {
        if let Some(pos) = self.watchlist.iter().position(|a| a.id == album.id) {
            self.watchlist.remove(pos);
            self.watch_meta.remove(&album.id);
            false
        } else {
            self.watch_meta.entry(album.id).or_default();
            self.watchlist.insert(0, album);
            true
        }
    }

    pub fn add_watch_folder(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty()
            || self
                .watch_folders
                .iter()
                .any(|folder| folder.eq_ignore_ascii_case(name))
        {
            return false;
        }
        self.watch_folders.push(name.to_string());
        true
    }

    pub fn remove_watch_folder(&mut self, name: &str) {
        self.watch_folders.retain(|folder| folder != name);
        for meta in self.watch_meta.values_mut() {
            if meta.folder.as_deref() == Some(name) {
                meta.folder = None;
            }
        }
    }

    pub fn rename_watch_folder(&mut self, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        if new.is_empty() {
            return Err("Folder name cannot be empty.".into());
        }
        if new != old
            && self
                .watch_folders
                .iter()
                .any(|folder| folder.eq_ignore_ascii_case(new))
        {
            return Err("A folder with that name already exists.".into());
        }
        if let Some(folder) = self.watch_folders.iter_mut().find(|folder| *folder == old) {
            *folder = new.to_string();
        }
        for meta in self.watch_meta.values_mut() {
            if meta.folder.as_deref() == Some(old) {
                meta.folder = Some(new.to_string());
            }
        }
        Ok(())
    }

    pub fn set_watch_folder(&mut self, album_id: u64, folder: Option<String>) -> bool {
        let folder = folder
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        if let Some(name) = &folder {
            self.add_watch_folder(name);
        }
        let meta = self.watch_meta.entry(album_id).or_default();
        if meta.folder == folder {
            false
        } else {
            meta.folder = folder;
            true
        }
    }

    pub fn watch_folder_of(&self, album_id: u64) -> Option<&str> {
        self.watch_meta
            .get(&album_id)
            .and_then(|meta| meta.folder.as_deref())
    }

    pub fn watch_note_of(&self, album_id: u64) -> &str {
        self.watch_meta
            .get(&album_id)
            .map(|meta| meta.note.as_str())
            .unwrap_or("")
    }

    /// Remember a query. Repeating a query moves it to the top instead of
    /// creating duplicates, and the list is capped so the state file stays small.
    pub fn record_search(
        &mut self,
        text: &str,
        kind: SearchKind,
        formats: FormatFilter,
        at: u64,
    ) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        let key = text.to_lowercase();
        let existing = self
            .search_history
            .iter()
            .position(|entry| entry.text.to_lowercase() == key);
        let unchanged = existing == Some(0)
            && self.search_history[0].kind == kind
            && self.search_history[0].formats == formats
            && self.search_history[0].text == text;
        if unchanged {
            return false;
        }
        if let Some(index) = existing {
            self.search_history.remove(index);
        }
        self.search_history.insert(
            0,
            SearchHistoryEntry {
                text: text.to_string(),
                kind,
                formats,
                at,
            },
        );
        self.search_history.truncate(SEARCH_HISTORY_LIMIT);
        !unchanged
    }

    pub fn clear_search_history(&mut self) -> bool {
        let changed = !self.search_history.is_empty();
        self.search_history.clear();
        changed
    }

    /// Save or replace a named search. Using the same name updates the stored
    /// query, which makes "Save search" behave like a lightweight bookmark edit.
    pub fn save_search(
        &mut self,
        name: &str,
        text: &str,
        kind: SearchKind,
        formats: FormatFilter,
        at: u64,
    ) -> bool {
        let name = name.trim();
        let text = text.trim();
        if name.is_empty() || text.is_empty() {
            return false;
        }
        let key = name.to_lowercase();
        self.saved_searches
            .retain(|saved| saved.name.to_lowercase() != key);
        self.saved_searches.insert(
            0,
            SavedSearch {
                name: name.to_string(),
                text: text.to_string(),
                kind,
                formats,
                saved_at: at,
            },
        );
        self.saved_searches.truncate(SAVED_SEARCH_LIMIT);
        true
    }

    pub fn remove_saved_search(&mut self, name: &str) -> bool {
        let before = self.saved_searches.len();
        self.saved_searches
            .retain(|saved| !saved.name.eq_ignore_ascii_case(name));
        self.saved_searches.len() != before
    }

    pub fn add_save_rule(&mut self, mut rule: SaveRule) -> Result<(), String> {
        rule.query = rule.query.trim().to_string();
        if rule.query.is_empty() {
            return Err("Enter text to match.".into());
        }
        if self.save_rules.iter().any(|existing| {
            existing.field == rule.field && existing.query.eq_ignore_ascii_case(&rule.query)
        }) {
            return Err("A rule with that text and field already exists.".into());
        }
        rule.folder = rule
            .folder
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        rule.enabled = true;
        self.save_rules.insert(0, rule);
        self.save_rules.truncate(50);
        Ok(())
    }

    pub fn remove_save_rule(&mut self, index: usize) -> bool {
        if index < self.save_rules.len() {
            self.save_rules.remove(index);
            true
        } else {
            false
        }
    }

    /// Apply enabled auto-save rules to freshly fetched releases. Returns only
    /// albums that were not already on the watchlist.
    pub fn apply_save_rules(&mut self, albums: &[Album]) -> Vec<Album> {
        let mut saved = Vec::new();
        for album in albums {
            if self.watchlist.iter().any(|entry| entry.id == album.id) {
                continue;
            }
            let rule = match self
                .save_rules
                .iter()
                .find(|rule| rule.enabled && rule.matches(album))
            {
                Some(rule) => rule.clone(),
                None => continue,
            };
            let folder = rule.folder.and_then(|name| {
                self.add_watch_folder(&name);
                self.watch_folders
                    .iter()
                    .find(|existing| existing.eq_ignore_ascii_case(name.trim()))
                    .cloned()
            });
            let meta = self.watch_meta.entry(album.id).or_default();
            if folder.is_some() {
                meta.folder = folder;
            }
            self.watchlist.insert(0, album.clone());
            saved.push(album.clone());
        }
        saved
    }

    pub fn in_library(&self, album_id: u64) -> bool {
        self.library.iter().any(|entry| entry.album.id == album_id)
    }

    /// Add or refresh a completed download. Album ids are unique in the
    /// library, so downloading a new copy replaces the old record instead of
    /// creating a duplicate row.
    pub fn record_download(
        &mut self,
        album: Album,
        path: PathBuf,
        extracted: Option<PathBuf>,
        warning: Option<String>,
    ) -> bool {
        let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let completed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let mut changed = false;
        if let Some(entry) = self
            .library
            .iter_mut()
            .find(|entry| entry.album.id == album.id)
        {
            if entry.path != path {
                entry.path = path;
                entry.extracted = extracted;
                entry.size = size;
                entry.completed_at = completed_at;
                entry.warning = warning;
                changed = true;
            }
            entry.album = album;
        } else {
            self.library.insert(
                0,
                LibraryEntry {
                    album,
                    path,
                    extracted,
                    size,
                    completed_at,
                    warning,
                },
            );
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: u64, title: &str) -> Album {
        Album {
            id,
            title: title.into(),
            artist: "Artist".into(),
            ..Album::default()
        }
    }

    #[test]
    fn library_deduplicates_by_album_id() {
        let mut state = Persisted::default();
        assert!(state.record_download(album(7, "First"), PathBuf::from("old.zip"), None, None));
        assert!(state.record_download(album(7, "Renamed"), PathBuf::from("new.zip"), None, None));
        assert_eq!(state.library.len(), 1);
        assert_eq!(state.library[0].album.title, "Renamed");
        assert_eq!(state.library[0].path, PathBuf::from("new.zip"));
        assert!(state.in_library(7));
    }

    #[test]
    fn watch_folders_survive_assignment_and_removal() {
        let mut state = Persisted::default();
        let watched = album(11, "Watched");
        assert!(state.toggle_watch(watched.clone()));
        assert!(state.add_watch_folder("Favorites"));
        assert!(!state.add_watch_folder("favorites"));
        assert!(state.set_watch_folder(11, Some("Favorites".into())));
        assert_eq!(state.watch_folder_of(11), Some("Favorites"));
        state.watch_meta.get_mut(&11).unwrap().note = "A note".into();
        assert_eq!(state.watch_note_of(11), "A note");
        state.remove_watch_folder("Favorites");
        assert!(state.watch_folders.is_empty());
        assert_eq!(state.watch_folder_of(11), None);
        assert!(!state.toggle_watch(watched));
        assert!(state.watch_meta.is_empty());
    }

    #[test]
    fn watch_folder_rename_rejects_duplicates() {
        let mut state = Persisted::default();
        state.add_watch_folder("One");
        state.add_watch_folder("Two");
        assert!(state.rename_watch_folder("One", "Three").is_ok());
        assert!(state.rename_watch_folder("Three", "Two").is_err());
        assert_eq!(state.watch_folders, vec!["Three", "Two"]);
    }

    #[test]
    fn search_history_deduplicates_and_caps() {
        let mut state = Persisted::default();
        let formats = FormatFilter::default();
        assert!(!state.record_search("   ", SearchKind::Blanket, formats, 1));
        assert!(state.record_search("Rock", SearchKind::Blanket, formats, 1));
        assert!(!state.record_search(" Rock ", SearchKind::Blanket, formats, 2));
        assert_eq!(state.search_history.len(), 1);
        assert_eq!(state.search_history[0].text, "Rock");
        assert_eq!(state.search_history[0].at, 1);

        state.record_search("Jazz", SearchKind::Tags, formats, 3);
        assert_eq!(state.search_history[0].text, "Jazz");
        assert_eq!(state.search_history[1].text, "Rock");

        for index in 0..(SEARCH_HISTORY_LIMIT + 4) {
            state.record_search(
                &format!("query-{index}"),
                SearchKind::Artist,
                formats,
                index as u64,
            );
        }
        assert_eq!(state.search_history.len(), SEARCH_HISTORY_LIMIT);
        assert_eq!(
            state.search_history[0].text,
            format!("query-{}", SEARCH_HISTORY_LIMIT + 3)
        );
        assert!(state.clear_search_history());
        assert!(state.search_history.is_empty());
        assert!(!state.clear_search_history());
    }

    #[test]
    fn saved_searches_replace_by_name_and_remove_cleanly() {
        let mut state = Persisted::default();
        let formats = FormatFilter::default();
        assert!(state.save_search("Morning", "jazz", SearchKind::Tags, formats, 10));
        assert!(state.save_search("Evening", "rock", SearchKind::Blanket, formats, 11));
        assert!(state.save_search("morning", "ambient", SearchKind::Tags, formats, 12));
        assert_eq!(state.saved_searches.len(), 2);
        assert_eq!(state.saved_searches[0].name, "morning");
        assert_eq!(state.saved_searches[0].text, "ambient");
        assert!(state.remove_saved_search("MORNING"));
        assert_eq!(state.saved_searches.len(), 1);
        assert_eq!(state.saved_searches[0].name, "Evening");
        assert!(!state.remove_saved_search("Missing"));
        assert!(!state.save_search("", "query", SearchKind::Blanket, formats, 1));
    }

    #[test]
    fn search_sort_keeps_every_album() {
        let mut first = album(3, "Beta");
        first.artist = "Zed".into();
        first.format = "MP3".into();
        let mut second = album(9, "alpha");
        second.artist = "Amy".into();
        second.format = "FLAC".into();
        let mut third = album(1, "Gamma");
        third.artist = "Amy".into();
        third.format = "TTA".into();
        let albums = vec![first, second, third];

        let titles: Vec<u64> = sort_albums(albums.clone(), SearchSort::Title)
            .iter()
            .map(|album| album.id)
            .collect();
        assert_eq!(titles, vec![9, 3, 1]);

        let artists: Vec<u64> = sort_albums(albums.clone(), SearchSort::Artist)
            .iter()
            .map(|album| album.id)
            .collect();
        assert_eq!(artists, vec![9, 1, 3]);

        let newest: Vec<u64> = sort_albums(albums.clone(), SearchSort::Newest)
            .iter()
            .map(|album| album.id)
            .collect();
        assert_eq!(newest, vec![9, 3, 1]);

        let formats: Vec<u64> = sort_albums(albums.clone(), SearchSort::Format)
            .iter()
            .map(|album| album.id)
            .collect();
        assert_eq!(formats, vec![9, 3, 1]);

        let default: Vec<u64> = sort_albums(albums, SearchSort::Default)
            .iter()
            .map(|album| album.id)
            .collect();
        assert_eq!(default, vec![3, 9, 1]);
    }

    #[test]
    fn save_rules_match_and_assign_watch_folders() {
        let mut state = Persisted::default();
        assert!(state
            .add_save_rule(SaveRule {
                query: "yonder".into(),
                field: RuleField::Artist,
                folder: Some("Favorites".into()),
                enabled: true,
            })
            .is_ok());
        assert!(state
            .add_save_rule(SaveRule {
                query: "YONDER".into(),
                field: RuleField::Artist,
                folder: None,
                enabled: true,
            })
            .is_err());

        let mut yonder = album(21, "Best Of");
        yonder.artist = "Yonder Voice".into();
        yonder.tags = vec!["Vocal".into()];
        let other = album(22, "Unrelated");
        let saved = state.apply_save_rules(&[yonder.clone(), other]);
        assert_eq!(saved.len(), 1);
        assert_eq!(state.watchlist.len(), 1);
        assert_eq!(state.watchlist[0].id, 21);
        assert_eq!(state.watch_folder_of(21), Some("Favorites"));

        let saved_again = state.apply_save_rules(&[yonder]);
        assert!(saved_again.is_empty());
        assert_eq!(state.watchlist.len(), 1);
    }

    #[test]
    fn save_rules_can_be_disabled_and_removed() {
        let mut state = Persisted::default();
        state
            .add_save_rule(SaveRule {
                query: "rock".into(),
                field: RuleField::Tag,
                folder: None,
                enabled: true,
            })
            .unwrap();
        let mut album = album(30, "Live");
        album.tags = vec!["Rock".into()];
        assert_eq!(state.apply_save_rules(&[album.clone()]).len(), 1);
        state.save_rules[0].enabled = false;
        state.watchlist.clear();
        assert!(state.apply_save_rules(&[album]).is_empty());
        assert!(state.remove_save_rule(0));
        assert!(state.save_rules.is_empty());
        assert!(!state.remove_save_rule(0));
    }
}
