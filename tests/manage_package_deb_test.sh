#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
MOCK_BIN="$TMP_DIR/bin"
CALL_LOG="$TMP_DIR/calls.log"
OUTPUT_LOG="$TMP_DIR/output.log"
PACKAGE_OUTPUT_DIR="$TMP_DIR/packages"
VERSION="0.1.0-test"
ARCH="amd64"
PARENT_DIR="$(dirname "$REPO_ROOT")"
PARENT_DEB="$PARENT_DIR/open-switcher_${VERSION}_${ARCH}.deb"
PARENT_DDEB="$PARENT_DIR/open-switcher-dbgsym_${VERSION}_${ARCH}.ddeb"
PARENT_CHANGES="$PARENT_DIR/open-switcher_${VERSION}_${ARCH}.changes"
PARENT_BUILDINFO="$PARENT_DIR/open-switcher_${VERSION}_${ARCH}.buildinfo"
DIST_DEB="$PACKAGE_OUTPUT_DIR/open-switcher_${VERSION}_${ARCH}.deb"
DIST_DDEB="$PACKAGE_OUTPUT_DIR/open-switcher-dbgsym_${VERSION}_${ARCH}.ddeb"
SYSTEMD_PROFILE="h06-package-test-$$"
SYSTEMD_TARGET_DIR="$REPO_ROOT/target/$SYSTEMD_PROFILE"

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

cleanup() {
    rm -rf "$TMP_DIR"
    rm -rf "$SYSTEMD_TARGET_DIR"
    rm -f "$PARENT_DEB" "$PARENT_DDEB" "$PARENT_CHANGES" "$PARENT_BUILDINFO" "$DIST_DEB" "$DIST_DDEB"
}
trap cleanup EXIT

mkdir -p "$MOCK_BIN"

cat >"$MOCK_BIN/rustup" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "rustup $*" >>"$CALL_LOG"
if [[ "$1" == "run" && "$2" == "1.95.0" && "$3" == "cargo" && "$4" == "--version" ]]; then
    echo "cargo 1.95.0 (test)"
    exit 0
fi
exit 1
MOCK

cat >"$MOCK_BIN/dpkg-query" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "dpkg-query $*" >>"$CALL_LOG"
echo "install ok installed"
MOCK

cat >"$MOCK_BIN/dpkg-parsechangelog" <<MOCK
#!/usr/bin/env bash
set -euo pipefail
echo "dpkg-parsechangelog \$*" >>"\$CALL_LOG"
if [[ "\$1" == "-S" && "\$2" == "Version" ]]; then
    echo "$VERSION"
    exit 0
fi
exit 1
MOCK

cat >"$MOCK_BIN/dpkg-architecture" <<MOCK
#!/usr/bin/env bash
set -euo pipefail
echo "dpkg-architecture \$*" >>"\$CALL_LOG"
if [[ "\$1" == "-qDEB_HOST_ARCH" ]]; then
    echo "$ARCH"
    exit 0
fi
exit 1
MOCK

cat >"$MOCK_BIN/dpkg-buildpackage" <<MOCK
#!/usr/bin/env bash
set -euo pipefail
echo "dpkg-buildpackage \$*" >>"\$CALL_LOG"
printf 'deb' >"$PARENT_DEB"
printf 'ddeb' >"$PARENT_DDEB"
printf 'changes' >"$PARENT_CHANGES"
printf 'buildinfo' >"$PARENT_BUILDINFO"
MOCK

cat >"$MOCK_BIN/desktop-file-validate" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "desktop-file-validate $*" >>"$CALL_LOG"
MOCK

cat >"$MOCK_BIN/git" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "git $*" >>"$CALL_LOG"
if [[ "$1" == "diff" && "$2" == "--check" ]]; then
    exit 0
fi
exit 1
MOCK

cat >"$MOCK_BIN/lintian" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "lintian $*" >>"$CALL_LOG"
MOCK

cat >"$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "systemctl $*" >>"$CALL_LOG"

if [[ "$*" == *" is-enabled "* ]]; then
    exit 1
fi
if [[ "$*" == *" show "*open-switcher-xtest-guardian.service* ]]; then
    echo "inactive"
fi
MOCK

