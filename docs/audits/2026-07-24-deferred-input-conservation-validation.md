# Проверка сохранения deferred-ввода и X11 focus barrier

- Дата завершения: 2026-07-24
- Ветка: `fix/deferred-input-conservation`
- База: `217c12e` (`docs: update audit remediation handoff`)
- Проверенный диапазон: `217c12e..a73bad8`
- Основной артефакт: Debian package `open-switcher 0.1.0-3`, `amd64`
- Runtime-среды: Linux Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland
- Статус: основной контракт сохранения ввода, штатного освобождения устройства
  и повторного захвата прошёл; два искусственных миллисекундных stress-сценария
  зафиксированы как принятый малозначимый риск

## Что исправлено

До этой работы физические события, пришедшие во время асинхронной F12-коррекции,
могли считаться доставленными раньше фактической записи virtual writer. При
ошибке, отмене или перестройке backend это создавало риск потери хвоста,
неполной пары press/release и логически зажатого модификатора.

Теперь действует следующий контракт:

1. Каждое принятое deferred-событие получает монотонный sequence id и остаётся
   в ledger до подтверждения обработки.
2. Событие, которое должно попасть в `/dev/uinput`, подтверждается только после
   успешных `write + synchronize` writer-потока.
3. Timeout, disconnect или ошибка writer не удаляют неоднозначную голову
   ledger. Она reconciled только терминальным recovery, без повторного
   воспроизведения через новое поколение backend.
4. Cancel, pointer/context invalidation, Wayland focus shortcut, ошибка
   коррекции и частично завершённая инициализация не выбрасывают уже принятый
   физический хвост.
5. Полные последовательности модификаторов сохраняют FIFO и обе половины
   press/release.
6. В X11 отдельное постоянное поколение `_NET_ACTIVE_WINDOW` не теряется при
   чтении одноразового invalidation-флага.
7. Для редкого пересечения F12 с полным `Alt/Meta+Tab` после ACK финального
   отпускания modifier следующий press ждёт смены active target либо
   ограниченного fail-open через 300 мс.
8. Обычный physical fast path, пользовательские correction delays, click
   policy и Wayland не переведены на X11 barrier.

Основные файлы:

- `src/daemon/keyboard.rs` — подтверждаемая writer-транзакция и постоянное
  поколение X11 active target;
- `src/daemon/service.rs` — deferred ledger, cancellation/reconciliation и
  X11 focus marker;
- `debian/changelog` — Debian revision `0.1.0-3`.

## Локальная проверка

Перед сборкой пакета выполнена безопасная матрица, не открывающая реальные
host `/dev/input` и `/dev/uinput`.

| Проверка | Результат |
|---|---|
| targeted post-ACK regression | 1 passed |
| ACK финального modifier release | 1 passed |
| повторный Tab state-machine | 1 passed |
| Cinnamon repeated-Tab unit path | 1 passed |
| X11 barrier filter | 12 passed |
| основная library matrix | 711 passed, 0 failed |
| `settings-ui` library matrix | 772 passed, 0 failed |
| D-Bus integration | 11 passed, 0 failed |
| `cargo check --locked --all-targets` | pass |
| `cargo clippy --locked --all-targets --all-features` | exit 0 |
| targeted `rustfmt --check` для изменённого `service.rs` | pass |
| Debian package scripts | pass |
| `git diff --check` | pass |

Clippy оставил только прежние предупреждения. Полный `cargo fmt --all --check`
по-прежнему указывает на старое форматирование неизменённых `config.rs`,
`model.rs` и `tray_service.rs`; эти несвязанные файлы намеренно не
переформатировались.

Два независимых read-only review не нашли Critical или Important замечаний в
ACK/FIFO, modifier release, нескольких chord, временно пустом ledger, timeout,
writer/backend recovery и отсутствии busy-spin.

## Идентичность Debian package

- build command: `DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb`;
- package:
  `dist/packages/open-switcher_0.1.0-3_amd64.deb`;
