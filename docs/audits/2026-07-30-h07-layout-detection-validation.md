# Проверка H-07: fail-closed определение раскладок

**Дата:** 2026-07-30

**Ветка:** `fix/h07-fail-closed-layout-detection`

**База:** `7e5046ebb35aa35cb80ba1f3aecedb8b4fae847a`

**Проверенное production-состояние:**
`225b0d9aab698a055de9b0254b3dd2346575c65a`

**Статус:** реализация, локальные gates, сборка Debian package и целевые
проверки в Linux Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland завершены.
Слияние в `master` и отправка в remote не выполнялись.

## Краткий результат

H-07 закрыта в согласованной области:

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
- X11-замена `us,ru` на `us,de` с тем же числом групп немедленно лишает
  старую пару доверия и запускает одноразовую фоновую перепроверку;
- ручная комбинация не переопределяется, новый постоянный polling, новый
  поток и внешняя команда в input path не добавлены;
- один и тот же финальный DEB установлен и физически проверен в обеих ВМ.

## Дефект, найденный на финальном review

После первоначальной VM-матрицы review диапазона обнаружил реальный
непокрытый X11-сценарий. Runtime перепроверял setup при изменении числа XKB
групп, но не мог отличить:

```text
us,ru -> us,de
```

В обеих конфигурациях `num_groups=2`. На кандидате
`8dc986b75e05d32c824bb6b9052a9ad706127ec1` это было воспроизведено в Mint:
`setxkbmap -query` уже показывал `us,de`, но перехода
`layout-setup-detection` не происходило и старый `StrictPair` оставался
доверенным.

Исправление использует уже существующий X11 watcher:

1. `ActiveWindowMonitor` один раз intern-ит `_XKB_RULES_NAMES`;
2. существующая подписка root window `PropertyNotify` публикует отдельный
   `KeyboardLayoutChanged`;
3. capacity-one atomic flag потребляется до обработки следующего fetched
   keyboard batch;
4. layout epoch инвалидируется сразу, а существующий background coordinator
   выполняет `setxkbmap -query` и классификацию вне watcher/input path.

Постоянный setup polling, дополнительный worker и blocking I/O на нажатии
клавиши не появились.

## Коммиты

```text
f3d692b docs: plan fail-closed layout detection
abd5f0c fix: classify layout setup from trusted sources
084ef87 fix: require trusted layout setup and current group
78baabe fix: recover trusted layout setup off the input path
02df387 packaging: require layout detection tools
5b9356e test: isolate layout refresh from host session
8c7a360 fix: recover auto layout switch after setup
8dc986b fix: recover combo from gnome observation
6553170 docs: validate fail-closed layout detection
21ad8dd docs: cover x11 same-count layout changes
225b0d9 fix: invalidate changed x11 layout setup
```

Коммиты `8c7a360` и `8dc986b` появились по результатам первоначальной
VM-проверки. Первый восстанавливал комбинацию после retry backend, но Ubuntu
показала ещё один путь: GNOME observation мог подтвердить setup раньше
retry. Коммит `8dc986b` добавил тот же одноразовый recovery к этому переходу.

Коммиты `21ad8dd` и `225b0d9` закрывают найденный на последующем review
same-count X11-сценарий.

## Идентичность финального пакета

```text
Package:      open-switcher
Version:      0.1.0-5
Architecture: amd64
Size:         3361038 bytes

Path:
/home/andrey/Projects/OpenSwitcher/.worktrees/h07-fail-closed-layout-detection/dist/packages/open-switcher_0.1.0-5_amd64.deb

SHA-256:
12de80c3f5acac2118304784d1bc729882bcce8e784ddd1921355cf292a9dc0a
```

Хеши бинарников внутри DEB:

