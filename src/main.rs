// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

use gamepad_minecraft::{account, audio, auth, fabric, haptics, input, instance, launch, net, qr};

use account::{AccountStore, CachedAccount};
use input::GamepadAction;
use instance::{InstallState, InstanceMeta, InstanceStore};
use net::HttpError;

// ─── Application state ────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Login,
    Home,
    VersionPicker,
    InstanceDetail,
    Accounts,
    Settings,
}

struct AppData {
    view: View,
    accounts: AccountStore,
    instances: InstanceStore,
    auth: auth::AuthState,
    /// Set while a device-code poll thread is running, so Back can
    /// cancel it instead of leaving it to poll Microsoft until expiry.
    poll_cancel: Option<Arc<AtomicBool>>,
}

// ─── Shared widget handles ─────────────────────────────────────────
// Bundled together (rather than threaded through each function as a long,
// easy-to-misorder parameter list) so every input handler updates the
// same set of widgets consistently, including keyboard/gamepad focus —
// same rationale as gamepad-2048's `Widgets` struct.
#[derive(Clone)]
struct Widgets {
    window: ApplicationWindow,
    stack: gtk::Stack,
    // Login page
    login_status_label: gtk::Label,
    qr_picture: gtk::Picture,
    user_code_label: gtk::Label,
    sign_in_btn: gtk::Button,
    play_offline_btn: gtk::Button,
    // Home page
    home_status_label: gtk::Label,
    instance_list: gtk::Box,
    add_instance_btn: gtk::Button,
    install_progress: gtk::ProgressBar,
    // Version picker page
    version_status_label: gtk::Label,
    version_list: gtk::Box,
    // Accounts page
    accounts_list: gtk::Box,
    add_account_btn: gtk::Button,
    // Settings page
    settings_list: gtk::Box,
    sound: audio::Player,
    haptics: Option<haptics::Haptics>,
}

impl Widgets {
    /// Seeds keyboard/gamepad focus for `view`. Every screen transition
    /// must call this — a revealer/page swap without a refocus call was
    /// gamepad-2048's single biggest bug class (PLAN.md §5, lesson 2).
    fn focus_default_for(&self, view: View) {
        self.stack.set_visible_child_name(view_name(view));
        match view {
            View::Login => {
                if self.sign_in_btn.get_sensitive() {
                    self.sign_in_btn.grab_focus();
                } else {
                    self.play_offline_btn.grab_focus();
                }
            }
            View::Home => {
                if let Some(first) = self.instance_list.first_child() {
                    first.grab_focus();
                } else {
                    self.add_instance_btn.grab_focus();
                }
            }
            View::VersionPicker => {
                if let Some(first) = self.version_list.first_child() {
                    first.grab_focus();
                }
            }
            View::Accounts => {
                if let Some(first) = self.accounts_list.first_child() {
                    first.grab_focus();
                } else {
                    self.add_account_btn.grab_focus();
                }
            }
            View::Settings => {
                if let Some(first) = self.settings_list.first_child() {
                    first.grab_focus();
                }
            }
            _ => {}
        }
    }
}

/// X (Accounts) and Y (Settings) are Home-level shortcuts (PLAN.md §5's
/// face-button scheme) - guarding on this avoids e.g. X popping open
/// Accounts mid-QR-code sign-in.
fn is_on_home(state: &Arc<Mutex<AppData>>) -> bool {
    state.lock().unwrap().view == View::Home
}

fn view_name(view: View) -> &'static str {
    match view {
        View::Login => "login",
        View::Home => "home",
        View::VersionPicker => "version-picker",
        View::InstanceDetail => "instance-detail",
        View::Accounts => "accounts",
        View::Settings => "settings",
    }
}

/// Applies `auth` to every widget that reflects sign-in state. This is
/// the single place that decides what the Login/Home screens show for a
/// given `AuthState`, so there is exactly one source of truth instead of
/// call sites disagreeing about what a given state looks like.
fn render_auth_state(widgets: &Widgets, auth: &auth::AuthState) {
    match auth {
        auth::AuthState::Resolving => {
            widgets.login_status_label.set_label("Loading...");
            widgets.qr_picture.set_visible(false);
            widgets.user_code_label.set_visible(false);
            widgets.sign_in_btn.set_sensitive(true);
            widgets.sign_in_btn.set_label("Sign in with Microsoft");
        }
        auth::AuthState::NeedsLogin { .. } => {
            widgets
                .login_status_label
                .set_label("Sign in to play online, or play offline.");
            widgets.qr_picture.set_visible(false);
            widgets.user_code_label.set_visible(false);
            widgets.sign_in_btn.set_sensitive(true);
            widgets.sign_in_btn.set_label("Sign in with Microsoft");
        }
        auth::AuthState::AwaitingUser {
            user_code,
            verification_uri,
            qr_data,
            ..
        } => {
            widgets.login_status_label.set_label(&format!(
                "Go to {verification_uri} and enter the code below, or scan it."
            ));
            widgets.user_code_label.set_label(user_code);
            widgets.user_code_label.set_visible(true);
            match qr::render_verification_qr(qr_data, 8) {
                Ok(image) => {
                    widgets.qr_picture.set_paintable(Some(&qr_texture(&image)));
                    widgets.qr_picture.set_visible(true);
                }
                Err(_) => widgets.qr_picture.set_visible(false),
            }
            widgets.sign_in_btn.set_sensitive(false);
            widgets.sign_in_btn.set_label("Waiting for approval...");
        }
        auth::AuthState::LoggedIn { profile } => {
            widgets
                .home_status_label
                .set_label(&format!("Signed in as {}", profile.name));
        }
        auth::AuthState::OfflinePlaying { profile } => {
            widgets
                .home_status_label
                .set_label(&format!("Offline mode - playing as {}", profile.username));
        }
        auth::AuthState::Error { message, .. } => {
            widgets.login_status_label.set_label(message);
            widgets.qr_picture.set_visible(false);
            widgets.user_code_label.set_visible(false);
            widgets.sign_in_btn.set_sensitive(true);
            widgets.sign_in_btn.set_label("Sign in with Microsoft");
        }
    }
}

