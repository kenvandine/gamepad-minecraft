// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Builds the classpath/JVM args for an installed instance, extracts
//! native libraries, and spawns the bundled `java` binary. Strict
//! confinement forbids exec'ing arbitrary host binaries, so the JRE is
//! staged inside the snap and invoked via its full `$SNAP` path — see
//! PLAN.md §7.

use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::account::CachedAccount;
use crate::instance::{InstanceMeta, InstanceStore, LaunchProfile};

#[derive(Debug, Clone)]
pub enum LaunchEvent {
    Started,
    Exited(i32),
    Crashed(String),
}

/// Path to the JRE staged inside the snap (see snapcraft.yaml for why
/// this tracks the current OpenJDK LTS rather than a fixed old
/// version - current Minecraft releases use JVM arguments older JDKs
/// don't recognize at all). Falls back to a bare `java` lookup on
/// `$PATH` when not running under snap confinement (local dev).
fn java_binary() -> PathBuf {
    match std::env::var("SNAP") {
        Ok(snap) => PathBuf::from(snap).join("usr/lib/jvm/java-25-openjdk-amd64/bin/java"),
        Err(_) => PathBuf::from("java"),
    }
}

/// Resolves `profile.classpath`'s instance-relative paths to absolute
/// paths on disk.
pub fn build_classpath(instance: &InstanceMeta, profile: &LaunchProfile) -> Vec<PathBuf> {
    let dir = InstanceStore::instance_dir(&instance.id);
    profile.classpath.iter().map(|rel| dir.join(rel)).collect()
}

fn classpath_string(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

/// Builds the `${placeholder}` substitution table shared by JVM and
/// game argument templates. `access_token` is `None` for offline play -
/// Minecraft doesn't validate it against Mojang for a local/offline
/// session, so a fixed placeholder is enough (see PLAN.md §3).
fn substitutions(
    instance: &InstanceMeta,
    profile: &LaunchProfile,
    account: &CachedAccount,
    access_token: Option<&str>,
    natives_dir: &std::path::Path,
    classpath: &str,
) -> HashMap<&'static str, String> {
    let mut map = HashMap::new();
    map.insert("auth_player_name", account.username.clone());
    map.insert("auth_uuid", account.uuid.clone());
    map.insert(
        "auth_access_token",
        access_token.unwrap_or("0").to_string(),
    );
    map.insert("auth_xuid", "0".to_string());
    map.insert(
        "user_type",
        if access_token.is_some() { "msa" } else { "legacy" }.to_string(),
    );
    map.insert("version_name", instance.mc_version.clone());
    map.insert("version_type", "release".to_string());
    map.insert(
        "game_directory",
        InstanceStore::instance_dir(&instance.id).display().to_string(),
    );
    map.insert("assets_root", InstanceStore::assets_dir().display().to_string());
    map.insert("assets_index_name", profile.asset_index_id.clone());
    map.insert("natives_directory", natives_dir.display().to_string());
    map.insert("launcher_name", "gamepad-minecraft".to_string());
    map.insert("launcher_version", env!("CARGO_PKG_VERSION").to_string());
    map.insert("classpath", classpath.to_string());
    map.insert("clientid", String::new());
    map
}

fn substitute(template: &str, map: &HashMap<&str, String>) -> String {
    let mut out = template.to_string();
    for (key, value) in map {
        out = out.replace(&format!("${{{key}}}"), value);
    }
    out
}

/// Builds the JVM argument list (`-Djava.library.path=...`, `-cp`, ...),
/// with every `${placeholder}` resolved.
pub fn build_jvm_args(
    instance: &InstanceMeta,
    profile: &LaunchProfile,
    account: &CachedAccount,
    access_token: Option<&str>,
    natives_dir: &std::path::Path,
    classpath: &str,
) -> Vec<String> {
    let map = substitutions(instance, profile, account, access_token, natives_dir, classpath);
    profile
        .jvm_arg_templates
        .iter()
        .map(|t| substitute(t, &map))
        .collect()
}

