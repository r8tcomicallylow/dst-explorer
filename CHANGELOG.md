# Changelog

All notable changes to this project will be documented in this format.

## [Unreleased]

## [1.3.1] - 2026-11-08

### Changed
- Library filter, sort and header totals are cached and rebuilt only when the
  collection or a filter control changes; sorting lowercases each key once
  (`sort_by_cached_key`) instead of inside the comparator
- Download settings are compared before being applied, so the per-frame
  `configure()` no longer reassigns every field or re-dispatches the queue
- The watchlist grid draws from indices instead of cloning every album each
  frame, and the folder chip row no longer clones the folder list
- The download cache size is formatted once and reused by the sidebar and
  Settings; the cache directory is no longer walked every frame
- Grid scroll-area IDs use tuple salts instead of allocating `format!` strings
  every frame

## [1.3.0] - 2026-11-08

### Added
- Dominant-colour ambient glow behind the detail panel, player bar and highlighted cards
- Card hover lift, diagonal light sweep and subtle cover parallax
- Frosted-glass styling for the detail panel, player bar and selection bar
- Animated aurora background on empty Discover / Search states
- "Visual effects" level (Full / Soft / Off) and a "Reduce motion" setting
- `DST_FPS=1` frame-pacing overlay (FPS and frame time)
- Download queue prioritization (`First`, `↑`, `↓`) and per-host pause/resume chips
- Resume interrupted downloads automatically on launch (`.part` + HTTP Range)
- Global download speed limit, optional scheduler window and source archive auto-delete
- `Content-MD5` verification for freshly downloaded files and smarter fragment retry handling
- Bulk download confirmation modal and library statistics/cleanup tools
- Search filters for regex, lossless/lossy quality and availability (NEW, owned, queued)
- Library sorting (newest, title, artist, format, size) and optional year/tags columns
- Daily state database backups (keeps the last 10) with manual backup controls
- Saved-search notifications, RSS/Atom feed watcher and toast action buttons
- Compact cover-only grid, UI scale slider and language quick-switch in the sidebar
- CLI mode: `--search`, `--latest`, `--download`, `--json`, `--help`
- Terminal UI: `--tui` with keyboard navigation, refresh, paging and download
- Queue text filter and status chips (active, paused, failed, done)
- MPD integration: optional host/port, connection test and "add folder to queue" from the library
- Fixed TUI keyboard input in release builds by reopening `CONIN$`/`CONOUT$` after attaching to the parent console
- Command palette, cheat sheet, F11 fullscreen, Ctrl+Alt+P/R pause/resume and Ctrl+Alt+T accent cycling
- External player fallback to the system handler and improved WCAG contrast for muted text
- Built-in audio preview player (rodio + Symphonia): album/folder queue, gapless pre-roll, seek, pause/stop,
  volume and background waveform extraction, with library/detail play buttons and player hotkeys
- True light/dark theme with a persisted toggle (`Ctrl+Alt+L`) and a light-aware palette across all widgets
- Player shuffle and repeat modes (off / all / one), persisted between launches
- Resume playback from the last track and position on the next launch (toggle in Settings)
- Album cover in the player bar, clickable to open the matching album card
- Animated now-playing equaliser and a hover Play button on downloaded cards
- Undo toasts for watchlist/library removals and for clearing search history
- Copy album links from the context menu, the selection bar and `Ctrl+Shift+C`
- Collapsible sidebar, "Surprise me" random-album picker and a Konami-code accent easter egg
- Tray tooltip with the current track and the number of active downloads
- Animated view transitions, detail-panel slide, player-bar reveal, sidebar collapse,
  card hover states, cover fade-in, shimmer placeholders and toast fade-out

### Changed
- Render optimisations: watchlist/library membership is indexed once per frame instead of
  scanned per card, text elision is a single layout pass, texture trimming tracks bytes
  incrementally, and image requests are skipped when a key is already known
- Player queue handling now resets prefetch state correctly when shuffle, repeat or resume changes
- Lower memory footprint: photo cache now has a 20 MB byte cap in addition to the entry limit,
  thumbnails and covers decode at smaller sizes, image workers share one HTTP client,
  and locale bundles are parsed lazily instead of all five at startup

### Fixed
- A failed cover download no longer triggers a permanent ~12 FPS repaint loop with an endlessly
  animated shimmer placeholder
- Command palette and shortcut overlay no longer force a full repaint every frame
- UI scale slider no longer jumps back and forth; zoom is applied when the drag ends
- Waveform scrubbing previews the position while dragging and seeks once on release
- Seeking after a gapless track transition now uses the correct offset inside the sink
- Player bar no longer clips the track duration; the time label and volume controls have reserved space
- Library thumbnails use their own texture key, so a small row cover can no longer blur the grid artwork
- Library warning badge showed the literal text `WARNING()` instead of `WARNING`

## [1.2.0] - 2026-11-06

### Added
- Five interface languages: English, German, French, Chinese and Japanese, with flag emoji selectors in Settings
- Automatic language detection via `DST_EXPLORER_LANG` (falls back to the saved setting)
- Fluent (`.ftl`) based localization bundled into the binary, so the portable build stays self-contained
- Localized sidebar navigation, page headers and the whole Settings screen
- Locale parity tests that fail the build when a translation key is missing

### Changed
- `theme` widget helpers now accept any string type, making localized labels ergonomic

## [1.1.0] - 2026-11-06

### Added
- Auto-retry of failed downloads with exponential backoff (only for temporary errors)
- Windows taskbar progress reporting and an "All downloads complete" notification
- Import/Export of settings, watchlist, library, watch folders and auto-save rules as JSON
- Library filters: format, year and instant text search
- Dynamic version label in the sidebar and About card

## [1.0.0] - 2026-10-27

### Added
- Initial public release
- Discover latest releases with infinite scroll
- Search by everything/artist/tag/provider with format filters
- Virtualized cover grid with lazy thumbnail loading
- Album detail inspector with cover preview
- Bulk downloads with configurable concurrency
- Resumable download queue with pause/resume/retry
- Persistent queue survives restarts
- Automatic ZIP extraction
- Watchlist with folders and private notes
- Library of completed downloads
- Auto-save rules for new releases
- Search history and saved searches
- Keyboard shortcuts (Ctrl+F, Ctrl+R, Ctrl+D, Esc)
- System tray icon with minimize-to-tray
- Persistent settings (accent, paths, limits)
- **foobar2000 integration** - send albums to foobar2000 via `/add`
- **Mass download by tag/artist** from context menus
- **Download all results** button in search
- Optional logging via `DST_EXPLORER_LOG=1`

### Fixed
- Cover art preview window close button duplication
- RichText compilation error in command palette

### Supported Download Hosts
- MediaFire (direct CDN link)
- Google Drive (large file confirmation + cookies)
- Dropbox (dl=1 conversion)
- Pixeldrain (API endpoint)
- Direct file URLs
- MEGA and others (opens in browser)

## [Unreleased]

### Planned
- Auto-update check
- More host resolvers
- Theme customization
- Import/export settings