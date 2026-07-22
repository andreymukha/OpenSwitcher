# Закрытие финальных input-гонок — план реализации

> **Для агентных исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОДНАВЫК: использовать `superpowers:subagent-driven-development` (рекомендуется) либо `superpowers:executing-plans` и выполнять план по задачам. Для отслеживания используются checkbox (`- [ ]`).

**Цель:** Закрыть четыре Important-замечания финального review без изменения штатной функциональности ввода OpenSwitcher.

**Архитектура:** Существующий `terminal_gate` становится единственной точкой линеаризации публикации writer reply и terminal state. Startup X11 сохраняет rendezvous-channel, но явно уничтожает receiver до stop/join; deferred replay использует отдельный маленький маршрутизатор в существующий recovery; vendored `uinput` закрепляется exact-версией.

**Технологии:** Rust 2021, `std::sync::{mpsc, Mutex}`, atomics, Cargo `[patch.crates-io]`, unit tests, Debian package, Mint/Cinnamon/X11 VM.

---

### Задача 1: Атомарная публикация writer reply

**Файлы:**
- Изменить и тестировать: `src/daemon/keyboard.rs:240-558, 4152-4200, 6473-6554`

- [ ] **Шаг 1: написать RED-тест видимости reply под terminal gate**

Добавить в `mod tests`:

```rust
#[test]
fn writer_reply_is_not_visible_before_terminal_completion_is_committed() {
    let terminal_gate = Arc::new(Mutex::new(()));
    let control = WriterTransactionControl::new_with_terminal_gate(
        110,
        Duration::from_secs(1),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&terminal_gate),
    );
    let publisher_control = control.clone();
    let (reply_tx, reply_rx) = mpsc::channel();
    let held_gate = terminal_gate.lock().unwrap();
    let publisher = thread::spawn(move || {
        publish_writer_transaction_result(
            &publisher_control,
            reply_tx,
            Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 110,
                reason: "uinput write failed".to_string(),
            }),
        )
    });

    let early_reply = reply_rx.recv_timeout(Duration::from_millis(50));
    drop(held_gate);
    let publisher_result = publisher.join().expect("publisher should not panic");

    assert!(
        matches!(early_reply, Err(mpsc::RecvTimeoutError::Timeout)),
        "reply must not become visible before terminal state can be committed"
    );
    assert!(matches!(
        publisher_result,
        Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
            request_id: 110,
            ..
        })
    ));
    assert!(matches!(
        reply_rx.recv_timeout(Duration::from_millis(100)),
        Ok(Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
            request_id: 110,
            ..
        }))
    ));
    assert_eq!(control.state(), WriterTransactionState::Completed);
}
```

- [ ] **Шаг 2: запустить тест и подтвердить правильный RED**

Команда:

```bash
cargo test --lib writer_reply_is_not_visible_before_terminal_completion_is_committed
```

Ожидается `FAIL`: текущий `reply.send(...)` виден до освобождения удерживаемого `terminal_gate`.

- [ ] **Шаг 3: добавить минимальную атомарную publication boundary**

