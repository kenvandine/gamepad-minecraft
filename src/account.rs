// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Cached-account persistence, structured the same way as gamepad-2048's
//! `scores.rs`: a missing or corrupt file silently falls back to an empty
//! default rather than panicking. See PLAN.md §6 for why this is not
//! encrypted at rest in v1.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A previously-authenticated profile, cached to allow offline play.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedAccount {
    pub username: String,
    /// Mojang profile UUID if this account has ever signed in online;
    /// otherwise an offline-derived UUID (see `launch.rs`).
    pub uuid: String,
    pub refresh_token: Option<String>,
    pub obtained_at: String,
}

/// Cached accounts, keyed by supporting future multi-account switching
/// (the X button).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountStore {
    pub accounts: Vec<CachedAccount>,
    pub active_uuid: Option<String>,
}

impl AccountStore {
    /// Directory for storing account data. See `crate::paths` for why
    /// this is `$SNAP_USER_COMMON`, not `$SNAP_USER_DATA`.
    fn data_dir() -> PathBuf {
        crate::paths::data_root()
    }

    fn store_path() -> PathBuf {
        Self::data_dir().join("accounts.json")
    }

    /// Loads the account store from disk, falling back silently to an
    /// empty store if the file is missing or unparseable.
    pub fn load() -> Self {
        match fs::read_to_string(Self::store_path()) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Saves the account store, restricting the file to owner
    /// read/write (0600) since it may hold a refresh token.
    pub fn save(&self) -> Result<(), String> {
        let data_dir = Self::data_dir();
        fs::create_dir_all(&data_dir).map_err(|e| format!("Failed to create data dir: {}", e))?;

        let contents = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let path = Self::store_path();
        fs::write(&path, contents).map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())
    }

    /// Returns the currently active cached account, if any.
    pub fn active(&self) -> Option<&CachedAccount> {
        let uuid = self.active_uuid.as_ref()?;
        self.accounts.iter().find(|a| &a.uuid == uuid)
    }

    /// Inserts or updates a cached account and marks it active.
    pub fn upsert(&mut self, account: CachedAccount) {
        self.active_uuid = Some(account.uuid.clone());
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.uuid == account.uuid) {
            *existing = account;
        } else {
            self.accounts.push(account);
        }
    }
}

/// The OS username, used as the default identity for true offline play
/// (no Microsoft/Mojang account involved at all) when no account has
/// ever been cached.
pub fn default_offline_username() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Player".to_string())
}

/// Derives the same offline-mode UUID the vanilla client and official
/// launcher use for a name-only (no Mojang account) session:
/// `UUID.nameUUIDFromBytes(("OfflinePlayer:" + username).getBytes(UTF_8))`
/// in Java terms - MD5 of the raw bytes (no RFC 4122 namespace prefix,
/// unlike `Uuid::new_v3`), with the version/variant bits then forced to
/// mark it as a name-based (v3) UUID. Deterministic, so the same
/// username always maps to the same offline identity across runs.
pub fn offline_uuid(username: &str) -> String {
    use md5::{Digest, Md5};

    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{username}").as_bytes());
    let mut bytes: [u8; 16] = hasher.finalize().into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30; // version 3 (name-based, MD5)
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    uuid::Uuid::from_bytes(bytes).simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_is_empty() {
        let store = AccountStore::default();
        assert!(store.accounts.is_empty());
        assert!(store.active().is_none());
    }

    #[test]
    fn upsert_sets_active() {
        let mut store = AccountStore::default();
        store.upsert(CachedAccount {
            username: "Steve".into(),
            uuid: "abc123".into(),
            refresh_token: Some("token".into()),
            obtained_at: "2026-01-01".into(),
        });
        assert_eq!(store.active().unwrap().username, "Steve");
    }

    #[test]
    fn offline_uuid_is_deterministic() {
        assert_eq!(offline_uuid("Steve"), offline_uuid("Steve"));
        assert_ne!(offline_uuid("Steve"), offline_uuid("Alex"));
    }

    #[test]
    fn offline_uuid_has_expected_version_and_variant_bits() {
        let uuid = offline_uuid("Steve");
        assert_eq!(uuid.len(), 32);
        let bytes: Vec<u8> = (0..16)
            .map(|i| u8::from_str_radix(&uuid[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        assert_eq!(bytes[6] & 0xf0, 0x30);
        assert_eq!(bytes[8] & 0xc0, 0x80);
    }
}
