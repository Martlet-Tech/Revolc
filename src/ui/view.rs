//! What a tab's content actually is.
//!
//! `tabs/mod.rs` stays pure logic (id/path/label only). The content side —
//! whether a tab owns a foreign HWND or is drawn by us — lives here, in the UI
//! layer next to `Absorbed` and the painters.
//!
//! The split is by **who owns the pixels**, not by feature name:
//! * `Hosted` — a foreign window re-parented into our client. Explorer today;
//!   a future notepad/calculator would be another `Hosted` variant.
//! * `Painted` — no HWND at all, we draw into the tab's view rect and give it
//!   the main window's keyboard focus. Settings today; a future Everything
//!   results page is another `PaintedPage`.
//!
//! `Pending` is not a kind of content but a transient state: the tab was
//! created (Ctrl+T / stray adoption) and its Explorer window has not arrived
//! yet. It used to be `None` in `Vec<Option<Absorbed>>`, which overloaded one
//! slot with two meanings ("not here yet" vs "never will be — it's a painted
//! page"). Making it explicit lets every traversal say exactly which states
//! it cares about.

use crate::shell::host::Absorbed;

/// A tab's content slot. Parallel to `App::tabs` by index.
pub enum View {
    /// Reserved tab, waiting for the Explorer window we spawned / adopted.
    Pending,
    /// Foreign window re-parented into our client.
    Hosted(Absorbed),
    /// Drawn by Revolc; no foreign window, no input-queue attach.
    /// Constructed in stage C (settings tab); declared now so every traversal
    /// learns to account for it in stage A.
    #[allow(dead_code)]
    Painted(PaintedPage),
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Absorbed is main-thread-only and holds raw window state; printing
        // just the discriminant is enough for logs and avoids leaking HWNDs.
        match self {
            View::Pending => f.write_str("Pending"),
            View::Hosted(_) => f.write_str("Hosted"),
            View::Painted(p) => f.debug_tuple("Painted").field(p).finish(),
        }
    }
}

/// Pages Revolc paints itself. Open enum: add a variant + a paint/hit branch
/// to add a page. `Hosted` counterparts (notepad etc.) do not go here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintedPage {
    #[allow(dead_code)] // stage C
    Settings,
}

impl View {
    pub fn is_pending(&self) -> bool {
        matches!(self, View::Pending)
    }

    #[allow(dead_code)] // stage C
    pub fn is_painted(&self) -> bool {
        matches!(self, View::Painted(_))
    }

    /// The hosted window, if this tab has one. `Pending` and `Painted` both
    /// return None; callers that need to distinguish check `is_pending()` /
    /// `is_painted()` first.
    pub fn as_hosted(&self) -> Option<&Absorbed> {
        match self {
            View::Hosted(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_hosted_mut(&mut self) -> Option<&mut Absorbed> {
        match self {
            View::Hosted(v) => Some(v),
            _ => None,
        }
    }

    /// Which painted page, if any.
    #[allow(dead_code)] // stage C
    pub fn as_painted(&self) -> Option<PaintedPage> {
        match self {
            View::Painted(p) => Some(*p),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_is_neither_hosted_nor_painted() {
        let v = View::Pending;
        assert!(v.is_pending());
        assert!(!v.is_painted());
        assert!(v.as_hosted().is_none());
        assert!(v.as_painted().is_none());
    }

    #[test]
    fn painted_page_is_identified() {
        let v = View::Painted(PaintedPage::Settings);
        assert!(v.is_painted());
        assert!(!v.is_pending());
        assert!(v.as_hosted().is_none());
        assert_eq!(v.as_painted(), Some(PaintedPage::Settings));
    }
}
