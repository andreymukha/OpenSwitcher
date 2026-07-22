# Подтверждённая остановка virtual writer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Исключить ложный успешный shutdown и создание нового input backend рядом с неподтверждённо живым writer: после снятия физического grab ждать подтверждённый выход не более одной секунды, а при timeout завершать daemon с ошибкой для безопасного systemd restart.

**Architecture:** Один crate-visible `WriterShutdownOutcome` проходит без потери через writer, keyboard controller, backend lifecycle, service и daemon finalizer. `Unresponsive` является необратимым состоянием текущего процесса: retry запрещён, вторичные worker’ы только получают stop и отсоединяются без blocking join, а `main` возвращает ненулевой статус. Штатный путь по-прежнему присоединяет все потоки и лишь затем разрешает recovery.

**Tech Stack:** Rust 2021, `std::sync::{mpsc, Arc, Mutex}`, atomics, monotonic `Instant`, evdev/uinput/X11, zbus, systemd user service, Cargo tests, Debian package `0.1.0-2`, сохранённые Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland VM.

---

## Границы работы

В этом плане меняется только lifecycle остановки input writer и распространение
его результата. Не менять алгоритмы F12/автокоррекции, Caps Lock, двух
заглавных, layout switch, pointer invalidation, clipboard, значения
`delay_ms`/`backspace_ms`/`typing_ms`, polling-интервалы, udev/ACL и параметры
`Restart=`/`RestartSec=`.

Локальные тесты не должны открывать реальные `/dev/input` или `/dev/uinput`,
посылать клавиши, менять clipboard/layout/systemd/udev/ACL. Опасная runtime
fault injection разрешена только внутри сохранённых гостей. Лабораторию,
диски, worktree и evidence не удалять без прямой просьбы пользователя.

## Стабильный контракт типов