/// Renders a QR code straight to a GPU-uploadable texture - no temp
/// files, no external image crate (see `qr.rs`).
fn qr_texture(image: &qr::QrImage) -> gtk::gdk::MemoryTexture {
    let bytes = glib::Bytes::from(image.rgba.as_slice());
    let stride = (image.width * 4) as usize;
    gtk::gdk::MemoryTexture::new(
        image.width as i32,
        image.height as i32,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    )
}

// ─── Login flow ─────────────────────────────────────────────────────
// This is the one place that drives AuthState forward on the "online"
// path: request a device code, poll for approval, then exchange for a
// Mojang session - each step handed to a background thread via
// `net::spawn_blocking` and reported back on the GTK main loop, per
// PLAN.md §2. Never blocks the UI thread.
fn start_sign_in(state: Arc<Mutex<AppData>>, widgets: Widgets) {
    widgets
        .login_status_label
        .set_label("Requesting a device code...");
    widgets.sign_in_btn.set_sensitive(false);

    let state1 = state.clone();
    let widgets1 = widgets.clone();
    net::spawn_blocking(auth::request_device_code, move |result| {
        let device = match result {
            Ok(device) => device,
            Err(e) => {
                fail_login(&state1, &widgets1, e.to_string());
                return;
            }
        };

        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut data = state1.lock().unwrap();
            data.poll_cancel = Some(cancel.clone());
        }

        let awaiting = auth::AuthState::AwaitingUser {
            qr_data: auth::qr_payload(&device),
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_secs(device.expires_in),
        };
        update_login_state(&state1, &widgets1, awaiting);
        widgets1.play_offline_btn.grab_focus();

        let state2 = state1.clone();
        let widgets2 = widgets1.clone();
        let device_code = device.device_code.clone();
        let interval = device.interval;
        net::spawn_blocking(
            move || auth::poll_for_token(&device_code, interval, cancel),
            move |poll_result| {
                let msa = match poll_result {
                    Ok(msa) => msa,
                    Err(e) => {
                        fail_login(&state2, &widgets2, e.to_string());
                        return;
                    }
                };

                let state3 = state2.clone();
                let widgets3 = widgets2.clone();
                let refresh_token = msa.refresh_token.clone();
                let access_token = msa.access_token.clone();
                net::spawn_blocking(
                    move || auth::complete_mojang_login(&access_token),
                    move |mojang_result| match mojang_result {
                        Ok(session) => {
                            let cached = account::CachedAccount {
                                username: session.profile.name.clone(),
                                uuid: session.profile.id.clone(),
                                refresh_token: Some(refresh_token.clone()),
                                obtained_at: chrono::Local::now()
                                    .format("%Y-%m-%d %H:%M")
                                    .to_string(),
                            };
                            {
                                let mut data = state3.lock().unwrap();
                                data.accounts.upsert(cached);
                                let _ = data.accounts.save();
                                data.poll_cancel = None;
                            }
                            let name = session.profile.name.clone();
                            widgets3
                                .play_offline_btn
                                .set_label(&format!("Play Offline as {name}"));
                            go_to_home(
                                &state3,
                                &widgets3,
                                auth::AuthState::LoggedIn {
                                    profile: session.profile,
                                },
                            );
                        }
                        Err(e) => fail_login(&state3, &widgets3, e.to_string()),
                    },
                );
            },
        );
    });
}

fn fail_login(state: &Arc<Mutex<AppData>>, widgets: &Widgets, message: String) {
    let cached = {
        let mut data = state.lock().unwrap();
        data.poll_cancel = None;
        data.accounts.active().cloned()
    };
    update_login_state(state, widgets, auth::AuthState::Error { message, cached });
    widgets.play_offline_btn.grab_focus();
}

/// Updates `AppData.auth` and re-renders while staying on the Login
/// page.
fn update_login_state(state: &Arc<Mutex<AppData>>, widgets: &Widgets, new_auth: auth::AuthState) {
    state.lock().unwrap().auth = new_auth.clone();
    render_auth_state(widgets, &new_auth);
}

/// Updates `AppData.auth`, switches to Home, and re-seeds focus there -
/// the online/offline counterpart of `handle_gp_back`'s view transitions
/// (PLAN.md §5, lesson 2: every transition seeds focus explicitly).
fn go_to_home(state: &Arc<Mutex<AppData>>, widgets: &Widgets, new_auth: auth::AuthState) {
    {
        let mut data = state.lock().unwrap();
        data.auth = new_auth.clone();
        data.view = View::Home;
    }
    render_auth_state(widgets, &new_auth);
    refresh_instance_grid(state, widgets);
    widgets.focus_default_for(View::Home);
}

/// The offline path: no network, no Mojang account required at all.
/// Reuses whichever account is already cached (real or previously
/// synthesized); if none exists yet, synthesizes and caches one from the
/// OS username using the same offline-UUID algorithm the vanilla client
/// uses (PLAN.md §3 - "network required for login" only ever meant the
/// *online* path, never this one).
fn start_play_offline(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    if let Some(cancel) = state.lock().unwrap().poll_cancel.take() {
        cancel.store(true, Ordering::Relaxed);
    }

    let profile = {
        let mut data = state.lock().unwrap();
        if let Some(existing) = data.accounts.active() {
            existing.clone()
        } else {
            let username = account::default_offline_username();
            let synthetic = account::CachedAccount {
                username: username.clone(),
                uuid: account::offline_uuid(&username),
                refresh_token: None,
                obtained_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            };
            data.accounts.upsert(synthetic.clone());
            let _ = data.accounts.save();
            synthetic
        }
    };
    go_to_home(state, widgets, auth::AuthState::OfflinePlaying { profile });
}

