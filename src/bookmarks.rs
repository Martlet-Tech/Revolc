//! Bookmark store: a flat list of `{ label, path }` persisted to `bookmarks.json`.
//!
//! Scope (TODO.md §4b phase 3): flat list only — folder hierarchy is deferred.
//! The file lives next to `revolc.toml` in `%APPDATA%\revolc\`. It carries a
//! `version` field so a future schema can migrate instead of guessing.
//!
//! Writes are atomic: serialise to a `.tmp` sibling, `sync_all`, then
//! `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` so a crash never leaves a half
//! written JSON behind (Rebuild_Target_Spec.md §3.3). Reads are defensive: a
//! missing / corrupt / future-version file yields an empty list with a log,
//! never a panic — bookmarks must not be able to take down the file manager.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};

const SCHEMA_VERSION: u32 = 1;

/// One bookmark. `path` is the filesystem folder to open; `label` is the
/// display text (Explorer window title, or a short path tail when empty).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bookmark {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BookmarkFile {
    version: u32,
    items: Vec<Bookmark>,
}

/// Where the bookmark file lives — same directory as the TOML config.
pub fn bookmarks_path() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("revolc").join("bookmarks.json")
}

/// Normalise a path for identity comparison: Windows paths are case
/// insensitive and a trailing separator is not significant. Forward slashes
/// are folded to backslashes so a UNC or user-typed path still dedupes.
pub fn normalize_path(p: &str) -> String {
    p.trim()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

pub struct BookmarkStore {
    items: Vec<Bookmark>,
    path: PathBuf,
}

impl BookmarkStore {
    /// Open the store at `path`, loading whatever is already on disk.
    pub fn open(path: PathBuf) -> Self {
        let mut store = Self { items: Vec::new(), path };
        store.reload();
        store
    }

    pub fn items(&self) -> &[Bookmark] {
        &self.items
    }

    pub fn contains(&self, path: &str) -> bool {
        let key = normalize_path(path);
        self.items.iter().any(|b| normalize_path(&b.path) == key)
    }

    /// Append a bookmark unless this folder is already saved. Returns true
    /// only when something was actually added (so the caller can tell the
    /// user "already bookmarked" instead of silently duplicating it).
    pub fn add(&mut self, label: impl Into<String>, path: impl Into<String>) -> bool {
        let path = path.into();
        if self.contains(&path) {
            return false;
        }
        self.items.push(Bookmark { label: label.into(), path });
        self.save();
        true
    }

    /// Remove every bookmark pointing at `path`. Returns true when removed.
    pub fn remove(&mut self, path: &str) -> bool {
        let key = normalize_path(path);
        let before = self.items.len();
        self.items.retain(|b| normalize_path(&b.path) != key);
        if self.items.len() == before {
            return false;
        }
        self.save();
        true
    }

    /// Remove the bookmark at `index` (as painted on the toolbar). Used by the
    /// chip's context menu, which identifies its target by chip index.
    pub fn remove_at(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        self.save();
        true
    }

    fn reload(&mut self) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(error = %e, "bookmarks read failed, starting empty");
                return;
            }
        };
        match serde_json::from_str::<BookmarkFile>(&text) {
            Ok(file) if file.version == SCHEMA_VERSION => self.items = file.items,
            Ok(file) => tracing::warn!(
                version = file.version,
                expected = SCHEMA_VERSION,
                "unsupported bookmarks schema, starting empty"
            ),
            Err(e) => tracing::error!(error = %e, "bookmarks parse failed, starting empty"),
        }
    }

    fn save(&self) {
        let file = BookmarkFile {
            version: SCHEMA_VERSION,
            items: self.items.clone(),
        };
        let json = match serde_json::to_string_pretty(&file) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "bookmarks serialise failed");
                return;
            }
        };
        if let Err(e) = atomic_write(&self.path, json.as_bytes()) {
            tracing::error!(error = %e, path = ?self.path, "bookmarks write failed");
        }
    }
}

/// Write `data` to `path` atomically on Windows: stage in a sibling `.tmp`,
/// flush it, then replace the target in one MoveFileEx call. A crash between
/// the two leaves the old file intact rather than a truncated JSON.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| e.to_string())?;
        f.flush().map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    let from = windows::core::HSTRING::from(tmp.to_string_lossy().as_ref());
    let to = windows::core::HSTRING::from(path.to_string_lossy().as_ref());
    unsafe {
        MoveFileExW(
            windows::core::PCWSTR(from.as_ptr()),
            windows::core::PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING,
        )
    }
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let base = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
        PathBuf::from(base)
            .join(format!("revolc-bm-test-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn normalize_folds_case_slash_and_trailing_separator() {
        assert_eq!(normalize_path("C:\\Users\\ZT\\Downloads\\"), normalize_path("c:/users/zt/downloads"));
        assert_eq!(normalize_path("\\\\srv\\Share"), normalize_path("//SRV/share/"));
    }

    #[test]
    fn add_dedupes_and_remove_is_exact() {
        let mut store = BookmarkStore::open(temp_path("dedupe"));
        // Don't touch the real file: clear via remove on whatever loaded.
        for b in store.items().to_vec() {
            let _ = store.remove(&b.path);
        }
        assert!(store.add("下载", "C:\\Users\\ZT\\Downloads"));
        assert!(!store.add("again", "c:/users/zt/downloads/"), "duplicate must be rejected");
        assert_eq!(store.items().len(), 1);
        assert!(store.contains("C:\\Users\\ZT\\Downloads"));
        assert!(store.remove("c:/users/zt/downloads/"));
        assert!(!store.remove("C:\\Users\\ZT\\Downloads"));
        assert!(store.items().is_empty());
    }

    #[test]
    fn save_then_open_round_trips_with_version() {
        let path = temp_path("roundtrip");
        let mut store = BookmarkStore::open(path.clone());
        for b in store.items().to_vec() {
            let _ = store.remove(&b.path);
        }
        assert!(store.add("Projects", "D:\\Projects"));
        let text = std::fs::read_to_string(&path).expect("bookmarks.json written");
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("D:\\\\Projects"), "{text}");
        let reopened = BookmarkStore::open(path);
        assert_eq!(reopened.items().len(), 1);
        assert_eq!(reopened.items()[0].label, "Projects");
        assert_eq!(reopened.items()[0].path, "D:\\Projects");
    }

    #[test]
    fn remove_at_deletes_by_chip_index() {
        let mut store = BookmarkStore::open(temp_path("remove_at"));
        for b in store.items().to_vec() {
            let _ = store.remove(&b.path);
        }
        assert!(store.add("one", "C:\\one"));
        assert!(store.add("two", "C:\\two"));
        assert!(store.add("three", "C:\\three"));
        assert!(store.remove_at(1));
        let paths: Vec<_> = store.items().iter().map(|b| b.path.as_str()).collect();
        assert_eq!(paths, ["C:\\one", "C:\\three"]);
        assert!(!store.remove_at(99), "out-of-range index is a no-op");
        assert_eq!(store.items().len(), 2);
    }

    #[test]
    fn corrupt_file_starts_empty_without_panic() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = BookmarkStore::open(path);
        assert!(store.items().is_empty());
    }

    #[test]
    fn future_version_starts_empty() {
        let path = temp_path("future");
        std::fs::write(&path, b"{\"version\":99,\"items\":[{\"label\":\"x\",\"path\":\"C:\\\\x\"}]}").unwrap();
        let store = BookmarkStore::open(path);
        assert!(store.items().is_empty());
    }
}
