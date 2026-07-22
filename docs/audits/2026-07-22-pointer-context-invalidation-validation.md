# Проверка сброса контекста по настоящему клику

- Дата: 2026-07-22
- Ветка: `fix/audit-remediation`
- Базовый checkpoint: `ac718cd` (`docs: plan pointer and X11 watcher remediation`)
- Реализация evdev: `a53d303` (`fix: distinguish pointer buttons from touch contact`)
- Реализация XInput2: `0f1945d` (`fix: observe logical pointer clicks on X11`)
- Основной артефакт: Debian package `open-switcher 0.1.0-1`, `amd64`
- Статус: изменение A реализовано и проверено; независимая оптимизация 5-мс X11 polling не начиналась

## Результат

Сырой evdev-наблюдатель больше не считает касание, присутствие инструмента или
жест тачпада кликом. Контекст слова сбрасывается только для явного списка
физических кнопок: left, right, middle, side, extra, forward, back и task.

В Cinnamon/X11 добавлена необязательная глобальная подписка XInput2
`RawButtonPress` без захвата указателя. Логические кнопки 1, 2, 3, 8 и 9
сбрасывают контекст, а scroll-кнопки 4–7 игнорируются. Это позволяет учитывать
сформированный X11-клик даже тогда, когда отдельное физическое устройство
недоступно для evdev-наблюдателя. Ошибка XInput2-подписки не отключает прежнее
наблюдение `_NET_ACTIVE_WINDOW`.

Флаги физического и логического клика извлекаются без short-circuit: оба
источника очищаются до объединения результата. Один и тот же клик может быть
виден через evdev и XInput2, но не оставляет отложенный второй сброс.

Прежние правила Enter, Tab, пробела, смены активного окна и Wayland focus policy
не менялись. `src/daemon/service.rs` отсутствует в diff реализации.

## Основные места в коде

- `src/daemon/keyboard.rs:626-704` — отдельный X11-флаг и тип события;
- `src/daemon/keyboard.rs:979-983` — безусловное извлечение обоих атомарных флагов;
- `src/daemon/keyboard.rs:1035-1040` — объединение физического и логического источников;
- `src/daemon/keyboard.rs:1310-1360` — evdev-наблюдатель физических кнопок;
- `src/daemon/keyboard.rs:1418-1530` — публикация X11-событий watcher-потоком;
- `src/daemon/keyboard.rs:2250-2340` — необязательная XInput2-подписка без grab;
- `src/daemon/keyboard.rs:2717-2733` — точные классификаторы evdev и X11;
- `src/daemon/keyboard.rs:6725-6875` — регрессионные тесты;
- `Cargo.toml:41` — feature `xinput` у прежней версии `x11rb 0.13.2`.

## TDD и локальная верификация

RED-проверки наблюдались до реализации:

- тест запрета touch-событий падал на `BTN_TOUCH` при прежнем числовом диапазоне;
- X11-классификатор, combined drain и X11-event pipeline сначала не компилировались,
  потому что соответствующих функций и полей ещё не существовало.

После реализации:

| Проверка | Результат |
|---|---|
| raw evdev pointer classifiers | pass |
| X11 detail classifiers и event mask | pass |
| physical/logical combined drain | pass |
| input-target watcher readiness | pass |
| все `daemon::keyboard` tests | 115 passed |
| точный Enter regression test | pass |
| точный Tab regression test | pass |
| точный space regression test | pass |
| Wayland focus policy tests | 3 passed |
| base lib matrix в restricted host sandbox | 563 passed; 9 socket tests получили `EPERM` |
| settings-ui lib matrix в restricted host sandbox | 624 passed; те же 9 socket tests получили `EPERM` |
| те же exact base test executables внутри VM | 572 passed, 0 failed |
| те же exact settings-ui test executables внутри VM | 633 passed, 0 failed |
| targeted stable rustfmt check для `src/daemon/keyboard.rs` | pass |
| `git diff --check ac718cd..0f1945d` | pass |

Таким образом, девять tests не исключены из итоговой проверки: ограничение на
создание Unix/D-Bus sockets существовало только в host sandbox, а те же
скомпилированные test executables полностью прошли внутри VM.

У зафиксированного exact toolchain `1.95` отсутствовал компонент `rustfmt`.
Полный stable-format check репозитория затрагивает прежние несвязанные файлы;
изменённый Rust-файл отдельно проходит stable `rustfmt --check`.

## Идентичность Debian package

- build command, дошедшая до package payload: `DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb`;
- причина `nocheck`: обычный package flow останавливался только на девяти
  запрещённых host sandbox socket tests;
- эти exact tests затем полностью прошли в VM, как указано выше;
- package: `dist/packages/open-switcher_0.1.0-1_amd64.deb`;
- размер: `3 050 820` bytes;
- SHA-256 package:
  `9a6d20849086431d9dbf8f62630e23e3a9a410dd69ab23964c4d2cc407aca33e`;
- SHA-256 packaged daemon:
  `eb9d1def8bf4c63411a977606865ea02a24284d87709bbc3392487961a5c6bcd`.

