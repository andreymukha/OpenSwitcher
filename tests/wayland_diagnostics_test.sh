#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=/dev/null
source "$REPO_ROOT/scripts/wayland_diagnostics.sh"

assert_contains() {
    local haystack="$1"
    local needle="$2"
    if [[ "$haystack" != *"$needle"* ]]; then
        echo "expected output to contain: $needle" >&2
        echo "--- output ---" >&2
        printf '%s\n' "$haystack" >&2
        exit 1
    fi
}

write_gsettings_fixture() {
    local root="$1"
    mkdir -p "$root"
    printf '%s\n' "['<Super>space']" \
        >"$root/org.gnome.desktop.wm.keybindings.switch-input-source"
    printf '%s\n' "['<Shift><Super>space']" \
        >"$root/org.gnome.desktop.wm.keybindings.switch-input-source-backward"
    printf '%s\n' "[('xkb', 'us'), ('xkb', 'ru')]" \
        >"$root/org.gnome.desktop.input-sources.sources"
    printf '%s\n' "[('xkb', 'ru'), ('xkb', 'us')]" \
        >"$root/org.gnome.desktop.input-sources.mru-sources"
}

start_unix_socket() {
    local path="$1"
    python3 - "$path" <<'PY' &
import socket
import sys
import time

path = sys.argv[1]
sock = socket.socket(socket.AF_UNIX)
sock.bind(path)
sock.listen(1)
try:
    time.sleep(30)
finally:
    sock.close()
PY
    START_UNIX_SOCKET_PID=$!
    for _ in {1..50}; do
        [[ -S "$path" ]] && break
        sleep 0.02
    done
}

test_wayland_doctor_reports_trusted_gnome_wayland_context() {
    local fixture
    fixture="$(mktemp -d)"
    trap '[[ -n "${START_UNIX_SOCKET_PID:-}" ]] && kill "$START_UNIX_SOCKET_PID" 2>/dev/null || true; rm -rf "$fixture"' RETURN

    mkdir -p "$fixture/runtime" "$fixture/dev"
    : >"$fixture/dev/uinput"
    chmod 600 "$fixture/dev/uinput"
    write_gsettings_fixture "$fixture/gsettings"

    START_UNIX_SOCKET_PID=""
    start_unix_socket "$fixture/runtime/wayland-test"

    local output
    output="$(
        XDG_SESSION_TYPE=wayland \
        XDG_CURRENT_DESKTOP=ubuntu:GNOME \
        WAYLAND_DISPLAY=wayland-test \
        XDG_RUNTIME_DIR="$fixture/runtime" \
        DISPLAY=:0 \
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
        OPEN_SWITCHER_WAYLAND_DOCTOR_GSETTINGS_DIR="$fixture/gsettings" \
            openswitcher_wayland_doctor 2>&1
    )"

    assert_contains "$output" "OpenSwitcher Wayland doctor"
    assert_contains "$output" "Session hint: Wayland"
    assert_contains "$output" "Desktop hint: GNOME"
    assert_contains "$output" "Wayland socket: live"
    assert_contains "$output" "DISPLAY under Wayland: present (normal for XWayland)"
    assert_contains "$output" "GNOME keybinding primary: ['<Super>space']"
    assert_contains "$output" "GNOME keybinding primary summary: supported Super+Space"
    assert_contains "$output" "GNOME keybinding backward summary: unsupported"
    assert_contains "$output" "GNOME sources: trusted xkb/us+xkb/ru"
    assert_contains "$output" "Current GNOME layout: Russian"
    assert_contains "$output" "uinput access: available"
}

test_wayland_doctor_reports_degraded_gnome_sources_without_failing() {
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' RETURN

    mkdir -p "$fixture/gsettings" "$fixture/dev"
    printf '%s\n' "['<Primary>space']" \
        >"$fixture/gsettings/org.gnome.desktop.wm.keybindings.switch-input-source"
    printf '%s\n' "[]" \
        >"$fixture/gsettings/org.gnome.desktop.wm.keybindings.switch-input-source-backward"
    printf '%s\n' "[('xkb', 'us'), ('xkb', 'ru'), ('xkb', 'de')]" \
        >"$fixture/gsettings/org.gnome.desktop.input-sources.sources"
    printf '%s\n' "[('ibus', 'mozc-jp'), ('xkb', 'us')]" \
        >"$fixture/gsettings/org.gnome.desktop.input-sources.mru-sources"

    local output
    output="$(
        XDG_SESSION_TYPE=x11 \
        XDG_CURRENT_DESKTOP=GNOME \
        WAYLAND_DISPLAY=missing-wayland \
        XDG_RUNTIME_DIR="$fixture/runtime" \
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
        OPEN_SWITCHER_WAYLAND_DOCTOR_GSETTINGS_DIR="$fixture/gsettings" \
            openswitcher_wayland_doctor 2>&1
    )"

    assert_contains "$output" "Session hint: X11"
    assert_contains "$output" "Wayland socket: missing/non-socket"
    assert_contains "$output" "GNOME keybinding primary summary: supported Ctrl+Space"
    assert_contains "$output" "GNOME sources: untrusted"
    assert_contains "$output" "Current GNOME layout: unsupported"
    assert_contains "$output" "uinput access: unavailable"
}

test_manage_dispatches_wayland_doctor() {
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' RETURN

    mkdir -p "$fixture/runtime" "$fixture/dev"
    write_gsettings_fixture "$fixture/gsettings"

    local output
    output="$(
        XDG_SESSION_TYPE=wayland \
        XDG_CURRENT_DESKTOP=GNOME \
        WAYLAND_DISPLAY=wayland-test \
        XDG_RUNTIME_DIR="$fixture/runtime" \
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
        OPEN_SWITCHER_WAYLAND_DOCTOR_GSETTINGS_DIR="$fixture/gsettings" \
            "$REPO_ROOT/manage.sh" doctor wayland 2>&1
    )"

    assert_contains "$output" "OpenSwitcher Wayland doctor"
    assert_contains "$output" "Session hint: Wayland"
    assert_contains "$output" "GNOME keybinding primary:"
}

test_wayland_doctor_reports_trusted_gnome_wayland_context
test_wayland_doctor_reports_degraded_gnome_sources_without_failing
test_manage_dispatches_wayland_doctor

echo "wayland_diagnostics_test.sh: ok"
