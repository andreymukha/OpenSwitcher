# Проверка безопасной clipboard-транзакции OpenSwitcher

**Дата:** 2026-08-11

**Область:** M-01, M-02 и M-03 исходного аудита

**Функциональный candidate:**
`063610454aad6df4055e29aef899093debcb1e6f`

**База `master`:**
`e673c19e9d90495a476aa31d2fd532e4637f80a5`

## Итог

M-01 закрыт: единый `ClipboardTransaction` выполняет условный rollback на
ранних ошибках и при Rust panic с unwinding. M-03 закрыт в практически
достижимой границе owner/value checks: OpenSwitcher не восстанавливает старый
clipboard, если обнаружено новое значение, новый owner либо неоднозначное
наблюдение.

M-02 не объявляется полностью исправленным. Зафиксировано согласованное
продуктовое поведение: произвольные MIME-данные не архивируются, нетекстовый
clipboard не блокирует преобразование и после успешной операции заменяется
преобразованным текстом. Это осознанный компромисс, а не гарантия сохранения
изображений или списка файлов.

Автоматические тесты и один и тот же установленный DEB прошли в
Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland. Во всех runtime-сценариях daemon
остался активен с прежним PID и `NRestarts=0`.

## Реализованная граница

Функциональные изменения находятся в:

- `src/daemon/selected_text/clipboard_transaction.rs` — снимок, mutation
  intent, финализация и rollback в `Drop`;
- `src/daemon/selected_text/clipboard_owner.rs` — получение X11 `CLIPBOARD`
  selection owner;
- `src/daemon/selected_text/clipboard.rs` — orchestration через транзакцию;
- `src/daemon/selected_text/mod.rs` — типизированный
  `ClipboardDisposition`;
- `src/daemon/selected_text/runner.rs` — предупреждение только для реального
  `RestoreFailed`.

Связанные функциональные commits:

- `448d1aa` — успешный fallback для невосстановимого clipboard;
- `dffe612` — rollback ранних ошибок и unwind;
- `54ecf34` — owner/value защита от конкурентной записи;
- `0636104` — расширенная failure matrix и диагностика результата.

Алгоритмы F12-коррекции слова, автопереключения, исправления двух заглавных и
случайного Caps Lock этим изменением не затрагивались. Существующие интервалы
10/60/120/300/900 мс сохранены; новых фиксированных ожиданий не добавлено.

## Автоматические проверки

### Целевой набор

`cargo test --locked --lib selected_text -- --nocapture`:

```text
58 passed; 0 failed
```

Матрица fake-backend покрывает:

- ошибку чтения исходного clipboard;
- sentinel write до и после частичной мутации;
- ошибку и timeout копирования;
- converted write до и после частичной мутации;
- ошибку paste и restore;
- panic на copy и paste;
- смену owner, значения и owner во время согласованного чтения;
- одинаковый текст от другого owner;
- чужую запись во время обычного завершения и `Drop`;
- невосстановимый исходный clipboard и `NoSelectedText`.

Тесты используют только fake clipboard/transport и не обращаются к clipboard,
X11, Wayland или устройствам ввода хоста.

### Полный последовательный gate

`cargo test --locked --all-targets -- --test-threads=1` вне restricted
seccomp-песочницы:

```text
library:       957 passed; 0 failed; 1 ignored
main:            4 passed; 0 failed
D-Bus API:       11 passed; 0 failed
H-06 VM probe:    5 passed; 0 failed
total:          977 passed; 0 failed; 1 ignored
```

Дополнительно прошли:

- `cargo fmt --check`;
- `git diff --check`;
- `tests/debian_package_scripts_test.sh`;
- `tests/input_access_package_test.sh`;
- `tests/manage_package_deb_test.sh`.

Сборщик DEB повторно выполнил обычный Rust gate и gate с
`--features settings-ui`; последний дал `1018 passed`, `0 failed`,
`1 ignored`. Package shell tests внутри сборки также прошли.

## Идентичность пакета

```text
Package: open-switcher
Version: 0.1.0-7
Architecture: amd64
Size: 3370840 bytes
SHA-256: 3a0c7f130fbbc88ac94bbff24901bb376e1a978192db8df6eceebb966767b451
```

Артефакт:

```text
dist/packages/open-switcher_0.1.0-7_amd64.deb
```

