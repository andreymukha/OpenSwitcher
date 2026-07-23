# Проверка подтверждённой остановки virtual writer

- Дата: 2026-07-23
- Ветка: `fix/quiescent-writer-shutdown`
- База: `b5b99d9` (`docs: plan acknowledged writer shutdown`)
- Проверенный диапазон: `b5b99d9..ebf6303`
- Основной артефакт: Debian package `open-switcher 0.1.0-2`, `amd64`
- Runtime-среды: Linux Mint/Cinnamon/X11 и Ubuntu 24.04/GNOME/Wayland
- Статус: штатный shutdown и детерминированные fail-stop границы прошли;
  writer-specific runtime fault injection не выполнен из-за невозможности
  доказанно остановить только writer TID у stripped release-бинарника

## Что исправлено

До этой работы остановка virtual keyboard writer не имела подтверждения того,
что поток действительно завершился и принадлежащее ему `/dev/uinput` устройство
уничтожено. Короткое ожидание могло закончиться ложным успехом, после чего тот
же процесс мог открыть новый input backend рядом с ещё живым старым writer.

Теперь действует единый контракт `WriterShutdownOutcome`:

1. Writer получает sticky `stop_requested`, поэтому новые mutation permit после
   начала остановки запрещены независимо от заполненности command queue.
2. `Stopped` публикуется только после возврата writer loop, уничтожения owned
   virtual device и успешного `JoinHandle::join`.
3. Ожидание подтверждения ограничено одной секундой от первого stop request.
4. При timeout результат навсегда фиксируется как
   `Unresponsive { timeout_ms }`; поздний выход потока не разрешает recovery в
   том же процессе и не превращает результат обратно в `Stopped`.
5. Физический keyboard grab освобождается до ожидания writer. Если
   `EVIOCGRAB(0)` возвращает ошибку, fd физической клавиатуры немедленно
   закрывается до любых ожиданий, что даёт ядру освободить grab по close.
6. Watcher'ы штатно join'ятся только после подтверждённого выхода writer. На
   fail-stop пути им публикуется stop, а handles отсоединяются, чтобы `Drop` не
   смог снова заблокировать завершение процесса.
7. Startup, partial initialization, readiness failure, runtime health failure,
   D-Bus reset и обычный daemon finalizer сохраняют typed outcome без потери.
8. После `Unresponsive` запрещены повторное открытие backend и повторный
   `/dev/uinput`; daemon возвращает ошибку, а существующая systemd policy может
   запустить новый чистый процесс.
9. Предшествующая typed fail-stop ошибка не маскируется повторным shutdown в
   `Drop` или finalizer.

Основные места в коде:

- `src/daemon/keyboard.rs:84` — единый `WriterShutdownOutcome`;
- `src/daemon/keyboard.rs:1266` — release-first shutdown controller;
- `src/daemon/keyboard.rs:1457` — close fd при неуспешном ungrab;
- `src/daemon/keyboard.rs:2109` — sticky stop request writer;
- `src/daemon/keyboard.rs:2120` — bounded finish/ACK;
- `src/daemon/keyboard.rs:2197` — join только после фактического завершения;
- `src/daemon/input_backend.rs:173` — latch необратимого fail-stop;
- `src/daemon/input_backend.rs:264` — запрет второго backend после latch;
- `src/daemon/service.rs:1142` и `:2652` — распространение outcome через
  service shutdown и runtime failure;
- `src/daemon/mod.rs:163` — финальный shutdown и ненулевой daemon result;
- `src/dbus/mod.rs:78` — неблокирующий detach monitor на process fail-stop;
- `src/error/mod.rs:127` — typed
  `VirtualKeyboardWriterShutdownUnresponsive`.

## TDD и локальная проверка

RED-проверки до production-изменений зафиксировали отсутствие outcome/ACK,
потерю `JoinHandle`, возможность повторного backend open и маскировку ошибки на
финальных путях. После GREEN проверены обычный выход, panic/error writer,
полная очередь, disconnected command receiver, поздний выход, startup timeout,
partial open, runtime recovery, D-Bus reset, monitor detach и daemon finalizer.

Отдельное независимое review нашло один реальный gap: при ошибке
`EVIOCGRAB(0)` fd оставался открыт до ожидания writer. Исправление перевело
владение физическим `Device` в `Option` и закрывает fd немедленно на этом пути.
Тесты `failed_ungrab_closes_device_before_shutdown_waits` и
`successful_ungrab_keeps_open_device_available_until_controller_drop`
фиксируют оба варианта. Повторное read-only review дало вердикт `READY` и не
нашло новых Critical/Important замечаний.

