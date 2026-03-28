#!/bin/bash
case "$1" in
    start)
        killall -9 open-switcher tray 2>/dev/null
        cd ~/projects/open-switcher
        nohup ./target/debug/open-switcher > daemon.log 2>&1 &
        sleep 1
        nohup ./target/debug/tray > tray.log 2>&1 &
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
