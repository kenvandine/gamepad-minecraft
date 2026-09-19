// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fabric Loader installation and Controlify mod injection, so
//! Minecraft's own title screen and in-game menus are gamepad-navigable
//! without a keyboard or mouse — see PLAN.md §1.

use serde::Deserialize;

use crate::instance::InstanceMeta;

const FABRIC_LOADER_META_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";

#[derive(Debug, Clone, Deserialize)]
pub struct FabricLoaderMeta {
    pub version: String,
    pub stable: bool,
}

/// Installs the latest stable Fabric Loader for `instance`'s Minecraft
/// version into its instance directory.
pub fn install_fabric_loader(_instance: &InstanceMeta) -> Result<(), crate::net::HttpError> {
    // TODO: GET FABRIC_LOADER_META_URL/{mc_version}, pick the newest
    // `stable: true` entry, download the loader profile, install into
    // InstanceStore::instance_dir(&instance.id).
    let _ = FABRIC_LOADER_META_URL;
    todo!("fetch fabric loader metadata and install into the instance dir")
}

/// Drops the Controlify mod jar into `instance`'s `mods/` directory.
pub fn inject_controlify_mod(_instance: &InstanceMeta) -> Result<(), crate::net::HttpError> {
    // TODO: download the Controlify release matching the instance's
    // Fabric/Minecraft version into instance_dir(&instance.id)/mods/.
    todo!("download and place the Controlify mod jar")
}
