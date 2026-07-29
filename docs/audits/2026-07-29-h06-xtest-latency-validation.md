# Проверка снижения задержки XTEST после H-06

**Дата:** 2026-07-29

**Статус:** выбранный barrier-only вариант и предынтеграционные проверки
завершены

**Техническое решение:** рекомендовать только `barrier-only`, коммит
`9276bc05814b48113dc2285bae6199e454f0e501`. Operation-scoped mapping cache из
`7b1475cd707804830a64e24cbdb2fa8a6efc3221` сохранить как проверенный
эксперимент, но не включать в интеграцию: дополнительный пользовательский
выигрыш не отделяется от шума.

Для интеграции создана отдельная ветка `perf/h06-xtest-barrier`: в её
production-код входит `9276bc0`, но отсутствует `7b1475c`. Автоматическое
слияние не выполнялось.

## Что проверялось

Исходный H-06:

```text
d2ae65e3d585bff1f8f3915e42fd438a652003c3
```

Первый production-коммит:

```text
9276bc05814b48113dc2285bae6199e454f0e501
perf: reuse checked XTEST mutation confirmation
```

Он сохраняет `xtest_fake_input(...).check()` для каждой press/release, но
следующий протокольный `Synchronize` потребляет уже полученное типизированное
подтверждение вместо второго `get_input_focus().reply()` round-trip.

Второй production-коммит:

```text
7b1475cd707804830a64e24cbdb2fa8a6efc3221
perf: reuse XTEST mapping within one operation
```

Он кэширует mapping одинаковой клавиши только внутри одной bounded operation.
Уникальные token, protocol sequence, authoritative ledger и terminal cleanup
не меняются.

Delays, XTEST event plan, guardian protocol, uinput/Wayland, clipboard,
layout backend и общая логика коррекции в production-коммитах не изменялись.

## Статические и автоматические gates

До VM-кампании на точном combined source выполнены:

| Проверка | Результат |
|---|---|
| focused `xtest_guardian` | 68 active passed, 1 ignored |
| `synthetic_input` | 47 passed |
| полная Rust-регрессия | 924 library + 4 daemon + 11 D-Bus + 5 probe passed; 1 ignored |
| `cargo check --locked --all-targets --features settings-ui` | exit 0 |
| `cargo fmt --all -- --check` | exit 0 |
| `git diff --check` | exit 0 |
| Wayland diagnostics | `ok` |
| Linux input setup tests | `ok` |
| Debian package scripts | `ok` |
| DEB management tests | `ok` |

Полная Rust-регрессия запускалась без `DISPLAY` и `WAYLAND_DISPLAY`, поэтому
не открывала host X11/Wayland и реальные input devices.

Barrier-only DEB собран с package test phase. Combined DEB собран с
`DEB_BUILD_OPTIONS=nocheck` только после уже пройденных полных gates, чтобы не
дублировать тот же тестовый прогон; эта опция не меняет release binary.

После выбора варианта на точной ветке `perf/h06-xtest-barrier` выполнена
свежая предынтеграционная перепроверка:

| Проверка | Результат |
|---|---|
| полная Rust-регрессия без `DISPLAY`/`WAYLAND_DISPLAY` | 920 library + 4 daemon + 11 D-Bus + 5 probe passed; 1 ignored |
| `cargo check --locked --all-targets --features settings-ui` | exit 0 |
| `cargo fmt --all -- --check` | exit 0 |
| `git diff --check` | exit 0 |
| Wayland diagnostics | `ok` вне syscall-песочницы |
| Linux input setup tests | `ok` |
| Debian package scripts | `ok` |
| DEB management tests | `ok` |

Первый Wayland diagnostics run внутри restricted syscall sandbox получил
`EPERM` на создании временного Unix socket. Тот же неизменённый тест вне этой
песочницы завершился `ok`; это ограничение среды запуска, а не отказ
OpenSwitcher.

## Идентичность пакетов

Все три файла имеют:

