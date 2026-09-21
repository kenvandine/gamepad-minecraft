// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fabric Loader installation and Controlify mod injection, so
//! Minecraft's own title screen and in-game menus are gamepad-navigable
//! without a keyboard or mouse — see PLAN.md §1.

use std::collections::HashSet;
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

/// Fetches the Maven `.sha1` checksum sidecar Maven repositories
/// conventionally publish alongside every artifact (verified live
/// against maven.fabricmc.net while diagnosing this). Some repos omit
/// it for some artifacts, and the sidecar's own body format varies
/// (bare hex, or `sha1sum`-style "hash  filename") - any failure or
/// unparseable body just means "no hash available", falling back to
/// `download_and_verify`'s own truncation check rather than aborting
/// the install over missing-but-optional metadata.
fn fetch_maven_sha1(jar_url: &str) -> String {
    crate::net::get_text(&format!("{jar_url}.sha1"))
        .ok()
        .and_then(|body| body.split_whitespace().next().map(str::to_string))
        .filter(|hash| hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or_default()
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
        // Real integrity check, not just "does a file already exist at
        // this path" - a corrupted/truncated jar left over from an
        // earlier failed attempt was previously treated as "already
        // downloaded" forever, since nothing ever re-verified it.
        let expected_sha1 = fetch_maven_sha1(&url);
        crate::net::download_and_verify(&url, &dest, 0, &expected_sha1)?;
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
    hashes: ModrinthHashes,
}

#[derive(Debug, Clone, Deserialize)]
struct ModrinthHashes {
    sha1: String,
}

/// Downloads the newest Fabric build of a Modrinth project compatible
/// with `mc_version` into `mods_dir`, verified against the SHA-1
/// Modrinth's own API already provides per file (no separate sidecar
/// fetch needed here, unlike the Maven case).
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
    crate::net::download_and_verify(&file.url, &mods_dir.join(&file.filename), 0, &file.hashes.sha1)
}

#[derive(Debug, Clone, Deserialize)]
struct ModrinthVersionGameVersions {
    game_versions: Vec<String>,
}

/// Every Minecraft release Controlify has published a Fabric build for.
/// This launcher is controller-only, so a Minecraft version Controlify
/// hasn't caught up to yet (confirmed live over the 2026-09-19 weekend:
/// 26.3 has no Controlify build at all) isn't a playable choice here -
/// the version picker uses this to keep such versions off the list
/// rather than letting the player hit an install failure after the
/// fact.
pub fn fetch_controlify_supported_versions() -> Result<HashSet<String>, HttpError> {
    let url = format!("{MODRINTH_API}/project/controlify/version?loaders=[\"fabric\"]");
    let versions: Vec<ModrinthVersionGameVersions> = crate::net::get_json(&url)?;
    Ok(versions.into_iter().flat_map(|v| v.game_versions).collect())
}

/// Drops Controlify and its required dependencies (Fabric API,
/// YetAnotherConfigLib) into `instance`'s `mods/` directory, fetched
/// from Modrinth by project slug. Requires Fabric to already be
/// installed on `instance`.
pub fn inject_controlify_mod(instance: &InstanceMeta) -> Result<(), HttpError> {
    let mods_dir = InstanceStore::instance_dir(&instance.id).join("mods");
    // Order matters for the dependency chain: YACL requires fabric-api,
    // and Controlify requires YACL - confirmed live via Fabric's own
    // "Incompatible mods found!" error, which named the missing
    // fabric-api version needed by YACL. Without fabric-api, Fabric
    // refuses to start with a missing-dependency error before the game
    // even gets to load Controlify's or YACL's own code.
    download_modrinth_mod("fabric-api", &instance.mc_version, &mods_dir)?;
    download_modrinth_mod("yacl", &instance.mc_version, &mods_dir)?;
    download_modrinth_mod("controlify", &instance.mc_version, &mods_dir)?;
    seed_controlify_mixed_input(instance)?;
    Ok(())
}

/// Controlify's own config schema version as of the build this launcher
/// currently injects (`ControlifyDataFixer.CURRENT_VERSION`, confirmed
/// live by reading Controlify's own source). Any value in Controlify's
/// "split config" range - above its frozen legacy boundary of 2, up to
/// whatever a future Controlify build's own `CURRENT_VERSION` is - gets
/// transparently upgraded by Controlify's own DataFixerUpper before it's
/// ever read, so this doesn't need bumping just because Controlify ships
/// a newer schema later; it only breaks if a future Controlify build's
/// `CURRENT_VERSION` regresses below this, which schema versions don't.
const CONTROLIFY_SCHEMA_VERSION: u32 = 8;

/// Seeds `instance`'s Controlify config with Mixed Input already on,
/// before Controlify has ever run - so even the player's very first play
/// session is covered by the same fix `launch::ensure_controlify_mixed_input`
/// applies from the second launch onward (see that function's doc comment
/// for why this controller-only device needs Mixed Input forced on at all).
/// A no-op if the instance somehow already has a config (never true for a
/// freshly installed instance, but keeps this safe to call more than once).
///
/// Deliberately writes only `schema_version` and the one `global.mixed_input`
/// key we need changed, rather than hand-authoring Controlify's full config
/// schema: confirmed live by reading `ConfigMigrator`'s own source that any
/// split-schema config missing fields is completed by deep-merging it onto
/// Controlify's own freshly-generated defaults before decoding, so this
/// seed inherits every other default from Controlify itself and survives
/// Controlify adding, removing, or renaming unrelated fields later.
fn seed_controlify_mixed_input(instance: &InstanceMeta) -> Result<(), HttpError> {
    let config_dir = InstanceStore::instance_dir(&instance.id)
        .join("config")
        .join("controlify");
    let path = config_dir.join("controlify.json");
    if path.exists() {
        return Ok(());
    }

    fs::create_dir_all(&config_dir).map_err(|e| HttpError(e.to_string()))?;
    let json = serde_json::to_string_pretty(&controlify_mixed_input_seed())
        .map_err(|e| HttpError(e.to_string()))?;
    fs::write(&path, json).map_err(|e| HttpError(e.to_string()))
}

/// The minimal `controlify.json` seed content itself, split out from
/// `seed_controlify_mixed_input` so it's testable without touching a real
/// instance directory.
fn controlify_mixed_input_seed() -> serde_json::Value {
    serde_json::json!({
        "schema_version": CONTROLIFY_SCHEMA_VERSION,
        "global": {
            "mixed_input": true
        }
    })
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

    #[test]
    fn controlify_mixed_input_seed_sets_mixed_input_true() {
        let seed = controlify_mixed_input_seed();
        assert_eq!(seed["global"]["mixed_input"], true);
        assert_eq!(seed["schema_version"], CONTROLIFY_SCHEMA_VERSION);
    }

    #[test]
    fn controlify_mixed_input_seed_schema_version_is_in_split_schema_range() {
        // Controlify's ConfigMigrator rejects (and refuses to start with)
        // any schema_version <= 2 (its frozen legacy boundary) - confirmed
        // live by reading ConfigMigrator's own source.
        assert!(CONTROLIFY_SCHEMA_VERSION > 2);
    }
}