// ─── Instance management & launch ──────────────────────────────────
// Rebuilds the Home page's instance tiles from `AppData.instances`.
// Called after every state change that affects the list (install
// started/finished, launch finished) rather than mutating tiles in
// place, since the list length itself changes on "Add Instance".
fn refresh_instance_grid(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    while let Some(child) = widgets.instance_list.first_child() {
        widgets.instance_list.remove(&child);
    }

    let instances = state.lock().unwrap().instances.instances.clone();
    for meta in instances {
        let loader_tag = match meta.loader {
            instance::Loader::Fabric => "Fabric",
            instance::Loader::Vanilla => "Vanilla",
        };
        let label = match meta.state {
            InstallState::Installed => format!("Play {} ({loader_tag})", meta.mc_version),
            InstallState::Downloading => format!("{} — installing...", meta.mc_version),
            InstallState::Failed => format!("{} — install failed, select to retry", meta.mc_version),
            InstallState::NotInstalled => format!("{} — not installed", meta.mc_version),
        };
        let btn = gtk::Button::with_label(&label);
        btn.set_css_classes(&["action-button"]);
        btn.set_sensitive(!matches!(meta.state, InstallState::Downloading));

        let state_for_btn = state.clone();
        let widgets_for_btn = widgets.clone();
        let meta_for_btn = meta.clone();
        btn.connect_clicked(move |_| match meta_for_btn.state {
            InstallState::Installed => {
                start_launch_instance(&state_for_btn, &widgets_for_btn, meta_for_btn.clone())
            }
            _ => start_install(state_for_btn.clone(), widgets_for_btn.clone(), meta_for_btn.clone()),
        });
        widgets.instance_list.append(&btn);
    }
}

/// How many recent release versions the picker offers. Mojang's
/// manifest lists 900+ versions total (every release and snapshot ever
/// shipped) - scrolling a gamepad-navigable list through all of them
/// would be miserable, and nobody picking a version for a fresh install
/// wants anything but a recent one anyway.
const VERSION_PICKER_LIMIT: usize = 40;

/// Switches to the version-picker page and populates it from Mojang's
/// version manifest, most-recent-release-first.
fn open_version_picker(state: Arc<Mutex<AppData>>, widgets: Widgets) {
    {
        let mut data = state.lock().unwrap();
        data.view = View::VersionPicker;
    }
    widgets.stack.set_visible_child_name(view_name(View::VersionPicker));
    widgets.version_status_label.set_label("Fetching version list...");
    while let Some(child) = widgets.version_list.first_child() {
        widgets.version_list.remove(&child);
    }

    let state1 = state.clone();
    let widgets1 = widgets.clone();
    net::spawn_blocking(instance::fetch_version_manifest, move |result| {
        let manifest = match result {
            Ok(m) => m,
            Err(e) => {
                widgets1
                    .version_status_label
                    .set_label(&format!("Couldn't fetch version list: {e}"));
                return;
            }
        };
        widgets1
            .version_status_label
            .set_label(&format!("{VERSION_PICKER_LIMIT} most recent releases:"));

        let releases = manifest
            .versions
            .iter()
            .filter(|v| v.kind == "release")
            .take(VERSION_PICKER_LIMIT);
        for entry in releases {
            let label = if entry.id == manifest.latest.release {
                format!("{} (latest)", entry.id)
            } else {
                entry.id.clone()
            };
            let btn = gtk::Button::with_label(&label);
            btn.set_css_classes(&["action-button"]);

            let state2 = state1.clone();
            let widgets2 = widgets1.clone();
            let mc_version = entry.id.clone();
            btn.connect_clicked(move |_| {
                open_or_install_version(state2.clone(), widgets2.clone(), mc_version.clone())
            });
            widgets1.version_list.append(&btn);
        }
        widgets1.focus_default_for(View::VersionPicker);
    });
}

/// Adds `mc_version` as a new instance and starts installing it, unless
/// it's already been added before (installed, failed, or mid-download)
/// - in which case there's nothing new to do here, so just return to
/// Home where the existing tile can be played/retried.
fn open_or_install_version(state: Arc<Mutex<AppData>>, widgets: Widgets, mc_version: String) {
    let already_added = state
        .lock()
        .unwrap()
        .instances
        .instances
        .iter()
        .any(|i| i.id == mc_version);

    {
        let mut data = state.lock().unwrap();
        data.view = View::Home;
    }

    if already_added {
        refresh_instance_grid(&state, &widgets);
        widgets.focus_default_for(View::Home);
        return;
    }

    let meta = InstanceMeta {
        id: mc_version.clone(),
        mc_version,
        loader: instance::Loader::Fabric,
        state: InstallState::NotInstalled,
    };
    {
        let mut data = state.lock().unwrap();
        data.instances.instances.push(meta.clone());
        let _ = data.instances.save();
    }
    widgets.focus_default_for(View::Home);
    start_install(state, widgets, meta);
}

/// Downloads the vanilla client + Fabric Loader + Controlify for
/// `meta`, updating its `InstallState`, driving `install_progress`, and
/// re-rendering the grid when done. The progress bar only reflects
/// `instance::install_instance`'s own byte-level progress (client jar +
/// libraries + assets, by far the dominant cost); the much smaller
/// Fabric Loader + Controlify steps that follow aren't individually
/// tracked, so the bar sits at 100% for the last second or two of a
/// fresh install.
fn start_install(state: Arc<Mutex<AppData>>, widgets: Widgets, meta: InstanceMeta) {
    {
        let mut data = state.lock().unwrap();
        if let Some(entry) = data.instances.instances.iter_mut().find(|i| i.id == meta.id) {
            entry.state = InstallState::Downloading;
        }
        let _ = data.instances.save();
    }
    refresh_instance_grid(&state, &widgets);
    widgets
        .home_status_label
        .set_label(&format!("Installing Minecraft {}...", meta.mc_version));
    widgets.install_progress.set_fraction(0.0);
    widgets.install_progress.set_visible(true);

    let widgets_progress = widgets.clone();
    let on_progress = move |done: u64, total: u64| {
        let fraction = if total > 0 { done as f64 / total as f64 } else { 0.0 };
        widgets_progress.install_progress.set_fraction(fraction.clamp(0.0, 1.0));
    };

    let state2 = state.clone();
    let widgets2 = widgets.clone();
    let meta2 = meta.clone();
    net::spawn_blocking_with_progress(
        // `Ok(Some(warning))` means the vanilla install succeeded but
        // Fabric/Controlify couldn't be layered on - most commonly
        // because mod authors haven't published a build for a
        // brand-new Minecraft release yet (this lags by days/weeks
        // after every release, completely normal). Falling back to a
        // working vanilla instance beats losing the whole install over
        // it; `Err` is reserved for the vanilla install itself failing.
        move |report| -> Result<Option<String>, HttpError> {
            instance::install_instance(&meta2, |done, total| report(done, total))?;
            if meta2.loader == instance::Loader::Fabric {
                let fabric_result = fabric::install_fabric_loader(&meta2)
                    .and_then(|_| fabric::inject_controlify_mod(&meta2));
                if let Err(e) = fabric_result {
                    return Ok(Some(format!(
                        "Fabric/Controlify aren't available yet for Minecraft {} ({e}). \
                         Installed without them - try an older version for full controller support.",
                        meta2.mc_version
                    )));
                }
            }
            Ok(None)
        },
        on_progress,
        move |result| {
            let fell_back_to_vanilla = matches!(result, Ok(Some(_)));
            {
                let mut data = state2.lock().unwrap();
                if let Some(entry) = data.instances.instances.iter_mut().find(|i| i.id == meta.id) {
                    entry.state = if result.is_ok() {
                        InstallState::Installed
                    } else {
                        InstallState::Failed
                    };
                    if fell_back_to_vanilla {
                        entry.loader = instance::Loader::Vanilla;
                    }
                }
                let _ = data.instances.save();
            }
            widgets2.install_progress.set_visible(false);
            widgets2.home_status_label.set_label(&match result {
                Ok(None) => "Install complete.".to_string(),
                Ok(Some(warning)) => warning,
                Err(e) => format!("Install failed: {e}"),
            });
            refresh_instance_grid(&state2, &widgets2);
        },
    );
}