```text
Package: open-switcher
Version: 0.1.0-3
Architecture: amd64
```

| Состояние | Source | SHA-256 DEB | SHA-256 daemon внутри DEB |
|---|---|---|---|
| H-06 | `d2ae65e3d585bff1f8f3915e42fd438a652003c3` | `f084191d2e0c469db3eea0d75b0304dc7acf4954f60e8f15c05e4f149720b2fe` | `044d43f95905462700aa778650f4b0842662f99dfe3a0e2ad5e6f0ae14caf90e` |
| barrier-only | `9276bc05814b48113dc2285bae6199e454f0e501` | `3ac9360dbe79b15565958968a1cbef5bc5984ac915c4948b7aa0063ca2d15157` | `9193f22eff01c8ae190b5a7da21ffcf75a0e010404972195763e0828cb02699e` |
| combined | `7b1475cd707804830a64e24cbdb2fa8a6efc3221` | `2b8ef7b3cf40e14484cbff4482e74144c1c06407b7cd80091baf9d20ec834b7b` | `4c6a6e747a18c6c5371143415ed24c539ea9e57ecbde4164ce5c76939f44afa7` |

Артефакты находятся в:

```text
/home/andrey/VMs/OpenSwitcherLab/artifacts/
```

Перед каждой серией SHA пакета, daemon внутри пакета и установленного
`/usr/bin/open-switcher-daemon` сверялись. Несовпадений не было.

## Среда и конфигурация

```text
Linux Mint 22.2
kernel 6.14.0-29-generic
desktop Cinnamon
session X11
```

Во всех состояниях использовалась одна сохранённая VM, одно окно Xed и одна
конфигурация:

```text
layout delay_ms = 30
backspace_ms = 10
typing_ms = 10
manual correction = F12
XKB layouts = us,ru
```

Input отправлялся только через QEMU USB keyboard внутри guest. Host input,
clipboard, раскладка, systemd, udev и ACL не изменялись.

## Методика performance-кампании

Исторический pre-H-06 baseline сохранён в предыдущем H-06 evidence, но не
смешивался с новыми числами: он измерен в другое время и не позволяет отделить
изменение кода от дрейфа VM. Для текущего решения прямым baseline служит exact
H-06 DEB, переустановленный вперемешку с обоими новыми вариантами.

Порядок из девяти серий:

```text
H-06
barrier-only
combined
H-06
combined
barrier-only
combined
barrier-only
H-06
```

Каждая серия содержала 30 одинаковых коррекций `ыгвщ -> sudo`, всего:

```text
90 коррекций на состояние
270 учитываемых коррекций
```

Для каждой коррекции проверялись:

- успешный `manual-current-word-completion`;
- полный `elapsed_ms`;
- X server time от первого synthetic Backspace press до последнего replay
  release;
- точное число и порядок press/release;
- guardian aggregate;
- отсутствие failure, timeout, protocol failure и `Unreconciled`.

Один ранний прогон из 30 коррекций не включён: `systemctl stop` завершил daemon
сигналом и не дал записать итоговый guardian aggregate. Он сохранён с
префиксом `preflight-`. В учитываемых сериях daemon завершался через штатный
D-Bus `RequestExit`.

## Exact trace

Во всех 270 коррекциях получен один и тот же блок из 16 событий:

```text
Backspace press/release × 4
S press/release
U press/release
D press/release
O press/release
```

X11 keycode:

```text
Backspace = 22
S = 39
U = 30
D = 40
O = 32
```

Каждая серия содержит ровно 30 таких блоков. Число уникальных normalized trace
во всех девяти сериях равно одному.

## Результаты производительности

Полное время F12-коррекции:

| Состояние | n | minimum, ms | median, ms | p95, ms | maximum, ms |
|---|---:|---:|---:|---:|---:|
| H-06 | 90 | 126 | 133 | 139 | 146 |
| barrier-only | 90 | 116 | 127 | 137 | 139 |
| combined | 90 | 117 | 128 | 135 | 139 |