Использовать один результат на всех слоях:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriterShutdownOutcome {
    Stopped,
    Unresponsive { timeout_ms: u64 },
}
```

И отдельную невосстановимую ошибку:

```rust
#[error(
    "Virtual keyboard writer did not stop within {timeout_ms} ms during {phase}; trigger: {trigger}"
)]
VirtualKeyboardWriterShutdownUnresponsive {
    timeout_ms: u64,
    phase: &'static str,
    trigger: String,
},
```

`Unresponsive` не добавлять в `SwitcherError::is_recoverable_input_error`.
`trigger` содержит только текст исходной ошибки/фазы, без ввода, clipboard,
названия окна или correction plan.

### Task 1: Изолировать реализацию и зафиксировать baseline

**Файлы:**
- Не изменять production-файлы.
- Рабочая копия: `.worktrees/quiescent-writer-shutdown`

- [ ] **Step 1: применить `superpowers:using-git-worktrees`**

Создать ветку `fix/quiescent-writer-shutdown` от commit, содержащего эту
спецификацию и план. Сначала проверить, что `.worktrees/` игнорируется.

```bash
git check-ignore -q .worktrees
git worktree add .worktrees/quiescent-writer-shutdown -b fix/quiescent-writer-shutdown
git -C .worktrees/quiescent-writer-shutdown status --short --branch
```

Expected: чистый новый worktree; пользовательские `.gitignore` и старые
untracked docs в основной рабочей копии не попали в ветку.

- [ ] **Step 2: снять безопасный baseline**

```bash
cargo test --locked --lib writer_stop
cargo test --locked --lib keyboard_shutdown_sequence
cargo test --locked --lib recoverable_runtime_health_failure_requests_recovery
cargo test --locked --lib daemon_error_releases_input_before_potentially_blocking_monitor_stop
```

Expected: текущие тесты проходят; старый
`writer_stop_returns_when_writer_thread_does_not_finish` ещё подтверждает
дефект и будет заменён в Task 2.

### Task 2: Сделать остановку writer подтверждаемой и ограниченной

**Файлы:**
- Modify: `src/daemon/keyboard.rs:30-55`
- Modify: `src/daemon/keyboard.rs:130-150`
- Modify: `src/daemon/keyboard.rs:1822-2005`
- Modify tests: `src/daemon/keyboard.rs:5920-6160`

- [ ] **Step 1: написать RED-тесты результата и владения JoinHandle**

Добавить fake-thread seam без uinput и тесты:

```rust
writer_stop_without_thread_exit_returns_unresponsive
writer_stop_timeout_retains_join_handle
writer_stop_joins_after_exit_notification
writer_exit_notification_follows_owned_device_drop
writer_unresponsive_outcome_is_sticky_after_late_thread_exit
writer_stop_with_full_data_queue_acks_after_stop_check
```

Тестовый timeout — 10–30 ms; release channel обязательно освобождает и join’ит
fake thread в конце теста, чтобы сам тест не оставлял detached worker.

```bash
cargo test --locked --lib writer_stop_without_thread_exit_returns_unresponsive
cargo test --locked --lib writer_stop_timeout_retains_join_handle
```

Expected RED: outcome/exit notification ещё отсутствуют либо старый код
возвращает ложный успех и теряет handle.

- [ ] **Step 2: добавить ACK после уничтожения virtual device**

Заменить 50-ms join-константу на:

```rust
const WRITER_SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(1);
```

Добавить в `VirtualKeyboardWriter`:

```rust
exit_rx: mpsc::Receiver<()>,
shutdown_started_at: Option<Instant>,
shutdown_outcome: Option<WriterShutdownOutcome>,
```

Thread wrapper получает bounded exit sender. RAII notifier публикует exit
только после возврата/unwind `run_virtual_keyboard_writer_loop`; к этому
моменту переданный в loop `uinput::Device` уже уничтожен. Финальным
доказательством остаётся успешный `JoinHandle::join`.

`finish_stop_with_timeout` обязан:

1. считать deadline от первого `request_stop`;
2. разбудить writer best-effort существующей `Shutdown`-командой;
3. ждать notification/`is_finished` только до deadline;
4. делать `take()` handle только после `is_finished`;
5. вернуть `Stopped` только после `join`;
6. при timeout оставить handle в поле и навсегда latch’ить `Unresponsive`.

Не удерживать `transaction_terminal_gate` во время uinput/X11-вызова или
ожидания ACK: иначе `EVIOCGRAB(0)` снова сможет ждать зависший writer.

- [ ] **Step 3: закрыть startup timeout/disconnect**

В `VirtualKeyboardWriter::new` сначала собрать объект с `join_handle` и
`exit_rx`, затем ждать readiness. При ошибке readiness вызвать тот же bounded
stop. Чистая остановка сохраняет исходную startup error; `Unresponsive`
возвращает typed fail-stop с phase `writer-startup` и исходной ошибкой в
`trigger`. Не допускать неявного detach локального `JoinHandle`.

Проверить порядок notification отдельным fake owned object с Drop-trace:
`loop-return -> owned-device-drop -> exit-notification -> join`.

- [ ] **Step 4: сделать Drop идемпотентной страховкой**

`Drop` повторно публикует stop, но уважает cached outcome. После уже
зафиксированного `Unresponsive` он не ждёт ещё одну секунду и не может заменить
его на `Stopped`; production Drop не panic’ует.

- [ ] **Step 5: подтвердить GREEN и существующие admission-тесты**

```bash
cargo test --locked --lib writer_stop
cargo test --locked --lib writer_stop_request_denies_transaction_and_fast_mutation_permits
cargo test --locked --lib writer_command_loop
```

Expected: новые тесты PASS; full queue не мешает `stop_requested=true`; stop до
permit по-прежнему запрещает mutation.

- [ ] **Step 6: зафиксировать task**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: acknowledge virtual writer shutdown"
```

### Task 3: Сохранить release-first и исключить blocking Drop

**Файлы:**
- Modify: `src/daemon/keyboard.rs:95-125`
- Modify: `src/daemon/keyboard.rs:1120-1200`
- Modify: `src/daemon/keyboard.rs:1510-1760`
- Modify: `src/daemon/keyboard.rs:1333,2181,2437-2446`
- Modify: `src/error/mod.rs:100-175`

- [ ] **Step 1: написать RED-тесты порядка controller shutdown**

```rust
keyboard_shutdown_releases_grab_before_waiting_for_writer_ack
keyboard_shutdown_joins_watchers_only_after_writer_stopped
keyboard_shutdown_detaches_watchers_after_writer_unresponsive
repeated_keyboard_shutdown_cannot_mask_unresponsive
partial_prepare_error_is_preserved_after_stopped_writer
partial_prepare_unresponsive_writer_returns_fail_stop
```

