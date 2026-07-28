#!/usr/bin/env bash
set -euo pipefail

print_linux_input_bootstrap_migration() {
    echo "Source-tree Linux input bootstrap is disabled." >&2
    echo "Linux input setup helpers and assets were not executed or installed with elevated privileges." >&2
    echo "Do not run ./manage.sh with sudo." >&2
    echo "Build the canonical package:" >&2
    echo "  ./manage.sh package deb" >&2
    echo 'Then run the exact `sudo apt install <artifact>` command printed by the build.' >&2
    echo 'Use `--reinstall` only when the same package version is already installed.' >&2
    echo "Sign out and sign in again, then verify:" >&2
    echo "  ./manage.sh doctor" >&2
    echo "Privileged Linux input setup and system configuration were not changed." >&2
}

if [[ "${1:-}" == "bootstrap" ]] && [[ "${2:-}" == "linux-input" ]]; then
    print_linux_input_bootstrap_migration
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_DIR="$SCRIPT_DIR/.run"
LOG_DIR="$RUN_DIR/logs"
PID_DIR="$RUN_DIR/pids"
LINUX_INPUT_HELPER="$SCRIPT_DIR/scripts/linux_input_setup.sh"
WAYLAND_DIAGNOSTICS_HELPER="$SCRIPT_DIR/scripts/wayland_diagnostics.sh"
RUST_TOOLCHAIN_FILE="$SCRIPT_DIR/rust-toolchain.toml"
PACKAGE_OUTPUT_DIR="${OPEN_SWITCHER_PACKAGE_OUTPUT_DIR:-$SCRIPT_DIR/dist/packages}"

PROFILE="${OPEN_SWITCHER_PROFILE:-debug}"
TARGET_DIR="$SCRIPT_DIR/target/$PROFILE"
DEV_RUNTIME_MODE="dev"

DAEMON_BIN="$TARGET_DIR/open-switcher"
TRAY_BIN="$TARGET_DIR/open-switcher-tray"
SETTINGS_BIN="$TARGET_DIR/open-switcher-settings"

DAEMON_PIDFILE="$PID_DIR/daemon.pid"
TRAY_PIDFILE="$PID_DIR/tray.pid"
SETTINGS_PIDFILE="$PID_DIR/settings.pid"

SYSTEMD_UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
APPLICATIONS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps"
SYSTEMD_BIN_DIR="${OPEN_SWITCHER_SYSTEMD_BINDIR:-$HOME/.local/bin}"

DAEMON_UNIT="open-switcher-daemon.service"
TRAY_UNIT="open-switcher-tray.service"
GUARDIAN_SOCKET_UNIT="open-switcher-xtest-guardian.socket"
GUARDIAN_SERVICE_UNIT="open-switcher-xtest-guardian.service"
DESKTOP_FILE="open-switcher.desktop"

DAEMON_UNIT_SOURCE="$SCRIPT_DIR/dist/systemd/$DAEMON_UNIT"
TRAY_UNIT_SOURCE="$SCRIPT_DIR/dist/systemd/$TRAY_UNIT"
GUARDIAN_SOCKET_UNIT_SOURCE="$SCRIPT_DIR/dist/systemd/$GUARDIAN_SOCKET_UNIT"
GUARDIAN_SERVICE_UNIT_SOURCE="$SCRIPT_DIR/dist/systemd/$GUARDIAN_SERVICE_UNIT"
DESKTOP_FILE_SOURCE="$SCRIPT_DIR/dist/$DESKTOP_FILE"
ICON_SOURCE="$SCRIPT_DIR/dist/icons/hicolor/512x512/apps/open-switcher.png"

INSTALLED_DAEMON_BIN="$SYSTEMD_BIN_DIR/open-switcher-daemon"
INSTALLED_TRAY_BIN="$SYSTEMD_BIN_DIR/open-switcher-tray"
INSTALLED_SETTINGS_BIN="$SYSTEMD_BIN_DIR/open-switcher-settings"
INSTALLED_ICON="$ICON_DIR/open-switcher.png"
INSTALLED_AUTOSTART_FILE="$AUTOSTART_DIR/$DESKTOP_FILE"

mkdir -p "$LOG_DIR" "$PID_DIR"

