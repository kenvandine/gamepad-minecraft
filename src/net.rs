// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bridges blocking network calls to the GTK main loop.
//!
//! Auth polling and downloads run on plain `std::thread`s using
//! `reqwest::blocking` — nothing outside this module ever writes
//! `async`/`.await`. Results cross to the GTK main loop over an
//! `async-channel`, consumed via `glib::spawn_future_local`. This keeps
//! the mental model identical to gamepad-2048's
//! `glib::source::timeout_add_local` polling loop: a background thread
//! does blocking work, then pushes a plain value the main loop picks up.
//!
//! Never call `spawn_blocking`'s job closure from a `glib::timeout_add`
//! callback directly — that would block the main loop (and gamepad input
//! with it) for the duration of the network round trip.

use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct HttpError(pub String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A client for small, bounded requests (auth exchanges, manifest
/// fetches). `reqwest` sets no timeout at all by default, so a stalled
/// connection - a dropped packet an intermediate hop never resets, a
/// server that accepts the connection but never replies - hangs the
/// request forever instead of failing. That turns into a silent,
/// indefinite freeze anywhere this is awaited from a poll loop (e.g.
/// `auth::poll_for_token`), with no error to report and nothing for the
/// caller to react to.
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(20))
        .build()
        .expect("failed to build HTTP client")
}

/// A client for downloads, where an overall request timeout would
/// misfire on a large but healthy transfer. Only the connect phase is
/// bounded; a stalled connect still fails fast instead of hanging.
fn download_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("failed to build HTTP client")
}

/// Runs `job` on a background thread, then delivers its result to `on_done`
/// on the GTK main loop via `glib::spawn_future_local`.
pub fn spawn_blocking<T, F, U>(job: F, on_done: U)
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, HttpError> + Send + 'static,
    U: FnOnce(Result<T, HttpError>) + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(job());
    });
    glib::spawn_future_local(async move {
        if let Ok(result) = rx.recv().await {
            on_done(result);
        }
    });
}

/// Like `spawn_blocking`, but `job` also receives a `report(done, total)`
/// callback it can call as it goes (e.g. `instance::install_instance`'s
/// own `on_progress` parameter) - each call crosses to the GTK main loop
/// on its own channel and drives `on_progress`, independently of the
/// final `on_done` result. A progress bar doesn't need every single
/// update, so a bounded channel with `send_blocking` is fine: it applies
/// gentle backpressure on a slow consumer rather than dropping updates
/// or blocking the GTK main loop (the worker thread blocks briefly, not
/// the UI thread).
pub fn spawn_blocking_with_progress<T, F, G, U>(job: F, on_progress: G, on_done: U)
where
    T: Send + 'static,
    F: FnOnce(&dyn Fn(u64, u64)) -> Result<T, HttpError> + Send + 'static,
    G: Fn(u64, u64) + 'static,
    U: FnOnce(Result<T, HttpError>) + 'static,
{
    let (progress_tx, progress_rx) = async_channel::bounded::<(u64, u64)>(8);
    let (done_tx, done_rx) = async_channel::bounded(1);

    std::thread::spawn(move || {
        let report = |done: u64, total: u64| {
            let _ = progress_tx.send_blocking((done, total));
        };
        let result = job(&report);
        let _ = done_tx.send_blocking(result);
    });

    glib::spawn_future_local(async move {
        while let Ok((done, total)) = progress_rx.recv().await {
            on_progress(done, total);
        }
    });
    glib::spawn_future_local(async move {
        if let Ok(result) = done_rx.recv().await {
            on_done(result);
        }
    });
}

/// GET `url` and deserialize the JSON response as `T`.
pub fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, HttpError> {
    client()
        .get(url)
        .send()
        .map_err(|e| HttpError(e.to_string()))?
        .json::<T>()
        .map_err(|e| HttpError(e.to_string()))
}

/// POST `form` as `application/x-www-form-urlencoded` and deserialize the
/// JSON response as `T`. Used for the device-code and token endpoints.
pub fn post_form<T: DeserializeOwned>(url: &str, form: &[(&str, &str)]) -> Result<T, HttpError> {
    client()
        .post(url)
        .form(form)
        .send()
        .map_err(|e| HttpError(e.to_string()))?
        .json::<T>()
        .map_err(|e| HttpError(e.to_string()))
}

