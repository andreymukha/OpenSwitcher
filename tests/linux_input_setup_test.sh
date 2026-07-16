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

assert_equals() {
    local expected="$1"
    local actual="$2"
    local description="$3"

    if [[ "$actual" != "$expected" ]]; then
        echo "unexpected $description" >&2
        echo "--- expected ---" >&2
        printf '%s\n' "$expected" >&2
        echo "--- actual ---" >&2
        printf '%s\n' "$actual" >&2
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

create_fake_linux_input_commands() {
    local fixture="$1"
    local fake_bin="$fixture/fake-bin"

    mkdir -p "$fake_bin"

    cat >"$fake_bin/udevadm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

: "${OPEN_SWITCHER_TEST_FIXTURE_ROOT:?}"
: "${OPEN_SWITCHER_TEST_UDEVADM_LOG:?}"

case "$OPEN_SWITCHER_TEST_UDEVADM_LOG" in
    "$OPEN_SWITCHER_TEST_FIXTURE_ROOT"/*) ;;
    *)
        echo "refusing to write udevadm log outside test fixture" >&2
        exit 1
        ;;
esac

printf '%s\n' "$*" >>"$OPEN_SWITCHER_TEST_UDEVADM_LOG"
EOF

    cat >"$fake_bin/setfacl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

: "${OPEN_SWITCHER_TEST_FIXTURE_ROOT:?}"
: "${OPEN_SWITCHER_TEST_SETFACL_LOG:?}"

case "$OPEN_SWITCHER_TEST_SETFACL_LOG" in
    "$OPEN_SWITCHER_TEST_FIXTURE_ROOT"/*) ;;
    *)
        echo "refusing to write setfacl log outside test fixture" >&2
        exit 1
        ;;
esac

printf '%s\n' "$*" >>"$OPEN_SWITCHER_TEST_SETFACL_LOG"
EOF

    cat >"$fake_bin/install" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

: "${OPEN_SWITCHER_TEST_FIXTURE_ROOT:?}"
: "${OPEN_SWITCHER_TEST_INSTALL_LOG:?}"
: "${OPEN_SWITCHER_TEST_RULE_SOURCE:?}"
: "${OPEN_SWITCHER_TEST_RULE_TARGET:?}"

case "$OPEN_SWITCHER_TEST_INSTALL_LOG" in
    "$OPEN_SWITCHER_TEST_FIXTURE_ROOT"/*) ;;
    *)
        echo "refusing to write install log outside test fixture" >&2
        exit 1
        ;;
esac

printf '%s\n' "$*" >>"$OPEN_SWITCHER_TEST_INSTALL_LOG"

if [[ "$#" -ne 4 ]] || [[ "$1" != "-m" ]] || [[ "$2" != "0644" ]]; then
    echo "unexpected install arguments" >&2
    exit 1
fi

source_path="$3"
target_path="$4"
if [[ "$source_path" != "$OPEN_SWITCHER_TEST_RULE_SOURCE" ]] ||
    [[ "$target_path" != "$OPEN_SWITCHER_TEST_RULE_TARGET" ]]; then
    echo "refusing unexpected install source or target" >&2
    exit 1
fi

case "$target_path" in
    "$OPEN_SWITCHER_TEST_FIXTURE_ROOT"/*) ;;
    *)
        echo "refusing to install outside test fixture" >&2
        exit 1
        ;;
esac

umask 022
while IFS= read -r line || [[ -n "$line" ]]; do
    printf '%s\n' "$line"
done <"$source_path" >"$target_path"
EOF

    chmod +x "$fake_bin/udevadm" "$fake_bin/setfacl" "$fake_bin/install"
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
    create_fake_linux_input_commands "$fixture"

    local rules_dir="$fixture/etc/udev/rules.d"
    local fake_bin="$fixture/fake-bin"
    local install_log="$fixture/install.log"
    local udevadm_log="$fixture/udevadm.log"
    local setfacl_log="$fixture/setfacl.log"
    local rule_source="$REPO_ROOT/dist/udev/80-openswitcher-input.rules"
    local rule_target="$rules_dir/80-openswitcher-input.rules"
    local target_user
    target_user="$(id -un)"

    mkdir -p "$rules_dir"
    : >"$install_log"
    : >"$udevadm_log"
    : >"$setfacl_log"

    PATH="$fake_bin:$PATH" \
    OPEN_SWITCHER_TEST_FIXTURE_ROOT="$fixture" \
    OPEN_SWITCHER_TEST_INSTALL_LOG="$install_log" \
    OPEN_SWITCHER_TEST_UDEVADM_LOG="$udevadm_log" \
    OPEN_SWITCHER_TEST_SETFACL_LOG="$setfacl_log" \
    OPEN_SWITCHER_TEST_RULE_SOURCE="$rule_source" \
    OPEN_SWITCHER_TEST_RULE_TARGET="$rule_target" \
    OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT="$fixture/dev" \
    OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES="$fixture/proc/bus/input/devices" \
    OPEN_SWITCHER_LINUX_INPUT_RULES_DIR="$rules_dir" \
        openswitcher_linux_input_bootstrap_root "$REPO_ROOT" "$target_user" >/dev/null

    if [[ ! -f "$rule_target" ]]; then
        echo "udev rule was not installed into temp rules dir" >&2
        exit 1
    fi

    if ! cmp -s "$rule_source" "$rule_target"; then
        echo "installed udev rule does not match source asset" >&2
        exit 1
    fi

    assert_equals \
        "-m 0644 $rule_source $rule_target" \
        "$(<"$install_log")" \
        "install calls"

    assert_equals \
        $'control --reload-rules\ntrigger --subsystem-match=input --action=change\ntrigger --subsystem-match=misc --sysname-match=uinput --action=change' \
        "$(<"$udevadm_log")" \
        "udevadm calls"

    local expected_setfacl_calls
    expected_setfacl_calls="$(printf '%s\n' \
        "-m u:${target_user}:rw $fixture/dev/input/event4" \
        "-m u:${target_user}:rw $fixture/dev/input/event8" \
        "-m u:${target_user}:rw $fixture/dev/input/event9" \
        "-m u:${target_user}:rw $fixture/dev/uinput")"
    assert_equals \
        "$expected_setfacl_calls" \
        "$(<"$setfacl_log")" \
        "setfacl calls"
}

test_doctor_reports_mixed_setup_problem
test_doctor_reports_ready_state
test_doctor_reports_keyboard_not_found
test_bootstrap_installs_rule_and_applies_acl_bridge

echo "linux_input_setup_test.sh: ok"
