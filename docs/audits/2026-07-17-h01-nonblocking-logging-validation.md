# Проверка H-01: неблокирующая диагностика

- Дата: 2026-07-17
- Ветка: `fix/audit-remediation`
- Проверенный commit: `16f760b` (`fix: isolate layout detection diagnostics`)
- Scope: logging/output slice H-01; блокирующие runtime/config/backend вызовы остаются отдельным следующим slice
- Статус: исправлено и проверено в заявленном scope

## Результат

Все debug-категории daemon переведены на один process-wide bounded hub:

- очередь ограничена 256 записями;
- одна запись ограничена 4096 bytes на границе UTF-8;
- producer использует только `try_send` и при переполнении отбрасывает новую запись;
- file/stderr I/O выполняет только отдельный worker;
- при выключенных debug-флагах channel и worker не создаются;
- shutdown не ждёт незавершённый logger worker;
- selected-text payload по-прежнему проходит только через metadata summary.

Файлы открываются с `O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK`, после открытия
проверяются как regular file текущего effective UID и приводятся к mode `0600`.
Ошибка одного sink отключает только его и не возвращается в daemon control flow.

Обычный вывод, способный выполняться на grab-critical path, устранён или
переставлен после release. Panic input loop теперь сохраняет reason
неблокирующе, вызывает `service.shutdown()`, останавливает capture monitor и
только затем печатает postmortem. Ошибка явного `ungrab` и fallback `Drop`
больше не печатаются синхронно до закрытия keyboard fd.

Во время финального review обнаружен пропуск первоначального плана:
`src/layout_switch/mod.rs` не входил в его static `eprintln!` boundary, хотя
auto-detection вызывается и из periodic runtime sync при активном grab. Commit
`16f760b` перевёл все шесть fallback diagnostics этого модуля в тот же
неблокирующий hub и добавил regression gate на отсутствие прямого output.

## Коммиты

- `71e08a4` — implementation plan;
- `d31d258` — bounded nonblocking producer;
- `f40af54` — worker lifecycle и secure sinks;
- `f79a056` — lazy initialization и миграция debug helpers;
- `d8e681d` — release-before-postmortem ordering;
- `16f760b` — review-fix для layout auto-detection diagnostics.

## Доказанные свойства

| Свойство | Проверка |
|---|---|
| Полная очередь не ждёт и сохраняет первую запись | capacity-one saturation test |
| Закрытый receiver не вызывает panic/fallback I/O | disconnected queue test |
| Disabled category не строит record | closure side-effect test |
| Default config не создаёт channel/worker/files | disabled runtime test и static package-unit scan |
| Blocked sink не блокирует producer | intentionally blocked fake sink test |
| `Drop` не join-ит blocked worker | bounded drop regression |
| Symlink/FIFO отклоняются, mode становится `0600` | secure-file tests |
| Unsafe sink не переоткрывается и не имеет fallback | permanent per-sink disable test |
| Prefixes и category routing сохранены | formatter/routing tests |
| Selected text не попадает в record | redaction regression |
| Input release предшествует postmortem | ordered finalizer regression |
| Layout detection не имеет прямого stderr | source-boundary regression |

Installed Debian user units не задают debug-переменные. Их по-прежнему включает
только development flow `manage.sh`; selected-text debug остаётся отдельно
выключенным по умолчанию.

## Классификация оставшегося прямого output

- `KeyboardController::prepare` печатает до `EVIOCGRAB`;
- virtual writer и selected-text messages выполняются в отдельных worker threads;
- D-Bus publisher и capture owner monitor loop messages выполняются в их workers;
- panic output из `CaptureOwnerMonitor::Drop` возможен только после join; в
  normal shutdown input уже release, а в startup error grab ещё не получен;
- daemon panic и capture-monitor shutdown messages выполняются после
  `service.shutdown()` и release input backend;
- `main` печатает после возврата из `daemon::run`;
- tray/settings являются отдельными процессами и не владеют keyboard grab.

Эти paths могут потерять или задержать собственное сообщение, но не участвуют в
forwarding либо в обязательном release keyboard fd.

## Верификация

| Проверка | Результат |
|---|---|
| stable `rustfmt --check` для изменённых файлов | pass |
| `cargo check --all-targets` | pass |
| logger tests | 16 passed |
| keyboard tests | 103 passed |
| runtime tests | 67 passed |
| capture tests | 25 passed |
| selected-text tests | 32 passed |
| layout-switch tests | 26 passed |
| local `cargo test --lib` | 525 passed, 9 sandbox-only `EPERM` |
| тот же test binary в Mint VM с session D-Bus | 534 passed, 0 failed |
| `git diff --check` | pass |

Девять local failures были ровно прежними ограничениями sandbox: четыре D-Bus
session tests и пять Unix-socket tests. В Mint guest они прошли без изменений
кода. Прогон не открывал `/dev/input`, не создавал uinput device и не менял
clipboard, layout, systemd, udev или ACL.

## Ограничения и остаточный риск

- Этот checkpoint проверял текущий Rust test binary, а не новый установленный
  `.deb`; package-first двухпрофильный прогон будет выполнен после следующего
  связного H-01 slice, чтобы не пересобирать одинаковый пакет после каждого
  малого изменения.
- Реальный blocked stderr/filesystem одновременно с активным virtual grab не
  запускался; изоляция доказана deterministic fake-sink test и архитектурной
  границей worker thread.
- Доставка debug records best-effort: нет flush guarantee, rotation и retry.
- Logger не устраняет синхронные external commands, config persistence,
  runtime/backend locks или другие потенциально долгие вызовы input loop.
- Общий механизм input lifecycle всё ещё нельзя назвать полностью fail-safe до
  закрытия оставшихся H-01/H-03/H-04/H-06/H-08 и runtime campaign.

## Следующий slice

Следующим должен быть snapshot/cache boundary для данных, нужных input loop:
выявить все runtime/config/layout/backend вызовы под grab, отделить pure snapshot
от I/O и lock-heavy refresh, определить stale/error semantics и доказать, что
event forwarding и release больше не зависят от external command, D-Bus,
filesystem или длительного runtime lock.

Лаборатория и обе guest-системы сохраняются; удаление не выполнялось.
