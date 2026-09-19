// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fabric Loader installation and Controlify mod injection, so
//! Minecraft's own title screen and in-game menus are gamepad-navigable
//! without a keyboard or mouse — see PLAN.md §1.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::instance::{InstanceMeta, InstanceStore, LaunchProfile};
use crate::net::HttpError;

const FABRIC_LOADER_META_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";
const MODRINTH_API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Clone, Deserialize)]
struct FabricLoaderEntry {
    loader: FabricLoaderInfo,
}

#[derive(Debug, Clone, Deserialize)]
struct FabricLoaderInfo {
    version: String,
    stable: bool,
}

/// The subset of Fabric's "profile json" (same shape family as Mojang's
/// client manifest, but library entries are Maven coordinates + a repo
/// base URL rather than pre-resolved download URLs - see `maven_path`).
#[derive(Debug, Clone, Deserialize)]
struct FabricProfile {
    #[serde(rename = "mainClass")]
    main_class: String,
    #[serde(default)]
    libraries: Vec<FabricLibrary>,
}

#[derive(Debug, Clone, Deserialize)]
struct FabricLibrary {
    name: String,
    url: String,
}

/// Resolves a Maven coordinate (`group:artifact:version`, optionally
/// with a `:classifier`) to the relative jar path Maven repositories
/// serve it at - the layout Fabric's profile JSON expects callers to
/// derive themselves rather than providing pre-built URLs.
fn maven_path(coordinate: &str) -> Option<String> {
    let parts: Vec<&str> = coordinate.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let (group, artifact, version) = (parts[0], parts[1], parts[2]);
    let group_path = group.replace('.', "/");
    let classifier = parts.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    Some(format!(
        "{group_path}/{artifact}/{version}/{artifact}-{version}{classifier}.jar"
    ))
}

/// Picks the newest stable loader version for `mc_version`, falling
/// back to the newest entry at all if Fabric has no stable build for
/// this Minecraft version yet.
fn pick_loader_version(mc_version: &str) -> Result<String, HttpError> {
    let entries: Vec<FabricLoaderEntry> =
        crate::net::get_json(&format!("{FABRIC_LOADER_META_URL}/{mc_version}"))?;
    entries
        .iter()
        .find(|e| e.loader.stable)
        .or_else(|| entries.first())
        .map(|e| e.loader.version.clone())
        .ok_or_else(|| HttpError(format!("no Fabric Loader build found for Minecraft {mc_version}")))
}

/// Installs Fabric Loader for `instance` and layers it onto the
/// already-written vanilla `LaunchProfile` (main class override, extra
/// libraries appended to the classpath). Must run after
/// `instance::install_instance`, since it edits that profile in place
/// rather than replacing it.
pub fn install_fabric_loader(instance: &InstanceMeta) -> Result<(), HttpError> {
    let loader_version = pick_loader_version(&instance.mc_version)?;
    let profile_url = format!(
        "{FABRIC_LOADER_META_URL}/{}/{loader_version}/profile/json",
        instance.mc_version
    );
    let fabric_profile: FabricProfile = crate::net::get_json(&profile_url)?;

    let instance_dir = InstanceStore::instance_dir(&instance.id);
    let libraries_dir = instance_dir.join("libraries");
    let mut extra_classpath = Vec::new();
    for lib in &fabric_profile.libraries {
        let Some(rel_path) = maven_path(&lib.name) else {
            continue;
        };
        let url = format!("{}/{rel_path}", lib.url.trim_end_matches('/'));
        let dest = libraries_dir.join(&rel_path);
        if !dest.exists() {
            crate::net::download_to_file(&url, &dest, |_, _| {})?;
        }
        extra_classpath.push(format!("libraries/{rel_path}"));
    }

    let mut launch_profile = LaunchProfile::load(&instance.id).map_err(HttpError)?;
    launch_profile.main_class = fabric_profile.main_class;
    launch_profile.classpath.extend(extra_classpath);
    launch_profile.save(&instance.id).map_err(HttpError)?;

    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct ModrinthVersion {
    files: Vec<ModrinthFile>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModrinthFile {
    url: String,
    filename: String,
    primary: bool,
}

/// Downloads the newest Fabric build of a Modrinth project compatible
/// with `mc_version` into `mods_dir`.
fn download_modrinth_mod(slug: &str, mc_version: &str, mods_dir: &Path) -> Result<(), HttpError> {
    let url = format!(
        "{MODRINTH_API}/project/{slug}/version?loaders=[\"fabric\"]&game_versions=[\"{mc_version}\"]"
    );
    let versions: Vec<ModrinthVersion> = crate::net::get_json(&url)?;
    let version = versions.first().ok_or_else(|| {
        HttpError(format!("no {slug} build found for Minecraft {mc_version} on Modrinth"))
    })?;
    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .ok_or_else(|| HttpError(format!("{slug} version has no downloadable files")))?;

    fs::create_dir_all(mods_dir).map_err(|e| HttpError(e.to_string()))?;
    crate::net::download_to_file(&file.url, &mods_dir.join(&file.filename), |_, _| {})
}

/// Drops Controlify (and its required YetAnotherConfigLib dependency)
/// into `instance`'s `mods/` directory, fetched from Modrinth by
/// project slug. Requires Fabric to already be installed on `instance`.
pub fn inject_controlify_mod(instance: &InstanceMeta) -> Result<(), HttpError> {
    let mods_dir = InstanceStore::instance_dir(&instance.id).join("mods");
    // Controlify hard-depends on YACL for its config screens; without it
    // Fabric refuses to start with a missing-dependency error.
    download_modrinth_mod("yacl", &instance.mc_version, &mods_dir)?;
    download_modrinth_mod("controlify", &instance.mc_version, &mods_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_path_resolves_group_artifact_version() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.15.11"),
            Some("net/fabricmc/fabric-loader/0.15.11/fabric-loader-0.15.11.jar".to_string())
        );
    }

    #[test]
    fn maven_path_handles_classifiers() {
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-linux"),
            Some("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar".to_string())
        );
    }

    #[test]
    fn maven_path_rejects_malformed_coordinates() {
        assert_eq!(maven_path("not-a-coordinate"), None);
    }
}