# shellcheck source=/dev/null
source "$LINUX_INPUT_HELPER"
# shellcheck source=/dev/null
source "$WAYLAND_DIAGNOSTICS_HELPER"

ensure_dbus_address() {
    if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && -S "/run/user/$(id -u)/bus" ]]; then
        export DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$(id -u)/bus"
    fi
}

binary_path_for() {
    case "$1" in
        daemon) printf '%s\n' "$DAEMON_BIN" ;;
        tray) printf '%s\n' "$TRAY_BIN" ;;
        settings) printf '%s\n' "$SETTINGS_BIN" ;;
        *) return 1 ;;
    esac
}

pidfile_for() {
    case "$1" in
        daemon) printf '%s\n' "$DAEMON_PIDFILE" ;;
        tray) printf '%s\n' "$TRAY_PIDFILE" ;;
        settings) printf '%s\n' "$SETTINGS_PIDFILE" ;;
        *) return 1 ;;
    esac
}

logfile_for() {
    case "$1" in
        daemon) printf '%s\n' "$LOG_DIR/daemon.log" ;;
        tray) printf '%s\n' "$LOG_DIR/tray.log" ;;
        settings) printf '%s\n' "$LOG_DIR/settings.log" ;;
        *) return 1 ;;
    esac
}

apply_default_debug_env() {
    local component="$1"

    if [[ "$component" != "daemon" ]]; then
        return 0
    fi

    if [[ "${OPEN_SWITCHER_DEFAULT_DEBUG:-1}" == "0" ]]; then
        return 0
    fi

    export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
    export OPEN_SWITCHER_LAYOUT_DEBUG="${OPEN_SWITCHER_LAYOUT_DEBUG:-1}"
    export OPEN_SWITCHER_LAYOUT_DEBUG_FILE="${OPEN_SWITCHER_LAYOUT_DEBUG_FILE:-$LOG_DIR/layout-debug.log}"
    export OPEN_SWITCHER_INPUT_DEBUG="${OPEN_SWITCHER_INPUT_DEBUG:-1}"
    export OPEN_SWITCHER_INPUT_DEBUG_FILE="${OPEN_SWITCHER_INPUT_DEBUG_FILE:-$LOG_DIR/input-debug.log}"
    export OPEN_SWITCHER_SELECTED_TEXT_DEBUG="${OPEN_SWITCHER_SELECTED_TEXT_DEBUG:-0}"
    export OPEN_SWITCHER_SELECTED_TEXT_DEBUG_FILE="${OPEN_SWITCHER_SELECTED_TEXT_DEBUG_FILE:-$LOG_DIR/selected-text-debug.log}"
    export OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG="${OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG:-1}"
    export OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG_FILE="${OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG_FILE:-$LOG_DIR/daemon-capture-debug.log}"
}

process_name_for() {
    case "$1" in
        daemon) printf '%s\n' 'open-switcher' ;;
        tray) printf '%s\n' 'open-switcher-tray' ;;
        settings) printf '%s\n' 'open-switcher-settings' ;;
        *) return 1 ;;
    esac
}

find_component_pids() {
    local component="$1"
    local binary process_name target_dir_pattern
    binary="$(binary_path_for "$component")"
    process_name="$(process_name_for "$component")"
    target_dir_pattern="${TARGET_DIR//\//\\/}"

    ps -eo pid=,args= | awk -v binary="$binary" -v process_name="$process_name" -v target_dir_pattern="$target_dir_pattern" '
        {
            pid = $1
            cmd = $2
            base = cmd
            sub("^.*/", "", base)
        }
        base != process_name { next }
        cmd == binary { print pid; next }
        cmd ~ ("^" target_dir_pattern "/" process_name "$") { print pid; next }
        cmd ~ ("^\\./target/(debug|release)/" process_name "$") { print pid; next }
    ' | sort -u
}

is_running() {
    local pidfile="$1"

    [[ -f "$pidfile" ]] || return 1

    local pid
    pid="$(cat "$pidfile")"
    [[ -n "$pid" ]] || return 1

    kill -0 "$pid" 2>/dev/null
}

require_binary() {
    local component="$1"
    local binary
    binary="$(binary_path_for "$component")"

    if [[ ! -x "$binary" ]]; then
        echo "Бинарник для '$component' не найден: $binary" >&2
        echo "Сначала собери проект:" >&2
        echo "  ./manage.sh build" >&2
        exit 1
    fi
}

