// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

use gamepad_minecraft::{
    account, audio, auth, fabric, haptics, input, instance, launch, net, pixelart, qr,
};

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
}

/// The tabs in Home's top bar, as in the official launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeTab {
    Play,
    Installations,
}

/// Home's two dropdown panels. They're drawn in-window over a dimming
/// scrim rather than as `gtk::Popover`s: a popover is its own surface,
/// so its items would sit outside the focus walk in `home_nav_rows` and
/// D-Pad navigation could escape it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dropdown {
    Accounts,
    Versions,
}

struct AppData {
    view: View,
    home_tab: HomeTab,
    dropdown: Option<Dropdown>,
    accounts: AccountStore,
    instances: InstanceStore,
    auth: auth::AuthState,
    /// Set while a device-code poll thread is running, so Back can
    /// cancel it instead of leaving it to poll Microsoft until expiry.
    poll_cancel: Option<Arc<AtomicBool>>,
    /// An installation whose Delete button has been pressed once and is
    /// waiting for a second press to confirm - throwing away several GB
    /// of downloads shouldn't be one stray A press away.
    pending_delete: Option<String>,
    /// Bumped by every `show_notice`, so an older notice's auto-hide
    /// timer can tell it's been superseded and leave the new one up.
    notice_serial: u64,
    /// True from the moment a launch is kicked off until Minecraft's
    /// process exits. `gilrs` reads the gamepad's raw evdev device
    /// directly, completely bypassing the compositor's window-focus
    /// routing - so unlike keyboard input, our own gilrs poll loop never
    /// stops seeing button presses just because Minecraft's window is
    /// now the one actually focused/visible. Without this flag, every
    /// "A" press during real gameplay (jumping, interacting - a
    /// constantly-used button) would still land on `handle_gp_confirm`,
    /// re-activate whichever button was last focused in our own
    /// (backgrounded) window - almost always the "Play" button the
    /// player just used - and spawn *another* independent `java`
    /// process on top of the one already running. Confirmed live: this
    /// is what was producing the repeated "small window + Mojang
    /// loading screen" flashes and runaway CPU/memory use during a
    /// single play session, not an actual Minecraft crash-and-restart.
    minecraft_running: bool,
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
    login_back_btn: gtk::Button,
    // Home: top bar
    top_bar: gtk::Box,
    account_btn: gtk::Button,
    account_avatar: gtk::Image,
    account_name_label: gtk::Label,
    account_kind_label: gtk::Label,
    tab_play_btn: gtk::Button,
    tab_installs_btn: gtk::Button,
    home_hint: gtk::Label,
    // Home: tab content
    home_tabs: gtk::Stack,
    new_install_btn: gtk::Button,
    installs_list: gtk::Box,
    // Home: bottom bar
    bottom_bar: gtk::CenterBox,
    version_btn: gtk::Button,
    version_title_label: gtk::Label,
    version_subtitle_label: gtk::Label,
    play_btn: gtk::Button,
    play_label: gtk::Picture,
    bottom_account_label: gtk::Label,
    // Home: overlays
    notice: gtk::Box,
    notice_label: gtk::Label,
    install_progress: gtk::ProgressBar,
    scrim: gtk::Box,
    accounts_panel: gtk::Box,
    accounts_items: gtk::Box,
    versions_panel: gtk::Box,
    versions_items: gtk::Box,
    // Version picker page
    version_status_label: gtk::Label,
    version_list: gtk::Box,
    grass_icon: gdk::Texture,
    sound: audio::Player,
    haptics: Option<haptics::Haptics>,
}

impl Widgets {
    /// Seeds keyboard/gamepad focus for `view`. Every screen transition
    /// must call this — a revealer/page swap without a refocus call was
    /// gamepad-2048's single biggest bug class (PLAN.md §5, lesson 2).
    /// Home's sub-state (open dropdown, active tab) is read back from
    /// the widgets themselves so this never needs `AppData`.
    fn focus_default_for(&self, view: View) {
        self.stack.set_visible_child_name(view_name(view));
        match view {
            View::Login => {
                if self.sign_in_btn.is_visible() && self.sign_in_btn.is_sensitive() {
                    self.sign_in_btn.grab_focus();
                } else {
                    self.play_offline_btn.grab_focus();
                }
            }
            View::Home => {
                if self.accounts_panel.is_visible() {
                    focus_current_item(self.accounts_panel.upcast_ref());
                } else if self.versions_panel.is_visible() {
                    focus_current_item(self.versions_panel.upcast_ref());
                } else if self.home_tabs.visible_child_name().as_deref() == Some("installations") {
                    let first_row = self
                        .installs_list
                        .first_child()
                        .and_then(|row| focusables(&row).into_iter().next());
                    match first_row {
                        Some(btn) => btn.grab_focus(),
                        None => self.new_install_btn.grab_focus(),
                    };
                } else if self.play_btn.is_sensitive() {
                    self.play_btn.grab_focus();
                } else {
                    self.version_btn.grab_focus();
                }
            }
            View::VersionPicker => {
                if let Some(first) = self.version_list.first_child() {
                    first.grab_focus();
                }
            }
        }
    }
}

fn view_name(view: View) -> &'static str {
    match view {
        View::Login => "login",
        View::Home => "home",
        View::VersionPicker => "version-picker",
    }
}

/// The Home-level shortcuts (X, Y, shoulder buttons) only apply on
/// Home - guarding on this avoids e.g. X popping open the account
/// switcher mid-QR-code sign-in.
fn is_on_home(state: &Arc<Mutex<AppData>>) -> bool {
    state.lock().unwrap().view == View::Home
}

// ─── Small widget helpers ──────────────────────────────────────────

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_css_classes(classes);
    label.set_xalign(0.0);
    label
}