Рядом с `WriterTransactionState` добавить:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterCompletionPublication {
    Completed,
    Cancelled,
    ReceiverDisconnected,
}
```

Заменить внутренность `publish_completed` общей функцией:

```rust
fn publish_completed_with(
    &self,
    publish_reply: impl FnOnce() -> bool,
) -> WriterCompletionPublication {
    let _terminal_guard = self
        .terminal_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if self.stop_requested.load(Ordering::SeqCst)
        || self.failure_request_id.load(Ordering::SeqCst) != 0
        || self.state() != WriterTransactionState::Pending
    {
        return WriterCompletionPublication::Cancelled;
    }
    if Instant::now() >= self.deadline {
        let _ = self.try_mark_timed_out_while_terminal_gate_is_held();
        return WriterCompletionPublication::Cancelled;
    }
    if !publish_reply() {
        return WriterCompletionPublication::ReceiverDisconnected;
    }

    let completed = self
        .state
        .compare_exchange(
            WriterTransactionState::Pending as u8,
            WriterTransactionState::Completed as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok();
    debug_assert!(completed, "terminal gate must serialize completion");
    if completed {
        WriterCompletionPublication::Completed
    } else {
        WriterCompletionPublication::Cancelled
    }
}

fn publish_completed(&self) -> bool {
    matches!(
        self.publish_completed_with(|| true),
        WriterCompletionPublication::Completed
    )
}
```

В `publish_writer_transaction_result` и `publish_deferred_manual_completion` отправлять channel payload только через closure `publish_completed_with`, затем явно обработать три outcome. `Completed` сохраняет прежнюю подробную writer-ошибку, `Cancelled` возвращает `control.cancellation_error()`, `ReceiverDisconnected` возвращает `VirtualKeyboardWriterDisconnected`.

- [ ] **Шаг 4: подтвердить GREEN и порядок ошибок**

```bash
cargo test --lib writer_reply_is_not_visible_before_terminal_completion_is_committed
cargo test --lib queued_writer_error_wins_over_concurrent_input_worker_loss
cargo test --lib writer_stop_wins_terminal_race_against_late_completion
```

Ожидается по одному прошедшему тесту в каждой команде.

- [ ] **Шаг 5: зафиксировать задачу**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: publish writer terminal result atomically"
```

### Задача 2: Исключить deadlock позднего X11 ready

**Файлы:**
- Изменить и тестировать: `src/daemon/keyboard.rs:1005-1019, 1596-1657, tests`

- [ ] **Шаг 1: написать RED-тест требуемого порядка receiver-before-stop**

Добавить тест, использующий wished-for helper:

```rust
#[test]
fn startup_abort_drops_ready_receiver_before_requesting_worker_stop() {
    let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(0);
    let observed = Cell::new(false);

    abort_input_worker_startup(ready_rx, || {
        observed.set(matches!(
            ready_tx.try_send(()),
            Err(mpsc::TrySendError::Disconnected(()))
        ));
    });

    assert!(observed.get());
}
```

- [ ] **Шаг 2: запустить тест и подтвердить RED**

```bash
cargo test --lib startup_abort_drops_ready_receiver_before_requesting_worker_stop
```

Ожидается ошибка компиляции: `abort_input_worker_startup` ещё отсутствует.

- [ ] **Шаг 3: реализовать минимальный helper и применить его только к X11 watcher**

```rust
fn abort_input_worker_startup<T>(
    ready_rx: mpsc::Receiver<T>,
    request_stop: impl FnOnce(),
) {
    drop(ready_rx);
    request_stop();
}
```

В error-ветви `InputTargetWatcher::spawn` перед `handle.join()` вызвать:

```rust
abort_input_worker_startup(ready_rx, || {
    stop_flag.store(true, Ordering::SeqCst);
    alive.store(false, Ordering::SeqCst);
    signal_input_target_stop(stop_wakeup.as_ref());
});
let _ = handle.join();
```

Успешный startup и optional non-X11 policy не менять.

- [ ] **Шаг 4: подтвердить GREEN и соседние startup-policy tests**

```bash
cargo test --lib startup_abort_drops_ready_receiver_before_requesting_worker_stop
cargo test --lib input_target_watcher_readiness_is_true_when_disabled_by_policy
cargo test --lib input_target_monitor_connection_failure_is_required_worker_failure
```

Ожидается PASS.

- [ ] **Шаг 5: зафиксировать задачу**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: unblock late input watcher startup"
```

### Задача 3: Маршрутизировать deferred replay через recovery

**Файлы:**
- Изменить и тестировать: `src/daemon/service.rs:150-217, 1036-1054, 3010-3070`

- [ ] **Шаг 1: написать два RED-теста маршрутизации**

```rust
#[test]
fn deferred_input_worker_failure_requests_event_loop_recovery() {
    let recovery_called = Cell::new(false);
    let result = route_deferred_input_result(
        Err(SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        }),
        |error| {
            recovery_called.set(matches!(error, SwitcherError::InputWorkerDisconnected { .. }));
            true
        },
    );

    assert_eq!(result.unwrap(), DeferredInputRouting::Recovered);
    assert!(recovery_called.get());
}

#[test]
fn fatal_deferred_input_failure_preserves_detailed_error() {
    let result = route_deferred_input_result(
        Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 902 }),
        |_| false,
    );

    assert!(matches!(
        result,
        Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 902 })
    ));
}
```

- [ ] **Шаг 2: запустить тесты и подтвердить RED**

```bash
cargo test --lib deferred_input_worker_failure_requests_event_loop_recovery
```

Ожидается ошибка компиляции: `route_deferred_input_result` и `DeferredInputRouting` отсутствуют.

- [ ] **Шаг 3: реализовать маршрутизатор и подключить его к event-loop**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredInputRouting {
    Continue,
    Recovered,
}

fn route_deferred_input_result<E>(
    result: Result<(), E>,
    recover: impl FnOnce(&E) -> bool,
) -> Result<DeferredInputRouting, E> {
    match result {
        Ok(()) => Ok(DeferredInputRouting::Continue),
        Err(error) if recover(&error) => Ok(DeferredInputRouting::Recovered),
        Err(error) => Err(error),
    }
}
```