/// Launches an installed instance. Online sessions need a live Mojang
/// access token, which isn't cached (only the MSA refresh token is, see
/// `account.rs`) - so this silently refreshes and redoes the XBL/XSTS/
/// Mojang exchange first, falling back to an offline-style launch if
/// that fails, same non-blocking philosophy as PLAN.md §3's silent
/// refresh.
fn start_launch_instance(state: &Arc<Mutex<AppData>>, widgets: &Widgets, meta: InstanceMeta) {
    let (account, refresh_token) = {
        let data = state.lock().unwrap();
        let account = match &data.auth {
            auth::AuthState::LoggedIn { .. } | auth::AuthState::OfflinePlaying { .. } => {
                data.accounts.active().cloned()
            }
            _ => None,
        };
        let refresh_token = account.as_ref().and_then(|a| a.refresh_token.clone());
        (account, refresh_token)
    };
    let Some(account) = account else {
        widgets.home_status_label.set_label("Sign in or play offline first.");
        return;
    };

    widgets.home_status_label.set_label("Launching...");
    match refresh_token {
        Some(rt) => {
            let state2 = state.clone();
            let widgets2 = widgets.clone();
            let meta2 = meta.clone();
            let account2 = account.clone();
            net::spawn_blocking(
                move || -> Result<auth::MojangSession, HttpError> {
                    let msa = auth::refresh_silently(&rt)?;
                    auth::complete_mojang_login(&msa.access_token)
                },
                move |result| {
                    let token = result.ok().map(|s| s.access_token);
                    do_launch(&state2, &widgets2, meta2, account2, token);
                },
            );
        }
        None => do_launch(state, widgets, meta, account, None),
    }
}

fn do_launch(
    state: &Arc<Mutex<AppData>>,
    widgets: &Widgets,
    meta: InstanceMeta,
    account: CachedAccount,
    access_token: Option<String>,
) {
    let widgets2 = widgets.clone();
    let state2 = state.clone();
    net::spawn_blocking(
        move || -> Result<(), HttpError> {
            launch::spawn_minecraft(&meta, &account, access_token.as_deref(), |_event| {})
                .map_err(HttpError)
        },
        move |result| {
            widgets2.home_status_label.set_label(match &result {
                Ok(()) => "Minecraft exited.",
                Err(_) => "Launch failed.",
            });
            if let Err(e) = &result {
                widgets2.home_status_label.set_label(&format!("Launch failed: {e}"));
            }
            refresh_instance_grid(&state2, &widgets2);
        },
    );
}

// ─── Accounts ───────────────────────────────────────────────────────
// Switching or adding an account is Home-adjacent, not its own auth
// flow: switching just re-points AppData.accounts.active_uuid and
// derives an optimistic AuthState from the cached entry (no network
// call - matches PLAN.md §3's "Home renders immediately from cache").
// Adding another Microsoft account reuses the exact same Login-page
// device-code flow, just entered from here instead of a fresh launch.
fn refresh_accounts_list(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    while let Some(child) = widgets.accounts_list.first_child() {
        widgets.accounts_list.remove(&child);
    }

    let (accounts, active_uuid) = {
        let data = state.lock().unwrap();
        (data.accounts.accounts.clone(), data.accounts.active_uuid.clone())
    };
    for acc in accounts {
        let is_active = active_uuid.as_deref() == Some(acc.uuid.as_str());
        let kind = if acc.refresh_token.is_some() { "Microsoft" } else { "Offline" };
        let label = if is_active {
            format!("{} ({kind}) — Active", acc.username)
        } else {
            format!("Switch to {} ({kind})", acc.username)
        };
        let btn = gtk::Button::with_label(&label);
        btn.set_css_classes(&["action-button"]);
        btn.set_sensitive(!is_active);

        let state_for_btn = state.clone();
        let widgets_for_btn = widgets.clone();
        let uuid = acc.uuid.clone();
        btn.connect_clicked(move |_| switch_active_account(&state_for_btn, &widgets_for_btn, uuid.clone()));
        widgets.accounts_list.append(&btn);
    }
}

fn switch_active_account(state: &Arc<Mutex<AppData>>, widgets: &Widgets, uuid: String) {
    let new_auth = {
        let mut data = state.lock().unwrap();
        data.accounts.active_uuid = Some(uuid);
        let _ = data.accounts.save();
        let auth_state = match data.accounts.active() {
            Some(account) if account.refresh_token.is_some() => auth::AuthState::LoggedIn {
                profile: auth::McProfile {
                    id: account.uuid.clone(),
                    name: account.username.clone(),
                },
            },
            Some(account) => auth::AuthState::OfflinePlaying {
                profile: account.clone(),
            },
            None => auth::AuthState::NeedsLogin { cached: None },
        };
        data.auth = auth_state.clone();
        data.view = View::Home;
        auth_state
    };
    render_auth_state(widgets, &new_auth);
    refresh_instance_grid(state, widgets);
    widgets.focus_default_for(View::Home);
}

