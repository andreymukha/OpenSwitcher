#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_HELPER="$REPO_ROOT/debian/scripts/open-switcher-input-access-maintenance"
SOURCE_POSTINST="$REPO_ROOT/debian/open-switcher.postinst"
SOURCE_POSTRM="$REPO_ROOT/debian/open-switcher.postrm"
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

        if [[ "${STICKY_ONLY_TAGS:-}" == "1" ]]; then
            printf '%s\n' \
                'TAGS=:seat:uaccess:openswitcher-input:'
        elif [[ "${FAIL_PHASE:-}" == "verify" ]]; then
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
        -e "s|/run/open-switcher|$fixture_root/run/open-switcher|g" \
        -e "s|/dev|$fixture_root/dev|g" \
        -e "s|/sys|$fixture_root/sys|g" \
        -e 's/\[ -c /[ -f /g' \
        -e 's/\[ ! -c /[ ! -f /g' \
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

prepare_acl_fixture() {
    local fixture_root="$1"
    local fake_bin="$fixture_root/bin"

    mkdir -p \
        "$fake_bin" \
        "$fixture_root/run/udev" \
        "$fixture_root/sys/class/input" \
        "$fixture_root/sys/class/misc" \
        "$fixture_root/dev/input" \
        "$fixture_root/acl-state"
    : >"$fixture_root/run/udev/control"
    : >"$fixture_root/sys/class/input/event4"
    : >"$fixture_root/sys/class/input/event5"
    : >"$fixture_root/sys/class/misc/uinput"
    : >"$fixture_root/dev/input/event4"
    : >"$fixture_root/dev/input/event5"
    : >"$fixture_root/dev/uinput"

    cat >"$fake_bin/udevadm" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf 'udevadm' >>"$CALL_LOG"
printf ' %s' "$@" >>"$CALL_LOG"
printf '\n' >>"$CALL_LOG"

case "${1:-}" in
    control)
        [[ "${FAIL_POSTRM_PHASE:-}" != "reload" ]] || exit 59
        ;;
    trigger)
        [[ "${FAIL_POSTRM_PHASE:-}" != "trigger" ]] || exit 60
        ;;
    settle)
        [[ "${FAIL_POSTRM_PHASE:-}" != "settle" ]] || exit 61
        ;;
    info)
        is_cleanup=0
        [[ "$*" != *"--name"* ]] || is_cleanup=1

        case "$*" in
            *event4*)
                printf '%s\n' \
                    "DEVNAME=$FIXTURE_ROOT/dev/input/event4" \
                    'ID_INPUT_KEYBOARD=1' \
                    'ID_SEAT=seat0'
                ;;
            *event5*)
                printf '%s\n' \
                    "DEVNAME=$FIXTURE_ROOT/dev/input/event5" \
                    'ID_INPUT_KEYBOARD=1' \
                    'ID_SEAT=seat1'
                ;;
            *uinput*)
                printf '%s\n' "DEVNAME=$FIXTURE_ROOT/dev/uinput"
                ;;
            *)
                exit 51
                ;;
        esac

        if ((is_cleanup)) && [[ "$*" == *event5* ]]; then
            printf '%s\n' \
                'TAGS=:seat:uaccess:' \
                'CURRENT_TAGS=:seat:uaccess:'
        elif ((is_cleanup)) && [[ "${STICKY_ONLY_TAGS:-}" == "1" ]]; then
            printf '%s\n' \
                'TAGS=:seat:uaccess:openswitcher-input:'
        elif ((is_cleanup)); then
            printf '%s\n' \
                'TAGS=:seat:' \
                'CURRENT_TAGS=:seat:'
        else
            printf '%s\n' \
                'TAGS=:seat:uaccess:openswitcher-input:' \
                'CURRENT_TAGS=:seat:uaccess:openswitcher-input:'
        fi
        ;;
    *)
        exit 52
        ;;
esac
MOCK

    cat >"$fake_bin/loginctl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf 'loginctl %s\n' "$*" >>"$CALL_LOG"

case "${1:-} ${2:-}" in
    "show-seat seat0") printf '%s\n' 'session-seat0' ;;
    "show-seat seat1") printf '%s\n' 'session-seat1' ;;
    "show-session session-seat0") printf '%s\n' '1000' ;;
    "show-session session-seat1") printf '%s\n' '1001' ;;
    *) exit 53 ;;
esac
MOCK

    cat >"$fake_bin/stat" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
path="${!#}"

