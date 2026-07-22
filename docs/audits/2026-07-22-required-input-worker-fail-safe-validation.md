# Проверка fail-safe восстановления обязательных input worker

- Дата: 2026-07-22
- Ветка проверки: `perf/wakeable-x11-watcher`
- База ветки: `94f0372` (`docs: validate pointer context invalidation`)
- Реализация fail-safe: `c8976a2..c3d3beb`; два найденных review-gap закрыты
  commit `c3d3beb`
- Исправление владения `/dev/uinput`: vendoring `a1acf42`, RAII-fix `675b4d7`
- Основной артефакт: Debian package `open-switcher 0.1.0-1`, `amd64`
- Среда runtime: сохранённая VM `mint-install-v1`, Linux Mint/Cinnamon/X11
- Статус: целевой отказ X11 watcher, в том числе во время активной
  синхронной коррекции, startup без X11 endpoint, потеря последнего pointer
  device, утечка uinput fd при recovery и автоматическое восстановление закрыты
  кодом и package-first проверкой в доступных границах

## Что исправлено

До этого изменения обязательный `input-target-watcher` мог завершиться после
ошибки X11, но daemon продолжал удерживать физическую клавиатуру через
`EVIOCGRAB`. Атомарный признак смерти существовал, однако runtime-loop его не
читал. Отдельно X11 startup допускал продолжение без работающего monitor.

Во время повторных recovery-проверок обнаружен ещё один подтверждённый дефект
серьёзности Medium: зависимость `uinput 0.1.3` уничтожала виртуальное устройство
через `UI_DEV_DESTROY`, но не закрывала принадлежащий `Device` файловый
дескриптор. У `Builder` также отсутствовал `Drop`, поэтому fd утекал на
ошибочных и прерванных путях создания. При каждом восстановлении backend daemon
мог накапливать ещё один открытый `/dev/uinput` fd.

Теперь действуют следующие границы:

1. Перед первым grab одновременно проверяется готовность writer, pointer watcher
   и обязательного X11 input-target watcher.
2. Ошибка первоначального подключения X11 monitor нормализуется в recoverable
   `InputWorkerDisconnected("input-target-watcher")`; grab не выполняется.
3. Runtime health-check проверяет writer и оба уже запущенных обязательных
   watcher до чтения, после чтения и перед каждым событием полученного batch.
4. Смерть обязательного watcher переводит lifecycle из `Ready` в `Recovering`,
   сбрасывает временный input-контекст и немедленно удаляет активный backend.
5. Существующий shutdown order сначала просит writer остановиться, затем снимает
   `EVIOCGRAB`, завершает writer и только после этого присоединяет watcher.
6. Повторный grab разрешён только после полной подготовки нового backend.
7. Ожидание синхронной коррекции теперь наблюдает health обязательных input
   worker с шагом до 5 ms. Если worker потерян, общий transaction stop flag
   прерывает writer-side удаление/ввод вместо ожидания полного deadline 5 s.
   Уже поставленный в reply-channel подробный writer result имеет приоритет над
   одновременно замеченной смертью watcher.
8. Если pointer watcher успешно стартовал хотя бы с одним устройством, но затем
   все открытые pointer device сообщили ошибку или исчезли, worker завершается
   как unavailable. Общий lifecycle снимает keyboard grab и пересобирает весь
   input pipeline. Политика optional startup при исходных нуле pointer devices
   не менялась.
9. В точной локальной копии `uinput 0.1.3` `Builder` теперь закрывает свой fd на
   всех путях `Drop`, а успешный `create` явно передаёт владение `Device` и
   помечает исходный fd как переданный.
10. `Device::drop` сначала выполняет best-effort `UI_DEV_DESTROY`, затем всегда
    закрывает fd и помечает его закрытым. Это исключает и утечку, и двойное
    закрытие при штатном уничтожении объекта.

Основные места в коде:

- `src/daemon/keyboard.rs:992` — публикация readiness обязательного worker;
- `src/daemon/keyboard.rs:1040` — общий readiness-контракт до grab;
- `src/daemon/keyboard.rs:1096` — обязательное создание X11 monitor;
- `src/daemon/keyboard.rs:1251` — runtime health обязательных worker;
- `src/daemon/input_backend.rs:167` — классификация runtime-отказа;
- `src/daemon/input_backend.rs:174` — переход в `Recovering`;
- `src/daemon/service.rs:165` — маршрутизация health-ошибки;
- `src/daemon/service.rs:186` и `:201` — health-границы операции и batch;
- `src/daemon/service.rs:1098` — общая runtime-проверка backend;
- `src/daemon/service.rs:2565` — reset и удаление активного backend;
- `src/daemon/keyboard.rs:455` — terminal gate и stop активной transaction при
  смерти обязательного input worker;