| Файл | SHA-256 |
|---|---|
| `open-switcher-daemon` | `3ad29b9365fbf6ce08c47e3abf9fe8eb142b1f8ceb3491b1fb2fb0f700cdcfbd` |
| `open-switcher-tray` | `e5846abc9037b24a3bf77c28c172c8074470491575bb971c7a719071df888e36` |
| `open-switcher-settings` | `d7354e273967be2da22ac664a131056fe5991b53cc9a75fee5c2ed796f3049ca` |

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

Предыдущий SHA
`2e1e9c809acce76a0707710a5c8e1ad463013f01e414b8a0cd47a9e565688b44`
считается superseded и не является финальным пакетом.

## Локальные gates

Полная безопасная Rust-регрессия:

```bash
cargo test --locked --all-targets --features settings-ui \
  -- --test-threads=1
```

Результат:

```text
library:       981 passed, 1 ignored
daemon binary:   4 passed
D-Bus:          11 passed
VM probe:         5 passed
total:         1001 passed, 0 failed, 1 ignored
```

Дополнительно:

| Проверка | Результат |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `git diff --check` | exit 0 |
| `tests/debian_package_scripts_test.sh` | `ok` |
| `tests/manage_package_deb_test.sh` | `ok` |
| все `daemon::runtime` tests после финальной правки | 87/87 passed |
| затронутые `input_target_` watcher tests | 5/5 passed |
| новый atomic X11 event regression test | passed |
| новый same-count runtime fail-closed regression test | passed |

Restricted syscall sandbox блокирует один существующий unit-тест ожидания
Unix socket shutdown. Тот же изолированный набор и полный suite выполнены
вне seccomp и завершились успешно. Тест не открывает физические устройства;
признаков зависания production-кода этот случай не дал.

## Финальный Linux Mint / Cinnamon / X11

В гостевую систему установлен точный финальный SHA:

```text
12de80c3f5acac2118304784d1bc729882bcce8e784ddd1921355cf292a9dc0a
```

Исходное состояние:

```text
layout:  us,ru
variant: ,
options: grp:win_space_toggle,terminate:ctrl_alt_bksp
MainPID=3766
NRestarts=0
```

### Обычная F12-коррекция

Через QEMU virtual keyboard в Xed набраны физические keycodes, которые в RU
дают `екгу`, затем нажат F12. Контрольный файл:

```text
true
```

### Замена на `us,de` с тем же числом групп

Без restart daemon выполнен `setxkbmap -layout us,de`. Журналы подтвердили:

```text
[input-debug] stage=layout-setup-change
source=x11-root-property atom=_XKB_RULES_NAMES

[layout-debug] stage=layout-setup-detection
strategy=x11-setxkbmap result=unsupported
compatibility=Unsupported generation=4
reason=russian-layout-missing
```

Физически набранное `test` после F12 осталось:

```text
test
```

Daemon сохранил `MainPID=3766`, `NRestarts=0`.

### Возврат `us,ru`

Без restart daemon выполнен возврат `setxkbmap -layout us,ru`. Журнал:

```text
[layout-debug] stage=layout-setup-detection
strategy=x11-setxkbmap result=confirmed
compatibility=FullStrictPair generation=6 reason=none
```

Повторная физическая проверка `екгу` + F12 снова дала:

```text
true
```

PID остался `3766`, `NRestarts=0`.

## Финальный Ubuntu / GNOME / Wayland

В гостевую систему установлен тот же точный финальный SHA:

```text
12de80c3f5acac2118304784d1bc729882bcce8e784ddd1921355cf292a9dc0a
```

Подтверждены:

```text
XDG_SESSION_TYPE=wayland
sources=[('xkb', 'us'), ('xkb', 'ru')]
mru-sources=[('xkb', 'us'), ('xkb', 'ru')]
layout_switch_combo=SuperSpace
MainPID=7578
NRestarts=0
```

Через QEMU virtual keyboard в GNOME Text Editor набраны физические keycodes
в RU и нажат F12. Контрольный файл содержит:

```text
true
```