case "$*" in
    *"%u"*)
        printf '%s\n' '0'
        ;;
    *"%a"*)
        case "$path" in
            *input-access-acl.tsv) printf '%s\n' '600' ;;
            *) printf '%s\n' '700' ;;
        esac
        ;;
    *"%t:%T"*)
        case "$path" in
            *event4)
                if [[ "${CHANGED_EVENT4:-}" == "1" ]]; then
                    printf '%s\n' '9:9'
                else
                    printf '%s\n' '1:4'
                fi
                ;;
            *event5) printf '%s\n' '1:5' ;;
            *uinput) printf '%s\n' '1:6' ;;
            *) exit 54 ;;
        esac
        ;;
    *)
        exit 55
        ;;
esac
MOCK

    cat >"$fake_bin/getfacl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
path="${!#}"
name="$(basename "$path")"
printf 'getfacl %s\n' "$*" >>"$CALL_LOG"

printf '%s\n' 'user::rw-'
case "$name" in
    event4)
        [[ -e "$ACL_STATE_DIR/event4-1000-removed" ]] \
            || printf '%s\n' 'user:1000:rw-'
        printf '%s\n' 'user:2000:r--' 'group:3000:r--'
        ;;
    event5)
        [[ -e "$ACL_STATE_DIR/event5-1001-removed" ]] \
            || printf '%s\n' 'user:1001:rw-'
        printf '%s\n' 'user:2001:r--' 'group:3001:r--'
        ;;
    uinput)
        [[ -e "$ACL_STATE_DIR/uinput-1000-removed" ]] \
            || printf '%s\n' 'user:1000:rw-'
        printf '%s\n' 'user:2000:r--' 'group:3000:r--'
        ;;
    *)
        exit 56
        ;;
esac
printf '%s\n' 'group::---' 'mask::rw-' 'other::---'
MOCK

    cat >"$fake_bin/setfacl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
path="${!#}"
entry=""
previous=""
for argument in "$@"; do
    if [[ "$previous" == "-x" ]]; then
        entry="$argument"
        break
    fi
    previous="$argument"
done

printf 'setfacl %s\n' "$*" >>"$CALL_LOG"
[[ "$entry" =~ ^u:([0-9]+)$ ]] || exit 57
[[ "${FAIL_SETFACL:-}" != "1" ]] || exit 58
touch "$ACL_STATE_DIR/$(basename "$path")-${BASH_REMATCH[1]}-removed"
MOCK

    cat >"$fake_bin/sleep" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf 'sleep %s\n' "$*" >>"$CALL_LOG"

if [[ "${LATE_ACL_READD:-}" == "1" \
    && ! -e "$ACL_STATE_DIR/late-uinput-readd-fired" ]]; then
    rm -f "$ACL_STATE_DIR/uinput-1000-removed"
    touch "$ACL_STATE_DIR/late-uinput-readd-fired"
fi
MOCK

    chmod +x \
        "$fake_bin/udevadm" \
        "$fake_bin/loginctl" \
        "$fake_bin/stat" \
        "$fake_bin/getfacl" \
        "$fake_bin/setfacl" \
        "$fake_bin/sleep"
}

render_postrm() {
    local fixture_root="$1"
    local rendered="$fixture_root/open-switcher.postrm"

    sed \
        -e "s|/run/udev/control|$fixture_root/run/udev/control|g" \
        -e "s|/run/open-switcher|$fixture_root/run/open-switcher|g" \
        -e "s|/dev|$fixture_root/dev|g" \
        -e "s|/sys|$fixture_root/sys|g" \
        -e 's/\[ -c /[ -f /g' \
        -e 's/\[ ! -c /[ ! -f /g' \
        "$SOURCE_POSTRM" >"$rendered"
    chmod +x "$rendered"
    printf '%s\n' "$rendered"
}

