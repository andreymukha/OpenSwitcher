#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

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

assert_occurs_in_order() {
    local file="$1"
    shift

    local previous_line=0
    local expected=""
    local line=""
    for expected in "$@"; do
        line="$(grep -Fn -- "$expected" "$file" | head -n 1 | cut -d: -f1 || true)"
        if [[ -z "$line" ]]; then
            echo "expected '$file' to contain in order: $expected" >&2
            exit 1
        fi
        if ((line <= previous_line)); then
            echo "expected '$expected' after line $previous_line in '$file'" >&2
            exit 1
        fi
        previous_line="$line"
    done
}

assert_count() {
    local file="$1"
    local expected="$2"
    local count="$3"
    local actual=""

    actual="$(grep -Fc -- "$expected" "$file" || true)"
    if [[ "$actual" != "$count" ]]; then
        echo "expected '$file' to contain '$expected' $count time(s), got $actual" >&2
        exit 1
    fi
}

prepare_session_fixture() {
    local fixture_root="$1"
    local mock_bin="$fixture_root/bin"
    local runtime_root="$fixture_root/run/user"

    mkdir -p "$mock_bin" "$runtime_root/1000"
    : >"$runtime_root/1000/bus"

    cat >"$mock_bin/loginctl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf 'loginctl %s\n' "$*" >>"$CALL_LOG"

case "${1:-}" in
    list-sessions)
        printf '%s\n' \
            '7 1000 alice seat0' \
            '8 1000 alice seat0'
        ;;
    show-session)
        property=""
        while (($#)); do
            if [[ "$1" == "-p" ]]; then
                property="${2:-}"
                break
            fi
            shift
        done
        case "$property" in
            Active) printf '%s\n' 'no' ;;
            Remote) printf '%s\n' 'no' ;;
            Type) printf '%s\n' 'x11' ;;
            Class) printf '%s\n' 'user' ;;
            User) printf '%s\n' '1000' ;;
            Name) printf '%s\n' 'alice' ;;
            *) exit 1 ;;
        esac
        ;;
    *)
        exit 1
        ;;
esac
MOCK

    cat >"$mock_bin/runuser" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf 'runuser %s\n' "$*" >>"$CALL_LOG"
MOCK

    chmod +x "$mock_bin/loginctl" "$mock_bin/runuser"
}