Expected trace для clean path:

```text
request-writer-stop -> release-grab -> wait-writer -> join-watchers
```

Expected trace для fail-stop:

```text
request-writer-stop -> release-grab -> wait-writer -> detach-watchers
```

- [ ] **Step 2: вернуть outcome из KeyboardController**

Изменить `KeyboardController::shutdown(&mut self)` на
`-> WriterShutdownOutcome`. `release_grab_best_effort()` всегда вызывается до
`finish_stop()`. При `Stopped` watcher’ы штатно stop/join; при `Unresponsive`
им только публикуется stop/wakeup, их `JoinHandle` извлекается и detach’ится,
чтобы последующий `Drop` не вошёл в blocking join.

Разделить у `PointerWatcher` и `InputTargetWatcher` операции:

```rust
fn request_stop(&self);
fn stop_and_join(&mut self);
fn detach_for_process_fail_stop(&mut self);
```

Обычный Drop использует `stop_and_join`; fail-stop предварительно забирает
handle, поэтому Drop становится неблокирующим. Реальный keyboard fd всё равно
закрывается при drop controller; grab уже снят явно.

- [ ] **Step 3: закрыть recoverable partial initialization**

В `KeyboardController::prepare` явно обрабатывать ошибки `PointerWatcher::spawn`
и `InputTargetWatcher::spawn`: остановить уже созданный writer, при clean
shutdown вернуть исходную ошибку, при `Unresponsive` — typed fail-stop.

В `PreparedKeyboardController::activate` аналогично объединить ошибку readiness
или grab с outcome явного shutdown. Это не даёт background retry создать новый
writer после неявного Drop частично собранного controller.

- [ ] **Step 4: проверить typed error и partial paths**

```bash
cargo test --locked --lib keyboard_shutdown
cargo test --locked --lib input_dependencies
cargo test --locked --lib input_pipeline
cargo test --locked --lib error::tests
```

Expected: `Unresponsive` не recoverable; clean partial cleanup сохраняет
исходную подробную ошибку.

- [ ] **Step 5: зафиксировать task**

```bash
git add src/daemon/keyboard.rs src/error/mod.rs
git commit -m "fix: make keyboard shutdown fail-stop safe"
```

### Task 4: Протащить outcome через backend lifecycle

**Файлы:**
- Modify: `src/daemon/input_backend.rs:1-230`
- Modify tests: `src/daemon/input_backend.rs:275-650`

- [ ] **Step 1: расширить fake backend и написать RED-тесты**

`FakeBackend` получает настраиваемый `shutdown_outcome`. Добавить:

```rust
incomplete_backend_with_clean_shutdown_schedules_retry
incomplete_backend_with_unresponsive_writer_returns_fatal_error
unresponsive_partial_backend_does_not_call_opener_again
post_activation_failure_preserves_original_error_after_clean_shutdown
post_activation_failure_is_overridden_by_unresponsive_fail_stop
```

```bash
cargo test --locked --lib incomplete_backend_with_unresponsive_writer_returns_fatal_error
```

Expected RED: `InputBackendHandle::shutdown` ещё возвращает `()` и lifecycle
планирует retry независимо от реального завершения writer.

- [ ] **Step 2: изменить trait и disposal paths**

```rust
pub trait InputBackendHandle {
    fn shutdown(&mut self) -> WriterShutdownOutcome;
}
```

`ActiveInputBackend` возвращает результат `keyboard.shutdown()`.
`KeyboardInputBackendOpener::reopen_backend` и
`InputBackendLifecycle::try_reopen` разрешают прежний retry только после
`Stopped`. При `Unresponsive` немедленно возвращают typed fatal error с phase
`backend-open`/`backend-readiness`; исходная ошибка остаётся в `trigger`.

Разделить чистую классификацию runtime error и изменение lifecycle state:
shutdown старого backend должен завершиться как `Stopped` до перехода
`Ready -> Recovering` и установки retry deadline.

- [ ] **Step 3: подтвердить lifecycle GREEN**

```bash
cargo test --locked --lib daemon::input_backend
```

Expected: все прежние backoff/readiness tests и новые fail-stop tests PASS;
opener counter не увеличивается после unresponsive shutdown.