Обе гостевые системы подтвердили тот же SHA-256 после копирования пакета.

## Mint 22.2 / Cinnamon / X11

Пакет обновлён с `0.1.0-6` до `0.1.0-7`. Подтверждены
`XDG_SESSION_TYPE=x11`, раскладки `us,ru` и работа установленного
`/usr/bin/open-switcher-daemon`.

| Сценарий | Фактический результат |
|---|---|
| Текстовый clipboard | выделенное `ghbdtn` заменено на `привет`; `prior-mint-text-2` восстановлен |
| `image/png` без текстового target | преобразование выполнено; итоговый clipboard содержит `привет` |
| Конкурентная запись | вставлено `привет`; более новый `foreign-mint-wins` сохранён |
| Нет выделения | восстановлен `prior-no-selection`; префикс sentinel отсутствует |

До и после матрицы:

```text
MainPID=3889
NRestarts=0
ActiveState=active
SubState=running
```

Первый подготовительный прогон Mint не включён в результат: одна составная
команда `xdotool` ошибочно ввела свой хвост как текст. После разделения команд
подготовки все четыре чистых сценария были повторены успешно. Это была ошибка
test harness, а не наблюдаемое поведение OpenSwitcher.

## Ubuntu 24.04 / GNOME / Wayland

Пакет обновлён с `0.1.0-5` до `0.1.0-7`. Подтверждены
`XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-0` и запуск текущего
установленного executable:

```text
/usr/bin/open-switcher-daemon SHA-256:
f1df630765a2087ed049dc5e374a0aee05cb886fe4aea39e88051d4b541a8262

/proc/7428/exe SHA-256:
f1df630765a2087ed049dc5e374a0aee05cb886fe4aea39e88051d4b541a8262
```

Текст и hotkey отправлялись только виртуальной QEMU USB keyboard; `xdotool` для
нативного Wayland-окна не использовался.

| Сценарий | Фактический результат |
|---|---|
| Текстовый clipboard | выделенное `ghbdtn` заменено на `привет`; `prior-ubuntu-text` восстановлен |
| `image/png` без текстового MIME | преобразование выполнено; итоговый clipboard содержит `привет` |
| Конкурентная запись | вставлено `привет`; более новый `foreign-ubuntu-wins` сохранён |
| Нет выделения | восстановлен `prior-ubuntu-no-selection`; префикс sentinel отсутствует |

До и после матрицы:

```text
MainPID=7428
NRestarts=0
ActiveState=active
SubState=running
```

## Граница безопасности проверки

- Гостям не передавались физические input-устройства, USB хоста или shared
  clipboard.
- Клавиатурные события отправлялись только в `testkbd` виртуальной машины.
- Clipboard, systemd и пакеты изменялись только внутри гостей.
- Использовались QEMU user-mode NAT и loopback SSH; маршруты, DNS, firewall и
  сессия хоста не менялись.
- Обе VM штатно выключены. Overlays и лаборатория сохранены и не удалялись.

## Ограничения и остаточные риски

1. Произвольные MIME-данные не восстанавливаются по принятому продуктовому
   решению. При ранней ошибке после sentinel исходное изображение также может
   быть потеряно, поскольку backend не умеет его архивировать.
2. `Drop` работает при Rust unwind, но не при `SIGKILL`, `abort`, гибели всего
   процесса, power loss или остановке ядра.
3. Между последней проверкой owner/value и записью старого текста остаётся
   короткое TOCTOU-микроокно: X11/Wayland clipboard не предоставляет атомарный
   compare-and-swap.
4. Текущий backend `arboard` и owner probe используют X11; в Wayland-профиле
   это проверенный XWayland bridge. Будущий native Wayland backend должен
   предоставить собственный устойчивый owner/serial token.
5. Реальные ошибки каждого clipboard API не инъектировались в desktop VM;
   failure-at-every-step проверен детерминированными unit-тестами.
6. Это целевой package-first smoke M-01..M-03, а не отложенная объединённая
   финальная install/upgrade/remove/fault-кампания всего проекта.

## Статус аудита после проверки

- M-01 — **закрыто**;
- M-02 — **принятое продуктовое поведение**;
- M-03 — **закрыто в практической границе owner/value checks**;
- открыты M-04, M-05 и M-06.

Сводка: **17 закрыто, 1 принято, 3 открыто**.
