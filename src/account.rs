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
    /// Directory for storing account data. Snap sets `$SNAP_USER_DATA`;
    /// falls back to `$XDG_DATA_HOME`/`~/.local/share` off-snap.
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
}
