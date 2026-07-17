# Проверка H-01: input runtime snapshot и зависший layout backend

- Дата: 2026-07-17
- Ветка: `fix/audit-remediation`
- Проверенный код: `2523d67` (`fix: reject unconfirmed layout observations`)
- Предыдущий checkpoint: `32a249e` (`docs: record nonblocking logging validation`)
- Основной артефакт: установленный Debian package `open-switcher 0.1.0-1`
- Статус: snapshot/cache slice исправлен и проверен; весь input lifecycle ещё не объявляется безусловно fail-safe

## Результат

Grab-critical обработка физического события больше не выполняет синхронное
чтение конфигурации, запись конфигурации, desktop command, D-Bus layout query или
полный refresh layout backend. Вместо этого input loop использует локальную
неизменяемую копию опубликованного snapshot и только неблокирующие операции:

- `try_read` публикации без ожидания;
- capacity-one `try_send` запроса на refresh;
- чтение generation/epoch через atomics;
- pure decision по уже подтверждённым данным.

Обновление settings сначала сохраняется и становится committed, затем одним
поколением публикуется для input consumer. Layout observation обновляется только
координатором вне input loop. После переключения раскладки epoch немедленно
инвалидирует прежнее подтверждение; новая layout-dependent коррекция разрешается
только после успешного наблюдения нового состояния.

Если snapshot занят, poisoned, unknown, awaiting confirmation или старше 1 s,
физическое событие всё равно пересылается. Автокоррекция в таком окне
пропускается, а не выполняется по предположению. Это сознательная fail-open
семантика: возможен пропуск одной коррекции при реальном длительном отказе
backend, но клавиатура не должна ждать backend и исходный ввод не теряется.

В установленном финальном package намеренно зависший `xset -q` подтвердил эту
границу в Linux Mint: обычный ввод продолжался, неверно набранное `ghbdtn ` не
было ошибочно преобразовано, журнал зафиксировал `status=Stale`, а остановка
unit освободила виртуальную клавиатуру за 107 ms.

## Основные места в коде

- `src/daemon/input_snapshot.rs:7-176` — polling/freshness constants, snapshot,
  authorization generations, неблокирующая публикация и bounded refresh wakeup;
- `src/daemon/runtime.rs:3091-3196` — background coordinator, snapshot access,
  invalidation epoch и запрос refresh;
- `src/daemon/runtime.rs:3470-3565` — publish-after-confirmation и запрет
  обновлять freshness после непроверенного observation;
- `src/daemon/service.rs:1048-1058` — service-local adoption через `try_load`;
- `src/daemon/service.rs:1735-1762,1880-1905` — fail-open skip для stale/unknown
  space и boundary corrections;
- `src/daemon/service.rs:2040-2055,2258-2272` — invalidation после physical и
  synthetic layout switch;
- `src/dbus/mod.rs:412-417` — D-Bus status только из последнего подтверждённого
  опубликованного состояния;
- `src/daemon/runtime.rs:2962-2986` — Cinnamon X11 current group через XKB;
- `src/daemon/keyboard.rs:2967-3074,3232-3250` — проверка ровно двух XKB groups,
  переключение group и XTest replay для Cinnamon.

Static boundary scan не нашёл `Command::new`, `.output()` или `.status()` в
`src/daemon/service.rs` и `src/daemon/input_snapshot.rs`.

## Коммиты slice

- `cea1f32` — pure snapshot model и publication cell;
- `b6f6f67` — публикация только committed configuration;
- `27c27e4` — layout refresh вынесен в background coordinator;
- `bebd00e` — service-local consumption и generation fencing;
- `1026458` — status только из confirmed snapshot;
- `1ee1886` — механическая нормализация switch logic;
- `8c0ff08` — публикация runtime redetection layout shortcut;
- `78f37b6` — рабочее XKB observation/switching для Cinnamon;
- `2523d67` — observation failure больше не освежает прежнее состояние.

## Дополнительные дефекты, найденные во время runtime-проверки

### Неработающий Cinnamon backend

Первый установленный candidate выявил подтверждённый runtime-дефект: Cinnamon
6.4.8 не предоставляет использованные методы `GetInputSources` и
`ActivateInputSourceIndex`. Observation становился `Unknown`, статус запрашивал
повторное обновление, а коррекции систематически пропускались. Это не было видно
по fake-backend unit tests.

Исправление `78f37b6` читает текущую XKB group напрямую и переключает её через
XKB с последующим XTest replay. До мутации проверяется, что активны ровно две
groups; иное состояние отклоняется. В Mint после исправления:

- strategy: `cinnamon-xkb-xtest`, ready;
- auto correction: pass;
- manual correction: pass, 56 ms;
- за 10 s наблюдались ожидаемые 32 poll tick и 0 feedback/deferred-status storm.