- [ ] **Step 4: зафиксировать task**

```bash
git add src/daemon/input_backend.rs
git commit -m "fix: gate backend recovery on writer exit"
```

### Task 5: Запретить recovery в service и сохранить fatal latch

**Файлы:**
- Modify: `src/daemon/service.rs:140-270`
- Modify: `src/daemon/service.rs:900-1090`
- Modify: `src/daemon/service.rs:2550-2630`
- Modify tests: `src/daemon/service.rs:2880-3130` и lifecycle tests ниже

- [ ] **Step 1: написать RED-тесты propagation**

```rust
capture_reset_returns_shutdown_outcome_when_lock_is_poisoned
occupied_install_slot_preserves_unresponsive_release_failure
runtime_recovery_state_changes_only_after_writer_stopped
runtime_recovery_propagates_unresponsive_shutdown
service_latches_unresponsive_shutdown_across_repeated_calls
unresponsive_writer_forbids_second_backend_install
```

- [ ] **Step 2: сделать helpers outcome-aware**

`reset_capture_epoch_then_shutdown_backend<F, R>` возвращает пару
`(Option<LayoutSwitchCaptureState>, R)`. Ошибка capture lock логируется, но не
скрывает writer outcome.

`admit_input_backend_install` принимает release closure, возвращающую
`WriterShutdownOutcome`: `Stopped` сохраняет прежний
`InputBackendAlreadyActive`, `Unresponsive` возвращает fail-stop.

Изменить routing callbacks:

```rust
FnOnce(&E) -> Result<bool, E>
```

для `route_runtime_health_failure` и `route_deferred_input_result`, чтобы
ошибка shutdown не превращалась в `true`/обычный recovery.

- [ ] **Step 3: изменить service lifecycle**

`drop_active_input_backend` и `DaemonService::shutdown` возвращают
`WriterShutdownOutcome`. Перед writer stop удалить selected-text transport;
после `Stopped` полностью drop старый controller/fd и только потом записать
`Recovering`. При `Unresponsive` не вызывать retry/install.

Добавить в `DaemonService` absorbing latch последнего `Unresponsive`, чтобы
последующий terminal `shutdown()` при уже пустом keyboard slot не вернул
ложный `Stopped`.

Если typed fail-stop пришёл из partial backend, который ещё не успел попасть в
service slot, он может не изменить этот latch. Поэтому terminal finalizer в
Task 6 обязан считать process fail-stop достаточным основанием сам по себе,
даже когда повторный `service.shutdown()` вернул `Stopped`.

`handle_runtime_input_failure` становится:

```rust
fn handle_runtime_input_failure(
    &mut self,
    error: &SwitcherError,
) -> Result<bool, SwitcherError>;
```

Убрать дублирующие `self.shutdown()` из ветвей `run`, которые немедленно
возвращают `Ok`/`Err`: единственный terminal owner — daemon finalizer.
Внутри живого event loop явный shutdown остаётся только частью clean internal
recovery.

- [ ] **Step 4: подтвердить GREEN и отсутствие второго backend**

```bash
cargo test --locked --lib recoverable_runtime_health_failure_requests_recovery
cargo test --locked --lib fatal_runtime_health_failure_preserves_detailed_error
cargo test --locked --lib deferred_input_worker_failure_requests_event_loop_recovery
cargo test --locked --lib unresponsive_writer_forbids_second_backend_install
cargo test --locked --lib service_latches_unresponsive_shutdown_across_repeated_calls
```

- [ ] **Step 5: зафиксировать task**

```bash
git add src/daemon/service.rs
git commit -m "fix: fail stop unresponsive input recovery"
```

### Task 6: Довести fail-stop до process exit и systemd restart

**Файлы:**
- Modify: `src/daemon/mod.rs:75-150`
- Modify tests: `src/daemon/mod.rs:175-260`
- Modify: `src/dbus/mod.rs:20-100`
- Verify unchanged: `src/main.rs:1-14`
- Verify unchanged: `debian/open-switcher.open-switcher-daemon.user.service`

- [ ] **Step 1: написать RED-тесты finalizer и monitor Drop**

