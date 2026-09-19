# gamepad-minecraft

A controller-first Minecraft Java Edition launcher for Ubuntu handheld
gaming devices (Lenovo Legion Go S, MSI Claw). Sign in once with a
Microsoft account via a QR-code device-code flow, then play fully offline
afterward using a cached profile. No keyboard or mouse is ever required.

This mirrors the architecture of its sibling
[gamepad-2048](https://github.com/kenvandine/gamepad-2048): a single Rust
binary, GTK4 for UI, `gilrs` for controller input, and the Ubuntu "Resolute
Raccoon" theme (aubergine backgrounds, orange accents) inherited from
[gamepad-shell](https://github.com/kenvandine/gamepad-shell). Where the
old draft of this document proposed Godot/SDL2/C++/Go, that choice is
rejected in favor of Rust/GTK4, for consistency with the rest of the
gamepad-* family and because there is no shared crate between siblings —
each app hand-rolls its own gamepad/theme code, so staying in the same
language keeps that duplication cheap to review and port.

Minecraft Java Edition itself is never bundled or distributed by this
snap — Mojang's EULA does not permit redistributing the client, so the
first launch requires network access to fetch it directly from Mojang's
own version manifest and CDN, the same as the official launcher does.

## 1. Module layout

Flat `src/`, one binary crate, no workspace — same shape as gamepad-2048:

| Module | Responsibility |
|---|---|
| `main.rs` | Window/CSS setup, `gtk::Stack` pages, `View` enum, `AppData`/`Widgets`, keyboard input, the `gilrs` poll loop, `handle_gp_navigate/confirm/back` dispatch. |
| `input.rs` | The **only** gamepad button→action mapping table (`classify_button`). gamepad-2048 shipped a second, unused copy of this in its own `gamepad.rs` while `main.rs` reimplemented the mapping inline — `input.rs` is designed so that mistake can't happen here: nothing matches on `gilrs::Button` except this one function. |
| `net.rs` | Background-thread + `async-channel` bridge to the GTK main loop (`spawn_blocking`), plus thin `reqwest::blocking` helpers (`get_json`, `post_form`, `download_to_file`). No GTK dependency. |
| `auth.rs` | RFC 8628 device-code flow, MSA → XBL → XSTS → Mojang token exchange, the `AuthState`/`AuthEvent` state machine (online login vs. cached offline play — see §3). HTTP calls delegate to `net.rs`; the state machine itself is unit-testable in isolation, same as gamepad-2048's `game.rs`. |
| `account.rs` | Cached-account JSON store (`$SNAP_USER_DATA/accounts.json`), structured identically to gamepad-2048's `scores.rs`: silent empty-default on a missing/corrupt file, never panics. |
| `qr.rs` | Renders the device-code verification URI into an RGBA pixel buffer using the `qrcode` crate directly — no external `qrencode` binary, no `image` crate. `main.rs` wraps the buffer in a `gdk::MemoryTexture`. |
| `instance.rs` | Mojang version-manifest fetch, the installed-instance list (multiple versions side by side, switchable — see §5), download/verify orchestration. Persisted the same way as `account.rs`/`scores.rs`. |
| `fabric.rs` | Fabric Loader install and Controlify mod auto-injection into a new instance's `mods/`, so Minecraft's own title screen is gamepad-navigable too. |
| `launch.rs` | Classpath/JVM-arg construction, native-library extraction, spawning the bundled `java` binary, child-process monitoring. |
| `audio.rs` | Synthesized WAV chimes (no asset files), played via `gtk::MediaFile::for_filename` — ported structurally from gamepad-2048's `audio.rs`, including its file-based-`MediaFile`-only constraint (the on-device GStreamer build aborts on the stream variant). |
| `haptics.rs` | `gilrs::ff` rumble, gated behind `const ENABLED: bool = false` until there's a settings UI to make it optional — ported structurally from gamepad-2048's `haptics.rs`. |

## 2. Networking without an async runtime

Network calls (auth polling, downloads) use `reqwest::blocking` on plain
`std::thread`s, with results crossing to the GTK main loop over an
`async-channel` consumed by `glib::spawn_future_local`. This keeps the app
in the same mental model as gamepad-2048's `glib::source::timeout_add_local`
polling loop — "a thread does blocking work, then pushes a plain value the
main loop picks up" — without pulling in a tokio runtime anywhere in our
own code. `reqwest::blocking::Client` uses a private tokio instance
internally, but nothing outside `net.rs` ever writes `async`/`.await`.

Auth polling and downloads each run on their own background thread rather
than a `glib::timeout_add` that blocks on HTTP — a timer callback that
blocks the main loop for a network round trip would freeze the whole UI,
including gamepad input. The auth-poll thread checks an `Arc<AtomicBool>`
cancel flag each iteration so pressing B actually stops polling Microsoft's
token endpoint instead of orphaning the thread. Download progress is
throttled (e.g. every 100 ms) before crossing the channel, so a large asset
set doesn't flood the main loop with widget updates.

## 3. Online login vs. offline play

Network access is **required** for the first sign-in — the device-code
flow and the MSA→XBL→XSTS→Mojang exchange cannot work offline, and there is
nothing useful to cache before it succeeds once. After that first success,
the launcher caches the profile (`username`, `uuid`, `refresh_token`) in
`account.rs` and can **play fully offline** afterward, the same way the
official Minecraft Launcher's "Play Offline" works: launch proceeds using
the cached identity with no live Mojang session required.

```rust
enum AuthState {
    Resolving,                                   // startup, before cache is read
    NeedsLogin { cached: Option<CachedAccount> }, // cached = Some enables offline play
    AwaitingUser { qr_data, user_code, verification_uri, expires_at },
    LoggedIn { profile: McProfile },              // fresh/refreshed online session
    OfflinePlaying { profile: CachedAccount },     // not validating online, by choice or necessity
    Error { message: String, cached: Option<CachedAccount> },
}
```

Silent token refresh is attempted opportunistically (once after `Home`
first renders from cache, and again before an online launch if the cached
token is stale) but **never blocks** — `Home` renders immediately from the
cached profile either way, and a failed refresh just falls back to offline
identity rather than surfacing an error. Refresh is **never** attempted for
the very first login (there's no cache yet) and is **skipped outright**
when the user explicitly picks "Play Offline."

"Play Offline" is a normal, focusable `gtk::Button` in the same widget tree
and focus chain as "Sign in with Microsoft" — reachable by D-Pad/stick like
everything else, and hidden entirely (not just disabled) when no account
has ever been cached. It also stays visible during the QR/polling screen,
so a controller-only user without their phone handy isn't stuck waiting on
a scan that may never happen.

## 4. Theme

Reused exactly from gamepad-shell / gamepad-2048's "Resolute Raccoon"
palette — hand-copied as CSS-in-Rust in `main.rs`, since no shared theme
crate exists across the gamepad-* family:

```css
@define-color orange         #E95420;  /* primary accent, focus highlight */
@define-color orange_bright  #F4703C;  /* hover/focus border, headings */
@define-color aubergine      #2C001E;  /* deep background */
@define-color purple         #77216F;  /* supporting accent */
```

Backgrounds: near-black aubergine `#150610`; card/button surfaces
`#3A1A28` (default) / `#4A2550` (hover); focused/active widgets get an
orange fill, an `orange_bright` border, and a soft glow
(`box-shadow: 0 0 0 3px alpha(orange, 0.35)`). Text is `#FFFFFF`, with
`alpha(#FFFFFF, 0.40–0.62)` for secondary/hint copy. Font: `"Ubuntu Sans",
"Ubuntu", sans-serif`. The window launches fullscreen
(`window.fullscreen()` after `present()`) since this targets a handheld
screen, not a resizable desktop window.

## 5. Screens and gamepad UX

`gtk::Stack` pages, mirroring gamepad-2048's Menu/Game/Scores pattern:

| Page | Default focus | A / Confirm | B / Back |
|---|---|---|---|
| `Login` | "Sign in" (no cache) or "Play Offline" (cache exists) | Activates focused widget | Cancels device-code polling if active, else quits |
| `Home` | First instance tile, or "Add Instance" if none exist | Launch focused instance | Quit-confirm overlay |
| `InstanceDetail` | "Play" | Play / Update / Delete per focused button | Back to `Home`, refocusing the tile just left |
| `Accounts` (X) | Active account row | Switch active account | Back to `Home` |
| `Settings` (Y) | First setting control | Toggle/activate | Back to `Home` |
| `Launching` (overlay over `Home`) | "Cancel" | n/a during progress | Cancels the job, hides the overlay |

`Home`'s instance grid supports **multiple installed Minecraft versions
side by side** — each tile is a separately downloaded, separately updatable
instance (`instance.rs`), and `InstanceDetail` is where a new version gets
added by browsing Mojang's version manifest online. Switching versions is
just navigating to a different tile; nothing is a single hardcoded version.

Face-button scheme (both Legion Go S and MSI Claw are Xbox-style ABXY
pads, so South = A = confirm and East = B = back is the correct convention
here — documented as a deliberate choice in `input.rs`, not an oversight):

- **A / South:** Select / Confirm / Launch
- **B / East:** Back / Cancel
- **X / West:** Accounts — switch signed-in user
- **Y / North:** Refresh instances / Settings
- **Start:** System / power overlay

gamepad-2048's `docs/UX_REVIEW.md` found two bug classes after its initial
gamepad wiring, both designed in from the start here rather than discovered
later:

1. **Confirm must activate whatever GTK reports as focused**
   (`window.focus()` + `Widget::activate()`), never a per-screen hardcoded
   action based on app state — the old bug was "navigate to option B, press
   A, get option A anyway." This matters more here than in a puzzle game:
   a stale-focus bug could launch the wrong instance or switch the wrong
   account.
2. **Every screen transition explicitly seeds focus** via a
   `Widgets::focus_default_for(View)` call, including sub-states inside
   `Login` (`NeedsLogin` / `AwaitingUser` / `Error`) — a revealer swap
   without a refocus call was the single biggest bug class in gamepad-2048.
3. All "back" gestures (gamepad B, keyboard Escape) funnel through one
   `handle_gp_back()` so the `View` enum and the visible `Stack` page can
   never desync.
4. Custom horizontal button rows (Login's two buttons, InstanceDetail's
   Play/Update/Delete row, the instance grid) use manual index-stepping for
   Left/Right rather than trusting GTK's geometric `child_focus()` search,
   which skipped neighbors in gamepad-2048's own board-size row.
5. Every screen carries an on-screen button-hint legend (e.g. "A: Sign In
   • B: Play Offline / Quit") — there's no keyboard in view, so nothing is
   gated on the player already knowing a control exists.

A dedicated UX-hardening pass happens after gamepad wiring lands (see the
roadmap below), re-auditing every screen against this list and producing
its own `docs/UX_REVIEW.md`, the same artifact gamepad-2048 produced.

## 6. Account storage

`account.rs` writes `$SNAP_USER_DATA/accounts.json` with `0600`
permissions. Deliberately **not** encrypted at rest in v1: the cached token
is scoped to `XboxLive.signin offline_access` (not a full account
credential) and is remotely revocable from the user's Microsoft account;
real at-rest encryption would need a secret store
(`org.freedesktop.secrets` over D-Bus), which strict-confinement snaps
don't get cleanly, and gamepad-2048's own precedent
(`PULSE_AUTOSPAWN=0`, avoiding D-Bus session activation) argues against
adding a D-Bus dependency for this. The real security boundary is the snap
sandbox itself — `$SNAP_USER_DATA` isn't readable by other confined snaps —
the same trust model the official launcher relies on. An `oo7`/libsecret
store is a reasonable fast-follow, not a v1 blocker.

## 7. Snap packaging

`base: core24`, `confinement: strict`, `extensions: [gnome]`, `plugin:
rust` — same as gamepad-2048, with the same documented
`LD_LIBRARY_PATH: ""` build-environment workaround (the gnome extension
SDK's libc, prepended for the build step, breaks the rustup/cargo
toolchain). New plugs beyond gamepad-2048's set, and why:

- **`network`** — device-code login, the XBL/XSTS/Mojang exchange, and all
  Minecraft/Fabric downloads. gamepad-2048 has no network plug at all
  (it's fully offline); this app cannot avoid it for first login.
- **`network-bind`** — reserved for a possible future LAN-play feature, not
  wired up in v1.
- **`process-control`** — spawn and monitor the bundled `java` child
  process.
- **`removable-media`** — reserved for storing instances on a MicroSD card,
  not wired up in v1.

OpenJDK 21 is staged inside the snap (`openjdk-21-jre-headless`) because
strict confinement forbids exec'ing arbitrary host binaries — `launch.rs`
invokes `$SNAP/usr/lib/jvm/java-21-openjdk-amd64/bin/java` directly, the
same pattern the original draft of this plan already got right.

The `joystick` interface doesn't auto-connect on the Snap Store by
default — filing an auto-connect request on `forum.snapcraft.io` is a
follow-up step once this is published, same as for gamepad-2048.

## 8. Roadmap

Mirrors gamepad-2048's actual build order (pure logic → GTK shell →
gamepad wiring → dedicated UX-hardening pass → gated audio/haptics → snap
packaging), extended for this project's auth/instance/launch scope:

1. **Core auth logic**, no GTK: `net.rs`, `auth.rs`, `account.rs`, `qr.rs`.
   Verify the full device-code → MSA → XBL → XSTS → Mojang chain against
   the real endpoints before any UI exists.
2. **Core instance/launch logic**, still no GTK: `instance.rs`,
   `fabric.rs`, `launch.rs`. Install one vanilla instance and confirm
   `java` actually boots Minecraft headlessly.
3. **GTK shell**: `main.rs` with all `Stack` pages and the theme, wired to
   keyboard/mouse only — no `gilrs` yet.
4. **Gamepad wiring**: `input.rs` + the `gilrs` poll loop + `handle_gp_*`
   dispatch.
5. **Dedicated UX-hardening pass**: re-audit every screen against §5
   before calling gamepad support done; write `docs/UX_REVIEW.md`.
6. **Audio + haptics, gated**: port structurally from gamepad-2048,
   `haptics::ENABLED = false` initially.
7. **Snap packaging**: verify the staged JRE path, the
   `LD_LIBRARY_PATH` workaround, and that `process-control` permits
   spawning/monitoring `java` under strict confinement. Hardware-validate
   on both Legion Go S and MSI Claw.
8. **Store submission**: publish to edge/candidate, file the `joystick`
   auto-connect forum request, and add an entry to the sibling
   `gamepad-game-store` catalog so gamepad-shell's in-shell "Ubuntu Store"
   can surface it.

### Sources used

- gamepad-2048 (`src/`, `Cargo.toml`, `snap/snapcraft.yaml`,
  `docs/UX_REVIEW.md`) — the reference architecture mirrored throughout.
- gamepad-shell (`shell/style.css`) — the "Resolute Raccoon" theme source.
- RFC 8628: OAuth 2.0 Device Authorization Grant.
- Mojang Services & Xbox Live authentication protocol specifications.
- Canonical Snapcraft interface documentation (joystick, process-control,
  opengl).
- Controlify mod specification (Fabric gamepad-native subsystem).