/// A response whose status/body the caller needs to inspect itself,
/// e.g. because a non-2xx response is an expected, meaningful outcome
/// (device-code polling's "authorization_pending") rather than a plain
/// error.
pub struct RawResponse {
    pub status: u16,
    pub body: String,
}

/// POST `form` and return the raw status/body without treating a non-2xx
/// status as an error.
pub fn post_form_raw(url: &str, form: &[(&str, &str)]) -> Result<RawResponse, HttpError> {
    let response = client()
        .post(url)
        .form(form)
        .send()
        .map_err(|e| HttpError(e.to_string()))?;
    let status = response.status().as_u16();
    let body = response.text().map_err(|e| HttpError(e.to_string()))?;
    Ok(RawResponse { status, body })
}

/// POST a JSON body and return the raw status/body without treating a
/// non-2xx status as an error.
pub fn post_json_raw<B: Serialize>(url: &str, body: &B) -> Result<RawResponse, HttpError> {
    let response = client()
        .post(url)
        .header("Accept", "application/json")
        .json(body)
        .send()
        .map_err(|e| HttpError(e.to_string()))?;
    let status = response.status().as_u16();
    let body = response.text().map_err(|e| HttpError(e.to_string()))?;
    Ok(RawResponse { status, body })
}

/// POST a JSON body and deserialize the response as `T`. Any non-2xx
/// status is treated as an error.
pub fn post_json<B: Serialize, T: DeserializeOwned>(url: &str, body: &B) -> Result<T, HttpError> {
    client()
        .post(url)
        .header("Accept", "application/json")
        .json(body)
        .send()
        .map_err(|e| HttpError(e.to_string()))?
        .error_for_status()
        .map_err(|e| HttpError(e.to_string()))?
        .json::<T>()
        .map_err(|e| HttpError(e.to_string()))
}

/// GET `url` with a bearer token and deserialize the response as `T`.
pub fn get_json_bearer<T: DeserializeOwned>(url: &str, bearer: &str) -> Result<T, HttpError> {
    client()
        .get(url)
        .bearer_auth(bearer)
        .send()
        .map_err(|e| HttpError(e.to_string()))?
        .error_for_status()
        .map_err(|e| HttpError(e.to_string()))?
        .json::<T>()
        .map_err(|e| HttpError(e.to_string()))
}

/// GET `url` and return the response body as plain text. A non-2xx
/// status is *not* treated as an error - callers that use this for
/// optional metadata (e.g. a Maven `.sha1` checksum sidecar that may
/// not exist for a given artifact) can just treat an empty/failed
/// result as "no data available" rather than aborting.
pub fn get_text(url: &str) -> Result<String, HttpError> {
    client()
        .get(url)
        .send()
        .map_err(|e| HttpError(e.to_string()))?
        .text()
        .map_err(|e| HttpError(e.to_string()))
}

/// How many times a single download is retried before giving up. A
/// real install makes thousands of sequential requests (one per asset
/// object, plus libraries and the client jar); a connection dropped
/// mid-transfer by a flaky link or a server closing an idle-too-long
/// keep-alive is common at that volume, and surfaces as reqwest's
/// generic "error decoding response body" - not a parsing bug, just an
/// interrupted stream. Without a retry, one such drop anywhere in that
/// sequence aborted the *entire* install, even after successfully
/// downloading almost everything else.
const DOWNLOAD_MAX_ATTEMPTS: u32 = 4;

/// Downloads `url` to `dest`, calling `on_progress(bytes_done, bytes_total)`
/// periodically (not per-chunk) so a large asset set doesn't flood the
/// caller with updates. Retries the whole transfer (not a resume - these
/// files are small enough that restarting is simpler and still fast)
/// up to `DOWNLOAD_MAX_ATTEMPTS` times on failure, with a short backoff.
pub fn download_to_file(
    url: &str,
    dest: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), HttpError> {
    let mut last_err = None;
    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        match download_to_file_once(url, dest, &mut on_progress) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < DOWNLOAD_MAX_ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(300 * attempt as u64));
                last_err = Some(e);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("loop always sets last_err before exiting"))
}

