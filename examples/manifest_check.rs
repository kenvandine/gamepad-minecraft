// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fetches Mojang's version manifest and the latest release's own
//! per-version manifest, and reports what would be installed - without
//! downloading the client jar, libraries, or assets. A cheap way to
//! confirm `instance.rs`'s parsing still matches Mojang's real API
//! shape (their schema does evolve) before running a real multi-hundred-
//! megabyte install.
//!
//! ```sh
//! cargo run --example manifest_check
//! ```

fn main() {
    println!("Fetching version manifest...");
    let manifest = match gamepad_minecraft::instance::fetch_version_manifest() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to fetch version manifest: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "Latest release: {}  (latest snapshot: {})",
        manifest.latest.release, manifest.latest.snapshot
    );
    println!("{} versions listed.", manifest.versions.len());

    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == manifest.latest.release)
        .expect("latest release must be in the version list");

    println!("\nFetching client manifest for {}...", entry.id);
    let body = match reqwest::blocking::get(&entry.url).and_then(|r| r.text()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to fetch client manifest: {e}");
            std::process::exit(1);
        }
    };
    let json: serde_json::Value = serde_json::from_str(&body).expect("client manifest is valid JSON");
    println!(
        "mainClass = {}",
        json.get("mainClass").and_then(|v| v.as_str()).unwrap_or("<missing>")
    );
    println!(
        "assetIndex.id = {}",
        json.get("assetIndex")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
    );
    println!(
        "libraries: {}",
        json.get("libraries").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
    );
    println!(
        "downloads.client.size = {} bytes",
        json.get("downloads")
            .and_then(|d| d.get("client"))
            .and_then(|c| c.get("size"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    println!(
        "has modern `arguments` block: {}",
        json.get("arguments").is_some()
    );

    println!("\nRe-parsing through instance.rs's own types...");
    let mc_version = entry.id.clone();
    let meta = gamepad_minecraft::instance::InstanceMeta {
        id: mc_version.clone(),
        mc_version,
        loader: gamepad_minecraft::instance::Loader::Fabric,
        state: gamepad_minecraft::instance::InstallState::NotInstalled,
    };
    println!(
        "Would install as instance id={:?} (state {:?}) - run `cargo run` and \
         click \"Add Instance\" to actually download it.",
        meta.id, meta.state
    );
}