start_component() {
    local component="$1"
    local binary pidfile logfile
    binary="$(binary_path_for "$component")"
    pidfile="$(pidfile_for "$component")"
    logfile="$(logfile_for "$component")"

    require_binary "$component"
    ensure_dbus_address
    apply_default_debug_env "$component"

    if is_running "$pidfile"; then
        echo "$component уже запущен (PID $(cat "$pidfile"))."
        return 0
    fi

    OPEN_SWITCHER_RUNTIME_MODE="$DEV_RUNTIME_MODE" nohup "$binary" >"$logfile" 2>&1 &
    local pid=$!
    echo "$pid" >"$pidfile"
    sleep 1

    if kill -0 "$pid" 2>/dev/null; then
        echo "$component запущен (PID $pid)."
    else
        echo "Не удалось запустить $component. Лог: $logfile" >&2
        rm -f "$pidfile"
        if [[ -s "$logfile" ]]; then
            tail -n 20 "$logfile" >&2
        fi
        if [[ "$component" == "daemon" ]] && [[ -f "$logfile" ]]; then
            if grep -Eq "KeyboardAccessDenied|UinputAccessDenied|Linux input setup is not ready" "$logfile"; then
                echo >&2
                echo "Похоже, не выполнен Linux input setup." >&2
                echo "Проверь:" >&2
                echo "  ./manage.sh doctor" >&2
                echo "Установи или переустанови собранный OpenSwitcher .deb:" >&2
                echo "  ./manage.sh package deb" >&2
                echo "Выполни точную команду 'sudo apt install <artifact>', которую напечатает сборка." >&2
                echo "Добавляй --reinstall, только если эта же версия пакета уже установлена." >&2
                echo "Затем выйди из пользовательской сессии, войди снова и повтори doctor." >&2
            fi
        fi
        exit 1
    fi
}

run_doctor_command() {
    local target="${1:-linux-input}"

    case "$target" in
    linux-input|"")
        openswitcher_linux_input_doctor
        ;;
    wayland)
        openswitcher_wayland_doctor
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная doctor-команда: $target" >&2
        usage >&2
        exit 1
        ;;
    esac
}

bootstrap_linux_input() {
    print_linux_input_bootstrap_migration
    return 1
}

run_bootstrap_command() {
    local target="${1:-}"

    case "$target" in
    linux-input)
        bootstrap_linux_input
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная bootstrap-команда: ${target:-<empty>}" >&2
        usage >&2
        exit 1
        ;;
    esac
}

stop_component() {
    local component="$1"
    local pidfile
    pidfile="$(pidfile_for "$component")"

    local -a pids=()
    if is_running "$pidfile"; then
        pids+=("$(cat "$pidfile")")
    fi

    while IFS= read -r pid; do
        [[ -n "$pid" ]] || continue
        pids+=("$pid")
    done < <(find_component_pids "$component")

    if [[ "${#pids[@]}" -eq 0 ]]; then
        rm -f "$pidfile"
        echo "$component уже остановлен."
        return 0
    fi

    mapfile -t pids < <(printf '%s\n' "${pids[@]}" | sort -u)

    local pid
    for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
    done

    for _ in {1..20}; do
        local any_running=0
        for pid in "${pids[@]}"; do
            if kill -0 "$pid" 2>/dev/null; then
                any_running=1
                break
            fi
        done

        if [[ "$any_running" -eq 0 ]]; then
            rm -f "$pidfile"
            echo "$component остановлен (${#pids[@]} process(es))."
            return 0
        fi
        sleep 0.2
    done

    echo "$component не завершился после SIGTERM, отправляю SIGKILL."
    for pid in "${pids[@]}"; do
        kill -9 "$pid" 2>/dev/null || true
    done
    rm -f "$pidfile"
}

