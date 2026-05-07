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

Цель первого релиза — настольное Linux-приложение для EN/RU-сценариев ввода, построенное вокруг модели `daemon + tray`.

Текущая базовая среда релиза:
- подтверждённые окружения перечислены в разделе [Проверенные окружения](#проверенные-окружения)
- Wayland — обязательная поддерживаемая цель
- фокус только на EN/RU-вводе
- официальная модель запуска и автозапуска — `systemd --user`

## Проверенные окружения

| Окружение | Сессия | Статус | Проверка |
| --- | --- | --- | --- |
| Linux Mint 22.2 Cinnamon | X11 | Поддерживаемый baseline | Подтверждено на текущем ноутбуке |

Окружения, которых нет в этой таблице, считаются best-effort. Wayland — обязательная
поддерживаемая цель; эта таблица фиксирует только окружения, явно подтверждённые в текущем
состоянии репозитория.

## Быстрый старт

```bash
./manage.sh dev build
./manage.sh doctor
./manage.sh bootstrap linux-input
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

## Настройка горячих клавиш

Ручное исправление и конвертация выделенного текста используют одну и ту же ограниченную модель hotkey.
Комбинация переключения раскладки остаётся отдельной настройкой, потому что она должна совпадать
с поведением переключения раскладки в desktop/session.

Разрешённые trigger-клавиши:
- `F9`
- `F10`
- `F12`
- `Pause`
- `ScrollLock`
- `Insert`
- `Menu`

Каждую trigger-клавишу можно использовать без модификаторов или с любой комбинацией `Shift`,
`Ctrl` и `Alt`. Примеры: `F12`, `Shift+F12`, `Ctrl+Alt+F12`, `Ctrl+Alt+Shift+Insert`.

OpenSwitcher намеренно не принимает произвольные клавиши обычного ввода для этих действий.
Буквы, цифры, `Space`, `Enter`, `Tab`, `Backspace`, `Delete`, `Escape`, стрелки, `F1`-`F8`,
`F11` и `PrintScreen` не разрешены как trigger для ручного исправления или selected-text.

Одно и то же точное сочетание нельзя назначить одновременно на ручное исправление и
конвертацию выделенного текста. Один trigger с разными модификаторами разрешён, поэтому
`F12` и `Shift+F12` могут сосуществовать. Если hotkey содержит текущую комбинацию
переключения раскладки как префикс, настройки показывают предупреждение, но разрешают сохранение.

## Текущие границы и ограничения

- Только Linux
- Подтверждённые окружения перечислены в разделе [Проверенные окружения](#проверенные-окружения)
- Wayland — обязательная поддерживаемая цель
- Настольные окружения Linux и Wayland-композиторы, которых нет в списке проверенных окружений, считаются best-effort
- Основной поддерживаемый сценарий ввода — EN/RU
- Поддержка раскладок и backend-слоя пока остаётся консервативной и опирается на конкретные backend-реализации
- Текущий backend-слой спроектирован с расчётом на расширение, но поддержка пока не охватывает широко все настольные окружения
- Исключения для отдельных приложений не входят в первый релиз
- GNOME Shell extension не входит в первый релиз
- Работа tray зависит от настольного окружения с совместимым хостом StatusNotifier/AppIndicator
- Официальная модель запуска и автозапуска зависит от `systemd --user`
- Окно настроек собирается под feature-флагом Cargo `settings-ui`

## Требования

### Среда выполнения

- настольная Linux-сессия
- сессионный D-Bus
- `systemd --user` для официальной модели запуска и автозапуска
- настольное окружение с совместимым хостом StatusNotifier/AppIndicator для tray

## Linux input setup

OpenSwitcher читает реальные input-устройства из `/dev/input/event*` и пишет виртуальные нажатия через `/dev/uinput`.

Проверить, готова ли текущая сессия:

```bash
./manage.sh doctor
```

Если doctor сообщает об отказе в доступе, запусти официальный setup-шаг:

```bash
./manage.sh bootstrap linux-input
```

Что делает bootstrap:
- устанавливает udev rule проекта из `dist/udev/80-openswitcher-input.rules`
- перезагружает udev rules, если доступен `udevadm`
- применяет same-session ACL bridge для текущего пользователя, если доступен `setfacl`
- повторно запускает `./manage.sh doctor` и подтверждает результат

Этот setup-слой сделан явным специально, чтобы позже его можно было напрямую использовать в packaging без переизобретения Linux input-модели.

Примечания:
- `./manage.sh bootstrap linux-input` работает и без `setfacl`, но same-session ACL bridge применяется только если `setfacl` доступен. В Debian/Ubuntu-подобных системах эта команда обычно приходит из пакета `acl`.
- runtime auto-detect раскладки может использовать environment-specific инструменты, например `gsettings`, `xfconf-query` или `setxkbmap`, в зависимости от текущего окружения.

## Определение переключателя раскладки

OpenSwitcher пытается определить shortcut переключения раскладки по текущему окружению и типу сессии.

Текущее поведение:
- Cinnamon X11 сначала читает настройки клавиатуры Cinnamon
- если Cinnamon settings пустые или не подходят, Cinnamon X11 использует fallback через `setxkbmap -query`
- Xfce X11 и GNOME Wayland имеют отдельные пути определения
- на неподдерживаемых или неизвестных окружениях может понадобиться ручной выбор shortcut в настройках

Найденная настройка сохраняется в конфиге daemon. Ручной выбор, сделанный в окне настроек, сохраняется и не перезаписывается auto-detection.

## Конвертация выделенного текста

Конвертация выделенного текста работает через clipboard:
- OpenSwitcher отправляет copy shortcut, чтобы получить текущее выделение
- конвертирует скопированный текст между EN/RU physical layouts
- временно заменяет clipboard на сконвертированный текст
- отправляет paste shortcut
- затем пытается восстановить предыдущее содержимое clipboard

Захват hotkey может зависеть от физической клавиатуры и desktop environment. На некоторых
ноутбуках функциональные клавиши, `Pause`, `ScrollLock`, `Insert` или `Menu` могут зависеть от
Fn-клавиш, поведения прошивки или глобальных shortcut-ов окружения.

Selected-text debug logging включается только явно. Если он включён, selected-text debug summaries содержат только metadata, например длину и количество строк, без preview текста.

### Зависимости для сборки

Для Linux Mint / Ubuntu-подобных систем:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  libdbus-1-dev \
  libudev-dev \
  libgtk-4-dev \
  libadwaita-1-dev
```

Дополнительные полезные пакеты для Debian/Ubuntu-подобных систем:
- `acl` для `setfacl` во время `./manage.sh bootstrap linux-input`
- `libglib2.0-bin` для `gdbus` в примерах D-Bus ниже

## Сборка

Проверить все бинарники:

```bash
cargo check --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Собрать всё локально:

```bash
cargo build --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
```

Запустить регулярные проверки:

```bash
cargo test -q --lib
cargo test -q --features settings-ui --lib
cargo test --test dbus_api -q
cargo check -q --features settings-ui --bin open-switcher --bin open-switcher-tray --bin open-switcher-settings
git diff --check
```

Опциональная более широкая проверка:

```bash
cargo test -q --all-targets --features settings-ui
```

CI также запускает `settings-ui` feature tests, чтобы feature-gated покрытие не пропускалось случайно.

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
- `dist/icons/hicolor/512x512/apps/open-switcher.png`

Команда `./manage.sh systemd install` устанавливает их в:

- `~/.config/systemd/user/`
- `~/.config/autostart/`
- `~/.local/share/applications/`
- `~/.local/share/icons/hicolor/512x512/apps/`
- `~/.local/bin/`

Примечания:
- desktop-файл запускает сервис tray через `systemctl --user`
- tray unit подтягивает daemon unit
- XDG autostart fallback в `~/.config/autostart/open-switcher.desktop` тоже запускает tray systemd service, а не tray binary напрямую, чтобы автозапуск был надёжнее после входа в desktop session

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

В Debian/Ubuntu-подобных системах `gdbus` обычно входит в пакет `libglib2.0-bin`.

Просмотр сигналов статуса и изменений настроек:

```bash
gdbus monitor \
  --session \
  --dest org.oswitch.core \
  --object-path /org/oswitch/core
```

## Известные ограничения

- OpenSwitcher сейчас ориентирован только на EN/RU-сценарии ввода.
- Подтверждённые окружения перечислены в разделе [Проверенные окружения](#проверенные-окружения).
- Wayland — обязательная поддерживаемая цель.
- Настольные окружения Linux и Wayland-композиторы, которых нет в списке проверенных окружений, считаются best-effort.
- Исключения для отдельных приложений не входят в первый релиз.
- GNOME Shell extension не входит в первый релиз.
- Видимость tray зависит от совместимого StatusNotifier/AppIndicator host.
- Официальная модель запуска и автозапуска зависит от `systemd --user`.
- Конвертация выделенного текста временно использует clipboard и пытается восстановить предыдущее содержимое после конвертации.
- Захват hotkey для ручного исправления и selected-text может зависеть от Fn-клавиш ноутбука и глобальных shortcut-ов окружения.
- Эвристика автокоррекции намеренно консервативна. Некоторые короткие RU -> EN technical false negatives могут оставаться, например `cargo`, `rust`, `sudo`, `git`, `ssh`, `npm`, `jwt`.
- Текущий rustfmt drift и не полностью hermetic shell/platform tests — известный technical debt для будущей cleanup-итерации.

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
