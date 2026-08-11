#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANAGE_SH="$REPO_ROOT/manage.sh"

failures=0

fail() {
    echo "manage_dev_retirement_test.sh: $*" >&2
    failures=$((failures + 1))
}

assert_contains() {
    local text="$1"
    local expected="$2"

    if [[ "$text" != *"$expected"* ]]; then
        fail "expected output to contain: $expected"
    fi
}

assert_not_contains() {
    local text="$1"
    local unexpected="$2"

    if [[ "$text" == *"$unexpected"* ]]; then
        fail "expected output not to contain: $unexpected"
    fi
}

assert_source_absent() {
    local pattern="$1"
    local description="$2"
    shift 2

    if grep -Ern -- "$pattern" "$@" >/dev/null; then
        fail "retired source remains: $description"
    fi
}

# This barrier must run before any old lifecycle command. On the vulnerable
# implementation it fails safely, without starting, scanning, or signalling a
# process on the host.
assert_source_absent \
    'RUN_DIR=|LOG_DIR=|PID_DIR=|[A-Z_]+_PIDFILE=' \
    'dev .run/PID state' \
    "$MANAGE_SH"
assert_source_absent \
    '^(start_component|stop_component|find_component_pids|is_running|pidfile_for|logfile_for)\(\)' \
    'direct process lifecycle functions' \
    "$MANAGE_SH"
assert_source_absent \
    '^[[:space:]]*(nohup|kill)([[:space:]]|$)' \
    'direct process launch or signal command' \
    "$MANAGE_SH"
assert_source_absent \
    'OPEN_SWITCHER_RUNTIME_MODE|RuntimeMode::Dev|is_dev_runtime_mode' \
    'Rust or shell dev-mode bypass' \
    "$MANAGE_SH" \
    "$REPO_ROOT/src"

if [[ "$failures" -ne 0 ]]; then
    echo "manage_dev_retirement_test.sh: static safety barrier stopped before lifecycle commands" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

FIXTURE="$TMP_DIR/repo"
MOCK_BIN="$TMP_DIR/bin"
CALL_LOG="$TMP_DIR/calls.log"
mkdir -p "$FIXTURE/scripts" "$MOCK_BIN" "$TMP_DIR/home"

cp "$MANAGE_SH" "$FIXTURE/manage.sh"
cp "$REPO_ROOT/scripts/linux_input_setup.sh" "$FIXTURE/scripts/linux_input_setup.sh"
cp "$REPO_ROOT/scripts/wayland_diagnostics.sh" "$FIXTURE/scripts/wayland_diagnostics.sh"
cp "$REPO_ROOT/rust-toolchain.toml" "$FIXTURE/rust-toolchain.toml"
chmod +x "$FIXTURE/manage.sh"
: >"$CALL_LOG"

for command_name in systemctl cargo nohup ps; do
    printf '%s\n' \
        '#!/bin/sh' \
        'printf "%s\\n" "$(basename "$0") $*" >>"$CALL_LOG"' \
        'exit 97' \
        >"$MOCK_BIN/$command_name"
    chmod +x "$MOCK_BIN/$command_name"
done

run_manage() {
    (
        cd "$FIXTURE"
        PATH="$MOCK_BIN:$PATH" \
            CALL_LOG="$CALL_LOG" \
            HOME="$TMP_DIR/home" \
            XDG_CONFIG_HOME="$TMP_DIR/config" \
            XDG_DATA_HOME="$TMP_DIR/data" \
            ./manage.sh "$@"
    ) 2>&1
}

assert_retired_command() {
    local output status

    set +e
    output="$(run_manage "$@")"
    status=$?
    set -e

    if [[ "$status" -eq 0 ]]; then
        fail "retired command unexpectedly succeeded: $*"
    fi
    assert_contains "$output" "Прямой dev-runtime удалён"
    assert_contains "$output" "./manage.sh package deb"
}

assert_retired_command dev help
assert_retired_command dev build

for command_name in start stop restart status logs settings; do
    assert_retired_command "$command_name"
done

set +e
help_output="$(run_manage --help)"
help_status=$?
set -e

if [[ "$help_status" -ne 0 ]]; then
    fail "--help exited with status $help_status"
fi
assert_contains "$help_output" "./manage.sh build"
assert_contains "$help_output" "./manage.sh package <команда>"
assert_contains "$help_output" "./manage.sh systemd <команда>"
assert_contains "$help_output" "./manage.sh doctor"
assert_not_contains "$help_output" "./manage.sh dev"
assert_not_contains "$help_output" "dev-команды"

if [[ -s "$CALL_LOG" ]]; then
    fail "retired commands invoked an external lifecycle/build command"
    sed 's/^/  invoked: /' "$CALL_LOG" >&2
fi

if [[ -e "$FIXTURE/.run" ]]; then
    fail "retired commands created .run state"
fi

if [[ "$failures" -ne 0 ]]; then
    exit 1
fi

echo "manage_dev_retirement_test.sh: ok"