show_status() {
    for component in daemon tray settings; do
        local pidfile
        pidfile="$(pidfile_for "$component")"

        local -a pids=()
        if is_running "$pidfile"; then
            pids+=("$(cat "$pidfile")")
        fi
        while IFS= read -r pid; do
            [[ -n "$pid" ]] || continue
            pids+=("$pid")
        done < <(find_component_pids "$component")

        if [[ "${#pids[@]}" -gt 0 ]]; then
            mapfile -t pids < <(printf '%s\n' "${pids[@]}" | sort -u)
            echo "$component: running (PID(s) ${pids[*]})"
        else
            rm -f "$pidfile"
            echo "$component: stopped"
        fi
    done
}

show_logs() {
    local component="${1:-all}"

    case "$component" in
        daemon|tray|settings)
            tail -n 40 "$(logfile_for "$component")"
            ;;
        all)
            for name in daemon tray settings; do
                echo "==> $name <=="
                local logfile
                logfile="$(logfile_for "$name")"
                if [[ -f "$logfile" ]]; then
                    tail -n 20 "$logfile"
                else
                    echo "Логов пока нет."
                fi
                echo
            done
            ;;
        *)
            echo "Неизвестный компонент для logs: $component" >&2
            exit 1
            ;;
    esac
}

build_binaries() {
    local profile_args=()
    if [[ "$PROFILE" == "release" ]]; then
        profile_args+=(--release)
    fi

    cargo build \
        "${profile_args[@]}" \
        --features settings-ui \
        --bin open-switcher \
        --bin open-switcher-tray \
        --bin open-switcher-settings
}

project_rust_toolchain() {
    if [[ ! -f "$RUST_TOOLCHAIN_FILE" ]]; then
        echo "Файл rust-toolchain.toml не найден: $RUST_TOOLCHAIN_FILE" >&2
        exit 1
    fi

    local toolchain
    toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$RUST_TOOLCHAIN_FILE" | head -n 1)"
    if [[ -z "$toolchain" ]]; then
        echo "Не удалось прочитать channel из rust-toolchain.toml." >&2
        exit 1
    fi

    printf '%s\n' "$toolchain"
}

find_rustup_binary() {
    if command -v rustup >/dev/null 2>&1; then
        command -v rustup
        return 0
    fi

    if [[ -x "$HOME/.cargo/bin/rustup" ]]; then
        printf '%s\n' "$HOME/.cargo/bin/rustup"
        return 0
    fi

    return 1
}

require_project_rust_toolchain() {
    local toolchain="$1"
    local rustup_bin

    if ! rustup_bin="$(find_rustup_binary)"; then
        echo "Для сборки .deb нужен rustup и toolchain из rust-toolchain.toml." >&2
        echo "Установи rustup, затем выполни:" >&2
        echo "  rustup toolchain install $toolchain" >&2
        exit 1
    fi

    if ! "$rustup_bin" run "$toolchain" cargo --version >/dev/null 2>&1; then
        echo "Для сборки .deb нужен rustup toolchain '$toolchain' из rust-toolchain.toml." >&2
        echo "Установи его командой:" >&2
        echo "  rustup toolchain install $toolchain" >&2
        exit 1
    fi

    echo "Rust toolchain: $("$rustup_bin" run "$toolchain" cargo --version)"
}

require_command_for_package() {
    local command_name="$1"
    local package_hint="$2"

    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Не найдена команда '$command_name'. Установи пакет: $package_hint" >&2
        exit 1
    fi
}

require_debian_package_dependencies() {
    local -a required_packages=(
        debhelper
        dpkg-dev
        build-essential
        pkg-config
        libdbus-1-dev
        libudev-dev
        libgtk-4-dev
        libadwaita-1-dev
        desktop-file-utils
    )
    local -a missing_packages=()

    require_command_for_package dpkg-query dpkg
    require_command_for_package dpkg-buildpackage dpkg-dev
    require_command_for_package dpkg-parsechangelog dpkg-dev
    require_command_for_package dpkg-architecture dpkg-dev
    require_command_for_package desktop-file-validate desktop-file-utils

    local package status
    for package in "${required_packages[@]}"; do
        status="$(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true)"
        if [[ "$status" != "install ok installed" ]]; then
            missing_packages+=("$package")
        fi
    done

    if [[ "${#missing_packages[@]}" -gt 0 ]]; then
        echo "Не хватает системных Debian build dependencies:" >&2
        printf '  %s\n' "${missing_packages[@]}" >&2
        echo >&2
        echo "Установи их командой:" >&2
        echo "  sudo apt-get install ${missing_packages[*]}" >&2
        echo >&2
        echo "cargo/rustc из apt здесь не требуются: Rust берётся из rustup toolchain проекта." >&2
        exit 1
    fi
}

