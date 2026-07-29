# H-06: итоговая проверка fail-safe synthetic input ledger

- Дата проверки: 2026-07-29
- Ветка: `fix/h06-synthetic-input-ledger`
- База H-06:
  `9e894b2c00a57b9f74a5ccb293a21b1bcebc04d1`
- Проверенный source commit:
  `d2ae65e3d585bff1f8f3915e42fd438a652003c3`
- Пакет: `open-switcher 0.1.0-3`, `amd64`
- Статус release gates 1–11 из спецификации: **пройдены**
- Статус дополнительного performance-gate плана:
  **не пройден, merge заблокирован до отдельного решения**

## Краткий результат

H-06 устраняет исходный архитектурный риск: synthetic down больше не считается
безопасно завершённым только на основании локального состояния daemon.
Для uinput и Cinnamon/XTEST введён общий ledger, а XTEST-инъекция вынесена в
отдельный socket-activated guardian. При одиночной гибели daemon guardian
освобождает известный ему synthetic debt; при гибели guardian daemon сначала
освобождает physical `EVIOCGRAB`, затем выполняет ограниченную emergency
очистку и прекращает работу без перехода на другой backend.

Подтверждены:

- идентичный normal XTEST trace до и после H-06;
- полная функциональная матрица Mint/Cinnamon/X11 и
  Ubuntu/GNOME/Wayland;
- освобождение реального XTEST Backspace после `SIGKILL` daemon;
- `Reconciled` emergency cleanup после `SIGKILL` guardian;
- работа физического ввода и systemd restart после обеих аварий;
- 20 paced restart и 20 paced stop/start на Mint;
- same-version reinstall, remove и purge в обеих VM;
- отсутствие лишнего guardian process в Wayland;
- 938 прошедших host-тестов, один отдельно запущенный ignored benchmark и
  четыре shell/package suite.

Новых подтверждённых дефектов H-06 финальный review не нашёл. Однако
дополнительный критерий плана `guardian debug p95_us <= 1000` не выполнен:
получено `2822 μs`. Этот таймер измеряет не только IPC, но и server-side XTEST
вызов с X11 `.check()`/round-trip, который уже входил в прежний путь.
Одновременно пользовательская end-to-end медиана ухудшилась только на `2,8%`
при допустимых `10%`, а максимальная коррекция не получила timeout.

Поэтому текущий DEB не передаётся на merge автоматически. Рекомендуемое
следующее решение — заменить ошибочно названный IPC-порог на измеряемый
дифференциальный end-to-end gate с существующими жёсткими protocol timeout и
cleanup bounds. Альтернатива — реализовать предусмотренную планом bounded
batching ветку и полностью повторить обе VM-кампании; это существенно более
рискованное изменение хрупкого input flow.

## Идентичность артефактов

Проверенный candidate:

```text
source:
d2ae65e3d585bff1f8f3915e42fd438a652003c3

DEB:
/home/andrey/Projects/OpenSwitcher/.worktrees/h06-synthetic-input-ledger/dist/packages/open-switcher_0.1.0-3_amd64.deb

SHA-256:
f084191d2e0c469db3eea0d75b0304dc7acf4954f60e8f15c05e4f149720b2fe
```

Один и тот же exact DEB без промежуточной пересборки передан в Mint и Ubuntu.
SHA-256 в обеих guest совпал с host.

Внешняя X11-проба:

```text
target/release/examples/h06_x11_vm_probe
9872545f073de2ecc5b960c2cff15cc10b4f5d73792685956710827b13309b89
```

Baseline:

```text
source:
9e894b2c00a57b9f74a5ccb293a21b1bcebc04d1

DEB:
/home/andrey/VMs/OpenSwitcherLab/artifacts/open-switcher_h06-baseline_9e894b2c00a57b9f74a5ccb293a21b1bcebc04d1_amd64.deb

SHA-256:
2e4512d83d2f72c83d3d6d1ab0eb3a1b54481873d63881dd35ed64e631dba840
```

