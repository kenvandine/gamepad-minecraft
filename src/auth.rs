// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! RFC 8628 device-code sign-in and the online/offline session state
//! machine. See PLAN.md §3 for the full design: network access is
//! required for the very first login, but a previously cached profile
//! (see `account.rs`) allows fully offline play afterward.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::account::CachedAccount;
use crate::net::HttpError;

const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MOJANG_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MOJANG_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

/// Azure application (public client) ID. Overridable via
/// `GAMEPAD_MINECRAFT_CLIENT_ID` so a real registration can be dropped in
/// without a rebuild; the compiled-in default is a placeholder.
///
/// Deliberately no client *secret* anywhere in this file: the app
/// registration must have "Allow public client flows" enabled (Azure
/// Portal -> Authentication), which is Microsoft's mode for apps that
/// can't keep a secret safe - true of anything shipped as a binary/snap
/// to end users. With that setting on, the device-code and token
/// endpoints accept requests with just a client_id. Adding a secret here
/// would not be a secret at all once compiled into a distributed binary.
const DEFAULT_CLIENT_ID: &str = "89725525-f77c-49e5-9960-77ace559b9a6";

fn client_id() -> String {
    std::env::var("GAMEPAD_MINECRAFT_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Builds the QR-code payload for a device-code response: the
/// verification URI with the user code pre-filled via `otc`, so scanning
/// the code on a phone skips the manual-entry step (PLAN.md §2).
pub fn qr_payload(device: &DeviceCodeResponse) -> String {
    format!("{}?otc={}", device.verification_uri, device.user_code)
}

#[derive(Debug, Clone, Deserialize)]
pub struct MsaToken {
    pub access_token: String,
    pub refresh_token: String,
}

/// The wire shape shared by both the XBL and XSTS `/authenticate`
/// responses.
#[derive(Debug, Clone, Deserialize)]
struct XboxLiveTokenResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Debug, Clone, Deserialize)]
struct DisplayClaims {
    xui: Vec<XuiClaim>,
}

#[derive(Debug, Clone, Deserialize)]
struct XuiClaim {
    uhs: String,
}

fn user_hash_of(resp: XboxLiveTokenResponse, source: &str) -> Result<(String, String), HttpError> {
    let uhs = resp
        .display_claims
        .xui
        .into_iter()
        .next()
        .map(|c| c.uhs)
        .ok_or_else(|| HttpError(format!("{source} response was missing a user hash")))?;
    Ok((resp.token, uhs))
}

#[derive(Debug, Clone)]
pub struct XblToken {
    pub token: String,
    pub user_hash: String,
}

#[derive(Debug, Clone)]
pub struct XstsToken {
    pub token: String,
    pub user_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McProfile {
    pub id: String,
    pub name: String,
}

/// A completed Mojang sign-in: the bearer token needed to launch
/// Minecraft online, plus the resolved player profile.
#[derive(Debug, Clone)]
pub struct MojangSession {
    pub access_token: String,
    pub profile: McProfile,
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
    /// The cached MSA refresh token was just confirmed still valid (one
    /// cheap token-endpoint call) but the XBL/XSTS/Mojang leg of
    /// `start_silent_refresh` hasn't completed yet. Purely a transient UI
    /// state between an optimistic `LoggedIn` and a confirmed one -
    /// nothing else transitions into or out of it.
    Verifying { profile: CachedAccount },
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
    LoginSucceeded { msa: MsaToken, session: MojangSession },
    LoginFailed(String),
    Cancelled,
}

/// POSTs to the device-code endpoint. Requires network; there is nothing
/// to fall back to here since there is no session yet to cache from.
pub fn request_device_code() -> Result<DeviceCodeResponse, HttpError> {
    let id = client_id();
    crate::net::post_form(
        DEVICE_CODE_URL,
        &[("client_id", id.as_str()), ("scope", "XboxLive.signin offline_access")],
    )
}

/// One iteration's worth of interpreting a non-200 token-endpoint
/// response. Split out from `poll_for_token` so it's testable without a
/// network call.
enum PollOutcome {
    Pending,
    SlowDown,
    Failed(String),
}

fn interpret_poll_error(body: &str) -> PollOutcome {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: String,
    }
    let error = serde_json::from_str::<ErrorBody>(body)
        .map(|e| e.error)
        .unwrap_or_else(|_| "unknown_error".to_string());
    match error.as_str() {
        "authorization_pending" => PollOutcome::Pending,
        "slow_down" => PollOutcome::SlowDown,
        "authorization_declined" => PollOutcome::Failed("sign-in was declined".to_string()),
        "expired_token" => PollOutcome::Failed("the device code expired".to_string()),
        other => PollOutcome::Failed(format!("device code sign-in failed: {other}")),
    }
}