copy_debian_package_artifacts() {
    local version arch deb_source ddeb_source changes_source buildinfo_source deb_target ddeb_target
    version="$(dpkg-parsechangelog -S Version)"
    arch="$(dpkg-architecture -qDEB_HOST_ARCH)"
    deb_source="$SCRIPT_DIR/../open-switcher_${version}_${arch}.deb"
    ddeb_source="$SCRIPT_DIR/../open-switcher-dbgsym_${version}_${arch}.ddeb"
    changes_source="$SCRIPT_DIR/../open-switcher_${version}_${arch}.changes"
    buildinfo_source="$SCRIPT_DIR/../open-switcher_${version}_${arch}.buildinfo"
    deb_target="$PACKAGE_OUTPUT_DIR/open-switcher_${version}_${arch}.deb"
    ddeb_target="$PACKAGE_OUTPUT_DIR/open-switcher-dbgsym_${version}_${arch}.ddeb"

    if [[ ! -f "$deb_source" ]]; then
        echo "Ожидаемый .deb не найден после сборки: $deb_source" >&2
        exit 1
    fi

    mkdir -p "$PACKAGE_OUTPUT_DIR"
    rm -f "$PACKAGE_OUTPUT_DIR"/open-switcher_*.deb "$PACKAGE_OUTPUT_DIR"/open-switcher-dbgsym_*.ddeb
    cp "$deb_source" "$deb_target"
    echo "Package artifact: $deb_target"
    echo "Install this canonical project artifact with:"
    echo "  sudo apt install $(installable_package_path "$deb_target")"

    if [[ -f "$ddeb_source" ]]; then
        cp "$ddeb_source" "$ddeb_target"
        echo "Debug-symbol artifact: $ddeb_target"
        echo "The .ddeb file is optional and only needed for debugging."
    else
        echo "Debug-symbol package не найден; это не обязательно для обычной установки."
    fi

    rm -f "$deb_source" "$ddeb_source" "$changes_source" "$buildinfo_source"
    echo "Temporary parent-directory Debian artifacts cleaned up."
}

