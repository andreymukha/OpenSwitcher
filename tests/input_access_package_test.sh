#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_HELPER="$REPO_ROOT/debian/scripts/open-switcher-input-access-maintenance"
SOURCE_POSTINST="$REPO_ROOT/debian/open-switcher.postinst"
TMP_DIR="$(mktemp -d)"

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

fail() {
    echo "$*" >&2
    exit 1
}

assert_status() {
    local expected="$1"
    shift
    local actual=""

    set +e
    "$@" >/dev/null 2>&1
    actual="$?"
    set -e

    [[ "$actual" == "$expected" ]] \
        || fail "expected status $expected, got $actual: $*"
}

assert_contains() {
    local file="$1"
    local expected="$2"

    grep -Fq -- "$expected" "$file" \
        || fail "expected '$file' to contain: $expected"
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"

    if [[ -e "$file" ]] && grep -Fq -- "$unexpected" "$file"; then
        fail "expected '$file' not to contain: $unexpected"
    fi
}

assert_count() {
    local file="$1"
    local expected="$2"
    local count="$3"
    local actual=""

    actual="$(grep -Fc -- "$expected" "$file" || true)"
    [[ "$actual" == "$count" ]] \
        || fail "expected '$file' to contain '$expected' $count time(s), got $actual"
}

prepare_fake_udevadm() {
    local fixture_root="$1"
    local fake_bin="$fixture_root/bin"

    mkdir -p "$fake_bin"
    cat >"$fake_bin/udevadm" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

printf 'udevadm' >>"$CALL_LOG"
printf ' %s' "$@" >>"$CALL_LOG"
printf '\n' >>"$CALL_LOG"

case "${1:-}" in
    control)
        [[ "${FAIL_PHASE:-}" != "reload" ]] || exit 31
        ;;
    trigger)
        [[ "${FAIL_PHASE:-}" != "trigger" ]] || exit 32
        ;;
    settle)
        [[ "${FAIL_PHASE:-}" != "settle" ]] || exit 33
        ;;
    info)
        case "$*" in
            *event0*)
                printf '%s\n' \
                    'DEVNAME=/dev/input/event0' \
                    'ID_INPUT_KEYBOARD=1'
                ;;
            *uinput*)
                printf '%s\n' 'DEVNAME=/dev/uinput'
                ;;
            *)
                exit 34
                ;;
        esac

        if [[ "${FAIL_PHASE:-}" == "verify" ]]; then
            printf '%s\n' \
                'TAGS=:seat:uaccess:' \
                'CURRENT_TAGS=:seat:uaccess:'
        else
            printf '%s\n' \
                'TAGS=:seat:uaccess:openswitcher-input:' \
                'CURRENT_TAGS=:seat:uaccess:openswitcher-input:'
        fi
        ;;
    *)
        exit 35
        ;;
esac
MOCK
    chmod +x "$fake_bin/udevadm"
}

render_helper() {
    local fixture_root="$1"
    local rendered="$fixture_root/open-switcher-input-access-maintenance"

    sed \
        -e "s|/run/udev/control|$fixture_root/run/udev/control|g" \
        -e "s|/sys|$fixture_root/sys|g" \
        "$SOURCE_HELPER" >"$rendered"
    chmod +x "$rendered"
    printf '%s\n' "$rendered"
}

prepare_live_fixture() {
    local fixture_root="$1"

    prepare_fake_udevadm "$fixture_root"
    mkdir -p \
        "$fixture_root/run/udev" \
        "$fixture_root/sys/class/input" \
        "$fixture_root/sys/class/misc"
    : >"$fixture_root/run/udev/control"
    : >"$fixture_root/sys/class/input/event0"
    : >"$fixture_root/sys/class/misc/uinput"
}

test_helper_rejects_unknown_invocations() {
    local fixture_root="$TMP_DIR/usage"
    local helper=""

    mkdir -p "$fixture_root"
    helper="$(render_helper "$fixture_root")"

    assert_status 2 "$helper"
    assert_status 2 "$helper" unknown
    assert_status 2 "$helper" apply extra
}

test_offline_udev_is_explicit_success_without_commands() {
    local fixture_root="$TMP_DIR/offline"
    local call_log="$fixture_root/calls.log"
    local output="$fixture_root/output.log"
    local helper=""

    mkdir -p "$fixture_root"
    prepare_fake_udevadm "$fixture_root"
    helper="$(render_helper "$fixture_root")"

    PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" \
        "$helper" apply >"$output"

    assert_contains "$output" "open-switcher: udev activation deferred until boot"
    [[ ! -s "$call_log" ]] || fail "offline apply unexpectedly called udevadm"
}

test_live_apply_orders_and_verifies_both_device_classes() {
    local fixture_root="$TMP_DIR/live"
    local call_log="$fixture_root/calls.log"
    local helper=""
    local -a calls=()

    prepare_live_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"

    PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" "$helper" apply
    mapfile -t calls <"$call_log"

    [[ "${#calls[@]}" == 6 ]] || fail "expected 6 udevadm calls, got ${#calls[@]}"
    [[ "${calls[0]}" == "udevadm control --reload-rules" ]] \
        || fail "reload was not first"
    [[ "${calls[1]}" == "udevadm trigger --subsystem-match=input --action=change" ]] \
        || fail "input trigger was not second"
    [[ "${calls[2]}" == \
        "udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change" ]] \
        || fail "uinput trigger was not third"
    [[ "${calls[3]}" == "udevadm settle --timeout=10" ]] \
        || fail "settle was not fourth"
    [[ "${calls[4]}" == *" info "*"event0"* ]] \
        || fail "event device was not verified after settle"
    [[ "${calls[5]}" == *" info "*"uinput"* ]] \
        || fail "uinput was not verified after event devices"
}

