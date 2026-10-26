# DST-Explorer

A fast, minimal desktop browser and download manager for [DoujinStyle](https://doujinstyle.com), written in Rust with `eframe`/`egui`.

![Screenshot](assets/screenshot.png)

## Features

- **Discover** — latest releases with infinite scroll and "new since last visit" badges
- **Search** — by everything, artist, tag, or provider; with FLAC/MP3/TTA/Opus/OGG filters
- **Virtualized grid** — only visible cards rendered; thumbnails load lazily on worker pool
- **Album inspector** — cover, artists, tags, format, date, rating, downloads, crossfade link
- **Bulk downloads** — select multiple, queue together, or download entire watchlist/folder
- **Queue manager** — configurable concurrency, live progress/speed, pause/resume/cancel/retry
- **Persistent queue** — unfinished transfers survive restarts, restored as paused
- **Auto-extract** — optional ZIP unpacking next to downloaded file
- **Watchlist** — save albums locally, group into folders, add private notes
- **Library** — completed downloads indexed automatically with folder access
- **Keyboard shortcuts** — `Ctrl+F` search, `Ctrl+R` refresh, `Ctrl+D` download, `Esc` close
- **System tray** — minimize to tray, pause/resume all, quit
- **foobar2000 integration** — send albums/folders to foobar2000 with one click
- **Mass download by tag/artist** — right-click any album → "Download by tag/artist"

## Screenshots

| Discover | Search | Detail | Downloads |
|----------|--------|--------|-----------|
| ![Discover](assets/discover.png) | ![Search](assets/search.png) | ![Detail](assets/detail.png) | ![Downloads](assets/downloads.png) |

## Installation

### Portable (Recommended)

Download the latest release for your platform:
- **Windows**: `dst-explorer-windows-x64.exe`
- **Linux**: `dst-explorer-linux-x64`
- **macOS**: `dst-explorer-macos-universal`

No installation needed — just run the executable. All data is stored in platform-standard directories:
- Windows: `%APPDATA%\DST-Explorer\`
- Linux: `~/.local/share/DST-Explorer/`
- macOS: `~/Library/Application Support/DST-Explorer/`

### Build from Source

```bash
# Requirements: Rust 1.75+
cargo build --release
# Binary at target/release/dst-explorer[.exe]
```

## Usage

1. **Browse** — Start at "Discover" for latest releases
2. **Search** — Press `Ctrl+F` or click Search, enter query, pick type/filters
3. **Inspect** — Click any album card for details, large cover preview
4. **Download** — `Ctrl+D` on selection, or right-click → "Download album"
5. **Watchlist** — Click ★ on cards to save; organize into folders
6. **foobar2000** — Set path in Settings → Downloads; right-click → "Play in foobar2000"

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+F` / `/` | Focus search |
| `Ctrl+R` | Refresh current view |
| `Ctrl+D` | Download selection / open album |
| `Esc` | Close preview/inspector/selection |
| `Ctrl+P` | Command palette |
| `1`–`6` | Switch views (Discover/Search/Watchlist/Library/Downloads/Settings) |

## Supported Download Hosts

| Host | Behavior |
|------|----------|
| MediaFire | Resolved to direct CDN link |
| Google Drive | Resolved incl. large-file confirmation & cookies |
| Dropbox | Converted to `dl=1` direct link |
| Pixeldrain | Resolved to API download endpoint |
| Direct URLs | Downloaded as-is |
| MEGA, others | Opens in browser (marked "Needs browser") |

## Configuration

Settings → adjust:
- Download folder & simultaneous downloads
- Auto-extract ZIPs
- Accent color (5 presets)
- Thumbnail memory budget
- Tray icon & minimize-to-tray
- Desktop notifications
- foobar2000 executable path

Data locations (override with env vars):
- `DST_EXPLORER_DATA_DIR` — state.json, queue.json, watchlist, library
- `DST_EXPLORER_CACHE_DIR` — image cache (thumbs/covers)

## Logging

Set `DST_EXPLORER_LOG=1` to enable file logging:
```
%APPDATA%\DST-Explorer\dst-explorer.log
```

## Disclaimer

- Unofficial personal-use client for DoujinStyle.com
- No content is shipped; all data fetched from public site
- Respect DoujinStyle rules, creators' rights, and file host terms
- Download only material you're entitled to access

## License

MIT — see [LICENSE](LICENSE)