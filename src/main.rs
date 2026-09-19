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

use gamepad_minecraft::{account, audio, auth, haptics, input, instance, net, qr};

use account::AccountStore;
use input::GamepadAction;
use instance::InstanceStore;

// ─── Application state ────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Login,
    Home,
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
                self.home_status_label.grab_focus();
            }
            _ => {}
        }
    }
}

fn view_name(view: View) -> &'static str {
    match view {
        View::Login => "login",
        View::Home => "home",
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
        View::InstanceDetail | View::Accounts | View::Settings => {
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
    home_status_label.set_can_focus(true);
    home_page.append(&home_status_label);

    let home_hint = gtk::Label::new(Some(
        "No instances installed yet. B: Quit  •  X: Accounts  •  Y: Settings",
    ));
    home_hint.set_css_classes(&["control-hint"]);
    home_page.append(&home_hint);

    stack.add_named(&home_page, Some(view_name(View::Home)));

    // ── Remaining pages: still placeholders (PLAN.md §5 roadmap phases
    // 3+ - instance grid, account switching, settings) ──
    for view in [View::InstanceDetail, View::Accounts, View::Settings] {
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

    // ── Keyboard fallback ──
    // Arrow keys drive the same directional focus search as the D-Pad
    // (GTK doesn't wire arrow keys to focus movement on its own, unlike
    // Tab); Enter/Space already activate the focused widget via GTK's
    // own default key handling, so only Escape needs an explicit hook.
    let key_controller = gtk::EventControllerKey::new();
    let state_kb = state.clone();
    let widgets_kb = widgets.clone();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        use gtk::gdk::Key;
        match key {
            Key::Up => widgets_kb.window.child_focus(gtk::DirectionType::Up),
            Key::Down => widgets_kb.window.child_focus(gtk::DirectionType::Down),
            Key::Left => widgets_kb.window.child_focus(gtk::DirectionType::Left),
            Key::Right => widgets_kb.window.child_focus(gtk::DirectionType::Right),
            Key::Escape => {
                handle_gp_back(&state_kb, &widgets_kb);
                true
            }
            _ => false,
        };
        glib::Propagation::Proceed
    });
    window.add_controller(key_controller);

    // ── Gamepad polling ──
    // One shared Gilrs instance, polled every 16ms via
    // glib::source::timeout_add_local, same cadence as gamepad-2048.
    // Button identity for face buttons is resolved exclusively through
    // input::classify_button (see input.rs for why); D-Pad directions
    // are handled here directly, same split gamepad-2048 uses.
    if let Ok(gilrs) = gilrs::Gilrs::new() {
        let gilrs = Rc::new(RefCell::new(gilrs));
        let state_for_gp = state.clone();
        let widgets_for_gp = widgets.clone();
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
                        gilrs::Button::DPadUp => {
                            widgets_for_gp.window.child_focus(gtk::DirectionType::Up);
                        }
                        gilrs::Button::DPadDown => {
                            widgets_for_gp.window.child_focus(gtk::DirectionType::Down);
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
                            // TODO: OpenAccounts / RefreshOrSettings / SystemMenu
                            // dispatch once the Accounts/Settings pages are
                            // built out.
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
