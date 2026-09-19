// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where this app's persistent data lives.
//!
//! Deliberately `$SNAP_USER_COMMON`, not `$SNAP_USER_DATA`: the latter
//! is versioned per snap revision (`~/snap/<name>/<revision>/`), so
//! every refresh would orphan the previous revision's downloaded
//! Minecraft instances and assets - easily several gigabytes - and
//! force a full re-download. `$SNAP_USER_COMMON`
//! (`~/snap/<name>/common/`) persists across revisions, which is what a
//! cache this size needs. Off-snap, falls back to
//! `$XDG_DATA_HOME`/`~/.local/share`, same as gamepad-2048's
//! `scores.rs`.

use std::path::PathBuf;

pub fn data_root() -> PathBuf {
    let dir = std::env::var("SNAP_USER_COMMON")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(format!("{home}/.local/share"))
        });
    dir.join("gamepad-minecraft")
}