/// Polls the token endpoint at `interval_secs` until the user approves,
/// the device code expires, or `cancel` is set. Runs on a background
/// thread; never call this from the GTK main loop, since each iteration
/// blocks on a network round trip and/or a multi-second sleep.
pub fn poll_for_token(
    device_code: &str,
    interval_secs: u64,
    cancel: Arc<AtomicBool>,
) -> Result<MsaToken, HttpError> {
    let id = client_id();
    let mut interval = interval_secs.max(1);

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(HttpError("sign-in cancelled".to_string()));
        }

        let raw = crate::net::post_form_raw(
            TOKEN_URL,
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", id.as_str()),
                ("device_code", device_code),
            ],
        )?;

        if raw.status == 200 {
            return serde_json::from_str(&raw.body).map_err(|e| HttpError(e.to_string()));
        }

        match interpret_poll_error(&raw.body) {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval += 5,
            PollOutcome::Failed(message) => return Err(HttpError(message)),
        }

        if !sleep_cancellable(Duration::from_secs(interval), &cancel) {
            return Err(HttpError("sign-in cancelled".to_string()));
        }
    }
}

/// Sleeps in short increments so `cancel` is noticed promptly instead of
/// only between polls. Returns `false` if cancelled mid-sleep.
fn sleep_cancellable(total: Duration, cancel: &Arc<AtomicBool>) -> bool {
    const STEP: Duration = Duration::from_millis(250);
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let remaining = total - elapsed;
        thread::sleep(remaining.min(STEP));
        elapsed += STEP;
    }
    true
}

/// Exchanges an MSA access token for an Xbox Live token.
pub fn exchange_xbl(msa_access_token: &str) -> Result<XblToken, HttpError> {
    #[derive(Serialize)]
    struct Properties {
        #[serde(rename = "AuthMethod")]
        auth_method: &'static str,
        #[serde(rename = "SiteName")]
        site_name: &'static str,
        #[serde(rename = "RpsTicket")]
        rps_ticket: String,
    }
    #[derive(Serialize)]
    struct Request {
        #[serde(rename = "Properties")]
        properties: Properties,
        #[serde(rename = "RelyingParty")]
        relying_party: &'static str,
        #[serde(rename = "TokenType")]
        token_type: &'static str,
    }

    let body = Request {
        properties: Properties {
            auth_method: "RPS",
            site_name: "user.auth.xboxlive.com",
            rps_ticket: format!("d={msa_access_token}"),
        },
        relying_party: "http://auth.xboxlive.com",
        token_type: "JWT",
    };

    let resp: XboxLiveTokenResponse = crate::net::post_json(XBL_AUTH_URL, &body)?;
    let (token, user_hash) = user_hash_of(resp, "XBL")?;
    Ok(XblToken { token, user_hash })
}

/// Maps a known Xbox Live `XErr` code to a message a player can act on.
/// The two included here are by far the most common real-world failures
/// (a brand-new Microsoft account with no Xbox profile, and a child
/// account outside a Family group); anything else falls back to a
/// generic message rather than guessing.
fn xsts_error_message(x_err: Option<u64>, status: u16) -> String {
    match x_err {
        Some(2148916233) => {
            "This Microsoft account has no Xbox Live profile. Create one at \
             xbox.com, then sign in again."
                .to_string()
        }
        Some(2148916238) => {
            "This Microsoft account is a child account and must be added to \
             a Family group by an adult before it can sign in."
                .to_string()
        }
        Some(code) => format!("Xbox Live rejected this sign-in (error {code})."),
        None => format!("Xbox Live rejected this sign-in (HTTP {status})."),
    }
}

/// Exchanges an Xbox Live token for an XSTS token scoped to Minecraft.
pub fn exchange_xsts(xbl_token: &str) -> Result<XstsToken, HttpError> {
    #[derive(Serialize)]
    struct Properties<'a> {
        #[serde(rename = "SandboxId")]
        sandbox_id: &'static str,
        #[serde(rename = "UserTokens")]
        user_tokens: [&'a str; 1],
    }
    #[derive(Serialize)]
    struct Request<'a> {
        #[serde(rename = "Properties")]
        properties: Properties<'a>,
        #[serde(rename = "RelyingParty")]
        relying_party: &'static str,
        #[serde(rename = "TokenType")]
        token_type: &'static str,
    }

    let body = Request {
        properties: Properties {
            sandbox_id: "RETAIL",
            user_tokens: [xbl_token],
        },
        // Must be the Minecraft-specific relying party, not the generic
        // "http://xboxlive.com" used for other Xbox services.
        relying_party: "rp://api.minecraftservices.com/",
        token_type: "JWT",
    };

    let raw = crate::net::post_json_raw(XSTS_AUTH_URL, &body)?;
    if raw.status != 200 {
        #[derive(Deserialize)]
        struct XstsErrorBody {
            #[serde(rename = "XErr")]
            x_err: Option<u64>,
        }
        let x_err = serde_json::from_str::<XstsErrorBody>(&raw.body)
            .ok()
            .and_then(|e| e.x_err);
        return Err(HttpError(xsts_error_message(x_err, raw.status)));
    }

    let resp: XboxLiveTokenResponse =
        serde_json::from_str(&raw.body).map_err(|e| HttpError(e.to_string()))?;
    let (token, user_hash) = user_hash_of(resp, "XSTS")?;
    Ok(XstsToken { token, user_hash })
}