## Безопасные host-проверки

На host не открывались физические `/dev/input` или `/dev/uinput`, не создавалось
активное virtual input device, не отправлялись реальные нажатия и не менялись
host clipboard, раскладка, systemd, udev или ACL.

Финальные команды и результаты:

| Команда | Результат |
|---|---|
| `cargo fmt --check` | exit 0 |
| `cargo test --locked --all-targets --features settings-ui -- --test-threads=1` | 938 passed, 0 failed, 1 ignored |
| `cargo check --locked --all-targets --features settings-ui` | exit 0 |
| release benchmark максимального debt | 1 passed |
| `tests/wayland_diagnostics_test.sh` | `ok` |
| `tests/linux_input_setup_test.sh` | `ok` |
| `tests/debian_package_scripts_test.sh` | `ok` |
| `tests/manage_package_deb_test.sh` | `ok` |
| `git diff --check` | exit 0 |

Полный Rust-прогон включает:

- library: `918 passed`, `1 ignored`;
- daemon binary: `4 passed`;
- D-Bus integration: `11 passed`;
- H-06 probe: `5 passed`.

Суммарно: `938 passed`, `0 failed`, `1 ignored`.

Игнорируемый release-mode benchmark запущен отдельно:

```text
runs=200
debt=32
p50=2 μs
p95=2 μs
p99=2 μs
max=14 μs
remaining debt=0 во всех циклах
```

Gate `p99 < 500 ms`, каждый run `< 1 s` пройден с большим запасом.

В restricted syscall sandbox два тестовых действия получили искусственный
`EPERM`: `UnixStream::shutdown(Write)` и создание временного UNIX-сокета
Wayland fixture. Те же точные тесты повторены вне syscall sandbox; для полного
Rust-прогона дополнительно удалены `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`
и `DBUS_SESSION_BUS_ADDRESS`. Повторные прогоны завершились exit 0 и не имели
доступа к графической пользовательской сессии.

## Mint/Cinnamon/X11

Среда:

```text
Linux Mint 22.2
kernel 6.14.0-29-generic
desktop Cinnamon
session X11
```

### Обычная функциональность

Через QEMU physical keyboard проверены:

1. F12-коррекция текущего/последнего слова;
2. auto correction;
3. случайный Caps Lock;
4. исправление двух заглавных;
5. separator;
6. shifted symbol;
7. EN/RU XKB group switch;
8. Copy/Paste selected-text smoke;
9. сброс контекста по Enter, Tab и физическому click;
10. отсутствие сброса от простого pointer motion.

Все сценарии прошли. Для Caps отдельно подтверждено:

- случайный вариант `пРИВЕТ` исправляется в `Привет`;
- намеренно полностью заглавное `ПРИВЕТ` остаётся заглавным.

После операций probe подтверждал отсутствие лишнего synthetic down.

Первая click-проверка была недействительной: QMP down/up попали в один
коалесцированный batch. Она не учитывалась как результат продукта. Повтор с
раздельными QMP-командами создал настоящий click и прошёл.

### Normal trace и timing

Baseline содержит 30 коррекций по четыре Backspace, всего 240
press/release-событий. Candidate raw trace содержит ещё одну пару
press/release: это отдельно введённый физический Backspace, который успел
увидеть observer до начала 30 измеряемых коррекций. Эта контрольная пара
исключена до сравнения.

После исключения контрольной пары:

```text
baseline events:  240
candidate events: 240
normalized kind/keycode SHA-256, оба:
74159f173f109f2dbb64d86617105d29f210381449558dc323e0788ef6e3377f
```

Exact press/release order совпадает.

Интервал измерен от первого Backspace press до последнего replay release:

| Показатель | Baseline | Candidate |
|---|---:|---:|
| minimum | 44,975 ms | 47,389 ms |
| median | 50,322 ms | 51,732 ms |
| p95 по расчёту кампании | 53,247 ms | 54,432 ms |
| maximum | 54,264 ms | 55,235 ms |

