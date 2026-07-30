# Проверка H-07: fail-closed определение раскладок

**Дата:** 2026-07-30

**Ветка:** `fix/h07-fail-closed-layout-detection`

**База:** `7e5046e`

**Проверенное production-состояние:** `8dc986b75e05d32c824bb6b9052a9ad706127ec1`

**Статус:** реализация, локальные gates, финальный Debian package и целевые
проверки в Linux Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland завершены. Слияние
в `master` и отправка в remote не выполнялись.

## Краткий результат

H-07 можно считать закрытой в согласованной области:

- ошибка, пустой или неподдерживаемый ответ источника больше не создаёт
  выдуманную `StrictPair` US/RU;
- destructive layout-dependent коррекция разрешается только при
  подтверждённых setup и текущей раскладке;
- дополнительная раскладка переводит setup в `PairPlusOther` и не допускает
  F12-коррекцию через небезопасный `switch_next`;
- временно недоступные `setxkbmap` и `gsettings` дают fail-closed состояние,
  после чего функции восстанавливаются без перезапуска daemon;
- ранний отказ GNOME `gsettings` больше не оставляет runtime с резервной
  комбинацией `CtrlShift`: после восстановления setup один раз определяется
  и публикуется фактическая `SuperSpace`;
- ручная комбинация не переопределяется, новый постоянный опрос комбинации не
  добавлен;
- один и тот же финальный DEB установлен и проверен в обеих ВМ.

## Коммиты реализации

```text
f3d692b docs: plan fail-closed layout detection
abd5f0c fix: classify layout setup from trusted sources
084ef87 fix: require trusted layout setup and current group
78baabe fix: recover trusted layout setup off the input path
02df387 packaging: require layout detection tools
5b9356e test: isolate layout refresh from host session
8c7a360 fix: recover auto layout switch after setup
8dc986b fix: recover combo from gnome observation
```

Последние два коммита появились по результатам VM-проверки. Первый вариант
восстанавливал комбинацию после retry backend, но Ubuntu показала ещё один
реальный путь: GNOME observation мог подтвердить setup раньше retry. В этом
случае setup становился `FullStrictPair`, а комбинация оставалась
`CtrlShift`. Коммит `8dc986b` добавил тот же одноразовый recovery к переходу,
подтверждённому GNOME observation.

## Идентичность финального пакета

```text
Package:      open-switcher
Version:      0.1.0-5
Architecture: amd64
Size:         3306366 bytes

Path:
/home/andrey/Projects/OpenSwitcher/.worktrees/h07-fail-closed-layout-detection/dist/packages/open-switcher_0.1.0-5_amd64.deb

SHA-256:
2e1e9c809acce76a0707710a5c8e1ad463013f01e414b8a0cd47a9e565688b44
```

Хеши бинарников внутри DEB:

| Файл | SHA-256 |
|---|---|
| `open-switcher-daemon` | `df3ac06bfdd8321eb3eb65b6e08f30f4038bdb0b71af41c88297f8549694e09a` |
| `open-switcher-tray` | `8fdce53631548dfe2be36c55cb44aef908616fd25436deef6096b2ff6b077ada` |
| `open-switcher-settings` | `47aa283b39bacfbe5e442e3586c90a752f1a5ab029c03a48235e28220c46ccb6` |

В `Depends` подтверждены:

- `x11-xkb-utils`;
- `libglib2.0-bin`;
- `gsettings-desktop-schemas`.

DEB собран командой:

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
```

`nocheck` использован после отдельного полного Rust-прогона и не меняет
release binary.

## Локальные gates

Полная безопасная Rust-регрессия:

```bash
cargo test --locked --all-targets --features settings-ui \
  -- --test-threads=1