/// Exchanges an XSTS token for a Mojang bearer token, then fetches the
/// player profile.
pub fn exchange_mojang(xsts_token: &str, user_hash: &str) -> Result<MojangSession, HttpError> {
    #[derive(Serialize)]
    struct Request {
        #[serde(rename = "identityToken")]
        identity_token: String,
    }
    #[derive(Deserialize)]
    struct LoginResponse {
        access_token: String,
    }

    let body = Request {
        identity_token: format!("XBL3.0 x={user_hash};{xsts_token}"),
    };
    let raw = crate::net::post_json_raw(MOJANG_LOGIN_URL, &body)?;
    if raw.status != 200 {
        // Surface Mojang's actual response body rather than a bare
        // "403 Forbidden" - it's the only way to tell a real auth
        // problem apart from e.g. edge/IP-based blocking on their side.
        return Err(HttpError(format!(
            "Mojang sign-in failed (HTTP {}): {}",
            raw.status, raw.body
        )));
    }
    let login: LoginResponse =
        serde_json::from_str(&raw.body).map_err(|e| HttpError(e.to_string()))?;
    let profile: McProfile = crate::net::get_json_bearer(MOJANG_PROFILE_URL, &login.access_token)?;
    Ok(MojangSession {
        access_token: login.access_token,
        profile,
    })
}

/// Runs the XBL -> XSTS -> Mojang chain for an already-obtained MSA
/// access token. This is the single call site the rest of the app should
/// use once it has an `MsaToken` (from `poll_for_token` or
/// `refresh_silently`), rather than chaining the three exchanges itself.
pub fn complete_mojang_login(msa_access_token: &str) -> Result<MojangSession, HttpError> {
    let xbl = exchange_xbl(msa_access_token)?;
    let xsts = exchange_xsts(&xbl.token)?;
    exchange_mojang(&xsts.token, &xsts.user_hash)
}

/// Attempts a silent refresh using a cached refresh token. Never blocks
/// the caller's UI — a failure here just means offline play continues,
/// it is not surfaced as an error (see PLAN.md §3).
pub fn refresh_silently(refresh_token: &str) -> Result<MsaToken, HttpError> {
    let id = client_id();
    crate::net::post_form(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", id.as_str()),
            ("refresh_token", refresh_token),
            ("scope", "XboxLive.signin offline_access"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_payload_appends_otc() {
        let device = DeviceCodeResponse {
            device_code: "dc".into(),
            user_code: "B7DK-9LPQ".into(),
            verification_uri: "https://microsoft.com/link".into(),
            expires_in: 900,
            interval: 5,
        };
        assert_eq!(
            qr_payload(&device),
            "https://microsoft.com/link?otc=B7DK-9LPQ"
        );
    }

    #[test]
    fn parses_msa_token() {
        let json = r#"{"access_token":"a","refresh_token":"r","token_type":"Bearer","expires_in":3600}"#;
        let token: MsaToken = serde_json::from_str(json).unwrap();
        assert_eq!(token.access_token, "a");
        assert_eq!(token.refresh_token, "r");
    }

    #[test]
    fn parses_xbox_live_token_response() {
        let json = r#"{"Token":"t","DisplayClaims":{"xui":[{"uhs":"hash123"}]}}"#;
        let resp: XboxLiveTokenResponse = serde_json::from_str(json).unwrap();
        let (token, uhs) = user_hash_of(resp, "XBL").unwrap();
        assert_eq!(token, "t");
        assert_eq!(uhs, "hash123");
    }

    #[test]
    fn missing_user_hash_is_an_error() {
        let json = r#"{"Token":"t","DisplayClaims":{"xui":[]}}"#;
        let resp: XboxLiveTokenResponse = serde_json::from_str(json).unwrap();
        assert!(user_hash_of(resp, "XBL").is_err());
    }

    #[test]
    fn interprets_authorization_pending() {
        let body = r#"{"error":"authorization_pending"}"#;
        assert!(matches!(interpret_poll_error(body), PollOutcome::Pending));
    }

    #[test]
    fn interprets_expired_token() {
        let body = r#"{"error":"expired_token"}"#;
        assert!(matches!(interpret_poll_error(body), PollOutcome::Failed(_)));
    }

    #[test]
    fn xsts_error_message_for_no_xbox_account() {
        let msg = xsts_error_message(Some(2148916233), 401);
        assert!(msg.contains("no Xbox Live profile"));
    }

    #[test]
    fn xsts_error_message_falls_back_for_unknown_codes() {
        let msg = xsts_error_message(Some(999), 401);
        assert!(msg.contains("999"));
    }

    #[test]
    fn sleep_cancellable_returns_false_when_already_cancelled() {
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(!sleep_cancellable(Duration::from_secs(5), &cancel));
    }
}
