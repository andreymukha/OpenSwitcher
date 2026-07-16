# Проверка исправлений C-01 и H-05

- Дата: 2026-07-17
- Ветка: `fix/audit-remediation`
- Проверенный commit: `25056d3b` (`fix: release input before joining dbus monitor`)
- Исходный audit baseline: `4251d88d` (`v0.1.0`)
- Статус: C-01 и H-05 исправлены в проверенном объёме; общий input lifecycle ещё нельзя считать полностью fail-safe

## Резюме

Закрыты две первоочередные проблемы:

1. source-tree bootstrap больше не является каналом привилегированной установки
   input assets: привилегированная граница привязана к установленному Debian
   package, а исходный helper оставлен только для read-only диагностики;
2. layout-switch capture теперь принадлежит unique D-Bus owner, ограничен lease,
   отменяется при исчезновении owner, не оставляет несбалансированные key release
   и поддерживается settings UI через одну постоянную connection с heartbeat.

Во время итогового review был найден дополнительный важный дефект порядка
очистки: обычная ошибка input loop могла перейти к потенциально блокирующему
`CaptureOwnerMonitor::stop/join` до явного освобождения input backend. Commit
`25056d3b` переставляет безусловный `service.shutdown()` перед остановкой
монитора и добавляет regression test с намеренно заблокированным monitor stop.

Один и тот же свежий `.deb` после этого прошёл package-first runtime regression
в Ubuntu 24.04/GNOME/Wayland и Linux Mint 22.2/Cinnamon/X11. В обеих гостевых
системах owner-loss и soft lease закрыли capture, физический QEMU input был
возвращён, SIGKILL daemon восстановился через systemd, а штатный stop освободил
ввод за существенно меньше секунды.

Это подтверждает исправление H-05 и проверенных exit paths, но не закрывает
оставшиеся H-01..H-04 и H-06. В частности, синхронная writer transaction всё ещё
имеет неограниченный `recv`, а hung writer thread и failure-at-every-step
synthetic sequences пока не доказаны fail-safe.

## Идентичность артефакта

- build flow: `./manage.sh package deb`;
- Rust toolchain: `1.95.0`;
- package: `open-switcher_0.1.0-1_amd64.deb`;
- размер: `3 007 900` bytes;
- SHA-256:
  `c67608fc390716ba70bc7bfe210e35cfa5ee87b42baa3c20dd8abd70e27636ea`;
- сохранённая копия:
  `/home/andrey/VMs/OpenSwitcherLab/artifacts/open-switcher_0.1.0-1_amd64_c67608fc3907.deb`.

Установленные binaries имели одинаковые SHA-256 в обеих VM:

| Binary | SHA-256 |
|---|---|
| `/usr/bin/open-switcher-daemon` | `4ce30fe48a2545bed62cedbc7a9022a2d7f3dd5605f727e41bf54b6b70544486` |
| `/usr/bin/open-switcher-settings` | `a10ba014ed5dcf138531213c3d2c6296413ee1965aeddd111294666b587f0254` |
| `/usr/bin/open-switcher-tray` | `abf9af878f90a7f02970344d02bc236b15adf94be5f0dddb22c90581fe1f8637` |

Ubuntu использовал clean install после purge baseline package. Mint обновил ранее
установленный `0.1.0-1` тем же version string, но другим package payload. В обоих
случаях acceptance запускал только binaries и units из установленного package.

## Изменения C-01

Ключевые commits:

- `bd290eb` — изоляция setup commands в tests;
- `161dfe4` — запрет privileged path overrides;
- `ec584b8` — отделение source tests от sudo bootstrap;
- `014b2f5` — privileged setup привязан к установленному package;
- `a230c2e` — package-only migration без side effects из source tree;
- `f5044ab` — package-safe runtime hints.

Текущая граница:

- `./manage.sh bootstrap linux-input` немедленно отказывает и не выполняет
  privileged mutation;
- `scripts/linux_input_setup.sh:229-340` выполняет только диагностику и указывает
  на canonical `.deb` install/reinstall;
- `tests/linux_input_setup_test.sh:69-202` проверяет отказ без mutation, включая
  попытки подменить dev/proc/rules paths;
