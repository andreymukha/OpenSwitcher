#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

assert_contains() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq -- "$expected" "$file"; then
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

assert_not_contains() {
    local file="$1"
    local unexpected="$2"

    if grep -Fq -- "$unexpected" "$file"; then
        echo "expected '$file' not to contain: $unexpected" >&2
        exit 1
    fi
}

test_launch_imports_graphical_environment_before_start() {
    local file="$REPO_ROOT/debian/scripts/open-switcher-launch"

    assert_contains "$file" "--manual"
    assert_contains "$file" "--autostart"
    assert_contains "$file" "systemctl --user import-environment"
    assert_contains "$file" "DISPLAY"
    assert_contains "$file" "XAUTHORITY"
    assert_contains "$file" "XDG_SESSION_TYPE"
    assert_contains "$file" "XDG_CURRENT_DESKTOP"
    assert_contains "$file" ".config/autostart/open-switcher.desktop"
    assert_contains "$file" "X-GNOME-Autostart-enabled=true"
    assert_contains "$file" "systemctl --user start open-switcher-tray.service"
}

test_package_session_start_imports_loginctl_environment() {
    local file="$REPO_ROOT/debian/scripts/open-switcher-user-session-start"

    assert_contains "$file" "Display --value"
    assert_contains "$file" "Type --value"
    assert_contains "$file" "Desktop --value"
    assert_contains "$file" "systemctl --user import-environment"
    assert_contains "$file" "systemctl --user disable open-switcher-daemon.service open-switcher-tray.service"
    assert_contains "$file" "/usr/lib/open-switcher/open-switcher-launch --autostart"
}

test_package_installs_xdg_autostart_fallback() {
    local autostart="$REPO_ROOT/debian/autostart/open-switcher-autostart.desktop"
    local desktop="$REPO_ROOT/debian/open-switcher.desktop"
    local install_file="$REPO_ROOT/debian/open-switcher.install"

    assert_file_exists "$autostart"
    assert_contains "$desktop" "Exec=/usr/lib/open-switcher/open-switcher-launch --manual"
    assert_contains "$autostart" "Exec=/usr/lib/open-switcher/open-switcher-launch --autostart"
    assert_contains "$autostart" "NoDisplay=true"
    assert_contains "$autostart" "X-GNOME-Autostart-enabled=true"
    assert_contains "$install_file" "debian/autostart/open-switcher-autostart.desktop etc/xdg/autostart/"
}

test_package_does_not_globally_enable_user_units() {
    local rules="$REPO_ROOT/debian/rules"
    local postinst="$REPO_ROOT/debian/open-switcher.postinst"

    assert_contains "$rules" "dh_installsystemduser --no-enable --name=open-switcher-daemon"
    assert_contains "$rules" "dh_installsystemduser --no-enable --name=open-switcher-tray"
    assert_contains "$postinst" "/etc/systemd/user/default.target.wants/open-switcher-daemon.service"
    assert_contains "$postinst" "/etc/systemd/user/graphical-session.target.wants/open-switcher-tray.service"
}

test_privileged_input_setup_uses_package_owned_trust_anchors() {
    local install_file="$REPO_ROOT/debian/open-switcher.install"
    local rules="$REPO_ROOT/debian/rules"
    local postinst="$REPO_ROOT/debian/open-switcher.postinst"
    local prerm="$REPO_ROOT/debian/open-switcher.prerm"
    local postrm="$REPO_ROOT/debian/open-switcher.postrm"
    local acl_bridge="$REPO_ROOT/debian/scripts/open-switcher-input-acl-bridge"
    local udev_rule="$REPO_ROOT/debian/open-switcher.openswitcher-input.udev"
    local file=""

    assert_contains "$install_file" "debian/scripts/open-switcher-input-acl-bridge usr/lib/open-switcher/"
    assert_contains "$rules" "dh_installudev --name=openswitcher-input --priority=80"
    assert_contains "$rules" "chmod 0755 debian/open-switcher/usr/lib/open-switcher/open-switcher-input-acl-bridge"
    assert_contains "$postinst" "/usr/lib/open-switcher/open-switcher-input-acl-bridge"

    for file in \
        "$install_file" \
        "$rules" \
        "$postinst" \
        "$prerm" \
        "$postrm" \
        "$acl_bridge" \
        "$udev_rule"; do
        assert_not_contains "$file" "scripts/linux_input_setup.sh"
        assert_not_contains "$file" "dist/udev"
        assert_not_contains "$file" "OPEN_SWITCHER_LINUX_INPUT_"
    done
}

test_launch_imports_graphical_environment_before_start
test_package_session_start_imports_loginctl_environment
test_package_installs_xdg_autostart_fallback
test_package_does_not_globally_enable_user_units
test_privileged_input_setup_uses_package_owned_trust_anchors

echo "debian_package_scripts_test.sh: ok"
