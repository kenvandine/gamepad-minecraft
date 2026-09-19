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

/// GET `url` and deserialize the JSON response as `T`.
pub fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, HttpError> {
    reqwest::blocking::get(url)
        .map_err(|e| HttpError(e.to_string()))?
        .json::<T>()
        .map_err(|e| HttpError(e.to_string()))
}

/// POST `form` as `application/x-www-form-urlencoded` and deserialize the
/// JSON response as `T`. Used for the device-code and token endpoints.
pub fn post_form<T: DeserializeOwned>(url: &str, form: &[(&str, &str)]) -> Result<T, HttpError> {
    reqwest::blocking::Client::new()
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
    let response = reqwest::blocking::Client::new()
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
    let response = reqwest::blocking::Client::new()
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
    reqwest::blocking::Client::new()
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
    reqwest::blocking::Client::new()
        .get(url)
        .bearer_auth(bearer)
        .send()
        .map_err(|e| HttpError(e.to_string()))?
        .error_for_status()
        .map_err(|e| HttpError(e.to_string()))?
        .json::<T>()
        .map_err(|e| HttpError(e.to_string()))
}

/// Downloads `url` to `dest`, calling `on_progress(bytes_done, bytes_total)`
/// periodically (not per-chunk) so a large asset set doesn't flood the
/// caller with updates.
pub fn download_to_file(
    url: &str,
    dest: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), HttpError> {
    use std::io::Write;

    let mut response = reqwest::blocking::get(url).map_err(|e| HttpError(e.to_string()))?;
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
    Ok(())
}
