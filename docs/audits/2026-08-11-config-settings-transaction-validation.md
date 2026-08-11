# Валидация транзакций конфигурации и настроек M-04/M-05

- Дата: 2026-08-11
- Статус: M-04 и M-05 закрыты в согласованной границе
- База `master`: `83e871009f8210b4b8bedfcc61ec391e640c41c4`
- Проверенный code candidate: `77e2c3af9fe548c89b9dfa15a69e550fad53cd27`
- Спецификация:
  `docs/superpowers/specs/2026-08-11-config-settings-transaction-safety-design.md`
- План:
  `docs/superpowers/plans/2026-08-11-config-settings-transaction-safety.md`

## Итог

M-04 устранён: `config.toml` больше не перезаписывается непосредственно.
Новая версия полностью записывается и синхронизируется во временный файл в
том же каталоге, публикуется через atomic `rename`, после чего синхронизируется
родительский каталог. Ошибка до `rename` не меняет прежний файл или состояние
daemon; ошибка синхронизации каталога после `rename` правильно классифицируется
как уже выполненный commit с неопределённой устойчивостью к потере питания.

M-05 устранён без глобальных revision/CAS: окно настроек передаёт типизированную
маску только реально изменённых полей. Daemon под единым
`settings_update_gate` накладывает её на последний committed config, проверяет
получившийся полный набор настроек, сохраняет его и только затем публикует в
runtime. Поэтому устаревшее окно больше не затирает независимое изменение из
tray или другого клиента. Для одного и того же поля действует понятное правило
last-write-wins.

Один и тот же установленный Debian-пакет прошёл package-first smoke в двух
сохранённых VM: Linux Mint 22.2/Cinnamon/X11 и Ubuntu 24.04/GNOME/Wayland.
Сбоев daemon/tray и потери несвязанных настроек не обнаружено.

## Идентичность пакета

- Файл: `open-switcher_0.1.0-8_amd64.deb`
- Размер: 3 336 870 bytes
- Архитектура: `amd64`
- SHA-256:
  `6ac0f056ea588385a0a88dcf10c31f9e2fd64178bd82c116828a0e11134b2f59`
- Путь:
  `/home/andrey/Projects/OpenSwitcher/.worktrees/config-settings-safety/dist/packages/open-switcher_0.1.0-8_amd64.deb`

Каноническая сборка `./manage.sh package deb` завершилась успешно. Встроенные
package gates прошли последовательно. `lintian` оставил только ранее известные
предупреждения: отсутствие AppStream modalias, вызовы `systemctl` из `preinst`
и отсутствие man pages; к M-04/M-05 они не относятся.

## Автоматические gates

Все команды выполнялись последовательно, чтобы исключить взаимное влияние
тестов через process environment и общие package artifacts.

| Gate | Результат |
|---|---|
| Focused config tests | 41 passed, 0 failed |
| Focused settings patch tests | 4 passed, 0 failed |
| D-Bus integration | 15 passed, 0 failed |
| Settings UI | 59 passed, 0 failed |
| Полный `--all-targets --features settings-ui` | library: 1040 passed, 1 ignored; main: 4 passed; D-Bus: 15 passed; examples: 5 passed |
| Package internal gate без UI | library: 975 passed; main: 4 passed; D-Bus: 15 passed |
| Package internal gate с UI | 1040 passed, 1 ignored |
| `debian_package_scripts_test.sh` | `ok` |
| `input_access_package_test.sh` | `ok` |
| `manage_package_deb_test.sh` | `ok` |
| `cargo fmt --check`, `git diff --check` | passed |

Во время канонической сборки был обнаружен отдельный дефект самого package
gate: две Cargo-матрицы могли запускаться параллельно и конфликтовать через
глобальное окружение тестового процесса. В `debian/rules` они переведены на
последовательный `--test-threads=1`; shell regression сначала подтвердил старое
поведение как RED, затем прошёл после исправления.

## Доказательства M-04

Проверены следующие границы atomic replace:

- читатель видит старые либо полностью новые bytes, но не частичный TOML;
- временный файл создаётся рядом с конечным, имеет mode `0600`, получает
  `write_all` и `fsync` до commit point;
- успешный `rename` заменяет конечный symlink, не изменяя его target;
- ошибка `rename` сохраняет прежний объект и удаляет временный файл;
- ошибка синхронизации родительского каталога возвращается как
  `CommittedDurabilityUncertain`, а не как ложный rollback;
