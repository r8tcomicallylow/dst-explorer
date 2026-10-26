use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::models::{Persisted, STATE_VERSION};
use crate::persist;

pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("DST_EXPLORER_DATA_DIR") {
        return PathBuf::from(path);
    }
    dirs::data_dir()
        .or_else(|| dirs::config_dir())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DST-Explorer")
}

pub fn cache_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("DST_EXPLORER_CACHE_DIR") {
        return PathBuf::from(path);
    }
    dirs::cache_dir()
        .unwrap_or_else(data_dir)
        .join("DST-Explorer")
}

pub fn config_path() -> PathBuf {
    data_dir().join("state.json")
}

pub fn queue_path() -> PathBuf {
    data_dir().join("queue.json")
}

pub fn thumbs_dir() -> PathBuf {
    cache_dir().join("thumbs")
}

pub fn covers_dir() -> PathBuf {
    cache_dir().join("covers")
}

pub fn ensure_dirs() {
    let _ = fs::create_dir_all(data_dir());
    let _ = fs::create_dir_all(thumbs_dir());
    let _ = fs::create_dir_all(covers_dir());
}

pub fn load_state() -> Persisted {
    ensure_dirs();
    let mut state = persist::read_json::<Persisted>(&config_path()).unwrap_or_default();
    migrate_state(&mut state);
    state
}

fn migrate_state(state: &mut Persisted) {
    if state.version < STATE_VERSION {
        // Older files are forward-compatible: every added field has a serde
        // default. Bump the version so future migrations can rely on it.
        state.version = STATE_VERSION;
    }
}

pub fn save_state(state: &Persisted) {
    ensure_dirs();
    if let Err(err) = persist::write_json(&config_path(), state) {
        eprintln!("Could not save state: {err}");
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn cache_size() -> u64 {
    dir_size(&cache_dir())
}

pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

pub fn clear_cache() {
    let _ = fs::remove_dir_all(cache_dir());
    ensure_dirs();
}

pub fn thumb_cache_path(id: u64) -> PathBuf {
    thumbs_dir().join(format!("{id}.jpg"))
}

pub fn cover_cache_path(id: u64) -> PathBuf {
    covers_dir().join(format!("{id}.jpg"))
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn open_path(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

pub fn reveal_path(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(parent) = path.parent() {
            open_path(parent);
        }
    }
}
