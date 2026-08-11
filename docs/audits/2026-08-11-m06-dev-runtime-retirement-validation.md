# Валидация удаления прямого dev-runtime M-06

- Дата: 2026-08-11
- Статус: M-06 закрыт
- База `master`: `d9b785441f92a5bc78e65f63be0e254723019079`
- Проверенный code candidate: `df3ef27e9e17df3563c3316cc1e3f4947f5f5b6e`
- Спецификация:
  `docs/superpowers/specs/2026-08-11-m06-dev-runtime-retirement-design.md`
- План:
  `docs/superpowers/plans/2026-08-11-m06-dev-runtime-retirement.md`

## Итог

M-06 устранён удалением самой опасной поверхности, а не усложнением проверки
PID. `manage.sh` больше не создаёт PID-файлы, не ищет процессы, не запускает
бинарники через `nohup` и не посылает им сигналы. Поэтому stale PID больше не
может привести к остановке чужого процесса.

Удалён и связанный внутренний режим `OPEN_SWITCHER_RUNTIME_MODE=dev`, который
отключал tray watchdog и managed recovery. Daemon и tray теперь имеют одну
runtime-семантику. Установленный Debian-пакет и раньше работал в managed-режиме,
поэтому его штатное поведение не изменено.

Для исходного дерева сохранены только безопасные и явные операции:

- `./manage.sh build` собирает бинарники, но не запускает их;
- `./manage.sh package deb` собирает канонический пакет;
- `./manage.sh doctor` выполняет диагностику;
- `./manage.sh systemd ...` явно управляет процессами через `systemd --user`.

Старые команды `dev`, `start`, `stop`, `restart`, `status`, `logs` и
`settings` завершаются ненулевым кодом, печатают инструкцию миграции и не
перенаправляются неявно на systemd.

## Идентичность пакета

- Файл: `open-switcher_0.1.0-8_amd64.deb`
- Размер: 3 385 992 bytes
- Архитектура: `amd64`
- SHA-256:
  `d0f2887e8ffffabfd878b39ee6e2564aa62a7bbc00e62ba0da6283e49bd1b5b5`
- Путь:
  `/home/andrey/Projects/OpenSwitcher/.worktrees/m06-dev-runtime-retirement/dist/packages/open-switcher_0.1.0-8_amd64.deb`

Каноническая сборка `./manage.sh package deb` завершилась успешно. Пакет
содержит три ожидаемых бинарника, user units, XDG autostart/desktop entry,
udev-правило, иконку и служебные скрипты. `lintian` оставил только ранее
известные предупреждения: отсутствие AppStream modalias, вызовы `systemctl` из
`preinst` и отсутствие man pages; к M-06 они не относятся.

## Автоматические gates

| Gate | Результат |
|---|---|
| TDD RED на прежней реализации | безопасно обнаружены PID-state, lifecycle functions, прямые launch/signal и runtime bypass; команды не запускались |
| `tests/manage_dev_retirement_test.sh` | `ok` |
| Focused `system::tests` | 17 passed, 0 failed |
| Focused tray recovery tests | 2 passed, 0 failed |
| Полный `cargo test --all-targets --all-features -- --test-threads=1` | library: 1038 passed, 1 ignored; main: 4 passed; D-Bus: 15 passed; examples: 5 passed |
| `tests/manage_package_deb_test.sh` | `ok` |
| `tests/linux_input_setup_test.sh` | `ok` |
| `tests/wayland_diagnostics_test.sh` | `ok` |
| Канонический `./manage.sh package deb` со встроенными gates | passed, artifact создан |
| `bash -n`, `cargo fmt --check`, `git diff --check` | passed |

Regression test сначала выполняет статический барьер и только после него
вызывает старые lifecycle-команды в изолированной fixture с заглушками. Он
проверяет одновременно, что:

- production-код не содержит `.run`/PID-state, прямого `nohup`/`kill`, старых
  lifecycle-функций и dev-mode bypass;
- устаревшие команды fail closed;
- они не вызывают `cargo`, `systemctl`, `nohup` или process scan;
- они не создают `.run`;
- help показывает только поддерживаемый интерфейс.

## Результат inline-review

Повторный просмотр полного diff относительно базы не выявил побочных изменений
input pipeline, clipboard, layout correction, конфигурации, D-Bus API, Debian
maintainer scripts, udev/ACL или systemd units. Rust-изменения ограничены
удалением специального dev-исключения; shell-изменения удаляют самостоятельное
владение процессами, сохраняя build/package/doctor и явный systemd namespace.

Отдельный поиск по production-коду не нашёл `OPEN_SWITCHER_RUNTIME_MODE`,
`RuntimeMode::Dev`, `is_dev_runtime_mode`, PID lifecycle helpers, `nohup` или
прямого process-signalling из `manage.sh`. Оставшийся `timeout --signal=KILL`
относится к ограничению времени ожидания дочернего shell при остановке systemd
guardian и не посылает сигнал PID из файла.

## Особенности среды проверки

В ограниченной sandbox два socket-based теста среды сначала получили
`Operation not permitted`: `system::tests`, создающие Unix socket, и
`wayland_diagnostics_test.sh` с временным socket fixture. Они прошли полностью
при повторе вне sandbox.

Попытка package build внутри той же sandbox остановилась на тесте пробуждения
idle waiter из-за ограничений polling/socket. Тот же тест прошёл в полном
наборе, а повторная каноническая package build вне sandbox завершилась с кодом
0. Это классифицировано как ограничение test runner, а не дефект OpenSwitcher.

## Ограничения и остаточные риски

- DEB не устанавливался в VM и на хост в рамках этой узкой задачи. M-06 касается
  удалённого source-tree process manager; состав пакета и полный package gate
  проверены, а установленный managed-путь кодом не менялся.
- Физические `/dev/input` и `/dev/uinput`, clipboard, раскладка, udev, ACL,
  systemd пользовательской сессии и текущий установленный OpenSwitcher не
  изменялись.
- Существующий каталог `.run` от очень старой dev-установки намеренно не
  удаляется автоматически. Он инертен: новый `manage.sh` его не читает и не
  использует. Автоочистка создала бы ненужную файловую мутацию.
- Прямой ручной запуск бинарника из `target/` технически возможен вне
  `manage.sh`, но не является поддерживаемым workflow и не возвращает удалённую
  PID/signal поверхность скрипта.
- Debug-переменные `OPEN_SWITCHER_*_DEBUG` сохранены как явный opt-in для
  контролируемой диагностики; автоматически dev-командой они больше не
  включаются.

Эти ограничения не оставляют исходную первопричину M-06 в поддерживаемом коде.
Пункт можно считать закрытым без отдельной опасной runtime-кампании с
физическими устройствами ввода.
