//! Shared horizontal drag-to-reorder logic for the two rows in the chrome
//! popup: the tab strip (`tabbar.rs`) and the bookmark bar (`chrome.rs`).
//!
//! Both rows are evenly spaced and reordered by the same **centre-line rule**:
//! a held item changes places with its neighbour only once its own centre has
//! crossed that neighbour's centre. The only things that differ are the row's
//! origin x, item width and gap — so those are parameters here, not constants.
//!
//! The model never sees a "dragged index": callers reorder their list the
//! moment `landing` returns a new index, so `Drag::from` always means "where
//! the item sits in the *current* order".
//!
//! This module is pure geometry + state (no Win32, no painting): the window
//! decides when to begin, how to paint the held item, and when to commit.

/// A press on an item, before the pointer has moved far enough to be a drag.
/// `key` is a stable identifier (tab id, bookmark path hash/index) resolved by
/// the caller — a reorder or removal can shift indices mid-press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    pub key: u64,
    pub x: i32,
}

/// An in-row drag: the item left its slot and follows the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drag {
    /// Index in the current order (kept up to date by the caller after each
    /// reorder via `set_from`).
    pub from: usize,
    /// Pointer offset inside the item at press time, so it does not jump.
    grab: i32,
    /// Requested left edge, **unclamped**. Painting clamps it to the row; using
    /// the clamped value for hit-testing would make the first slot unreachable
    /// (same lesson as the tab strip's original drag).
    pub raw: i32,
}

impl Drag {
    /// Begin dragging item `from`. `press_x` is where the button went down,
    /// `item_left` the item's left edge at that moment (both client coords).
    pub fn new(from: usize, press_x: i32, item_left: i32) -> Self {
        Self { from, grab: press_x - item_left, raw: item_left }
    }

    /// Follow the pointer, preserving the grab offset.
    pub fn move_to(&mut self, x: i32) {
        self.raw = x - self.grab;
    }

    /// Where the item is painted, pinned inside `[min_left, max_left]`.
    pub fn left(&self, min_left: i32, max_left: i32) -> i32 {
        self.raw.clamp(min_left, max_left)
    }

    /// Adopt a new (post-reorder) index for the held item.
    pub fn set_from(&mut self, i: usize) {
        self.from = i;
    }

    /// Centre of the held item on the row.
    pub fn center(&self, item_w: i32) -> i32 {
        self.raw + item_w / 2
    }

    /// Where the held item lands, or `None` when it should stay put.
    pub fn landing(&self, item_w: i32, gap: i32, x0: i32, n: usize) -> Option<usize> {
        landing(self.center(item_w), item_w, gap, x0, n, self.from)
    }
}

/// Centre-line landing rule as pure geometry. Slot `i` occupies
/// `x0 + i * (item_w + gap)`; `center` is the held item's centre.
///
/// The scan yields an *insertion slot* (0..=n). An item that has not moved
/// resolves to its own slot or the one right after it; the correction folds
/// both back to "no change" so the first pixel of a drag does nothing.
pub fn landing(
    center: i32,
    item_w: i32,
    gap: i32,
    x0: i32,
    n: usize,
    from: usize,
) -> Option<usize> {
    if n < 2 || from >= n {
        return None;
    }
    let mid = |i: usize| x0 + i as i32 * (item_w + gap) + item_w / 2;
    let mut insert = n;
    for i in 0..n {
        if center < mid(i) {
            insert = i;
            break;
        }
    }
    let to = if insert > from { insert - 1 } else { insert };
    (to != from && to < n).then_some(to)
}

/// Has the pointer crossed the system drag threshold since the press?
pub fn crossed(press_x: i32, x: i32, threshold: i32) -> bool {
    crossed_2d(press_x, 0, x, 0, threshold)
}

/// 2D version for drags that can leave the strip vertically (tab detach).
/// `SM_CXDRAG` is only the horizontal threshold Windows exposes, but a pull
/// straight up/down must start a drag too — using x alone makes vertical
/// detach feel dead. Chebyshev distance matches user32's box-like threshold.
pub fn crossed_2d(press_x: i32, press_y: i32, x: i32, y: i32, threshold: i32) -> bool {
    let t = threshold.max(1);
    (x - press_x).abs() >= t || (y - press_y).abs() >= t
}

#[cfg(test)]
mod tests {
    use super::*;

    const X0: i32 = 28;
    const W: i32 = 100;
    const GAP: i32 = 4;

    fn mid(i: usize) -> i32 {
        X0 + i as i32 * (W + GAP) + W / 2
    }

    #[test]
    fn resting_in_own_slot_is_not_a_move() {
        for from in 0..3 {
            assert_eq!(landing(mid(from), W, GAP, X0, 3, from), None, "from={from}");
        }
    }

    #[test]
    fn swaps_only_after_crossing_neighbour_centre() {
        assert_eq!(landing(mid(1) - 1, W, GAP, X0, 3, 0), None);
        assert_eq!(landing(mid(1), W, GAP, X0, 3, 0), Some(1));
    }

    #[test]
    fn both_ends_are_reachable_and_single_item_never_moves() {
        assert_eq!(landing(mid(0) - 1, W, GAP, X0, 3, 1), Some(0));
        assert_eq!(landing(mid(2) + 1, W, GAP, X0, 3, 0), Some(2));
        assert_eq!(landing(mid(0), W, GAP, X0, 1, 0), None);
        assert_eq!(landing(mid(0), W, GAP, X0, 3, 3), None);
    }

    #[test]
    fn drag_geometry_is_parameterised_by_origin_and_width() {
        // The bookmark bar uses a different origin/width but the same rule.
        let (bx0, bw, bgap) = (8, 92, 4);
        let bmid = |i: usize| bx0 + i as i32 * (bw + bgap) + bw / 2;
        assert_eq!(landing(bmid(0), bw, bgap, bx0, 2, 0), None);
        assert_eq!(landing(bmid(1), bw, bgap, bx0, 2, 0), Some(1));
        assert_eq!(landing(bmid(0) - 1, bw, bgap, bx0, 2, 1), Some(0));
    }

    #[test]
    fn drag_left_is_clamped_but_landing_uses_raw() {
        let mut d = Drag::new(1, 100, 90); // grab = 10
        d.move_to(20); // raw = 10, left of the row
        assert_eq!(d.left(X0, 500), X0, "painting pinned inside the row");
        assert_eq!(
            d.landing(W, GAP, X0, 3),
            Some(0),
            "but landing decided on the raw position"
        );
    }

    #[test]
    fn threshold_is_symmetric_and_never_zero() {
        assert!(!crossed(100, 102, 4));
        assert!(crossed(100, 104, 4));
        assert!(crossed(100, 96, 4));
        assert!(crossed(100, 101, 0), "threshold 0 is clamped to 1");
    }

    #[test]
    fn vertical_pull_starts_a_2d_drag() {
        assert!(!crossed_2d(100, 200, 100, 203, 4));
        assert!(crossed_2d(100, 200, 100, 204, 4), "pure vertical pull");
        assert!(crossed_2d(100, 200, 100, 196, 4));
        assert!(crossed_2d(100, 200, 104, 200, 4), "horizontal still works");
    }
}