- размер: `3 052 026` bytes;
- SHA-256 package:
  `9f18df63a32f551ecd790fd03796578ab7057d2cfba5417570877c57aa6b8b0c`;
- SHA-256 packaged daemon:
  `1adbdf1753740cafa9d7126c6fe333560e3541113ae4030298402f069badcb4e`.

Один и тот же exact file установлен в обе VM. В каждой гостевой системе SHA
переданного DEB совпал, `dpkg-query` показал `open-switcher 0.1.0-3`, SHA
`/usr/bin/open-switcher-daemon` совпал с packaged daemon, а
`/proc/$PID/exe` указывал на `/usr/bin/open-switcher-daemon` без `(deleted)`.

Lintian оставил два известных неблокирующих предупреждения:

- AppStream metadata не содержит `modalias` для udev-правила;
- для daemon/settings/tray отсутствуют man pages.

## Package-first проверка Mint/Cinnamon/X11

Проверка выполнена в сохранённом профиле `mint-install-v1`. Ввод отправлялся
только через виртуальную QEMU keyboard/tablet внутри гостя.

| Реалистичный сценарий | Результат |
|---|---|
| первое `ыгвщ` в новом Xed, затем F12 | `sudo` |
| движение pointer перед F12 | `sudo`, контекст сохранён |
| scroll перед F12 | `sudo`, контекст сохранён |
| настоящий left click перед F12 | старое слово не преобразовано |
| Enter после слова | контекст сброшен |
| Tab после слова | контекст сброшен |
| Space correction | pass |
| auto correction | `привет` |
| исправление двух заглавных | `Привет` |

Функциональная матрица дала `8/8`; начальный и конечный PID совпали.

Шесть одиночных пересечений F12 с одним `Alt+Tab` прошли: коррекция осталась в
исходном окне, а `1234` ровно один раз попало в целевое. Writer ledger во всех
проверенных успешных транзакциях имел
`accepted == acknowledged`, `reconciled == 0`.

### Принятая граница repeated-Tab stress

Отдельный искусственный тест отправлял два `Tab` под удерживаемым Alt примерно
через 2 мс после F12 и сразу начинал печатать. Cinnamon кратко публиковал
промежуточное active window, а затем возвращался в итоговое. В двух итерациях
потерялись первые один-два символа; daemon оставался active, PID не менялся,
modifier не залипал и устройство не оставалось захваченным ошибочно.

Последствие ограничено легко заметной потерей пары набираемых символов в
крайне редком тайминге. По решению владельца это принятый Low-риск: добавление
ещё одной Cinnamon-specific state machine несоразмерно практической пользе.
Cinnamon остаётся основным X11-профилем, но подобные миллисекундные сочетания
не блокируют выпуск, пока не приводят к частому отказу, залипшему вводу или
потере управления.

### Освобождение и повторный захват

Десять последовательных stop/start циклов проверяли `EVIOCGRAB` на QEMU
keyboard `/dev/input/event4`.

- stop latency:
  `82, 106, 113, 109, 109, 90, 88, 122, 103, 111 ms`;
- после каждого stop сторонний opener мог получить grab;
- после каждого start новый daemon снова владел grab;
- backend становился ready за `638–691 ms`;
- итог: `Result=success`, `NRestarts=0`, service active;
- в journal текущей загрузки нет warning/error.

Неактивный daemon после одного запуска VM оказался ожидаемым состоянием:
`~/.config/autostart/open-switcher.desktop` отсутствовал. Package намеренно
оставляет user units disabled и запускает их через opt-in XDG autostart.
`preset: enabled` отражал vendor policy, а не фактическое enablement.

## Package-first проверка Ubuntu/GNOME/Wayland

Использована активная GNOME-сессия:

```text
XDG_SESSION_TYPE=wayland
XDG_CURRENT_DESKTOP=ubuntu:GNOME
WAYLAND_DISPLAY=wayland-0
```

Wayland watcher явно записал
`input-target-watcher-disabled reason=non-x11-session`; записей
`focus-barrier` нет.