/// Builds the game argument list (`--username`, `--uuid`, ...), with
/// every `${placeholder}` resolved.
pub fn build_game_args(
    instance: &InstanceMeta,
    profile: &LaunchProfile,
    account: &CachedAccount,
    access_token: Option<&str>,
    natives_dir: &std::path::Path,
    classpath: &str,
) -> Vec<String> {
    let map = substitutions(instance, profile, account, access_token, natives_dir, classpath);
    profile
        .game_arg_templates
        .iter()
        .map(|t| substitute(t, &map))
        .collect()
}

/// Extracts every native-library jar recorded in `profile.natives_jars`
/// (`.so` files, mainly LWJGL) into a per-instance natives directory,
/// referenced by `-Djava.library.path`. Leftover non-native entries
/// (e.g. `META-INF/MANIFEST.MF`) land alongside them harmlessly - the
/// JVM's native loader only looks for the specific `.so` filenames it
/// wants.
pub fn extract_natives(instance: &InstanceMeta, profile: &LaunchProfile) -> Result<PathBuf, String> {
    let instance_dir = InstanceStore::instance_dir(&instance.id);
    let natives_dir = instance_dir.join("natives");
    std::fs::create_dir_all(&natives_dir).map_err(|e| e.to_string())?;

    for rel in &profile.natives_jars {
        let jar_path = instance_dir.join(rel);
        let file = File::open(&jar_path).map_err(|e| format!("{}: {e}", jar_path.display()))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        archive.extract(&natives_dir).map_err(|e| e.to_string())?;
    }
    Ok(natives_dir)
}

/// Forces `fullscreen:true` in the instance's `options.txt`, preserving
/// every other line as-is (a returning player's other settings, if the
/// file already exists from a previous launch). Minecraft reads this at
/// startup; there's no reliable command-line/launch-argument equivalent
/// across versions, and a controller-only launcher can't assume a mouse
/// is available to toggle it manually in Minecraft's own video settings.
fn ensure_fullscreen_option(instance: &InstanceMeta) -> Result<(), String> {
    let path = InstanceStore::instance_dir(&instance.id).join("options.txt");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    fs::write(&path, force_fullscreen_true(&existing)).map_err(|e| e.to_string())
}

/// Pure line-rewriting logic behind `ensure_fullscreen_option`, split
/// out so it's testable without touching a real instance directory.
fn force_fullscreen_true(existing_options_txt: &str) -> String {
    let mut lines: Vec<String> = existing_options_txt.lines().map(str::to_string).collect();
    match lines.iter_mut().find(|line| line.starts_with("fullscreen:")) {
        Some(line) => *line = "fullscreen:true".to_string(),
        None => lines.push("fullscreen:true".to_string()),
    }
    lines.join("\n") + "\n"
}

/// Recursively forces every JSON object key literally named
/// `mixed_input` to `true`. Returns whether anything actually changed.
fn force_mixed_input_true(value: &mut serde_json::Value) -> bool {
    let mut changed = false;
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "mixed_input" {
                    if *v != serde_json::Value::Bool(true) {
                        *v = serde_json::Value::Bool(true);
                        changed = true;
                    }
                } else if force_mixed_input_true(v) {
                    changed = true;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                if force_mixed_input_true(v) {
                    changed = true;
                }
            }
        }
        _ => {}
    }
    changed
}