### Ложное освежение старого observation

Финальный review нашёл второй подтверждённый дефект: успешный legacy backend
мог вернуть общий success, хотя обязательное GNOME/Cinnamon observation чтение
завершилось ошибкой. Старое значение тогда получало новый `confirmed_at` и могло
разрешить неверную коррекцию.

`2523d67` возвращает `BackendSyncResult::Skipped` при таком observation failure.
Два новых regression tests сначала получили `Unchanged` вместо ожидаемого
`Skipped`, затем прошли после исправления. Устаревшее значение теперь не может
получить новую freshness без подтверждения.

## Идентичность финального Debian package

- build flow: `./manage.sh package deb`;
- package: `open-switcher_0.1.0-1_amd64.deb`;
- размер: `3 091 644` bytes;
- SHA-256 package:
  `1ceeaa5e9bddaaf4308080f26bb80e05516962bee8537f8e23f870e18c2d742c`;
- сохранённая копия с тем же SHA-256:
  `/home/andrey/VMs/OpenSwitcherLab/artifacts/open-switcher_0.1.0-1_amd64_1ceeaa5e9bdd.deb`.

Packaged binaries:

| Binary | SHA-256 |
|---|---|
| `/usr/bin/open-switcher-daemon` | `01fe4439f37f384d6bc0ae59f8358caccdb041bbf77caefa69c153e88892fd8a` |
| `/usr/bin/open-switcher-settings` | `422544f687cac94e19031a0bb3bb581b830ade2bebd21bb098bd4c0a65821297` |
| `/usr/bin/open-switcher-tray` | `8c3ef5f0a2b52a31d1148ccd7ceec6498c80d4150c32a6a4b790a950fa476b23` |

В Ubuntu и Mint установлена версия `0.1.0-1`; SHA-256 реально запущенного
`/usr/bin/open-switcher-daemon` в обеих VM совпал с packaged binary.

## Локальная и package-верификация

| Проверка | Результат |
|---|---|
| stable `rustfmt --check` для изменённых Rust-файлов | pass |
| `cargo check --all-targets` | pass |
| `cargo check --all-targets --features settings-ui` | pass |
| `cargo test --lib -- --test-threads=1` | 561 passed |
| `cargo test --lib --features settings-ui -- --test-threads=1` | 622 passed |
| `cargo test --test dbus_api -- --test-threads=1` | 11 passed |
| runtime-focused tests | 91 passed |
| `bash tests/linux_input_setup_test.sh` | pass |
| `bash tests/debian_package_scripts_test.sh` | pass |
| `bash tests/manage_package_deb_test.sh` | pass |
| `git diff --check` | pass |

Canonical package build повторно выполнил base, D-Bus и settings-ui test
matrices. `lintian` оставил только прежние неблокирующие package warnings:
отсутствующие man pages/AppStream metadata и initial-upload bug closure.

При одном раннем прогоне внутри restricted host sandbox девять environment tests
получили ожидаемый `EPERM`: четыре session D-Bus и пять Unix-socket tests. Те же
tests прошли в разрешённом окружении/package build без изменения кода; это
ограничение sandbox, а не скрытое исключение из итоговых чисел.

## Двухпрофильная package-first проверка

Граница лаборатории:

- host `/dev/input` и `/dev/uinput` не передавались гостям;
- ввод шёл только через виртуальную QEMU USB keyboard;
- host/guest clipboard и shared folders отсутствуют;
- guest network — QEMU user NAT, SSH опубликован только на `127.0.0.1`;
- package, systemd и fault injection изменялись только внутри гостей;
- лаборатория и обе VM сохранены и не удалялись.

### Полный fresh-path candidate

Перед последним узким fail-closed commit один и тот же candidate package
`b3495d3f2e2e355346bf173608366e66703dd5ca6253ff653888a79b995850b1`
прошёл в обеих VM ordinary input, auto correction, manual correction, Caps Lock
case repair и two-capitals repair. Exact receiver oracle в обеих системах:

```text
hello123
привет␠
Hello␠
Hello␠
привет
hello
```

Знак `␠` обозначает сохранённый пробел перед переводом строки.

SHA-256 input в обеих VM:
`2fb9cbefca4eda14cf2dd865dd785885be7c09c8dcba0a7adb9958a79fa88679`.
Ubuntu manual correction заняла 109 ms; Mint — 56 ms.

Между этим candidate и финальным package изменена только семантика failed
observation в `2523d67`; normal fresh path не менялся. Тем не менее после
пересборки финальный package был установлен заново в обе VM и повторно проверен
на ordinary input и auto correction. В обеих системах exact suffix был:

```text
final2
привет␠
```

