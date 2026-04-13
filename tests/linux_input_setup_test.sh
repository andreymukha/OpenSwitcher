#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=/dev/null
source "$REPO_ROOT/scripts/linux_input_setup.sh"

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

create_fake_input_fixture() {
    local root="$1"

    mkdir -p \
        "$root/dev/input/by-path" \
        "$root/dev/input/by-id" \
        "$root/proc/bus/input"

    : >"$root/dev/input/event4"
    : >"$root/dev/input/event8"
    : >"$root/dev/input/event9"
    : >"$root/dev/uinput"

    chmod 000 "$root/dev/input/event4"
    chmod 000 "$root/dev/input/event8"
    chmod 000 "$root/dev/input/event9"
    chmod 600 "$root/dev/uinput"

    ln -s ../event4 "$root/dev/input/by-path/platform-i8042-serio-0-event-kbd"
    ln -s ../event8 "$root/dev/input/by-path/pci-0000:00:1f.4-event-mouse"
    ln -s ../event9 "$root/dev/input/by-id/usb-trackpoint-event-mouse"

    cat >"$root/proc/bus/input/devices" <<'EOF'
I: Bus=0011 Vendor=0001 Product=0001 Version=ab83
N: Name="AT Translated Set 2 keyboard"
H: Handlers=sysrq kbd event4 leds

I: Bus=0018 Vendor=04f3 Product=000e Version=0000
N: Name="Elan Touchpad"
H: Handlers=mouse0 event8

I: Bus=0018 Vendor=04f3 Product=000e Version=0000
N: Name="Elan TrackPoint"
H: Handlers=mouse1 event9
EOF
}

test_doctor_reports_mixed_setup_problem() {
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' RETURN
    create_fake_input_fixture "$fixture"

    local output
    set +e
    output="$(
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES="$fixture/proc/bus/input/devices" \
        openswitcher_linux_input_doctor 2>&1
    )"
    local status=$?
    set -e

    if [[ "$status" -eq 0 ]]; then
        echo "doctor unexpectedly succeeded" >&2
        printf '%s\n' "$output" >&2
        exit 1
    fi

    assert_contains "$output" "Keyboard device detected:"
    assert_contains "$output" "Keyboard access: denied"
    assert_contains "$output" "Pointer access: denied"
    assert_contains "$output" "uinput access: available"
    assert_contains "$output" 'Run `./manage.sh bootstrap linux-input`'
}

test_doctor_reports_ready_state() {
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' RETURN
    create_fake_input_fixture "$fixture"

    chmod 600 "$fixture/dev/input/event4" "$fixture/dev/input/event8" "$fixture/dev/input/event9"

    local output
    output="$(
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES="$fixture/proc/bus/input/devices" \
        openswitcher_linux_input_doctor 2>&1
    )"

    assert_contains "$output" "Keyboard access: available"
    assert_contains "$output" "Pointer access: available"
    assert_contains "$output" "uinput access: available"
    assert_contains "$output" "Linux input setup is ready."
}

test_doctor_reports_keyboard_not_found() {
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' RETURN
    create_fake_input_fixture "$fixture"

    rm -f "$fixture/dev/input/by-path/platform-i8042-serio-0-event-kbd"
    : >"$fixture/proc/bus/input/devices"

    local output
    set +e
    output="$(
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES="$fixture/proc/bus/input/devices" \
        openswitcher_linux_input_doctor 2>&1
    )"
    local status=$?
    set -e

    if [[ "$status" -eq 0 ]]; then
        echo "doctor unexpectedly succeeded for missing keyboard" >&2
        printf '%s\n' "$output" >&2
        exit 1
    fi

    assert_contains "$output" "Keyboard device: not found"
    assert_contains "$output" 'Run `./manage.sh doctor` after connecting the keyboard device or fixing the Linux input setup.'
}

test_bootstrap_installs_rule_and_applies_acl_bridge() {
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' RETURN
    create_fake_input_fixture "$fixture"

    local rules_dir="$fixture/etc/udev/rules.d"
    mkdir -p "$rules_dir"

    OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
    OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES="$fixture/proc/bus/input/devices" \
    OPEN_SWITCHER_LINUX_INPUT_RULES_DIR="$rules_dir" \
        openswitcher_linux_input_bootstrap_root "$REPO_ROOT" "$(id -un)" >/dev/null

    if [[ ! -f "$rules_dir/80-openswitcher-input.rules" ]]; then
        echo "udev rule was not installed into temp rules dir" >&2
        exit 1
    fi

    if command -v setfacl >/dev/null 2>&1 && command -v getfacl >/dev/null 2>&1; then
        local acl_dump
        acl_dump="$(
            getfacl -p \
                "$fixture/dev/input/event4" \
                "$fixture/dev/input/event8" \
                "$fixture/dev/input/event9" \
                "$fixture/dev/uinput" 2>/dev/null
        )"

        assert_contains "$acl_dump" "user:$(id -un):rw-"
    fi
}

test_doctor_reports_mixed_setup_problem
test_doctor_reports_ready_state
test_doctor_reports_keyboard_not_found
test_bootstrap_installs_rule_and_applies_acl_bridge

echo "linux_input_setup_test.sh: ok"