installable_package_path() {
    local path="$1"
    case "$path" in
        /*) printf '%s\n' "$path" ;;
        *) printf './%s\n' "$path" ;;
    esac
}

run_package_post_checks() {
    desktop-file-validate \
        debian/open-switcher.desktop \
        debian/autostart/open-switcher-autostart.desktop
    git diff --check

    if command -v lintian >/dev/null 2>&1; then
        lintian "$PACKAGE_OUTPUT_DIR"/open-switcher_*.deb
    else
        echo "warning: lintian не установлен; lintian check пропущен." >&2
    fi
}

build_debian_package() {
    local toolchain
    toolchain="$(project_rust_toolchain)"

    echo "Сборка .deb использует Rust toolchain проекта через rustup: $toolchain"
    echo "dpkg-buildpackage запускается с -d осознанно: apt cargo/rustc не являются источником истины."

    require_project_rust_toolchain "$toolchain"
    require_debian_package_dependencies

    dpkg-buildpackage -us -uc -b -d -tc
    copy_debian_package_artifacts
    run_package_post_checks
}

run_package_command() {
    local command="${1:-}"

    case "$command" in
    deb)
        build_debian_package
        ;;
    ""|-h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная package-команда: $command" >&2
        usage >&2
        exit 1
        ;;
    esac
}

ensure_systemd_command() {
    if ! command -v systemctl >/dev/null 2>&1; then
        echo "systemctl не найден в PATH." >&2
        exit 1
    fi
}

run_systemctl_user() {
    ensure_dbus_address
    ensure_systemd_command
    systemctl --user "$@"
}

run_journalctl_user() {
    ensure_dbus_address
    if ! command -v journalctl >/dev/null 2>&1; then
        echo "journalctl не найден в PATH." >&2
        exit 1
    fi
    journalctl --user "$@"
}

ensure_dist_file() {
    local path="$1"
    if [[ ! -f "$path" ]]; then
        echo "Файл не найден: $path" >&2
        exit 1
    fi
}

escape_sed_replacement() {
    printf '%s' "$1" | sed 's/[&|]/\\&/g'
}

install_rewritten_file() {
    local source="$1"
    local destination="$2"
    local search="$3"
    local replacement="$4"

    local escaped_replacement
    escaped_replacement="$(escape_sed_replacement "$replacement")"
    sed "s|$search|$escaped_replacement|" "$source" >"$destination"
}

xdg_autostart_content() {
    cat <<EOF
[Desktop Entry]
Type=Application
Name=OpenSwitcher
Comment=Start OpenSwitcher tray service
Exec=systemctl --user start $TRAY_UNIT
X-GNOME-Autostart-enabled=true
EOF
}

install_xdg_autostart_fallback() {
    mkdir -p "$AUTOSTART_DIR"
    xdg_autostart_content >"$INSTALLED_AUTOSTART_FILE"
}

remove_xdg_autostart_fallback() {
    rm -f "$INSTALLED_AUTOSTART_FILE"
}

xdg_autostart_fallback_installed() {
    [[ -f "$INSTALLED_AUTOSTART_FILE" ]] \
        && grep -Fxq "Exec=systemctl --user start $TRAY_UNIT" "$INSTALLED_AUTOSTART_FILE" \
        && grep -Fxq "X-GNOME-Autostart-enabled=true" "$INSTALLED_AUTOSTART_FILE"
}

systemd_autostart_enabled() {
    run_systemctl_user is-enabled "$DAEMON_UNIT" >/dev/null 2>&1 \
        && run_systemctl_user is-enabled "$TRAY_UNIT" >/dev/null 2>&1
}

install_systemd_runtime() {
    require_binary daemon
    require_binary tray

    mkdir -p "$SYSTEMD_UNIT_DIR" "$APPLICATIONS_DIR" "$SYSTEMD_BIN_DIR" "$ICON_DIR"

    ensure_dist_file "$DAEMON_UNIT_SOURCE"
    ensure_dist_file "$TRAY_UNIT_SOURCE"
    ensure_dist_file "$GUARDIAN_SOCKET_UNIT_SOURCE"
    ensure_dist_file "$GUARDIAN_SERVICE_UNIT_SOURCE"
    ensure_dist_file "$DESKTOP_FILE_SOURCE"
    ensure_dist_file "$ICON_SOURCE"
    ensure_systemd_command

    install -m 0755 "$DAEMON_BIN" "$INSTALLED_DAEMON_BIN"
    install -m 0755 "$TRAY_BIN" "$INSTALLED_TRAY_BIN"
    if [[ -x "$SETTINGS_BIN" ]]; then
        install -m 0755 "$SETTINGS_BIN" "$INSTALLED_SETTINGS_BIN"
    fi
    install -m 0644 "$ICON_SOURCE" "$INSTALLED_ICON"

    install_rewritten_file \
        "$DAEMON_UNIT_SOURCE" \
        "$SYSTEMD_UNIT_DIR/$DAEMON_UNIT" \
        '^ExecStart=open-switcher-daemon$' \
        "ExecStart=$INSTALLED_DAEMON_BIN"
    install_rewritten_file \
        "$TRAY_UNIT_SOURCE" \
        "$SYSTEMD_UNIT_DIR/$TRAY_UNIT" \
        '^ExecStart=open-switcher-tray$' \
        "ExecStart=$INSTALLED_TRAY_BIN"
    install -m 0644 \
        "$GUARDIAN_SOCKET_UNIT_SOURCE" \
        "$SYSTEMD_UNIT_DIR/$GUARDIAN_SOCKET_UNIT"
    install_rewritten_file \
        "$GUARDIAN_SERVICE_UNIT_SOURCE" \
        "$SYSTEMD_UNIT_DIR/$GUARDIAN_SERVICE_UNIT" \
        '^ExecStart=open-switcher-daemon --internal-xtest-guardian-v1$' \
        "ExecStart=$INSTALLED_DAEMON_BIN --internal-xtest-guardian-v1"
    install_rewritten_file \
        "$DESKTOP_FILE_SOURCE" \
        "$APPLICATIONS_DIR/$DESKTOP_FILE" \
        '^Exec=systemctl --user start open-switcher-tray\.service$' \
        "Exec=$(command -v systemctl) --user start $TRAY_UNIT"
    install_rewritten_file \
        "$APPLICATIONS_DIR/$DESKTOP_FILE" \
        "$APPLICATIONS_DIR/$DESKTOP_FILE.tmp" \
        '^Icon=open-switcher$' \
        "Icon=$INSTALLED_ICON"
    mv "$APPLICATIONS_DIR/$DESKTOP_FILE.tmp" "$APPLICATIONS_DIR/$DESKTOP_FILE"

    run_systemctl_user daemon-reload
    if systemd_autostart_enabled; then
        install_xdg_autostart_fallback
    fi

    echo "systemd user-файлы установлены:"
    echo "  units: $SYSTEMD_UNIT_DIR"
    echo "  desktop: $APPLICATIONS_DIR/$DESKTOP_FILE"
    echo "  autostart: $INSTALLED_AUTOSTART_FILE"
    echo "  icon: $INSTALLED_ICON"
    echo "  binaries: $SYSTEMD_BIN_DIR"
}

show_systemd_status() {
    local unit active enabled autostart
    for unit in \
        "$DAEMON_UNIT" \
        "$TRAY_UNIT" \
        "$GUARDIAN_SOCKET_UNIT" \
        "$GUARDIAN_SERVICE_UNIT"; do
        active="$(run_systemctl_user is-active "$unit" 2>/dev/null || true)"
        enabled="$(run_systemctl_user is-enabled "$unit" 2>/dev/null || true)"
        [[ -n "$active" ]] || active="unknown"
        [[ -n "$enabled" ]] || enabled="unknown"
        echo "$unit: active=$active enabled=$enabled"
    done
    autostart="missing"
    if xdg_autostart_fallback_installed; then
        autostart="installed"
    fi
    echo "xdg-autostart: $autostart path=$INSTALLED_AUTOSTART_FILE"
}

show_systemd_logs() {
    local target="${1:-all}"

    case "$target" in
        daemon)
            run_journalctl_user -u "$DAEMON_UNIT" -n 40 --no-pager
            ;;
        tray)
            run_journalctl_user -u "$TRAY_UNIT" -n 40 --no-pager
            ;;
        guardian)
            run_journalctl_user \
                -u "$GUARDIAN_SOCKET_UNIT" \
                -u "$GUARDIAN_SERVICE_UNIT" \
                -n 40 \
                --no-pager
            ;;
        all)
            run_journalctl_user \
                -u "$DAEMON_UNIT" \
                -u "$TRAY_UNIT" \
                -u "$GUARDIAN_SOCKET_UNIT" \
                -u "$GUARDIAN_SERVICE_UNIT" \
                -n 60 \
                --no-pager
            ;;
        *)
            echo "Неизвестный компонент для systemd logs: $target" >&2
            exit 1
            ;;
    esac
}

usage() {
    cat <<EOF
Использование:
  ./manage.sh dev <команда>
  ./manage.sh systemd <команда>
  ./manage.sh package <команда>
  ./manage.sh doctor [linux-input|wayland]
  ./manage.sh bootstrap linux-input # устаревшая compatibility-команда; ничего не меняет
  ./manage.sh <команда>            # алиасы на dev-режим

dev-команды:
  dev build             Собрать open-switcher, tray и settings
  dev start             Запустить daemon и tray из target/
  dev stop              Остановить daemon, tray и settings из dev-режима
  dev restart           Перезапустить daemon и tray из dev-режима
  dev status            Показать статус dev-процессов
  dev logs [name]       Показать dev-логи: daemon | tray | settings | all
  dev settings          Открыть окно настроек из target/

systemd-команды:
  systemd install       Установить user units, desktop entry, autostart fallback и бинарники в ~/.local
  systemd start         Запустить $DAEMON_UNIT и $TRAY_UNIT
  systemd stop          Последовательно остановить tray, daemon и XTEST guardian
  systemd restart       Безопасно остановить runtime и снова запустить daemon/tray
  systemd status        Показать active/enabled статус user units
  systemd logs [name]   Показать journalctl-логи: daemon | tray | guardian | all
  systemd enable        Включить автозапуск user units и XDG fallback
  systemd disable       Выключить автозапуск user units и XDG fallback

package-команды:
  package deb           Собрать .deb через rustup toolchain проекта в dist/packages/

doctor-команды:
  doctor                Проверить Linux input setup для '/dev/input/*' и '/dev/uinput'
  doctor wayland        Показать диагностику Wayland/GNOME окружения и uinput

compatibility-команды:
  bootstrap linux-input Показать безопасную миграцию на package-only Linux input setup (ничего не меняет)

Переменные окружения:
  OPEN_SWITCHER_PROFILE=debug|release   По умолчанию: debug
  OPEN_SWITCHER_SYSTEMD_BINDIR=/path    Куда ставить бинарники для systemd install
EOF
}

run_dev_command() {
    local command="${1:-}"
    local arg="${2:-}"

    case "$command" in
    build)
        build_binaries
        ;;
    start)
        start_component daemon
        start_component tray
        ;;
    stop)
        stop_component settings
        stop_component tray
        stop_component daemon
        ;;
    restart)
        stop_component settings
        stop_component tray
        stop_component daemon
        start_component daemon
        start_component tray
        ;;
    status)
        show_status
        ;;
    logs)
        show_logs "${arg:-all}"
        ;;
    settings)
        start_component settings
        ;;
    ""|-h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная dev-команда: $command" >&2
        usage >&2
        exit 1
        ;;
    esac
}

wait_for_systemd_guardian_inactive() {
    ensure_dbus_address
    ensure_systemd_command
    command -v timeout >/dev/null 2>&1 || return 0

    # Переменные внутри строки раскрывает дочерний shell с тем же user bus.
    # shellcheck disable=SC2016
    timeout --signal=KILL 7s sh -c '
        while :; do
            state="$(
                systemctl --user show \
                    --property=ActiveState \
                    --value \
                    open-switcher-xtest-guardian.service \
                    2>/dev/null
            )" || exit 0
            case "$state" in
                active|activating|deactivating|reloading)
                    sleep 0.1
                    ;;
                *)
                    exit 0
                    ;;
            esac
        done
    ' || true
}

stop_systemd_runtime() {
    run_systemctl_user stop "$TRAY_UNIT" || true
    run_systemctl_user stop "$DAEMON_UNIT" || true
    wait_for_systemd_guardian_inactive
    run_systemctl_user stop "$GUARDIAN_SOCKET_UNIT" || true
    run_systemctl_user stop "$GUARDIAN_SERVICE_UNIT" || true
    run_systemctl_user daemon-reload || true
}

start_systemd_runtime() {
    run_systemctl_user start "$DAEMON_UNIT"
    run_systemctl_user start "$TRAY_UNIT"
}

run_systemd_command() {
    local command="${1:-}"
    local arg="${2:-}"

    case "$command" in
    install)
        install_systemd_runtime
        ;;
    start)
        start_systemd_runtime
        ;;
    stop)
        stop_systemd_runtime
        ;;
    restart)
        stop_systemd_runtime
        start_systemd_runtime
        ;;
    status)
        show_systemd_status
        ;;
    logs)
        show_systemd_logs "${arg:-all}"
        ;;
    enable)
        run_systemctl_user enable "$DAEMON_UNIT"
        run_systemctl_user enable "$TRAY_UNIT"
        install_xdg_autostart_fallback
        ;;
    disable)
        run_systemctl_user disable "$TRAY_UNIT"
        run_systemctl_user disable "$DAEMON_UNIT"
        remove_xdg_autostart_fallback
        ;;
    ""|-h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная systemd-команда: $command" >&2
        usage >&2
        exit 1
        ;;
    esac
}

namespace="${1:-}"

case "$namespace" in
    dev)
        run_dev_command "${2:-}" "${3:-}"
        ;;
    systemd)
        run_systemd_command "${2:-}" "${3:-}"
        ;;
    package)
        run_package_command "${2:-}"
        ;;
    doctor)
        run_doctor_command "${2:-linux-input}"
        ;;
    bootstrap)
        run_bootstrap_command "${2:-}"
        ;;
    build|start|stop|restart|status|logs|settings|"")
        run_dev_command "${1:-}" "${2:-}"
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная команда: $namespace" >&2
        usage >&2
        exit 1
        ;;
esac
