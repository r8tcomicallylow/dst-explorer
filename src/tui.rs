//! A small terminal interface for browsing the latest releases.
//!
//! Enabled with `dst-explorer --tui`. It deliberately shares the same API and
//! download helpers as the graphical app, so it stays lightweight and does not
//! pull in the GUI toolkit at runtime.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue, style};

use crate::api;
use crate::downloads;
use crate::models::Album;

pub struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|error| error.to_string())?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, cursor::Hide)
            .map_err(|error| error.to_string())?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

pub fn run() -> Result<(), String> {
    let _guard = TerminalGuard::enter()?;
    let client = api::page_client();
    let mut albums: Vec<Album> = Vec::new();
    let mut selected = 0usize;
    let mut status = String::from("Loading latest releases…");
    let mut page = 0usize;

    draw(&albums, selected, &status)?;
    match api::fetch_latest(&client, page) {
        Ok(feed) => {
            albums = feed.albums;
            status = format!("{} releases loaded", albums.len());
        }
        Err(error) => status = format!("Could not load releases: {error}"),
    }

    loop {
        draw(&albums, selected, &status)?;
        if !event::poll(std::time::Duration::from_millis(250))
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        let event = event::read().map_err(|error| error.to_string())?;
        let Event::Key(key) = event else {
            continue;
        };
        log_key(&key);
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if selected + 1 < albums.len() {
                    selected += 1;
                }
            }
            KeyCode::PageUp => selected = selected.saturating_sub(10),
            KeyCode::PageDown => {
                selected = (selected + 10).min(albums.len().saturating_sub(1));
            }
            KeyCode::Char('r') => {
                status = "Refreshing…".to_string();
                draw(&albums, selected, &status)?;
                match api::fetch_latest(&client, page) {
                    Ok(feed) => {
                        albums = feed.albums;
                        selected = 0;
                        status = format!("{} releases loaded", albums.len());
                    }
                    Err(error) => status = format!("Refresh failed: {error}"),
                }
            }
            KeyCode::Char('n') => {
                page += 1;
                status = format!("Loading page {}…", page);
                draw(&albums, selected, &status)?;
                match api::fetch_latest(&client, page) {
                    Ok(mut feed) => {
                        albums.append(&mut feed.albums);
                        status = format!("{} releases loaded", albums.len());
                    }
                    Err(error) => {
                        page = page.saturating_sub(1);
                        status = format!("Could not load page: {error}");
                    }
                }
            }
            KeyCode::Char('o') => {
                if let Some(album) = albums.get(selected) {
                    status = format!("Opened {} in the browser", album.title);
                    let _ = webbrowser::open(&album.page_url());
                }
            }
            KeyCode::Char('d') | KeyCode::Enter => {
                if let Some(album) = albums.get(selected).cloned() {
                    status = format!("Resolving “{}”…", album.title);
                    draw(&albums, selected, &status)?;
                    let dir = crate::store::data_dir().join("downloads");
                    match downloads::cli_download(album.id, &dir) {
                        Ok(path) => status = format!("Saved {}", path.display()),
                        Err(error) => status = format!("Download failed: {error}"),
                    }
                }
            }
            _ => {}
        }
    }
}

fn draw(albums: &[Album], selected: usize, status: &str) -> Result<(), String> {
    let mut stdout = io::stdout();
    let (width, height) = terminal::size().map_err(|error| error.to_string())?;
    let width = width as usize;
    let height = height as usize;
    queue!(stdout, terminal::Clear(terminal::ClearType::All)).map_err(|e| e.to_string())?;

    let title = " DST-Explorer TUI ";
    queue!(
        stdout,
        cursor::MoveTo(0, 0),
        style::SetBackgroundColor(style::Color::DarkGrey),
        style::SetForegroundColor(style::Color::White),
        style::Print(format!("{title:<width$}")),
        style::ResetColor
    )
    .map_err(|e| e.to_string())?;

    let list_height = height.saturating_sub(4);
    let start = selected.saturating_sub(list_height / 2);
    for (offset, album) in albums.iter().enumerate().skip(start).take(list_height) {
        let row = 1 + (offset - start) as u16;
        let is_selected = offset == selected;
        let format = if album.format.trim().is_empty() {
            "-"
        } else {
            album.format.trim()
        };
        let artist = if album.artist.trim().is_empty() {
            "Unknown artist"
        } else {
            album.artist.trim()
        };
        let text = format!(
            " {:>7}  {:<6}  {} — {}",
            album.id, format, artist, album.title
        );
        let text = truncate(&text, width.saturating_sub(1));
        if is_selected {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                style::SetBackgroundColor(style::Color::DarkBlue),
                style::SetForegroundColor(style::Color::White),
                style::Print(format!("{text:<width$}")),
                style::ResetColor
            )
            .map_err(|e| e.to_string())?;
        } else {
            queue!(stdout, cursor::MoveTo(0, row), style::Print(text))
                .map_err(|e| e.to_string())?;
        }
    }

    let status_row = height.saturating_sub(2) as u16;
    queue!(
        stdout,
        cursor::MoveTo(0, status_row),
        style::SetForegroundColor(style::Color::Cyan),
        style::Print(truncate(status, width.saturating_sub(1))),
        style::ResetColor,
        cursor::MoveTo(0, height.saturating_sub(1) as u16),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(truncate(
            "↑/↓ move  Enter/d download  o open  r refresh  n next page  q quit",
            width.saturating_sub(1)
        )),
        style::ResetColor
    )
    .map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn log_key(key: &KeyEvent) {
    let Some(path) = std::env::var_os("DST_EXPLORER_TUI_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "key={:?} kind={:?} mods={:?}",
            key.code, key.kind, key.modifiers
        );
    }
}
