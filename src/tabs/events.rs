//! Actions + centralized keymap (spec §3.5: no scattered hotkeys).
//! Pure logic: no `windows` imports (M0_Tech_Design.md 铁律).
//! Detection is EVENT-DRIVEN: called from WM_KEYDOWN with (vk, modifiers).
//! GetAsyncKeyState polling is banned (design review: misses keys, races
//! with Shell child-window focus).

/// Modifier/VK constants (Win32 values kept local so this module stays pure).
pub mod vk {
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const TAB: u16 = 0x09;
    pub const T: u16 = 0x54;
    pub const W: u16 = 0x57;
}

/// Named actions. Feature code subscribes to these, never to raw keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
}

/// One key binding: Ctrl/Shift/Alt + VK code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub vk: u16,
}

impl Binding {
    pub const fn new(ctrl: bool, shift: bool, alt: bool, vk: u16) -> Self {
        Self { ctrl, shift, alt, vk }
    }

    /// Match against a single key event (WM_KEYDOWN payload).
    /// Keymap below guarantees binding uniqueness by construction.
    pub fn matches(&self, ctrl: bool, shift: bool, alt: bool, vk: u16) -> bool {
        self.ctrl == ctrl && self.shift == shift && self.alt == alt && self.vk == vk
    }

    /// ACCEL.fVirt bits for building a Win32 accelerator table.
    /// FVIRTKEY=0x01 (key is a VK, not a char) + modifier flags:
    /// FVIRT_SHIFT=0x04, FVIRT_CONTROL=0x08, FVIRT_ALT=0x10.
    /// Missing FVIRTKEY makes every binding silently unmatchable.
    pub fn fvirt(&self) -> u8 {
        let mut v = 0x01u8; // FVIRTKEY
        if self.shift { v |= 0x04; }
        if self.ctrl { v |= 0x08; }
        if self.alt { v |= 0x10; }
        v
    }
}

/// Central keymap. M0: defaults here; later loaded from config keymap table (spec §3.5).
pub fn default_keymap() -> Vec<(Binding, Action)> {
    vec![
        (Binding::new(true, false, false, vk::T), Action::NewTab),
        (Binding::new(true, false, false, vk::W), Action::CloseTab),
        (Binding::new(true, false, false, vk::TAB), Action::NextTab),
        (Binding::new(true, true, false, vk::TAB), Action::PrevTab),
    ]
}

/// Event-driven match: called once per WM_KEYDOWN.
pub fn match_key(keymap: &[(Binding, Action)], ctrl: bool, shift: bool, alt: bool, vk: u16) -> Option<Action> {
    keymap.iter()
        .find(|(b, _)| b.matches(ctrl, shift, alt, vk))
        .map(|(_, a)| *a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_event_driven() {
        let km = default_keymap();
        assert_eq!(match_key(&km, true, false, false, vk::T), Some(Action::NewTab));
        assert_eq!(match_key(&km, true, true, false, vk::TAB), Some(Action::PrevTab));
        assert_eq!(match_key(&km, false, false, false, vk::T), None);
    }

    #[test]
    fn bindings_unique() {
        let km = default_keymap();
        for (i, (a, _)) in km.iter().enumerate() {
            for (b, _) in km.iter().skip(i + 1) {
                assert!(
                    !(a.ctrl == b.ctrl && a.shift == b.shift && a.alt == b.alt && a.vk == b.vk),
                    "duplicate binding"
                );
            }
        }
    }

    #[test]
    fn fvirt_bits() {
        assert_eq!(Binding::new(true, false, false, 0).fvirt(), 0x09); // FVIRTKEY|FCONTROL
        assert_eq!(Binding::new(true, true, false, 0).fvirt(), 0x0D);
    }
}
