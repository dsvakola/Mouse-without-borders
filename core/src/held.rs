//! Remembers which keys and mouse buttons are held, and on which computer,
//! so a key or button is always released on the computer where it was pressed.
//! Without this a key could stay "stuck down" after switching computers.

use std::collections::HashSet;

use crate::protocol::Button;

/// A key: scan code plus the "extended" flag.
pub type KeyId = (u16, bool);

/// Where an input event must go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Let this PC handle it.
    Local,
    /// Send it to the computer being controlled (and hide it from this PC).
    Remote,
}

#[derive(Debug, Default)]
pub struct Held {
    local_keys: HashSet<KeyId>,
    remote_keys: HashSet<KeyId>,
    remote_buttons: HashSet<Button>,
}

impl Held {
    /// `remote_active` is true while another computer is being controlled.
    pub fn key(&mut self, key: KeyId, down: bool, remote_active: bool) -> Route {
        if down {
            // A key already held on this PC keeps belonging to this PC (auto-repeat).
            if remote_active && !self.local_keys.contains(&key) {
                self.remote_keys.insert(key);
                Route::Remote
            } else {
                self.local_keys.insert(key);
                Route::Local
            }
        } else if self.local_keys.remove(&key) {
            Route::Local
        } else if self.remote_keys.remove(&key) || remote_active {
            Route::Remote
        } else {
            Route::Local
        }
    }

    pub fn button(&mut self, button: Button, down: bool, remote_active: bool) -> Route {
        if down {
            if remote_active {
                self.remote_buttons.insert(button);
                Route::Remote
            } else {
                Route::Local
            }
        } else if self.remote_buttons.remove(&button) {
            Route::Remote
        } else {
            Route::Local
        }
    }

    /// True while a button pressed on the other computer is still held.
    pub fn remote_button_held(&self) -> bool {
        !self.remote_buttons.is_empty()
    }

    /// Forget stale buttons (a release that never arrived).
    pub fn clear_remote_buttons(&mut self) {
        self.remote_buttons.clear();
    }

    /// The controlled computer is being left: everything still held there
    /// must be released there. Returns what to release.
    pub fn release_remote(&mut self) -> (Vec<KeyId>, Vec<Button>) {
        (
            self.remote_keys.drain().collect(),
            self.remote_buttons.drain().collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: KeyId = (0x1D, false);
    const A: KeyId = (0x1E, false);

    #[test]
    fn hotkey_from_this_pc_does_not_leave_ctrl_stuck_here() {
        let mut h = Held::default();
        // Ctrl pressed on this PC, then the hotkey moves control to another PC.
        assert_eq!(h.key(CTRL, true, false), Route::Local);
        // Auto-repeat and release of Ctrl still belong to this PC.
        assert_eq!(h.key(CTRL, true, true), Route::Local);
        assert_eq!(h.key(CTRL, false, true), Route::Local);
        // Pressed again later, it goes to the other PC.
        assert_eq!(h.key(CTRL, true, true), Route::Remote);
        assert_eq!(h.key(CTRL, false, true), Route::Remote);
    }

    #[test]
    fn keys_held_on_the_other_pc_are_released_when_leaving_it() {
        let mut h = Held::default();
        assert_eq!(h.key(CTRL, true, true), Route::Remote);
        assert_eq!(h.key(A, true, true), Route::Remote);
        assert_eq!(h.button(Button::Left, true, true), Route::Remote);
        let (mut keys, buttons) = h.release_remote();
        keys.sort();
        assert_eq!(keys, vec![CTRL, A]);
        assert_eq!(buttons, vec![Button::Left]);
        // Nothing is left, and the late physical releases stay on this PC.
        assert_eq!(h.release_remote(), (vec![], vec![]));
        assert_eq!(h.key(CTRL, false, false), Route::Local);
        assert_eq!(h.button(Button::Left, false, false), Route::Local);
    }

    #[test]
    fn button_is_released_where_it_was_pressed() {
        let mut h = Held::default();
        assert_eq!(h.button(Button::Left, true, false), Route::Local);
        assert!(!h.remote_button_held());
        // Control jumps away while the button is held: release stays local.
        assert_eq!(h.button(Button::Left, false, true), Route::Local);
        assert_eq!(h.button(Button::Right, true, true), Route::Remote);
        assert!(h.remote_button_held());
        assert_eq!(h.button(Button::Right, false, true), Route::Remote);
        assert!(!h.remote_button_held());
    }
}
