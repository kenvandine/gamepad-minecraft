// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Builds the classpath/JVM args for an installed instance, extracts
//! native libraries, and spawns the bundled `java` binary. Strict
//! confinement forbids exec'ing arbitrary host binaries, so the JRE is
//! staged inside the snap and invoked via its full `$SNAP` path — see
//! PLAN.md §7.

use std::path::PathBuf;

use crate::account::CachedAccount;
use crate::instance::InstanceMeta;

#[derive(Debug, Clone)]
pub enum LaunchEvent {
    Started,
    Exited(i32),
    Crashed(String),
}

/// Path to the JRE staged inside the snap. Falls back to a bare `java`
/// lookup on `$PATH` when not running under snap confinement (local dev).
fn java_binary() -> PathBuf {
    match std::env::var("SNAP") {
        Ok(snap) => PathBuf::from(snap).join("usr/lib/jvm/java-21-openjdk-amd64/bin/java"),
        Err(_) => PathBuf::from("java"),
    }
}

/// Builds the `-cp` classpath: the instance's client jar plus its
/// downloaded libraries (and the Fabric loader's own jars, if present).
pub fn build_classpath(_instance: &InstanceMeta) -> Vec<PathBuf> {
    // TODO: enumerate instance_dir(&instance.id)/libraries plus the
    // client jar; include Fabric loader jars when instance.loader is
    // Loader::Fabric.
    todo!("enumerate client jar + libraries (+ fabric loader) into a classpath")
}

/// Builds the JVM/game argument list, including the account identity.
/// `account` may be an offline-cached profile — no live Mojang session is
/// required to launch (see PLAN.md §3).
pub fn build_jvm_args(_instance: &InstanceMeta, _account: &CachedAccount) -> Vec<String> {
    // TODO: -Djava.library.path=<extracted natives dir>, -cp <classpath>,
    // main class, --username/--uuid/--accessToken (or an offline
    // placeholder token when account.refresh_token is None).
    todo!("assemble JVM + Minecraft launch arguments")
}

/// Extracts the LWJGL "natives" jars for this instance's platform into a
/// per-instance natives directory, referenced by `-Djava.library.path`.
pub fn extract_natives(_instance: &InstanceMeta) -> Result<PathBuf, String> {
    // TODO: unzip each *-natives-linux.jar (via the `zip` crate) into
    // instance_dir(&instance.id)/natives/.
    todo!("extract natives jars for this instance")
}

/// Spawns Minecraft for `instance` as `account`, monitoring the child
/// process and reporting `LaunchEvent`s via `on_event`. Requires the
/// `process-control` plug under strict confinement.
pub fn spawn_minecraft(
    _instance: &InstanceMeta,
    _account: &CachedAccount,
    _on_event: impl FnMut(LaunchEvent) + Send + 'static,
) -> Result<(), String> {
    let _ = java_binary();
    // TODO: std::process::Command::new(java_binary()).args(build_jvm_args
    // (..)).spawn(), then wait() on a background thread and forward
    // LaunchEvent::Exited/Crashed via on_event (bridged to the GTK main
    // loop the same way as net.rs::spawn_blocking).
    todo!("spawn and monitor the java child process")
}
