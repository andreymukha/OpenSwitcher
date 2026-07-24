# OpenSwitcher audit remediation: pause handoff

- Исходная дата остановки: 2026-07-18
- Последнее обновление: 2026-07-24
- Текущая ветка: `master`
- Последний implementation/package commit: `a73bad8`
- Последний validation commit: `bf19ae6`
- Политика продолжения: ничего из backlog не начинать без новой прямой просьбы
  владельца
- VM-лабораторию не удалять без отдельной прямой просьбы

## Актуальная точка 2026-07-24

Завершён deferred input conservation slice. Теперь события, уже принятые
daemon после физического grab, учитываются до подтверждения writer после
`write + synchronize`; неподтверждённый остаток согласуется при пересборке
backend. Для X11 добавлен generation barrier, не позволяющий допечатать
отложенный текст в окно, сменившееся во время коррекции.

Итоговый validation report:

- `docs/audits/2026-07-24-deferred-input-conservation-validation.md`.

Локальная матрица на итоговом commit:

- 711 base library tests;
- 772 settings-ui library tests;
- 11 D-Bus integration tests;
- all-target check, all-feature clippy, package shell tests, rustfmt и
  `git diff --check`.

Exact package `0.1.0-3` проверен в Linux Mint/Cinnamon/X11 и
Ubuntu/GNOME/Wayland. В обоих профилях выполнено по 10 stop/start циклов:
grab каждый раз освобождался после stop и возвращался после start, daemon
завершал цикл с `Result=success` без restart и предупреждений.

Приняты как Low и не требуют дальнейшего углубления два искусственных
миллисекундных сценария: повторный double-Tab примерно через 2 ms в Cinnamon
может потерять 1–2 символа, а Wayland tail примерно через 2 ms может остаться
в исходной раскладке. Ни один сценарий не оставляет grab или модификатор,
не останавливает daemon и не блокирует управление. Реалистичные paced-сценарии
в обоих профилях проходят.

Сохранённые состояния лаборатории:

- оба VM-профиля установлены с `0.1.0-3` и остановлены после проверки;
- disks, profiles, logs, screenshots, QMP evidence и worktrees не удалялись.

## Актуальная точка 2026-07-23

После исходной паузы завершены ещё четыре связанных input-safety slice:

1. Сброс контекста по настоящему клику с сохранением контекста при движении и
   scroll; raw touch codes не считаются физической кнопкой.
2. Обязательные input watcher переведены в fail-safe lifecycle; потеря worker
   снимает grab и пересобирает backend.
3. Исправлено владение `/dev/uinput` fd в vendored `uinput 0.1.3`.
4. Virtual writer получил подтверждённый bounded shutdown. Новый backend нельзя
   открыть рядом с неподтверждённо живым writer; при timeout процесс переходит
   в необратимый typed fail-stop.

Последний slice уже fast-forward слит в `master`. Validation report:

- `docs/audits/2026-07-23-quiescent-writer-shutdown-validation.md`.

Штатный путь проверен exact DEB в Mint/X11 и Ubuntu/Wayland: 20 paced
stop/start циклов, функциональный smoke и по одному `/dev/uinput` fd. Полная
локальная матрица: 634 base tests и 695 settings-ui tests.

Главное ограничение последнего slice: production writer-specific hang
injection не выполнялся. У stripped release-бинарника нельзя доказанно
выделить только безымянный writer TID, а process-wide `SIGSTOP` не соответствует
согласованному safety gate. Fail-stop подтверждён детерминированными
fake-thread tests и review, но не полным runtime hang experiment.

Сохранённые состояния лаборатории:

- Mint profile установлен с `0.1.0-2` и остановлен после проверки;
- Ubuntu profile установлен с `0.1.0-2` и остановлен после проверки;
- disks, profiles, screenshots, QMP evidence и worktrees не удалялись.

## Предыдущая точка 2026-07-18

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

- package identity: `open-switcher 0.1.0-3`, `amd64`;
- canonical source artifact:
  `.worktrees/deferred-input-conservation/dist/packages/open-switcher_0.1.0-3_amd64.deb`;
- удобная копия для host install:
  `dist/packages/open-switcher_0.1.0-3_amd64.deb`;
- размер: `3 052 026` bytes;
- SHA-256:
  `9f18df63a32f551ecd790fd03796578ab7057d2cfba5417570877c57aa6b8b0c`;
- packaged daemon SHA-256:
  `1adbdf1753740cafa9d7126c6fe333560e3541113ae4030298402f069badcb4e`.

Package прошёл 711 base tests, 772 settings-ui tests, 11 D-Bus integration
tests, all-target check, all-feature clippy и package shell checks. Тот же
daemon hash установлен и проверен в Ubuntu 24.04/GNOME/Wayland и Linux
Mint/Cinnamon/X11.

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

1. operation-wide synthetic key ledger и failure-at-operation-N;
2. transactional clipboard/selected-text safety;
3. package upgrade/remove и seat/ACL boundary;
4. отдельный диагностический writer fault seam и production hang injection;
5. расширенная runtime campaign: hot-unplug/replug, suspend/resume,
   kill/power-loss timing и hardware acceptance.

## Как продолжать позже

1. Открыть этот handoff и
   `docs/audits/2026-07-24-deferred-input-conservation-validation.md`.
2. Проверить `git status` и HEAD `master`; пользовательские untracked audit/VM
   документы в основном worktree не удалять и не включать в случайный commit.
3. Не пересобирать VM-лабораторию; использовать сохранённые Ubuntu и Mint
   profiles по необходимости.
4. Выбрать один связный safety slice, написать отдельную spec/plan и только
   затем менять код.
5. Reasoning Max использовать по умолчанию; перед genuinely high-risk redesign
   input lifecycle отдельно предложить владельцу Ultra.
6. После следующего связного slice собирать новый canonical `.deb` и запускать
   только релевантную regression matrix.