- udev rule, ACL bridge и session helpers поступают только из `.deb` и находятся
  под контролем dpkg.

Runtime exploit старой C-01 намеренно не воспроизводился: для исправления
достаточны статическая проверка privilege boundary, shell regression и проверка
содержимого/установки package. H-08 (blanket ACL/multi-seat) является отдельным
открытым finding и этим изменением не объявляется исправленным.

## Изменения H-05

Точные основные места:

- `src/daemon/capture.rs:13-14,279-510` — soft lease 10 s, absolute lease 65 s,
  owner checks, expiry и suppression debt;
- `src/daemon/runtime.rs:3076-3167` — owner-aware команды и одна атомарная
  маршрутизация capture event;
- `src/daemon/service.rs:97-145,838-887,1214-1218,2236-2248` — physical-only
  routing, fail-open forwarding и reset input epoch при смене/остановке backend;
- `src/dbus/mod.rs:24-100,330-410` — sender берётся из `MessageHeader`, owner-loss
  monitor и `RenewLayoutSwitchCapture`;
- `src/daemon/mod.rs:80-120` — input backend освобождается до monitor stop/join;
- `src/settings_ui/dbus_client.rs:20-90` — одна retained D-Bus connection;
- `src/settings_ui/presenter.rs:13,280-595` — heartbeat 3 s и generation fencing;
- `src/settings_ui/ui.rs:410-435,1081-1185,1232-1294` — terminal/error/window
  paths закрывают локальную capture session.

Существующие и новые tests проверяют owner isolation, owner disconnect, soft и
absolute deadlines, same-owner renew, stale events, failed renew/finish,
pre-held key release, suppression debt, backend epoch reset и cleanup ordering.

## Безопасная статическая и unit-верификация

| Проверка | Результат |
|---|---|
| `cargo test --lib` | 427 passed |
| `cargo test --lib --features settings-ui` | 488 passed |
| `cargo test --test dbus_api -- --test-threads=1` | 11 passed |
| `cargo check --all-targets` | pass |
| `cargo check --all-targets --features settings-ui` | pass |
| `bash tests/linux_input_setup_test.sh` | pass |
| `bash tests/debian_package_scripts_test.sh` | pass |
| `bash tests/manage_package_deb_test.sh` | pass |
| `git diff --check` | pass |
| focused cleanup-order regression | pass |

Canonical Debian build повторно выполнил 427 base tests, 11 D-Bus integration
tests, 488 settings-ui tests и shell package checks. `lintian` оставил только
известные неблокирующие предупреждения: отсутствующие man pages, initial-upload
bug closure и AppStream modalias metadata.

## Runtime regression в VM

Граница безопасности кампании не изменилась:

- физические `/dev/input` и `/dev/uinput` хоста не передавались;
- использовалась только виртуальная QEMU USB keyboard;
- host/guest clipboard и shared folders отсутствовали;
- сеть гостей — QEMU user NAT, SSH только через `127.0.0.1`;
- udev, ACL, systemd и package mutations происходили только внутри гостей;
- обе VM после кампании выключены; лаборатория и evidence сохранены.

### Ubuntu 24.04 / GNOME / Wayland

- clean install exact package: pass;
- daemon/tray из `/usr/bin`: active;
- short-lived `gdbus` owner: `Idle -> Waiting -> Cancelled` за время менее 1 s;
- owner, остающийся подключённым без renew: `Waiting -> Cancelled` после 10 s
  soft lease (проверено через 11 s);
- renew каждые 3 s: capture оставался `Waiting` после 12 s и штатно отменился;
- другая connection не смогла отменить capture: D-Bus error и exit status 1;
- после owner-loss через physical QEMU path дошла точная строка
  `h05ownerlossok`;
- SIGKILL: PID `2736 -> 14743`, unit снова active примерно через 1 s, после чего
  дошла точная строка `crashrestartok`;
- штатный stop: 84 ms, старый PID исчез, direct input `stoppedinputok` дошёл.

Evidence:

- `/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h05-candidate-input-restored.png`;
- `/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h05-candidate-crash-restart.png`;
- `/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h05-candidate-stop-release.png`.

### Linux Mint 22.2 / Cinnamon / X11

- same exact package SHA installed over baseline package: pass;
- daemon/tray из `/usr/bin`: active;
- short-lived owner: `Idle -> Waiting -> Cancelled` за время менее 1 s;
- connected owner без renew: `Waiting -> Cancelled` после soft lease;
- после owner-loss дошла точная строка `minth05ownerok`;
- SIGKILL: PID `2416 -> 4706`, unit снова active примерно через 1 s, затем дошла
  точная строка `mintcrashok`;
- штатный stop: 77 ms, старый PID исчез, direct input `mintstoppedok` дошёл.

Evidence:

- `/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h05-candidate-input-restored.png`;
- `/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h05-candidate-crash-restart.png`;
- `/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h05-candidate-stop-release.png`.

## Вывод по findings

### C-01

Статус: **исправлен в проверенном scope**, уверенность высокая.

Source checkout больше не может выбрать привилегированные production paths и
запустить mutation через прежний bootstrap flow. Package-only privileged assets
собираются и устанавливаются ожидаемо. Остаточный ACL/multi-seat риск H-08 не
является частью C-01 closure.

### H-05

Статус: **исправлен и runtime-подтверждён в двух профилях**, уверенность высокая.

Исчезновение клиента больше не оставляет capture активным; soft lease закрывает
живого, но не renewing owner; heartbeat удерживает действующую UI session;
не-owner команды отклоняются; после cancellation события снова доходят через
реальный evdev/uinput pipeline. Cleanup ordering гарантирует освобождение backend
до потенциально блокирующего monitor join.

### Общая fail-safe оценка

Механизм H-05 теперь bounded и fail-open в проверенных сценариях. Весь механизм
захвата/освобождения OpenSwitcher **пока нельзя назвать полностью fail-safe**:

- H-01: `src/daemon/keyboard.rs:1206-1219` всё ещё содержит synchronous
  `reply_rx.recv()` без deadline;
- H-02: grab происходит до полной готовности writer/watchers;
- H-03: writer `JoinHandle` может быть оставлен после bounded stop timeout;
- H-04: deferred queue error/overflow ещё не имеет строгого conservation oracle;
- H-06: synthetic correction sequences не покрыты failure-at-every-step ledger;
- H-08: blanket ACL/multi-seat boundary остаётся открытой;
- M-01..M-03 clipboard, M-09a active upgrade и M-09b ACL cleanup этой кампанией
  не перепроверялись и не объявляются исправленными.

## Следующие шаги

1. Закрыть H-01 минимальным slice: bounded writer transaction, typed
   timeout/cancel, release-first реакция service и tests `accepted but never
   replies`/late reply/no further mutation.
2. Переставить grab после доказанной готовности sink (H-02) и проверить каждый
   partial-init return path.
3. Сделать writer shutdown ACK обязательным и запретить late synthetic writes
   после `stop()` (H-03).
4. Добавить conservation/reconciliation для deferred queue (H-04).
5. Добавить synthetic key ledger и failure-at-every-step matrix (H-06).
6. После каждого связного slice собирать новый canonical `.deb` и повторять
   только релевантную VM regression; полную двухпрофильную acceptance выполнить
   после закрытия H-01..H-06.
7. Отдельными волнами закрыть ACL/multi-seat, clipboard и package lifecycle
   findings. Лабораторию до прямой просьбы пользователя не удалять.

## Ограничения

- QEMU keyboard проверяет реальный guest evdev/uinput pipeline, но не всё
  разнообразие физического hardware/firmware;
- проверялись смерть процесса и штатный stop, а не произвольный deadlock одного
  живого writer thread;
- не выполнялись clipboard, multi-seat, unplug/replug и hardware acceptance;
- Mint upgrade выполнялся при заранее остановленных services, поэтому M-09a
  active-process replacement остаётся открытым;
- absolute 65 s lease доказан deterministic unit tests, но в VM отдельно не
  ожидался, поскольку soft 10 s lease уже завершает отсутствие heartbeat.