/// Every visible, sensitive button inside `root`, in document order.
/// Buttons are the only things this UI ever focuses.
fn focusables(root: &gtk::Widget) -> Vec<gtk::Widget> {
    fn walk(widget: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        if !widget.is_visible() {
            return;
        }
        if widget.is::<gtk::Button>() {
            if widget.is_sensitive() {
                out.push(widget.clone());
            }
            return;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            walk(&c, out);
            child = c.next_sibling();
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// Focuses the dropdown item marked `.current`, else the first item.
fn focus_current_item(panel: &gtk::Widget) {
    let items = focusables(panel);
    let current = items.iter().find(|w| w.has_css_class("current"));
    if let Some(item) = current.or(items.first()) {
        item.grab_focus();
    }
}

fn texture_from_image(image: &pixelart::Image) -> gdk::MemoryTexture {
    gdk::MemoryTexture::new(
        image.width as i32,
        image.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from(image.rgba.as_slice()),
        (image.width * 4) as usize,
    )
}

/// Renders a QR code straight to a GPU-uploadable texture - no temp
/// files, no external image crate (see `qr.rs`).
fn qr_texture(image: &qr::QrImage) -> gdk::MemoryTexture {
    let bytes = glib::Bytes::from(image.rgba.as_slice());
    let stride = (image.width * 4) as usize;
    gdk::MemoryTexture::new(
        image.width as i32,
        image.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    )
}

/// The PLAY button's label, drawn in the blocky pixel font (see
/// `pixelart.rs`) rather than a system font.
fn play_label_texture(text: &str) -> gdk::MemoryTexture {
    texture_from_image(&pixelart::render_text(
        text,
        4,
        [0xFF, 0xFF, 0xFF, 0xFF],
        [0x2B, 0x2B, 0x2B, 0xFF],
    ))
}

fn avatar_texture(seed: &str, size: i32) -> gdk::MemoryTexture {
    texture_from_image(&pixelart::render_avatar(seed, (size as u32 / 12).max(2)))
}

/// A circular avatar, clipped by its `.avatar` frame.
fn avatar_widget(seed: &str, size: i32) -> gtk::Widget {
    let image = gtk::Image::from_paintable(Some(&avatar_texture(seed, size)));
    image.set_pixel_size(size);
    let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    frame.add_css_class("avatar");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.set_valign(gtk::Align::Center);
    frame.append(&image);
    frame.upcast()
}

/// One row of a dropdown panel: icon, title, subtitle, and a check
/// mark on the current choice.
fn dropdown_item(icon: &gtk::Widget, title: &str, subtitle: &str, current: bool) -> gtk::Button {
    let btn = gtk::Button::new();
    btn.set_css_classes(&["flat-mc", "dropdown-item"]);
    set_class(&btn, "current", current);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    row.append(icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    text.append(&label(title, &["item-title"]));
    text.append(&label(subtitle, &["item-subtitle"]));
    row.append(&text);
    if current {
        let check = gtk::Label::new(Some("✓"));
        check.add_css_class("check");
        row.append(&check);
    }
    btn.set_child(Some(&row));
    btn
}

fn grass_image(widgets: &Widgets, size: i32) -> gtk::Widget {
    let image = gtk::Image::from_paintable(Some(&widgets.grass_icon));
    image.set_pixel_size(size);
    image.upcast()
}

fn icon_image(name: &str, size: i32) -> gtk::Widget {
    let image = gtk::Image::from_icon_name(name);
    image.set_pixel_size(size);
    image.upcast()
}

/// "Latest release" for the newest installed version, like the
/// official launcher's default profile name; "Release" otherwise.
fn installation_title(store: &InstanceStore, meta: &InstanceMeta) -> String {
    let newest = store.newest_installed().is_some_and(|n| n.id == meta.id);
    if newest { "Latest release" } else { "Release" }.to_string()
}

fn installation_subtitle(meta: &InstanceMeta) -> String {
    let loader = match meta.loader {
        instance::Loader::Fabric => "Fabric + Controlify",
        instance::Loader::Vanilla => "Vanilla",
    };
    match meta.state {
        InstallState::Installed => format!("{}  ·  {loader}", meta.mc_version),
        InstallState::Downloading => format!("{}  ·  Installing…", meta.mc_version),
        InstallState::Failed => format!("{}  ·  Install failed", meta.mc_version),
        InstallState::NotInstalled => format!("{}  ·  Not installed", meta.mc_version),
    }
}

// ─── Auth state rendering ──────────────────────────────────────────

/// Applies `auth` to every widget that reflects sign-in state. This is
/// the single place that decides what the Login page and Home's account
/// displays show for a given `AuthState`, so there is exactly one source
/// of truth instead of call sites disagreeing about what a given state
/// looks like.
fn render_auth_state(widgets: &Widgets, auth: &auth::AuthState) {
    match auth {
        auth::AuthState::Resolving => {
            widgets.login_status_label.set_label("Loading...");
            widgets.qr_picture.set_visible(false);
            widgets.user_code_label.set_visible(false);
            widgets.sign_in_btn.set_visible(true);
            widgets.sign_in_btn.set_sensitive(true);
        }
        auth::AuthState::NeedsLogin { .. } => {
            widgets
                .login_status_label
                .set_label("Sign in to play online, or play offline.");
            widgets.qr_picture.set_visible(false);
            widgets.user_code_label.set_visible(false);
            widgets.sign_in_btn.set_visible(true);
            widgets.sign_in_btn.set_sensitive(true);
        }
        auth::AuthState::AwaitingUser {
            user_code,
            verification_uri,
            qr_data,
            ..
        } => {
            widgets.login_status_label.set_label(&format!(
                "Scan the code with your phone, or go to {verification_uri} and enter:"
            ));
            widgets.user_code_label.set_label(user_code);
            widgets.user_code_label.set_visible(true);
            match qr::render_verification_qr(qr_data, 6) {
                Ok(image) => {
                    widgets.qr_picture.set_paintable(Some(&qr_texture(&image)));
                    widgets.qr_picture.set_visible(true);
                }
                Err(_) => widgets.qr_picture.set_visible(false),
            }
            // Nothing to press while waiting - the status text says
            // what's happening, and hiding it keeps the card short
            // enough for a 800px-tall handheld screen.
            widgets.sign_in_btn.set_visible(false);
        }
        auth::AuthState::Verifying { profile } => {
            render_identity(widgets, &profile.username, &profile.uuid, "Verifying…");
        }
        auth::AuthState::LoggedIn { profile } => {
            render_identity(widgets, &profile.name, &profile.id, "Microsoft account");
        }
        auth::AuthState::OfflinePlaying { profile } => {
            let kind = if profile.refresh_token.is_some() {
                "Microsoft account · offline"
            } else {
                "Offline account"
            };
            render_identity(widgets, &profile.username, &profile.uuid, kind);
        }
        auth::AuthState::Error { message, .. } => {
            widgets.login_status_label.set_label(message);
            widgets.qr_picture.set_visible(false);
            widgets.user_code_label.set_visible(false);
            widgets.sign_in_btn.set_visible(true);
            widgets.sign_in_btn.set_sensitive(true);
        }
    }
}

/// The account switcher (top left) and the name in the bottom bar.
fn render_identity(widgets: &Widgets, name: &str, uuid: &str, kind: &str) {
    widgets.account_name_label.set_label(name);
    widgets.account_kind_label.set_label(kind);
    widgets
        .account_avatar
        .set_paintable(Some(&avatar_texture(uuid, 48)));
    widgets.bottom_account_label.set_label(name);
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
        widgets1.focus_default_for(View::Login);

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
    refresh_home(state, widgets);
    widgets.focus_default_for(View::Home);
}

/// Leaves the Login page for Home without signing in - only possible
/// when some account is already cached (i.e. the player came here via
/// "Add a Microsoft account"), and cancels any device-code polling.
fn return_home_from_login(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let cached = {
        let mut data = state.lock().unwrap();
        if let Some(cancel) = data.poll_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        data.accounts.active().cloned()
    };
    go_to_home(state, widgets, auth_state_for_cached(cached.as_ref()));
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

// ─── Home: tabs, bottom bar, installations ─────────────────────────

const HINT_PLAY: &str = "A Play  ·  X Accounts  ·  Y Versions  ·  LB/RB Tabs";
const HINT_INSTALLS: &str = "A Select  ·  B Back  ·  LB/RB Tabs";

fn set_home_tab(state: &Arc<Mutex<AppData>>, widgets: &Widgets, tab: HomeTab) {
    {
        let mut data = state.lock().unwrap();
        data.home_tab = tab;
        data.pending_delete = None;
    }
    let play = tab == HomeTab::Play;
    widgets
        .home_tabs
        .set_visible_child_name(if play { "play" } else { "installations" });
    set_class(&widgets.tab_play_btn, "active", play);
    set_class(&widgets.tab_installs_btn, "active", !play);
    widgets.bottom_bar.set_visible(play);
    widgets.play_btn.set_visible(play);
    widgets
        .home_hint
        .set_label(if play { HINT_PLAY } else { HINT_INSTALLS });
    refresh_home(state, widgets);
    widgets.focus_default_for(View::Home);
}

fn other_tab(tab: HomeTab) -> HomeTab {
    match tab {
        HomeTab::Play => HomeTab::Installations,
        HomeTab::Installations => HomeTab::Play,
    }
}

/// Re-renders everything on Home that depends on the installation list
/// or launch state: the version selector, the PLAY button, and (when
/// visible) the Installations tab. Called after every state change that
/// affects them, rather than patching individual widgets in place.
fn refresh_home(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let (store, running, tab) = {
        let data = state.lock().unwrap();
        (data.instances.clone(), data.minecraft_running, data.home_tab)
    };
    let selected = store.selected().cloned();

    match &selected {
        Some(meta) => {
            widgets
                .version_title_label
                .set_label(&installation_title(&store, meta));
            widgets
                .version_subtitle_label
                .set_label(&installation_subtitle(meta));
        }
        None => {
            widgets.version_title_label.set_label("No installations");
            widgets
                .version_subtitle_label
                .set_label("Add one to start playing");
        }
    }

    let (text, sensitive) = if running {
        ("PLAYING", false)
    } else {
        match selected.map(|m| m.state) {
            None | Some(InstallState::NotInstalled) => ("INSTALL", true),
            Some(InstallState::Installed) => ("PLAY", true),
            Some(InstallState::Downloading) => ("INSTALLING", false),
            Some(InstallState::Failed) => ("RETRY", true),
        }
    };
    widgets.play_label.set_paintable(Some(&play_label_texture(text)));
    let had_focus = widgets.play_btn.has_focus();
    widgets.play_btn.set_sensitive(sensitive);
    if had_focus && !sensitive {
        // An insensitive widget can't hold focus; don't leave the
        // controller with nothing highlighted.
        widgets.version_btn.grab_focus();
    }

    if tab == HomeTab::Installations {
        refresh_installations(state, widgets);
    }
}

/// Where focus sits inside `container`'s rows, as (row, column), so a
/// rebuild of those rows can put it back in the same place.
fn focus_position_in(container: &gtk::Box, window: &ApplicationWindow) -> Option<(usize, usize)> {
    let focused = gtk::prelude::RootExt::focus(window)?;
    let mut row = container.first_child();
    let mut r = 0;
    while let Some(w) = row {
        if let Some(c) = focusables(&w).iter().position(|f| f == &focused) {
            return Some((r, c));
        }
        r += 1;
        row = w.next_sibling();
    }
    None
}

fn refresh_installations(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let restore = focus_position_in(&widgets.installs_list, &widgets.window);
    while let Some(child) = widgets.installs_list.first_child() {
        widgets.installs_list.remove(&child);
    }

    let (store, pending_delete, running) = {
        let data = state.lock().unwrap();
        (data.instances.clone(), data.pending_delete.clone(), data.minecraft_running)
    };
    if store.instances.is_empty() {
        let empty = label(
            "No installations yet. Choose \"New installation\" to download a Minecraft version.",
            &["item-subtitle"],
        );
        empty.set_wrap(true);
        widgets.installs_list.append(&empty);
    }

    let selected_id = store.selected().map(|m| m.id.clone());
    let mut metas = store.instances.clone();
    metas.sort_by(|a, b| instance::compare_versions(&b.mc_version, &a.mc_version));
    for meta in metas {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        row.add_css_class("install-row");
        set_class(&row, "current", selected_id.as_deref() == Some(meta.id.as_str()));
        row.append(&grass_image(widgets, 48));

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&label(&installation_title(&store, &meta), &["item-title"]));
        text.append(&label(&installation_subtitle(&meta), &["item-subtitle"]));
        row.append(&text);

        let play_text = match meta.state {
            InstallState::Installed => "Play",
            InstallState::Downloading => "Installing…",
            InstallState::Failed => "Retry",
            InstallState::NotInstalled => "Install",
        };
        let play = gtk::Button::with_label(play_text);
        play.set_css_classes(&["flat-mc", "mc-button", "green"]);
        play.set_valign(gtk::Align::Center);
        play.set_sensitive(!running && meta.state != InstallState::Downloading);
        {
            let (state, widgets, id) = (state.clone(), widgets.clone(), meta.id.clone());
            play.connect_clicked(move |_| {
                select_installation(&state, &id);
                play_selected(&state, &widgets);
            });
        }
        row.append(&play);

        let confirming = pending_delete.as_deref() == Some(meta.id.as_str());
        let delete = gtk::Button::with_label(if confirming { "Confirm delete" } else { "Delete" });
        delete.set_css_classes(&["flat-mc", "mc-button"]);
        set_class(&delete, "danger", confirming);
        delete.set_valign(gtk::Align::Center);
        delete.set_sensitive(meta.state != InstallState::Downloading);
        {
            let (state, widgets, id) = (state.clone(), widgets.clone(), meta.id.clone());
            delete.connect_clicked(move |_| delete_installation(&state, &widgets, id.clone()));
        }
        row.append(&delete);
        widgets.installs_list.append(&row);
    }

    if let Some((r, c)) = restore {
        let rows: Vec<Vec<gtk::Widget>> = {
            let mut rows = Vec::new();
            let mut child = widgets.installs_list.first_child();
            while let Some(w) = child {
                rows.push(focusables(&w));
                child = w.next_sibling();
            }
            rows
        };
        let target = rows
            .get(r.min(rows.len().saturating_sub(1)))
            .and_then(|row| row.get(c.min(row.len().saturating_sub(1))).or(row.first()));
        match target {
            Some(w) => w.grab_focus(),
            None => widgets.new_install_btn.grab_focus(),
        };
    }
}

fn select_installation(state: &Arc<Mutex<AppData>>, id: &str) {
    let mut data = state.lock().unwrap();
    data.instances.selected = Some(id.to_string());
    let _ = data.instances.save();
}

/// What the PLAY button does for the selected installation: launch it,
/// (re)try installing it, or - with nothing installed yet - go pick a
/// version.
fn play_selected(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let selected = state.lock().unwrap().instances.selected().cloned();
    match selected {
        None => open_version_picker(state.clone(), widgets.clone()),
        Some(meta) => match meta.state {
            InstallState::Installed => start_launch_instance(state, widgets, meta),
            InstallState::Downloading => {}
            InstallState::Failed | InstallState::NotInstalled => {
                start_install(state.clone(), widgets.clone(), meta)
            }
        },
    }
}

/// First press arms the delete (the button turns red and asks to
/// confirm); a second press on the same installation actually deletes
/// it, freeing its disk space.
fn delete_installation(state: &Arc<Mutex<AppData>>, widgets: &Widgets, id: String) {
    let confirmed = {
        let mut data = state.lock().unwrap();
        if data.pending_delete.as_deref() == Some(id.as_str()) {
            data.pending_delete = None;
            data.instances.instances.retain(|i| i.id != id);
            if data.instances.selected.as_deref() == Some(id.as_str()) {
                data.instances.selected = None;
            }
            let _ = data.instances.save();
            true
        } else {
            data.pending_delete = Some(id.clone());
            false
        }
    };
    if confirmed {
        let _ = std::fs::remove_dir_all(InstanceStore::instance_dir(&id));
        show_notice(state, widgets, &format!("Deleted Minecraft {id}."), false);
    }
    refresh_home(state, widgets);
}

// ─── Notices ────────────────────────────────────────────────────────
// The light-green banner in the hero's corner: install progress and
// the outcome of installs/launches. Transient ones hide themselves.

const NOTICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

fn show_notice(state: &Arc<Mutex<AppData>>, widgets: &Widgets, text: &str, progress: bool) {
    let serial = {
        let mut data = state.lock().unwrap();
        data.notice_serial += 1;
        data.notice_serial
    };
    widgets.notice_label.set_label(text);
    widgets.install_progress.set_visible(progress);
    widgets.notice.set_visible(true);
    if !progress {
        let (state, widgets) = (state.clone(), widgets.clone());
        glib::timeout_add_local_once(NOTICE_TIMEOUT, move || {
            if state.lock().unwrap().notice_serial == serial {
                widgets.notice.set_visible(false);
            }
        });
    }
}

// ─── Dropdowns ──────────────────────────────────────────────────────

fn open_dropdown(state: &Arc<Mutex<AppData>>, widgets: &Widgets, which: Dropdown) {
    {
        let mut data = state.lock().unwrap();
        if data.view != View::Home {
            return;
        }
        data.dropdown = Some(which);
    }
    match which {
        Dropdown::Accounts => populate_accounts_dropdown(state, widgets),
        Dropdown::Versions => populate_versions_dropdown(state, widgets),
    }
    widgets.scrim.set_visible(true);
    widgets
        .accounts_panel
        .set_visible(which == Dropdown::Accounts);
    widgets
        .versions_panel
        .set_visible(which == Dropdown::Versions);
    widgets.focus_default_for(View::Home);
}

/// X/Y and the switcher buttons toggle their dropdown.
fn toggle_dropdown(state: &Arc<Mutex<AppData>>, widgets: &Widgets, which: Dropdown) {
    let open = state.lock().unwrap().dropdown;
    match open {
        Some(current) if current == which => close_dropdown(state, widgets),
        Some(_) => {
            close_dropdown(state, widgets);
            open_dropdown(state, widgets, which);
        }
        None => open_dropdown(state, widgets, which),
    }
}

/// Hides any open dropdown and hands focus back to the control that
/// opened it.
fn close_dropdown(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let (which, tab) = {
        let mut data = state.lock().unwrap();
        (data.dropdown.take(), data.home_tab)
    };
    widgets.scrim.set_visible(false);
    widgets.accounts_panel.set_visible(false);
    widgets.versions_panel.set_visible(false);
    match which {
        Some(Dropdown::Accounts) => {
            widgets.account_btn.grab_focus();
        }
        Some(Dropdown::Versions) if tab == HomeTab::Play => {
            widgets.version_btn.grab_focus();
        }
        _ => widgets.focus_default_for(View::Home),
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

// Switching or adding an account is Home-adjacent, not its own auth
// flow: switching just re-points AppData.accounts.active_uuid and
// derives an optimistic AuthState from the cached entry (no network
// call - matches PLAN.md §3's "Home renders immediately from cache").
// Adding another Microsoft account reuses the exact same Login-page
// device-code flow, just entered from here instead of a fresh launch.
fn populate_accounts_dropdown(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    clear_box(&widgets.accounts_items);
    let (accounts, active_uuid) = {
        let data = state.lock().unwrap();
        (data.accounts.accounts.clone(), data.accounts.active_uuid.clone())
    };
    for acc in accounts {
        let is_active = active_uuid.as_deref() == Some(acc.uuid.as_str());
        let kind = if acc.refresh_token.is_some() {
            "Microsoft account"
        } else {
            "Offline account"
        };
        let item = dropdown_item(&avatar_widget(&acc.uuid, 40), &acc.username, kind, is_active);
        widgets.accounts_items.append(&item);
        let (state, widgets, uuid) = (state.clone(), widgets.clone(), acc.uuid.clone());
        item.connect_clicked(move |_| switch_active_account(&state, &widgets, uuid.clone()));
    }
}

fn populate_versions_dropdown(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    clear_box(&widgets.versions_items);
    let store = state.lock().unwrap().instances.clone();
    let selected_id = store.selected().map(|m| m.id.clone());
    let mut metas = store.instances.clone();
    metas.sort_by(|a, b| instance::compare_versions(&b.mc_version, &a.mc_version));
    if metas.is_empty() {
        let empty = label("Nothing installed yet.", &["item-subtitle"]);
        empty.set_margin_start(14);
        empty.set_margin_top(8);
        empty.set_margin_bottom(8);
        widgets.versions_items.append(&empty);
    }
    for meta in metas {
        let item = dropdown_item(
            &grass_image(widgets, 40),
            &installation_title(&store, &meta),
            &installation_subtitle(&meta),
            selected_id.as_deref() == Some(meta.id.as_str()),
        );
        widgets.versions_items.append(&item);
        let (state, widgets, id) = (state.clone(), widgets.clone(), meta.id.clone());
        item.connect_clicked(move |_| {
            select_installation(&state, &id);
            close_dropdown(&state, &widgets);
            refresh_home(&state, &widgets);
        });
    }
}

/// Derives an optimistic `AuthState` from a cached account with no
/// network call: `LoggedIn` if it has a refresh token (upgraded to a
/// freshly-verified session by a silent refresh shortly after, see
/// `start_silent_refresh`), `OfflinePlaying` otherwise. `None` means no
/// account has ever been cached at all.
fn auth_state_for_cached(account: Option<&CachedAccount>) -> auth::AuthState {
    match account {
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
    }
}

fn switch_active_account(state: &Arc<Mutex<AppData>>, widgets: &Widgets, uuid: String) {
    let account = {
        let mut data = state.lock().unwrap();
        data.accounts.active_uuid = Some(uuid);
        let _ = data.accounts.save();
        data.accounts.active().cloned()
    };
    close_dropdown(state, widgets);
    go_to_home(state, widgets, auth_state_for_cached(account.as_ref()));
    if let Some(account) = account {
        start_silent_refresh(state, widgets, account);
    }
}

/// Opens the same device-code sign-in flow the Login page uses,
/// entered from the account switcher instead of a fresh launch - a
/// second successful sign-in just adds another cached account rather
/// than replacing the current one (`account::AccountStore::upsert` keys
/// on UUID).
fn start_add_account(state: Arc<Mutex<AppData>>, widgets: Widgets) {
    close_dropdown(&state, &widgets);
    {
        let mut data = state.lock().unwrap();
        data.view = View::Login;
    }
    widgets.login_back_btn.set_visible(true);
    widgets.stack.set_visible_child_name(view_name(View::Login));
    start_sign_in(state, widgets);
}

// ─── Version picker ─────────────────────────────────────────────────

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
    clear_box(&widgets.version_list);

    let state1 = state.clone();
    let widgets1 = widgets.clone();
    net::spawn_blocking(
        // Combined into one background job (rather than two independent
        // `spawn_blocking` calls) so the list is only ever rendered once
        // both are in - filtering needs Controlify's supported-version
        // set before it can decide what to show at all.
        || {
            let manifest = instance::fetch_version_manifest()?;
            let controlify_versions = fabric::fetch_controlify_supported_versions()?;
            Ok((manifest, controlify_versions))
        },
        move |result| {
            let (manifest, controlify_versions) = match result {
                Ok(m) => m,
                Err(e) => {
                    widgets1
                        .version_status_label
                        .set_label(&format!("Couldn't fetch version list: {e}"));
                    return;
                }
            };
            widgets1.version_status_label.set_label(&format!(
                "The {VERSION_PICKER_LIMIT} most recent releases with controller support:"
            ));

            // Only releases Controlify has a Fabric build for are offered -
            // anything newer that Controlify hasn't caught up to yet would
            // otherwise install and then leave the player with no
            // controller input in-game at all.
            let releases: Vec<_> = manifest
                .versions
                .iter()
                .filter(|v| v.kind == "release" && controlify_versions.contains(&v.id))
                .take(VERSION_PICKER_LIMIT)
                .collect();
            for (i, entry) in releases.into_iter().enumerate() {
                let text = if i == 0 {
                    format!("{}  ·  latest supported", entry.id)
                } else {
                    entry.id.clone()
                };
                let btn = gtk::Button::with_label(&text);
                btn.set_css_classes(&["flat-mc", "list-item"]);
                if let Some(child) = btn.child().and_downcast::<gtk::Label>() {
                    child.set_xalign(0.0);
                }

                let state2 = state1.clone();
                let widgets2 = widgets1.clone();
                let mc_version = entry.id.clone();
                btn.connect_clicked(move |_| {
                    open_or_install_version(state2.clone(), widgets2.clone(), mc_version.clone())
                });
                widgets1.version_list.append(&btn);
            }
            // Only take focus if the player is still on this page - the
            // fetch may finish after they've already backed out.
            if state1.lock().unwrap().view == View::VersionPicker {
                widgets1.focus_default_for(View::VersionPicker);
            }
        },
    );
}

/// Adds `mc_version` as a new instance and starts installing it, unless
/// it's already been added before (installed, failed, or mid-download)
/// - in which case there's nothing new to do here. Either way it
/// becomes the selected installation and the player lands on the Play
/// tab, where the PLAY button and progress banner reflect it.
fn open_or_install_version(state: Arc<Mutex<AppData>>, widgets: Widgets, mc_version: String) {
    let already_added = {
        let mut data = state.lock().unwrap();
        data.view = View::Home;
        data.instances.instances.iter().any(|i| i.id == mc_version)
    };
    select_installation(&state, &mc_version);

    if already_added {
        widgets.stack.set_visible_child_name(view_name(View::Home));
        set_home_tab(&state, &widgets, HomeTab::Play);
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
    widgets.stack.set_visible_child_name(view_name(View::Home));
    set_home_tab(&state, &widgets, HomeTab::Play);
    start_install(state, widgets, meta);
}

// ─── Install & launch ──────────────────────────────────────────────

/// Downloads the vanilla client + Fabric Loader + Controlify for
/// `meta`, updating its `InstallState`, driving `install_progress`, and
/// re-rendering Home when done. The progress bar only reflects
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
    refresh_home(&state, &widgets);
    widgets.install_progress.set_fraction(0.0);
    show_notice(
        &state,
        &widgets,
        &format!("Installing Minecraft {}...", meta.mc_version),
        true,
    );

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
            let message = match result {
                Ok(None) => format!("Minecraft {} is ready to play.", meta.mc_version),
                Ok(Some(warning)) => warning,
                Err(e) => format!("Install failed: {e}"),
            };
            show_notice(&state2, &widgets2, &message, false);
            refresh_home(&state2, &widgets2);
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
    if state.lock().unwrap().minecraft_running {
        return;
    }

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
        show_notice(state, widgets, "Sign in or play offline first.", false);
        return;
    };

    // Marked running before the token refresh, not after: the refresh
    // is a network round trip, and a second press of PLAY during it
    // would otherwise start a second launch.
    state.lock().unwrap().minecraft_running = true;
    refresh_home(state, widgets);
    show_notice(
        state,
        widgets,
        &format!("Starting Minecraft {}...", meta.mc_version),
        false,
    );
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
    state.lock().unwrap().minecraft_running = true;

    let widgets2 = widgets.clone();
    let state2 = state.clone();
    net::spawn_blocking(
        move || -> Result<(), HttpError> {
            launch::spawn_minecraft(&meta, &account, access_token.as_deref(), |_event| {})
                .map_err(HttpError)
        },
        move |result| {
            state2.lock().unwrap().minecraft_running = false;
            let message = match &result {
                Ok(()) => "Minecraft exited.".to_string(),
                Err(e) => format!("Launch failed: {e}"),
            };
            show_notice(&state2, &widgets2, &message, false);
            refresh_home(&state2, &widgets2);
        },
    );
}

/// Opportunistic silent sign-in, run once at startup right after Home
/// has already rendered from the cached profile (PLAN.md §3: "Home
/// renders immediately from the cached profile either way"). Runs the
/// MSA refresh and the XBL/XSTS/Mojang chain as two separate network
/// stages rather than one combined call, so `Verifying` shows the moment
/// the cheap first call confirms the cached credential is still valid,
/// instead of the UI staying silent for the full four-request round
/// trip. A failure at either stage is never surfaced as an error - the
/// player is simply left on the offline identity they already had, same
/// as a failed refresh at launch time in `start_launch_instance`.
fn start_silent_refresh(state: &Arc<Mutex<AppData>>, widgets: &Widgets, account: CachedAccount) {
    let Some(refresh_token) = account.refresh_token.clone() else {
        return;
    };
    let state2 = state.clone();
    let widgets2 = widgets.clone();
    net::spawn_blocking(
        move || auth::refresh_silently(&refresh_token),
        move |result| {
            // Only act if nothing has moved the player on since this
            // refresh started (e.g. they haven't already switched
            // accounts or picked a different sign-in state by hand).
            let still_current = {
                let data = state2.lock().unwrap();
                matches!(data.auth, auth::AuthState::LoggedIn { .. })
                    && data.accounts.active_uuid.as_deref() == Some(account.uuid.as_str())
            };
            if !still_current {
                return;
            }
            let msa = match result {
                Ok(msa) => msa,
                Err(_) => {
                    fall_back_to_offline(&state2, &widgets2);
                    return;
                }
            };

            let verifying = auth::AuthState::Verifying {
                profile: account.clone(),
            };
            state2.lock().unwrap().auth = verifying.clone();
            render_auth_state(&widgets2, &verifying);

            let state3 = state2.clone();
            let widgets3 = widgets2.clone();
            let refresh_token = msa.refresh_token.clone();
            let access_token = msa.access_token.clone();
            net::spawn_blocking(
                move || auth::complete_mojang_login(&access_token),
                move |result| {
                    if !matches!(state3.lock().unwrap().auth, auth::AuthState::Verifying { .. }) {
                        return;
                    }
                    match result {
                        Ok(session) => {
                            let cached = account::CachedAccount {
                                username: session.profile.name.clone(),
                                uuid: session.profile.id.clone(),
                                refresh_token: Some(refresh_token),
                                obtained_at: chrono::Local::now()
                                    .format("%Y-%m-%d %H:%M")
                                    .to_string(),
                            };
                            let new_auth = auth::AuthState::LoggedIn {
                                profile: session.profile,
                            };
                            {
                                let mut data = state3.lock().unwrap();
                                data.accounts.upsert(cached);
                                let _ = data.accounts.save();
                                data.auth = new_auth.clone();
                            }
                            render_auth_state(&widgets3, &new_auth);
                        }
                        Err(_) => fall_back_to_offline(&state3, &widgets3),
                    }
                },
            );
        },
    );
}

/// Shared failure path for `start_silent_refresh`'s two stages: drop
/// back to the cached offline identity without surfacing an error
/// (PLAN.md §3).
fn fall_back_to_offline(state: &Arc<Mutex<AppData>>, widgets: &Widgets) {
    let profile = state.lock().unwrap().accounts.active().cloned();
    if let Some(profile) = profile {
        let new_auth = auth::AuthState::OfflinePlaying { profile };
        state.lock().unwrap().auth = new_auth.clone();
        render_auth_state(widgets, &new_auth);
    }
}

// ─── Navigation ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nav {
    Up,
    Down,
    Left,
    Right,
}

/// Routes one D-Pad/arrow step. Home is laid out in two dimensions (a
/// top bar of tabs, a bottom bar of controls, a list of installation
/// rows each with its own buttons), so it gets an explicit row/column
/// grid (`home_nav_rows` + `grid_step`) instead of GTK's geometric
/// `child_focus` search - the same reasoning as PLAN.md §5, lesson 4,
/// applied in both directions. The other pages are single columns and
/// keep the document-order walk in `step_focus`.
fn navigate(state: &Arc<Mutex<AppData>>, widgets: &Widgets, dir: Nav) -> bool {
    let view = state.lock().unwrap().view;
    match view {
        View::Home => {
            let rows = home_nav_rows(state, widgets);
            let focused = gtk::prelude::RootExt::focus(&widgets.window);
            grid_step(&rows, focused.as_ref(), dir)
        }
        _ => match dir {
            Nav::Up => step_focus(&widgets.window, false),
            Nav::Down => step_focus(&widgets.window, true),
            Nav::Left => widgets.window.child_focus(gtk::DirectionType::Left),
            Nav::Right => widgets.window.child_focus(gtk::DirectionType::Right),
        },
    }
}

/// Home's focusable controls as rows, top to bottom. An open dropdown
/// is modal: its items are the only rows, so the D-Pad can't wander out
/// of it onto the dimmed page behind.
fn home_nav_rows(state: &Arc<Mutex<AppData>>, widgets: &Widgets) -> Vec<Vec<gtk::Widget>> {
    let (tab, dropdown) = {
        let data = state.lock().unwrap();
        (data.home_tab, data.dropdown)
    };
    if let Some(which) = dropdown {
        let panel = match which {
            Dropdown::Accounts => &widgets.accounts_panel,
            Dropdown::Versions => &widgets.versions_panel,
        };
        return focusables(panel.upcast_ref())
            .into_iter()
            .map(|w| vec![w])
            .collect();
    }

    let mut rows = vec![focusables(widgets.top_bar.upcast_ref())];
    match tab {
        HomeTab::Play => {
            let bottom: Vec<gtk::Widget> = [
                widgets.version_btn.clone().upcast::<gtk::Widget>(),
                widgets.play_btn.clone().upcast(),
            ]
            .into_iter()
            .filter(|w| w.is_sensitive())
            .collect();
            rows.push(bottom);
        }
        HomeTab::Installations => {
            rows.push(vec![widgets.new_install_btn.clone().upcast()]);
            let mut child = widgets.installs_list.first_child();
            while let Some(row) = child {
                rows.push(focusables(&row));
                child = row.next_sibling();
            }
        }
    }
    rows
}

/// Moves focus one step across `rows`. Left/Right stay within the
/// current row; Up/Down move to the neighbouring row, keeping the
/// column where that row is wide enough. Never wraps. Returns whether
/// focus moved.
fn grid_step(rows: &[Vec<gtk::Widget>], focused: Option<&gtk::Widget>, dir: Nav) -> bool {
    let rows: Vec<&Vec<gtk::Widget>> = rows.iter().filter(|r| !r.is_empty()).collect();
    let position = focused.and_then(|f| {
        rows.iter()
            .enumerate()
            .find_map(|(r, row)| row.iter().position(|w| w == f).map(|c| (r, c)))
    });
    let Some((r, c)) = position else {
        // Focus is nowhere sensible (e.g. its widget was just rebuilt
        // away) - recover onto the first control rather than ignoring
        // the press.
        return rows
            .first()
            .and_then(|row| row.first())
            .is_some_and(|w| w.grab_focus());
    };
    let target = match dir {
        Nav::Left => c.checked_sub(1).map(|c| &rows[r][c]),
        Nav::Right => rows[r].get(c + 1),
        Nav::Up => r.checked_sub(1).map(|r| &rows[r][c.min(rows[r].len() - 1)]),
        Nav::Down => rows.get(r + 1).map(|row| &row[c.min(row.len() - 1)]),
    };
    target.is_some_and(|w| w.grab_focus())
}

/// Moves focus to the next/previous focusable widget in document order,
/// instead of GTK's generic geometric `child_focus` search (PLAN.md §5,
/// lesson 4). That search is unreliable across a tall stack of
/// same-sized buttons - the exact bug gamepad-2048 hit for its
/// board-size row - and manifests on the longer single-column lists
/// (the version picker especially): Up/Down would unpredictably skip
/// several rows at once instead of moving one at a time.
///
/// `focus_relative` below climbs to the parent and keeps looking there
/// once a container's own siblings are exhausted, and descends into a
/// sibling that's itself a container to find its first/last focusable
/// descendant - a real (if small) preorder tree walk rather than a
/// single sibling hop. It stops dead at each screen's own top-level page
/// (detected by the page being a direct child of `stack`) rather than
/// climbing further, so it can never wrap around into a different,
/// hidden page's own widgets. Deliberately doesn't wrap at the ends of a
/// screen, same as gamepad-2048's own documented behavior.
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
                // top-level widget is a traversal boundary, so this
                // can't wrap into a completely different (hidden) page's
                // own widgets by climbing too far.
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
    if !widget.is_visible() {
        return false;
    }
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
// "skips rows" looked like, even though each individual step only ever
// moves one row on its own.
const DPAD_NAV_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);

/// Navigates unless another accepted step (from *either* input path)
/// happened within `DPAD_NAV_DEBOUNCE`. `gate` must be shared between
/// the gamepad and keyboard handlers - a debounce scoped to only one of
/// them can't catch a duplicate arriving through the other.
fn nav_if_not_debounced(
    gate: &Rc<RefCell<std::time::Instant>>,
    state: &Arc<Mutex<AppData>>,
    widgets: &Widgets,
    dir: Nav,
) -> bool {
    let mut last = gate.borrow_mut();
    if last.elapsed() < DPAD_NAV_DEBOUNCE {
        return false;
    }
    *last = std::time::Instant::now();
    drop(last);
    navigate(state, widgets, dir)
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
            } else if data.accounts.active().is_some() {
                drop(data);
                return_home_from_login(state, widgets);
            } else {
                drop(data);
                widgets.window.close();
            }
        }
        View::VersionPicker => {
            data.view = View::Home;
            drop(data);
            widgets.focus_default_for(View::Home);
        }
        View::Home => {
            if data.dropdown.is_some() {
                drop(data);
                close_dropdown(state, widgets);
            } else if data.home_tab == HomeTab::Installations {
                drop(data);
                set_home_tab(state, widgets, HomeTab::Play);
            }
            // TODO: quit-confirm overlay on the Play tab.
        }
    }
}

