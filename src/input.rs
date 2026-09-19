// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! The single source of truth for gamepad button meaning.
//!
//! gamepad-2048 shipped a `gamepad.rs` button-mapping module that ended up
//! as dead code: `main.rs` reimplemented the same mapping inline against
//! raw `gilrs::Button`, and the two copies could have silently drifted
//! apart. Here, `classify_button` is the *only* place a `gilrs::Button` is
//! matched against meaning — `main.rs` must go through it rather than
//! matching on `gilrs::Button` itself.

use gilrs::Button;

/// A gamepad button press translated into an app-level action.
///
/// Directional navigation from the D-Pad and left stick is handled
/// separately in `main.rs` (it needs axis thresholding and repeat-rate
/// logic, not button-identity mapping, so it doesn't belong in this table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadAction {
    Confirm,
    Back,
    OpenAccounts,
    RefreshOrSettings,
    SystemMenu,
}

/// Maps a physical button press to its app-level meaning.
///
/// Both Legion Go S and MSI Claw use Xbox-style ABXY layouts, so this is a
/// deliberate convention choice, not an oversight: `gilrs` names buttons by
/// physical position, and `South`/`East` line up with Xbox's A/B (confirm/
/// back) on both target devices. A Nintendo-style pad would see this
/// convention reversed from its own labeling; that's an intentionally
/// unaddressed gap for now, same as gamepad-2048's.
pub fn classify_button(button: Button) -> Option<GamepadAction> {
    match button {
        Button::South | Button::Start => Some(GamepadAction::Confirm),
        Button::East | Button::Select => Some(GamepadAction::Back),
        Button::West => Some(GamepadAction::OpenAccounts),
        Button::North => Some(GamepadAction::RefreshOrSettings),
        Button::Mode => Some(GamepadAction::SystemMenu),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn south_is_confirm() {
        assert_eq!(classify_button(Button::South), Some(GamepadAction::Confirm));
    }

    #[test]
    fn east_is_back() {
        assert_eq!(classify_button(Button::East), Some(GamepadAction::Back));
    }

    #[test]
    fn unmapped_button_is_none() {
        assert_eq!(classify_button(Button::C), None);
    }
}
