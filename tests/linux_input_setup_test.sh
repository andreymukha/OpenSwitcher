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
        return 1
    fi
}

assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    if [[ "$haystack" == *"$needle"* ]]; then
        echo "expected output not to contain: $needle" >&2
        echo "--- output ---" >&2
        printf '%s\n' "$haystack" >&2
        return 1
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

run_disabled_bootstrap_case() (
    set -euo pipefail

    local case_name="$1"
    shift
    local test_root
    test_root="$(mktemp -d)"
    trap 'rm -rf "$test_root"' EXIT
    local fixture="$test_root/source-copy"
    local fake_bin="$test_root/inert-bin"
    local output=""
    local status=0
    local command_name=""

    mkdir -p "$fixture" "$fake_bin"
    cp "$REPO_ROOT/manage.sh" "$fixture/manage.sh"
    chmod +x "$fixture/manage.sh"

    for command_name in sudo install udevadm setfacl; do
        : >"$test_root/$command_name.log"
        printf '%s\n' \
            '#!/usr/bin/env bash' \
            'set -euo pipefail' \
            ': "${OPEN_SWITCHER_TEST_GUARD_ROOT:?}"' \
            'command_name="$(basename "$0")"' \
            'printf "%s\\n" "$*" >>"$OPEN_SWITCHER_TEST_GUARD_ROOT/$command_name.log"' \
            'exit 97' >"$fake_bin/$command_name"
        chmod +x "$fake_bin/$command_name"
    done

    set +e
    output="$(
        cd "$fixture"
        env -i \
            PATH="$fake_bin:/usr/bin:/bin" \
            HOME="$fixture/home" \
            OPEN_SWITCHER_TEST_GUARD_ROOT="$test_root" \
            "$@" \
            ./manage.sh bootstrap linux-input 2>&1
    )"
    status=$?
    set -e

    local failures=0
    if [[ "$status" -eq 0 ]]; then
        echo "$case_name: disabled source bootstrap unexpectedly succeeded" >&2
        failures=$((failures + 1))
    fi

    assert_contains "$output" "Source-tree Linux input bootstrap is disabled." || failures=$((failures + 1))
    assert_contains "$output" "Linux input setup helpers and assets were not executed or installed with elevated privileges." || failures=$((failures + 1))
    assert_contains "$output" "./manage.sh package deb" || failures=$((failures + 1))
    assert_contains "$output" 'exact `sudo apt install <artifact>` command printed by the build' || failures=$((failures + 1))
    assert_contains "$output" 'Use `--reinstall` only when the same package version is already installed.' || failures=$((failures + 1))
    assert_contains "$output" "Sign out and sign in again" || failures=$((failures + 1))
    assert_contains "$output" "./manage.sh doctor" || failures=$((failures + 1))
    assert_contains "$output" "Privileged Linux input setup and system configuration were not changed." || failures=$((failures + 1))
    assert_contains "$output" "Do not run ./manage.sh with sudo." || failures=$((failures + 1))
    assert_not_contains "$output" "Повторная проверка Linux input setup..." || failures=$((failures + 1))

    if [[ -e "$fixture/.run" ]]; then
        echo "$case_name: early migration gate created fixture .run" >&2
        failures=$((failures + 1))
    fi
    if [[ -e "$fixture/scripts" ]]; then
        echo "$case_name: source-copy unexpectedly gained a scripts directory" >&2
        failures=$((failures + 1))
    fi

    for command_name in sudo install udevadm setfacl; do
        if [[ -s "$test_root/$command_name.log" ]]; then
            echo "$case_name: fake $command_name was invoked" >&2
            sed 's/^/  argv: /' "$test_root/$command_name.log" >&2
            failures=$((failures + 1))
        fi
    done

    if [[ "$failures" -ne 0 ]]; then
        echo "--- source bootstrap output for $case_name (status $status) ---" >&2
        if [[ -n "$output" ]]; then
            printf '%s\n' "$output" >&2
        else
            echo "<empty>" >&2
        fi
        return 1
    fi
)

test_source_tree_bootstrap_is_disabled_without_mutation() {
    local -a dev_override=(
        "OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT=/tmp/openswitcher-test-dev"
    )
    local -a proc_override=(
        "OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES=/tmp/openswitcher-test-proc-devices"
    )
    local -a rules_override=(
        "OPEN_SWITCHER_LINUX_INPUT_RULES_DIR=/tmp/openswitcher-test-rules"
    )
    local -a all_overrides=(
        "${dev_override[@]}"
        "${proc_override[@]}"
        "${rules_override[@]}"
    )

    local failures=0
    local case_output=""
    if ! case_output="$(run_disabled_bootstrap_case clean 2>&1)"; then
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    fi
    if ! case_output="$(run_disabled_bootstrap_case dev-root "${dev_override[@]}" 2>&1)"; then
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    fi
    if ! case_output="$(run_disabled_bootstrap_case proc-devices "${proc_override[@]}" 2>&1)"; then
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    fi
    if ! case_output="$(run_disabled_bootstrap_case rules-dir "${rules_override[@]}" 2>&1)"; then
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    fi
    if ! case_output="$(run_disabled_bootstrap_case all-overrides "${all_overrides[@]}" 2>&1)"; then
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    fi

    if [[ "$failures" -ne 0 ]]; then
        echo "source-tree bootstrap invariant failed in $failures/5 cases" >&2
        return 1
    fi
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
        return 1
    fi

    assert_contains "$output" "Keyboard device detected:"
    assert_contains "$output" "Keyboard access: denied"
    assert_contains "$output" "Pointer access: denied"
    assert_contains "$output" "uinput access: available"
    assert_contains "$output" "./manage.sh package deb"
    assert_contains "$output" 'exact `sudo apt install <artifact>` command printed by the build'
    assert_contains "$output" 'Use `--reinstall` only when the same package version is already installed.'
    assert_contains "$output" "Sign out and sign in again"
    assert_contains "$output" './manage.sh doctor'
    assert_not_contains "$output" './manage.sh bootstrap linux-input'
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
        return 1
    fi

    assert_contains "$output" "Keyboard device: not found"
    assert_contains "$output" "Connect the keyboard device before checking the setup again."
    assert_contains "$output" "./manage.sh package deb"
    assert_contains "$output" 'exact `sudo apt install <artifact>` command printed by the build'
    assert_contains "$output" 'Use `--reinstall` only when the same package version is already installed.'
    assert_contains "$output" "Sign out and sign in again"
    assert_contains "$output" './manage.sh doctor'
    assert_not_contains "$output" './manage.sh bootstrap linux-input'
}

test_source_tree_bootstrap_is_disabled_without_mutation
test_doctor_reports_mixed_setup_problem
test_doctor_reports_ready_state
test_doctor_reports_keyboard_not_found

echo "linux_input_setup_test.sh: ok"