```rust
clean_shutdown_preserves_primary_daemon_error_and_stops_monitor
unresponsive_shutdown_skips_monitor_join_and_returns_fatal_error
unresponsive_shutdown_preserves_primary_error_in_trigger
prior_fail_stop_is_not_masked_by_repeated_shutdown
service_initialization_fail_stop_detaches_monitor
capture_owner_monitor_detach_makes_drop_nonblocking
```

В blocking-monitor test closure должна сообщить вход в join. Для
`Unresponsive` канал обязан остаться пустым; тест не должен реально зависать.

- [ ] **Step 2: добавить неблокирующий monitor fail-stop path**

У `CaptureOwnerMonitor` добавить `detach_for_process_fail_stop`: best-effort
послать stop, извлечь и drop `JoinHandle` без join. После этого его обычный
`Drop` видит `None` и не блокирует завершение процесса.

Это разрешено только после release физического grab и latched
`Unresponsive`; clean path по-прежнему обязан `stop_and_join()`.

- [ ] **Step 3: объединить run result и shutdown outcome**

Оба finalizer принимают `Shutdown: FnOnce() -> WriterShutdownOutcome`.
Один monitor callback получает выбранный режим, чтобы не создавать два
одновременных mutable borrow одного monitor:

```rust
enum SecondaryShutdownMode {
    Join,
    DetachForProcessFailStop,
}

StopMonitor: FnOnce(SecondaryShutdownMode) -> std::thread::Result<()>;
```

- `Stopped`: остановить capture monitor и вернуть исходный `Result`;
- `Unresponsive`: вызвать monitor detach, не входить в secondary join и вернуть
  `VirtualKeyboardWriterShutdownUnresponsive`;
- если исходный result уже является этой typed fail-stop ошибкой, не маскировать
  её новым пустым shutdown result и выбрать `DetachForProcessFailStop`, даже
  если повторный service shutdown вернул `Stopped`;
- postmortem выполнить после release/detach и до возврата из `daemon::run`.

Не оставлять `DaemonService::new(...)?` после запуска capture monitor. Явно
обработать startup error тем же termination decision: обычная startup error
делает clean monitor join, typed writer fail-stop — monitor detach. Иначе
partial-init timeout обойдёт основной finalizer через оператор `?`.

`src/main.rs` уже преобразует любой `Err` в `ExitCode::FAILURE`.
Unit `Restart=on-failure`, `RestartSec=1` уже обеспечивает новый процесс;
systemd-конфигурацию не менять.

- [ ] **Step 4: подтвердить finalizer GREEN**

```bash
cargo test --locked --lib daemon_error_releases_input_before_potentially_blocking_monitor_stop
cargo test --locked --lib input_loop_postmortem_is_reported_only_after_backend_shutdown
cargo test --locked --lib unresponsive_shutdown_skips_monitor_join_and_returns_fatal_error
cargo test --locked --lib service_initialization_fail_stop_detaches_monitor
cargo test --locked --lib capture_owner_monitor_detach_makes_drop_nonblocking
```

- [ ] **Step 5: зафиксировать task**

```bash
git add src/daemon/mod.rs src/dbus/mod.rs
git commit -m "fix: terminate daemon after writer shutdown timeout"
```

### Task 7: Закрыть детерминированные гонки и выполнить полную проверку

**Файлы:**
- Modify tests: `src/daemon/keyboard.rs`
- Modify tests: `src/daemon/input_backend.rs`
- Modify tests: `src/daemon/service.rs`
- Modify tests: `src/daemon/mod.rs`

- [ ] **Step 1: добавить cross-layer race tests**

Без реального uinput смоделировать barrier после mutation permit:

```rust
stop_before_mutation_permit_prevents_backend_call
admitted_mutation_keeps_shutdown_unresponsive_until_thread_exit
late_writer_exit_does_not_enable_same_process_recovery
writer_error_exit_is_joined_as_stopped
writer_panic_exit_is_joined_as_stopped
drop_does_not_retry_or_mask_latched_unresponsive
```

Test trace должен доказывать: admission закрыт, grab-release phase уже прошла,
writer остаётся неподтверждённым до release barrier, opener не вызывается.
Не добавлять operation-wide synthetic ledger: это следующий отдельный High
slice.

- [ ] **Step 2: запустить targeted matrix**