Участок от первого Backspace press до последнего replay release:

| Состояние | n | minimum, ms | median, ms | p95, ms | maximum, ms |
|---|---:|---:|---:|---:|---:|
| H-06 | 90 | 108 | 116 | 123 | 128 |
| barrier-only | 90 | 103 | 112 | 118 | 120 |
| combined | 90 | 102 | 111 | 117 | 122 |

Относительно H-06:

- barrier-only: median полного пути `-6 ms`, примерно `-4,5%`;
- barrier-only: median измеренного XTEST-участка `-4 ms`, примерно `-3,4%`;
- combined: median полного пути `-5 ms`, примерно `-3,8%`;
- combined: median измеренного XTEST-участка `-5 ms`, примерно `-4,3%`.

Медианы полного пути по отдельным сериям:

| Состояние | Серия A | Серия B | Серия C |
|---|---:|---:|---:|
| H-06 | 132,5 | 133 | 132 |
| barrier-only | 127 | 126 | 128,5 |
| combined | 127 | 127 | 128,5 |

Это подтверждает устойчивый сдвиг barrier-only относительно H-06.

Guardian aggregate по трём сериям:

| Состояние | p50 range, μs | p95 range, μs | max range, μs |
|---|---:|---:|---:|
| H-06 | 244–271 | 2462–2894 | 5894–7596 |
| barrier-only | 154–172 | 2648–2787 | 5961–6682 |
| combined | 138–149 | 2503–2696 | 5192–7103 |

Mapping cache уменьшает guardian p50 ещё примерно на 14–23 μs. Однако на
полном пользовательском пути combined не лучше barrier-only по aggregate
median (`128 ms` против `127 ms`). Улучшение p95 на 1–2 ms и XTEST-участка на
1 ms находится на уровне межсерийного шума.

### Финальный A/B замер перед интеграцией

После выбора barrier-only выполнен отдельный финальный замер без combined:

```text
H-06 -> barrier-only -> barrier-only -> H-06 -> H-06 -> barrier-only
```

Каждая из шести серий содержала 30 коррекций. Итого получено 90 измерений на
вариант и 180 успешных коррекций.

Полное время F12-коррекции:

| Состояние | n | minimum, ms | median, ms | p95, ms | maximum, ms |
|---|---:|---:|---:|---:|---:|
| H-06 | 90 | 119 | 131 | 139 | 147 |
| barrier-only | 90 | 113 | 126 | 132 | 136 |

Участок от первого Backspace press до последнего replay release:

| Состояние | n | minimum, ms | median, ms | p95, ms | maximum, ms |
|---|---:|---:|---:|---:|---:|
| H-06 | 90 | 105 | 114 | 120 | 127 |
| barrier-only | 90 | 102 | 109 | 115 | 116 |

Относительно exact H-06 финальный barrier-only дал:

- median полного пути `-5 ms`, примерно `-3,8%`;
- p95 полного пути `-7 ms`, примерно `-5,0%`;
- maximum полного пути `-11 ms`;
- median и p95 измеренного XTEST-участка по `-5 ms`.

Медианы полного пути по отдельным сериям:

| Состояние | Серия A | Серия B | Серия C |
|---|---:|---:|---:|
| H-06 | 128 | 133,5 | 128 |
| barrier-only | 126 | 127 | 126 |

Guardian p50 range снизился с `254–269 μs` до `141–167 μs`. Его p95 ranges
перекрываются (`2488–2727 μs` и `2385–2830 μs`), поэтому отдельное улучшение
редкого IPC-хвоста не заявляется.

Во всех 180 коррекциях:

- ровно 30 exact trace на серию;
- число уникальных normalized trace равно одному;
- completion samples и XTEST span samples полны;
- failure, timeout, protocol failure и `Unreconciled` не найдены.