Вместо прямого `self.drain_one_deferred_input_event()?`:

```rust
let drain_result = self.drain_one_deferred_input_event();
match route_deferred_input_result(drain_result, |error| {
    self.handle_runtime_input_failure(error)
}) {
    Ok(DeferredInputRouting::Continue) => {}
    Ok(DeferredInputRouting::Recovered) => continue 'event_loop,
    Err(error) => {
        self.shutdown();
        return Err(error);
    }
}
```

- [ ] **Шаг 4: подтвердить GREEN и существующий batch recovery**

```bash
cargo test --lib deferred_input_worker_failure_requests_event_loop_recovery
cargo test --lib fatal_deferred_input_failure_preserves_detailed_error
cargo test --lib recoverable_runtime_health_failure_requests_recovery
```

Ожидается PASS.

- [ ] **Шаг 5: зафиксировать задачу**

```bash
git add src/daemon/service.rs
git commit -m "fix: recover deferred input replay failures"
```

### Задача 4: Закрепить точный источник uinput

**Файлы:**
- Изменить: `Cargo.toml:26`
- Проверить: `Cargo.lock`, `vendor/uinput-0.1.3/Cargo.toml`

- [ ] **Шаг 1: подтвердить, что exact-pin пока отсутствует**

```bash
rg '^uinput = "=0\.1\.3"$' Cargo.toml
```

Ожидается exit `1` и отсутствие совпадений.

- [ ] **Шаг 2: заменить requirement**

```toml
uinput = "=0.1.3"
```

- [ ] **Шаг 3: проверить exact requirement и path resolution**

```bash
rg '^uinput = "=0\.1\.3"$' Cargo.toml
cargo tree -i uinput --offline
cargo metadata --offline --format-version 1
cargo check --locked --offline --all-targets
```

Ожидается `uinput v0.1.3 (/.../vendor/uinput-0.1.3)`; metadata package `uinput` не имеет registry source; check завершается с exit `0`.

- [ ] **Шаг 4: зафиксировать задачу**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: pin patched uinput version"
```

### Задача 5: Финальная проверка, package-first runtime и отчёт

**Файлы:**
- Изменить: `docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md`
- Создать артефакт: `dist/packages/open-switcher_0.1.0-1_amd64.deb`

- [ ] **Шаг 1: локальная полная проверка**

```bash
cargo test --lib
cargo test --lib --features settings-ui -j1
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml
rustfmt --edition 2021 --check src/daemon/keyboard.rs src/daemon/input_backend.rs src/daemon/service.rs src/daemon/x11_wait.rs
git diff --check
```

Ожидается соответственно `590+` и `651+` тестов без failures, ownership `3/3`, форматирование и diff-check без вывода.

- [ ] **Шаг 2: собрать и идентифицировать новый Debian package**

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-1_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-1_amd64.deb
```

Зафиксировать новый SHA-256 и hash извлечённого `/usr/bin/open-switcher-daemon`.

- [ ] **Шаг 3: установить пакет в сохранённую VM и выполнить bounded runtime**

Проверить exact daemon hash, затем выполнить:

- функциональную матрицу movement/scroll/click/Enter/Tab/space/auto/two-capitals — `8/8`;
- не менее трёх повторов `ыгвщ -> движение -> F12 -> sudo`;
- один контролируемый разрыв подтверждённого X11 watcher fd;
- реальное окно release/regrab через bounded probe;
- восстановление в том же PID с `12` потоками, одним `/dev/uinput` fd и одним виртуальным устройством.

После теста удалить debug manager variables, закрыть Xed/Xephyr, восстановить layout group `0` и задержки `30/0/0`. Лабораторию не удалять.

- [ ] **Шаг 4: обновить русский отчёт и зафиксировать**

Добавить новые commit ids, RED/GREEN evidence, package hashes, runtime PID/latency и остаточные ограничения.

```bash
git add -f docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md
git commit -m "docs: validate final input race fixes"
```

- [ ] **Шаг 5: независимое review и интеграция**

Провести read-only review диапазона от `6056ea2` до нового HEAD. Critical/Important должны отсутствовать. Затем применить `superpowers:finishing-a-development-branch`, выполнить уже выбранный пользователем local fast-forward merge в `master`, повторить тесты на merged tree и скопировать проверенный `.deb` в основной `dist/packages`. Ветку, worktree и VM не удалять.