Изменение медианы: `+2,8%`; gate `<= +10%` пройден. Transaction timeout не
наблюдался.

Guardian aggregate:

```text
count=512
p50_us=248
p95_us=2822
max_us=5599
```

Лог не содержит текста, полного key trace или authentication token.

### Почему performance-gate оставлен failed

В плане указан порог `guardian debug p95_us <= 1000`. Фактическое значение
`2822 μs`, поэтому буквальный gate не пройден.

В `src/daemon/xtest_guardian/client.rs` таймер начинается до `exchange()` и
заканчивается только после response. Он включает всю обработку guardian.
В `src/daemon/xtest_guardian/x11.rs` mutation выполняет
`xtest_fake_input(...).check()`, а synchronization — настоящий
`get_input_focus().reply()` round-trip. Следовательно, метрика не изолирует
добавленную стоимость локального `SOCK_SEQPACKET`.

Практическое end-to-end сравнение одновременно показывает только `+2,8%`.
Это не доказывает, что batching бесполезен, но не даёт достаточного основания
менять корректную последовательность XTEST-операций и повторять всю
package-first кампанию без отдельного решения.

### Гибель daemon после реального XTEST down

Внешняя проба дождалась реального synthetic Backspace down и послала
`SIGKILL` daemon.

Наблюдаемый результат:

- guardian отправил matching key-up и завершил cleanup;
- `XQueryKeymap` показал key up;
- physical marker `123` ввёлся;
- systemd запустил новый daemon;
- новый backend не пересёкся со старой session.

Production-журнал peer EOF не печатает сам enum terminal proof. Поэтому
внешне доказаны matching up, нулевой key state и bounded exit, но строка
`proof=Reconciled` для этого пути в журнале отсутствует. Это ограничение
наблюдаемости, а не подтверждённый остаточный debt.

### Гибель guardian после реального XTEST down

Внешняя проба дождалась real down и уничтожила guardian.

Журнал:

```text
13:48:24.254435 stage=grab-released
13:48:24.255111 stage=guardian-emergency-terminal proof=Reconciled
```

Physical grab освобождён до ожидания emergency cleanup. Разница между
записями — примерно `0,676 ms`. Daemon выполнил fail-stop, systemd запустил
новый экземпляр, fallback на uinput не происходил, synthetic key остался up,
physical marker после аварии ввёлся.

В реальных VM-сценариях `Unreconciled` не наблюдался. Искусственно вызванные
cleanup failures в unit/process tests возвращают
`TerminalProof::Unreconciled`, продолжают остальные release и не публикуют
ложный `Stopped(Reconciled)`.

### systemd и package lifecycle

Успешно выполнены:

- 20 paced `restart`;
- 20 paced `stop/start`;
- same-version reinstall;
- remove;
- purge.

После каждого paced цикла keymap был чист, orphan/zombie не найден, physical
marker работал. После reinstall новый PID использовал текущий inode.

Предварительная серия без пауз достигла стандартного systemd start-limit на
шестом слишком быстром запуске. Это была некорректная нагрузка относительно
запланированного paced gate; защита systemd сработала штатно. После reset и
правильного pacing все 40 циклов прошли.

После purge:

- пакет и процессы отсутствуют;
- units, binary, udev rule и guardian socket удалены;
- пользовательская конфигурация сохранена;
- physical input работает.

На существующем input node осталось ACL
`user:openswitcher:rw-`. Это отдельный ранее известный package/ACL residual,
не созданный ledger H-06. Он должен оставаться в общем плане аудита.

## Ubuntu/GNOME/Wayland

Среда:

```text
Ubuntu 24.04.4 LTS
kernel 6.8.0-136-generic
desktop GNOME
session Wayland
```

Установлен тот же candidate SHA-256. Guardian socket мог быть active из-за
`Wants=`, но guardian service/process во время Wayland-функций оставался
inactive. Uinput path не инициировал X11/XTEST handshake.

