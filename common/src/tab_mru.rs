//! Per-window most-recently-used tab labels, shared with the tray.
//!
//! The tray's right-click menu shows, for each open Revolc window, the three
//! tabs touched most recently. Tab order lives only inside each window
//! process, and the shared ledger is a single fixed-size struct with no room
//! per window, so the labels are parked in tiny files keyed by the frame HWND.
//! Writes happen on tab activation (low frequency); reads happen when the
//! menu opens. A stale/missing file is harmless — the menu falls back to the
//! window title.

use std::path::PathBuf;
use windows::Win32::Foundation::HWND;

const MAX_LABELS: usize = 3;
const MAX_LABEL_CHARS: usize = 64;

fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("revolc").join("mru")
}

fn path_for(hwnd: HWND) -> PathBuf {
    dir().join(format!("{:x}.txt", hwnd.0 as usize))
}

/// Record that `label` was just activated in `hwnd`, keeping at most three
/// labels, most recent first. Duplicate labels collapse to one entry.
pub fn note_activation(hwnd: HWND, label: &str) {
    if label.is_empty() {
        return;
    }
    let mut labels = read(hwnd);
    labels.retain(|l| l != label);
    labels.insert(0, label.chars().take(MAX_LABEL_CHARS).collect());
    labels.truncate(MAX_LABELS);
    let path = path_for(hwnd);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, labels.join("\n"));
}

/// Drop a label that is no longer a tab (closed tab), so the menu never shows
/// a folder the window does not have any more.
pub fn forget(hwnd: HWND, label: &str) {
    let mut labels = read(hwnd);
    labels.retain(|l| l != label);
    let _ = std::fs::write(path_for(hwnd), labels.join("\n"));
}

/// Current MRU labels, most recent first.
pub fn read(hwnd: HWND) -> Vec<String> {
    std::fs::read_to_string(path_for(hwnd))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .take(MAX_LABELS)
        .map(str::to_string)
        .collect()
}

/// Called when a window exits: remove its file so the tray cannot show a
/// dead window's labels.
pub fn remove(hwnd: HWND) {
    let _ = std::fs::remove_file(path_for(hwnd));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_hwnd_keyed() {
        let a = path_for(HWND(0x1234_usize as _)).to_string_lossy().into_owned();
        let b = path_for(HWND(0x5678_usize as _)).to_string_lossy().into_owned();
        assert_ne!(a, b);
        assert!(a.ends_with("1234.txt"));
    }
}
