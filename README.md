Language: **English** | [Русский](README.ru.md)

# OpenSwitcher

OpenSwitcher is an EN/RU-focused Linux desktop typing utility written in Rust.

The project focuses on day-to-day EN/RU typing workflows:
- automatic correction of the last word when it was typed in the wrong layout
- manual correction of the current or previous word
- selected-text layout conversion
- lightweight tray control and a separate settings window

> **Development note**
>
> OpenSwitcher is developed through AI-assisted engineering. The Rust implementation in this repository is produced by AI tools under human direction, review, and acceptance.
>
> The project owner is not a Rust developer and focuses on product requirements, architecture decisions, testing, UX, and final validation rather than manual Rust implementation.

This repository contains the full development history of the project.

## Project Status

OpenSwitcher is in active development.

The first release target is a Linux desktop application for EN/RU typing workflows, built around a `daemon + tray` runtime model.

Current release baseline:
- confirmed environments are listed in [Tested Environments](#tested-environments)
- current confirmed Wayland target: GNOME Wayland
- other Wayland desktops/compositors are best-effort until tested and backed by diagnostics/backend support
- focused on EN/RU typing only
- official runtime and autostart model: `systemd --user`

## Tested Environments

| Environment | Session | Status | Verification |
| --- | --- | --- | --- |
| Linux Mint 22.2 Cinnamon | X11 | Supported baseline | Confirmed by local smoke testing |
| Ubuntu 24.04 LTS GNOME 46 | Wayland | Supported Wayland target | Confirmed by local Wayland smoke testing |

Environments not listed here are best-effort. The current confirmed Wayland target is GNOME
Wayland; other Wayland desktops/compositors are diagnostics-first until tested and backed by
desktop-specific layout detection/observation support.

## Installation

For normal use, download the Debian package from the GitHub Release and install it locally:

```bash
sudo apt install ./open-switcher_0.1.0-1_amd64.deb
```

Then start OpenSwitcher from the desktop application menu. The settings window controls whether
OpenSwitcher starts automatically after login/reboot; launching it manually from the application
menu always starts it even when autostart is disabled.

The package installs the required application files, user systemd units, desktop launchers, icon,
and Linux input udev rule. A normal package install should not require manually running
`./manage.sh bootstrap linux-input`, enabling user units, or editing `/dev/input` permissions.

## Development Quick Start

```bash
./manage.sh dev build
./manage.sh doctor
./manage.sh bootstrap linux-input
./manage.sh dev start
./manage.sh dev settings
```

## Components

OpenSwitcher is split into three binaries:

- `open-switcher`
  The daemon binary. It owns configuration, input handling, correction logic, and the D-Bus API.
- `open-switcher-tray`
  The tray binary and the main user-facing entrypoint. It provides the tray icon, status menu, and talks to the daemon over D-Bus.
- `open-switcher-settings`
  A GTK4 + libadwaita settings tool. It is separate from the mandatory `daemon + tray` pair.

## Runtime Model

For users, OpenSwitcher is one application built from two cooperating processes:

- `daemon`
- `tray`

Those two are expected to run together as one user-facing application.

Current runtime model:
- the official user-facing startup path is the tray
- the official autostart path is `systemd --user`
- `daemon + tray` are treated as one application lifecycle
- the settings window is optional and can be started separately

## Current Features

- Auto-switch the last word on word commit when it looks like wrong-layout EN/RU input
- Manual correction hotkey for the current or previous word
- Selected-text layout conversion
- Case correction options:
  - fix two uppercase letters at the beginning of a word
  - fix accidental Caps Lock pattern
- Settings UI for system, correction, and hotkey options
- User-level `systemd` integration for the `daemon + tray` application pair
- Tray menu with status and control actions

## Hotkey Settings

Manual correction and selected-text conversion use the same curated hotkey model.
The layout-switch shortcut remains a separate setting because it must match the desktop/session
layout-switch behavior.

Allowed trigger keys:
- `F9`
- `F10`
- `F12`
- `Pause`
- `ScrollLock`
- `Insert`
- `Menu`

Each trigger may be used with no modifiers or with any combination of `Shift`, `Ctrl`, and `Alt`.
Examples: `F12`, `Shift+F12`, `Ctrl+Alt+F12`, `Ctrl+Alt+Shift+Insert`.

OpenSwitcher intentionally does not accept arbitrary typing keys for these actions. Letters,
digits, `Space`, `Enter`, `Tab`, `Backspace`, `Delete`, `Escape`, arrow keys, `F1`-`F8`, `F11`,
and `PrintScreen` are not accepted as manual/selected-text hotkey triggers.

The exact same hotkey cannot be assigned to manual correction and selected-text conversion at
the same time. The same trigger with different modifiers is allowed, so `F12` and `Shift+F12`
can coexist. If a hotkey contains the current layout-switch shortcut as a prefix, settings shows
a warning but allows saving.

## Current Scope And Limitations

- Linux only
- Confirmed environments are listed in [Tested Environments](#tested-environments)
- Current confirmed Wayland target: GNOME Wayland
- Linux desktop environments and Wayland compositors not listed in Tested Environments are best-effort until tested and backed by diagnostics/backend support
- Main supported typing scenario is EN/RU
- Layout/backend support is still conservative and backend-driven
- The current backend layer is designed for expansion, but support is not yet broad across all desktop environments
- Application-specific exclusions are not included in the first release
- A GNOME Shell extension is not included
- Tray support depends on the desktop environment providing a compatible StatusNotifier/AppIndicator host
- A missing tray icon does not by itself prove that the daemon, D-Bus, or settings are broken
- A visible, working tray is still required for any environment considered fully supported
- The official runtime and autostart model depends on `systemd --user`
- The settings UI is built behind the `settings-ui` Cargo feature

## Requirements

### Runtime environment

- Linux desktop session
- session D-Bus
- `systemd --user` for the official autostart/runtime model
- a desktop environment with a compatible StatusNotifier/AppIndicator tray host

## Linux Input Setup

OpenSwitcher reads real input devices from `/dev/input/event*` and writes virtual key events through `/dev/uinput`.

When OpenSwitcher is installed from the `.deb` package, the package installs the udev rule needed
for normal runtime access. The commands below are mainly for source-tree development and
troubleshooting.

Check whether the current development session is ready:

```bash
./manage.sh doctor
```

If the doctor reports denied access while running from the source tree, run the setup helper:

```bash
./manage.sh bootstrap linux-input
```

What the bootstrap does:
- installs the project udev rule from `dist/udev/80-openswitcher-input.rules`
- reloads udev rules when `udevadm` is available
- applies a same-session ACL bridge for the current user when `setfacl` is available
- reruns `./manage.sh doctor` to confirm the result

Notes:
- `./manage.sh bootstrap linux-input` works without `setfacl`, but the same-session ACL bridge is only applied when `setfacl` is available. On Debian/Ubuntu-like systems that command is typically provided by the `acl` package.
- runtime layout auto-detect may use desktop-specific tools such as `gsettings`, `xfconf-query`, or `setxkbmap` depending on the current environment.

## Layout Switch Detection

OpenSwitcher tries to detect the user's layout-switch shortcut from the current desktop/session context.

Current behavior:
- Cinnamon X11 reads Cinnamon keyboard settings first
- if Cinnamon settings are empty or unusable, Cinnamon X11 falls back to `setxkbmap -query`
- Xfce X11 and GNOME Wayland have separate detection paths
- unsupported or unknown desktop environments may require a manual layout-switch override in settings

The detected setting is stored in the daemon config. Manual choices made in settings are preserved and are not overwritten by auto-detection.

## Selected-text Conversion

Selected-text conversion uses a clipboard-based flow:
- OpenSwitcher sends a copy shortcut to read the current selection
- converts the copied text between EN/RU physical layouts
- temporarily replaces the clipboard with the converted text
- sends a paste shortcut
- then attempts to restore the previous clipboard contents

Hotkey capture can depend on the physical keyboard and desktop environment. On some laptops,
function keys, `Pause`, `ScrollLock`, `Insert`, or `Menu` may be affected by Fn-key handling,
firmware behavior, or global desktop shortcuts.

Selected-text debug logging is opt-in. When enabled, selected-text debug summaries contain metadata only, such as length and line count, not text previews.

### Build dependencies

For Linux Mint / Ubuntu-like systems:

```bash
sudo apt-get update
sudo apt-get install -y \
  debhelper \
  dpkg-dev \
  build-essential \
  pkg-config \
  libdbus-1-dev \
  libudev-dev \
  libgtk-4-dev \
  libadwaita-1-dev \
  desktop-file-utils
```

Optional helper packages on Debian/Ubuntu-like systems:
- `acl` for `setfacl` during `./manage.sh bootstrap linux-input`
- `libglib2.0-bin` for `gdbus` in the D-Bus examples below
- `lintian` for optional local Debian package checks

## Building

Check all binaries:

```bash
cargo check --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Build everything locally:

```bash
cargo build --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Run regular checks:

```bash
cargo test -q --lib
cargo test -q --features settings-ui --lib
cargo test --test dbus_api -q
cargo check -q --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
git diff --check
```

Optional broader check:

```bash
cargo test -q --all-targets --features settings-ui
```

CI also runs the `settings-ui` feature tests so feature-gated coverage is not skipped accidentally.

### Building a `.deb` from source

OpenSwitcher packages are built with the project Rust toolchain from `rust-toolchain.toml` through
`rustup`. The `cargo`/`rustc` packages from apt may be too old and are not the source of truth for
this GitHub Release package build path.

Install the non-Rust Debian build dependencies listed above, then build the package:

```bash
./manage.sh package deb
```

The command checks `rustup`, verifies that the required toolchain is available, runs
`dpkg-buildpackage -us -uc -b -d -tc`, validates the desktop files, and copies package artifacts to:

```text
dist/packages/
```

Install the locally built package with:

```bash
sudo apt install ./dist/packages/open-switcher_*_amd64.deb
```

The optional `open-switcher-dbgsym_*.ddeb` file contains debug symbols. It is useful for debugging
but is not needed for normal installation.

## Development Workflow

The repository ships with `manage.sh`, which supports two explicit modes:

- `dev`
  Direct local binaries from `target/`
- `systemd`
  Real user-service runtime through `systemctl --user`

Old top-level commands are kept as aliases for `dev`, but the preferred form is the explicit namespace.

### `dev` mode

Build:

```bash
./manage.sh dev build
```

Start the local `daemon + tray` pair from the build tree:

```bash
./manage.sh dev start
```

Useful helpers:

```bash
./manage.sh dev status
./manage.sh dev logs
./manage.sh dev settings
./manage.sh dev stop
```

Use a release profile if needed:

```bash
OPEN_SWITCHER_PROFILE=release ./manage.sh dev build
OPEN_SWITCHER_PROFILE=release ./manage.sh dev start
```

## `systemd --user` Runtime

The published package uses `systemd --user` internally for the `daemon + tray` runtime model.
Normal users should install the `.deb`, start OpenSwitcher from the desktop application menu, and
use the settings UI to control autostart.

The commands below are mainly for local source-tree development and manual debugging.

Install user units, desktop entry, and locally installed binaries:

```bash
./manage.sh systemd install
```

Start the `daemon + tray` application through user services:

```bash
./manage.sh systemd start
```

Inspect runtime state:

```bash
./manage.sh systemd status
./manage.sh systemd logs
```

Stop the current session:

```bash
./manage.sh systemd stop
```

Enable or disable autostart for future sessions:

```bash
./manage.sh systemd enable
./manage.sh systemd disable
```

### What gets installed

Distribution assets in the repository:

- `dist/systemd/open-switcher-daemon.service`
- `dist/systemd/open-switcher-tray.service`
- `dist/open-switcher.desktop`
- `dist/icons/hicolor/512x512/apps/open-switcher.png`

Installed by `./manage.sh systemd install` into:

- `~/.config/systemd/user/`
- `~/.config/autostart/`
- `~/.local/share/applications/`
- `~/.local/share/icons/hicolor/512x512/apps/`
- `~/.local/bin/`

Notes:
- the desktop entry starts the tray service through `systemctl --user`
- the tray unit pulls in the daemon unit
- the XDG autostart fallback at `~/.config/autostart/open-switcher.desktop` also starts the tray systemd service, not the tray binary directly, for desktop-session login reliability

## Direct Binary Runs

This section is mainly for development and manual local debugging.

If you want to run the binaries manually from the build tree:

Daemon:

```bash
./target/debug/open-switcher
```

Tray:

```bash
./target/debug/open-switcher-tray
```

Settings:

```bash
./target/debug/open-switcher-settings
```

## Configuration

Configuration file path:

```text
~/.config/open-switcher/config.toml
```

Important behavior:
- only the daemon reads and writes the config file
- tray and settings talk to the daemon over D-Bus
- the settings window does not write the config directly

## D-Bus Notes

The daemon exposes the session D-Bus name `org.oswitch.core`.

Quick inspection:

```bash
gdbus call \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core \
  --method org.oswitch.core.GetSettings
```

On Debian/Ubuntu-like systems, `gdbus` is typically provided by `libglib2.0-bin`.

Watch status and settings-related signals:

```bash
gdbus monitor \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core
```

## Known Limitations

- OpenSwitcher is currently focused on EN/RU typing workflows only.
- Confirmed environments are listed in [Tested Environments](#tested-environments).
- Current confirmed Wayland target: GNOME Wayland.
- Linux desktop environments and Wayland compositors not listed in Tested Environments are best-effort until tested and backed by diagnostics/backend support.
- Application-specific exclusions are not included in the first release.
- A GNOME Shell extension is not included.
- Tray visibility depends on a compatible StatusNotifier/AppIndicator host.
- Tray visibility is checked separately from daemon, D-Bus, and settings health.
- A missing tray icon does not by itself prove daemon failure, but it is a user-facing acceptance failure for any fully supported environment.
- The official runtime and autostart model depends on `systemd --user`.
- Selected-text conversion temporarily uses the clipboard and attempts to restore previous clipboard contents after conversion.
- Manual and selected-text hotkey capture may depend on laptop Fn keys and desktop/global shortcut handling.
- The autocorrection heuristic is intentionally conservative. Some short RU -> EN technical false negatives may remain, for example `cargo`, `rust`, `sudo`, `git`, `ssh`, `npm`, `jwt`.
- Existing rustfmt drift and non-hermetic shell/platform tests are known technical debt for later cleanup.

## Development Smoke Test

Recommended source-tree sanity-check order:

1. `./manage.sh dev build`
2. `./manage.sh dev start`
3. `./manage.sh dev settings`
4. `./manage.sh dev stop`
5. `./manage.sh systemd install`
6. `./manage.sh systemd start`
7. `./manage.sh systemd status`
8. `./manage.sh systemd logs`

Supported-environment smoke checklist:

1. The daemon is running.
2. D-Bus responds on `org.oswitch.core`.
3. The settings window opens.
4. The tray icon is visible.
5. The tray menu opens.
6. Settings can be opened from the tray.
7. The application can be stopped or disabled through the intended user-facing path.
8. Restarting the tray service through `systemd --user` works correctly.

## Troubleshooting

### `Dbus(NameTaken)` or `The name org.oswitch.core is already owned`

That usually means another daemon instance is already running.

Check:

```bash
gdbus call \
  --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner \
  org.oswitch.core
```

### `ServiceUnknown` on D-Bus calls

That usually means the daemon is not running or exited during startup.

Check:

```bash
./manage.sh dev status
./manage.sh systemd status
./manage.sh systemd logs
```

### Tray icon does not appear

A missing tray icon should be diagnosed separately from daemon health. The daemon, D-Bus, and
settings may still work, but a visible and working tray is required for an environment to be
considered fully supported.

Likely causes:
- the tray process is not running
- the desktop environment does not expose a compatible tray host
- the tray was started in a different way than expected for the current mode

Check:

```bash
./manage.sh dev status
./manage.sh systemd status
./manage.sh systemd logs tray
gdbus call \
  --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner \
  org.oswitch.core
./manage.sh dev settings
```

## License

This project is licensed under the MIT License. See the LICENSE file for details.
