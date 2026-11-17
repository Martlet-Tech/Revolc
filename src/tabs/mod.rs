//! Tab model: pure logic, no Win32 imports (doc/M0_Tech_Design.md 铁律).

pub mod events;

/// One tab. Shell-view lifecycle lives in shell/; this struct is data only.
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: u64,
    /// Path used to open a new tab here (Ctrl+T). May be stale for a tab the
    /// user navigated away in — ui resolves the live folder before reuse.
    pub path: String,
    /// Display text. Filled from the Explorer window title, which follows
    /// navigation ("此电脑", "下载"), unlike `path` which is fixed at absorb.
    pub label: String,
}

/// Ordered tab list with an active index. Owned exclusively by the main thread.
#[derive(Debug, Default)]
pub struct TabList {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
}

impl TabList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Iterate all tabs in order (for UI snapshots).
    pub fn iter_all(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn get(&self, i: usize) -> Option<&Tab> {
        self.tabs.get(i)
    }

    /// Append a tab and activate it. Returns its id.
    pub fn push(&mut self, path: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab { id, path: path.into(), label: String::new() });
        self.active = self.tabs.len() - 1;
        id
    }

    /// Update a tab's path (used when an absorbed window's folder resolves).
    pub fn set_path(&mut self, i: usize, path: impl Into<String>) {
        if let Some(t) = self.tabs.get_mut(i) {
            t.path = path.into();
        }
    }

    /// Update a tab's display text (Explorer window title; follows navigation).
    /// Returns true when it actually changed, so callers can skip repainting.
    pub fn set_label(&mut self, i: usize, label: impl Into<String>) -> bool {
        let label = label.into();
        match self.tabs.get_mut(i) {
            Some(t) if t.label != label => {
                t.label = label;
                true
            }
            _ => false,
        }
    }

    /// Remove a tab; adjust active index. Returns removed tab.
    pub fn remove(&mut self, i: usize) -> Option<Tab> {
        if i >= self.tabs.len() {
            return None;
        }
        let removed = self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > i {
            self.active -= 1;
        }
        Some(removed)
    }

    pub fn activate(&mut self, i: usize) -> bool {
        if i < self.tabs.len() {
            self.active = i;
            true
        } else {
            false
        }
    }

    pub fn cycle(&mut self, forward: bool) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len();
        self.active = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_activates_new_tab() {
        let mut t = TabList::new();
        let a = t.push("C:\\a");
        let b = t.push("C:\\b");
        assert_eq!(t.active_index(), 1);
        assert_eq!(t.active().unwrap().id, b);
        assert_ne!(a, b);
    }

    #[test]
    fn remove_adjusts_active() {
        let mut t = TabList::new();
        t.push("a");
        t.push("b");
        t.push("c");
        t.activate(2);
        t.remove(2);
        assert_eq!(t.active_index(), 1);
        t.remove(1);
        assert_eq!(t.active_index(), 0);
        t.remove(0);
        assert!(t.is_empty());
    }

    #[test]
    fn cycle_wraps() {
        let mut t = TabList::new();
        t.push("a");
        t.push("b");
        // push auto-activates the new tab, so we start at index 1.
        t.cycle(true);
        assert_eq!(t.active_index(), 0);
        t.cycle(true);
        assert_eq!(t.active_index(), 1);
        t.cycle(false);
        assert_eq!(t.active_index(), 0);
    }
}
