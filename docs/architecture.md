# Архитектура OpenSwitcher

## Компоненты

Проект собирается в три бинаря:

- `open-switcher` — демон
- `open-switcher-tray` — tray-клиент
- `open-switcher-settings` — окно настроек

Но пользовательская модель приложения теперь такая:

- обязательная пользовательская связка — `daemon + tray`
- `open-switcher-settings` — отдельный служебный инструмент

Связь между компонентами идёт через D-Bus, а штатный жизненный цикл `daemon + tray`
управляется через `systemd --user`.

## Главный принцип

Демон — единственный источник истины.

Только демон:

- читает `config.toml`
- валидирует настройки
- сохраняет настройки
- обновляет runtime state

Tray и settings UI:

- не читают `config.toml`
- не пишут `config.toml`
- не знают путь к конфигу
- работают только через D-Bus API демона

Дополнительно:

- tray является основным пользовательским entrypoint
- tray и daemon рассматриваются как единое приложение
- settings UI не входит в обязательную пару и может запускаться отдельно

## Слои внутри проекта

### Core / daemon

Основные модули:

- [src/daemon/runtime.rs](/home/fly/projects/open-switcher/src/daemon/runtime.rs)
- [src/daemon/keyboard.rs](/home/fly/projects/open-switcher/src/daemon/keyboard.rs)
- [src/daemon/switch_logic.rs](/home/fly/projects/open-switcher/src/daemon/switch_logic.rs)
- [src/daemon/service.rs](/home/fly/projects/open-switcher/src/daemon/service.rs)

Роли:

- `runtime.rs` — runtime state и `ConfigService`
- `keyboard.rs` — работа с `evdev` и `uinput`
- `switch_logic.rs` — логика определения и исправления слова
- `service.rs` — orchestration event loop и публикация D-Bus signal

### D-Bus слой

Основной файл:

- [src/dbus/mod.rs](/home/fly/projects/open-switcher/src/dbus/mod.rs)

Роль слоя:

- описывает D-Bus контракт
- маппит transport DTO в domain types
- не содержит бизнес-логики переключения

Основной API:

- `Toggle()`
- `GetSettings()`
- `UpdateSettings(SettingsDto)`
- `StatusChanged(enabled, layout)`

### Модель и ошибки

Основные файлы:

- [src/model.rs](/home/fly/projects/open-switcher/src/model.rs)
- [src/error/mod.rs](/home/fly/projects/open-switcher/src/error/mod.rs)

Роль:

- общие transport/domain types
- typed validation
- typed errors через `thiserror`

### Settings UI

Основные файлы:

- [src/settings_ui/dbus_client.rs](/home/fly/projects/open-switcher/src/settings_ui/dbus_client.rs)
- [src/settings_ui/state.rs](/home/fly/projects/open-switcher/src/settings_ui/state.rs)
- [src/settings_ui/presenter.rs](/home/fly/projects/open-switcher/src/settings_ui/presenter.rs)
- [src/settings_ui/ui.rs](/home/fly/projects/open-switcher/src/settings_ui/ui.rs)

Разделение ответственности:

- `dbus_client.rs` — только D-Bus client
- `state.rs` — состояние формы и derived view state
- `presenter.rs` — orchestration загрузки/сохранения
- `ui.rs` — GTK4/libadwaita виджеты и отображение ошибок

UI не содержит бизнес-логики демона.

### Tray

Основные файлы:

- [src/tray/dbus_listener.rs](/home/fly/projects/open-switcher/src/tray/dbus_listener.rs)
- [src/tray/tray_service.rs](/home/fly/projects/open-switcher/src/tray/tray_service.rs)
- [src/tray/mod.rs](/home/fly/projects/open-switcher/src/tray/mod.rs)

Роли:

- `dbus_listener.rs` — подписка на D-Bus signal, bounded reconnect и startup retry
- `tray_service.rs` — ksni tray menu и icon state
- `mod.rs` — запуск tray service и single-instance guard

Состояние tray синхронизируется по D-Bus сигналу демона.

Tray не считается самостоятельным долгоживущим клиентом:

- при потере daemon tray пытается его восстановить
- если daemon недоступен, tray завершает себя
- отдельный экземпляр tray не должен создавать вторую иконку

## Жизненный цикл daemon + tray

Основной пользовательский запуск идёт через tray.

Официальный runtime-сценарий:

1. desktop entry запускает `open-switcher-tray.service`
2. tray user unit тянет `open-switcher-daemon.service`
3. daemon публикует состояние через D-Bus
4. tray читает initial state и подписывается на `StatusChanged`

Ключевые свойства:

- `systemd --user` — официальный способ автозапуска
- tray имеет single-instance guard через отдельное well-known D-Bus имя
- daemon остаётся single-instance через своё D-Bus имя `org.oswitch.core`
- tray и daemon имеют bounded recovery, но не бесконечно рестартят друг друга

## Поток настроек

1. `open-switcher-settings` загружает настройки через D-Bus.
2. Демон возвращает `SettingsDto`.
3. UI редактирует локальный view state.
4. При сохранении UI отправляет DTO обратно через D-Bus.
5. Демон валидирует данные.
6. Демон сохраняет `config.toml`.
7. Демон обновляет runtime state.

## Поток состояния tray

1. Tray создаёт D-Bus proxy.
2. Tray bounded-retry ждёт доступности daemon на D-Bus.
3. Tray читает текущее состояние демона.
4. Tray подписывается на `StatusChanged`.
5. При `Toggle()` tray не угадывает итоговое состояние локально.
6. Каноническое обновление приходит обратно из сигнала демона.

## Конфиг и путь

Конфиг определяется в одном месте:

- [src/config.rs](/home/fly/projects/open-switcher/src/config.rs)

Используется путь:

```text
~/.config/open-switcher/config.toml
```

## Что важно при развитии проекта

Нужно сохранять следующие инварианты:

- демон остаётся единственным источником истины
- клиенты не работают с конфигом напрямую
- D-Bus остаётся единственным IPC-контрактом
- tray и daemon остаются единой пользовательской парой
- UI не забирает в себя бизнес-логику переключения
- transport types и domain types остаются типизированными
