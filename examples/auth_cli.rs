// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Manual, no-GTK verification of the device-code -> MSA -> XBL -> XSTS
//! -> Mojang chain (PLAN.md roadmap phase 1). Run with a real Azure
//! public-client app registration (personal Microsoft accounts, no
//! redirect URI needed for the device-code flow):
//!
//! ```sh
//! GAMEPAD_MINECRAFT_CLIENT_ID=<your-app-id> cargo run --example auth_cli
//! ```
//!
//! On success, caches the resulting profile via `account::AccountStore`
//! the same way the real app will, so this also exercises that
//! persistence path end-to-end - re-run it and it'll still work even
//! offline, once `main.rs` grows a code path that reads the cache (this
//! example only ever exercises the online path).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use gamepad_minecraft::account::{AccountStore, CachedAccount};
use gamepad_minecraft::auth;

fn main() {
    if std::env::var("GAMEPAD_MINECRAFT_CLIENT_ID").is_err() {
        eprintln!(
            "GAMEPAD_MINECRAFT_CLIENT_ID is not set. Register a public-client \
             Azure AD app (personal Microsoft accounts, no redirect URI \
             required for the device-code flow) and export its application \
             ID first, e.g.:\n\n  \
             GAMEPAD_MINECRAFT_CLIENT_ID=<your-app-id> cargo run --example auth_cli"
        );
        std::process::exit(1);
    }

    println!("Requesting a device code...");
    let device = match auth::request_device_code() {
        Ok(d) => d,
        Err(e) => fail("Failed to request a device code", &e),
    };

    println!();
    println!("Go to: {}", device.verification_uri);
    println!("Enter code: {}", device.user_code);
    println!();
    print_qr(&auth::qr_payload(&device));
    println!("Waiting for you to approve on your phone or browser...");

    // The CLI has no "cancel" gesture, so the flag never flips - real
    // cancellation (via gamepad B) is exercised once this is wired into
    // main.rs's poll thread instead.
    let cancel = Arc::new(AtomicBool::new(false));
    let msa = match auth::poll_for_token(&device.device_code, device.interval, cancel) {
        Ok(msa) => msa,
        Err(e) => fail("Sign-in failed", &e),
    };

    println!("Signed in to Microsoft. Exchanging for a Minecraft session...");
    let session = match auth::complete_mojang_login(&msa.access_token) {
        Ok(session) => session,
        Err(e) => fail("Minecraft sign-in failed", &e),
    };

    println!(
        "Signed in as {} ({})",
        session.profile.name, session.profile.id
    );

    let mut store = AccountStore::load();
    store.upsert(CachedAccount {
        username: session.profile.name.clone(),
        uuid: session.profile.id.clone(),
        refresh_token: Some(msa.refresh_token.clone()),
        obtained_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    });
    match store.save() {
        Ok(()) => println!("Cached this profile for offline play."),
        Err(e) => eprintln!("Warning: failed to cache the profile: {e}"),
    }
}

fn fail(context: &str, err: &gamepad_minecraft::net::HttpError) -> ! {
    eprintln!("{context}: {err}");
    std::process::exit(1);
}

/// Renders `data` as a QR code directly to the terminal using the
/// `qrcode` crate's built-in Unicode renderer (not feature-gated, so no
/// extra dependency beyond what the app already needs for `qr.rs`).
fn print_qr(data: &str) {
    use qrcode::render::unicode;
    use qrcode::QrCode;

    match QrCode::new(data.as_bytes()) {
        Ok(code) => {
            let image = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
            println!("{image}");
        }
        Err(e) => eprintln!("(couldn't render a QR code: {e})"),
    }
}