| Проверка | Результат |
|---|---|
| `writer_stop` | 13 passed |
| `keyboard_shutdown` | 4 passed |
| `daemon::input_backend` | 19 passed |
| `runtime_health_failure` | 2 passed |
| `unresponsive_shutdown` | 5 passed |
| `ungrab` | 2 passed |
| полная base library matrix | 634 passed, 0 failed |
| полная `settings-ui` library matrix | 695 passed, 0 failed |
| vendored `uinput` ownership tests | 3 passed, 0 failed |
| `cargo check --locked --offline --all-targets` | pass |
| `settings-ui` all-target check | pass |
| Wayland diagnostics | pass |
| Debian/package shell tests | pass |
| targeted `rustfmt --check` | pass |
| `git diff --check` | pass |

Существующие предупреждения относятся к deprecated API vendored
`uinput 0.1.3` и прежним dead-code methods. Новых `unsafe`, polling interval,
layout/correction delay, clipboard, udev/ACL или systemd policy в диапазоне нет.

## Идентичность Debian package

- build command: `DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb`;
- package: `dist/packages/open-switcher_0.1.0-2_amd64.deb`;
- размер: `3 027 178` bytes;
- SHA-256 package:
  `6dc7ccb8ca3a1f326475072ec1a2b001f9595d135ace4431e56cf08ffdaf3acd`;
- SHA-256 packaged daemon:
  `e83f3ec06570be36c4ebc3d7bb5bed9109af0a0344ada7939af99f4b0d9b1129`.

Один и тот же exact file передан в обе VM. В каждой гостевой системе SHA
переданного DEB совпал, `dpkg-query` показал `open-switcher 0.1.0-2`, SHA
`/usr/bin/open-switcher-daemon` совпал с packaged daemon, а
`/proc/$PID/exe` указывал на `/usr/bin/open-switcher-daemon` без `(deleted)`.
После upgrade из-за отдельно известного `M-09a` выполнены явные
`systemctl --user daemon-reload` и restart.

## Package-first проверка Mint/Cinnamon/X11

Граница проверки:

- использована сохранённая VM `mint-install-v1`;
- ввод отправлялся только QEMU USB keyboard/tablet внутри гостя;
- временно включался обезличенный input-debug без текста пользователя;
- после проверки debug manager environment снят, тестовый редактор закрыт,
  первая раскладка восстановлена и service оставлен active до остановки VM.

| Сценарий | Результат |
|---|---|
| первое `ыгвщ` в новом окне, затем F12 | целиком преобразовано в `sudo` |
| auto correction `ghbdtn ` | получено `привет ` |
| две заглавные `ПРивет ` | получено `Привет ` |
| случайный Caps Lock `пРИВЕТ ` | получено `Привет ` |
| Enter/Tab после слова | старый контекст не преобразован |
| Space и коррекция предыдущего слова | слово и separator восстановлены |
| движение tablet между словом и F12 | контекст сохранён, `ыгвщ -> sudo` |
| wheel-up между словом и F12 | контекст сохранён, `ыгвщ -> sudo` |
| раздельный left press/release, затем F12 | слово не изменилось |
| 10 paced stop/start циклов | pass |

Для клика debug-журнал подтвердил два независимых источника:
`source=xinput2 detail=1` и `device=QEMU QEMU USB Tablet key=BTN_LEFT`, после
чего service зафиксировал `pointer-invalidation`. QMP press/release пришлось
отправлять раздельно: одна batch-команда схлопывала состояние и не являлась
валидным тестом клика.

Время штатного stop в десяти paced циклах:
`79, 124, 119, 96, 115, 123, 116, 96, 106, 94 ms`. После каждого цикла service
запускался с новым PID; в финальном процессе был ровно один `/dev/uinput` fd.
`VirtualKeyboardWriterShutdownUnresponsive` не наблюдался.

Первая версия harness запустила пять start менее чем за секунду и на шестом
получила стандартный systemd `start-limit-hit` (`StartLimitBurst=5`, интервал
10 s). Это не отказ OpenSwitcher: цикл не выдерживал штатный интервал. Counter
был сброшен только внутри гостя, после чего paced matrix выше прошла. Systemd
unit и его policy не менялись.

