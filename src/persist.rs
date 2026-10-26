use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Read a JSON document. Missing or malformed files yield `None`; callers decide
/// whether that should fall back to defaults or surface an error.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<T>(&text) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!("Could not parse {}: {err}", path.display());
            None
        }
    }
}

/// Write JSON atomically: write a sibling temporary file, then rename it over
/// the destination. On Windows an open destination may block the rename, so the
/// destination is removed and retried once.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(first) => {
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)
                .map_err(|second| format!("replace {}: {first} / {second}", path.display()))
        }
    }
}
