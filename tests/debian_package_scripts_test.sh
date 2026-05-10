#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

assert_contains() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq "$expected" "$file"; then
        echo "expected '$file' to contain: $expected" >&2
        exit 1
    fi
}

assert_file_exists() {
    local file="$1"

    if [[ ! -f "$file" ]]; then
        echo "expected file to exist: $file" >&2
        exit 1
    fi
}

test_launch_imports_graphical_environment_before_start() {
    local file="$REPO_ROOT/debian/scripts/open-switcher-launch"

    assert_contains "$file" "systemctl --user import-environment"
    assert_contains "$file" "DISPLAY"
    assert_contains "$file" "XAUTHORITY"
    assert_contains "$file" "XDG_SESSION_TYPE"
    assert_contains "$file" "XDG_CURRENT_DESKTOP"
    assert_contains "$file" "systemctl --user start open-switcher-tray.service"
}

test_package_session_start_imports_loginctl_environment() {
    local file="$REPO_ROOT/debian/scripts/open-switcher-user-session-start"

    assert_contains "$file" "Display --value"
    assert_contains "$file" "Type --value"
    assert_contains "$file" "Desktop --value"
    assert_contains "$file" "systemctl --user import-environment"
    assert_contains "$file" "systemctl --user start open-switcher-tray.service"
}

test_package_installs_xdg_autostart_fallback() {
    local autostart="$REPO_ROOT/debian/autostart/open-switcher-autostart.desktop"
    local install_file="$REPO_ROOT/debian/open-switcher.install"

    assert_file_exists "$autostart"
    assert_contains "$autostart" "Exec=/usr/lib/open-switcher/open-switcher-launch"
    assert_contains "$autostart" "NoDisplay=true"
    assert_contains "$autostart" "X-GNOME-Autostart-enabled=true"
    assert_contains "$install_file" "debian/autostart/open-switcher-autostart.desktop etc/xdg/autostart/"
}

test_launch_imports_graphical_environment_before_start
test_package_session_start_imports_loginctl_environment
test_package_installs_xdg_autostart_fallback

echo "debian_package_scripts_test.sh: ok"