- `src/daemon/keyboard.rs:483` — bounded ожидание writer reply с input-health;
- `src/daemon/keyboard.rs:1394` — удаление потерянных pointer devices и переход
  N -> 0 в unavailable worker;
- `src/daemon/keyboard.rs:2283` — подключение watcher health к синхронным
  correction transaction;
- `vendor/uinput-0.1.3/src/device/builder.rs:258` — явная передача fd и
  `Builder::drop`;
- `vendor/uinput-0.1.3/src/device/device.rs:78` — уничтожение устройства и
  обязательный `close` fd.

## TDD и локальная проверка

До production-кода наблюдались RED-проверки для каждой новой границы:

- отсутствие общего watcher health helper;
- отсутствие обязательной X11 startup policy;
- `InputWorkerDisconnected` не переводил lifecycle в `Recovering`;
- health-ошибка не удаляла backend и не прекращала текущую обработку.
- активная синхронная transaction могла не увидеть смерть watcher до deadline;
- потеря последнего из ранее открытых pointer devices не завершала watcher;
- watcher health мог скрыть уже поставленный в очередь подробный writer error;
- `Device::drop` оставлял свой fd открытым после `UI_DEV_DESTROY`;
- `Builder::drop` оставлял fd открытым на error/abandon path;
- передача fd от `Builder` к `Device` не имела проверяемой модели владения.

После GREEN:

| Проверка | Результат |
|---|---|
| targeted watcher readiness/health tests | pass |
| targeted X11 startup policy tests | pass |
| targeted lifecycle recovery tests | pass |
| targeted service operation/batch routing tests | pass |
| полная base library matrix | 590 passed, 0 failed |
| полная `settings-ui` library matrix | 651 passed, 0 failed |
| vendored `uinput` ownership tests | 3 passed, 0 failed |
| package D-Bus matrix | 11 passed, 0 failed |
| package shell checks | pass |
| targeted `rustfmt --check` изменённых Rust-файлов | pass |
| `git diff --check` | pass |
| поиск нового `unsafe` | новый `unsafe` отсутствует |

Некоторые stop-socket tests нельзя корректно выполнить в restricted host
sandbox: sandbox запрещает `shutdown(2)` с `EPERM` и тест зависает в ожидании
wakeup. Тот же sibling test подтвердил именно `EPERM`. Эти tests были повторены
вне restricted sandbox и прошли; обе полные матрицы выше также завершились
успешно.

Полный `cargo fmt --check` всё ещё показывает существовавший до этой ветки drift
в `src/config.rs`, `src/model.rs` и `src/tray/tray_service.rs`. Изменённые в этой
работе `src/daemon/keyboard.rs`, `src/daemon/input_backend.rs` и
`src/daemon/service.rs` отдельно проходят pinned `rustfmt 1.95`.

Локальная копия старого `uinput 0.1.3` при сборке показывает 45 предупреждений
о давно deprecated API (`try!`, `Error::description`). Они присутствуют в
точном upstream-коде зависимости и не означают новые ошибки компиляции. В этой
ветке намеренно изменено только владение fd, без широкой миграции backend.

## Идентичность Debian package

- финальный build command: `DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb`;
- package: `dist/packages/open-switcher_0.1.0-1_amd64.deb`;
- размер: `3 079 132` bytes;
- SHA-256 package:
  `e4eecab763d4335c8591852df0627886d4569281390b17cd7a0d36885c60dcfd`;
- SHA-256 packaged daemon:
  `4dde6886b981aa86fa15b33a6dd0b295aecd9aa91098c1efa094e81a9e024174`.

Перед финальной сборкой обе полные Rust-матрицы были запущены заново, поэтому
точная финальная пересборка выполнялась с `DEB_BUILD_OPTIONS=nocheck` и не
дублировала их третий раз. Ранее package flow также подтвердил 11 D-Bus tests и
shell checks. Остались только прежние lintian warnings по AppStream metadata,
changelog и manpages.

