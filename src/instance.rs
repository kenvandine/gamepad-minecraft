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
//!
//! Scope note: this targets modern releases only (roughly 1.17+, the
//! range Fabric supports and the bundled JRE can execute - see
//! snapcraft.yaml for which OpenJDK version that currently is). Pre-1.13
//! legacy argument strings and the pre-1.7 "virtual" legacy assets
//! layout are intentionally not handled - this launcher isn't trying to
//! be a full historical-version manager.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::net::HttpError;

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
    /// See `crate::paths` for why this is `$SNAP_USER_COMMON`, not
    /// `$SNAP_USER_DATA` (the latter is versioned per snap revision,
    /// which would orphan every previously-downloaded instance on
    /// each refresh).
    fn data_dir() -> PathBuf {
        crate::paths::data_root()
    }

    fn store_path() -> PathBuf {
        Self::data_dir().join("instances.json")
    }

    /// Where a given instance's files (jar, libraries, assets, mods) live.
    pub fn instance_dir(id: &str) -> PathBuf {
        Self::data_dir().join("instances").join(id)
    }

    /// Shared asset store, deduped by content hash across every
    /// instance rather than per-instance (the same asset objects are
    /// reused across Minecraft versions).
    pub fn assets_dir() -> PathBuf {
        Self::data_dir().join("assets")
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
pub fn fetch_version_manifest() -> Result<VersionManifest, HttpError> {
    crate::net::get_json(VERSION_MANIFEST_URL)
}

// ─── Mojang's per-version "client" manifest ─────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    /// Presence alone (regardless of its content) means this rule is
    /// gated on a player-chosen feature (demo mode, custom resolution,
    /// quick-play, ...) that this launcher never sets - see module docs.
    #[serde(default)]
    pub features: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
}

