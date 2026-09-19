// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

mod account;
mod audio;
mod auth;
mod fabric;
mod haptics;
mod input;
mod instance;
mod launch;
mod net;
mod qr;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

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
    // TODO: per-page widget handles as each screen is built out (PLAN.md
    // §5 roadmap phase 3):
    //   Login: sign_in_btn, play_offline_btn, qr_picture, user_code_label
    //   Home: instance grid buttons, add_instance_btn
    //   InstanceDetail: play_btn, update_btn, delete_btn
    //   Accounts: account rows
    //   Settings: setting controls
    sound: audio::Player,
    haptics: Option<haptics::Haptics>,
}

impl Widgets {
    /// Seeds keyboard/gamepad focus for `view`. Every screen transition
    /// must call this — a revealer/page swap without a refocus call was
    /// gamepad-2048's single biggest bug class (PLAN.md §5, lesson 2).
    fn focus_default_for(&self, view: View) {
        self.stack.set_visible_child_name(view_name(view));
        // TODO: grab_focus() on the sensible default widget for `view`
        // once each page's real widget tree exists (e.g. "Sign In" or
        // "Play Offline" for Login, the first instance tile for Home).
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
    let next = match data.view {
        View::Login => {
            // TODO: if auth::AuthState::AwaitingUser, cancel the poll
            // thread and fall back to NeedsLogin instead of quitting.
            View::Login
        }
        View::InstanceDetail | View::Accounts | View::Settings => View::Home,
        View::Home => View::Home, // TODO: quit-confirm overlay
    };
    data.view = next;
    drop(data);
    widgets.focus_default_for(next);
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
    // gamepad-shell / gamepad-2048 (PLAN.md §4). TODO: fill in per-widget
    // rules (buttons, cards, hint text) as each screen is built.
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_data(
        r#"
        @define-color orange #E95420;
        @define-color orange_bright #F4703C;
        @define-color aubergine #2C001E;
        @define-color purple #77216F;

        * { font-family: "Ubuntu Sans", "Ubuntu", sans-serif; }
        window { background-color: #150610; color: #FFFFFF; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("no display"),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let stack = gtk::Stack::new();
    // TODO: build each page's real widget tree (PLAN.md §5). Placeholder
    // content only, so `View` navigation can be exercised end-to-end
    // before the real screens exist.
    for view in [
        View::Login,
        View::Home,
        View::InstanceDetail,
        View::Accounts,
        View::Settings,
    ] {
        let placeholder = gtk::Label::new(Some(view_name(view)));
        stack.add_named(&placeholder, Some(view_name(view)));
    }
    window.set_child(Some(&stack));

    let state = Arc::new(Mutex::new(AppData {
        view: View::Login,
        accounts: AccountStore::load(),
        instances: InstanceStore::load(),
        auth: auth::AuthState::Resolving,
    }));

    let widgets = Widgets {
        window: window.clone(),
        stack: stack.clone(),
        sound: audio::Player::new(),
        haptics: None,
    };

    // ── Gamepad polling ──
    // One shared Gilrs instance, polled every 16ms via
    // glib::source::timeout_add_local, same cadence as gamepad-2048.
    // Button identity is resolved exclusively through
    // input::classify_button (see input.rs for why).
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
                    match input::classify_button(button) {
                        Some(GamepadAction::Confirm) => handle_gp_confirm(&state_for_gp, &widgets_for_gp),
                        Some(GamepadAction::Back) => handle_gp_back(&state_for_gp, &widgets_for_gp),
                        // TODO: OpenAccounts / RefreshOrSettings / SystemMenu
                        // dispatch, and D-Pad/left-stick direction handling
                        // (axis thresholding + edge-triggered repeat, same
                        // as gamepad-2048's main.rs), once real per-screen
                        // widget trees exist to navigate between.
                        _ => {}
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