Оба daemon unit остались active, а D-Bus подтвердил переход English -> Russian.

Сохранённые final-package evidence:

| Profile | Evidence | SHA-256 |
|---|---|---|
| Ubuntu 24.04.4 / GNOME / Wayland | `runs/ubuntu-cloud-provision-v1/h01-snapshot-final-package-input.txt` | `471db94bc96f8fe2ca06b7cb4cfd318c4b9d8d3eeff566017ac51eb47699f5e2` |
| Ubuntu frame | `runs/ubuntu-cloud-provision-v1/h01-snapshot-final-package-smoke.ppm` | `ff6a5ad4e794ffc410d4cafa1e763b689bbd3f65d8c63682af809e2b552fb35e` |

В первом Mint final-package вводе отсутствовал начальный `f` строки `final1`.
Контроль без warm-up затем передал `final3` целиком, а повтор с нейтральным
`Pause` перед oracle также прошёл. Аномалия не воспроизвелась и не коррелировала
с layout correction; поэтому она записана как неопределённость QMP/focus, а не
как подтверждённый дефект OpenSwitcher.

### Финальный hung-backend сценарий в Mint

Проверялся именно daemon hash `01fe4439...` из финального package:

1. Fake `xset` делегировал в `/usr/bin/xset` до arm marker.
2. После явного журнала `input-pipeline-prepared` строка `pre4` дошла целиком.
3. Marker заставил только guest shim `xset -q` зависнуть на `sleep 600`.
4. Наличие дочернего `/bin/sh .../xset -q` было подтверждено по PID.
5. При зависшем backend дошли `alive4` и исходное `ghbdtn `.
6. Журнал зафиксировал `space-correction-skip status=Stale` и
   `boundary-case-correction-skip status=Stale`; input heartbeat продолжался.
7. `systemctl --user stop open-switcher-daemon.service` завершился за
   `0.107306` s с return code 0.
8. Unit стал `inactive/dead`, зависших дочерних `xset` не осталось.
9. После stop строка `afterstop4` дошла напрямую, подтверждая возврат ввода.

Exact suffix:

```text
pre4
alive4
ghbdtn␠
afterstop4
```

Evidence:

| Evidence | SHA-256 |
|---|---|
| `runs/mint-install-v1/h01-snapshot-final-package-hung-input.txt` | `059beba12255b9815ebfa94b90480ca53539492dee5609ff71e619eaca4f3d53` |
| `runs/mint-install-v1/h01-snapshot-final-package-hung-backend.ppm` | `96e67b8416a02287f2adb802772b13ecbcdf134f7738282edd4a7294e45c2411` |

После теста arm marker удалён, debug variables сняты, normal `PATH` восстановлен,
daemon снова active, D-Bus отвечает, current layout подтверждён как English.

## Fail-safe оценка

В проверенном H-01 scope механизм теперь **fail-open для физического ввода**:
зависание layout observation не остановило event forwarding и не помешало
быстрому освобождению backend при остановке процесса. Старое или неподтверждённое
состояние больше не разрешает коррекцию.

Весь механизм захвата и освобождения пока нельзя назвать безусловно fail-safe:

- зависший external child не отменяется, пока daemon остаётся жив; зависает один
  background coordinator, а systemd stop завершает весь control group;
- polling Cinnamon раз в 300 ms в QEMU дал около 6.24% одного guest vCPU за
  10 s; это подтверждённое измерение candidate с той же polling архитектурой,
  но влияние на реальном host требует profiling;
- Cinnamon correction намеренно поддерживает только ровно две XKB groups;
- этот slice не повторял kill -9/power loss во время самого backend hang,
  замороженный display server или kernel, hot-unplug/replug и multi-seat;
- QEMU keyboard не покрывает всё физическое hardware/firmware;
- clipboard и selected-text paths этой кампанией не активировались;
- остаются отдельные lifecycle/conservation findings: writer shutdown/late
  mutation, deferred queue oracle, failure-at-every-step synthetic replay и
  ACL/multi-seat/package-upgrade границы.

## Следующие шаги

1. Не менять свежий пользовательский path без отдельного дефекта; наблюдать
   реальные correction misses в обычной эксплуатации.
2. Уменьшить стоимость 300 ms polling либо перейти на event-driven observation,
   сохранив ту же freshness/epoch семантику.
3. Добавить cancellation/deadline для зависшего layout child, чтобы coordinator
   восстанавливался без restart daemon.
4. Следующим safety slice закрыть writer shutdown и late synthetic mutation,
   затем deferred queue conservation и failure-at-every-step replay.
5. После связного следующего slice снова собирать canonical `.deb` и повторять
   только релевантную VM regression; лабораторию не удалять без прямой просьбы.
