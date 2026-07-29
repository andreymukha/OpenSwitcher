# H-06: итоговая передача на интеграцию

**Дата:** 2026-07-29

**Статус:** реализация, package-first runtime-проверка и выбранная оптимизация
завершены; H-06 интегрирована в `master` и отправлена в `origin`.

## Итоговое решение

H-06 закрывает исходный архитектурный риск синтетического ввода:

- временные synthetic key-down учитываются общим operation ledger;
- состояние удерживаемых модификаторов учитывается session ledger;
- uinput и Cinnamon/XTEST используют один fail-safe контракт;
- XTEST выполняется отдельным socket-activated guardian;
- daemon не захватывает физическую клавиатуру до готовности guardian;
- при одиночной гибели daemon или guardian новые mutations запрещаются,
  `EVIOCGRAB` освобождается, а известный synthetic debt очищается либо
  завершается явным `Unreconciled`;
- normal XTEST trace и пользовательская логика коррекции не изменены.

Дополнительный абсолютный порог `guardian p95 <= 1 ms` признан некорректным
для метрики, включающей server-side XTEST `.check()` и X11 round-trip.
Безопасностные bounds сохранены, а пользовательская производительность
оценена дифференциальным end-to-end сравнением.

Из двух проверенных вариантов оптимизации выбран только `barrier-only`:

- production-коммит:
  `9276bc05814b48113dc2285bae6199e454f0e501`;
- median полной F12-коррекции улучшилась с `131 ms` до `126 ms`;
- p95 улучшился с `139 ms` до `132 ms`;
- maximum улучшился с `147 ms` до `136 ms`;
- во всех 180 финальных коррекциях exact trace совпал;
- timeout, protocol failure и `Unreconciled` не наблюдались.

Operation-scoped mapping cache из
`7b1475cd707804830a64e24cbdb2fa8a6efc3221` в интеграцию не входит: его
дополнительный выигрыш не отделяется от шума, а жизненный цикл усложняется.
Экспериментальная ветка сохранена для воспроизводимости.

## Проверенный source и пакет

Финальный DEB пересобран из чистого source:

```text
1fa7cbdda7ca765064bd8c9db46de5c5574f358d
```

Пакет:

```text
dist/packages/open-switcher_0.1.0-3_amd64.deb
SHA-256:
3ac9360dbe79b15565958968a1cbef5bc5984ac915c4948b7aa0063ca2d15157
```

Debug symbols:

```text
dist/packages/open-switcher-dbgsym_0.1.0-3_amd64.ddeb
SHA-256:
fd5699b5c765015811eeae416c5b541c7f2f064f66b15f471ae013e2bcf4e0e9
```

Основной DEB побайтно совпал с exact `barrier-only` пакетом, который ранее
прошёл Mint/Cinnamon/X11 crash smoke и Ubuntu/GNOME/Wayland package smoke.
Поэтому повторная тяжёлая VM-кампания после пересборки не требуется.

Daemon внутри DEB:

```text
SHA-256:
9193f22eff01c8ae190b5a7da21ffcf75a0e010404972195763e0828cb02699e
```

Метаданные:

```text
Package: open-switcher
Version: 0.1.0-3
Architecture: amd64
```

## Пройденные проверки

На объединённой линии H-06 до формирования handoff:

- полная Rust-регрессия:
  `920` library passed, `1` ignored, `4` daemon, `11` D-Bus и `5` VM probe;
- `cargo check --locked --all-targets --features settings-ui`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- Debian package build с включёнными package tests;
- `wayland_diagnostics_test.sh`;
- `linux_input_setup_test.sh`;
- `debian_package_scripts_test.sh`;
- `manage_package_deb_test.sh`;
- извлечение и проверка точного содержимого DEB;
- проверка maintainer scripts через `sh -n`;
- проверка четырёх user units через `systemd-analyze --user verify`;
- проверка hidden guardian mode без X11 и socket activation:
  быстрый ожидаемый отказ, а не timeout.

В пакете находятся daemon, tray и settings. Отдельного guardian binary нет:
guardian запускается скрытым режимом того же packaged daemon binary. В пакете
присутствуют отдельные guardian `.socket` и `.service`.

## Runtime evidence

Подробные результаты:

- `docs/audits/2026-07-28-h06-runtime-validation.md`;
- `docs/audits/2026-07-29-h06-stage-1-package-mint-smoke.md`;
- `docs/audits/2026-07-29-h06-xtest-latency-validation.md`.

В VM подтверждены:

- обычная F12-коррекция, Caps Lock и две заглавные;
- exact normal trace;
- daemon `SIGKILL` после реального XTEST down;
- guardian `SIGKILL` после реального XTEST down;
- matching key-up и чистый keymap после аварий;
- восстановление daemon и пользовательского ввода;
- install, reinstall, upgrade-compatible lifecycle, remove и purge;
- отсутствие лишнего guardian process на Wayland.

## Остаточные риски

H-06 считается single-fault-safe в проверенной userspace-модели. Он не может
гарантировать восстановление при:

1. одновременной гибели daemon и guardian;
2. зависании ядра, X server или всей пользовательской сессии;
3. power loss;
4. одновременном отказе guardian и заранее открытой emergency X11 connection.

USB hot-unplug/replug, suspend/resume и смена X server epoch непосредственно
во время активной транзакции не воспроизводились на реальном оборудовании.
Это ограничения проверки, а не подтверждённые дефекты.

Остаточная ACL-запись после purge относится к отдельному finding package/input
permissions и не является незавершённой частью H-06.

## Статус старых чекбоксов

Неотмеченные `[ ]` в исходном длинном implementation plan не следует
интерпретировать как незавершённый код. Фактическое выполнение зафиксировано
коммитами, финальными отчётами, SHA артефактов и runtime evidence. Массовое
ретроспективное переключение чекбоксов не выполняется, чтобы не создавать
шумный документационный diff.

## Результат интеграции

`fix/h06-synthetic-input-ledger` влита в `master` обычным fast-forward без
конфликтов. Интеграционная точка:

```text
48fc75ee4ca36d08dd98e5b148e4c64bd4c36c37
```

После слияния на `master` повторены:

- полный Rust-набор:
  `920` library passed, `1` ignored, `4` daemon, `11` D-Bus и `5` VM probe;
- `cargo check --locked --all-targets --features settings-ui`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- четыре shell/package suite.

Wayland diagnostics и mock package test внутри restricted syscall/filesystem
sandbox получили ожидаемые ограничения `EPERM`. Те же неизменённые команды
вне sandbox завершились `ok`.

Финальный пакет сохранён в основном worktree:

```text
/home/andrey/Projects/OpenSwitcher/dist/packages/
open-switcher_0.1.0-3_amd64.deb
```

Незакоммиченные документы от 2026-07-15 и изменение `.gitignore`, находившиеся
в основном worktree до fast-forward, не входят в H-06. Их содержимое сверено
по SHA-256 до и после слияния и осталось неизменным.

Установку пакета на host следует выполнять отдельной точной
`sudo apt install` командой. VM-лаборатория, overlay и evidence сохранены и
могут быть удалены только по прямой просьбе пользователя.