test_live_failures_are_not_suppressed() {
    local phase=""
    local status=""

    for phase in reload trigger settle verify; do
        local fixture_root="$TMP_DIR/failure-$phase"
        local call_log="$fixture_root/calls.log"
        local helper=""

        prepare_live_fixture "$fixture_root"
        helper="$(render_helper "$fixture_root")"

        set +e
        PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" FAIL_PHASE="$phase" \
            "$helper" apply >/dev/null 2>&1
        status="$?"
        set -e

        [[ "$status" != 0 ]] || fail "$phase failure was suppressed"
    done
}

test_postinst_applies_before_start_on_required_paths() {
    local fixture_root="$TMP_DIR/postinst-order"
    local call_log="$fixture_root/calls.log"
    local expected_log="$fixture_root/expected.log"
    local postinst="$fixture_root/open-switcher.postinst"
    local maintenance="$fixture_root/maintenance"
    local session_start="$fixture_root/session-start"
    local mode=""

    mkdir -p "$fixture_root/etc/systemd/user"
    cat >"$maintenance" <<'MOCK'
#!/usr/bin/env bash
printf '%s maintenance %s\n' "$CASE_MODE" "$*" >>"$CALL_LOG"
MOCK
    cat >"$session_start" <<'MOCK'
#!/usr/bin/env bash
printf '%s session-start\n' "$CASE_MODE" >>"$CALL_LOG"
MOCK
    chmod +x "$maintenance" "$session_start"

    sed \
        -e "s|/usr/lib/open-switcher/open-switcher-input-access-maintenance|$maintenance|g" \
        -e "s|/usr/lib/open-switcher/open-switcher-user-session-start|$session_start|g" \
        -e "s|/etc/systemd/user|$fixture_root/etc/systemd/user|g" \
        "$SOURCE_POSTINST" >"$postinst"
    chmod +x "$postinst"

    for mode in configure abort-remove abort-deconfigure abort-upgrade; do
        CASE_MODE="$mode" CALL_LOG="$call_log" "$postinst" "$mode"
    done

    cat >"$expected_log" <<'EXPECTED'
configure maintenance apply
configure session-start
abort-remove maintenance apply
abort-remove session-start
abort-deconfigure maintenance apply
abort-deconfigure session-start
abort-upgrade session-start
EXPECTED

    cmp -s "$expected_log" "$call_log" \
        || fail "postinst input-access/start order does not match the required paths"
}

test_postinst_does_not_start_sessions_after_apply_failure() {
    local fixture_root="$TMP_DIR/postinst-failure"
    local call_log="$fixture_root/calls.log"
    local postinst="$fixture_root/open-switcher.postinst"
    local maintenance="$fixture_root/maintenance"
    local session_start="$fixture_root/session-start"

    mkdir -p "$fixture_root/etc/systemd/user"
    cat >"$maintenance" <<'MOCK'
#!/usr/bin/env bash
printf 'maintenance %s\n' "$*" >>"$CALL_LOG"
exit 41
MOCK
    cat >"$session_start" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' 'session-start' >>"$CALL_LOG"
MOCK
    chmod +x "$maintenance" "$session_start"

    sed \
        -e "s|/usr/lib/open-switcher/open-switcher-input-access-maintenance|$maintenance|g" \
        -e "s|/usr/lib/open-switcher/open-switcher-user-session-start|$session_start|g" \
        -e "s|/etc/systemd/user|$fixture_root/etc/systemd/user|g" \
        "$SOURCE_POSTINST" >"$postinst"
    chmod +x "$postinst"

    set +e
    CALL_LOG="$call_log" "$postinst" configure >/dev/null 2>&1
    status="$?"
    set -e

    [[ "$status" == 41 ]] || fail "postinst suppressed apply failure: status $status"
    assert_contains "$call_log" "maintenance apply"
    assert_not_contains "$call_log" "session-start"
}

test_repeated_apply_has_the_same_postcondition() {
    local fixture_root="$TMP_DIR/repeated"
    local call_log="$fixture_root/calls.log"
    local helper=""

    prepare_live_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"

    PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" "$helper" apply
    PATH="$fixture_root/bin:$PATH" CALL_LOG="$call_log" "$helper" apply

    assert_count "$call_log" "udevadm control --reload-rules" 2
    assert_count "$call_log" \
        "udevadm trigger --subsystem-match=input --action=change" 2
    assert_count "$call_log" \
        "udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change" 2
    assert_count "$call_log" "udevadm settle --timeout=10" 2
    assert_count "$call_log" "event0" 2
    assert_count "$call_log" "uinput" 4
}

test_helper_rejects_unknown_invocations
test_offline_udev_is_explicit_success_without_commands
test_live_apply_orders_and_verifies_both_device_classes
test_live_failures_are_not_suppressed
test_postinst_applies_before_start_on_required_paths
test_postinst_does_not_start_sessions_after_apply_failure
test_repeated_apply_has_the_same_postcondition

echo "input_access_package_test.sh: ok"
