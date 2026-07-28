# H-06: результат этапа 1 — DEB и обычный Mint smoke

Дата: 2026-07-29

## Проверенный кандидат

- Ветка: `fix/h06-synthetic-input-ledger`
- Commit: `881c8f2`
- DEB:
  `dist/packages/open-switcher_0.1.0-3_amd64.deb`
- SHA-256:
  `a827727b805e2c22690ced0c4451fc7989a42a1e883e0521edf348252c1f60d9`
- Metadata: `open-switcher`, `0.1.0-3`, `amd64`

Пакет собран один раз. Внутренние release-тесты сборки прошли: основной
набор — 856 passed, 1 ignored; полный набор с дополнительными targets —
917 passed, 1 ignored.

Статическая проверка DEB прошла:

- обязательные daemon/tray/guardian units присутствуют;
- daemon, tray и settings исполняемые;
- guardian доступен только через скрытый режим daemon;
- maintainer scripts проходят `sh -n` и содержат ожидаемые lifecycle hooks;
- stop helper исполняемый и совпадает с исходником;
- скрытый guardian без systemd activation metadata завершается до обращения
  к X11;
- извлечённые user units проходят `systemd-analyze --user verify`.

## Установка в Mint

В сохранённую Mint 22.2 / Cinnamon / X11 VM передан и установлен exact DEB.
SHA-256 файла в guest совпал. Хэши трёх установленных binary совпали с
извлечённым пакетом:

- daemon:
  `6cf497a0fb400f24e21bb34a6ee1b8803ff393dcd4b14c1e0d66d0800a9116ba`
- tray:
  `9e13027c1a142073b59ff991240968b2c9819783b4e8ac6f2268a1db7ca26aad`
- settings:
  `272fba331ea272a33a19bd99b43f5044815b1d323ec56ab0b5f1aa0fb2d0d035`

Внутри VM восстановлено предусмотренное дизайном лаборатории правило
`openswitcher ALL=(ALL) NOPASSWD:ALL`; на host это не влияет.

## Подтверждённый blocker

Серьёзность: **High** (обычная работа X11-кандидата невозможна).

Точное место:

- `dist/systemd/open-switcher-xtest-guardian.service`:
  `PrivateDevices=yes`;
- `src/daemon/xtest_guardian/seqpacket.rs`:
  `authenticate_peer()` → `validate_sender_credentials()` →
  `fs::metadata("/proc/<daemon-pid>/exe")`.

Сценарий:

1. установить exact DEB в Mint/Cinnamon/X11;
2. запустить `/usr/lib/open-switcher/open-switcher-launch --manual`;
3. daemon подключается к socket-activated guardian;
4. guardian проверяет executable daemon через `/proc/<pid>/exe`.

Наблюдаемое последствие:

- guardian завершается с
  `Io(Os { code: 13, kind: PermissionDenied, message: "Permission denied" })`;
- daemon получает `Connection reset by peer`, затем
  `VirtualKeyboardWriterDisconnected`;
- systemd перезапускает daemon пять раз, после чего unit остаётся failed.

Первопричина подтверждена отдельной безопасной пробой в той же user session:

- обычный `stat /proc/<peer-pid>/exe` проходит;
- только `NoNewPrivileges=yes` проходит;
- только `RestrictAddressFamilies=AF_UNIX` проходит;
- только `PrivateDevices=yes` стабильно возвращает `Permission denied`.

Следовательно, mount namespace от `PrivateDevices` несовместим с текущей
проверкой identity peer через procfs.

Статические package checks и unit-тесты не предотвращают дефект, потому что
не создают реальный systemd namespace и не проверяют доступ guardian к
`/proc` другого процесса.

Направление исправления: сохранить проверку same-UID/same-executable и
hardening unit, но убрать их несовместимость. После выбора решения нужен
узкий systemd runtime regression test, затем повтор этого же обычного Mint
smoke. Код в текущем этапе не исправлялся.

Уверенность: высокая. Дополнительно следует проверить выбранное исправление
в Ubuntu и в двух запланированных crash/reconciliation сценариях, но это не
требуется для подтверждения текущего дефекта.

## Невыполненные smoke-сценарии

Из-за blocker не проверялись:

- обычный ввод;
- F12-коррекция последнего слова;
- переключение раскладки;
- исправление двух заглавных;
- исправление случайного Caps Lock.

Это осознанная остановка по границе этапа, а не успешный результат smoke.

## Точка продолжения

1. Отдельным коротким этапом исправить несовместимость guardian hardening с
   peer authentication.
2. Пересобрать DEB и повторить пять обычных Mint smoke-сценариев.
3. После успешного smoke отдельно выполнить два центральных H-06
   fault-injection сценария: авария daemon и авария guardian с проверкой
   reconciliation.
4. Ubuntu, stress, performance и повторные lifecycle-прогоны оставить для
   общей финальной приёмочной кампании.

Низкоприоритетный tooling-долг: `dh_clean` удаляет tracked
`vendor/uinput-0.1.3/Cargo.toml.orig`; после сборки файл восстановлен
byte-for-byte, рабочее дерево не оставлено повреждённым.
