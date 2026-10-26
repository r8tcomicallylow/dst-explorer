# Changelog

All notable changes to this project will be documented in this format.

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