/// X/Y/shoulder-button shortcuts, shared by the gamepad and keyboard
/// paths.
fn handle_home_shortcut(state: &Arc<Mutex<AppData>>, widgets: &Widgets, action: GamepadAction) {
    if !is_on_home(state) {
        return;
    }
    match action {
        GamepadAction::OpenAccounts => toggle_dropdown(state, widgets, Dropdown::Accounts),
        GamepadAction::OpenVersions => toggle_dropdown(state, widgets, Dropdown::Versions),
        GamepadAction::PrevTab | GamepadAction::NextTab => {
            let (tab, dropdown) = {
                let data = state.lock().unwrap();
                (data.home_tab, data.dropdown)
            };
            if dropdown.is_none() {
                set_home_tab(state, widgets, other_tab(tab));
            }
        }
        _ => {}
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

fn bundled_texture(bytes: &'static [u8]) -> gdk::Texture {
    gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes))
        .expect("bundled PNG failed to decode")
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::new(app);
    window.set_title(Some("Minecraft"));
    window.set_default_size(1280, 800);

    // ── Theme: the official launcher's charcoal-and-green look (see
    // style.css), on GTK's dark variant so scrollbars etc. match.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_data(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("no display"),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // ── Bundled artwork, generated by examples/gen_art.rs ──
    let hero_texture = bundled_texture(include_bytes!("../data/art/hero.png"));
    let logo_texture = bundled_texture(include_bytes!("../data/art/logo.png"));
    let grass_icon = bundled_texture(include_bytes!("../data/art/grass_block.png"));

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    // ── Login page ──
    // The key art, dimmed, behind the logo and a sign-in card.
    let login_page = gtk::Overlay::new();
    let login_bg = gtk::Picture::for_paintable(&hero_texture);
    login_bg.set_content_fit(gtk::ContentFit::Cover);
    login_bg.set_can_shrink(true);
    login_page.set_child(Some(&login_bg));
    let login_scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    login_scrim.add_css_class("scrim");
    login_page.add_overlay(&login_scrim);

    let login_column = gtk::Box::new(gtk::Orientation::Vertical, 20);
    login_column.set_halign(gtk::Align::Center);
    login_column.set_valign(gtk::Align::Center);
    let login_logo = gtk::Picture::for_paintable(&logo_texture);
    login_logo.set_can_shrink(true);
    login_column.append(&login_logo);

    let login_card = gtk::Box::new(gtk::Orientation::Vertical, 14);
    login_card.add_css_class("login-card");
    login_card.set_halign(gtk::Align::Center);
    login_card.set_size_request(460, -1);

    let login_status_label = gtk::Label::new(Some("Loading..."));
    login_status_label.add_css_class("login-status");
    login_status_label.set_wrap(true);
    login_status_label.set_max_width_chars(40);
    login_status_label.set_justify(gtk::Justification::Center);
    login_card.append(&login_status_label);

    let qr_picture = gtk::Picture::new();
    qr_picture.set_size_request(220, 220);
    qr_picture.set_content_fit(gtk::ContentFit::Contain);
    qr_picture.set_visible(false);
    login_card.append(&qr_picture);

    let user_code_label = gtk::Label::new(None);
    user_code_label.add_css_class("user-code");
    user_code_label.set_visible(false);
    login_card.append(&user_code_label);

    let sign_in_btn = gtk::Button::with_label("Sign in with Microsoft");
    sign_in_btn.set_css_classes(&["flat-mc", "mc-button", "green"]);
    login_card.append(&sign_in_btn);

    let play_offline_btn = gtk::Button::with_label("Play Offline");
    play_offline_btn.set_css_classes(&["flat-mc", "mc-button"]);
    login_card.append(&play_offline_btn);

    let login_back_btn = gtk::Button::with_label("Back");
    login_back_btn.set_css_classes(&["flat-mc", "mc-button"]);
    login_back_btn.set_visible(false);
    login_card.append(&login_back_btn);

    let login_hint = gtk::Label::new(Some("A Select  ·  B Cancel / Back"));
    login_hint.add_css_class("control-hint");
    login_card.append(&login_hint);

    login_column.append(&login_card);
    login_page.add_overlay(&login_column);
    stack.add_named(&login_page, Some(view_name(View::Login)));

    // ── Home page ──
    // Top bar: account switcher | edition title + tabs | hint, quit.
    let top_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    top_bar.add_css_class("top-bar");

    let account_btn = gtk::Button::new();
    account_btn.set_css_classes(&["flat-mc", "account-switcher"]);
    let account_row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let account_avatar = gtk::Image::new();
    account_avatar.set_pixel_size(48);
    let avatar_frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    avatar_frame.add_css_class("avatar");
    avatar_frame.set_overflow(gtk::Overflow::Hidden);
    avatar_frame.set_valign(gtk::Align::Center);
    avatar_frame.append(&account_avatar);
    account_row.append(&avatar_frame);
    let account_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    account_text.set_hexpand(true);
    account_text.set_valign(gtk::Align::Center);
    let account_name_label = label("", &["account-name"]);
    account_name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    account_name_label.set_max_width_chars(16);
    let account_kind_label = label("", &["account-kind"]);
    account_text.append(&account_name_label);
    account_text.append(&account_kind_label);
    account_row.append(&account_text);
    let account_chevron = icon_image("pan-down-symbolic", 16);
    account_chevron.set_valign(gtk::Align::Start);
    account_row.append(&account_chevron);
    account_btn.set_child(Some(&account_row));
    // The expanding text column inside would otherwise make the whole
    // switcher grab spare top-bar width on wide screens.
    account_btn.set_hexpand(false);
    top_bar.append(&account_btn);

    let title_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    title_column.set_margin_start(22);
    title_column.set_valign(gtk::Align::End);
    let edition_title = label("MINECRAFT: JAVA EDITION", &["edition-title"]);
    edition_title.set_margin_start(14);
    edition_title.set_margin_top(12);
    title_column.append(&edition_title);
    let tab_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let tab_button = |text: &str| {
        let btn = gtk::Button::new();
        btn.set_css_classes(&["flat-mc", "tab"]);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&gtk::Label::new(Some(text)));
        let underline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        underline.add_css_class("underline");
        underline.set_halign(gtk::Align::Center);
        column.append(&underline);
        btn.set_child(Some(&column));
        btn
    };
    let tab_play_btn = tab_button("Play");
    tab_play_btn.add_css_class("active");
    let tab_installs_btn = tab_button("Installations");
    tab_row.append(&tab_play_btn);
    tab_row.append(&tab_installs_btn);
    title_column.append(&tab_row);
    top_bar.append(&title_column);

    let top_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    top_spacer.set_hexpand(true);
    top_bar.append(&top_spacer);
    let home_hint = gtk::Label::new(Some(HINT_PLAY));
    home_hint.add_css_class("control-hint");
    home_hint.set_margin_end(12);
    top_bar.append(&home_hint);
    let quit_btn = gtk::Button::new();
    quit_btn.set_css_classes(&["flat-mc", "icon-btn"]);
    quit_btn.set_child(Some(&icon_image("window-close-symbolic", 24)));
    quit_btn.set_tooltip_text(Some("Quit"));
    quit_btn.set_valign(gtk::Align::Center);
    quit_btn.set_margin_end(12);
    top_bar.append(&quit_btn);

    // Tab content: the key art + logo, or the installation list.
    let home_tabs = gtk::Stack::new();
    home_tabs.set_vexpand(true);
    home_tabs.set_transition_type(gtk::StackTransitionType::Crossfade);

    let hero_overlay = gtk::Overlay::new();
    let hero = gtk::Picture::for_paintable(&hero_texture);
    hero.set_content_fit(gtk::ContentFit::Cover);
    hero.set_can_shrink(true);
    hero_overlay.set_child(Some(&hero));
    let hero_logo = gtk::Picture::for_paintable(&logo_texture);
    hero_logo.set_can_shrink(true);
    hero_logo.set_halign(gtk::Align::Center);
    hero_logo.set_valign(gtk::Align::Start);
    hero_logo.set_margin_top(28);
    hero_overlay.add_overlay(&hero_logo);
    home_tabs.add_named(&hero_overlay, Some("play"));

    let installs_page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    installs_page.add_css_class("installs-page");
    let installs_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let installs_title = label("Installations", &["page-title"]);
    installs_title.set_hexpand(true);
    installs_header.append(&installs_title);
    let new_install_btn = gtk::Button::with_label("New installation");
    new_install_btn.set_css_classes(&["flat-mc", "mc-button", "green"]);
    installs_header.append(&new_install_btn);
    installs_page.append(&installs_header);
    let installs_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let installs_scroll = gtk::ScrolledWindow::new();
    installs_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    installs_scroll.set_vexpand(true);
    installs_scroll.set_child(Some(&installs_list));
    installs_page.append(&installs_scroll);
    home_tabs.add_named(&installs_page, Some("installations"));

    // Bottom bar: installation selector | (PLAY, overlaid) | account.
    let bottom_bar = gtk::CenterBox::new();
    bottom_bar.add_css_class("bottom-bar");
    let version_btn = gtk::Button::new();
    version_btn.set_css_classes(&["flat-mc", "version-select"]);
    version_btn.set_valign(gtk::Align::Center);
    let version_row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let grass = gtk::Image::from_paintable(Some(&grass_icon));
    grass.set_pixel_size(40);
    version_row.append(&grass);
    let version_text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    version_text.set_hexpand(true);
    version_text.set_valign(gtk::Align::Center);
    let version_title_label = label("", &["item-title"]);
    let version_subtitle_label = label("", &["item-subtitle"]);
    version_text.append(&version_title_label);
    version_text.append(&version_subtitle_label);
    version_row.append(&version_text);
    version_row.append(&icon_image("pan-down-symbolic", 16));
    version_btn.set_child(Some(&version_row));
    version_btn.set_hexpand(false);
    bottom_bar.set_start_widget(Some(&version_btn));
    // Reserves the middle for the PLAY button, which is overlaid on
    // top so it can straddle the hero and the bar like the official
    // launcher's.
    let play_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    play_slot.set_size_request(400, -1);
    bottom_bar.set_center_widget(Some(&play_slot));
    let bottom_account_label = gtk::Label::new(None);
    bottom_account_label.add_css_class("bottom-account");
    bottom_account_label.set_margin_end(8);
    bottom_account_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    bottom_bar.set_end_widget(Some(&bottom_account_label));

    let home_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    home_column.append(&top_bar);
    home_column.append(&home_tabs);
    home_column.append(&bottom_bar);

    let home_page = gtk::Overlay::new();
    home_page.set_child(Some(&home_column));

    let play_btn = gtk::Button::new();
    play_btn.set_css_classes(&["flat-mc", "play"]);
    play_btn.set_halign(gtk::Align::Center);
    play_btn.set_valign(gtk::Align::End);
    play_btn.set_margin_bottom(30);
    let play_label = gtk::Picture::for_paintable(&play_label_texture("PLAY"));
    // A Picture measured against the button's full width would scale
    // itself up to fill it; inside a non-expanding horizontal box it
    // keeps the texture's own pixel size.
    let play_label_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    play_label_box.set_halign(gtk::Align::Center);
    play_label_box.set_valign(gtk::Align::Center);
    play_label_box.append(&play_label);
    play_btn.set_child(Some(&play_label_box));
    home_page.add_overlay(&play_btn);

    let notice = gtk::Box::new(gtk::Orientation::Vertical, 10);
    notice.add_css_class("notice");
    notice.set_halign(gtk::Align::End);
    notice.set_valign(gtk::Align::End);
    notice.set_margin_end(24);
    notice.set_margin_bottom(132);
    notice.set_visible(false);
    let notice_label = gtk::Label::new(None);
    notice_label.set_wrap(true);
    notice_label.set_max_width_chars(36);
    notice_label.set_justify(gtk::Justification::Center);
    notice.append(&notice_label);
    let install_progress = gtk::ProgressBar::new();
    install_progress.set_visible(false);
    notice.append(&install_progress);
    // Tapping the banner dismisses it.
    let notice_tap = gtk::GestureClick::new();
    {
        let notice = notice.clone();
        let progress = install_progress.clone();
        notice_tap.connect_released(move |_, _, _, _| {
            if !progress.is_visible() {
                notice.set_visible(false);
            }
        });
    }
    notice.add_controller(notice_tap);
    home_page.add_overlay(&notice);

    let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    scrim.add_css_class("scrim");
    scrim.set_visible(false);
    home_page.add_overlay(&scrim);

    let dropdown_panel = |heading: &str, footer: &gtk::Button| {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
        panel.add_css_class("dropdown");
        panel.set_size_request(400, -1);
        panel.set_halign(gtk::Align::Start);
        panel.set_visible(false);
        let title = label(heading, &["dropdown-heading"]);
        panel.append(&title);
        let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(380);
        scroll.set_child(Some(&items));
        panel.append(&scroll);
        panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        panel.append(footer);
        (panel, items)
    };

    let add_account_btn = dropdown_item(
        &icon_image("list-add-symbolic", 24),
        "Add a Microsoft account",
        "Sign in with a QR code",
        false,
    );
    let (accounts_panel, accounts_items) = dropdown_panel("ACCOUNTS", &add_account_btn);
    accounts_panel.set_valign(gtk::Align::Start);
    accounts_panel.set_margin_top(96);
    accounts_panel.set_margin_start(12);
    home_page.add_overlay(&accounts_panel);

    let new_install_item = dropdown_item(
        &icon_image("list-add-symbolic", 24),
        "New installation",
        "Download another Minecraft version",
        false,
    );
    let (versions_panel, versions_items) = dropdown_panel("INSTALLATIONS", &new_install_item);
    versions_panel.set_valign(gtk::Align::End);
    versions_panel.set_margin_bottom(104);
    versions_panel.set_margin_start(16);
    home_page.add_overlay(&versions_panel);

    stack.add_named(&home_page, Some(view_name(View::Home)));

    // ── Version picker page ──
    let version_page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    version_page.add_css_class("installs-page");
    let version_header = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let version_back_btn = gtk::Button::with_label("Back");
    version_back_btn.set_css_classes(&["flat-mc", "mc-button"]);
    version_header.append(&version_back_btn);
    version_header.append(&label("New installation", &["page-title"]));
    version_page.append(&version_header);

    let version_status_label = label("Fetching version list...", &["item-subtitle"]);
    version_page.append(&version_status_label);

    let version_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let version_scroll = gtk::ScrolledWindow::new();
    version_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    version_scroll.set_vexpand(true);
    version_scroll.set_child(Some(&version_list));
    version_page.append(&version_scroll);

    let version_hint = gtk::Label::new(Some("A Install  ·  B Back"));
    version_hint.add_css_class("control-hint");
    version_page.append(&version_hint);

    stack.add_named(&version_page, Some(view_name(View::VersionPicker)));

    window.set_child(Some(&stack));

    let state = Arc::new(Mutex::new(AppData {
        view: View::Login,
        home_tab: HomeTab::Play,
        dropdown: None,
        accounts: AccountStore::load(),
        instances: InstanceStore::load(),
        auth: auth::AuthState::Resolving,
        poll_cancel: None,
        pending_delete: None,
        notice_serial: 0,
        minecraft_running: false,
    }));

    let widgets = Widgets {
        window: window.clone(),
        stack: stack.clone(),
        login_status_label,
        qr_picture,
        user_code_label,
        sign_in_btn: sign_in_btn.clone(),
        play_offline_btn: play_offline_btn.clone(),
        login_back_btn: login_back_btn.clone(),
        top_bar,
        account_btn: account_btn.clone(),
        account_avatar,
        account_name_label,
        account_kind_label,
        tab_play_btn: tab_play_btn.clone(),
        tab_installs_btn: tab_installs_btn.clone(),
        home_hint,
        home_tabs,
        new_install_btn: new_install_btn.clone(),
        installs_list,
        bottom_bar,
        version_btn: version_btn.clone(),
        version_title_label,
        version_subtitle_label,
        play_btn: play_btn.clone(),
        play_label,
        bottom_account_label,
        notice,
        notice_label,
        install_progress,
        scrim: scrim.clone(),
        accounts_panel,
        accounts_items,
        versions_panel,
        versions_items,
        version_status_label,
        version_list,
        grass_icon,
        sound: audio::Player::new(),
        haptics: None,
    };

    // ── Initial state ──
    // A cached account (from a previous Microsoft sign-in or "Play
    // Offline") skips the Login page entirely: Home renders immediately
    // from the cached profile (PLAN.md §3), and a cached Microsoft
    // account additionally kicks off a silent refresh in the background
    // so the player is never sent through the device-code dance again
    // just because their MSA token went stale between launches. Only a
    // genuinely first-ever launch (no cached account at all) lands on
    // Login.
    {
        let cached = state.lock().unwrap().accounts.active().cloned();
        let display_name = cached
            .as_ref()
            .map(|a| a.username.clone())
            .unwrap_or_else(account::default_offline_username);
        widgets
            .play_offline_btn
            .set_label(&format!("Play Offline as {display_name}"));

        let initial = auth_state_for_cached(cached.as_ref());
        match cached {
            Some(account) => {
                go_to_home(&state, &widgets, initial);
                start_silent_refresh(&state, &widgets, account);
            }
            None => {
                state.lock().unwrap().auth = initial.clone();
                render_auth_state(&widgets, &initial);
                widgets.focus_default_for(View::Login);
            }
        }
    }

    // ── Button wiring ──
    // Every action is reachable by touch as well as by controller; the
    // gamepad path below simply activates the focused button.
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        sign_in_btn.connect_clicked(move |_| start_sign_in(state.clone(), widgets.clone()));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        play_offline_btn.connect_clicked(move |_| start_play_offline(&state, &widgets));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        login_back_btn.connect_clicked(move |_| return_home_from_login(&state, &widgets));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        account_btn
            .connect_clicked(move |_| toggle_dropdown(&state, &widgets, Dropdown::Accounts));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        tab_play_btn.connect_clicked(move |_| set_home_tab(&state, &widgets, HomeTab::Play));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        tab_installs_btn
            .connect_clicked(move |_| set_home_tab(&state, &widgets, HomeTab::Installations));
    }
    {
        let window = window.clone();
        quit_btn.connect_clicked(move |_| window.close());
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        version_btn
            .connect_clicked(move |_| toggle_dropdown(&state, &widgets, Dropdown::Versions));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        play_btn.connect_clicked(move |_| play_selected(&state, &widgets));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        new_install_btn
            .connect_clicked(move |_| open_version_picker(state.clone(), widgets.clone()));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        new_install_item.connect_clicked(move |_| {
            close_dropdown(&state, &widgets);
            open_version_picker(state.clone(), widgets.clone());
        });
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        add_account_btn
            .connect_clicked(move |_| start_add_account(state.clone(), widgets.clone()));
    }
    {
        let (state, widgets) = (state.clone(), widgets.clone());
        version_back_btn.connect_clicked(move |_| handle_gp_back(&state, &widgets));
    }
    {
        // Tapping outside an open dropdown closes it.
        let (state, widgets) = (state.clone(), widgets.clone());
        let tap = gtk::GestureClick::new();
        tap.connect_released(move |_, _, _, _| close_dropdown(&state, &widgets));
        scrim.add_controller(tap);
    }

    // Shared with the keyboard handler below, so it can tell whether a
    // real gamepad is currently connected.
    let gilrs_shared: Option<Rc<RefCell<gilrs::Gilrs>>> =
        gilrs::Gilrs::new().ok().map(|g| Rc::new(RefCell::new(g)));

    // ── Keyboard fallback ──
    // Arrow keys drive the same navigation as the D-Pad (GTK doesn't
    // wire arrow keys to focus movement the way this UI needs);
    // Enter/Space already activate the focused widget via GTK's own
    // default key handling, so only Escape and the shortcuts need
    // explicit hooks.
    //
    // Arrows are disabled outright whenever a real gamepad is
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
        use gdk::Key;
        let gamepad_connected = gilrs_for_kb
            .as_ref()
            .is_some_and(|g| g.borrow().gamepads().any(|(_, gp)| gp.is_connected()));
        let dir = match key {
            Key::Up => Some(Nav::Up),
            Key::Down => Some(Nav::Down),
            Key::Left => Some(Nav::Left),
            Key::Right => Some(Nav::Right),
            _ => None,
        };
        let handled = if let Some(dir) = dir {
            // Found via a real device trace: arrow events keep arriving
            // here even with a gamepad connected - Steam Input
            // synthesizes a keyboard echo of the D-Pad press. Declining
            // to act on it isn't enough: returning Propagation::Proceed
            // would let GTK's *own* default arrow-key-to-focus
            // keybinding also process the same event afterward, moving
            // focus a second time through a path nothing in this file
            // ever sees. So they're swallowed outright while a gamepad
            // is connected, not just left unhandled.
            if !gamepad_connected {
                navigate(&state_kb, &widgets_kb, dir);
            }
            true
        } else {
            let action = match key {
                Key::x | Key::X => Some(GamepadAction::OpenAccounts),
                Key::y | Key::Y => Some(GamepadAction::OpenVersions),
                Key::bracketleft => Some(GamepadAction::PrevTab),
                Key::bracketright => Some(GamepadAction::NextTab),
                _ => None,
            };
            match (key, action) {
                (Key::Escape, _) => {
                    handle_gp_back(&state_kb, &widgets_kb);
                    true
                }
                (_, Some(action)) if is_on_home(&state_kb) => {
                    handle_home_shortcut(&state_kb, &widgets_kb, action);
                    true
                }
                _ => false,
            }
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
    // Button identity for face/shoulder buttons is resolved exclusively
    // through input::classify_button (see input.rs for why); D-Pad
    // directions are handled here directly, same split gamepad-2048
    // uses.
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
            // Draining next_event() above (rather than bailing out
            // before it) still matters even while Minecraft owns the
            // screen: it keeps gilrs's own internal event queue from
            // growing unbounded for the whole play session. Only the
            // *reaction* to those events needs to be suppressed here -
            // see `AppData::minecraft_running`'s doc comment for why
            // gilrs itself can't tell that our window lost focus.
            if state_for_gp.lock().unwrap().minecraft_running {
                return glib::ControlFlow::Continue;
            }
            for gilrs::Event { event, .. } in events {
                if let gilrs::EventType::ButtonPressed(button, _) = event {
                    let dir = match button {
                        gilrs::Button::DPadUp => Some(Nav::Up),
                        gilrs::Button::DPadDown => Some(Nav::Down),
                        gilrs::Button::DPadLeft => Some(Nav::Left),
                        gilrs::Button::DPadRight => Some(Nav::Right),
                        _ => None,
                    };
                    if let Some(dir) = dir {
                        nav_if_not_debounced(&dpad_nav_gate_gp, &state_for_gp, &widgets_for_gp, dir);
                        continue;
                    }
                    match input::classify_button(button) {
                        Some(GamepadAction::Confirm) => {
                            handle_gp_confirm(&state_for_gp, &widgets_for_gp)
                        }
                        Some(GamepadAction::Back) => handle_gp_back(&state_for_gp, &widgets_for_gp),
                        // TODO: SystemMenu (Start button power/system
                        // overlay) once that screen exists.
                        Some(GamepadAction::SystemMenu) | None => {}
                        Some(action) => {
                            handle_home_shortcut(&state_for_gp, &widgets_for_gp, action)
                        }
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