/// Opens the same device-code sign-in flow the Login page uses,
/// entered from Accounts instead of a fresh launch - a second
/// successful sign-in just adds another cached account rather than
/// replacing the current one (`account::AccountStore::upsert` keys on
/// UUID).
fn start_add_account(state: Arc<Mutex<AppData>>, widgets: Widgets) {
    {
        let mut data = state.lock().unwrap();
        data.view = View::Login;
    }
    widgets.stack.set_visible_child_name(view_name(View::Login));
    start_sign_in(state, widgets);
}

fn open_accounts(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    state.lock().unwrap().view = View::Accounts;
    refresh_accounts_list(state, widgets);
    widgets.focus_default_for(View::Accounts);
}

// ─── Settings ───────────────────────────────────────────────────────
// The one setting worth exposing right now: freeing disk space by
// deleting an installed instance. There was previously no way to do
// this at all once an instance was added.
fn refresh_settings_list(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    while let Some(child) = widgets.settings_list.first_child() {
        widgets.settings_list.remove(&child);
    }

    let instances = state.lock().unwrap().instances.instances.clone();
    for meta in instances {
        let loader_tag = match meta.loader {
            instance::Loader::Fabric => "Fabric",
            instance::Loader::Vanilla => "Vanilla",
        };
        let btn = gtk::Button::with_label(&format!("Delete {} ({loader_tag})", meta.mc_version));
        btn.set_css_classes(&["action-button"]);

        let state_for_btn = state.clone();
        let widgets_for_btn = widgets.clone();
        let id = meta.id.clone();
        btn.connect_clicked(move |_| delete_instance(&state_for_btn, &widgets_for_btn, id.clone()));
        widgets.settings_list.append(&btn);
    }
}

fn delete_instance(state: &Arc<Mutex<AppData>>, widgets: &Widgets, id: String) {
    {
        let mut data = state.lock().unwrap();
        data.instances.instances.retain(|i| i.id != id);
        let _ = data.instances.save();
    }
    let _ = std::fs::remove_dir_all(InstanceStore::instance_dir(&id));
    refresh_settings_list(state, widgets);
    widgets.focus_default_for(View::Settings);
}

fn open_settings(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    state.lock().unwrap().view = View::Settings;
    refresh_settings_list(state, widgets);
    widgets.focus_default_for(View::Settings);
}

/// Moves focus to the next/previous focusable widget in document order,
/// instead of GTK's generic geometric `child_focus` search (PLAN.md §5,
/// lesson 4). That search is unreliable across a tall stack of
/// same-sized buttons - the exact bug gamepad-2048 hit for its
/// board-size row - and manifests here on any of the longer lists
/// (version picker, instance grid, accounts, settings): Up/Down would
/// unpredictably skip several rows at once instead of moving one at a
/// time.
///
/// Most screens lay their focusable controls out as flat siblings in a
/// single vertical `gtk::Box`, where "the literal next/previous
/// sibling" is exactly "the next item down/up". But Home and Accounts
/// nest their list of tiles inside its own `gtk::Box` (`instance_list`,
/// `accounts_list`) with the "Add" button as a sibling of *that box*,
/// not of the tiles inside it - so a flat sibling-only walk can never
/// step off the last tile onto "Add". `focus_relative` below climbs to
/// the parent and keeps looking there once a container's own siblings
/// are exhausted, and descends into a sibling that's itself a container
/// to find its first/last focusable descendant - a real (if small)
/// preorder tree walk rather than a single sibling hop. It stops dead
/// at each screen's own top-level page `Box` (detected by the page
/// being a direct child of `stack`) rather than climbing further, so it
/// can never wrap around into a different, hidden page's own widgets.
/// Deliberately doesn't wrap at the ends of a screen, same as
/// gamepad-2048's own documented behavior.
fn step_focus(window: &ApplicationWindow, forward: bool) -> bool {
    let Some(focused) = gtk::prelude::RootExt::focus(window) else {
        return false;
    };
    focus_relative(&focused, forward)
}

/// Walks from `from` to the next (or, if `!forward`, previous) focusable
/// widget in document order and focuses it. See `step_focus` for why
/// this needs to climb parents and descend into sibling containers
/// rather than just checking one sibling.
fn focus_relative(from: &gtk::Widget, forward: bool) -> bool {
    let mut node = from.clone();
    loop {
        let sibling = if forward { node.next_sibling() } else { node.prev_sibling() };
        match sibling {
            Some(candidate) => {
                if focus_into(&candidate, forward) {
                    return true;
                }
                // `candidate` and everything inside it declined focus
                // (e.g. a Label) - keep scanning past it.
                node = candidate;
            }
            None => {
                let Some(parent) = node.parent() else {
                    return false;
                };
                // Never escape the current Stack page - each page's
                // top-level Box is a traversal boundary, so this can't
                // wrap into a completely different (hidden) page's own
                // widgets by climbing too far.
                if parent.downcast_ref::<gtk::Stack>().is_some() {
                    return false;
                }
                node = parent;
            }
        }
    }
}

/// Tries to focus `widget` itself, or - if it can't take focus directly
/// - the first (or last, going backward) focusable widget among its own
/// descendants, checked in order.
fn focus_into(widget: &gtk::Widget, forward: bool) -> bool {
    if widget.grab_focus() {
        return true;
    }
    let mut child = if forward { widget.first_child() } else { widget.last_child() };
    while let Some(c) = child {
        if focus_into(&c, forward) {
            return true;
        }
        child = if forward { c.next_sibling() } else { c.prev_sibling() };
    }
    false
}

// A physical D-Pad press can arrive as more than one discrete
// navigation signal for the exact same press: some controllers/drivers
// send a brief repeat burst of `ButtonPressed` events rather than one
// clean edge, and on at least one real device (Steam Input, confirmed
// via its hostname in that device's own AppArmor logs) the same D-Pad
// press is *also* synthesized as a keyboard arrow-key event for
// compatibility with apps that don't read gamepads directly - meaning
// our gilrs handler and our keyboard fallback handler can each fire
// once for what is, physically, a single press. Every one of those
// signals is individually a perfectly valid single-step move, so
// without a shared cooldown across *both* input paths, one physical
// press could step several rows at once - which is exactly what
// "skips rows" looked like, even though each individual `step_focus`
// call only ever moves one row on its own.
const DPAD_NAV_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);