Первичная кампания дала выигрыш median `6 ms`, финальная — `5 ms`. Абсолютные
значения дрейфовали между сериями, но направление и практически значимый
размер выигрыша barrier-only воспроизвелись.

## Проверка checked mutation до локального Synchronize

Отдельный VM-only test-binary был собран из точного combined source и вызывал
production `GuardianX11Executor`:

1. исходный Backspace key state проверен как up;
2. вызван production `key_down`, внутри которого выполняется
   `xtest_fake_input(...).check()`;
3. до `GuardianX11Executor::synchronize()` другое X11-соединение выполнило
   `XQueryKeymap` и увидело Backspace down;
4. локальный proof потреблён;
5. выполнены checked key-up и локальный synchronize;
6. observer увидел Backspace up.

Фактический результат:

```text
visible_before_local_sync=true
visible_after_release=false
1 passed; 0 failed
```

Test-binary:

```text
/home/andrey/VMs/OpenSwitcherLab/artifacts/
open-switcher_checked-mutation-test_7b1475cd707804830a64e24cbdb2fa8a6efc3221

SHA-256:
c6459ec210ce97fc88278f681490e914a7dced77874eae991c4aa285d49b7c46
```

Тестовая вставка после сборки binary удалена обратным патчем. Production
worktree вернулся к точному коммиту `7b1475c`; сам binary и его вывод
сохранены в лаборатории.

## Runtime safety gates combined-кандидата

| Gate | Наблюдаемый результат | Статус |
|---|---|---|
| обычный F12 | `ыгвщ` → `sudo` | PASS |
| accidental Caps Lock | `пРИВЕТ` + Space → `Привет` + Space | PASS |
| две заглавные | `ПРивет` + Space → `Привет` + Space | PASS |
| Shift и punctuation | `руддщ!` + F12 → `hello!` | PASS |
| штатный stop | marker `789` при остановленном daemon | PASS |
| последующий start | marker `456`, отдельные daemon/guardian cgroup | PASS |
| daemon `SIGKILL` после real XTEST down | matching key-up, marker `123`, новый daemon | PASS |
| guardian `SIGKILL` после real XTEST down | emergency `Reconciled`, marker `456`, новый daemon | PASS |
| keymap после аварий | QEMU, XTEST и OpenSwitcher devices: все key up | PASS |
| orphan/zombie | старые PID исчезли; не более одного daemon/guardian | PASS |
| опасные строки | timeout/protocol failure/`Unreconciled` не найдены | PASS |

### Daemon SIGKILL

External probe сначала увидел:

```text
{"kind":"press","keycode":22,...}
```

и только затем послал `SIGKILL` PID `34161`. Старый guardian PID `34213`
завершил peer-EOF cleanup. Первый `XQueryKeymap` после восстановления показал
key up. Systemd запустил daemon PID `34373` и новый guardian PID `34423`;
physical marker `123` ввёлся.

### Guardian SIGKILL

External probe увидел real Backspace down и уничтожил guardian PID `34806`.
Старый daemon PID `34753` выполнил fail-stop. В input-debug порядок:

```text
stage=grab-released
stage=guardian-emergency-terminal proof=Reconciled
```

То есть physical grab освобождён до завершения emergency cleanup. Затем
запущены daemon PID `34961` и guardian PID `35013`; key state был up, marker
`456` ввёлся.

## Exact barrier-only Mint crash smoke

После выбора варианта короткая safety-матрица повторена на exact barrier-only
DEB с SHA-256
`3ac9360dbe79b15565958968a1cbef5bc5984ac915c4948b7aa0063ca2d15157`.
SHA daemon внутри пакета и установленного `/usr/bin/open-switcher-daemon`
совпал:

```text
9193f22eff01c8ae190b5a7da21ffcf75a0e010404972195763e0828cb02699e
```

