// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Library half of the crate, so `examples/` (and future tests) can
//! exercise modules like `auth` directly without a GTK main loop. The
//! `gamepad-minecraft` binary (`src/main.rs`) is a thin consumer of this
//! same library.

pub mod account;
pub mod audio;
pub mod auth;
pub mod fabric;
pub mod haptics;
pub mod input;
pub mod instance;
pub mod launch;
pub mod net;
pub mod paths;
pub mod qr;
