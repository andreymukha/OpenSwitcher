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

    if ! is_running "$pidfile"; then
        rm -f "$pidfile"
        echo "$component уже остановлен."
        return 0
    fi

    local pid
    pid="$(cat "$pidfile")"
    kill "$pid" 2>/dev/null || true

    for _ in {1..20}; do
        if ! kill -0 "$pid" 2>/dev/null; then
            rm -f "$pidfile"
            echo "$component остановлен."
            return 0
        fi
        sleep 0.2
    done

    echo "$component не завершился после SIGTERM, отправляю SIGKILL."
    kill -9 "$pid" 2>/dev/null || true
    rm -f "$pidfile"
}

show_status() {
    for component in daemon tray settings; do
        local pidfile
        pidfile="$(pidfile_for "$component")"

        if is_running "$pidfile"; then
            echo "$component: running (PID $(cat "$pidfile"))"
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

usage() {
    cat <<EOF
Использование: ./manage.sh <команда>

Команды:
  build       Собрать open-switcher, tray и settings
  start       Запустить демон и tray
  stop        Остановить демон, tray и settings
  restart     Перезапустить демон и tray
  status      Показать статус процессов
  logs        Показать последние логи всех компонентов
  logs <name> Показать лог одного компонента: daemon | tray | settings
  settings    Открыть окно настроек

Переменные окружения:
  OPEN_SWITCHER_PROFILE=debug|release   По умолчанию: debug
EOF
}

command="${1:-}"

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
        show_logs "${2:-all}"
        ;;
    settings)
        start_component settings
        ;;
    ""|-h|--help|help)
        usage
        ;;
    *)
        echo "Неизвестная команда: $command" >&2
        usage >&2
        exit 1
        ;;
esac