```bash
cargo test --locked --lib writer_stop
cargo test --locked --lib keyboard_shutdown
cargo test --locked --lib daemon::input_backend
cargo test --locked --lib runtime_health_failure
cargo test --locked --lib unresponsive_shutdown
```

Expected: PASS без секундных sleeps и без доступа к устройствам.

- [ ] **Step 3: запустить полную безопасную матрицу**

```bash
cargo test --locked --lib
cargo test --locked --features settings-ui --lib -j1
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml
cargo check --locked --offline --all-targets
cargo check --locked --offline --all-targets --features settings-ui
bash tests/wayland_diagnostics_test.sh
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
rustfmt --edition 2021 --check src/daemon/keyboard.rs src/daemon/input_backend.rs src/daemon/service.rs src/daemon/mod.rs src/dbus/mod.rs src/error/mod.rs
git diff --check
```

Expected: все доступные tests/checks PASS. Если host sandbox запрещает только
известные socket tests, прогнать exact собранные test binaries внутри VM и
зафиксировать это как ограничение, не выдавая локальный EPERM за code failure.

- [ ] **Step 4: проверить scope и отсутствие опасного расширения**

```bash
git diff --stat master...HEAD
git diff master...HEAD -- src/daemon/keyboard.rs src/daemon/input_backend.rs src/daemon/service.rs src/daemon/mod.rs src/dbus/mod.rs src/error/mod.rs
git diff master...HEAD -U0 | rg '^\+.*unsafe' || true
git diff master...HEAD -U0 | rg '^\+.*(delay_ms|backspace_ms|typing_ms|POINTER_POLL_INTERVAL|INPUT_TARGET_POLL_INTERVAL)' || true
```

Expected: нет нового `unsafe`; пользовательские задержки и unrelated input
semantics не менялись.

- [ ] **Step 5: применить `superpowers:requesting-code-review`**

Независимый review проверяет весь диапазон реализации относительно design:
handle retention, exit-after-device-drop, partial init, sticky fail-stop,
отсутствие второго backend и отсутствие blocking Drop. Любое Critical/High
или обоснованное Medium исправить через TDD и повторить матрицу.

- [ ] **Step 6: зафиксировать test hardening при наличии отдельного diff**

```bash
git add src/daemon/keyboard.rs src/daemon/input_backend.rs src/daemon/service.rs src/daemon/mod.rs src/dbus/mod.rs src/error/mod.rs
git commit -m "test: cover writer fail-stop boundaries"
```

Если после предыдущих task-коммитов diff пуст, этот commit не создавать.

### Task 8: Собрать `0.1.0-2` и проверить exact package в двух VM

**Файлы:**
- Modify: `debian/changelog`
- Create artifact: `dist/packages/open-switcher_0.1.0-2_amd64.deb`
- Create: `docs/audits/2026-07-23-quiescent-writer-shutdown-validation.md`

- [ ] **Step 1: поднять Debian revision**

Добавить верхнюю запись `0.1.0-2` с пунктом о подтверждённой остановке writer и
process fail-stop. Не менять Cargo application version и systemd policy.

```bash
head -n 12 debian/changelog
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
git add debian/changelog
git commit -m "chore: prepare 0.1.0-2 package"
```

- [ ] **Step 2: собрать и идентифицировать canonical DEB**