| Gate | Наблюдаемый результат | Статус |
|---|---|---|
| обычный F12 | `ыгвщ` → `sudo` | PASS |
| accidental Caps Lock | `пРИВЕТ` + Space → `Привет` + Space | PASS |
| две заглавные | `ПРивет` + Space → `Привет` + Space | PASS |
| Shift и punctuation | `руддщ!` + F12 → `hello!` | PASS |
| штатный stop/start | markers `789` и `456` | PASS |
| daemon `SIGKILL` после real XTEST down | matching key-up, marker `123`, новый daemon | PASS |
| guardian `SIGKILL` после real XTEST down | emergency `Reconciled`, marker `456`, новый daemon | PASS |
| порядок аварийного пути | `grab-released` до `guardian-emergency-terminal` | PASS |
| keymap после аварий | QEMU, XTEST и OpenSwitcher devices: все key up | PASS |
| опасные строки | timeout/protocol failure/`Unreconciled` не найдены | PASS |

Следовательно, предположение о совпадении критического terminal-release пути
combined и barrier-only подтверждено отдельным runtime-прогоном.

## Exact barrier-only Ubuntu/GNOME/Wayland package smoke

Тот же DEB установлен в Ubuntu 24.04.4/GNOME/Wayland. После установки:

- пакет и установленный daemon совпали с указанными выше SHA;
- daemon вышел в input-ready и захватил QEMU USB keyboard;
- guardian service/process остался inactive, guardian socket был active;
  это ожидаемый Wayland-путь без XTEST;
- при устойчивой начальной RU-раскладке F12 дал `sudo`, completion занял
  `102 ms`;
- опасных строк в input-debug не найдено.

Active same-version reinstall заменил и процесс, и inode бинарника:

```text
daemon PID: 8735 -> 15684
daemon inode: 3196 -> 3214
```

После reinstall физический marker `321` ввёлся. После `remove` marker `654`
ввёлся, процесс и units отсутствовали. После `purge` marker `987` ввёлся,
пакет, binary, systemd units, udev rule и guardian socket отсутствовали;
пользовательские config и opt-in autostart сохранились.

## Почему выбран barrier-only

Первый commit выполняет исходную цель:

- exact trace не меняется;
- каждую mutation по-прежнему подтверждает X server;
- ledger и аварийное освобождение не ослаблены;
- выигрыш полного пути повторяется во всех трёх сериях;
- production diff локален для XTEST executor.

Второй commit также прошёл unit и runtime gates. Подтверждённого дефекта в нём
не найдено. Но он добавляет ещё одно состояние жизненного цикла
`OperationMappingCache`, а его дополнительный эффект виден в десятках
микросекунд guardian p50 и не даёт устойчивого улучшения полного пути.

По критериям спецификации это соответствует варианту:

```text
barrier-only даёт достаточный выигрыш — рекомендовать только первый commit
```

Combined commit не требуется откатывать или удалять: ветка и DEB сохраняются
как воспроизводимый результат эксперимента.

## Методические отклонения

При подготовке runtime harness были обнаружены и исправлены четыре ошибки
самого теста:

1. Xed сохраняет один структурный финальный `\n`;
2. первый Caps preflight включал Caps Lock уже после первой буквы и не
   воспроизводил `пРИВЕТ`;
3. QEMU HMP называет пробел `spc`, а не `space`; harness теперь запрещает
   игнорировать непустой ответ `sendkey`;
4. shifted-symbol preflight не включал F12.

Эти прогоны не использованы как product results и сохранены с префиксом
`preflight-`. Финальная матрица после исправления harness прошла целиком.

При предынтеграционной перепроверке отдельно учтены ещё два свойства среды:

1. после холодного запуска Mint тестовое окно Xed не восстановилось
   автоматически; harness остановился до ввода, после явного открытия того же
   документа exact barrier-only smoke прошёл;
2. прямой `gsettings set ... current` в GNOME не является надёжным аналогом
   пользовательского переключения: daemon сообщил успешную F12-транзакцию,
   но GNOME не сменил фактическую раскладку при synthetic Super+Space, поэтому
   слово визуально осталось тем же. Этот прогон исключён; функциональный
   результат получен после запуска с устойчивой начальной RU-раскладкой.