Именно этот package установлен в VM. SHA-256 запущенного
`/usr/bin/open-switcher-daemon` совпадает с hash packaged daemon.

## Fault injection работающего X11 watcher

Граница эксперимента:

- ввод и `EVIOCGRAB` проверялись только на QEMU USB keyboard `/dev/input/event4`;
- host input, clipboard, layout, systemd и udev не менялись;
- временно включался только обезличенный input-debug;
- через root/gdb выполнен `shutdown(SHUT_RDWR)` только подтверждённого X11 fd;
- независимый bounded probe при успехе немедленно отпускал устройство.

Перед инъекцией:

- daemon PID: `99676`;
- старый watcher TID: `99712`;
- gdb-инспекция `pollfd` подтвердила X11 fd `16` и wakeup fd `18`;
- устройство было занято OpenSwitcher (`EVIOCGRAB` возвращал `EBUSY`).

Результат:

| Проверка | Наблюдение |
|---|---|
| точечный `shutdown(16, SHUT_RDWR)` | gdb вернул `0` |
| старый watcher | TID `99712` завершился |
| обнаружение смерти | `input-worker-health-error`, затем `Ready -> Recovering` |
| сброс состояния | `transient-input-reset reason=input-backend-unavailable` |
| снятие grab | `grab-released keyboard grab released during shutdown` |
| независимый probe | реальный успешный `EVIOCGRAB`, затем немедленный release |
| время probe после detach gdb | `2.646 ms` |
| daemon PID | сохранился `99676` |
| автоматическое восстановление | новый pipeline готов приблизительно через `1.079 s` |
| повторный grab | независимый probe снова получил `EBUSY` |
| watcher threads | старые TID заменены новыми, общее число потоков восстановилось |
| CPU после recovery | 14 ticks за 5 s при `CLK_TCK=100`, около `2.8%` одного CPU |

Journal показывает, что watcher сообщил ошибку на monotonic timestamp
`30250.278504`, а health gate, переход в `Recovering` и release grab произошли
на `30250.301674`: примерно через `23.17 ms`. Новый полный pipeline был готов на
`30251.380695`. Busy loop и restart daemon не возникли.

### Отказ во время активной синхронной коррекции

Финальное review обнаружило непокрытую ветвь: event-handler мог уже ожидать
ответ длинной correction transaction и до её deadline не возвращаться к общему
backend health gate. Для проверки исправления в установленном package были
временно выставлены `backspace_ms=10` и `typing_ms=10`, затем запущена коррекция
100-символьного слова. Journal подтвердил `buffer_len=100` и начало transaction
до fault injection.

Во время этой transaction был закрыт точно установленный X11 connection fd
watcher. Получены следующие monotonic timestamps:

- начало Space, запускающего correction: `1784747008120033509` ns;
- начало инъекции: `1784747008603333717` ns, то есть через `483.30 ms`;
- завершение инъекции и detach gdb: `1784747008791092311` ns;
- первый успешный независимый `EVIOCGRAB`: `1784747008798455478` ns.

Таким образом, grab стал доступен другому процессу через `7.363 ms` после
завершения инъекции, а не после 5-секундного deadline transaction. Journal
зафиксировал точный `InputWorkerDisconnected("input-target-watcher")`, переход
`Ready -> Recovering`, transient reset, `grab-released`, остановку writer и
последующую подготовку нового pipeline. PID daemon `133397` сохранился.

### Потеря последнего pointer device

Вторая непокрытая ветвь финального review состояла в том, что pointer watcher
мог продолжать считаться живым после ошибок всех устройств, которые были
успешно открыты при startup. Теперь устройства с ошибкой удаляются из рабочего
набора, а переход N -> 0 завершает worker как unavailable и запускает общий
fail-safe recovery. Граница подтверждена детерминированным unit test
`pointer_poll_cycle_stops_after_last_open_device_is_lost`. Физическое unplug
реального тачпада не выполнялось и отдельно отмечено в ограничениях.

### Повторные recovery и владение `/dev/uinput`

После RAII-исправления в одном процессе daemon выполнены три последовательных
закрытия подтверждённого X11 fd обязательного watcher. В каждом цикле
независимый probe реально получал и сразу отпускал `EVIOCGRAB`, после чего тот же
daemon автоматически собирал новый pipeline и снова захватывал устройство.

