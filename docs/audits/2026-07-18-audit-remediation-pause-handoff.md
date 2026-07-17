# OpenSwitcher audit remediation: pause handoff

- Дата остановки: 2026-07-18
- Причина паузы: решение владельца остановить работы после двух суток аудита и
  remediation из-за расхода времени и token budget
- Рабочая ветка: `fix/audit-remediation`
- Последний проверенный implementation commit: `2523d67`
- Последний validation commit перед handoff: `444d25a`
- Политика продолжения: ничего из backlog не начинать без новой прямой просьбы
  владельца
- VM-лабораторию не удалять без отдельной прямой просьбы

## Где остановились

Завершён input runtime snapshot slice. Grab-critical input loop больше не ждёт
configuration persistence, desktop commands или layout backend refresh;
неподтверждённое/устаревшее layout state приводит к пропуску коррекции при
сохранении исходного физического ввода.

Во время package-first VM validation дополнительно исправлены:

- неработавшие Cinnamon D-Bus observation/switch calls — заменены на XKB/XTest;
- ложное продление freshness после ошибки обязательного GNOME/Cinnamon
  observation.

Полный checkpoint и evidence:

- `docs/audits/2026-07-17-h01-input-runtime-snapshot-validation.md`;
- `docs/superpowers/specs/2026-07-17-input-runtime-snapshot-design.md`;
- `docs/superpowers/plans/2026-07-17-input-runtime-snapshot.md` — 45/45 steps
  отмечены выполненными.

Предыдущие завершённые checkpoints:

- `docs/audits/2026-07-17-c01-h05-remediation-validation.md`;
- `docs/audits/2026-07-17-h01-nonblocking-logging-validation.md`.

## Последний проверенный Debian package

- package identity: `open-switcher 0.1.0-1`, `amd64`;
- canonical source artifact:
  `.worktrees/audit-remediation/dist/packages/open-switcher_0.1.0-1_amd64.deb`;
- удобная копия для host install:
  `dist/packages/open-switcher_0.1.0-1_amd64.deb`;
- размер: `3 091 644` bytes;
- SHA-256:
  `1ceeaa5e9bddaaf4308080f26bb80e05516962bee8537f8e23f870e18c2d742c`;
- packaged daemon SHA-256:
  `01fe4439f37f384d6bc0ae59f8358caccdb041bbf77caefa69c153e88892fd8a`.

После сборки менялась только документация; diff от `2523d67` до handoff не
содержит изменений `src`, Cargo, packaging, scripts или `manage.sh`. Поэтому
повторная сборка того же binary payload не требуется.

Package прошёл 561 base tests, 622 settings-ui tests, 11 D-Bus integration
tests и package shell checks. Тот же daemon hash был установлен и проверен в
Ubuntu 24.04/GNOME/Wayland и Linux Mint 22.2/Cinnamon/X11.

## Host transition с developer install

Состояние перед очисткой 2026-07-18:

- dpkg package `open-switcher` не установлен;
- developer daemon/tray/settings процессы отсутствуют;
- user units inactive, dead и disabled;
- units указывают на `~/.local/bin/open-switcher-*`;
- XDG autostart fallback отсутствует;
- developer assets остались в `~/.local/bin`, `~/.config/systemd/user`,
  `~/.local/share/applications` и `~/.local/share/icons`;
- `/etc/udev/rules.d/80-openswitcher-input.rules` не принадлежит dpkg и
  совпадает с текущим rule payload;
- runtime named ACL `user:andrey:rw-` присутствует на `/dev/uinput` и input
  devices `event4`, `event8`, `event9`;
- пользовательские настройки находятся в `~/.config/open-switcher` и должны
  быть сохранены.

Выполненная user-level очистка:

- daemon/tray дополнительно остановлены и units отключены;
- удалены developer units из `~/.config/systemd/user`;
- удалены developer binaries из `~/.local/bin`;
- удалены developer desktop entry и icon из `~/.local/share`;
- user systemd перечитал units и больше не находит OpenSwitcher services;
- процессы OpenSwitcher отсутствуют;
- `~/.config/open-switcher` намеренно сохранён;
- dpkg package по-прежнему не установлен: владелец установит подготовленный
  `.deb` отдельно.

Privileged cleanup выполнен владельцем в отдельном terminal и затем проверен
agent read-only командами:

- непакетный `/etc/udev/rules.d/80-openswitcher-input.rules` отсутствует;
- named ACL `user:andrey` отсутствует на `/dev/uinput` и всех текущих
  `/dev/input/event*`;
- udev rules перечитаны;
- persistent developer install и его runtime ACL полностью удалены.

Существующие устройства до следующего udev event ещё могут показывать runtime
tag `uaccess` в udev database, но named ACL уже отсутствует. Установка нового
Debian package штатно выполнит trigger, установит package-owned rule и применит
актуальный runtime ACL. Agent не получал и не сохранял пароль пользователя.

## Открытый backlog

Работы сознательно отложены:

1. cancellation/deadline и recovery зависшего layout child без restart daemon;
2. снижение стоимости 300 ms Cinnamon polling или event-driven observation;
3. writer shutdown ACK и запрет late synthetic mutation после stop;
4. deferred queue conservation/reconciliation oracle;
5. failure-at-every-step synthetic correction replay;
6. ACL/multi-seat boundary;
7. clipboard/selected-text failure paths;
8. active Debian package upgrade/remove lifecycle;
9. расширенная runtime campaign: hot-unplug/replug, frozen display/backend,
   kill/power-loss timing и hardware acceptance.

## Как продолжать позже

1. Открыть этот handoff и последний H-01 validation report.
2. Проверить `git status` и HEAD ветки `fix/audit-remediation`.
3. Не пересобирать VM-лабораторию; использовать сохранённые Ubuntu и Mint
   profiles по необходимости.
4. Выбрать один связный safety slice, написать отдельную spec/plan и только
   затем менять код.
5. Reasoning Max использовать по умолчанию; перед genuinely high-risk redesign
   input lifecycle отдельно предложить владельцу Ultra.
6. После следующего связного slice собирать новый canonical `.deb` и запускать
   только релевантную regression matrix.