chmod +x "$MOCK_BIN"/*

PATH="$MOCK_BIN:$PATH" \
    CALL_LOG="$CALL_LOG" \
    OPEN_SWITCHER_PACKAGE_OUTPUT_DIR="$PACKAGE_OUTPUT_DIR" \
    "$REPO_ROOT/manage.sh" package deb >"$OUTPUT_LOG" 2>&1
cat "$OUTPUT_LOG"

grep -Fq "rustup run 1.95.0 cargo --version" "$CALL_LOG"
grep -Fq "dpkg-buildpackage -us -uc -b -d -tc" "$CALL_LOG"
grep -Fq "desktop-file-validate debian/open-switcher.desktop debian/autostart/open-switcher-autostart.desktop" "$CALL_LOG"
grep -Fq "git diff --check" "$CALL_LOG"
grep -Fq "lintian $DIST_DEB" "$CALL_LOG"
grep -Fq "sudo apt install $DIST_DEB" "$OUTPUT_LOG"
grep -Fq "The .ddeb file is optional and only needed for debugging." "$OUTPUT_LOG"
grep -Fq '$(CARGO) test --locked -- --test-threads=1' "$REPO_ROOT/debian/rules"
grep -Fq \
    '$(CARGO) test --locked --features settings-ui --lib -- --test-threads=1' \
    "$REPO_ROOT/debian/rules"

[[ -f "$DIST_DEB" ]]
[[ -f "$DIST_DDEB" ]]
[[ ! -e "$PARENT_DEB" ]]
[[ ! -e "$PARENT_DDEB" ]]
[[ ! -e "$PARENT_CHANGES" ]]
[[ ! -e "$PARENT_BUILDINFO" ]]

SYSTEMD_HOME="$TMP_DIR/home"
SYSTEMD_CONFIG_HOME="$TMP_DIR/config"
SYSTEMD_DATA_HOME="$TMP_DIR/data"
SYSTEMD_BIN_DIR="$TMP_DIR/systemd-bin"
SYSTEMD_CALL_LOG="$TMP_DIR/systemd-calls.log"
SYSTEMD_OUTPUT_LOG="$TMP_DIR/systemd-output.log"
SYSTEMD_UNIT_DIR="$SYSTEMD_CONFIG_HOME/systemd/user"

mkdir -p "$SYSTEMD_TARGET_DIR" "$SYSTEMD_HOME"
for binary in open-switcher open-switcher-tray open-switcher-settings; do
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$SYSTEMD_TARGET_DIR/$binary"
    chmod +x "$SYSTEMD_TARGET_DIR/$binary"
done

PATH="$MOCK_BIN:$PATH" \
    CALL_LOG="$SYSTEMD_CALL_LOG" \
    HOME="$SYSTEMD_HOME" \
    XDG_CONFIG_HOME="$SYSTEMD_CONFIG_HOME" \
    XDG_DATA_HOME="$SYSTEMD_DATA_HOME" \
    OPEN_SWITCHER_PROFILE="$SYSTEMD_PROFILE" \
    OPEN_SWITCHER_SYSTEMD_BINDIR="$SYSTEMD_BIN_DIR" \
    "$REPO_ROOT/manage.sh" systemd install >"$SYSTEMD_OUTPUT_LOG" 2>&1

[[ -f "$SYSTEMD_UNIT_DIR/open-switcher-xtest-guardian.socket" ]]
[[ -f "$SYSTEMD_UNIT_DIR/open-switcher-xtest-guardian.service" ]]
grep -Fq \
    "ExecStart=$SYSTEMD_BIN_DIR/open-switcher-daemon --internal-xtest-guardian-v1" \
    "$SYSTEMD_UNIT_DIR/open-switcher-xtest-guardian.service"
grep -Fq "Wants=open-switcher-xtest-guardian.socket" \
    "$SYSTEMD_UNIT_DIR/open-switcher-daemon.service"

: >"$SYSTEMD_CALL_LOG"
PATH="$MOCK_BIN:$PATH" \
    CALL_LOG="$SYSTEMD_CALL_LOG" \
    HOME="$SYSTEMD_HOME" \
    XDG_CONFIG_HOME="$SYSTEMD_CONFIG_HOME" \
    XDG_DATA_HOME="$SYSTEMD_DATA_HOME" \
    OPEN_SWITCHER_PROFILE="$SYSTEMD_PROFILE" \
    OPEN_SWITCHER_SYSTEMD_BINDIR="$SYSTEMD_BIN_DIR" \
    "$REPO_ROOT/manage.sh" systemd stop >"$SYSTEMD_OUTPUT_LOG" 2>&1

assert_occurs_in_order "$SYSTEMD_CALL_LOG" \
    "systemctl --user stop open-switcher-tray.service" \
    "systemctl --user stop open-switcher-daemon.service" \
    "systemctl --user show" \
    "systemctl --user stop open-switcher-xtest-guardian.socket" \
    "systemctl --user stop open-switcher-xtest-guardian.service" \
    "systemctl --user daemon-reload"

: >"$SYSTEMD_CALL_LOG"
PATH="$MOCK_BIN:$PATH" \
    CALL_LOG="$SYSTEMD_CALL_LOG" \
    HOME="$SYSTEMD_HOME" \
    XDG_CONFIG_HOME="$SYSTEMD_CONFIG_HOME" \
    XDG_DATA_HOME="$SYSTEMD_DATA_HOME" \
    OPEN_SWITCHER_PROFILE="$SYSTEMD_PROFILE" \
    OPEN_SWITCHER_SYSTEMD_BINDIR="$SYSTEMD_BIN_DIR" \
    "$REPO_ROOT/manage.sh" systemd restart >"$SYSTEMD_OUTPUT_LOG" 2>&1

assert_occurs_in_order "$SYSTEMD_CALL_LOG" \
    "systemctl --user stop open-switcher-tray.service" \
    "systemctl --user stop open-switcher-daemon.service" \
    "systemctl --user stop open-switcher-xtest-guardian.socket" \
    "systemctl --user stop open-switcher-xtest-guardian.service" \
    "systemctl --user daemon-reload" \
    "systemctl --user start open-switcher-daemon.service" \
    "systemctl --user start open-switcher-tray.service"

echo "manage_package_deb_test.sh: ok"
