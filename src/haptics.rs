// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! A brief rumble on notable events (download complete, launch), played
//! on every connected controller that reports force-feedback support.
//!
//! Not implemented yet — see PLAN.md §8, roadmap phase 6. Port
//! structurally from gamepad-2048's `src/haptics.rs`: `gilrs::ff`
//! EffectBuilder/BaseEffect/Replay, and its lesson that an `Effect`
//! handle must be kept alive past its own play duration or the rumble
//! stops immediately when the handle drops.

use std::cell::RefCell;
use std::rc::Rc;

use gilrs::Gilrs;

/// Gated off by default, same as gamepad-2048's haptics — flip once
/// there's a settings UI to make this a user-facing choice rather than a
/// blunt on/off switch.
const ENABLED: bool = false;

#[derive(Clone)]
pub struct Haptics {
    gilrs: Rc<RefCell<Gilrs>>,
}

impl Haptics {
    pub fn new(gilrs: Rc<RefCell<Gilrs>>) -> Self {
        Haptics { gilrs }
    }

    /// Fires a short pulse on every force-feedback-capable pad. Silently
    /// does nothing if disabled or if no connected pad supports it.
    pub fn pulse(&self) {
        if !ENABLED {
            return;
        }
        let _ = &self.gilrs;
        // TODO: port the EffectBuilder/BaseEffect/Replay rumble and the
        // delayed-drop timing from gamepad-2048's haptics.rs.
    }
}
