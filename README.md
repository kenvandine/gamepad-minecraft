# gamepad-minecraft

A controller-first Minecraft Java Edition launcher, written in Rust and
GTK4, for Ubuntu handheld gaming devices (Lenovo Legion Go S, MSI Claw).

gamepad-minecraft borrows its colour palette and visual language from
[gamepad-shell](https://github.com/kenvandine/gamepad-shell) (the Ubuntu
"Resolute Raccoon" theme: aubergine backgrounds, orange accents), the
same way its sibling [gamepad-2048](https://github.com/kenvandine/gamepad-2048)
does. No keyboard or mouse is ever required: sign in with a Microsoft
account via a QR-code device-code flow, then play fully offline afterward
using a cached profile.

Minecraft Java Edition itself is not distributed in this snap - the first
launch requires network access to download it directly from Mojang, the
same as the official launcher.

## Features (see [PLAN.md](PLAN.md) for the full architecture)

- QR-code Microsoft sign-in (RFC 8628 device-code flow), no browser or
  keyboard needed on the handheld itself
- Play fully offline afterward using a cached profile - network is only
  required for the first sign-in and for downloading new content
- Install and switch between multiple Minecraft versions side by side
- New instances get Fabric Loader and the Controlify mod injected
  automatically, so Minecraft's own title screen is gamepad-navigable too
- Full game-controller support throughout the launcher: D-pad/stick to
  navigate, A to confirm, B to go back, X for accounts, Y for
  refresh/settings

## Building

```sh
cargo build --release
./target/release/gamepad-minecraft
```

Requires GTK4 development libraries (`libgtk-4-dev` on Debian/Ubuntu).

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

```sh
snapcraft
sudo snap install --dangerous ./gamepad-minecraft_0.1.0_amd64.snap
sudo snap connect gamepad-minecraft:joystick
```

The snap is strictly confined and uses the GNOME extension for desktop
integration, plus the `joystick` interface for game-controller input and
a staged OpenJDK 21 runtime to launch Minecraft itself.

## License

Copyright (C) 2026 Ken VanDine

Licensed under the GNU General Public License v3.0 or later. See
[LICENSE](LICENSE) for the full text.