```

Результат:

```text
library:       980 passed, 1 ignored
daemon binary:   4 passed
D-Bus:          11 passed
VM probe:         5 passed
total:         1000 passed, 0 failed, 1 ignored
```

Дополнительно:

| Проверка | Результат |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `git diff --check` | exit 0 |
| `tests/debian_package_scripts_test.sh` | `ok` |
| `tests/manage_package_deb_test.sh` | `ok` |
| все `daemon::runtime` tests после последней правки | 86/86 passed |
| regression test GNOME observation recovery | passed |
| regression tests setup recovery/manual preservation | 2/2 passed |

Один запуск полного suite внутри restricted syscall sandbox завис на уже
известном тесте Unix socket shutdown. Неизменённый набор был сразу повторён
вне seccomp-песочницы и завершился результатом выше. Признаков зависания
production-кода этот случай не дал.

## Linux Mint / Cinnamon / X11

В гостевую систему установлен точный финальный SHA:

```text
2e1e9c809acce76a0707710a5c8e1ad463013f01e414b8a0cd47a9e565688b44
```

### Нормальная пара `us,ru`

`setxkbmap -query` подтвердил:

```text
layout:  us,ru
variant: ,
options: grp:win_space_toggle,terminate:ctrl_alt_bksp
```

Через QEMU virtual keyboard в Xed набраны физические keycodes, которые в RU
дают `екгу`, затем нажат F12. Сохранённый файл содержит:

```text
true
```

Daemon сохранил `MainPID=3833`, `NRestarts=0`.

### Временная недоступность `setxkbmap`

Guest-only wrapper до marker возвращал ошибку:

```text
strategy=x11-setxkbmap result=temporary
```

До marker слово `test` после F12 осталось `test`. После marker тот же daemon,
без restart, опубликовал:

```text
compatibility=FullStrictPair
```

PID остался `4772`, `NRestarts=0`. Повторная физическая проверка `екгу` + F12
сохранила `true`.

Комбинация уже была высокодостоверной `AutoDetected(SuperSpace)`, поэтому
одноразовый recovery корректно записал:

```text
result=skipped reason=not-auto-fallback
```

### Дополнительная раскладка

Для `us,de,ru` журнал подтвердил:

```text
compatibility=PairPlusOther
```

Физически набранное `test` после F12 осталось `test`; PID `4772` и
`NRestarts=0` не изменились.

## Ubuntu / GNOME / Wayland

В гостевую систему установлен тот же точный финальный SHA:

```text
2e1e9c809acce76a0707710a5c8e1ad463013f01e414b8a0cd47a9e565688b44
```

### Восстановление setup и комбинации

Guest-only `gsettings` wrapper до marker сделал недоступными и GNOME sources,
и определение комбинации. Стартовое состояние было ожидаемым:

```text
switch_combo  = CtrlShift
switch_source = AutoFallback
setup         = TemporarilyUnavailable
```

После marker тот же daemon с `MainPID=8653`, `NRestarts=0` опубликовал:

```text
compatibility=FullStrictPair
combo=SuperSpace
source=AutoDetected
confidence=High
stage=layout-switch-setup-recovery
```

Физически в GNOME Text Editor набраны keycodes `t,r,u,e` в RU и нажат F12.
Сохранённый файл содержит `true`, а GNOME MRU после коррекции показывает US.

### Отсутствие нового polling комбинации

После recovery выполнено отдельное трёхсекундное наблюдение wrapper:

```text
total calls:    18
sources calls:   9
MRU calls:       9
combo calls:     0
```

То есть остался существующий опрос текущего GNOME source, но повторного
определения `SuperSpace` в steady state нет.

### Дополнительная раскладка

GNOME sources и MRU были выставлены в:

```text
[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'ru')]
```

Физически набранное `test` после F12 осталось `test`; daemon был active,
`NRestarts=0`.

## Что дополнительно проверялось на ранних кандидатах

До обнаружения пропущенного GNOME recovery-пути VM-матрица также включала:

- automatic correction;
- исправление двух заглавных;
- исправление текста после случайного Caps Lock;
- selected-text conversion при `PairPlusOther`.

Эти проверки прошли, но их SHA (`256c1eb…` и затем `1362f1b…`) не являются
финальным пакетом. После них production-изменения были ограничены двумя
одноразовыми recovery-путями. На финальном SHA повторены критические
layout-dependent F12, transient и extra-layout сценарии, а остальные ветви
покрыты полным Rust suite. Результаты ранних пакетов не выдаются за
package-identity доказательство финального DEB.

## Ограничения и остаточные наблюдения

- Проверка относится к поддерживаемым профилям Cinnamon/X11 и
  GNOME/Wayland, обычным XKB `us|gb` + `ru` и безопасному отказу для
  дополнительных/неподдерживаемых sources. KDE, неизвестные Wayland
  compositor, IBus и нестандартные варианты намеренно остаются fail-closed.
- Полный объединённый runtime campaign всех исправлений аудита не выполнялся:
  это отдельный отложенный этап после остальных поведенческих задач.
- Длительные Ubuntu-сессии несколько раз завершались внешним лимитом
  launcher с кодом 130. Это не было падением OpenSwitcher. После одного
  такого завершения extra-layout проверка продолжена в новой загрузке.
  Основной файл `true` и recovery-журнал сохранены; отдельный объединённый
  physical-smoke log не успел синхронизироваться перед внешним выключением.
- В ранней дополнительной проверке исправление случайного Caps Lock меняло
  видимый текст на `Hello`, но физическое состояние Caps Lock оставалось
  включённым. Это не регрессия H-07 и не расширялось в текущей ветке; желаемое
  поведение следует отдельно зафиксировать перед возможной задачей.
- Runtime recovery обновляет опубликованный snapshot, но не записывает
  recovered auto-комбинацию на диск. При следующем запуске обычный startup
  detection снова определяет её. Это намеренно избегает фоновой записи
  конфигурации и не влияет на текущую сессию.

## Доказательства и состояние лаборатории

Финальные гостевые журналы сохранены в:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h07-layout-detection/
/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h07-layout-detection/
```

Имена финального прогона начинаются с `final-`; прежние evidence сохранены
отдельно и не смешиваются с exact final package.

Обе ВМ штатно выключены. QMP sockets отсутствуют. Base images, overlays,
ключи и все evidence сохранены; лаборатория не удалялась и не
перестраивалась.

## Итог

В пределах H-07 механизм теперь ведёт себя fail-closed:

- неподтверждённые данные не разрешают удаление и перепечатку текста;
- подтверждённая `us/ru` восстанавливает прежнюю F12-функциональность;
- extra layout не запускает разрушительную коррекцию;
- временный ранний отказ восстанавливается без restart;
- GNOME не остаётся со stale `CtrlShift`;
- input path не получил внешних вызовов или нового blocking I/O.

Перед слиянием требуется только финальный review диапазона
`7e5046e..HEAD`, проверка чистоты ветки и отдельное решение пользователя о
merge. Новые задачи аудита из этой ветки не начинались.