Через QEMU physical keyboard прошли:

- F12: `sudo`;
- auto correction: `привет`;
- случайный Caps Lock: `Привет`;
- две заглавные: `Привет`;
- separator;
- shifted symbol: `руддщ!` → `hello!`;
- явные EN/RU/EN switches;
- selected text: `hello` → `руддщ`.

Промежуточные строки `руддщ!` и `еуые` первоначально были получены из-за
ошибочного предположения теста о стартовой раскладке. После явной установки
начального layout последовательность прошла; это не дефект OpenSwitcher.

Lifecycle:

- `SIGKILL` daemon: PID изменился, `NRestarts=1`, marker `123` работает;
- manual stop: daemon/guardian inactive, marker `456` работает;
- start: новый PID, marker `789` работает;
- active same-version reinstall: новый PID/current inode, marker `321`;
- remove: процессы и units отсутствуют, marker `654`;
- purge: пакет и системные файлы отсутствуют, marker `987`.

Первый reinstall был выполнен при отсутствии пользовательского XDG autostart,
поэтому daemon остался остановлен. Это предусмотренное package-поведение:
units не включаются принудительно, если пользователь отказался от autostart.
После восстановления реалистичного opt-in autostart active reinstall прошёл.

После purge сохранены пользовательские config и opt-in autostart. Как и в
Mint, на существующих `/dev/uinput` и keyboard node осталась ACL-запись
`user:openswitcher:rw-`; это отдельный residual package/ACL finding.

## Проверка release gates 1–11

| Gate спецификации | Evidence | Статус |
|---|---|---|
| 1. Normal trace equivalence | frozen unit traces и exact 240/240 Mint sequence | PASS |
| 2. Failure-at-N ledger matrix | `failure_at_n_matrix_never_repeats_down_and_reports_honest_proof` | PASS |
| 3. Daemon panic/SIGKILL cleanup | process tests и real Mint daemon `SIGKILL` | PASS |
| 4. Guardian death даёт terminal outcome | process test и real Mint `proof=Reconciled` | PASS |
| 5. Lost ACK не повторяет down | service/process/client lost-ACK tests | PASS |
| 6. Cleanup failure не публикует safe outcome | `Unreconciled` unit/process tests | PASS |
| 7. Third backend contract | `fake_third_backend_passes_unmodified_sink_contract` | PASS |
| 8. Ungrab не зависит от X11 cleanup | `guardian_failure_releases_grab_before_emergency_wait` и real journal order | PASS |
| 9. Exact DEB проходит XTEST/uinput VM | тот же SHA в Mint и Ubuntu | PASS |
| 10. Package lifecycle | shell tests, reinstall/remove/purge обеих VM | PASS |
| 11. Остаточные сценарии перечислены | раздел ниже | PASS |

Дополнительный performance-gate задачи 16 не входит в нумерацию 1–11, но
является обязательным условием текущего implementation plan:

| Дополнительный gate | Факт | Статус |
|---|---|---|
| candidate median `<= baseline * 1.10` | `+2,8%` | PASS |
| guardian debug `p95_us <= 1000` | `2822 μs` | **FAIL** |
| нет transaction timeout | не наблюдался | PASS |

Итог: specification safety gates пройдены, но merge по текущему буквальному
плану запрещён до решения по performance-gate.

## Двухступенчатый review

### 1. Соответствие спецификации

Проверено:

- каждый production synthetic path входит в общий operation/session ledger;
- XTEST mutation находится только в guardian X11 executor;
- daemon не захватывает physical keyboard до готовности guardian;
- normal gate закрывается до release-only cleanup;
- physical release commit следует после writer write+sync;
- lost/mismatched ACK не разрешает следующий down;
- package помещает guardian в отдельную systemd cgroup;
- hidden guardian mode находится в том же packaged daemon binary;
- Wayland продолжает использовать uinput без guardian process.

Placeholder scan `CommitPhysicalState` не нашёл незавершённого контракта.
Production scan `xtest_fake_input|protocol::xtest::ConnectionExt` показал
только:

```text
src/daemon/xtest_guardian/x11.rs:11
src/daemon/xtest_guardian/x11.rs:220
```

### 2. Качество и безопасность реализации

Проверены границы IPC и отказов:

- `SOCK_SEQPACKET`, один bounded frame, `MAX_FRAME_BYTES=128`;
- максимум 512 prepared tokens и 32 active debts;
- mutation deadline не более 5 s, cleanup deadline не более 1 s;
- socket parent `0700`, socket `0600`;
- UID и фактический sender executable проверяются через kernel credentials
  и `/proc/<pid>/exe` inode;
- произвольных строк, путей, shell-команд и heap-sized protocol payload нет;
- normal mutation после terminal transition запрещена;
- cleanup продолжает reverse release после первой ошибки;
- X11 epoch не позволяет использовать token другой guardian/X server session;
- production input-debug хранит только aggregate latency и stage metadata.

Новых подтверждённых Critical/High/Medium/Low дефектов в H-06 review не
выявлено. Независимого второго исполнителя в этой сессии не было; обе ступени
review выполнены inline. Это учитывается как ограничение аудита.

## Коммиты H-06

Диапазон после базового `9e894b2` до проверенного source `d2ae65e`:

```text
a36ab39 docs: design H-06 synthetic input fail-safe
245cdb5 docs: plan H-06 synthetic input fail-safe
4643834 test: freeze synthetic input traces
5761288 feat: add synthetic input safety ledger
33ffe9c feat: track synthetic modifier ownership
2cf8735 refactor: route uinput through safety ledger
221bd57 feat: define bounded XTEST guardian protocol
bf2d02d feat: add authenticated guardian transport
8c7b9ca feat: reconcile guardian debt after daemon loss
8e4b7bf feat: isolate XTEST execution in guardian
205a982 feat: fail stop after XTEST guardian loss
4a3d939 feat: require guardian before Cinnamon input grab
0f2051e feat: reconcile restored modifiers after physical release
467c370 feat: add guarded XTEST service entrypoint
b7ca35e feat: package isolated XTEST guardian service
03d8605 test: add guarded X11 VM safety probe
881c8f2 test: measure maximum guardian cleanup debt
fc77427 docs: record H-06 Mint smoke blocker
41042f3 fix: keep guardian peer authentication usable
aecd1c4 docs: complete H-06 Mint package smoke
b4857cf chore: apply current Rust formatting
0e62553 fix: keep guardian operation ids monotonic
d2ae65e test: isolate guardian fixture descriptors
```

## Evidence

Mint:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/

h06-final-mint-journal-d2ae65e3.txt
8f8a51a0476da83b74dfc550ebcda1905c9729732bc339dc0f3acd6bc1da4d9e

h06-final-mint-summary-d2ae65e3.txt
d7ce7f6d5c9e0f2b6166daf9b9ae7e79ebedd5bf7bbd2a859c7a86d70355d9d6

h06-final-mint-runtime-evidence-d2ae65e3.tar.gz
4c3b4fceab20983ee18c06ea9c8132ef5292976131ef5901331b6bf8ffa42cf6
```

Ubuntu:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/

h06-final-ubuntu-journal-d2ae65e3.txt
0c32899d804e6291a78a93ae8e211a183fbadc63ea4f63940fff362584b14a2f

h06-final-ubuntu-summary-d2ae65e3.txt
e53067942023dca9d31e30939d79e3006c54033acda964c5dc61d040d2037ad9

h06-final-ubuntu-runtime-evidence-d2ae65e3.tar.gz
d36e6c88134081d42f9f89f3a30afa68ee7db0755cf2ae5ddb3002d36a7b12cc

h06-final-ubuntu-functional-after-d2ae65e3.ppm
9b889d55f576d55dfed99326e53fb43bf49a1ff338affb26fc3b4cf0d4e4432c
```

## Непроверенные сценарии и остаточные риски

