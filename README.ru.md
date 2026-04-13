Язык: [English](README.md) | **Русский**

# OpenSwitcher

OpenSwitcher — это настольная Linux-утилита, ориентированная на EN/RU-набор и написанная на Rust.

Проект ориентирован на повседневные EN/RU-сценарии ввода:
- автоматическое исправление последнего слова, если оно было набрано в неправильной раскладке
- ручное исправление текущего или предыдущего слова
- конвертацию раскладки для выделенного текста
- лёгкое управление через tray и отдельное окно настроек

> **Примечание о разработке**
>
> OpenSwitcher разрабатывается с использованием ИИ в инженерной разработке. Rust-реализация в этом репозитории создаётся AI-инструментами под руководством человека, проходит проверку и принимается только после явного одобрения.
>
> Владелец проекта не является Rust-разработчиком и сосредоточен на продуктовых требованиях, архитектурных решениях, тестировании, UX и финальной валидации, а не на ручной реализации на Rust.

Этот репозиторий содержит полную историю разработки проекта.

## Статус проекта

OpenSwitcher находится в активной разработке.

Текущий публичный фокус проекта — настольное Linux-приложение для EN/RU-сценариев ввода, построенное вокруг модели `daemon + tray`.

## Быстрый старт

```bash
./manage.sh dev build
./manage.sh dev start
./manage.sh dev settings
```

## Состав проекта

OpenSwitcher состоит из трёх бинарников:

- `open-switcher`  
  Бинарник daemon. Он отвечает за конфигурацию, обработку ввода, логику исправлений и D-Bus API.
- `open-switcher-tray`  
  Бинарник tray и основная пользовательская точка входа. Он показывает иконку в tray, меню состояния и взаимодействует с daemon по D-Bus.
- `open-switcher-settings`  
  Отдельная утилита настроек на GTK4 + libadwaita. Она не входит в обязательную пару `daemon + tray`.

## Модель работы

Для пользователя OpenSwitcher — это одно приложение, состоящее из двух взаимодействующих процессов:

- `daemon`
- `tray`

Они должны работать вместе как единое пользовательское приложение.

Текущая модель работы:
- официальная пользовательская точка запуска — tray
- официальный путь автозапуска — `systemd --user`
- `daemon + tray` рассматриваются как единый жизненный цикл приложения
- окно настроек является опциональным и может запускаться отдельно

## Текущие возможности

- Автопереключение последнего слова при завершении слова, если ввод похож на EN/RU-текст в неправильной раскладке
- Горячая клавиша для ручного исправления текущего или предыдущего слова
- Конвертация раскладки для выделенного текста
- Опции исправления регистра:
  - исправление двух заглавных букв в начале слова
  - исправление случайного Caps Lock-паттерна
- Окно настроек для системных параметров, исправлений и горячих клавиш
- Интеграция с `systemd` на уровне пользовательской сессии для пары `daemon + tray`
- Меню tray со статусом и управляющими действиями

## Текущие границы и ограничения

- Только Linux
- Основной поддерживаемый сценарий ввода — EN/RU
- Поддержка раскладок и backend-слоя пока остаётся консервативной и опирается на конкретные backend-реализации
- Текущий backend-слой спроектирован с расчётом на расширение, но поддержка пока не охватывает широко все настольные окружения
- Работа tray зависит от настольного окружения с совместимым хостом StatusNotifier/AppIndicator
- Окно настроек собирается под feature-флагом Cargo `settings-ui`

## Требования

### Среда выполнения

- настольная Linux-сессия
- сессионный D-Bus
- `systemd --user` для официальной модели запуска и автозапуска
- настольное окружение с совместимым хостом StatusNotifier/AppIndicator для tray

### Зависимости для сборки

Для Linux Mint / Ubuntu-подобных систем:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  libgtk-4-dev \
  libadwaita-1-dev
```

## Сборка

Проверить все бинарники:

```bash
cargo check --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Собрать всё локально:

