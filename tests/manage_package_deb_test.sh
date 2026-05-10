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
DIST_DEB="$PACKAGE_OUTPUT_DIR/open-switcher_${VERSION}_${ARCH}.deb"
DIST_DDEB="$PACKAGE_OUTPUT_DIR/open-switcher-dbgsym_${VERSION}_${ARCH}.ddeb"

cleanup() {
    rm -rf "$TMP_DIR"
    rm -f "$PARENT_DEB" "$PARENT_DDEB" "$DIST_DEB" "$DIST_DDEB"
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

[[ -f "$DIST_DEB" ]]
[[ -f "$DIST_DDEB" ]]

echo "manage_package_deb_test.sh: ok"
