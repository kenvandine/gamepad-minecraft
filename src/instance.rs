// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Installed Minecraft instances: which versions are on disk, and
//! fetching/installing new ones from Mojang's version manifest.
//!
//! Minecraft Java Edition is never bundled in this snap (Mojang's EULA
//! forbids redistributing it) — every instance is downloaded on demand,
//! which means the very first "Add Instance" requires network access.
//! Multiple instances (different versions) can be installed side by
//! side and switched between freely; see PLAN.md §5.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionManifestEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifestEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Loader {
    Vanilla,
    Fabric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallState {
    NotInstalled,
    Downloading,
    Installed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceMeta {
    pub id: String,
    pub mc_version: String,
    pub loader: Loader,
    pub state: InstallState,
}

/// The set of installed instances, persisted the same way as
/// `account.rs`/gamepad-2048's `scores.rs`: a missing/corrupt file falls
/// back silently to an empty list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstanceStore {
    pub instances: Vec<InstanceMeta>,
}

impl InstanceStore {
    fn data_dir() -> PathBuf {
        let dir = std::env::var("SNAP_USER_DATA")
            .or_else(|_| std::env::var("XDG_DATA_HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(format!("{}/.local/share", home))
            });
        dir.join("gamepad-minecraft")
    }

    fn store_path() -> PathBuf {
        Self::data_dir().join("instances.json")
    }

    /// Where a given instance's files (jar, libraries, assets, mods) live.
    pub fn instance_dir(id: &str) -> PathBuf {
        Self::data_dir().join("instances").join(id)
    }

    pub fn load() -> Self {
        match fs::read_to_string(Self::store_path()) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let data_dir = Self::data_dir();
        fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data dir: {}", e))?;
        let contents = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(Self::store_path(), contents).map_err(|e| e.to_string())
    }
}

/// Fetches Mojang's version manifest. Requires network; there is no
/// offline fallback for *discovering* new versions (installed instances
/// still launch offline fine, see `launch.rs`).
pub fn fetch_version_manifest() -> Result<VersionManifest, crate::net::HttpError> {
    crate::net::get_json(VERSION_MANIFEST_URL)
}

/// Downloads and installs the client jar, libraries, and assets for
/// `meta` into its instance directory, reporting progress as
/// `(bytes_done, bytes_total)`.
pub fn install_instance(
    _meta: &InstanceMeta,
    _on_progress: impl FnMut(u64, u64),
) -> Result<(), crate::net::HttpError> {
    // TODO: fetch the per-version manifest at meta's manifest URL, then
    // download the client jar, referenced libraries, and asset index into
    // InstanceStore::instance_dir(&meta.id), verifying each against its
    // sha1 (see the `sha2` dependency).
    todo!("download client jar + libraries + assets, verify against sha1s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_is_empty() {
        assert!(InstanceStore::default().instances.is_empty());
    }
}