Именно этот package установлен в сохранённой Linux Mint VM. `dpkg-query`
подтвердил `open-switcher 0.1.0-1`, а SHA-256 запущенного
`/usr/bin/open-switcher-daemon` совпал с packaged daemon.

## Package-first проверка в Mint/Cinnamon X11

Граница проверки:

- использована сохранённая VM `mint-install-v1`, лаборатория не перестраивалась
  и не удалялась;
- host `/dev/input`, `/dev/uinput`, clipboard, раскладка, systemd и udev не
  изменялись;
- ввод отправлялся только виртуальным QEMU USB keyboard/tablet внутри гостя;
- для проверки временно включался ограниченный input-debug без содержимого
  набранного текста; после матрицы debug environment снят и daemon перезапущен.

| Сценарий установленного package | Результат |
|---|---|
| `ыгвщ` и сразу F12 в текущем окне | целиком исправлено в `sudo` |
| первое слово через 120 ms после появления нового окна и сразу F12 | целиком исправлено в `sudo` |
| движение USB tablet между словом и F12 | контекст сохранён, слово исправлено |
| wheel-down press/release между словом и F12 | контекст сохранён, слово исправлено |
| QEMU `touch` request между словом и F12 | контекст сохранён, слово исправлено; raw `BTN_TOUCH` delivery не подтверждена |
| раздельный left press/release между словом и F12 | контекст сброшен, старое слово не исправлено |
| X11 buttons 1, 2 и 3 | каждый дал XInput2 click и evdev physical click |
| обычное Super+Space переключение | pass; использовалось во всей матрице |
| auto correction `ghbdtn ` | получено `привет ` и Russian layout |
| исправление двух заглавных `ПРивет ` | получено `Привет ` |
| исправление случайного Caps Lock `пРИВЕТ ` | получено `Привет `; следующая буква строчная |
| Enter после неверного слова, затем F12 | старое слово не исправлено |
| Tab после неверного слова, затем F12 | старое слово не исправлено |
| пробел и ручная коррекция предыдущего слова | слово исправлено, пробел восстановлен |

Debug-журнал подтвердил для движения и scroll отсутствие `pointer-click`.
QEMU `touch` request также не дал `pointer-click`, но отдельный raw-capture не
подтвердил, что виртуальный tablet действительно отправил `BTN_TOUCH`, поэтому
этот прогон не считается hardware-проверкой touch contact. Настоящий left click
был одновременно виден как
`source=xinput2 detail=1` и как `BTN_LEFT`, после чего service-loop зафиксировал
инвалидацию контекста. Для кнопок 2 и 3 получены соответственно XInput details
2 и 3 и evdev `BTN_MIDDLE`/`BTN_RIGHT`.

Сверхранний искусственный запуск ввода через 20 ms после команды создания окна
поместил первую клавишу ещё в прежнее окно; новое окно получило только три
следующие буквы. Это отличие от исходного дефекта: в целевом окне самой первой
буквы не было до F12. После фактического появления окна сценарии с 120 ms и
450 ms прошли полностью. Результат 20-ms прогона относится к границе фокуса
самого GUI/QMP harness и не выдан за дефект коррекции.

После тестов `open-switcher-daemon.service` и `open-switcher-tray.service`
остались `active`; запущен один `/usr/bin/open-switcher-daemon`. В текущем
запуске не найдено panic, смерти обязательного watcher или ошибки grab.
Старые записи `Dbus(NameTaken)` относятся к состоянию VM до установки этого
candidate и не повторились после нормализации и перезапуска службы.

## Не покрыто VM

- Виртуальный QEMU tablet не является реальным тачпадом с libinput и настройкой
  tap-to-click. Он не дал независимо наблюдаемого raw `BTN_TOUCH`, поэтому
  запрет этого кода подтверждён точным unit test, но не runtime. Поведение
  конкретного физического тачпада остаётся ручной hardware-проверкой пользователя.
- Виртуальный tablet предоставляет только buttons 1–7. X11/evdev buttons 8 и 9
  не удалось получить runtime; их принимаемые значения покрыты точными unit tests.
- Wayland runtime в этом slice не запускался. Сохранены и пройдены прежние
  Wayland focus policy tests; новая XInput2-ветвь включается только в X11.
- Отказ XInput2 на реальном старом X server не инъецировался. Код оставляет
  `_NET_ACTIVE_WINDOW` watcher работающим, но degraded path подтверждён только
  статически и unit-границами.

## Polling и остаточный риск

`INPUT_TARGET_POLL_INTERVAL` намеренно остался равен 5 ms, а
`POINTER_POLL_INTERVAL` — 20 ms. Событийная оптимизация ожидания относится к
следующему независимому плану и в эту реализацию не смешивалась.

Изменение A можно считать прошедшим package-first acceptance для
Mint/Cinnamon/X11. Оно подтверждает требуемое различие между движением,
прокруткой, touch-contact и настоящим кликом, но не заменяет короткое наблюдение
на реальном тачпаде пользователя. Лаборатория сохранена для будущих проверок.
