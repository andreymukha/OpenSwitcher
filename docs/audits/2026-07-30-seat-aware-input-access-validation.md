# Проверка seat-aware доступа к input-устройствам

**Дата:** 2026-07-30

**Ветка:** `fix/seat-aware-input-access`

**База:** `65eed1d18de3d81d9f7eef22ea2ccb91112ba84c`

**Проверенное состояние исходников:**
`e6c22985147d557440004ed29c84d0f2549a47f6`

**Статус:** реализация, локальные gates, Debian package и две VM-кампании
завершены; автоматическое слияние в `master` не выполнялось.

## Краткий результат

Закрыты три долга аудита:

- **H-08:** blanket ACL заменён на seat-aware `uaccess`, daemon проверяет
  активную локальную графическую сессию и seat до захвата устройства и перед
  input mutation;
- **M-09:** package stop имеет ограниченный deadline и проверяемое
  постусловие, а remove/purge очищает только точно записанные ACL после
  повторной проверки пути, `devnum`, UID и текущих udev tags;
- **L-02:** пакет устанавливает одно каноническое правило
  `70-openswitcher-input.rules`; прежний ACL bridge и дублирующее правило
  удалены.

В Mint и Ubuntu подтверждено:

- старый daemon не переживает upgrade/reinstall рядом с новым бинарником;
- активная сессия получает доступ, неактивная теряет ACL;
- уже открытый input backend освобождается при деактивации сессии;
- тот же daemon автоматически восстанавливает backend после возврата;
- lock/unlock не вызывает ложного отключения;
- F12, автопереключение, исправление двух заглавных и случайного Caps Lock
  продолжают работать;
- remove и purge не оставляют process, package tag, manifest или ACL.

Механизм можно считать fail-safe в проверенной модели отказов: потеря
авторизации сначала делает старую lease недействительной, после чего backend
проходит уже существующий подтверждаемый shutdown. Нельзя обещать
освобождение средствами самого процесса при зависании всего процесса в
непрерываемом kernel wait; этот остаточный предел описан ниже.

## Идентичность итогового пакета

```text
Package: open-switcher
Version: 0.1.0-4
Architecture: amd64

Path:
/home/andrey/Projects/OpenSwitcher/.worktrees/seat-aware-input-access/dist/packages/open-switcher_0.1.0-4_amd64.deb

Size:
3297896 bytes

SHA-256:
554c67eccf93435758071387f7fb46c9114ab11022b3892d5299fdf6f6b88c67
```

Хеши бинарников внутри пакета:

| Файл | SHA-256 |
|---|---|
| `open-switcher-daemon` | `770e0ca7c778b28eb3da7e67f06b1ba5b0e29d8f5edfb4fcc80e321e48141341` |
| `open-switcher-tray` | `b544c0591b08b0a77ac534346ec9e232f3a4977276680ca94784543a9f730ed4` |
| `open-switcher-settings` | `a68207f24c130b6237991e1e75b9921a067bdd060ff14d6f77800b1490052be4` |

Финальный exact DEB с SHA-256 `554c67…88c67` установлен и удалён в Mint.
Полная функциональная матрица до последней сборки выполнялась на
`96142c81ec35318d49669db95bc3f5bb6de83f914c813401758df93b9225d6cc`.
Между этими двумя сборками production-логика и Rust-бинарники не менялись:
уточнён только поясняющий комментарий в `postrm` и имя shell-теста. После
этого финальный exact DEB отдельно прошёл install/purge sanity.

## Архитектурные инварианты

### Сессия

Авторизуется только одна сессия, которая одновременно:

- принадлежит UID daemon;
- локальная и не remote;
- имеет class `user`;
- имеет type `x11` или `wayland`;
- активна;
- привязана к непустому seat.

Отсутствие кандидата, неоднозначность и потеря logind переводят publication в
fail-closed состояние. Смена session/seat увеличивает generation и делает
старые token недействительными до публикации нового.

### Захват и mutation

Session token проверяется:

- до открытия и захвата физического устройства;
- после `EVIOCGRAB`;
- на writer admission перед обычными mutation;
- на recovery и при смене generation.

Cleanup/release не блокируется потерей авторизации, поэтому запрет новых
mutation не мешает вернуть пользователю физическое устройство.