| Цикл | Доступность grab после fault | Состояние после recovery |
|---:|---:|---|
| 1 | `282.967 ms` | PID `162433`, 1 uinput fd, 1 virtual device, 12 threads |
| 2 | `256.882 ms` | PID `162433`, 1 uinput fd, 1 virtual device, 12 threads |
| 3 | `263.764 ms` | PID `162433`, 1 uinput fd, 1 virtual device, 12 threads |

Итог `3/3`: процесс не перезапускался, число `/dev/uinput` fd и виртуальных
устройств не росло. Последующий timeout независимого probe в каждом цикле
подтвердил, что восстановленный OpenSwitcher снова выполнил grab, а не просто
остался в отключённом состоянии.

## Startup/retry без X11 endpoint

Для отдельного startup-сценария только пользовательскому сервису внутри VM был
временно задан `DISPLAY=:99`, где X server отсутствовал.

Наблюдения до появления endpoint:

- daemon оставался `active` с PID `102180`;
- backend не переходил в готовое состояние;
- log фиксировал `input-target-watcher-start-error` и recoverable background
  retry с ограниченным backoff;
- частично созданные writer и pointer watcher штатно завершались;
- независимый `EVIOCGRAB` probe успешно получил и отпустил клавиатуру, то есть
  OpenSwitcher не захватывал её без обязательного X11 monitor.

Затем внутри гостя был временно запущен вложенный Xephyr `:99`. Без перезапуска
daemon и с тем же PID `102180` следующий retry создал X11 watcher, подготовил
полный pipeline и выполнил grab. Независимый probe получил `EBUSY`.

После проверки Xephyr завершён, socket `X99` отсутствует, manager environment
возвращён к `DISPLAY=:0`, debug-переменные удалены, а сервис перезапущен в
штатном режиме.

## Stop/start

Правильный прогон состоял из 10 разнесённых циклов. В каждом цикле:

- stop завершился менее чем за 1 секунду;
- независимый probe после stop успешно получал grab;
- после start pipeline возвращался к 12 потокам;
- независимый probe после готовности получал `EBUSY`.

Точные monotonic journal timestamps дали:

| Метрика | Минимум | Среднее | Максимум |
|---|---:|---:|---:|
| stop до `Stopped` | 80.451 ms | 89.846 ms | 107.987 ms |
| `Started` до сообщения полной готовности | 590.817 ms | 622.880 ms | 644.630 ms |

После цикла: `ActiveState=active`, `SubState=running`, `Result=success`,
`NRestarts=0`.

Первый вариант harness ошибочно выполнил старты без интервалов и на шестом
старте закономерно достиг системного `StartLimitBurst=5` за 10 секунд. Это не
авария OpenSwitcher: пять предшествующих stop/release/start/regrab прошли, после
`reset-failed` и соблюдения интервала все требуемые 10 циклов завершились. Этот
случай важен как ограничение методики и не скрывается из отчёта.

## Функциональная регрессия установленного package

QEMU ввод направлялся через `video0` к виртуальным QEMU HID keyboard/tablet.
Файлы сохранялись Xed внутри гостя и читались обратно через SSH.

| Сценарий | Результат |
|---|---|
| 20 отдельных новых Xed-окон, `ыгвщ`, сразу F12 | 20/20 получили ровно `sudo`; PID daemon не изменился |
| движение tablet между словом и F12 | контекст сохранён, `sudo` |
| wheel-down между словом и F12 | контекст сохранён, `sudo` |
| left click между словом и F12 | контекст сброшен, осталось `ыгвщ` |
| Enter между словом и F12 | контекст сброшен, осталось `ыгвщ` и введённый перевод строки |
| Tab между словом и F12 | контекст сброшен, осталось `ыгвщ` и четыре пробела |
| пробел и F12 | предыдущее слово исправлено, пробел восстановлен |
| auto correction `ghbdtn ` | `привет ` |
| две заглавные `ПРивет ` | `Привет ` |
| обычный Super+Space | English -> Russian -> English, pass |

Первоначальный oracle Enter ожидал один `\n` в файле и отметил raw результат
`ыгвщ\n\n` как FAIL. Это ошибка oracle: один перевод строки ввёл Enter, второй
добавил Xed как завершающий перевод строки. Сам сценарий контекстного сброса
прошёл правильно. После исправления oracle вся матрица повторена одним чистым
прогоном: `8/8`, start PID и end PID равны `141526`.