## Ограничения

1. Timing и crash gates выполнены в QEMU Mint/Cinnamon/X11, а не на реальной
   физической клавиатуре и X11-композиторе пользователя.
2. X server timestamps имеют миллисекундное разрешение. Разница combined и
   barrier-only в 1–2 ms поэтому не считается доказанным пользовательским
   выигрышем.
3. Не повторялись неизменившиеся предельные сценарии H-06: одновременная
   гибель daemon и guardian, зависание X server, power loss и отказ emergency
   X11 connection.
4. Mapping mutation непосредственно во время одной операции не
   воспроизводилась. Это не влияет на выбранный barrier-only результат,
   поскольку mapping cache не рекомендуется к интеграции.

## Evidence

Основной каталог:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h06-xtest-latency/
```

Ключевые файлы:

| Evidence | SHA-256 |
|---|---|
| `campaign-summary.json` | `9beff47282e5132d9327430df63d979197f36784c80970e96c67f8e89d2be41c` |
| `safety-combined-summary.json` | `bf03342fa2122d1331eed7b5239cff95aeeea3b35d048d422ef2f88e9d725435` |
| `safety-combined-journal.txt` | `1cea1cd35223b93ded667e642846fe1f484992b3083c9c814f4fce44ff47aded` |
| `checked-mutation-probe.txt` | `1e630fcd6f8bed35f91186f6d0d9c3caac8b89cbf5a102bad91461c067968617` |
| `checked-mutation-keyup.jsonl` | `757846d4c31aca6503263387f4bea3d6ac75a91ac3bec85baa8ad2e4a4b26030` |

Дополнительно сохранены девять input-debug логов, девять raw XInput traces,
девять per-series summary, screenshots, SIGKILL JSONL и все preflight
артефакты.

Предынтеграционные exact barrier-only evidence:

| Evidence | SHA-256 |
|---|---|
| Mint `barrier-final-20260729/safety-combined-summary.json` | `eb871168cc31ac3b99c3a5fb89695074be71ed4cf1389d5b7559bb035cc06593` |
| Mint `barrier-final-20260729/safety-combined-journal.txt` | `f2b82ddffe61c69c1dc35b6cfcd9c1c24d91029ebbf4769b4521d794db239105` |
| Ubuntu `barrier-final-20260729/wayland-input-debug.log` | `053576e469bc7b2233a0c72645fa6a11803d85080d220f294f7cf2c9240666b2` |
| Ubuntu `barrier-final-20260729/barrier-final-journal.txt` | `08a4901f9a07ac83ed543e242082332f924bdc80e17c4cd92933e7da812586b6` |
| Ubuntu `h06-barrier-final-wayland-f12.ppm` | `2e81dc2326020a9eb354f52c605cf53167b010e11af6ce0626d44e358d156013` |
| Ubuntu `h06-barrier-final-after-purge.ppm` | `7bbc3efee17d48653cb6062bf2ebb267019a1f22c8dcd6c82cc62bd86ae3b737` |
| Mint `final-ab-20260729/campaign-summary.json` | `e8b2748057cadedbf8e52a6a21af621446bf9d1cbf26ba68306752308aea615e` |

## Состояние после проверки

- Mint и Ubuntu guests штатно выключены;
- QEMU processes, SSH forwards `127.0.0.1:22222`/`22223` и оба QMP socket
  отсутствуют;
- VM, overlay, ключи, пакеты и evidence сохранены;
- лаборатория не удалялась;
- merge и push не выполнялись.

## Следующий интеграционный шаг

Integration branch, exact barrier-only Mint crash smoke и
Ubuntu/GNOME/Wayland package lifecycle smoke завершены. Следующий шаг — после
отдельного решения пользователя слить `perf/h06-xtest-barrier` в основную
линию H-06, затем выполнить проверку уже объединённого результата и собрать
финальный DEB.