| Реалистичный сценарий | Результат |
|---|---|
| `ыгвщ`, затем F12 | `sudo` |
| F12, затем хвост через 150 мс | `sudotail` |
| PID во время функциональной проверки | не изменился |
| service после проверки | active |
| journal warning/error | отсутствуют |

Отдельный 2-мс synthetic stress не считается блокером. Daemon завершил
коррекцию и сохранил ledger
`accepted=4, acknowledged=4, reconciled=0`, но GNOME Text Editor ещё не
применил смену раскладки к приложению и отобразил хвост в старой раскладке.
Это искусственная граница desktop timing, а не захвата устройства; по той же
согласованной политике дальнейшая оптимизация не выполнялась.

Десять stop/start циклов на `/dev/input/event4` дали:

- stop latency:
  `72, 70, 90, 89, 79, 80, 69, 83, 98, 81 ms`;
- после каждого stop grab был свободен;
- после каждого start OpenSwitcher снова захватывал устройство;
- ready latency `639–665 ms`;
- итог: `Result=success`, `NRestarts=0`, service active.

## Safety boundary проверки

На host не выполнялись:

- открытие или захват физических `/dev/input` и `/dev/uinput`;
- реальные нажатия клавиш;
- изменение clipboard или раскладки;
- изменение host systemd, udev, ACL или пользовательской сессии.

Все runtime-нажатия, pointer events, `EVIOCGRAB`, service stop/start и временные
настройки debug выполнялись только внутри VM. Mint и Ubuntu запускались
последовательно и штатно остановлены. Лаборатория, диски, журналы и снимки
сохранены; ничего не удалялось.

Основные evidence:

- `runs/mint-install-v1/deferred-20260724-0.1.0-3-input.log`;
- `runs/mint-install-v1/deferred-20260724-0.1.0-3-focus-stress.txt`;
- `runs/ubuntu-cloud-provision-v1/deferred-20260724-0.1.0-3-wayland-input.log`;
- `runs/ubuntu-cloud-provision-v1/deferred-20260724-0.1.0-3-start.png`;
- `runs/ubuntu-cloud-provision-v1/deferred-20260724-0.1.0-3-tail.png`.

Пути выше находятся под
`/home/andrey/VMs/OpenSwitcherLab/` и не входят в Git.

## Ограничения и остаточные риски

1. Double-Tab через примерно 2 мс после F12 и Wayland tail через 2 мс имеют
   принятые функциональные ограничения. Они не влияют на оценку освобождения
   устройства.
2. Реальные USB hot-unplug/replug, suspend/resume и физический тачпад в этой
   кампании не воспроизводились.
3. `SIGKILL`, power loss и зависание ядра не дают userspace выполнить cleanup.
   Ядро освобождает grab при закрытии fd/завершении процесса, но полный
   аппаратный путь не моделировался.
4. Runtime fault injection зависшего конкретного writer TID остаётся
   ограничением предыдущей работы: stripped release-бинарник не позволяет
   надёжно выделить только этот поток.
5. Clipboard/selected-text transaction safety, package remove/ACL boundary и
   operation-wide synthetic-key ledger относятся к отдельным следующим
   фронтам аудита.

## Итоговая оценка

В пределах этого slice сохранение физического deferred-ввода можно считать
подтверждённым: ledger не удаляет событие до writer ACK, неоднозначные ошибки
переходят в terminal reconciliation, а press/release остаются FIFO.

Штатный lifecycle устройства подтверждён в двух установленных package-first
средах: двадцать stop/start циклов каждый раз освобождали grab менее чем за
122 мс и успешно захватывали устройство снова. Новых зависаний, дедлоков,
залипших modifier, panic или warning/error не наблюдалось.

Это не абсолютное доказательство fail-safe при отказе ядра, power loss или
неинъецированном зависании writer. Корректная формулировка:
«штатное освобождение и userspace fail-stop архитектура подтверждены; тяжёлые
внешние отказы остаются отдельной runtime-кампанией».
