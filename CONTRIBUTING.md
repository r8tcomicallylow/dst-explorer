# Contributing to Doujin Explorer

Thank you for considering contributing! This is a personal project, but contributions are welcome.

## Development Setup

```bash
# Clone and build
git clone https://github.com/yourusername/doujin-explorer
cd doujin-explorer
cargo run
```

Requirements:
- Rust 1.75+ (stable)
- Windows/Linux/macOS with OpenGL
- On Linux: `libglib2.0-dev libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev pkg-config`

## Running Tests

```bash
# Offline unit tests
cargo test

# Live integration tests (hits doujinstyle.com)
cargo test -- --ignored --nocapture --test-threads=1
```

## Code Style

- `cargo fmt` for formatting
- `cargo clippy` for lints
- No unsafe code unless absolutely necessary
- Comments only for non-obvious logic

## Pull Requests

1. Fork the repo
2. Create a feature branch
3. Make your changes
4. Run `cargo fmt && cargo clippy && cargo test`
5. Submit PR with clear description

## Reporting Issues

Use GitHub Issues with:
- OS and Rust version
- Steps to reproduce
- Logs (set `DOUJIN_EXPLORER_LOG=1`)
- Screenshot if UI-related

## Architecture Overview

```
src/
├── main.rs       # Entry point, window setup
├── app.rs        # UI, views, state management
├── api.rs        # HTTP, HTML parsing, host resolvers
├── downloads.rs  # Download queue, workers, resume
├── images.rs     # Image worker pool, texture cache
├── models.rs     # Data structures, persistence
├── persist.rs    # Atomic JSON read/write
├── store.rs      # Paths, file helpers
├── theme.rs      # Colors, fonts, icons, widgets
└── tray.rs       # System tray icon
```

## License

By contributing, you agree that your contributions will be licensed under the MIT License.