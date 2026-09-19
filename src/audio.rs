// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tiny synthesized sound effects for sign-in / download / launch events.
//!
//! Not implemented yet — see PLAN.md §8, roadmap phase 6. Port
//! structurally from gamepad-2048's `src/audio.rs`: same synth-to-WAV-
//! temp-file approach, and the same constraint that this device's GTK/
//! GStreamer build only supports file-based `MediaFile`s
//! (`for_input_stream` aborts the process there).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::MediaFile;

#[derive(Clone, Copy)]
pub enum Sound {
    LoginSuccess,
    DownloadComplete,
    Error,
    Launch,
}

/// Plays short generated chimes. See gamepad-2048's `Player` for the
/// pool-of-`MediaFile` pattern this should reuse (a `MediaFile` must be
/// kept alive until GTK reports it finished, or playback is silenced
/// immediately).
#[derive(Clone)]
pub struct Player {
    active: Rc<RefCell<Vec<MediaFile>>>,
}

impl Player {
    pub fn new() -> Self {
        Player {
            active: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Plays `sound`. Failures (no writable temp dir, unsupported
    /// backend) degrade silently — audio is a nice-to-have, never
    /// load-bearing.
    pub fn play(&self, _sound: Sound) {
        // TODO: port the sine-wave synth + WAV encoding + MediaFile pool
        // from gamepad-2048's audio.rs, one temp file per Sound variant.
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}
