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

The current public scope is a Linux desktop application for EN/RU typing workflows, built around a `daemon + tray` runtime model.

## Quick Start

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

## Current Scope And Limitations

- Linux only
- Main supported typing scenario is EN/RU
- Layout/backend support is still conservative and backend-driven
- The current backend layer is designed for expansion, but support is not yet broad across all desktop environments
- Tray support depends on the desktop environment providing a compatible StatusNotifier/AppIndicator host
- The settings UI is built behind the `settings-ui` Cargo feature

## Requirements

### Runtime environment

- Linux desktop session
- session D-Bus
- `systemd --user` for the official autostart/runtime model
- a desktop environment with a compatible StatusNotifier/AppIndicator tray host

## Linux Input Setup

OpenSwitcher reads real input devices from `/dev/input/event*` and writes virtual key events through `/dev/uinput`.

Check whether the current session is ready:

```bash
./manage.sh doctor
```

If the doctor reports denied access, run the official setup step:

```bash
./manage.sh bootstrap linux-input
```

What the bootstrap does:
- installs the project udev rule from `dist/udev/80-openswitcher-input.rules`
- reloads udev rules when `udevadm` is available
- applies a same-session ACL bridge for the current user when `setfacl` is available
- reruns `./manage.sh doctor` to confirm the result

This setup layer is explicit on purpose so it can later be reused by packaging without redesigning the Linux input model.

### Build dependencies

For Linux Mint / Ubuntu-like systems:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  libdbus-1-dev \
  libgtk-4-dev \
  libadwaita-1-dev
```

## Building

Check all binaries:

```bash
cargo check --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Build everything locally:

```bash
cargo build --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Run tests:

```bash
cargo test -q --lib
cargo test --test dbus_api
```

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

This is the official runtime model for the published application.

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
- `~/.local/share/applications/`
- `~/.local/share/icons/hicolor/512x512/apps/`
- `~/.local/bin/`

Notes:
- the desktop entry starts the tray service through `systemctl --user`
- the tray unit pulls in the daemon unit
- `~/.config/autostart` is not used

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

Watch status and settings-related signals:

```bash
gdbus monitor \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core
```

## Practical Smoke Test

Recommended local sanity-check order:

1. `./manage.sh dev build`
2. `./manage.sh dev start`
3. `./manage.sh dev settings`
4. `./manage.sh dev stop`
5. `./manage.sh systemd install`
6. `./manage.sh systemd start`
7. `./manage.sh systemd status`
8. `./manage.sh systemd logs`

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

Likely causes:
- the tray process is not running
- the desktop environment does not expose a compatible tray host
- the tray was started in a different way than expected for the current mode

Check:

```bash
./manage.sh dev status
./manage.sh systemd status
```

## License

This project is licensed under the MIT License. See the LICENSE file for details.