/// Evaluates a Mojang-style rule list against this launcher's fixed
/// environment (Linux, no demo mode, no custom resolution, no
/// quick-play). Empty rule lists are always allowed - that's the common
/// case for platform-agnostic entries.
pub(crate) fn rules_allow(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        let os_matches = rule
            .os
            .as_ref()
            .and_then(|os| os.name.as_deref())
            .map(|name| name == "linux")
            .unwrap_or(true);
        let matches = os_matches && rule.features.is_none();
        match (rule.action.as_str(), matches) {
            ("allow", true) => allowed = true,
            ("disallow", true) => allowed = false,
            _ => {}
        }
    }
    allowed
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DownloadEntry {
    pub url: String,
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<DownloadEntry>,
    #[serde(default)]
    pub classifiers: Option<HashMap<String, DownloadEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Library {
    #[serde(default)]
    pub rules: Vec<Rule>,
    pub downloads: LibraryDownloads,
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum ArgValue {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ConditionalArg {
    #[serde(default)]
    pub rules: Vec<Rule>,
    pub value: ArgValue,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum ArgEntry {
    Plain(String),
    Conditional(ConditionalArg),
}

/// Flattens a modern (1.13+) argument list down to the plain strings
/// that survive rule evaluation, in order.
pub(crate) fn flatten_args(entries: &[ArgEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            ArgEntry::Plain(s) => out.push(s.clone()),
            ArgEntry::Conditional(cond) => {
                if rules_allow(&cond.rules) {
                    match &cond.value {
                        ArgValue::One(s) => out.push(s.clone()),
                        ArgValue::Many(list) => out.extend(list.iter().cloned()),
                    }
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Arguments {
    #[serde(default)]
    pub game: Vec<ArgEntry>,
    #[serde(default)]
    pub jvm: Vec<ArgEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AssetIndexRef {
    pub id: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ClientManifest {
    #[serde(rename = "mainClass")]
    pub main_class: String,
    pub downloads: ClientDownloads,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(rename = "assetIndex")]
    pub asset_index: AssetIndexRef,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(rename = "minecraftArguments", default)]
    pub minecraft_arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ClientDownloads {
    pub client: DownloadEntry,
}

#[derive(Debug, Clone, Deserialize)]
struct AssetIndexFile {
    objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

// ─── The resolved, launch-ready profile written after install ───────

/// Everything `launch.rs` needs to build a JVM invocation, with every
/// manifest/Maven-coordinate detail already resolved at install time.
/// Argument templates keep their `${placeholder}` tokens - those are
/// substituted at launch time with live account/session values, not
/// baked in here, so the same installed instance can be launched online
/// or offline without reinstalling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LaunchProfile {
    pub main_class: String,
    /// Paths relative to the instance directory: the client jar plus
    /// every downloaded library (vanilla and, if present, Fabric's own).
    pub classpath: Vec<String>,
    /// Native-library jars (relative paths) to extract before launch.
    pub natives_jars: Vec<String>,
    pub jvm_arg_templates: Vec<String>,
    pub game_arg_templates: Vec<String>,
    pub asset_index_id: String,
}

impl LaunchProfile {
    fn path(instance_id: &str) -> PathBuf {
        InstanceStore::instance_dir(instance_id).join("launch_profile.json")
    }

    pub fn load(instance_id: &str) -> Result<Self, String> {
        let contents = fs::read_to_string(Self::path(instance_id)).map_err(|e| e.to_string())?;
        serde_json::from_str(&contents).map_err(|e| e.to_string())
    }

    pub fn save(&self, instance_id: &str) -> Result<(), String> {
        let contents = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(Self::path(instance_id), contents).map_err(|e| e.to_string())
    }
}

/// Downloads and installs the client jar, libraries, and assets for
/// `meta` into its instance directory, then writes the base
/// (vanilla-only) `LaunchProfile`. If `meta.loader` is `Fabric`,
/// `fabric::install_fabric_loader` must be called afterward to layer the
/// loader's own libraries and mainClass override on top.
pub fn install_instance(
    meta: &InstanceMeta,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), HttpError> {
    let instance_dir = InstanceStore::instance_dir(&meta.id);
    fs::create_dir_all(&instance_dir).map_err(|e| HttpError(e.to_string()))?;

    // 1. Resolve the per-version manifest URL from the version list.
    let manifest = fetch_version_manifest()?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == meta.mc_version)
        .ok_or_else(|| HttpError(format!("unknown Minecraft version {}", meta.mc_version)))?;
    let client_manifest: ClientManifest = crate::net::get_json(&entry.url)?;

    // Fetched here (not down at step 4, where it's used) specifically
    // so its object sizes can go into `total` up front - assets are the
    // overwhelming majority of a real install's bytes. Computing `total`
    // without them let `done` catch up to (and exceed) it the moment
    // asset downloading started, pinning the progress bar at 100% for
    // what's actually the largest and slowest phase.
    let asset_index: AssetIndexFile = crate::net::get_json(&client_manifest.asset_index.url)?;

    let mut total = client_manifest.downloads.client.size;
    let allowed_libraries: Vec<&Library> = client_manifest
        .libraries
        .iter()
        .filter(|lib| rules_allow(&lib.rules))
        .collect();
    for lib in &allowed_libraries {
        if let Some(artifact) = &lib.downloads.artifact {
            total += artifact.size;
        }
        if let (Some(natives), Some(classifiers)) = (&lib.natives, &lib.downloads.classifiers) {
            if let Some(key) = natives.get("linux") {
                if let Some(artifact) = classifiers.get(key) {
                    total += artifact.size;
                }
            }
        }
    }
    total += asset_index.objects.values().map(|o| o.size).sum::<u64>();

    // 2. Client jar.
    let client_jar_rel = "client.jar";
    let client_jar_path = instance_dir.join(client_jar_rel);
    let mut done = 0u64;
    crate::net::download_and_verify(
        &client_manifest.downloads.client.url,
        &client_jar_path,
        client_manifest.downloads.client.size,
        &client_manifest.downloads.client.sha1,
    )?;
    done += client_manifest.downloads.client.size;
    on_progress(done, total);

    // 3. Libraries (+ Linux natives jars, tracked separately for
    // launch.rs to extract).
    let libraries_dir = instance_dir.join("libraries");
    let mut classpath = vec![client_jar_rel.to_string()];
    let mut natives_jars = Vec::new();
    for lib in &allowed_libraries {
        if let Some(artifact) = &lib.downloads.artifact {
            if let Some(rel_path) = &artifact.path {
                let dest = libraries_dir.join(rel_path);
                crate::net::download_and_verify(&artifact.url, &dest, artifact.size, &artifact.sha1)?;
                done += artifact.size;
                on_progress(done, total);
                classpath.push(format!("libraries/{rel_path}"));
            }
        }
        if let (Some(natives), Some(classifiers)) = (&lib.natives, &lib.downloads.classifiers) {
            if let Some(key) = natives.get("linux") {
                if let Some(artifact) = classifiers.get(key) {
                    if let Some(rel_path) = &artifact.path {
                        let dest = libraries_dir.join(rel_path);
                        crate::net::download_and_verify(&artifact.url, &dest, artifact.size, &artifact.sha1)?;
                        done += artifact.size;
                        on_progress(done, total);
                        natives_jars.push(format!("libraries/{rel_path}"));
                    }
                }
            }
        }
    }

    // 4. Asset objects, shared across instances by hash (the index
    // itself was already fetched above, to compute `total`).
    let assets_dir = InstanceStore::assets_dir();
    fs::create_dir_all(assets_dir.join("indexes")).map_err(|e| HttpError(e.to_string()))?;
    fs::write(
        assets_dir
            .join("indexes")
            .join(format!("{}.json", client_manifest.asset_index.id)),
        serde_json::to_string(&asset_index.objects).unwrap_or_default(),
    )
    .map_err(|e| HttpError(e.to_string()))?;
    for object in asset_index.objects.values() {
        let prefix = &object.hash[..2.min(object.hash.len())];
        let dest = assets_dir.join("objects").join(prefix).join(&object.hash);
        let url = format!("https://resources.download.minecraft.net/{prefix}/{}", object.hash);
        crate::net::download_and_verify(&url, &dest, object.size, &object.hash)?;
        done += object.size;
        on_progress(done, total);
    }

    // 5. Base launch profile (vanilla only - fabric.rs layers on top).
    let (jvm_templates, game_templates) = if let Some(args) = &client_manifest.arguments {
        (flatten_args(&args.jvm), flatten_args(&args.game))
    } else {
        // Pre-1.13 versions only ship the legacy space-separated string.
        let legacy = client_manifest.minecraft_arguments.unwrap_or_default();
        (
            vec![
                "-Djava.library.path=${natives_directory}".to_string(),
                "-cp".to_string(),
                "${classpath}".to_string(),
            ],
            legacy.split_whitespace().map(str::to_string).collect(),
        )
    };

    let profile = LaunchProfile {
        main_class: client_manifest.main_class,
        classpath,
        natives_jars,
        jvm_arg_templates: jvm_templates,
        game_arg_templates: game_templates,
        asset_index_id: client_manifest.asset_index.id,
    };
    profile.save(&meta.id).map_err(HttpError)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_is_empty() {
        assert!(InstanceStore::default().instances.is_empty());
    }

    // sha1_hex/file_matches/download_and_verify moved to net.rs (shared
    // with fabric.rs) - see their tests there.

    #[test]
    fn empty_rules_are_allowed() {
        assert!(rules_allow(&[]));
    }

    #[test]
    fn linux_os_rule_is_allowed() {
        let rules = vec![Rule {
            action: "allow".to_string(),
            os: Some(OsRule {
                name: Some("linux".to_string()),
            }),
            features: None,
        }];
        assert!(rules_allow(&rules));
    }

    #[test]
    fn windows_os_rule_is_disallowed() {
        let rules = vec![Rule {
            action: "allow".to_string(),
            os: Some(OsRule {
                name: Some("windows".to_string()),
            }),
            features: None,
        }];
        assert!(!rules_allow(&rules));
    }

    #[test]
    fn feature_gated_rule_is_disallowed() {
        let rules = vec![Rule {
            action: "allow".to_string(),
            os: None,
            features: Some(serde_json::json!({"is_demo_user": true})),
        }];
        assert!(!rules_allow(&rules));
    }

    #[test]
    fn flatten_args_keeps_plain_strings_in_order() {
        let entries = vec![
            ArgEntry::Plain("--username".to_string()),
            ArgEntry::Plain("${auth_player_name}".to_string()),
        ];
        assert_eq!(
            flatten_args(&entries),
            vec!["--username".to_string(), "${auth_player_name}".to_string()]
        );
    }

    #[test]
    fn flatten_args_drops_feature_gated_entries() {
        let entries = vec![ArgEntry::Conditional(ConditionalArg {
            rules: vec![Rule {
                action: "allow".to_string(),
                os: None,
                features: Some(serde_json::json!({"has_custom_resolution": true})),
            }],
            value: ArgValue::Many(vec!["--width".to_string(), "${resolution_width}".to_string()]),
        })];
        assert!(flatten_args(&entries).is_empty());
    }

    #[test]
    fn flatten_args_keeps_matching_conditional_entries() {
        let entries = vec![ArgEntry::Conditional(ConditionalArg {
            rules: vec![Rule {
                action: "allow".to_string(),
                os: Some(OsRule {
                    name: Some("linux".to_string()),
                }),
                features: None,
            }],
            value: ArgValue::One("-Dsome.linux.flag=true".to_string()),
        })];
        assert_eq!(flatten_args(&entries), vec!["-Dsome.linux.flag=true".to_string()]);
    }
}