### Package access

Пакет:

- использует `TAG+="uaccess"` и `TAG+="openswitcher-input"`;
- требует `udev (>= 247)`, потому что только `CURRENT_TAGS` отражает последний
  rules pass, а `TAGS` начиная с udev 247 является липкой историей;
- разрешает runtime backend только для `seat0`, поскольку `/dev/uinput`
  является общесистемным узлом без seat identity;
- сохраняет приватный root-owned manifest только для реально выданных ACL;
- при remove повторно проверяет canonical path, character type, `devnum`, UID,
  exact `rw-` entry и текущий `uaccess`;
- не запускает повторный udev trigger при последующем `postrm purge`, если
  manifest уже успешно удалён.

## Локальные gates

### Полная Rust-регрессия

На том же Rust-состоянии до VM-кампании выполнено:

```text
library:       953 passed, 1 ignored
daemon binary:   4 passed
D-Bus:          11 passed
VM probe:        5 passed
total:          973 passed, 1 ignored
```

Команда:

```bash
cargo test --locked --all-targets --features settings-ui \
  -- --test-threads=1
```

После последних shell-only изменений повторены узкие Rust gates:

| Проверка | Результат |
|---|---|
| `session_activity` | 13 passed |
| `input_device_identity` | 3 passed |
| `session_change_` | 4 passed |
| `input_target_stop_signal_wakes_idle_waiter` | 1 passed |
| `repeated_input_target_stop_is_idempotent` | 1 passed |
| `cargo fmt --check` | exit 0 |

Два stop-теста внутри restricted syscall sandbox ранее зависали. Те же точные
команды с 30-секундным deadline вне sandbox завершились за доли секунды.
Production hang не воспроизведён.

### Shell/package gates

| Проверка | Результат |
|---|---|
| `tests/input_access_package_test.sh` | `ok` |
| `tests/debian_package_scripts_test.sh` | `ok` |
| `tests/wayland_diagnostics_test.sh` | `ok` вне syscall sandbox |
| `tests/linux_input_setup_test.sh` | `ok` |
| `tests/manage_package_deb_test.sh` | `ok` |
| `bash -n` для maintainer scripts/helpers | exit 0 |
| `shellcheck` для изменённых scripts/tests | exit 0 |
| `git diff --check` | exit 0 |

Wayland diagnostics внутри restricted syscall sandbox получил `EPERM` на
создании временного Unix socket. Неизменённый тест вне sandbox прошёл; это
ограничение среды запуска.

Строгий:

```bash
cargo clippy --locked --all-targets --all-features -- -D warnings
```

не является зелёным из-за существующего baseline lint debt, включая
deprecated API vendored `uinput 0.1.3` и прежние dead-code предупреждения.
Новые lint, добавленные этой веткой, устранены. Clippy-долг не выдаётся за
успешный gate.

### Сборка и статическая проверка DEB