- невалидная конфигурация не меняет прежние bytes;
- ошибка сохранения не публикует новый in-memory/runtime snapshot;
- no-op patch не записывает файл и не увеличивает runtime generation.

Runtime подтвердил права файла: в Mint mode `0600` сохранился, а существовавший
в Ubuntu старый файл с mode `0664` после первого нового commit стал `0600`.
После записей в каталоге оставался только `config.toml`, без потерянных temp
files; TOML читался daemon и отражал возвращённый committed snapshot.

## Доказательства M-05

Unit и integration tests проверили:

- два клиента с одним исходным snapshot меняют разные поля, и оба изменения
  сохраняются;
- при двух записях одного поля побеждает последняя;
- неизвестные биты mask отклоняются;
- значения полей вне mask не копируются и не валидируются как изменения;
- конфликт hotkey после overlay отклоняется без изменения TOML, D-Bus state и
  runtime generation;
- старый full-settings D-Bus signature отклоняется без записи;
- UI строит mask только изменённых полей, а autostart-only save передаёт пустой
  settings patch;
- UI принимает именно committed snapshot из ответа daemon и fail-closed
  обрабатывает невалидный ответ.

В каждой VM выполнена одинаковая package-first семантическая матрица через
публичный API установленных `/usr/bin/open-switcher-daemon` и
`/usr/bin/open-switcher-tray`:

1. зафиксирован stale snapshot с включёнными
   `auto_switch_enabled` и `fix_two_capitals`;
2. `Toggle` выключил только `auto_switch_enabled`;
3. узкий `UpdateSettings` на базе stale snapshot изменил только
   `fix_two_capitals`;
4. `GetSettings` и TOML сохранили оба результата: `false + false`;
5. следующий patch только `auto_switch_enabled=true` победил последним, не
   вернув старое значение `fix_two_capitals`;
6. вызов старого full-settings signature завершился `Signature mismatch`, а
   SHA-256 `config.toml` не изменился.

В обоих профилях daemon и tray оставались `active/running`, `NRestarts=0`, их
`/proc/<pid>/exe` указывал на `/usr/bin`. Установленный
`/usr/bin/open-switcher-settings` запускался и штатно закрывался без изменения
конфига. В Ubuntu дополнительно визуально подтверждено открытое окно и итоговые
значения; evidence сохранён в:

`/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/m04-m05-settings-ui.ppm`.

Предупреждения EGL/Zink в Ubuntu относятся к виртуальному графическому стеку
QEMU: окно при них отрисовалось. В journal daemon/tray предупреждений не было.

## Граница безопасности проверки

- Физические `/dev/input` и `/dev/uinput` хоста в VM не передавались.
- Clipboard, layout, systemd, udev, ACL и пользовательская сессия хоста не
  менялись.
- В гостях устанавливался только exact DEB с указанным SHA-256; binaries из
  `target/` для package acceptance не использовались.
- Mint и Ubuntu запускались строго последовательно и штатно остановлены.
- Диски, ключи, overlays и все прежние evidence сохранены; лаборатория не
  удалялась.

## Ограничения и остаточные риски

- Реальное отключение питания точно между `rename` и directory `fsync` не
  выполнялось. Различие pre-commit/post-commit проверено инъекцией ошибки и
  структурой syscall-последовательности.
- Внешнее ручное редактирование TOML одновременно с работающим daemon не
  является поддерживаемым конкурентным клиентом и в этой кампании не
  проверялось: daemon сериализует собственные tray/D-Bus операции, но не
  наблюдает произвольные внешние записи.
- Конфликтная VM-матрица выполнялась детерминированно через те же публичные
  D-Bus методы, которыми пользуются установленные tray/settings. Pixel-click
  tray не автоматизировался; построение узкой mask в UI отдельно покрыто 59
  тестами, а установленное окно прошло process/visual smoke.
- Обычный F12 не повторялся в этом узком slice: input-код не менялся, а exact
  DEB `0.1.0-7` уже проходил двухпрофильную input/clipboard проверку. Повтор
  остаётся частью отложенной объединённой финальной кампании.
- Atomic rename защищает целостность одного config commit, но не обещает
  пережить отказ файловой системы, ядра или накопителя.

Эти ограничения не оставляют исходные первопричины M-04/M-05 в коде и не
мешают считать оба замечания закрытыми. Общая финальная runtime-кампания после
последнего audit slice по-прежнему не выполнена.
