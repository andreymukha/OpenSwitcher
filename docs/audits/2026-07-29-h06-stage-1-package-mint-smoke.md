# H-06: результат этапа 1 — DEB и обычный Mint smoke

Дата: 2026-07-29

## Проверенный кандидат

- Ветка: `fix/h06-synthetic-input-ledger`
- Commit с исправлением: `41042f3`
- DEB:
  `dist/packages/open-switcher_0.1.0-3_amd64.deb`
- SHA-256:
  `3a27e893aa56f4284c4767c282b7c1741c4b7506222bf5550543fe8b646ec405`
- Metadata: `open-switcher`, `0.1.0-3`, `amd64`

Финальный пакет собран после узкого TDD-исправления. Внутренние release-тесты
сборки прошли: основной набор — 856 passed, 1 ignored; полный набор с
дополнительными targets — 917 passed, 1 ignored.

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

## Подтверждённый и устранённый blocker

Исходная серьёзность: **High** (обычная работа X11-кандидата была
невозможна). Статус: **устранён в `41042f3`**.

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

Исправление сохраняет same-UID/same-executable authentication,
`NoNewPrivileges=yes`, `RestrictAddressFamilies=AF_UNIX` и `UMask=0077`.
Удалён только `PrivateDevices=yes`, который создавал несовместимый namespace.
Guardian остаётся непривилегированным процессом того же пользователя и того
же binary, что и daemon.

TDD-подтверждение:

1. package regression-тест изменён так, чтобы запрещать `PrivateDevices=yes`
   одновременно в Debian- и dist-unit и явно удерживать остальные hardening
   параметры;
2. до правки тест упал на существующем Debian unit;
3. после удаления двух дублирующихся строк тест прошёл;
4. `manage_package_deb_test.sh` и `systemd-analyze --user verify` прошли;
5. в реальной Mint VM guardian успешно аутентифицировал daemon и остался
   active.

Уверенность: высокая. Дополнительно следует проверить выбранное исправление
в Ubuntu и в двух запланированных crash/reconciliation сценариях, но это не
требуется для подтверждения текущего дефекта.

## Результат обычного Mint smoke

На финальном exact DEB проверены физическими QEMU key events:

- обычный ввод: `hello`;
- F12-коррекция: физические `sudo` в RU дали `ыгвщ`, после F12 получено
  `sudo`;
- переключение EN → RU → EN: физические `test` в RU дали `еуые`;
- две заглавные: `TEst` исправлено в `Test`;
- случайный Caps Lock: `tEST` исправлено в `Test`.

После всех сценариев daemon, tray, guardian socket и guardian service
остались active; failed user units и warning/error в журнале отсутствуют.
Снимок сохранён внутри лаборатории:
`runs/mint-install-v1/h06-stage1-fixed-smoke.png`.

## Точка продолжения

1. Отдельно выполнить два центральных H-06
   fault-injection сценария: авария daemon и авария guardian с проверкой
   reconciliation.
2. Ubuntu, stress, performance и повторные lifecycle-прогоны оставить для
   общей финальной приёмочной кампании.

Низкоприоритетный tooling-долг: `dh_clean` удаляет tracked
`vendor/uinput-0.1.3/Cargo.toml.orig`; после сборки файл восстановлен
byte-for-byte, рабочее дерево не оставлено повреждённым.
