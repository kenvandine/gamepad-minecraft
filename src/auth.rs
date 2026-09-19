// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! RFC 8628 device-code sign-in and the online/offline session state
//! machine. See PLAN.md §3 for the full design: network access is
//! required for the very first login, but a previously cached profile
//! (see `account.rs`) allows fully offline play afterward.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;

use crate::account::CachedAccount;

const CLIENT_ID: &str = "TODO-azure-app-client-id";
const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MOJANG_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MOJANG_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MsaToken {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct XblToken {
    #[serde(rename = "Token")]
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct XstsToken {
    #[serde(rename = "Token")]
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McProfile {
    pub id: String,
    pub name: String,
}

/// The launcher's current sign-in state. See PLAN.md §3 for the full
/// transition rules (when silent refresh runs vs. is skipped, etc).
#[derive(Debug, Clone)]
pub enum AuthState {
    /// Startup only, before the account cache has been read.
    Resolving,
    /// No active session. `cached` is `Some` when offline play is possible.
    NeedsLogin { cached: Option<CachedAccount> },
    /// Device code issued, waiting for the user to approve on their phone.
    AwaitingUser {
        qr_data: String,
        user_code: String,
        verification_uri: String,
        expires_at: Instant,
    },
    /// Fresh or successfully refreshed online session.
    LoggedIn { profile: McProfile },
    /// Deliberately or forcibly not validating online.
    OfflinePlaying { profile: CachedAccount },
    Error {
        message: String,
        cached: Option<CachedAccount>,
    },
}

#[derive(Debug, Clone)]
pub enum AuthEvent {
    DeviceCodeReady(DeviceCodeResponse),
    LoginSucceeded { msa: MsaToken, profile: McProfile },
    LoginFailed(String),
    Cancelled,
}

/// POSTs to the device-code endpoint. Requires network; there is nothing
/// to fall back to here since there is no session yet to cache from.
pub fn request_device_code() -> Result<DeviceCodeResponse, crate::net::HttpError> {
    crate::net::post_form(
        DEVICE_CODE_URL,
        &[("client_id", CLIENT_ID), ("scope", "XboxLive.signin offline_access")],
    )
}

/// Polls the token endpoint at `interval` until the user approves, the
/// device code expires, or `cancel` is set. Runs on a background thread;
/// never call this from the GTK main loop.
pub fn poll_for_token(
    _device_code: &str,
    _interval_secs: u64,
    _cancel: Arc<AtomicBool>,
) -> Result<MsaToken, crate::net::HttpError> {
    // TODO: loop { if cancel.load(Relaxed) { return Err(..) }; post to
    // TOKEN_URL with grant_type=device_code; on authorization_pending,
    // sleep(interval) and retry; on success, return the MsaToken. }
    todo!("poll TOKEN_URL until approval, expiry, or cancellation")
}

/// Exchanges an MSA access token for an Xbox Live token.
pub fn exchange_xbl(_msa_access_token: &str) -> Result<XblToken, crate::net::HttpError> {
    // TODO: POST XBL_AUTH_URL with the RPS ticket built from msa_access_token.
    todo!("XBL_AUTH_URL exchange")
}

/// Exchanges an Xbox Live token for an XSTS token.
pub fn exchange_xsts(_xbl_token: &str) -> Result<XstsToken, crate::net::HttpError> {
    // TODO: POST XSTS_AUTH_URL with the XBL token.
    todo!("XSTS_AUTH_URL exchange")
}

/// Exchanges an XSTS token for a Mojang bearer token, then fetches the
/// player profile.
pub fn exchange_mojang(_xsts_token: &str, _user_hash: &str) -> Result<McProfile, crate::net::HttpError> {
    // TODO: POST MOJANG_LOGIN_URL, then GET MOJANG_PROFILE_URL with the
    // resulting bearer token.
    let _ = MOJANG_LOGIN_URL;
    let _ = MOJANG_PROFILE_URL;
    todo!("Mojang token exchange + profile fetch")
}

/// Attempts a silent refresh using a cached refresh token. Never blocks
/// the caller's UI — a failure here just means offline play continues,
/// it is not surfaced as an error (see PLAN.md §3).
pub fn refresh_silently(_refresh_token: &str) -> Result<MsaToken, crate::net::HttpError> {
    // TODO: POST TOKEN_URL with grant_type=refresh_token.
    todo!("silent refresh via TOKEN_URL")
}

#[cfg(test)]
mod tests {
    // TODO: state-machine transition tests once AuthState transitions are
    // wired to real events (NeedsLogin -> AwaitingUser -> LoggedIn, and
    // the offline fallbacks from Error/AwaitingUser).
}