/// Steps focus unless another accepted step (from *either* input path)
/// happened within `DPAD_NAV_DEBOUNCE`. `gate` must be shared between
/// the gamepad and keyboard handlers - a debounce scoped to only one of
/// them can't catch a duplicate arriving through the other.
fn nav_step_if_not_debounced(
    gate: &Rc<RefCell<std::time::Instant>>,
    window: &ApplicationWindow,
    forward: bool,
) -> bool {
    let mut last = gate.borrow_mut();
    if last.elapsed() < DPAD_NAV_DEBOUNCE {
        return false;
    }
    *last = std::time::Instant::now();
    step_focus(window, forward)
}

// ─── Gamepad confirm/back dispatch ─────────────────────────────────
// Confirm always activates whatever GTK reports as focused - never a
// per-screen hardcoded action derived from AppData - so the highlighted
// widget and "what A does" can never disagree (PLAN.md §5, lesson 1).
// This matters more here than in a puzzle game: a stale-focus bug could
// launch the wrong instance or switch the wrong account.
fn handle_gp_confirm(_state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    if let Some(focused) = gtk::prelude::RootExt::focus(&widgets.window) {
        gtk::prelude::WidgetExt::activate(&focused);
    }
}

// All "back" gestures (gamepad B, keyboard Escape) funnel through this
// one function so `AppData.view` and the visible `Stack` page can never
// desync (PLAN.md §5, lesson 3).
fn handle_gp_back(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let mut data = state.lock().unwrap();
    match data.view {
        View::Login => {
            let awaiting = matches!(data.auth, auth::AuthState::AwaitingUser { .. });
            if awaiting {
                if let Some(cancel) = data.poll_cancel.take() {
                    cancel.store(true, Ordering::Relaxed);
                }
                let cached = data.accounts.active().cloned();
                data.auth = auth::AuthState::NeedsLogin { cached };
                let snapshot = data.auth.clone();
                drop(data);
                render_auth_state(widgets, &snapshot);
                widgets.sign_in_btn.grab_focus();
            } else {
                drop(data);
                widgets.window.close();
            }
        }
        View::VersionPicker | View::InstanceDetail | View::Accounts | View::Settings => {
            data.view = View::Home;
            drop(data);
            widgets.focus_default_for(View::Home);
        }
        View::Home => drop(data), // TODO: quit-confirm overlay
    }
}