fn download_to_file_once(
    url: &str,
    dest: &Path,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<(), HttpError> {
    use std::io::Write;

    let mut response = download_client()
        .get(url)
        .send()
        .map_err(|e| HttpError(e.to_string()))?;
    let total = response.content_length().unwrap_or(0);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HttpError(e.to_string()))?;
    }
    let mut file = std::fs::File::create(dest).map_err(|e| HttpError(e.to_string()))?;

    let mut buf = [0u8; 64 * 1024];
    let mut done = 0u64;
    let mut last_report = std::time::Instant::now();
    loop {
        let n = std::io::Read::read(&mut response, &mut buf).map_err(|e| HttpError(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| HttpError(e.to_string()))?;
        done += n as u64;
        if last_report.elapsed() >= Duration::from_millis(100) {
            on_progress(done, total);
            last_report = std::time::Instant::now();
        }
    }
    on_progress(done, total);

    // A connection closed cleanly by the far end mid-transfer (rather
    // than a hard I/O error) surfaces as `read()` returning 0 early -
    // exactly like reaching a real end of stream. Without this check
    // that looked identical to a successful download: a truncated file
    // got written to disk and reported as `Ok`, only to fail confusingly
    // much later (a corrupted jar on the classpath produces bizarre,
    // unrelated-looking JVM bootstrap errors, not a clear "bad download"
    // message).
    if total > 0 && done != total {
        return Err(HttpError(format!(
            "download truncated: got {done} of {total} expected bytes"
        )));
    }
    Ok(())
}

const INTEGRITY_MAX_ATTEMPTS: u32 = 3;

/// Downloads `url` to `dest` unless it's already there with a matching
/// size *and* SHA-1 hash - a real integrity check, not just a size
/// comparison. Pass `""` for `expected_sha1` when no hash is available
/// (falls back to a size-only check, or none at all if `expected_size`
/// is also `0`) and/or `0` for `expected_size` when the size isn't
/// known ahead of time (Fabric's own library metadata doesn't include
/// one). Retries a few times on a hash mismatch, since
/// `download_to_file`'s own retries only cover network/transfer
/// errors, not "downloaded successfully but wrong" - which matters
/// because a same-length-but-wrong-content file produces bizarre,
/// unrelated-looking failures much later (a corrupted jar on the
/// classpath crashing deep inside the JVM's own bootstrap) instead of
/// a clear "bad download" message right here.
pub fn download_and_verify(
    url: &str,
    dest: &Path,
    expected_size: u64,
    expected_sha1: &str,
) -> Result<(), HttpError> {
    if file_matches(dest, expected_size, expected_sha1) {
        return Ok(());
    }
    let mut last_err = None;
    for attempt in 1..=INTEGRITY_MAX_ATTEMPTS {
        download_to_file(url, dest, |_, _| {})?;
        if file_matches(dest, expected_size, expected_sha1) {
            return Ok(());
        }
        last_err = Some(HttpError(format!(
            "{} failed its integrity check after downloading (attempt {attempt}/{INTEGRITY_MAX_ATTEMPTS})",
            dest.display()
        )));
    }
    Err(last_err.expect("loop always sets last_err before exiting"))
}

fn file_matches(dest: &Path, expected_size: u64, expected_sha1: &str) -> bool {
    let Ok(meta) = std::fs::metadata(dest) else {
        return false;
    };
    if expected_size != 0 && meta.len() != expected_size {
        return false;
    }
    if expected_sha1.is_empty() {
        return true;
    }
    sha1_hex(dest)
        .map(|h| h.eq_ignore_ascii_case(expected_sha1))
        .unwrap_or(false)
}

fn sha1_hex(path: &Path) -> Result<String, HttpError> {
    use sha1::{Digest, Sha1};
    let bytes = std::fs::read(path).map_err(|e| HttpError(e.to_string()))?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_hex_matches_known_value() {
        let path = std::env::temp_dir().join("gamepad-minecraft-net-test-sha1-known.txt");
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(sha1_hex(&path).unwrap(), "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_matches_rejects_wrong_size() {
        let path = std::env::temp_dir().join("gamepad-minecraft-net-test-size.txt");
        std::fs::write(&path, b"short").unwrap();
        assert!(!file_matches(&path, 999, ""));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_matches_rejects_wrong_hash() {
        let path = std::env::temp_dir().join("gamepad-minecraft-net-test-hash.txt");
        std::fs::write(&path, b"hello world").unwrap();
        assert!(!file_matches(&path, 0, "0000000000000000000000000000000000000000"));
        assert!(file_matches(&path, 0, "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_matches_missing_file_is_false() {
        let path = std::env::temp_dir().join("gamepad-minecraft-net-test-does-not-exist.txt");
        let _ = std::fs::remove_file(&path);
        assert!(!file_matches(&path, 0, ""));
    }
}