Финальная сборка:

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
```

`nocheck` использован только после полного Rust gate и свежих shell gates,
чтобы не повторять неизменившуюся Rust-регрессию при пересборке package
metadata/scripts.

Подтверждено:

- metadata `0.1.0-4`, `amd64`;
- присутствует `70-openswitcher-input.rules`;
- присутствует executable
  `open-switcher-input-access-maintenance`;
- отсутствуют ACL bridge и `80-openswitcher-input.rules`;
- `udevadm verify` завершён успешно;
- maintainer scripts имеют mode `0755` и проходят `dash -n`;
- `dh_clean` больше не удаляет tracked
  `vendor/uinput-0.1.3/Cargo.toml.orig`.

`lintian` не выдал ошибок. Остались предупреждения:

- `appstream-metadata-missing-modalias-provide`;
- два `maintainer-script-calls-systemctl` в `preinst`;
- отсутствие man pages у трёх пользовательских бинарников.

Они не связаны с input trust boundary или удержанием устройств.

## Mint 22.2 Cinnamon X11

### Package lifecycle

Проверены:

- active upgrade `0.1.0-3 → 0.1.0-4`;
- same-version active reinstall;
- active remove;
- чистая повторная установка;
- purge.

Во всех replacement-сценариях прежний PID исчезал до использования нового
бинарника. Не было процесса с `(deleted)` executable. Daemon, tray и guardian
после штатного старта имели `NRestarts=0`.

Правило `70` и helper принадлежали пакету, правило `80` отсутствовало.
ACL выдавались только текущему владельцу `seat0`.

### Передача seat второму пользователю

Внутри VM создан пользователь `switchtest`, UID 1001.

При переходе с исходной сессии `c1` к `c3`:

- `c1` стала `Active=no`;
- исходный daemon PID `7496` остался жив, но имел `0` fd на
  `/dev/input/*` и `/dev/uinput`;
- ACL перешли только UID 1001.

После `loginctl activate c1`:

- тот же PID автоматически восстановил backend примерно за `459 ms`;
- ACL вернулись только UID 1000;
- старый текст не был переотправлен.

Lock/unlock не менял владельца seat и не отключал backend. Cinnamon не
публиковал ожидаемый `LockedHint=yes`, но `Active=yes`, fd и функциональность
оставались стабильными.

### Функциональность

Через QEMU USB keyboard и Xed проверены:

```text
ыгвщ + F12  -> sudo
ghbdtn + Space -> привет
TEst + Space -> Test
Caps ON, Shift+T, E,S,T, Caps OFF, Space -> Test
```

F12-транзакция заняла `113 ms`. Следующая буква после Caps-сценария осталась
строчной, физический Caps был выключен.

### Найденная при purge гонка

Первый candidate обнаружил воспроизводимый остаточный доступ:

- `CURRENT_TAGS` исчезли примерно на `3549 ms`;
- keyboard ACL удалились на `3635–3726 ms`;
- ACL мышей остались;
- ACL `/dev/uinput` вернулся примерно на `5641 ms`.

Подтверждены две первопричины:

1. код подставлял липкий исторический `TAGS`, когда пустой
   `CURRENT_TAGS` отсутствовал в выводе, и ошибочно считал старый `uaccess`
   текущим;
2. `apt purge` вызывает `postrm` сначала с `remove`, затем с `purge`.
   После первой успешной очистки manifest уже отсутствовал, но второй вызов
   снова выполнял `udevadm trigger` и возвращал ACL.

Исправление:

- использовать только `CURRENT_TAGS`;
- закрепить `udev >= 247`;
- проверять manifest до reload/trigger;
- оставить один ограниченный повторный ACL pass после первого trigger;
- добавить RED/GREEN-тесты sticky tags, manifest-free purge и позднего ACL
  update.

После исправления все шесть записанных ACL исчезли примерно к `3596 ms` от
начала purge и не появились в течение 15 секунд. Финальный exact DEB
`554c67…88c67` дополнительно дал через 10 секунд:

```text
uid1000_acl_count=0
package_tag_count=0
manifest_absent
package_absent
```

## Ubuntu 24.04.4 GNOME Wayland

### Package lifecycle

На чистой VM сначала установлен `0.1.0-3`. Baseline daemon PID `2613` держал:

```text
/dev/uinput
/dev/input/event2
/dev/input/event3
/dev/input/event4
/dev/input/event5
```

После active upgrade:

- PID `2613` исчез;
- установлен `0.1.0-4`;
- новый PID `4913` открыл тот же разрешённый набор;
- `dpkg -V` не сообщил расхождений.

После active reinstall PID `4913` исчез, новый PID `8467` восстановил backend.
Daemon, tray и guardian были active, `NRestarts=0`.

Active remove удалил PID, rule, helper, manifest и все ACL. После чистой
установки новый PID `25591` снова получил backend. Финальный purge через
10 секунд дал:

```text
uid1000_acl_count=0
package_current_tag_count=0
package_absent
```

Пользовательский `config.toml` сохранился.

### Lock и реальная деактивация

Обычный GNOME lock:

```text
Active=yes
LockedHint=yes
input fd=5
```

После unlock:

```text
Active=yes
LockedHint=no
input fd=5
```

GDM 46.2 не предоставляет `CreateTransientDisplay`, а `gdmflexiserver` из SSH
не считает SSH process частью графического seat. Поэтому реальная
деактивация проверена штатным переключением VM с графического `tty2` на
свободный VT:

```text
session Active=no: 33 ms
backend fd=0:       133 ms
ACL UID 1000:       0
```

После `loginctl activate 1`:

```text
первые восстановленные fd: 579 ms
stable backend:            5 fd
PID:                       тот же 8467
NRestarts:                 0
```

Полный переход доступа между двумя пользователями отдельно доказан в Mint;
Ubuntu-проверка подтверждает тот же обязательный
`Active=yes → no → yes` контракт на Wayland.

### Функциональность Wayland

Через QEMU USB keyboard в GNOME Text Editor сохранён буквальный файл:

```text
sudo
привет␠
Test␠
Test␠
```

Здесь `␠` обозначает один сохранённый конечный пробел.

Сценарии:

1. F12 `ыгвщ → sudo`;
2. auto correction `ghbdtn → привет`;
3. two capitals `TEst → Test`;
4. accidental Caps Lock `tEST → Test`.

F12 завершился за `108 ms` при пользовательской настройке `delay_ms=30`.
Input-debug был временно включён только внутри VM, содержал stage/latency и
длины буферов, но не пользовательский текст. Service остался active,
`NRestarts=0`. XTEST-specific проверки на Wayland не запускались.

## Дополнительные исправленные дефекты процесса и package tooling

Во время итогового gate найдены и устранены ещё три проблемы:

1. `tests/debian_package_scripts_test.sh` в двух no-process fixture читал
   настоящий host `/proc` и принимал запущенный package daemon за состояние
   fixture. Теперь `/proc` также перенаправляется в изолированное дерево.
2. Debian `dh_clean` удалял tracked файл
   `vendor/uinput-0.1.3/Cargo.toml.orig`, потому что удаляет `*.orig`.
   Добавлен узкий exception только для этого vendored файла.
3. Sticky `TAGS` и повторный manifest-free trigger оставляли input ACL после
   purge. Причина и runtime-доказательство приведены выше.

## Ограничения и остаточные риски

- `/dev/uinput` не имеет seat identity. Текущий безопасный выбор — поддержка
  только `seat0`; другая seat получает явный fail-closed отказ.
- Если весь process завис в непрерываемом kernel wait и его нельзя завершить,
  userspace не способен выполнить `Drop` или shutdown. Kernel закрывает fd и
  снимает `EVIOCGRAB` при фактическом завершении process; для неубиваемого
  D-state может потребоваться устранение kernel/device fault или reboot.
- VM проверяет настоящий Linux input/udev/logind path на QEMU USB keyboard,
  но не покрывает все firmware/USB/Bluetooth quirks физического оборудования.
- Runtime acceptance выполнен на двух официальных baseline:
  Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland. Другие desktop/session managers
  требуют отдельного acceptance при добавлении поддержки.
- Строгий clippy baseline остаётся отдельным техническим долгом и не закрыт
  этой задачей.
- Дополнительная одна секунда относится только к remove/purge cleanup и не
  влияет на запуск, набор текста или скорость коррекции.

## Операционный инцидент host isolation

Во время измерения VM-гонки одна диагностическая shell-команда имела
неверную границу удалённых кавычек: observer выполнялся в VM, а
`apt-get purge` непреднамеренно выполнился на host и удалил установленный
`open-switcher 0.1.0-3`.

Ошибка была обнаружена сразу. На host из сохранённого exact DEB восстановлен
тот же `0.1.0-3`; затем подтверждены:

```text
dpkg state: ii
version:    0.1.0-3
daemon:     active
tray:       active
guardian:   active
```

Незавершённый `0.1.0-4` на host не устанавливался. Пользовательская
конфигурация не удалялась. Этот эпизод означает, что абсолютное утверждение
«host udev/ACL ни разу не менялись» для всей кампании было бы неверным;
состояние восстановлено, но отклонение явно зафиксировано.

## Состояние передачи

- Ветка готова к финальному review и последующему решению о merge.
- Итоговый DEB собран, но на host не установлен.
- На host продолжает работать восстановленный `0.1.0-3`.
- Обе VM выключены.
- VM-диски, второй Mint user и все артефакты лаборатории сохранены.
- Лаборатория не удалялась и не должна удаляться без прямой просьбы
  пользователя.