// ─── Main application ───────────────────────────────────────────────
fn main() {
    // See gamepad-2048's main.rs for why both of these are set: strict
    // snap confinement's AppArmor policy denies the D-Bus calls that
    // PulseAudio auto-spawn and unique-instance registration would
    // otherwise make.
    std::env::set_var("PULSE_AUTOSPAWN", "0");

    let app = Application::builder()
        .application_id("com.github.gamepadminecraft")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::new(app);
    window.set_title(Some("Minecraft"));
    window.set_default_size(1280, 800);

    // ── Theme: Ubuntu "Resolute Raccoon" palette, inherited from
    // gamepad-shell / gamepad-2048 (PLAN.md §4).
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_data(
        r#"
        @define-color orange #E95420;
        @define-color orange_bright #F4703C;
        @define-color aubergine #2C001E;
        @define-color purple #77216F;

        * { font-family: "Ubuntu Sans", "Ubuntu", sans-serif; }
        window { background-color: #150610; color: #FFFFFF; }

        .page { padding: 40px; }
        .game-title { font-size: 44px; font-weight: 800; color: @orange; }
        .control-hint {
            font-size: 13px; font-weight: 500; letter-spacing: 0.5px;
            color: alpha(#FFFFFF, 0.55);
        }
        .user-code {
            font-size: 32px; font-weight: 700; letter-spacing: 4px;
            color: @orange_bright;
        }

        button.action-button {
            padding: 14px 24px; border-radius: 14px;
            border: 2px solid alpha(#FFFFFF, 0.12);
            background-color: #3A1A28;
            background-image: none;
            box-shadow: none;
            color: #FFFFFF; font-size: 16px; font-weight: 600;
            min-height: 48px; min-width: 260px;
            transition: all 120ms ease-out;
        }
        button.action-button:hover { background-color: #4A2550; }
        button.action-button:focus, button.action-button:active {
            background-color: @orange;
            border-color: @orange_bright;
            box-shadow: 0 0 0 3px alpha(@orange, 0.35);
            outline: none;
        }
        button.action-button:disabled {
            background-color: #251520; color: alpha(#FFFFFF, 0.45);
        }

        progressbar > trough {
            background-color: #251520;
            border-radius: 8px;
            min-height: 14px;
        }
        progressbar > trough > progress {
            background-color: @orange;
            background-image: none;
            border-radius: 8px;
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("no display"),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let stack = gtk::Stack::new();

    // ── Login page ──
    let login_page = gtk::Box::new(gtk::Orientation::Vertical, 20);
    login_page.set_css_classes(&["page"]);
    login_page.set_halign(gtk::Align::Center);
    login_page.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(Some("Minecraft"));
    title.set_css_classes(&["game-title"]);
    login_page.append(&title);

    let login_status_label = gtk::Label::new(Some("Loading..."));
    login_status_label.set_wrap(true);
    login_status_label.set_justify(gtk::Justification::Center);
    login_page.append(&login_status_label);

    let qr_picture = gtk::Picture::new();
    qr_picture.set_size_request(260, 260);
    qr_picture.set_content_fit(gtk::ContentFit::Contain);
    qr_picture.set_visible(false);
    login_page.append(&qr_picture);

    let user_code_label = gtk::Label::new(None);
    user_code_label.set_css_classes(&["user-code"]);
    user_code_label.set_visible(false);
    login_page.append(&user_code_label);

    let sign_in_btn = gtk::Button::with_label("Sign in with Microsoft");
    sign_in_btn.set_css_classes(&["action-button"]);
    login_page.append(&sign_in_btn);

    let play_offline_btn = gtk::Button::with_label("Play Offline");
    play_offline_btn.set_css_classes(&["action-button"]);
    login_page.append(&play_offline_btn);

    let login_hint = gtk::Label::new(Some("A: Select  •  B: Cancel / Quit"));
    login_hint.set_css_classes(&["control-hint"]);
    login_page.append(&login_hint);

    stack.add_named(&login_page, Some(view_name(View::Login)));

    // ── Home page ──
    let home_page = gtk::Box::new(gtk::Orientation::Vertical, 20);
    home_page.set_css_classes(&["page"]);
    home_page.set_halign(gtk::Align::Center);
    home_page.set_valign(gtk::Align::Center);

    let home_title = gtk::Label::new(Some("Minecraft"));
    home_title.set_css_classes(&["game-title"]);
    home_page.append(&home_title);

    let home_status_label = gtk::Label::new(None);
    home_page.append(&home_status_label);

    let install_progress = gtk::ProgressBar::new();
    install_progress.set_size_request(400, -1);
    install_progress.set_visible(false);
    home_page.append(&install_progress);

    let instance_list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    home_page.append(&instance_list);

    let add_instance_btn = gtk::Button::with_label("Add Instance");
    add_instance_btn.set_css_classes(&["action-button"]);
    home_page.append(&add_instance_btn);

    let home_hint = gtk::Label::new(Some(
        "A: Select  •  B: Quit  •  X: Accounts  •  Y: Settings",
    ));
    home_hint.set_css_classes(&["control-hint"]);
    home_page.append(&home_hint);

    stack.add_named(&home_page, Some(view_name(View::Home)));

    // ── Version picker page ──
    let version_page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    version_page.set_css_classes(&["page"]);
    version_page.set_halign(gtk::Align::Center);

    let version_title = gtk::Label::new(Some("Choose a Version"));
    version_title.set_css_classes(&["game-title"]);
    version_page.append(&version_title);

    let version_status_label = gtk::Label::new(Some("Fetching version list..."));
    version_page.append(&version_status_label);

    let version_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let version_scroll = gtk::ScrolledWindow::new();
    version_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    version_scroll.set_min_content_height(500);
    version_scroll.set_child(Some(&version_list));
    version_page.append(&version_scroll);

    let version_hint = gtk::Label::new(Some("A: Select  •  B: Back"));
    version_hint.set_css_classes(&["control-hint"]);
    version_page.append(&version_hint);

    stack.add_named(&version_page, Some(view_name(View::VersionPicker)));

    // ── Accounts page ──
    let accounts_page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    accounts_page.set_css_classes(&["page"]);
    accounts_page.set_halign(gtk::Align::Center);

    let accounts_title = gtk::Label::new(Some("Accounts"));
    accounts_title.set_css_classes(&["game-title"]);
    accounts_page.append(&accounts_title);

    let accounts_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let accounts_scroll = gtk::ScrolledWindow::new();
    accounts_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    accounts_scroll.set_min_content_height(300);
    accounts_scroll.set_child(Some(&accounts_list));
    accounts_page.append(&accounts_scroll);

    let add_account_btn = gtk::Button::with_label("Sign in with another Microsoft account");
    add_account_btn.set_css_classes(&["action-button"]);
    accounts_page.append(&add_account_btn);

    let accounts_hint = gtk::Label::new(Some("A: Switch  •  B: Back"));
    accounts_hint.set_css_classes(&["control-hint"]);
    accounts_page.append(&accounts_hint);

    stack.add_named(&accounts_page, Some(view_name(View::Accounts)));

    // ── Settings page ──
    let settings_page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    settings_page.set_css_classes(&["page"]);
    settings_page.set_halign(gtk::Align::Center);

    let settings_title = gtk::Label::new(Some("Settings"));
    settings_title.set_css_classes(&["game-title"]);
    settings_page.append(&settings_title);

    let settings_subtitle = gtk::Label::new(Some("Installed instances"));
    settings_subtitle.set_css_classes(&["control-hint"]);
    settings_page.append(&settings_subtitle);

    let settings_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let settings_scroll = gtk::ScrolledWindow::new();
    settings_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    settings_scroll.set_min_content_height(300);
    settings_scroll.set_child(Some(&settings_list));
    settings_page.append(&settings_scroll);

    let settings_hint = gtk::Label::new(Some("A: Delete  •  B: Back"));
    settings_hint.set_css_classes(&["control-hint"]);
    settings_page.append(&settings_hint);

    stack.add_named(&settings_page, Some(view_name(View::Settings)));

    // ── Remaining pages: still placeholders (PLAN.md §5 roadmap phase 3+
    // - per-instance management) ──
    for view in [View::InstanceDetail] {
        let placeholder = gtk::Label::new(Some(view_name(view)));
        stack.add_named(&placeholder, Some(view_name(view)));
    }

    window.set_child(Some(&stack));

    let state = Arc::new(Mutex::new(AppData {
        view: View::Login,
        accounts: AccountStore::load(),
        instances: InstanceStore::load(),
        auth: auth::AuthState::Resolving,
        poll_cancel: None,
    }));

    let widgets = Widgets {
        window: window.clone(),
        stack: stack.clone(),
        login_status_label,
        qr_picture,
        user_code_label,
        sign_in_btn: sign_in_btn.clone(),
        play_offline_btn: play_offline_btn.clone(),
        home_status_label,
        instance_list: instance_list.clone(),
        add_instance_btn: add_instance_btn.clone(),
        install_progress,
        version_status_label,
        version_list,
        accounts_list: accounts_list.clone(),
        add_account_btn: add_account_btn.clone(),
        settings_list,
        sound: audio::Player::new(),
        haptics: None,
    };

    // ── Initial state: NeedsLogin, with whatever account (if any) is
    // already cached from a previous run determining whether Play
    // Offline shows a real cached name or falls back to the OS username.
    {
        let cached = state.lock().unwrap().accounts.active().cloned();
        let display_name = cached
            .as_ref()
            .map(|a| a.username.clone())
            .unwrap_or_else(account::default_offline_username);
        widgets
            .play_offline_btn
            .set_label(&format!("Play Offline as {display_name}"));
        let initial = auth::AuthState::NeedsLogin { cached };
        state.lock().unwrap().auth = initial.clone();
        render_auth_state(&widgets, &initial);
    }
    widgets.focus_default_for(View::Login);

    {
        let state = state.clone();
        let widgets = widgets.clone();
        sign_in_btn.connect_clicked(move |_| start_sign_in(state.clone(), widgets.clone()));
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        play_offline_btn.connect_clicked(move |_| start_play_offline(&state, &widgets));
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        add_instance_btn
            .connect_clicked(move |_| open_version_picker(state.clone(), widgets.clone()));
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        add_account_btn
            .connect_clicked(move |_| start_add_account(state.clone(), widgets.clone()));
    }
    refresh_instance_grid(&state, &widgets);

    // Shared with the keyboard handler below, so it can tell whether a
    // real gamepad is currently connected.
    let gilrs_shared: Option<Rc<RefCell<gilrs::Gilrs>>> =
        gilrs::Gilrs::new().ok().map(|g| Rc::new(RefCell::new(g)));

    // ── Keyboard fallback ──
    // Arrow keys drive the same directional focus search as the D-Pad
    // (GTK doesn't wire arrow keys to focus movement on its own, unlike
    // Tab); Enter/Space already activate the focused widget via GTK's
    // own default key handling, so only Escape needs an explicit hook.
    //
    // Up/Down are disabled outright whenever a real gamepad is
    // connected - not just debounced. On at least one real device
    // (Steam Input, per its hostname in that device's own AppArmor
    // logs) the same D-Pad press is *also* synthesized as a keyboard
    // arrow key, and the desktop's own key-repeat treats that
    // synthesized key as genuinely held: it kept firing independent
    // "repeat" events for as long as the physical button was down,
    // each one spaced hundreds of milliseconds to over a second apart.
    // No debounce window can catch repeats spaced that far apart
    // without also breaking legitimate rapid keyboard navigation, so
    // the only reliable fix is to not listen to the keyboard path at
    // all while a gamepad exists to generate the same input for real.
    let key_controller = gtk::EventControllerKey::new();
    let state_kb = state.clone();
    let widgets_kb = widgets.clone();
    let gilrs_for_kb = gilrs_shared.clone();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        use gtk::gdk::Key;
        let gamepad_connected = gilrs_for_kb
            .as_ref()
            .is_some_and(|g| g.borrow().gamepads().any(|(_, gp)| gp.is_connected()));
        let handled = match key {
            Key::Up if !gamepad_connected => step_focus(&widgets_kb.window, false),
            Key::Down if !gamepad_connected => step_focus(&widgets_kb.window, true),
            // Found via a real device trace: `Key(65364)` (GDK_KEY_Down)
            // events keep arriving here even with a gamepad connected -
            // Steam Input synthesizes a keyboard echo of the D-Pad
            // press. Declining to act on it wasn't enough: returning
            // Propagation::Proceed for it (as the catch-all arm below
            // does) let GTK's *own* default arrow-key-to-focus
            // keybinding also process the same event afterward, moving
            // focus a second time through a path neither `step_focus`
            // nor anything else in this file ever sees or logs. These
            // two keys must be swallowed outright whenever a gamepad
            // is connected, not just left unhandled.
            Key::Up | Key::Down if gamepad_connected => true,
            Key::Left => widgets_kb.window.child_focus(gtk::DirectionType::Left),
            Key::Right => widgets_kb.window.child_focus(gtk::DirectionType::Right),
            Key::Escape => {
                handle_gp_back(&state_kb, &widgets_kb);
                true
            }
            Key::x | Key::X if is_on_home(&state_kb) => {
                open_accounts(&state_kb, &widgets_kb);
                true
            }
            Key::y | Key::Y if is_on_home(&state_kb) => {
                open_settings(&state_kb, &widgets_kb);
                true
            }
            _ => false,
        };
        if handled {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(key_controller);

    // ── Gamepad polling ──
    // One shared Gilrs instance, polled every 16ms via
    // glib::source::timeout_add_local, same cadence as gamepad-2048.
    // Button identity for face buttons is resolved exclusively through
    // input::classify_button (see input.rs for why); D-Pad directions
    // are handled here directly, same split gamepad-2048 uses.
    if let Some(gilrs) = gilrs_shared {
        let state_for_gp = state.clone();
        let widgets_for_gp = widgets.clone();
        let dpad_nav_gate_gp = Rc::new(RefCell::new(std::time::Instant::now() - DPAD_NAV_DEBOUNCE));
        glib::source::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let events: Vec<gilrs::Event> = {
                let mut gp = gilrs.borrow_mut();
                let mut events = Vec::new();
                while let Some(event) = gp.next_event() {
                    events.push(event);
                }
                events
            };
            for gilrs::Event { event, .. } in events {
                if let gilrs::EventType::ButtonPressed(button, _) = event {
                    match button {
                        gilrs::Button::DPadUp | gilrs::Button::DPadDown => {
                            nav_step_if_not_debounced(
                                &dpad_nav_gate_gp,
                                &widgets_for_gp.window,
                                button == gilrs::Button::DPadDown,
                            );
                        }
                        gilrs::Button::DPadLeft => {
                            widgets_for_gp.window.child_focus(gtk::DirectionType::Left);
                        }
                        gilrs::Button::DPadRight => {
                            widgets_for_gp.window.child_focus(gtk::DirectionType::Right);
                        }
                        other => match input::classify_button(other) {
                            Some(GamepadAction::Confirm) => {
                                handle_gp_confirm(&state_for_gp, &widgets_for_gp)
                            }
                            Some(GamepadAction::Back) => {
                                handle_gp_back(&state_for_gp, &widgets_for_gp)
                            }
                            Some(GamepadAction::OpenAccounts) if is_on_home(&state_for_gp) => {
                                open_accounts(&state_for_gp, &widgets_for_gp)
                            }
                            Some(GamepadAction::RefreshOrSettings) if is_on_home(&state_for_gp) => {
                                open_settings(&state_for_gp, &widgets_for_gp)
                            }
                            // TODO: SystemMenu (Start button power/system
                            // overlay) once that screen exists.
                            _ => {}
                        },
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    window.present();
    // Handheld target, not a resizable desktop window - see PLAN.md §4.
    window.fullscreen();
}
