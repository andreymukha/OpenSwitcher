#!/bin/bash
export DISPLAY=:0.0
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus

case "$1" in
    start)
        killall -9 open-switcher tray 2>/dev/null
        sleep 1 # Даем D-Bus время освободить имя
        cd ~/projects/open-switcher
        nohup ./target/release/open-switcher > daemon.log 2>&1 &
        sleep 1
        nohup ./target/release/tray > tray.log 2>&1 &
        echo "Open-Switcher запущен!"
        ;;
    stop)
        killall -9 open-switcher tray 2>/dev/null
        echo "Open-Switcher остановлен."
        ;;
    log)
        tail -n 20 daemon.log tray.log
        ;;
    *)
        echo "Использование: ./manage.sh {start|stop|log}"
        ;;
esac