Реальный raw `BTN_TOUCH` от физического тачпада эта VM создать не может. Его
запрет подтверждён unit-классификатором и предыдущей package-first проверкой;
конкретный hardware tap-to-click остаётся ручной проверкой пользователя.

## Package-first проверка Ubuntu/GNOME/Wayland

Использована сохранённая Ubuntu `24.04.4 LTS` VM с живым
`XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-0` и GNOME. Проверка не
выдаётся за XTest: физические qcode-события пришли только от QEMU USB keyboard,
а коррекция прошла через установленный daemon.

| Сценарий | Результат |
|---|---|
| первое `ыгвщ` в новом GNOME Text Editor, затем F12 | `sudo`, RU -> EN |
| auto correction `ghbdtn ` | `привет `, EN -> RU |
| две заглавные `ПРивет ` | `Привет ` |
| 10 paced stop/start циклов | pass |

Stop latencies составили `92, 89, 76, 97, 90, 79, 96, 81, 81, 81 ms`.
После матрицы service был `active`, `Result=success`, `NRestarts=0`, текущий
процесс имел ровно один `/dev/uinput` fd. В journal текущей загрузки нет
`Unresponsive` и `start-limit-hit`.

## Writer-specific fault injection

Согласованный safety gate требовал доказанно остановить только writer TID после
mutation permit. Production writer создан через безымянный `thread::spawn`, а
release-бинарник в DEB stripped. Несколько потоков одновременно ожидают через
одинаковые futex/poll границы; `/proc/$PID/task/*/comm` и стек без символов не
дают надёжно отличить writer от service/watcher.

`SIGSTOP` даже конкретному TID создаёт process-wide group stop, поэтому он не
подходит. Пытаться угадать TID по timing или syscall означало бы нарушить
условие теста и могло бы остановить весь daemon вместо одного writer. В
соответствии с планом runtime fault injection не запускался.

Детерминированные fake-thread тесты при этом подтверждают:

- timeout сохраняет живой `JoinHandle` и sticky `Unresponsive`;
- grab освобождается или fd закрывается до ожидания ACK;
- после timeout второй opener/backend не вызывается;
- поздний выход writer не снимает fail-stop;
- реальный `Drop` после latched timeout не повторяет ожидание;
- typed ошибка доходит до daemon finalizer и требует ненулевого выхода.

## Ограничения и остаточные риски

1. Полная цепочка «реально зависший production writer -> release grab ->
   ненулевой exit -> systemd restart -> один новый uinput» не подтверждена
   runtime-инъекцией. Это главный остаточный риск этой работы.
2. Ошибки partial prepare реального hardware и конкретные watcher objects с
   заблокированным OS handle не имеют прямого end-to-end unit seam. Порядок и
   typed routing покрыты детерминированными helpers, кодом и review.
3. Нельзя экспериментально доказать поведение конкретного kernel/driver, если
   сам `close(2)` fd физической клавиатуры зависнет в ядре. Для обычного Linux
   close освобождает grab, но такой kernel failure находится за пределами
   userspace-гарантии.
4. Реальный тачпад, unplug/replug USB-клавиатуры и suspend/resume в этой матрице
   не воспроизводились.
5. Независимые следующие фронты аудита не входят в ветку: conservation
   deferred physical events, operation-wide synthetic key ledger,
   transactional clipboard/selected-text и package remove/ACL boundary.

## Итоговая оценка

Штатный механизм остановки можно считать подтверждённо bounded и
release-first: в двух установленных DEB-средах двадцать clean stop/start циклов
прошли менее чем за 125 ms, без утечки uinput fd и без старого executable.
Статически и детерминированно закрыты ложный успешный shutdown, повторный
backend рядом с неподтверждённым writer, blocking Drop и удержание grab после
ошибки ungrab.

End-to-end механизм при реально зависшем production writer пока нельзя честно
назвать полностью runtime fail-safe: необходим writer-specific fault seam либо
отдельная диагностическая сборка с однозначной идентификацией writer TID.
До такой проверки корректная формулировка — «fail-stop архитектура реализована
и детерминированно проверена; production hang injection остаётся ограничением».

Host input, clipboard, layout, systemd, udev, ACL и конфигурация пользовательской
сессии во время VM-проверок не менялись. Лаборатория, диски, профили и evidence
сохранены; ничего не удалялось.