/// Forces Controlify's "Mixed Input" setting on, so this controller-only
/// device never trips Controlify's own conflict-detection safety valve
/// (`Controlify.java`'s `consecutiveInputSwitches` counter, confirmed
/// live via a real "Controller disabled" toast): touch input arriving as
/// real GLFW mouse events, racing real gamepad input, reads to Controlify
/// as "the controller conflicts with keyboard and mouse" and permanently
/// disables it for that session. "Mixed Input" is Controlify's own
/// designed escape hatch for exactly this - letting a controller and
/// mouse/keyboard coexist without ever forcing an exclusive mode switch.
///
/// A no-op until Controlify has run at least once and written its own
/// config (nothing to patch on a fresh install before the first launch).
/// Deliberately edits only the one key it already knows the name of
/// (`mixed_input`) via a generic JSON walk, rather than authoring or
/// assuming any of Controlify's DataFixerUpper-versioned schema - so it
/// can't fight a future Controlify update's own config migrations the
/// way hand-authoring the whole file would.
fn ensure_controlify_mixed_input(instance: &InstanceMeta) -> Result<(), String> {
    let path = InstanceStore::instance_dir(&instance.id)
        .join("config")
        .join("controlify")
        .join("controlify.json");
    let Ok(existing) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return Ok(());
    };
    if force_mixed_input_true(&mut value) {
        let updated = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        fs::write(&path, updated).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Spawns Minecraft for `instance` as `account` and blocks until it
/// exits, reporting `LaunchEvent`s via `on_event` (`Started` once the
/// process is up, then `Exited`/`Crashed` when it's done). Meant to be
/// called from a background thread the same way `net::spawn_blocking`'s
/// job closures are - this function itself does not spawn one.
/// `access_token` is `None` for offline play.
pub fn spawn_minecraft(
    instance: &InstanceMeta,
    account: &CachedAccount,
    access_token: Option<&str>,
    mut on_event: impl FnMut(LaunchEvent),
) -> Result<(), String> {
    let profile = LaunchProfile::load(&instance.id)
        .map_err(|e| format!("instance is not installed yet: {e}"))?;

    // A controller-first launcher can't assume a mouse is available to
    // dig into Minecraft's own video-settings menu, so force fullscreen
    // in options.txt before every launch rather than relying on the
    // player to have set it manually.
    ensure_fullscreen_option(instance)?;

    // Idempotent no-op until Controlify has run once and written its own
    // config - see that function's doc comment for why this device needs
    // Mixed Input forced on at all.
    ensure_controlify_mixed_input(instance)?;

    let natives_dir = extract_natives(instance, &profile)?;
    let classpath_paths = build_classpath(instance, &profile);
    let classpath = classpath_string(&classpath_paths);

    let jvm_args = build_jvm_args(instance, &profile, account, access_token, &natives_dir, &classpath);
    let game_args = build_game_args(instance, &profile, account, access_token, &natives_dir, &classpath);

    let mut command = Command::new(java_binary());
    command
        .current_dir(InstanceStore::instance_dir(&instance.id))
        .args(&jvm_args)
        .arg(&profile.main_class)
        .args(&game_args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // These handhelds run a Wayland-only compositor with no Xwayland, but
    // the session still exports a stale `DISPLAY=:0` left over from the
    // desktop environment defaults. GLFW's own platform auto-detection
    // sees that and tries X11 first, which then fails to open a display
    // that was never backed by a real X server - crashing every launch
    // with "GLFW X11: Failed to open display :0" even though the real
    // Wayland socket is right there and working. Since a real Wayland
    // socket is the ground truth for whether this session can render at
    // all, prefer it explicitly rather than trust GLFW's own guess.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        command.env_remove("DISPLAY");
        command.env("XDG_SESSION_TYPE", "wayland");
    }

    let mut child = command.spawn().map_err(|e| e.to_string())?;
    on_event(LaunchEvent::Started);

    match child.wait() {
        Ok(status) => match status.code() {
            Some(code) => on_event(LaunchEvent::Exited(code)),
            None => on_event(LaunchEvent::Crashed(
                "process terminated by a signal".to_string(),
            )),
        },
        Err(e) => on_event(LaunchEvent::Crashed(e.to_string())),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::{InstallState, Loader};

    #[test]
    fn force_fullscreen_true_appends_when_missing() {
        let result = force_fullscreen_true("gamma:2.0\nrenderDistance:12");
        assert_eq!(result, "gamma:2.0\nrenderDistance:12\nfullscreen:true\n");
    }

    #[test]
    fn force_fullscreen_true_replaces_existing_value() {
        let result = force_fullscreen_true("gamma:2.0\nfullscreen:false\nrenderDistance:12");
        assert_eq!(result, "gamma:2.0\nfullscreen:true\nrenderDistance:12\n");
    }

    #[test]
    fn force_fullscreen_true_handles_empty_file() {
        assert_eq!(force_fullscreen_true(""), "fullscreen:true\n");
    }

    #[test]
    fn force_mixed_input_true_flips_nested_false() {
        let mut value: serde_json::Value =
            serde_json::from_str(r#"{"settings":{"global":{"mixed_input":false,"other":1}}}"#)
                .unwrap();
        assert!(force_mixed_input_true(&mut value));
        assert_eq!(value["settings"]["global"]["mixed_input"], true);
        assert_eq!(value["settings"]["global"]["other"], 1);
    }

    #[test]
    fn force_mixed_input_true_is_noop_when_already_true() {
        let mut value: serde_json::Value =
            serde_json::from_str(r#"{"mixed_input":true}"#).unwrap();
        assert!(!force_mixed_input_true(&mut value));
    }

    #[test]
    fn force_mixed_input_true_is_noop_when_key_absent() {
        let mut value: serde_json::Value = serde_json::from_str(r#"{"other":"value"}"#).unwrap();
        assert!(!force_mixed_input_true(&mut value));
    }

    #[test]
    fn force_mixed_input_true_finds_key_inside_arrays() {
        let mut value: serde_json::Value =
            serde_json::from_str(r#"{"profiles":[{"mixed_input":false}]}"#).unwrap();
        assert!(force_mixed_input_true(&mut value));
        assert_eq!(value["profiles"][0]["mixed_input"], true);
    }

    fn test_instance() -> InstanceMeta {
        InstanceMeta {
            id: "test".to_string(),
            mc_version: "1.21".to_string(),
            loader: Loader::Fabric,
            state: InstallState::Installed,
        }
    }

    fn test_profile() -> LaunchProfile {
        LaunchProfile {
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".to_string(),
            classpath: vec!["client.jar".to_string()],
            natives_jars: vec![],
            jvm_arg_templates: vec![
                "-Djava.library.path=${natives_directory}".to_string(),
                "-cp".to_string(),
                "${classpath}".to_string(),
            ],
            game_arg_templates: vec![
                "--username".to_string(),
                "${auth_player_name}".to_string(),
                "--uuid".to_string(),
                "${auth_uuid}".to_string(),
                "--accessToken".to_string(),
                "${auth_access_token}".to_string(),
            ],
            asset_index_id: "17".to_string(),
        }
    }

    fn test_account() -> CachedAccount {
        CachedAccount {
            username: "Steve".to_string(),
            uuid: "abc123".to_string(),
            refresh_token: None,
            obtained_at: "2026-01-01".to_string(),
        }
    }

    #[test]
    fn game_args_substitute_identity() {
        let instance = test_instance();
        let profile = test_profile();
        let account = test_account();
        let args = build_game_args(
            &instance,
            &profile,
            &account,
            None,
            std::path::Path::new("/tmp/natives"),
            "cp",
        );
        assert_eq!(
            args,
            vec![
                "--username", "Steve", "--uuid", "abc123", "--accessToken", "0",
            ]
        );
    }

    #[test]
    fn jvm_args_substitute_classpath_and_natives() {
        let instance = test_instance();
        let profile = test_profile();
        let account = test_account();
        let args = build_jvm_args(
            &instance,
            &profile,
            &account,
            Some("real-token"),
            std::path::Path::new("/tmp/natives"),
            "/a.jar:/b.jar",
        );
        assert_eq!(
            args,
            vec![
                "-Djava.library.path=/tmp/natives".to_string(),
                "-cp".to_string(),
                "/a.jar:/b.jar".to_string(),
            ]
        );
    }

    #[test]
    fn online_access_token_is_used_when_present() {
        let instance = test_instance();
        let profile = test_profile();
        let account = test_account();
        let args = build_game_args(
            &instance,
            &profile,
            &account,
            Some("real-token"),
            std::path::Path::new("/tmp"),
            "cp",
        );
        assert!(args.contains(&"real-token".to_string()));
    }
}
