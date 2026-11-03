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
    /// Opens the account-switcher dropdown.
    OpenAccounts,
    /// Opens the installation (version) dropdown.
    OpenVersions,
    /// Shoulder buttons: switch between the Play and Installations tabs.
    PrevTab,
    NextTab,
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
///
/// `swap_xy` corrects the X/Y swap that both target devices' built-in
/// controllers have (see [`swaps_x_and_y`]): pass its result for the
/// gamepad the press came from.
pub fn classify_button(button: Button, swap_xy: bool) -> Option<GamepadAction> {
    // The physical X and Y buttons, after correcting for devices that
    // report them swapped.
    let (x, y) = if swap_xy {
        (Button::North, Button::West)
    } else {
        (Button::West, Button::North)
    };
    match button {
        Button::South | Button::Start => Some(GamepadAction::Confirm),
        Button::East | Button::Select => Some(GamepadAction::Back),
        b if b == x => Some(GamepadAction::OpenAccounts),
        b if b == y => Some(GamepadAction::OpenVersions),
        Button::LeftTrigger => Some(GamepadAction::PrevTab),
        Button::RightTrigger => Some(GamepadAction::NextTab),
        Button::Mode => Some(GamepadAction::SystemMenu),
        _ => None,
    }
}

/// True for controllers whose evdev node reports the X and Y face buttons
/// swapped relative to a genuine Xbox pad: a physical Y press arrives as
/// `BTN_WEST` (gilrs `Button::West`) and a physical X press as `BTN_NORTH`.
///
/// Both target devices' built-in controllers do this, via the same generic
/// Xbox360-compatibility HID descriptor. The IDs mirror gamepad-shell's
/// `msi-claw.toml` and `legion-go-s.toml` device profiles, which apply the
/// same correction to the shell's own input.
pub fn swaps_x_and_y(vendor: Option<u16>, product: Option<u16>) -> bool {
    matches!(
        (vendor, product),
        // MSI Claw
        (Some(0x0db0), Some(0x1901 | 0x1902))
        // Lenovo Legion Go S, Lenovo-VID revision
        | (Some(0x17ef), Some(0x6189 | 0x6182))
        // Lenovo Legion Go S, WCH-MCU revision (xinput / dinput mode)
        | (Some(0x1a86), Some(0xe310 | 0xe311))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn south_is_confirm() {
        assert_eq!(classify_button(Button::South, false), Some(GamepadAction::Confirm));
    }

    #[test]
    fn east_is_back() {
        assert_eq!(classify_button(Button::East, false), Some(GamepadAction::Back));
    }

    #[test]
    fn shoulders_switch_tabs() {
        assert_eq!(classify_button(Button::LeftTrigger, false), Some(GamepadAction::PrevTab));
        assert_eq!(classify_button(Button::RightTrigger, false), Some(GamepadAction::NextTab));
    }

    #[test]
    fn unmapped_button_is_none() {
        assert_eq!(classify_button(Button::C, false), None);
    }

    #[test]
    fn xbox_pad_x_is_accounts_y_is_versions() {
        assert_eq!(classify_button(Button::West, false), Some(GamepadAction::OpenAccounts));
        assert_eq!(classify_button(Button::North, false), Some(GamepadAction::OpenVersions));
    }

    #[test]
    fn swapped_pad_x_is_still_accounts() {
        // On a swapped pad the physical X button arrives as North.
        assert_eq!(classify_button(Button::North, true), Some(GamepadAction::OpenAccounts));
        assert_eq!(classify_button(Button::West, true), Some(GamepadAction::OpenVersions));
    }

    #[test]
    fn target_devices_swap_x_and_y() {
        assert!(swaps_x_and_y(Some(0x0db0), Some(0x1901)));
        assert!(swaps_x_and_y(Some(0x1a86), Some(0xe310)));
        assert!(swaps_x_and_y(Some(0x17ef), Some(0x6189)));
        assert!(!swaps_x_and_y(Some(0x045e), Some(0x028e))); // Xbox 360 pad
        assert!(!swaps_x_and_y(None, None));
    }
}