После финального uinput package матрица была повторена ещё раз. Первый прогон
дал ложные `7/8`: harness устанавливал Russian только в начале серии, хотя
успешный F12 штатно переключает текущую раскладку. Поэтому следующая попытка
могла начинаться в English и корректно преобразовываться обратно в `ыгвщ`.
Обезличенный debug-log при этом фиксировал успешную correction transaction и
не содержал pointer-click/context-reset. Harness исправлен: перед каждым
сценарием он читает фактическую группу и при необходимости использует обычный
`Super+Space`.

Чистый итоговый прогон дал `8/8`, start PID и end PID равны `167981`.
Дополнительно сценарий «новое окно -> `ыгвщ` -> движение tablet -> F12» повторён
10 раз: `10/10` получили `sudo`, start PID и end PID также равны `167981`.

Отдельный ранее подтверждённый дефект случайного Caps Lock не относится к этой
ветке и не объявляется исправленным. Механизм двух заглавных в текущем package
прошёл runtime-проверку.

## Смежное package-замечание

При установке candidate поверх уже запущенного `open-switcher 0.1.0-1` apt
успешно заменил файл, но не перезапустил активный пользовательский daemon.
Старый PID продолжал выполнять удалённый inode (`/proc/PID/exe -> ... (deleted)`)
со старым hash. Для этой проверки сервис был явно перезапущен, после чего hash
запущенного процесса совпал с package.

Это уже известный отдельный lifecycle-дефект Debian update (`M-09a`), а не
регрессия текущего fail-safe изменения. Он остаётся не исправлен в этой ветке:
до отдельного package lifecycle решения обновление требует явного restart
пользовательского сервиса или нового login.

## Итоговое состояние VM

- `open-switcher-daemon.service`: `active/running`, `Result=success`;
- PID: `177421`, 12 потоков, `NRestarts=0`;
- daemon SHA-256
  `4dde6886b981aa86fa15b33a6dd0b295aecd9aa91098c1efa094e81a9e024174`
  совпадает с package;
- `DISPLAY=:0`, `XDG_SESSION_TYPE=x11`, layout group `0` (English);
- временные debug manager variables отсутствуют;
- Xephyr, `X99` socket и Xed test process отсутствуют;
- одно активное `Open-Switcher Virtual Device` и один открытый `/dev/uinput` fd;
- штатные задержки восстановлены: `delay_ms=30`, `backspace_ms=0`,
  `typing_ms=0`; auto correction, two capitals и accidental Caps Lock включены;
- лаборатория сохранена и не удалялась.

## Ограничения

- Runtime выполнялся на QEMU USB keyboard/tablet, а не на физической клавиатуре
  и реальном тачпаде.
- Проверен Mint/Cinnamon/X11. Wayland policy покрыта unit tests, но отдельный
  Wayland runtime в этой работе не запускался.
- Fault injection закрывала только X11 connection fd watcher. Не моделировались
  зависание ядра, аппаратный отказ контроллера и полная остановка VM.
- Исправление uinput-пути проверено unit tests и повторным X11 recovery, но не
  инъекцией ошибки каждого отдельного ioctl/write внутри реального ядра.
- Переход pointer watcher N -> 0 покрыт unit test, но runtime-unplug физического
  тачпада или QEMU pointer device не выполнялся.
- Probe подтверждает реальное окно освобождения виртуального evdev-устройства,
  но не даёт hard-realtime гарантии для любого нагруженного физического хоста.
- Отдельный дефект accidental Caps Lock и package update restart остаются за
  пределами этого изменения.

## Вывод

Для проверенных отказов обязательного X11 watcher механизм теперь ведёт себя
fail-safe относительно физического ввода: смерть worker достигает владельца
backend, активный grab быстро снимается до потенциально долгих join, daemon
остаётся живым и автоматически возвращает полный pipeline только после
восстановления зависимостей. Startup без обязательного X11 monitor также
fail-open: клавиатура остаётся доступна системе.

Абсолютную fail-safe гарантию для зависшего ядра или неисправного hardware дать
нельзя, однако подтверждённая прежняя причина «watcher умер, grab остался»
устранена и воспроизведённые аварийные сценарии — включая отказ во время
активной коррекции — теперь проходят в package-first runtime. Подтверждённая
утечка `/dev/uinput` fd также устранена: три recovery подряд не увеличили число
fd или виртуальных устройств. Лаборатория сохранена для будущих проверок.