```bash
cargo build --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Запустить тесты:

```bash
cargo test -q --lib
cargo test --test dbus_api
```

## Процесс разработки

В репозитории есть `manage.sh`, который поддерживает два явных режима:

- `dev`  
  Прямой запуск локальных бинарников из `target/`
- `systemd`  
  Реальный пользовательский режим работы через `systemctl --user`

Старые верхнеуровневые команды сохранены как алиасы для `dev`, но предпочтительная форма — явное пространство имён.

### Режим `dev`

Сборка:

```bash
./manage.sh dev build
```

Запуск локальной пары `daemon + tray` из каталога сборки:

```bash
./manage.sh dev start
```

Полезные команды:

```bash
./manage.sh dev status
./manage.sh dev logs
./manage.sh dev settings
./manage.sh dev stop
```

При необходимости можно использовать профиль `release`:

```bash
OPEN_SWITCHER_PROFILE=release ./manage.sh dev build
OPEN_SWITCHER_PROFILE=release ./manage.sh dev start
```

## Работа через `systemd --user`

Это официальная модель работы для публикуемого приложения.

Установить пользовательские unit-файлы, desktop-файл и локально установленные бинарники:

```bash
./manage.sh systemd install
```

Запустить приложение `daemon + tray` через пользовательские сервисы:

```bash
./manage.sh systemd start
```

Проверить текущее состояние:

```bash
./manage.sh systemd status
./manage.sh systemd logs
```

Остановить текущую сессию:

```bash
./manage.sh systemd stop
```

Включить или выключить автозапуск для будущих сессий:

```bash
./manage.sh systemd enable
./manage.sh systemd disable
```

### Что устанавливается

Файлы дистрибутива в репозитории:

- `dist/systemd/open-switcher-daemon.service`
- `dist/systemd/open-switcher-tray.service`
- `dist/open-switcher.desktop`

Команда `./manage.sh systemd install` устанавливает их в:

- `~/.config/systemd/user/`
- `~/.local/share/applications/`
- `~/.local/bin/`

Примечания:
- desktop-файл запускает сервис tray через `systemctl --user`
- tray unit подтягивает daemon unit
- `~/.config/autostart` не используется

## Прямой запуск бинарников

Этот раздел нужен в основном для разработки и ручной локальной отладки.

Если нужно запускать бинарники вручную из каталога сборки:

Daemon:

```bash
./target/debug/open-switcher
```

Tray:

```bash
./target/debug/open-switcher-tray
```

Settings:

```bash
./target/debug/open-switcher-settings
```

## Конфигурация

Путь к конфигурационному файлу:

```text
~/.config/open-switcher/config.toml
```

Важное поведение:
- только daemon читает и записывает конфигурационный файл
- tray и утилита настроек взаимодействуют с daemon через D-Bus
- окно настроек не пишет конфиг напрямую

## Заметки по D-Bus

Daemon использует сессионное D-Bus-имя `org.oswitch.core`.

Быстрая проверка:

```bash
gdbus call \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core \
  --method org.oswitch.core.GetSettings
```

Просмотр сигналов статуса и изменений настроек:

```bash
gdbus monitor \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core
```

## Практическая проверка

Рекомендуемый порядок локальной базовой проверки:

1. `./manage.sh dev build`
2. `./manage.sh dev start`
3. `./manage.sh dev settings`
4. `./manage.sh dev stop`
5. `./manage.sh systemd install`
6. `./manage.sh systemd start`
7. `./manage.sh systemd status`
8. `./manage.sh systemd logs`

## Устранение проблем

### `Dbus(NameTaken)` or `The name org.oswitch.core is already owned`

Обычно это означает, что уже запущен другой экземпляр daemon.

Проверь:

```bash
gdbus call \
  --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner \
  org.oswitch.core
```

### `ServiceUnknown` on D-Bus calls

Обычно это означает, что daemon не запущен или завершился во время старта.

Проверь:

```bash
./manage.sh dev status
./manage.sh systemd status
./manage.sh systemd logs
```

### Иконка tray не появляется

Возможные причины:
- процесс tray не запущен
- настольное окружение не предоставляет совместимый tray host
- tray был запущен не тем способом, который ожидается для текущего режима

Проверь:

```bash
./manage.sh dev status
./manage.sh systemd status
```

## Лицензия

Этот проект распространяется под лицензией MIT. Подробности см. в файле LICENSE.