render_session_script_fixture() {
    local source="$1"
    local destination="$2"
    local runtime_root="$3"

    sed \
        -e "s|/run/user|$runtime_root|g" \
        -e 's/\[ -S /[ -e /g' \
        "$source" >"$destination"
    chmod +x "$destination"
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

test_package_installs_isolated_xtest_guardian_units() {
    local rules="$REPO_ROOT/debian/rules"
    local debian_daemon="$REPO_ROOT/debian/open-switcher.open-switcher-daemon.user.service"
    local dist_daemon="$REPO_ROOT/dist/systemd/open-switcher-daemon.service"
    local debian_socket="$REPO_ROOT/debian/open-switcher.open-switcher-xtest-guardian.user.socket"
    local debian_service="$REPO_ROOT/debian/open-switcher.open-switcher-xtest-guardian.user.service"
    local dist_socket="$REPO_ROOT/dist/systemd/open-switcher-xtest-guardian.socket"
    local dist_service="$REPO_ROOT/dist/systemd/open-switcher-xtest-guardian.service"
    local daemon_unit=""
    local socket_unit=""

    assert_file_exists "$debian_socket"
    assert_file_exists "$debian_service"
    assert_file_exists "$dist_socket"
    assert_file_exists "$dist_service"

    for daemon_unit in "$debian_daemon" "$dist_daemon"; do
        assert_contains "$daemon_unit" "Wants=open-switcher-xtest-guardian.socket"
        assert_contains "$daemon_unit" "After=open-switcher-xtest-guardian.socket"
        assert_not_contains "$daemon_unit" "PartOf=open-switcher-xtest-guardian"
        assert_not_contains "$daemon_unit" "BindsTo=open-switcher-xtest-guardian"
        assert_not_contains "$daemon_unit" "open-switcher-xtest-guardian.service"
    done

    for socket_unit in "$debian_socket" "$dist_socket"; do
        assert_contains "$socket_unit" "ListenSequentialPacket=%t/open-switcher/xtest-guardian.sock"
        assert_contains "$socket_unit" "SocketMode=0600"
        assert_contains "$socket_unit" "DirectoryMode=0700"
        assert_contains "$socket_unit" "FileDescriptorName=xtest-guardian"
        assert_not_contains "$socket_unit" "[Install]"
    done

    assert_contains "$debian_service" \
        "ExecStart=/usr/bin/open-switcher-daemon --internal-xtest-guardian-v1"
    assert_contains "$dist_service" \
        "ExecStart=open-switcher-daemon --internal-xtest-guardian-v1"
    assert_contains "$debian_service" "TimeoutStopSec=7s"
    assert_contains "$debian_service" "NoNewPrivileges=yes"
    assert_contains "$debian_service" "PrivateDevices=yes"
    assert_contains "$debian_service" "RestrictAddressFamilies=AF_UNIX"
    assert_contains "$rules" \
        "dh_installsystemduser --no-enable --name=open-switcher-xtest-guardian"
    assert_count "$rules" \
        "dh_installsystemduser --no-enable --name=open-switcher-xtest-guardian" 1
}

test_package_maintainer_scripts_cover_upgrade_and_abort_paths() {
    local preinst="$REPO_ROOT/debian/open-switcher.preinst"
    local prerm="$REPO_ROOT/debian/open-switcher.prerm"
    local postinst="$REPO_ROOT/debian/open-switcher.postinst"

    assert_file_exists "$preinst"
    assert_contains "$preinst" "upgrade"
    assert_contains "$prerm" "upgrade"
    assert_contains "$prerm" "remove"
    assert_contains "$prerm" "deconfigure"
    assert_contains "$prerm" "failed-upgrade"
    assert_contains "$postinst" "configure"
    assert_contains "$postinst" "abort-upgrade"
    assert_contains "$postinst" "abort-remove"
    assert_contains "$postinst" "abort-deconfigure"
}

test_session_stop_is_sequential_bounded_and_deduplicated_for_inactive_session() {
    local fixture_root="$TMP_DIR/session-stop"
    local call_log="$fixture_root/calls.log"
    local fixture_script="$fixture_root/open-switcher-user-session-stop"

    prepare_session_fixture "$fixture_root"
    render_session_script_fixture \
        "$REPO_ROOT/debian/scripts/open-switcher-user-session-stop" \
        "$fixture_script" \
        "$fixture_root/run/user"

    PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" "$fixture_script"

    assert_not_contains "$call_log" "-p Active"
    assert_count "$call_log" \
        "systemctl --user stop open-switcher-daemon.service" 1
    assert_occurs_in_order "$call_log" \
        "systemctl --user stop open-switcher-tray.service" \
        "systemctl --user stop open-switcher-daemon.service" \
        "timeout --signal=KILL 7s" \
        "systemctl --user stop open-switcher-xtest-guardian.socket" \
        "systemctl --user stop open-switcher-xtest-guardian.service" \
        "systemctl --user daemon-reload"
}

test_preinst_upgrade_stops_inactive_old_daemon_after_legacy_helper() {
    local fixture_root="$TMP_DIR/preinst"
    local call_log="$fixture_root/calls.log"
    local fixture_script="$fixture_root/open-switcher.preinst"
    local old_helper="$fixture_root/open-switcher-user-session-stop"

    prepare_session_fixture "$fixture_root"
    cat >"$old_helper" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' 'old-helper' >>"$CALL_LOG"
MOCK
    chmod +x "$old_helper"

    sed \
        -e "s|/usr/lib/open-switcher/open-switcher-user-session-stop|$old_helper|g" \
        -e "s|/run/user|$fixture_root/run/user|g" \
        -e 's/\[ -S /[ -e /g' \
        "$REPO_ROOT/debian/open-switcher.preinst" >"$fixture_script"
    chmod +x "$fixture_script"

    PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" \
        "$fixture_script" upgrade 0.1.0-3

    assert_not_contains "$call_log" "-p Active"
    assert_count "$call_log" \
        "systemctl --user stop open-switcher-daemon.service" 1
    assert_occurs_in_order "$call_log" \
        "old-helper" \
        "systemctl --user stop open-switcher-daemon.service"
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
test_package_installs_isolated_xtest_guardian_units
test_package_maintainer_scripts_cover_upgrade_and_abort_paths
test_session_stop_is_sequential_bounded_and_deduplicated_for_inactive_session
test_preinst_upgrade_stops_inactive_old_daemon_after_legacy_helper
test_privileged_input_setup_uses_package_owned_trust_anchors

echo "debian_package_scripts_test.sh: ok"