run_acl_command() {
    local fixture_root="$1"
    shift

    PATH="$fixture_root/bin:$PATH" \
        CALL_LOG="$fixture_root/calls.log" \
        FIXTURE_ROOT="$fixture_root" \
        ACL_STATE_DIR="$fixture_root/acl-state" \
        "$@"
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

test_sticky_historical_tags_do_not_satisfy_live_verification() {
    local fixture_root="$TMP_DIR/sticky-live-tags"
    local call_log="$fixture_root/calls.log"
    local helper=""
    local status=""

    prepare_live_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"

    set +e
    PATH="$fixture_root/bin:$PATH" \
        CALL_LOG="$call_log" \
        STICKY_ONLY_TAGS=1 \
        "$helper" apply >/dev/null 2>&1
    status="$?"
    set -e

    [[ "$status" != 0 ]] \
        || fail "sticky historical tags passed live udev verification"
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

test_capture_records_only_verified_active_seat_owners() {
    local fixture_root="$TMP_DIR/capture"
    local helper=""
    local manifest="$fixture_root/run/open-switcher/input-access-acl.tsv"
    local expected="$fixture_root/expected.tsv"

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture

    printf '%s\t%s\t%s\n' \
        "$fixture_root/dev/input/event4" '1:4' '1000' \
        "$fixture_root/dev/input/event5" '1:5' '1001' \
        "$fixture_root/dev/uinput" '1:6' '1000' >"$expected"

    cmp -s "$expected" "$manifest" \
        || fail "capture manifest contains an unverified path, devnum, or uid"
    [[ "$(stat -c %a "$manifest")" == "600" ]] \
        || fail "capture manifest is not private"
    assert_not_contains "$manifest" "2000"
    assert_not_contains "$manifest" "3000"
}

test_remove_cleanup_is_narrow_and_idempotent() {
    local fixture_root="$TMP_DIR/cleanup"
    local helper=""
    local postrm=""
    local manifest="$fixture_root/run/open-switcher/input-access-acl.tsv"
    local call_log="$fixture_root/calls.log"

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    postrm="$(render_postrm "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture
    : >"$call_log"

    run_acl_command "$fixture_root" "$postrm" remove

    assert_contains "$call_log" \
        "setfacl -n -x u:1000 -- $fixture_root/dev/input/event4"
    assert_contains "$call_log" \
        "setfacl -n -x u:1000 -- $fixture_root/dev/uinput"
    assert_not_contains "$call_log" \
        "setfacl -n -x u:1001 -- $fixture_root/dev/input/event5"
    assert_not_contains "$call_log" "u:2000"
    assert_not_contains "$call_log" "u:2001"
    assert_not_contains "$call_log" "g:3000"
    assert_not_contains "$call_log" "g:3001"
    [[ ! -e "$manifest" ]] || fail "successful cleanup retained its manifest"

    assert_count "$call_log" "setfacl " 2

    : >"$call_log"
    run_acl_command "$fixture_root" "$postrm" purge
    [[ ! -s "$call_log" ]] \
        || fail "manifest-free purge retriggered udev or ACL cleanup"
}

test_cleanup_ignores_sticky_historical_uaccess_tags() {
    local fixture_root="$TMP_DIR/cleanup-sticky-tags"
    local helper=""
    local postrm=""
    local call_log="$fixture_root/calls.log"

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    postrm="$(render_postrm "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture
    : >"$call_log"

    PATH="$fixture_root/bin:$PATH" \
        CALL_LOG="$call_log" \
        FIXTURE_ROOT="$fixture_root" \
        ACL_STATE_DIR="$fixture_root/acl-state" \
        STICKY_ONLY_TAGS=1 \
        "$postrm" remove

    assert_contains "$call_log" \
        "setfacl -n -x u:1000 -- $fixture_root/dev/input/event4"
    assert_contains "$call_log" \
        "setfacl -n -x u:1000 -- $fixture_root/dev/uinput"
}

test_changed_device_identity_is_never_mutated() {
    local fixture_root="$TMP_DIR/changed-devnum"
    local helper=""
    local postrm=""
    local call_log="$fixture_root/calls.log"
    local output_log="$fixture_root/output.log"

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    postrm="$(render_postrm "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture
    : >"$call_log"

    PATH="$fixture_root/bin:$PATH" \
        CALL_LOG="$call_log" \
        FIXTURE_ROOT="$fixture_root" \
        ACL_STATE_DIR="$fixture_root/acl-state" \
        CHANGED_EVENT4=1 \
        "$postrm" remove >"$output_log" 2>&1

    assert_not_contains "$call_log" \
        "setfacl -n -x u:1000 -- $fixture_root/dev/input/event4"
    assert_contains "$output_log" "open-switcher: warning:"
}

test_capture_rejects_unsafe_runtime_directory() {
    local fixture_root="$TMP_DIR/unsafe-runtime"
    local helper=""
    local output_log="$fixture_root/output.log"
    local status=""

    prepare_acl_fixture "$fixture_root"
    mkdir -p "$fixture_root/attacker"
    ln -s "$fixture_root/attacker" "$fixture_root/run/open-switcher"
    helper="$(render_helper "$fixture_root")"

    set +e
    run_acl_command "$fixture_root" "$helper" capture >"$output_log" 2>&1
    status="$?"
    set -e

    [[ "$status" != 0 ]] || fail "capture accepted a runtime directory symlink"
    [[ ! -e "$fixture_root/attacker/input-access-acl.tsv" ]] \
        || fail "capture followed an unsafe runtime directory symlink"
}

test_failed_acl_mutation_retains_manifest_for_retry() {
    local fixture_root="$TMP_DIR/cleanup-retry"
    local helper=""
    local postrm=""
    local manifest="$fixture_root/run/open-switcher/input-access-acl.tsv"
    local status=""

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    postrm="$(render_postrm "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture

    set +e
    PATH="$fixture_root/bin:$PATH" \
        CALL_LOG="$fixture_root/calls.log" \
        FIXTURE_ROOT="$fixture_root" \
        ACL_STATE_DIR="$fixture_root/acl-state" \
        FAIL_SETFACL=1 \
        "$postrm" remove >/dev/null 2>&1
    status="$?"
    set -e

    [[ "$status" != 0 ]] || fail "postrm suppressed a setfacl failure"
    [[ -f "$manifest" ]] || fail "failed cleanup discarded its retry manifest"

    run_acl_command "$fixture_root" "$postrm" remove
    [[ ! -e "$manifest" ]] || fail "successful cleanup retry retained its manifest"
}

test_late_uaccess_acl_readd_is_reconciled_before_manifest_removal() {
    local fixture_root="$TMP_DIR/cleanup-late-readd"
    local helper=""
    local postrm=""
    local manifest="$fixture_root/run/open-switcher/input-access-acl.tsv"
    local call_log="$fixture_root/calls.log"

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    postrm="$(render_postrm "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture
    : >"$call_log"

    PATH="$fixture_root/bin:$PATH" \
        CALL_LOG="$call_log" \
        FIXTURE_ROOT="$fixture_root" \
        ACL_STATE_DIR="$fixture_root/acl-state" \
        LATE_ACL_READD=1 \
        "$postrm" remove

    assert_contains "$call_log" "sleep 1"
    assert_count "$call_log" \
        "setfacl -n -x u:1000 -- $fixture_root/dev/uinput" 2
    [[ -e "$fixture_root/acl-state/uinput-1000-removed" ]] \
        || fail "late logind ACL re-add was not removed"
    [[ ! -e "$manifest" ]] \
        || fail "successful late ACL reconciliation retained its manifest"
}

test_postrm_udev_failures_abort_before_acl_mutation() {
    local phase=""
    local status=""

    for phase in reload trigger settle; do
        local fixture_root="$TMP_DIR/postrm-$phase-failure"
        local helper=""
        local postrm=""
        local manifest="$fixture_root/run/open-switcher/input-access-acl.tsv"
        local call_log="$fixture_root/calls.log"

        prepare_acl_fixture "$fixture_root"
        helper="$(render_helper "$fixture_root")"
        postrm="$(render_postrm "$fixture_root")"
        run_acl_command "$fixture_root" "$helper" capture
        : >"$call_log"

        set +e
        PATH="$fixture_root/bin:$PATH" \
            CALL_LOG="$call_log" \
            FIXTURE_ROOT="$fixture_root" \
            ACL_STATE_DIR="$fixture_root/acl-state" \
            FAIL_POSTRM_PHASE="$phase" \
            "$postrm" remove >/dev/null 2>&1
        status="$?"
        set -e

        [[ "$status" != 0 ]] || fail "postrm suppressed $phase failure"
        [[ -f "$manifest" ]] || fail "postrm discarded manifest after $phase failure"
        assert_not_contains "$call_log" "setfacl "
    done
}

test_upgrade_never_runs_remove_cleanup() {
    local fixture_root="$TMP_DIR/no-upgrade-cleanup"
    local helper=""
    local postrm=""
    local manifest="$fixture_root/run/open-switcher/input-access-acl.tsv"
    local call_log="$fixture_root/calls.log"

    prepare_acl_fixture "$fixture_root"
    helper="$(render_helper "$fixture_root")"
    postrm="$(render_postrm "$fixture_root")"
    run_acl_command "$fixture_root" "$helper" capture
    : >"$call_log"

    run_acl_command "$fixture_root" "$postrm" upgrade

    [[ -f "$manifest" ]] || fail "upgrade consumed the remove-only ACL manifest"
    [[ ! -s "$call_log" ]] || fail "upgrade unexpectedly changed udev or ACL state"
}

test_helper_rejects_unknown_invocations
test_offline_udev_is_explicit_success_without_commands
test_live_apply_orders_and_verifies_both_device_classes
test_live_failures_are_not_suppressed
test_sticky_historical_tags_do_not_satisfy_live_verification
test_postinst_applies_before_start_on_required_paths
test_postinst_does_not_start_sessions_after_apply_failure
test_repeated_apply_has_the_same_postcondition
test_capture_records_only_verified_active_seat_owners
test_remove_cleanup_is_narrow_and_idempotent
test_cleanup_ignores_sticky_historical_uaccess_tags
test_changed_device_identity_is_never_mutated
test_capture_rejects_unsafe_runtime_directory
test_failed_acl_mutation_retains_manifest_for_retry
test_late_uaccess_acl_readd_is_reconciled_before_manifest_removal
test_postrm_udev_failures_abort_before_acl_mutation
test_upgrade_never_runs_remove_cleanup

echo "input_access_package_test.sh: ok"