1. Одновременный `SIGKILL` daemon и guardian, либо kill всей user service
   boundary. Один userspace guardian не может пережить собственную
   одновременную смерть с daemon.
2. Гибель guardian вместе с отказом или вечным зависанием заранее открытой
   emergency X11 connection.
3. Kernel hang, power loss и зависание/завершение всего X server.
4. X server применил synthetic down и навсегда перестал принимать cleanup.
5. Реальный USB hot-unplug/replug, suspend/resume и смена X server epoch во
   время активной транзакции в этой кампании не выполнялись.
6. Две VM не покрывают все реальные клавиатуры, touchpad, X11 compositor и
   будущие desktop/backend.
7. Для daemon peer-EOF внешне доказаны matching up и чистый keymap, но
   production-журнал не печатает итоговый enum proof этого пути.
8. Реальный `Unreconciled` намеренно не создавался в VM, потому что для этого
   потребовался бы дополнительный production fault API или отказ X server.
   Семантика проверена fake/process tests.
9. Selected-text/clipboard прошёл функциональный smoke, но fault injection
   каждого прерванного clipboard-пути не относится к H-06 и здесь не
   повторялся.
10. Остаточная ACL-запись после purge остаётся отдельным finding package/input
    permissions.
11. Буквальный guardian `p95 <= 1 ms` не достигнут. До решения этот кандидат
    нельзя считать полностью прошедшим согласованный implementation plan.

## Состояние лаборатории

Обе VM штатно выключены. Процессы QEMU отсутствуют, SSH forwards
`127.0.0.1:22222` и `127.0.0.1:22223` не слушаются.

Лаборатория не удалена:

- Mint disk, screenshots, journals и runtime evidence сохранены;
- Ubuntu disk, screenshots, journals и runtime evidence сохранены;
- baseline и candidate artifacts сохранены.

Любое удаление лаборатории выполняется только после отдельной прямой просьбы
пользователя.

## Итоговая оценка fail-safe

Механизм можно считать **single-fault-safe в проверенной userspace модели**:

```text
успех ->
  trace завершён, debt подтверждённо согласован

одиночная ошибка daemon/guardian ->
  новые mutations запрещены,
  physical EVIOCGRAB освобождён,
  exact synthetic debt очищен либо обязан стать явным Unreconciled,
  повреждённый процесс не продолжает обычную работу
```

Его нельзя объявить абсолютно fail-safe при одновременной гибели всех
userspace участников, зависании ядра или X server. В двух реальных
одноотказных VM-сценариях управление клавиатурой восстановилось.

С точки зрения безопасности H-06 достиг цели. С точки зрения формальной
приёмки merge остаётся заблокирован только несогласованным performance-gate.

## Рекомендуемое следующее решение

Рекомендуемый вариант:

1. Уточнить в спецификации и плане, что `xtest-guardian-ipc` является
   end-to-end guardian exchange, а не чистой IPC latency.
2. Для пользовательской производительности оставить доказуемый
   дифференциальный gate:
   `candidate median <= baseline median * 1.10`.
3. Сохранить абсолютные safety bounds: transaction `<= 5 s`, cleanup
   `<= 1 s`, максимальный fake debt cleanup `< 1 s`.
4. Сохранить aggregate `p50/p95/max` как диагностическую телеметрию и
   отслеживать регрессии относительно зафиксированного baseline, но не
   требовать абсолютные `1 ms` от метрики, включающей X11 round-trip.
5. После отдельного согласования этого изменения повторить только
   документационные/final safe gates; exact DEB и VM evidence не устареют,
   потому что production code не изменится.

Альтернатива — реализовать bounded `PrepareKeys`/`ExecuteSegment`, собрать
новый DEB и полностью повторить Mint и Ubuntu. Этот путь оправдан только если
владелец решит, что абсолютный `1 ms` важнее риска новой перестройки XTEST
последовательности.

Merge, push, удаление worktree и удаление VM laboratory в рамках этой проверки
не выполняются.