Полная test matrix уже выполнена, поэтому не повторять её внутри package build:

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-2_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-2_amd64.deb
dpkg-deb --fsys-tarfile dist/packages/open-switcher_0.1.0-2_amd64.deb | tar -xOf - ./usr/bin/open-switcher-daemon | sha256sum
```

Expected: package version `0.1.0-2`, arch `amd64`; записать SHA package и
извлечённого `/usr/bin/open-switcher-daemon`.

- [ ] **Step 3: запустить сохранённые профили и установить exact artifact**

Из `.worktrees/vm-lab`:

```bash
python3 -m tools.vm_lab.session mint-installed
python3 -m tools.vm_lab.session ubuntu-installed
```

Передать exact file по loopback SSH на Mint `22223` и Ubuntu `22222`, сверить
SHA в гостях, установить `sudo apt install /tmp/open-switcher_0.1.0-2_amd64.deb`.
Из-за отдельно известного M-09a после install обязательно выполнить:

```bash
systemctl --user daemon-reload
systemctl --user restart open-switcher-daemon.service
dpkg-query -W -f='${Package} ${Version} ${Architecture}\n' open-switcher
```

Сравнить `/proc/$PID/exe`, SHA `/usr/bin/open-switcher-daemon` и packaged
binary. Старый `(deleted)` executable недопустим для acceptance.

- [ ] **Step 4: выполнить обычную package-first regression matrix**

В Mint/Cinnamon/X11 проверить: F12 `ыгвщ -> sudo`, первое слово в новом окне,
auto correction, layout switch, две заглавные, текущий Caps Lock baseline,
Enter/Tab/Space, движение tablet, scroll, physical click и отсутствие сброса
от movement/touch. Выполнить 10 clean stop/start циклов; каждый должен
завершаться без `Unresponsive`, start-limit-hit и роста uinput fd/device count.

В Ubuntu/GNOME/Wayland повторить доступный smoke без выдачи его за XTest
проверку.

- [ ] **Step 5: выполнить writer-specific fault injection только в Mint VM**

Использовать отдельный root/ptrace или gdb non-stop control channel внутри
гостя, который останавливает только подтверждённый writer TID после mutation
permit; весь daemon через `SIGSTOP` не останавливать. Затем известным
проверенным способом закрыть fd обязательного X11 watcher и запустить
независимый bounded `EVIOCGRAB` probe.

До инъекции записать PID/TID, fd и virtual-device count. Acceptance:

1. probe получает и сразу освобождает grab до writer ACK deadline;
2. старый PID не создаёт второй backend/uinput device;
3. через 1 s фиксируется typed fail-stop;
4. старый PID исчезает с ненулевым `ExecMainStatus`;
5. systemd через существующий `RestartSec=1` запускает новый PID;
6. старые fd/device исчезают вместе со старым процессом;
7. новый PID проходит readiness и имеет ровно один uinput fd/device.

Если управляющий канал не может доказанно остановить только writer TID, этот
runtime-тест не запускать: оставить unit/fake evidence и явно записать
непроверенный runtime-риск вместо опасной импровизации.

- [ ] **Step 6: очистить только временные guest-настройки**

Удалить временный debugger/fault helper, debug manager environment и test
windows/processes; восстановить layout group `0` и штатные config delays.
Оставить package установленным и службы active. VM, snapshots, disks и lab
metadata не удалять.

- [ ] **Step 7: написать русский validation report**

Отчёт должен содержать:

- commit range и clean/dirty state;
- RED/GREEN test evidence и точные counts;
- package SHA и daemon SHA;
- PID/TID/fd/device timeline clean и fail-stop paths;
- `EVIOCGRAB` release latency, exit status и systemd restart latency;
- функциональную матрицу X11/Wayland;
- факт явного restart после upgrade из-за M-09a;
- оставшиеся deferred-event, synthetic-ledger, clipboard и package/ACL риски;
- подтверждение, что host input/session/system configuration не менялись.

```bash
git add -f docs/audits/2026-07-23-quiescent-writer-shutdown-validation.md
git commit -m "docs: validate acknowledged writer shutdown"
```

### Task 9: Финальный review и интеграция

- [ ] **Step 1: применить `superpowers:verification-before-completion`**

Повторить изменённые targeted tests, `git diff --check`, проверить exact
package hashes и состояние обеих VM. Не заявлять fail-safe без фактического
writer-specific runtime PASS; при непроверенном fault injection формулировать
результат как «статически и детерминированно закрыто, runtime остаётся
ограничением».

- [ ] **Step 2: применить `superpowers:finishing-a-development-branch`**

Предложить пользователю интеграцию. При ранее выбранном local fast-forward:
обновить `master` только после зелёного review, повторить safe targeted matrix
на merged tree и скопировать exact проверенный `.deb` в основной
`dist/packages/`.

- [ ] **Step 3: сохранить продолжение аудита**

В pause/handoff отметить следующие независимые High-срезы в порядке:

1. conservation/reconciliation deferred physical events;
2. operation-wide synthetic key ledger с failure-at-operation-N;
3. transactional clipboard/selected-text safety;
4. package upgrade/remove и seat/ACL boundary.

Не начинать их в этой ветке и не удалять лабораторию.
