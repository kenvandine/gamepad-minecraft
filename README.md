# gamepad-minecraft

A controller-first Minecraft Java Edition launcher, written in Rust and
GTK4, for Ubuntu handheld gaming devices (Lenovo Legion Go S, MSI Claw).

Its look is modelled on the official Minecraft Launcher - a Play tab
with big key art and a chunky green PLAY button, an Installations tab,
and an account switcher - with original artwork drawn entirely in code
(see [Artwork](#artwork)). No keyboard or mouse is ever required:
everything works with a game controller or a touchscreen. Sign in with a
Microsoft account via a QR-code device-code flow, then play fully offline
afterward using a cached profile.

Minecraft Java Edition itself is not distributed in this snap - the first
launch requires network access to download it directly from Mojang, the
same as the official launcher.

## Features (see [PLAN.md](PLAN.md) for the full architecture)

- QR-code Microsoft sign-in (RFC 8628 device-code flow), no browser or
  keyboard needed on the handheld itself
- Play fully offline afterward using a cached profile - network is only
  required for the first sign-in and for downloading new content
- Install and switch between multiple Minecraft versions side by side,
  from the version dropdown next to PLAY or the Installations tab
- Switch between several Microsoft (or offline) accounts from the
  account dropdown in the top-left corner
- New instances get Fabric Loader and the Controlify mod injected
  automatically, so Minecraft's own title screen is gamepad-navigable too
- Full game-controller support throughout the launcher: D-pad to
  navigate, A to confirm, B to go back, LB/RB to switch tabs, X for the
  account switcher, Y for the version dropdown
- Every control is also a large touch target, so a touchscreen alone is
  enough

## Development setup (Ubuntu 26.04)

These steps take a fresh Ubuntu 26.04 install to a working build, test
run, and running app. They assume a normal desktop session (Wayland or
X11) - the launcher is a GTK4 GUI and needs a display to run.

### 1. Install system packages

```sh
sudo apt update
sudo apt install -y \
    build-essential pkg-config cmake git curl ca-certificates \
    libgtk-4-dev libudev-dev \
    openjdk-25-jre
```

What each is for:

| Package(s) | Why it's needed |
| --- | --- |
| `build-essential` | C compiler and linker used by Rust and by `-sys` crates |
| `pkg-config` | How the `gtk4-sys`/`libudev-sys` build scripts locate system libraries |
| `cmake` | Builds `aws-lc-sys`, the crypto backend of `reqwest`'s TLS stack |
| `libgtk-4-dev` | GTK4 headers and libraries (also pulls in GLib, Cairo, Pango, GdkPixbuf, Graphene). The crate requires GTK 4.8 or newer, which 26.04 satisfies |
| `libudev-dev` | Gamepad enumeration for `gilrs` (the controller input library) |
| `openjdk-25-jre` | Runs Minecraft itself. Only needed at runtime; the launcher falls back to `java` on `$PATH` when not running inside the snap |
| `git`, `curl`, `ca-certificates` | Cloning the repo and fetching the Rust toolchain |

### 2. Install Rust

Install a current stable toolchain with [rustup](https://rustup.rs/)
(the dependency tree uses recent crates - `gtk4` 0.11, `reqwest` 0.13 - so
a recent compiler is required, and rustup is the simplest way to be sure
of one):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
rustc --version   # confirm it works
```

Ubuntu's own `cargo`/`rustc` packages also work if they are recent
enough to build the project; if the build fails with a "requires rustc
1.xx or newer" error, use rustup instead.

### 3. Get the source

```sh
git clone https://github.com/kenvandine/gamepad-minecraft.git
cd gamepad-minecraft
```

### 4. Build

```sh
cargo build            # debug build: faster to compile, easier to debug
cargo build --release  # optimized build, output in target/release/
```

The first build downloads and compiles all dependencies, so expect it to
take several minutes.

### 5. Run the tests

```sh
cargo test
```

Tests live next to the code they cover (`#[cfg(test)]` modules in
`src/`) and don't need a display, a gamepad, a Microsoft account, or
network access.

For a lint/format pass before sending changes:

```sh
rustup component add clippy rustfmt   # once
cargo fmt --check
cargo clippy --all-targets
```

### 6. Run the app

```sh
cargo run --release
# or, after a build:
./target/release/gamepad-minecraft
```

Things to know when running from source:

- **Display**: run it from a graphical session. Over SSH you need
  `WAYLAND_DISPLAY` or `DISPLAY` pointing at a running session.
- **Controller**: plug in or pair a gamepad before launching. On Ubuntu
  the desktop session grants access to input devices automatically (via
  udev `uaccess`), so no group changes are normally needed. A keyboard
  also works as a fallback for development.
- **Java**: outside the snap, the launcher runs whatever `java` is first
  on your `$PATH` (installed in step 1). Current Minecraft releases need a
  recent JDK, which is why `openjdk-25-jre` is used rather than 21.
- **Network**: the first sign-in and the first Minecraft download need
  internet access. The default Microsoft sign-in app ID is built in; to
  use your own, see [Setting up Microsoft sign-in](#setting-up-microsoft-sign-in).
- **Data location**: instances, downloaded game files and the cached
  account profile are stored under
  `${XDG_DATA_HOME:-~/.local/share}/gamepad-minecraft/` (or
  `$SNAP_USER_COMMON/gamepad-minecraft` inside the snap). Delete that
  directory to reset to a clean first-run state, or set `XDG_DATA_HOME`
  to a scratch directory to keep a dev environment separate from your
  real data:

  ```sh
  XDG_DATA_HOME=/tmp/gpmc-dev cargo run
  ```

### Examples

Two small programs under `examples/` exercise parts of the stack without
the GTK UI:

```sh
# Mojang version manifest: fetch and parse, no large downloads
cargo run --example manifest_check

# Full Microsoft -> Xbox -> Minecraft sign-in chain, from the terminal
GAMEPAD_MINECRAFT_CLIENT_ID=<your-app-id> cargo run --example auth_cli
```

### Project layout

| Path | Contents |
| --- | --- |
| `src/main.rs`, `src/style.css` | GTK4 UI, application wiring and theme |
| `src/pixelart.rs` | Blocky pixel font (PLAY button) and account avatars |
| `src/auth.rs`, `src/account.rs` | Device-code sign-in chain and cached profile |
| `src/instance.rs`, `src/fabric.rs`, `src/launch.rs` | Installing, patching (Fabric/Controlify) and launching Minecraft |
| `src/input.rs`, `src/haptics.rs`, `src/audio.rs` | Gamepad input, rumble and sound |
| `data/` | Desktop entry, icons, and the generated artwork in `data/art/` |
| `examples/gen_art.rs` | Generator for the artwork in `data/art/` |
| `snap/snapcraft.yaml` | Snap packaging |
| `PLAN.md` | Architecture and design notes |

### Artwork

The Play tab's key art (an original Nether wastes scene), the
"MINECRAFT / JAVA EDITION" wordmark and the grass-block icon are not
Mojang assets: they're rendered by `examples/gen_art.rs`, a small voxel
ray tracer with procedural block textures, and committed as PNGs under
`data/art/` that the launcher embeds with `include_bytes!`. The player
in the key art wears `data/art/hero-skin.png`, a standard 64x64 skin
with slim arms; swap in another to change the character. To change
them, edit the generator and re-run it:

```sh
cargo run --release --example gen_art            # writes data/art/*.png
cargo run --release --example gen_art -- preview # quick low-res hero in target/
cargo run --release --example gen_art -- closeup # close-up of the player in target/
```

## Setting up Microsoft sign-in

The device-code sign-in flow needs an Azure AD application ID (a
`client_id`, not a secret - see below). To register one:

1. [Azure Portal](https://portal.azure.com) → App registrations → New
   registration.
2. Supported account types: **Personal Microsoft accounts only** (Xbox
   Live/Minecraft sign-in only works with personal accounts, and this app
   talks to the `/consumers/` tenant endpoint specifically).
3. Authentication → Add a platform → **Mobile and desktop applications**,
   then set **"Allow public client flows" to Yes**.
4. Leave *Certificates & secrets* empty. This is a public client - it's
   shipped as a binary/snap to end users, so it can't keep a secret safe,
   and the device-code/token endpoints don't require one once step 3 is
   done. If the portal auto-generates a secret anyway, ignore or delete
   it; never embed one here.

Then export the resulting application (client) ID and try the auth chain
standalone, with no GTK/UI involved yet:

```sh
GAMEPAD_MINECRAFT_CLIENT_ID=<your-app-id> cargo run --example auth_cli
```

## Building the snap

Snap builds are optional for development. Install the tooling once
(snapcraft builds inside an LXD container, which keeps the host clean):

```sh
sudo snap install snapcraft --classic
sudo snap install lxd
sudo lxd init --auto
sudo usermod -aG lxd "$USER"   # then log out and back in
```

Then, from the repository root:

```sh
snapcraft
sudo snap install --dangerous ./gamepad-minecraft_0.1.0_amd64.snap
sudo snap connect gamepad-minecraft:joystick
```

The snap is strictly confined and uses the GNOME extension for desktop
integration, plus the `joystick` interface for game-controller input and
a staged OpenJDK runtime (currently 25, tracking the OpenJDK LTS current
Minecraft releases expect) to launch Minecraft itself.

## License

Copyright (C) 2026 Ken VanDine

Licensed under the GNU General Public License v3.0 or later. See
[LICENSE](LICENSE) for the full text.