Финальная production-правка выполняется только в X11 watcher, поэтому на
новом SHA для Wayland повторены package identity и обычный физический smoke.
Расширенная GNOME recovery-матрица была выполнена на непосредственно
предшествующем кандидате до этой X11-only правки.

## Расширенная матрица на предшествующем кандидате

На superseded SHA
`2e1e9c809acce76a0707710a5c8e1ad463013f01e414b8a0cd47a9e565688b44`
дополнительно прошли:

- Mint: временная недоступность `setxkbmap`, fail-closed до marker и
  восстановление без restart;
- Mint: `us,de,ru` как `PairPlusOther`, F12 не изменяет `test`;
- Ubuntu: временная недоступность GNOME sources и combo, восстановление
  `FullStrictPair` и `SuperSpace` без restart;
- Ubuntu: трёхсекундное наблюдение после recovery — 18 reads sources/MRU и
  0 повторных combo-detection calls;
- Ubuntu: extra source `us,de,ru`, F12 не изменяет `test`.

Ещё более ранние кандидаты проверялись на automatic correction, исправление
двух заглавных, исправление текста после случайного Caps Lock и
selected-text conversion. Эти результаты полезны как поведенческая
регрессия, но не выдаются за package-identity доказательство финального DEB.

## Ограничения и остаточные наблюдения

- Проверка относится к поддерживаемым профилям Cinnamon/X11 и
  GNOME/Wayland, обычным XKB `us|gb` + `ru` и безопасному отказу для
  дополнительных/неподдерживаемых sources. KDE, неизвестные Wayland
  compositor, IBus и нестандартные варианты намеренно остаются fail-closed.
- На новом финальном SHA не повторялась вся расширенная transient/extra
  матрица: после её прохождения изменился только event-driven X11 сигнал.
  Финальный SHA прошёл полный Rust suite, целевой same-count сценарий и
  физический normal smoke в обеих ВМ.
- Полный объединённый runtime campaign всех исправлений аудита не выполнялся:
  это отдельный отложенный этап после остальных поведенческих задач.
- В ранней дополнительной проверке исправление случайного Caps Lock меняло
  видимый текст на `Hello`, но физическое состояние Caps Lock оставалось
  включённым. Это не регрессия H-07; желаемое поведение следует отдельно
  зафиксировать перед возможной задачей.
- Runtime recovery обновляет опубликованный snapshot, но не записывает
  recovered auto-комбинацию на диск. При следующем запуске обычный startup
  detection снова определяет её. Это избегает фоновой записи конфигурации и
  не влияет на текущую сессию.

## Доказательства и состояние лаборатории

Гостевые журналы сохранены в:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h07-layout-detection/
/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h07-layout-detection/
```

Ключевые финальные файлы:

```text
Mint:
  review-same-count-stale-setup.log
  final-same-count-summary.txt
  final-same-count-input.log
  final-same-count-layout.log
  final-same-count-normal.txt
  final-same-count-unsupported.txt
  final-same-count-recovered.txt

Ubuntu:
  final-sha-wayland-summary.txt
  final-sha-wayland-input.log
  final-sha-wayland-layout.log
  final-sha-wayland-normal.txt
```

Обе ВМ штатно выключены. QMP sockets отсутствуют. Base images, overlays,
ключи и все evidence сохранены; лаборатория не удалялась и не
перестраивалась.

## Итог

В пределах H-07 механизм ведёт себя fail-closed:

- неподтверждённые данные не разрешают удаление и перепечатку текста;
- подтверждённая `us/ru` сохраняет прежнюю F12-функциональность;
- extra или изменённая неподдерживаемая раскладка не запускает destructive
  коррекцию;
- временный ранний отказ восстанавливается без restart;
- GNOME не остаётся со stale `CtrlShift`;
- X11 same-count изменение больше не сохраняет stale `StrictPair`;
- input path не получил внешних вызовов или нового blocking I/O.

Новые задачи аудита из этой ветки не начинались. Перед интеграцией остаётся
только решение пользователя о merge; ветка намеренно остановлена до этого
решения.
