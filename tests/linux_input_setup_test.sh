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

    cat >"$fake_bin/sudo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

: "${OPEN_SWITCHER_TEST_FIXTURE_ROOT:?}"
: "${OPEN_SWITCHER_TEST_SUDO_LOG:?}"

case "$OPEN_SWITCHER_TEST_SUDO_LOG" in
    "$OPEN_SWITCHER_TEST_FIXTURE_ROOT"/*) ;;
    *)
        echo "refusing to write sudo log outside test fixture" >&2
        exit 1
        ;;
esac

printf '%s\n' "$*" >>"$OPEN_SWITCHER_TEST_SUDO_LOG"

# Never execute privileged argv in tests. A non-zero status also keeps manage.sh
# from continuing into its post-bootstrap doctor when the boundary is crossed.
exit 97
EOF

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

    chmod +x \
        "$fake_bin/sudo" \
        "$fake_bin/udevadm" \
        "$fake_bin/setfacl" \
        "$fake_bin/install"
}

test_production_bootstrap_rejects_override_case() (
    set -euo pipefail

    local override_name="$1"
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT

    create_fake_input_fixture "$fixture"
    create_fake_linux_input_commands "$fixture"

    local protected_target="$fixture/protected-target"
    local protected_snapshot="$fixture/protected-target.before"
    printf 'protected target must remain byte-identical\n' >"$protected_target"
    cp "$protected_target" "$protected_snapshot"
    rm -f "$fixture/dev/uinput"
    ln -s "$protected_target" "$fixture/dev/uinput"

    local rules_dir="$fixture/etc/udev/rules.d"
    local fake_bin="$fixture/fake-bin"
    local sudo_log="$fixture/sudo.log"
    local install_log="$fixture/install.log"
    local udevadm_log="$fixture/udevadm.log"
    local setfacl_log="$fixture/setfacl.log"
    local rule_source="$REPO_ROOT/dist/udev/80-openswitcher-input.rules"
    local rule_target="$rules_dir/80-openswitcher-input.rules"
    local override_value=""

    case "$override_name" in
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT)
            override_value="$fixture/dev"
            ;;
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES)
            override_value="$fixture/proc/bus/input/devices"
            ;;
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR)
            override_value="$rules_dir"
            ;;
        *)
            echo "unexpected override case: $override_name" >&2
            return 1
            ;;
    esac

    mkdir -p "$rules_dir" "$fixture/home"
    : >"$sudo_log"
    : >"$install_log"
    : >"$udevadm_log"
    : >"$setfacl_log"

    local output
    local status
    set +e
    output="$(
        cd "$REPO_ROOT"
        unset \
            OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT \
            OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES \
            OPEN_SWITCHER_LINUX_INPUT_RULES_DIR
        printf -v "$override_name" '%s' "$override_value"
        export "$override_name"

        PATH="$fake_bin:/usr/bin:/bin" \
        HOME="$fixture/home" \
        OPEN_SWITCHER_TEST_FIXTURE_ROOT="$fixture" \
        OPEN_SWITCHER_TEST_SUDO_LOG="$sudo_log" \
        OPEN_SWITCHER_TEST_INSTALL_LOG="$install_log" \
        OPEN_SWITCHER_TEST_UDEVADM_LOG="$udevadm_log" \
        OPEN_SWITCHER_TEST_SETFACL_LOG="$setfacl_log" \
        OPEN_SWITCHER_TEST_RULE_SOURCE="$rule_source" \
        OPEN_SWITCHER_TEST_RULE_TARGET="$rule_target" \
            ./manage.sh bootstrap linux-input 2>&1
    )"
    status=$?
    set -e

    local failures=0

    if [[ "$status" -eq 0 ]]; then
        echo "$override_name: production bootstrap unexpectedly succeeded" >&2
        failures=$((failures + 1))
    fi

    if [[ "$output" != *"$override_name"* ]]; then
        echo "$override_name: rejection output does not name the override" >&2
        failures=$((failures + 1))
    fi

    if [[ "$output" != *"test-only"* ]] ||
        [[ "$output" != *"not allowed for production bootstrap"* ]]; then
        echo "$override_name: rejection output lacks the stable test-only production-boundary message" >&2
        failures=$((failures + 1))
    fi

    local command_name=""
    local command_log=""
    for command_name in sudo install setfacl udevadm; do
        command_log="$fixture/$command_name.log"
        if [[ -s "$command_log" ]]; then
            echo "$override_name: fake $command_name was invoked" >&2
            sed 's/^/  argv: /' "$command_log" >&2
            failures=$((failures + 1))
        fi
    done

    if ! cmp -s "$protected_snapshot" "$protected_target"; then
        echo "$override_name: protected target was modified" >&2
        failures=$((failures + 1))
    fi

    if [[ "$output" == *"Повторная проверка Linux input setup..."* ]]; then
        echo "$override_name: production bootstrap reached the post-bootstrap doctor" >&2
        failures=$((failures + 1))
    fi

    if [[ "$failures" -ne 0 ]]; then
        echo "--- production output for $override_name (status $status) ---" >&2
        if [[ -n "$output" ]]; then
            printf '%s\n' "$output" >&2
        else
            echo "<empty>" >&2
        fi
        return 1
    fi
)

test_production_bootstrap_rejects_test_only_path_overrides() {
    if [[ "$EUID" -eq 0 ]]; then
        echo "SKIP: production bootstrap override boundary requires a non-root EUID" >&2
        return 0
    fi

    local failures=0
    local override_name=""
    local case_output=""
    local -a override_names=(
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR
    )

    for override_name in "${override_names[@]}"; do
        if case_output="$(
            test_production_bootstrap_rejects_override_case "$override_name" 2>&1
        )"; then
            continue
        fi

        echo "production bootstrap override case failed: $override_name" >&2
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    done

    if [[ "$failures" -ne 0 ]]; then
        echo "production bootstrap override cases failed: $failures/${#override_names[@]}" >&2
        return 1
    fi
}

test_production_bootstrap_uses_sanitized_sudo_boundary() (
    set -euo pipefail

    if [[ "$EUID" -eq 0 ]]; then
        echo "SKIP: sanitized sudo boundary requires a non-root EUID" >&2
        return 0
    fi

    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    create_fake_linux_input_commands "$fixture"

    local fake_bin="$fixture/fake-bin"
    local sudo_log="$fixture/sudo.log"
    : >"$sudo_log"
    mkdir -p "$fixture/home"

    local output
    local status
    set +e
    output="$(
        cd "$REPO_ROOT"
        PATH="$fake_bin:/usr/bin:/bin" \
        HOME="$fixture/home" \
        OPEN_SWITCHER_TEST_FIXTURE_ROOT="$fixture" \
        OPEN_SWITCHER_TEST_SUDO_LOG="$sudo_log" \
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT='' \
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES='' \
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR='' \
            ./manage.sh bootstrap linux-input 2>&1
    )"
    status=$?
    set -e

    if [[ "$status" -ne 97 ]]; then
        echo "sanitized sudo boundary did not reach the inert fake sudo (status $status)" >&2
        printf '%s\n' "$output" >&2
        return 1
    fi

    local sudo_argv
    sudo_argv="$(<"$sudo_log")"
    assert_contains "$sudo_argv" "-- /usr/bin/env -i"
    assert_contains "$sudo_argv" "PATH=/usr/sbin:/usr/bin:/sbin:/bin"
    assert_contains "$sudo_argv" "/bin/bash --noprofile --norc -c"

    if [[ "$sudo_argv" == *"bash -lc"* ]]; then
        echo "production bootstrap still uses bash -lc across sudo" >&2
        printf '%s\n' "$sudo_argv" >&2
        return 1
    fi

    local override_name=""
    for override_name in \
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT \
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES \
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR; do
        if [[ "$sudo_argv" == *"${override_name}="* ]]; then
            echo "production bootstrap still forwards $override_name through sudo" >&2
            printf '%s\n' "$sudo_argv" >&2
            return 1
        fi
    done
)

test_root_bootstrap_rejects_override_case() (
    set -euo pipefail

    local override_name="$1"
    local fixture
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT

    create_fake_input_fixture "$fixture"
    create_fake_linux_input_commands "$fixture"

    local protected_target="$fixture/protected-target"
    local protected_snapshot="$fixture/protected-target.before"
    printf 'protected target must remain byte-identical\n' >"$protected_target"
    cp "$protected_target" "$protected_snapshot"
    rm -f "$fixture/dev/uinput"
    ln -s "$protected_target" "$fixture/dev/uinput"

    local rules_dir="$fixture/etc/udev/rules.d"
    local fake_bin="$fixture/fake-bin"
    local install_log="$fixture/install.log"
    local udevadm_log="$fixture/udevadm.log"
    local setfacl_log="$fixture/setfacl.log"
    local rule_source="$REPO_ROOT/dist/udev/80-openswitcher-input.rules"
    local rule_target="$rules_dir/80-openswitcher-input.rules"
    local override_value=""

    case "$override_name" in
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT)
            override_value="$fixture/dev"
            ;;
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES)
            override_value="$fixture/proc/bus/input/devices"
            ;;
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR)
            override_value="$rules_dir"
            ;;
        *)
            echo "unexpected direct root override case: $override_name" >&2
            return 1
            ;;
    esac

    mkdir -p "$rules_dir"
    : >"$install_log"
    : >"$udevadm_log"
    : >"$setfacl_log"

    local output
    local status
    set +e
    output="$(
        unset \
            OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT \
            OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES \
            OPEN_SWITCHER_LINUX_INPUT_RULES_DIR
        printf -v "$override_name" '%s' "$override_value"
        export "$override_name"

        # Keep the vulnerable pre-fix implementation inside the fixture while
        # this regression is RED. The selected override is first in validation
        # order, so the fixed entrypoint must still name it.
        if [[ "$override_name" != "OPEN_SWITCHER_LINUX_INPUT_RULES_DIR" ]]; then
            export OPEN_SWITCHER_LINUX_INPUT_RULES_DIR="$rules_dir"
        fi

        PATH="$fake_bin:/usr/bin:/bin" \
        OPEN_SWITCHER_TEST_FIXTURE_ROOT="$fixture" \
        OPEN_SWITCHER_TEST_INSTALL_LOG="$install_log" \
        OPEN_SWITCHER_TEST_UDEVADM_LOG="$udevadm_log" \
        OPEN_SWITCHER_TEST_SETFACL_LOG="$setfacl_log" \
        OPEN_SWITCHER_TEST_RULE_SOURCE="$rule_source" \
        OPEN_SWITCHER_TEST_RULE_TARGET="$rule_target" \
            openswitcher_linux_input_bootstrap_root "$REPO_ROOT" "$(id -un)" 2>&1
    )"
    status=$?
    set -e

    local failures=0
    if [[ "$status" -eq 0 ]]; then
        echo "$override_name: direct root bootstrap unexpectedly succeeded" >&2
        failures=$((failures + 1))
    fi
    if [[ "$output" != *"$override_name"* ]]; then
        echo "$override_name: direct root rejection output does not name the override" >&2
        failures=$((failures + 1))
    fi
    if [[ "$output" != *"test-only"* ]] ||
        [[ "$output" != *"not allowed for production bootstrap"* ]]; then
        echo "$override_name: direct root rejection lacks the stable boundary message" >&2
        failures=$((failures + 1))
    fi

    local command_name=""
    local command_log=""
    for command_name in install setfacl udevadm; do
        command_log="$fixture/$command_name.log"
        if [[ -s "$command_log" ]]; then
            echo "$override_name: direct root bootstrap invoked fake $command_name" >&2
            sed 's/^/  argv: /' "$command_log" >&2
            failures=$((failures + 1))
        fi
    done

    if ! cmp -s "$protected_snapshot" "$protected_target"; then
        echo "$override_name: direct root bootstrap modified the protected target" >&2
        failures=$((failures + 1))
    fi

    if [[ "$failures" -ne 0 ]]; then
        echo "--- direct root output for $override_name (status $status) ---" >&2
        if [[ -n "$output" ]]; then
            printf '%s\n' "$output" >&2
        else
            echo "<empty>" >&2
        fi
        return 1
    fi
)

test_root_bootstrap_rejects_test_only_path_overrides() {
    local failures=0
    local override_name=""
    local case_output=""
    local -a override_names=(
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR
    )

    for override_name in "${override_names[@]}"; do
        if case_output="$(
            test_root_bootstrap_rejects_override_case "$override_name" 2>&1
        )"; then
            continue
        fi

        echo "direct root bootstrap override case failed: $override_name" >&2
        printf '%s\n' "$case_output" >&2
        failures=$((failures + 1))
    done

    if [[ "$failures" -ne 0 ]]; then
        echo "direct root override cases failed: $failures/${#override_names[@]}" >&2
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
        openswitcher_linux_input_bootstrap_test \
        "$REPO_ROOT" \
        "$target_user" \
        "$fixture/dev" \
        "$fixture/proc/bus/input/devices" \
        "$rules_dir" >/dev/null

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
test_root_bootstrap_rejects_test_only_path_overrides
test_production_bootstrap_rejects_test_only_path_overrides
test_production_bootstrap_uses_sanitized_sudo_boundary

echo "linux_input_setup_test.sh: ok"
