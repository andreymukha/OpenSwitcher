#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_DIR="$SCRIPT_DIR/.run"
LOG_DIR="$RUN_DIR/logs"
PID_DIR="$RUN_DIR/pids"

PROFILE="${OPEN_SWITCHER_PROFILE:-debug}"
TARGET_DIR="$SCRIPT_DIR/target/$PROFILE"

DAEMON_BIN="$TARGET_DIR/open-switcher"
TRAY_BIN="$TARGET_DIR/open-switcher-tray"
SETTINGS_BIN="$TARGET_DIR/open-switcher-settings"

DAEMON_PIDFILE="$PID_DIR/daemon.pid"
TRAY_PIDFILE="$PID_DIR/tray.pid"
SETTINGS_PIDFILE="$PID_DIR/settings.pid"

SYSTEMD_UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
APPLICATIONS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
SYSTEMD_BIN_DIR="${OPEN_SWITCHER_SYSTEMD_BINDIR:-$HOME/.local/bin}"

DAEMON_UNIT="open-switcher-daemon.service"
TRAY_UNIT="open-switcher-tray.service"
DESKTOP_FILE="open-switcher.desktop"

DAEMON_UNIT_SOURCE="$SCRIPT_DIR/dist/systemd/$DAEMON_UNIT"
TRAY_UNIT_SOURCE="$SCRIPT_DIR/dist/systemd/$TRAY_UNIT"
DESKTOP_FILE_SOURCE="$SCRIPT_DIR/dist/$DESKTOP_FILE"

INSTALLED_DAEMON_BIN="$SYSTEMD_BIN_DIR/open-switcher-daemon"
INSTALLED_TRAY_BIN="$SYSTEMD_BIN_DIR/open-switcher-tray"
INSTALLED_SETTINGS_BIN="$SYSTEMD_BIN_DIR/open-switcher-settings"

mkdir -p "$LOG_DIR" "$PID_DIR"

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
    export OPEN_SWITCHER_SELECTED_TEXT_DEBUG="${OPEN_SWITCHER_SELECTED_TEXT_DEBUG:-1}"
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

    nohup "$binary" >"$logfile" 2>&1 &
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
        exit 1
    fi
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

install_systemd_runtime() {
    require_binary daemon
    require_binary tray

    mkdir -p "$SYSTEMD_UNIT_DIR" "$APPLICATIONS_DIR" "$SYSTEMD_BIN_DIR"

    ensure_dist_file "$DAEMON_UNIT_SOURCE"
    ensure_dist_file "$TRAY_UNIT_SOURCE"
    ensure_dist_file "$DESKTOP_FILE_SOURCE"

    install -m 0755 "$DAEMON_BIN" "$INSTALLED_DAEMON_BIN"
    install -m 0755 "$TRAY_BIN" "$INSTALLED_TRAY_BIN"
    if [[ -x "$SETTINGS_BIN" ]]; then
        install -m 0755 "$SETTINGS_BIN" "$INSTALLED_SETTINGS_BIN"
    fi

    install -m 0644 "$DAEMON_UNIT_SOURCE" "$SYSTEMD_UNIT_DIR/$DAEMON_UNIT"
    install -m 0644 "$TRAY_UNIT_SOURCE" "$SYSTEMD_UNIT_DIR/$TRAY_UNIT"
    install -m 0644 "$DESKTOP_FILE_SOURCE" "$APPLICATIONS_DIR/$DESKTOP_FILE"

    run_systemctl_user daemon-reload

    echo "systemd user-файлы установлены:"
    echo "  units: $SYSTEMD_UNIT_DIR"
    echo "  desktop: $APPLICATIONS_DIR/$DESKTOP_FILE"
    echo "  binaries: $SYSTEMD_BIN_DIR"
}

show_systemd_status() {
    local unit active enabled
    for unit in "$DAEMON_UNIT" "$TRAY_UNIT"; do
        active="$(run_systemctl_user is-active "$unit" 2>/dev/null || true)"
        enabled="$(run_systemctl_user is-enabled "$unit" 2>/dev/null || true)"
        [[ -n "$active" ]] || active="unknown"
        [[ -n "$enabled" ]] || enabled="unknown"
        echo "$unit: active=$active enabled=$enabled"
    done
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
        all)
            run_journalctl_user -u "$DAEMON_UNIT" -u "$TRAY_UNIT" -n 60 --no-pager
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
  systemd install       Установить user units, desktop entry и бинарники в ~/.local
  systemd start         Запустить $DAEMON_UNIT и $TRAY_UNIT
  systemd stop          Остановить $TRAY_UNIT и $DAEMON_UNIT
  systemd restart       Перезапустить $DAEMON_UNIT и $TRAY_UNIT
  systemd status        Показать active/enabled статус user units
  systemd logs [name]   Показать journalctl-логи: daemon | tray | all
  systemd enable        Включить автозапуск user units
  systemd disable       Выключить автозапуск user units

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

run_systemd_command() {
    local command="${1:-}"
    local arg="${2:-}"

    case "$command" in
    install)
        install_systemd_runtime
        ;;
    start)
        run_systemctl_user start "$DAEMON_UNIT"
        run_systemctl_user start "$TRAY_UNIT"
        ;;
    stop)
        run_systemctl_user stop "$TRAY_UNIT" || true
        run_systemctl_user stop "$DAEMON_UNIT" || true
        ;;
    restart)
        run_systemctl_user restart "$DAEMON_UNIT"
        run_systemctl_user restart "$TRAY_UNIT"
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
        ;;
    disable)
        run_systemctl_user disable "$TRAY_UNIT"
        run_systemctl_user disable "$DAEMON_UNIT"
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
