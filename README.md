# OpenSwitcher

OpenSwitcher — Linux desktop utility на Rust для автоматического переключения раскладки клавиатуры.

Сейчас проект состоит из трёх программ:

- `open-switcher` — daemon, единственный источник истины для настроек
- `open-switcher-tray` — основной пользовательский entrypoint и tray-клиент
- `open-switcher-settings` — служебное окно настроек на GTK4 + libadwaita

Важно:

- `config.toml` читает и пишет только демон
- tray и settings UI работают с демоном только через D-Bus
- GUI не работает с конфигом напрямую
- пользовательская связка приложения — это `daemon + tray`
- `open-switcher-settings` не входит в обязательную пару и может запускаться отдельно как служебная утилита

## Зависимости

Для Linux Mint / Ubuntu-подобной системы:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  libgtk-4-dev \
  libadwaita-1-dev
```

## Сборка

Проверка всех бинарников:

```bash
cargo check --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Полная локальная сборка:

```bash
cargo build --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Тесты:

```bash
cargo test --lib --test dbus_api
```

## Запуск

Основной пользовательский запуск:

```bash
open-switcher-tray
```

Tray при старте проверяет daemon, при необходимости пытается поднять его через `systemctl --user start open-switcher-daemon.service`, и завершается, если daemon не удалось восстановить.

### User-level systemd автозапуск

OpenSwitcher использует только `systemd --user` units.

Файлы поставки:

- `dist/systemd/open-switcher-daemon.service`
- `dist/systemd/open-switcher-tray.service`
- `dist/open-switcher.desktop`

Установка user units:

```bash
mkdir -p ~/.config/systemd/user
install -m 0644 dist/systemd/open-switcher-daemon.service ~/.config/systemd/user/
install -m 0644 dist/systemd/open-switcher-tray.service ~/.config/systemd/user/
systemctl --user daemon-reload
```

Ручное включение автозапуска:

```bash
systemctl --user enable --now open-switcher-daemon.service
systemctl --user enable --now open-switcher-tray.service
```

Ручное отключение автозапуска:

```bash
systemctl --user disable --now open-switcher-tray.service
systemctl --user disable --now open-switcher-daemon.service
```

Desktop entry запускает только tray:

```bash
install -Dm0644 dist/open-switcher.desktop ~/.local/share/applications/open-switcher.desktop
```

Важно:

- `~/.config/autostart` для OpenSwitcher не используется
- официальный сценарий автозапуска — только `systemd --user`
- `open-switcher-settings` не добавляется в автозапуск

Демон:

```bash
./target/debug/open-switcher
```

Tray:

```bash
./target/debug/open-switcher-tray
```

Окно настроек:

```bash
./target/debug/open-switcher-settings
```

Для удобства можно использовать `manage.sh`:

```bash
./manage.sh build
./manage.sh start
./manage.sh status
./manage.sh logs
./manage.sh settings
./manage.sh stop
```

Если нужен `release`, можно передать:

```bash
OPEN_SWITCHER_PROFILE=release ./manage.sh build
OPEN_SWITCHER_PROFILE=release ./manage.sh start
```

## Конфиг

Путь к конфигу:

```text
~/.config/open-switcher/config.toml
```

Конфигом управляет только демон.

## Быстрая проверка D-Bus

Получить текущие настройки:

```bash
gdbus call \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core \
  --method org.oswitch.core.GetSettings
```

Обновить настройки:

```bash
gdbus call \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core \
  --method org.oswitch.core.UpdateSettings \
  "(uint32 77, 'F12')"
```

Подписаться на сигналы состояния:

```bash
gdbus monitor \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core
```

Переключить состояние демона:

```bash
gdbus call \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core \
  --method org.oswitch.core.Toggle
```

## Локальный сценарий проверки

1. Собрать проект через `cargo build --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings`.
2. Запустить `./target/debug/open-switcher-tray`.
3. Убедиться, что tray сам поднял daemon или daemon уже работает через `systemd --user`.
4. При необходимости открыть `./target/debug/open-switcher-settings`.
5. Проверить `GetSettings` и `UpdateSettings` через `gdbus`.
6. Убедиться, что `~/.config/open-switcher/config.toml` обновляет именно демон.

## Типичные проблемы

### `Dbus(NameTaken)` или `The name org.oswitch.core is already owned`

Причина:

- уже запущен другой экземпляр `open-switcher`
- или daemon уже поднят через `systemd --user`

Что проверить:

```bash
gdbus call \
  --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner \
  org.oswitch.core
```

Если нужно освободить имя:

```bash
pkill -f '/target/.*/open-switcher($| )'
```

### `ServiceUnknown` при вызове `gdbus`

Причина:

- демон не запущен
- или завершился сразу после старта

Что делать:

1. Убедиться, что tray действительно запущен или поднять daemon вручную через `systemctl --user start open-switcher-daemon.service`.
2. Проверить, что имя есть на session bus.
3. Только после этого вызывать `gdbus` или открывать settings UI.

### Tray завершился сразу после старта

Чаще всего это означает, что:

- уже работает другой экземпляр `open-switcher-tray`
- tray не смог восстановить daemon через `systemctl --user`

Что проверить:

```bash
systemctl --user status open-switcher-daemon.service
systemctl --user status open-switcher-tray.service
gdbus call \
  --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner \
  org.oswitch.tray
```

### Не собирается settings UI

Чаще всего не хватает системных GTK/libadwaita пакетов.

Установить:

```bash
sudo apt-get install -y \
  build-essential \
  pkg-config \
  libgtk-4-dev \
  libadwaita-1-dev
```

### Tray или settings UI не видят демон

Проверь окружение session D-Bus:

```bash
echo "$DBUS_SESSION_BUS_ADDRESS"
```

Если запускаешь из нестандартной сессии или через скрипт, убедись, что клиент и демон работают в одной пользовательской session bus.

### Демон стартует, но не работает с клавиатурой

Проверь:

- доступ к `/dev/input/event*`
- доступ к `uinput`
- что процесс действительно нашёл физическую клавиатуру

Полезно запустить демон напрямую и посмотреть его stdout/stderr:

```bash
./target/debug/open-switcher
```

### `xset` не возвращает корректную раскладку

Проверь переменные окружения:

```bash
echo "$DISPLAY"
echo "$XAUTHORITY"
```

Для X11 они должны указывать на реальную пользовательскую сессию.

## Документация

- Актуальная схема проекта: [docs/architecture.md](/home/fly/projects/open-switcher/docs/architecture.md)
- Историческое ТЗ и исходное описание проекта: [docs/technical-spec.md](/home/fly/projects/open-switcher/docs/technical-spec.md)
