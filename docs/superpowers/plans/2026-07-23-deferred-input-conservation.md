# Deferred Input Conservation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Не терять уже перехваченные физические key-события при отмене,
переполнении, replay-ошибке и остановке асинхронной ручной коррекции.

**Architecture:** `DaemonService` разделяет фазу ручной коррекции и
`DeferredInputLedger`, а writer получает отдельную кооперативную отмену
конкретного request, линеаризованную существующим terminal gate. Мягкая отмена
завершается только после нормализации модификаторов; replay использует
peek-before-ack. Смерть backend не переносит хвост в новое окно: после
release-first teardown он явно учитывается как terminal reconciliation.

**Tech Stack:** Rust 2021/1.95, `evdev`, vendored `uinput 0.1.3`, X11
XTest/XKB, `std::sync`, встроенный Rust test harness, Debian packaging.

---

## Карта файлов

- Create: `src/daemon/deferred_input.rs`
  - чистый ordered ledger, sequence ids, soft/hard limit, ACK и reconciliation;
  - без I/O, потоков и логирования.
- Modify: `src/daemon/mod.rs`
  - подключение внутреннего модуля.
- Modify: `src/error/mod.rs`
  - typed soft-cancel и аварийное переполнение.
- Modify: `src/daemon/keyboard.rs`
  - per-request cancel flag под terminal gate;
  - нефатальный `ManualCurrentWordOutcome::Cancelled`;
  - cleanup uinput/XTest и API controller.
- Modify: `src/daemon/service.rs`
  - состояния `InFlight`, `CancelRequested`, `Draining`;
  - отдельное владение ledger;
  - сохранение fetched batch/tail;
  - reconciliation перед уничтожением поколения backend.
- Create: `docs/audits/2026-07-23-deferred-input-conservation-validation.md`
  - фактические RED/GREEN результаты, VM evidence и остаточные ограничения.

Обычный fast path writer, параметры задержек, click-классификация, clipboard,
udev/ACL и package scripts не меняются.

### Task 1: Чистый ordered ledger

**Files:**

- Create: `src/daemon/deferred_input.rs`
- Modify: `src/daemon/mod.rs`

- [ ] **Step 1: Написать RED-тесты admission, ACK и reconciliation**

В `src/daemon/deferred_input.rs` сначала добавить тестовый контракт:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use evdev::Key;
    use std::time::SystemTime;

    fn event(sequence_id: u64) -> DeferredInputEvent {
        DeferredInputEvent {
            sequence_id,
            key: Key::KEY_A,
            value: 1,
            timestamp: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn two_hundred_fifty_seventh_event_is_retained_and_requests_cancel_once() {
        let mut ledger = DeferredInputLedger::default();
        for sequence_id in 1..=256 {
            assert_eq!(
                ledger.admit(event(sequence_id)),
                DeferredAdmission::Queued
            );
        }
        assert_eq!(
            ledger.admit(event(257)),
            DeferredAdmission::RequestCancellation
        );
        assert_eq!(ledger.len(), 257);
        assert_eq!(
            ledger.admit(event(258)),
            DeferredAdmission::Queued
        );
    }

    #[test]
    fn head_is_removed_only_after_matching_ack() {
        let mut ledger = DeferredInputLedger::default();
        ledger.admit(event(10));
        ledger.admit(event(11));
        assert_eq!(ledger.peek().map(|event| event.sequence_id), Some(10));
        assert_eq!(
            ledger.acknowledge(11),
            Err(DeferredAckError {
                expected: 10,
                received: 11,
            })
        );
        assert_eq!(ledger.len(), 2);
        ledger.acknowledge(10).unwrap();
        assert_eq!(ledger.peek().map(|event| event.sequence_id), Some(11));
    }

    #[test]
    fn terminal_reconciliation_accounts_for_every_queued_event_once() {
        let mut ledger = DeferredInputLedger::default();
        ledger.admit(event(1));
        ledger.admit(event(2));
        ledger.acknowledge(1).unwrap();
        let report = ledger.reconcile_all();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.acknowledged, 1);
        assert_eq!(report.reconciled, 1);
        assert_eq!(report.queued, 0);
        assert!(ledger.is_empty());
    }

    #[test]
    fn hard_limit_rejects_ownership_transfer_without_dropping_existing_queue() {
        let mut ledger = DeferredInputLedger::with_limits(2, 3);
        assert_eq!(ledger.admit(event(1)), DeferredAdmission::Queued);
        assert_eq!(ledger.admit(event(2)), DeferredAdmission::Queued);
        assert_eq!(
            ledger.admit(event(3)),
            DeferredAdmission::RequestCancellation
        );
        assert_eq!(
            ledger.admit(event(4)),
            DeferredAdmission::CapacityExceeded { limit: 3 }
        );
        assert_eq!(ledger.len(), 3);
    }
}
```

- [ ] **Step 2: Запустить тест и наблюсти RED**

Run:

```bash
cargo test --locked --lib daemon::deferred_input::tests -- --nocapture
```

Expected: FAIL/ошибка компиляции, потому что модуль и типы ещё отсутствуют.

- [ ] **Step 3: Реализовать минимальный ledger**

Добавить модуль в `src/daemon/mod.rs`:

```rust
pub(crate) mod deferred_input;
```

Создать типы:

```rust
use evdev::Key;
use std::collections::VecDeque;
use std::time::SystemTime;

pub(crate) const DEFERRED_INPUT_SOFT_LIMIT: usize = 256;
pub(crate) const DEFERRED_INPUT_HARD_LIMIT: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeferredInputEvent {
    pub(crate) sequence_id: u64,
    pub(crate) key: Key,
    pub(crate) value: i32,
    pub(crate) timestamp: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeferredAdmission {
    Queued,
    RequestCancellation,
    CapacityExceeded { limit: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeferredAckError {
    pub(crate) expected: u64,
    pub(crate) received: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeferredReconciliationReport {
    pub(crate) accepted: u64,
    pub(crate) acknowledged: u64,
    pub(crate) reconciled: u64,
    pub(crate) queued: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredInputLedger {
    queue: VecDeque<DeferredInputEvent>,
    accepted: u64,
    acknowledged: u64,
    soft_limit_reported: bool,
    soft_limit: usize,
    hard_limit: usize,
}
```

`admit()` должен сначала проверять hard capacity, затем добавлять событие.
Первое состояние `len > soft_limit` возвращает `RequestCancellation`;
следующие admission возвращают `Queued`. `acknowledge()` сравнивает sequence
только с головой. `reconcile_all()` прибавляет длину очереди к reconciled,
очищает очередь и сбрасывает session counters после формирования report.
Добавить `finish_drained()` с проверкой пустой очереди и тем же reset counters.
Для service tests добавить:

```rust
#[cfg(test)]
pub(crate) fn sequence_ids_for_test(&self) -> Vec<u64> {
    self.queue.iter().map(|event| event.sequence_id).collect()
}
```

- [ ] **Step 4: Запустить GREEN и форматирование**

Run:

```bash
cargo test --locked --lib daemon::deferred_input::tests -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected: все ledger-тесты PASS; formatting/diff-check без ошибок.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/deferred_input.rs src/daemon/mod.rs
git commit -m "feat: add deferred physical input ledger"
```

### Task 2: Per-request soft cancel под terminal gate

**Files:**

- Modify: `src/error/mod.rs`
- Modify: `src/daemon/keyboard.rs`

- [ ] **Step 1: Написать RED-тесты cancel/completion linearization**

Добавить в keyboard tests:

```rust
#[test]
fn soft_cancel_wins_before_next_mutation_permit() {
    let control =
        WriterTransactionControl::with_timeout_for_test(201, Duration::from_secs(1));
    assert_eq!(
        control.request_soft_cancel(),
        WriterSoftCancelRequest::Requested
    );
    assert!(matches!(
        control.authorize_mutation_start(),
        Err(SwitcherError::VirtualKeyboardWriterTransactionCancelled {
            request_id: 201
        })
    ));
    assert_eq!(
        control.request_soft_cancel(),
        WriterSoftCancelRequest::AlreadyRequested
    );
}

#[test]
fn completed_publication_wins_before_late_soft_cancel() {
    let control =
        WriterTransactionControl::with_timeout_for_test(202, Duration::from_secs(1));
    assert!(control.publish_completed());
    assert_eq!(
        control.request_soft_cancel(),
        WriterSoftCancelRequest::AlreadyCompleted
    );
}

#[test]
fn soft_cancelled_publication_completes_without_global_writer_failure() {
    let control =
        WriterTransactionControl::with_timeout_for_test(203, Duration::from_secs(1));
    assert_eq!(
        control.request_soft_cancel(),
        WriterSoftCancelRequest::Requested
    );
    let mut published = false;
    assert_eq!(
        control.publish_soft_cancelled_with(|| {
            published = true;
            true
        }),
        WriterCompletionPublication::Completed
    );
    assert!(published);
    assert_eq!(control.state(), WriterTransactionState::Completed);
    assert_eq!(control.failure_request_id.load(Ordering::SeqCst), 0);
}
```

- [ ] **Step 2: Наблюсти RED**

Run:

```bash
cargo test --locked --lib soft_cancel -- --nocapture
```

Expected: compile FAIL — отсутствуют cancel flag, outcome и typed error.

- [ ] **Step 3: Добавить typed error и control API**

В `SwitcherError`:

```rust
#[error("Virtual keyboard writer transaction {request_id} was cancelled")]
VirtualKeyboardWriterTransactionCancelled { request_id: u64 },

#[error("Deferred physical input reached emergency capacity {limit}")]
DeferredInputCapacityExceeded { limit: usize },
```

В `WriterTransactionControl` добавить:

```rust
soft_cancel_requested: Arc<AtomicBool>,
```

Все constructors создают/клонируют этот флаг. Добавить:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterSoftCancelRequest {
    Requested,
    AlreadyRequested,
    AlreadyCompleted,
    Terminal,
}
```

`request_soft_cancel()` берёт `terminal_gate` и в следующем порядке проверяет
global stop, failure id, transaction state и deadline. Только живой `Pending`
может установить cancel flag. `ensure_active_while_terminal_gate_is_held()` и
`authorize_mutation_start()` проверяют флаг после глобальных terminal причин.

Добавить `authorize_cleanup_mutation_start()`: он использует тот же gate и
проверяет stop/failure/deadline/state, но намеренно игнорирует soft cancel.

`publish_soft_cancelled_with()` разрешает публикацию только для
`Pending + soft_cancel_requested`, отправляет payload под gate и переводит
state в `Completed`. Timeout/stop/failure по-прежнему возвращают
`Cancelled` и не отправляют нефатальный ACK.

- [ ] **Step 4: Запустить GREEN и существующие transaction races**

Run:

```bash
cargo test --locked --lib soft_cancel -- --nocapture
cargo test --locked --lib transaction_terminal -- --nocapture
cargo test --locked --lib deferred_poll -- --nocapture
```

Expected: новые и существующие gate/race tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/error/mod.rs src/daemon/keyboard.rs
git commit -m "feat: add per-request writer cancellation"
```

### Task 3: Exception-safe cleanup и нефатальный writer outcome

**Files:**

- Modify: `src/daemon/keyboard.rs`

- [ ] **Step 1: Написать RED-тесты cleanup и продолжения writer**

Использовать уже определённые в модуле `FakeUinputStrokeSink` и
`FakeCinnamonX11XtestReplay`:

```rust
#[test]
fn soft_cancel_after_modifier_release_normalizes_to_frozen_state() {
    let control =
        WriterTransactionControl::with_timeout_for_test(211, Duration::from_secs(1));
    let frozen = ModifierState {
        left_ctrl: true,
        left_shift: true,
        ..ModifierState::default()
    };
    let mut sink = FakeUinputStrokeSink::default();
    control.request_soft_cancel();
    normalize_uinput_modifiers_after_soft_cancel(&mut sink, frozen, &control).unwrap();
    assert!(sink.events.ends_with(&[
        "key:KEY_LEFTCTRL:1".to_string(),
        "key:KEY_LEFTSHIFT:1".to_string(),
        "sync".to_string(),
    ]));
}

#[test]
fn cancelled_manual_completion_is_published_as_nonfatal() {
    let control =
        WriterTransactionControl::with_timeout_for_test(220, Duration::from_secs(1));
    let (completion_tx, completion_rx) = mpsc::channel();
    assert_eq!(
        control.request_soft_cancel(),
        WriterSoftCancelRequest::Requested
    );
    let completion = ManualCurrentWordCompletion {
        request_id: 220,
        outcome: ManualCurrentWordOutcome::Cancelled,
    };
    assert!(publish_deferred_manual_completion(
        &control,
        &completion_tx,
        completion.clone(),
        None,
    )
    .is_ok());
    assert_eq!(completion_rx.recv().unwrap(), completion);
    assert_eq!(control.failure_request_id.load(Ordering::SeqCst), 0);
}
```

Добавить аналогичный XTest test, cancel между temporary Shift down/up и
cleanup-error test, который доказывает terminal `FailedAfterMutation`.

- [ ] **Step 2: Наблюсти RED**

Run:

```bash
cargo test --locked --lib soft_cancel_after_modifier -- --nocapture
cargo test --locked --lib cancelled_manual_completion -- --nocapture
```

Expected: compile FAIL — нет normalization helper и cancelled outcome.

- [ ] **Step 3: Реализовать cleanup и outcome**

Расширить:

```rust
pub enum ManualCurrentWordOutcome {
    Succeeded(CorrectionPlan),
    Cancelled,
    FailedAfterMutation(String),
}
```

Добавить uinput/XTest normalization:

```text
release all Left/Right Shift/Ctrl/Alt/Meta
sync
press exactly frozen modifiers
sync
```

Каждая новая cleanup-мутация получает permit через
`authorize_cleanup_mutation_start()`. Release-фаза пытается отпустить все
клавиши и сохраняет первую backend-ошибку. Ошибка restore отпускает уже
восстановленный partial set best-effort.

В uinput и Cinnamon XTest ветвях `run_correction()` перехватывать только
`VirtualKeyboardWriterTransactionCancelled` данного request, выполнять
normalization и возвращать тот же soft-cancel. Другие ошибки не
переклассифицировать.

В deferred writer dispatch:

```rust
let outcome = match result {
    Ok(_) => ManualCurrentWordOutcome::Succeeded(plan),
    Err(SwitcherError::VirtualKeyboardWriterTransactionCancelled {
        request_id: cancelled_id,
    }) if cancelled_id == request_id => ManualCurrentWordOutcome::Cancelled,
    Err(error) => ManualCurrentWordOutcome::FailedAfterMutation(error.to_string()),
};
```

`publish_deferred_manual_completion()` использует
`publish_soft_cancelled_with()` для `Cancelled` и не возвращает writer error
после успешной публикации. Cleanup error сохраняет прежний fatal путь.

В `VirtualKeyboardWriter` и `KeyboardController` добавить
`request_manual_current_word_cancel(request_id)`, который обращается к control
только совпадающего pending request и возвращает enum outcome без постановки
новой команды в заполненную data queue.

- [ ] **Step 4: Запустить GREEN и полную keyboard-матрицу**

Run:

```bash
cargo test --locked --lib soft_cancel -- --nocapture
cargo test --locked --lib manual_current_word -- --nocapture
cargo test --locked --lib modifier_cleanup -- --nocapture
cargo test --locked --lib daemon::keyboard::tests
```

Expected: PASS; cancelled request не устанавливает failure id и тот же writer
обрабатывает следующую команду.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: cleanly cancel deferred manual correction"
```

### Task 4: Service phase machine без уничтожения ledger

**Files:**

- Modify: `src/daemon/service.rs`

- [ ] **Step 1: Заменить discard-тесты на RED conservation tests**

Добавить/переписать service tests. Рядом с ними определить локальные
`session(request_id)` и `sequenced_event(sequence_id, key, value)` из уже
существующих полей `DeferredManualCurrentWordSession` и `DeferredInputEvent`:

```rust
#[test]
fn overflow_event_is_retained_and_requests_cancel_without_reset() {
    let mut flow = ManualCurrentWordFlow::InFlight {
        session: session(301),
    };
    let mut ledger = DeferredInputLedger::default();
    let mut cancel_requests = Vec::new();
    for sequence_id in 1..=257 {
        let admission =
            ledger.admit(sequenced_event(sequence_id, Key::KEY_A, 1));
        if admission == DeferredAdmission::RequestCancellation {
            if let Some(request_id) =
                promote_in_flight_to_cancel_requested(&mut flow, "soft-limit")
            {
                cancel_requests.push(request_id);
            }
        }
    }
    assert!(matches!(
        flow,
        ManualCurrentWordFlow::CancelRequested { .. }
    ));
    assert_eq!(
        ledger.sequence_ids_for_test(),
        (1..=257).collect::<Vec<_>>()
    );
    assert_eq!(cancel_requests, vec![301]);
}

#[test]
fn invalidation_while_draining_preserves_alt_tab_releases() {
    let mut flow = ManualCurrentWordFlow::DrainingDeferredInput {
        session: session(302),
    };
    let mut ledger = DeferredInputLedger::default();
    for event in [
        sequenced_event(1, Key::KEY_LEFTALT, 1),
        sequenced_event(2, Key::KEY_TAB, 1),
        sequenced_event(3, Key::KEY_TAB, 0),
        sequenced_event(4, Key::KEY_LEFTALT, 0),
    ] {
        ledger.admit(event);
    }
    let mut forwarded = Vec::new();
    while !ledger.is_empty() {
        drain_deferred_head_with(&mut ledger, |event| {
            if event.sequence_id == 2 {
                assert_eq!(
                    invalidate_manual_flow_context(&mut flow),
                    ManualFlowInvalidation::ContextOnly
                );
            }
            forwarded.push((event.key, event.value));
            Ok::<_, ()>(())
        })
        .unwrap();
    }
    assert_eq!(
        forwarded,
        [
            (Key::KEY_LEFTALT, 1),
            (Key::KEY_TAB, 1),
            (Key::KEY_TAB, 0),
            (Key::KEY_LEFTALT, 0),
        ]
    );
}

#[test]
fn failed_replay_keeps_head_for_terminal_reconciliation() {
    let mut ledger = DeferredInputLedger::default();
    ledger.admit(sequenced_event(7, Key::KEY_A, 1));
    ledger.admit(sequenced_event(8, Key::KEY_A, 0));
    assert_eq!(
        drain_deferred_head_with(&mut ledger, |_| Err::<(), _>("writer-dead")),
        Err("writer-dead")
    );
    assert_eq!(ledger.sequence_ids_for_test(), vec![7, 8]);
}
```

Добавить тест late-success-after-invalidation и reconfigured-hotkey tail
preservation. `sequence_ids_for_test()` компилируется только под `cfg(test)`;
production logging использует только counts. Не создавать production fake
backend.

- [ ] **Step 2: Наблюсти RED**

Run:

```bash
cargo test --locked --lib deferred_ -- --nocapture
cargo test --locked --lib manual_current_word -- --nocapture
```

Expected: старые discard assertions либо новые conservation tests FAIL.

- [ ] **Step 3: Разделить phase и ledger**

В `DaemonService` добавить:

```rust
deferred_input: DeferredInputLedger,
next_physical_sequence_id: u64,
```

Убрать `deferred_input` из `DeferredManualCurrentWordSession`, добавить
`context_invalidated: bool`, а flow расширить:

```rust
enum ManualCurrentWordFlow {
    Idle,
    InFlight { session: DeferredManualCurrentWordSession },
    CancelRequested { session: DeferredManualCurrentWordSession },
    DrainingDeferredInput { session: DeferredManualCurrentWordSession },
}
```

Физическое событие получает nonzero sequence id сразу после fetch и передаётся
в `handle_key_event(DeferredInputEvent, InputOrigin)`.

`enqueue_deferred_physical_input_event()`:

- переносит ownership события в ledger;
- на `RequestCancellation` переводит только `InFlight -> CancelRequested`;
- вызывает controller cancel ровно один раз;
- на `CapacityExceeded` возвращает typed terminal error, оставляя текущее
  событие владельцу fetched batch.

`handle_non_key_invalidation()`:

- `Idle`: прежний `invalidate_word_context()`;
- `InFlight`: word invalidation + soft cancel + сохранение ledger;
- `CancelRequested`: только повторная word invalidation;
- `Draining`: только word invalidation, без abort.

Чистые переходы имеют явные типы:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManualFlowInvalidation {
    Idle,
    RequestCancellation { request_id: u64 },
    ContextOnly,
}

fn promote_in_flight_to_cancel_requested(
    flow: &mut ManualCurrentWordFlow,
    _reason: &str,
) -> Option<u64> {
    let previous = std::mem::replace(flow, ManualCurrentWordFlow::Idle);
    match previous {
        ManualCurrentWordFlow::InFlight { mut session } => {
            session.context_invalidated = true;
            let request_id = session.request_id;
            *flow = ManualCurrentWordFlow::CancelRequested { session };
            Some(request_id)
        }
        other => {
            *flow = other;
            None
        }
    }
}

fn invalidate_manual_flow_context(
    flow: &mut ManualCurrentWordFlow,
) -> ManualFlowInvalidation {
    if matches!(&*flow, ManualCurrentWordFlow::InFlight { .. }) {
        let request_id =
            promote_in_flight_to_cancel_requested(flow, "context-invalidation")
                .expect("in-flight flow must expose its request");
        return ManualFlowInvalidation::RequestCancellation { request_id };
    }

    match flow {
        ManualCurrentWordFlow::Idle => ManualFlowInvalidation::Idle,
        ManualCurrentWordFlow::CancelRequested { session }
        | ManualCurrentWordFlow::DrainingDeferredInput { session } => {
            session.context_invalidated = true;
            ManualFlowInvalidation::ContextOnly
        }
        ManualCurrentWordFlow::InFlight { .. } => {
            unreachable!("in-flight flow was handled before the match")
        }
    }
}
```

Service после результата `RequestCancellation` вызывает controller API;
`Idle` выполняет обычный `invalidate_word_context()`, а все active outcomes
также очищают только word context без полного transient reset.

`handle_manual_current_word_completion()` принимает completion и из
`InFlight`, и из `CancelRequested`. `Cancelled` всегда ведёт в `Draining`.
Success после invalidation не восстанавливает word context.

`drain_one_deferred_input_event()` клонирует `peek`, вызывает handler и только
после `Ok` делает `acknowledge(sequence_id)`. Empty drain вызывает
`finish_drained()` и только затем `Idle`/retry.

Для этого добавить общий helper, который используется production-методом и
приведёнными тестами:

```rust
fn drain_deferred_head_with<E>(
    ledger: &mut DeferredInputLedger,
    handle: impl FnOnce(DeferredInputEvent) -> Result<(), E>,
) -> Result<(), E> {
    let Some(event) = ledger.peek().copied() else {
        return Ok(());
    };
    handle(event)?;
    ledger
        .acknowledge(event.sequence_id)
        .expect("peeked deferred head must still be current");
    Ok(())
}
```

`promote_in_flight_to_cancel_requested()` и
`invalidate_manual_flow_context()` являются чистыми переходами над
`ManualCurrentWordFlow`; controller side effect выполняется service-методом
только по возвращённому `request_id`.

Во время `DeferredReplay` совпавшая новая manual hotkey ставит
`retry_after_drain_requested`, подавляет собственный lifecycle как раньше и не
вызывает `begin_*`, пока ledger не пуст.

- [ ] **Step 4: Запустить GREEN и функциональные регрессии**

Run:

```bash
cargo test --locked --lib deferred_ -- --nocapture
cargo test --locked --lib manual_current_word -- --nocapture
cargo test --locked --lib wayland_focus_switch -- --nocapture
cargo test --locked --lib pointer -- --nocapture
cargo test --locked --lib switch_logic::tests
```

Expected: conservation/Alt+Tab tests PASS; обычные correction tests не
изменились.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/service.rs
git commit -m "fix: preserve deferred input across cancellation"
```

### Task 5: Сохранить fetched batch и явно reconcile terminal tail

**Files:**

- Modify: `src/daemon/service.rs`
- Modify: `src/daemon/deferred_input.rs`

- [ ] **Step 1: Написать RED-тесты post-fetch и between-events tail**

```rust
fn sequenced_event(sequence_id: u64) -> DeferredInputEvent {
    DeferredInputEvent {
        sequence_id,
        key: Key::KEY_A,
        value: 1,
        timestamp: SystemTime::UNIX_EPOCH,
    }
}

#[test]
fn post_fetch_health_failure_returns_the_whole_accepted_tail() {
    let mut batch =
        VecDeque::from([sequenced_event(401), sequenced_event(402)]);
    let result = process_writer_healthy_batch(
        &mut (),
        &mut batch,
        |_| Err("writer-dead"),
        |_, _| Ok(()),
    );
    assert!(matches!(result, Err(WriterHealthyBatchError::Health(_))));
    assert_eq!(
        batch.iter().map(|event| event.sequence_id).collect::<Vec<_>>(),
        vec![401, 402]
    );
}

#[test]
fn health_failure_between_events_keeps_only_unacknowledged_tail() {
    let mut checks = 0;
    let mut batch = VecDeque::from([
        sequenced_event(411),
        sequenced_event(412),
        sequenced_event(413),
    ]);
    let result = process_writer_healthy_batch(
        &mut (),
        &mut batch,
        |_| {
            checks += 1;
            (checks < 2).then_some(()).ok_or("writer-dead")
        },
        |_, _| Ok(()),
    );
    assert!(matches!(result, Err(WriterHealthyBatchError::Health(_))));
    assert_eq!(
        batch.iter().map(|event| event.sequence_id).collect::<Vec<_>>(),
        vec![412, 413]
    );
}
```

Существующий тест `writer_health_failure_after_successful_fetch_discards_events`
переименовать и инвертировать: accepted tail должен попасть в reconciliation
summary, а не исчезнуть.

- [ ] **Step 2: Наблюсти RED**

Run:

```bash
cargo test --locked --lib writer_health_failure_after -- --nocapture
cargo test --locked --lib health_failure_between_events -- --nocapture
```

Expected: FAIL, потому что helper потребляет iterator и post-fetch output
теряется.

- [ ] **Step 3: Перестроить ownership без изменения health policy**

В event loop:

1. выполнить pre-fetch health;
2. прочитать raw events;
3. если read успешен, немедленно превратить все `InputEventKind::Key` в
   `VecDeque<DeferredInputEvent>` с sequence ids;
4. выполнить post-fetch health;
5. при health error сформировать безопасный terminal-tail report и только
   затем вызвать существующий runtime failure routing;
6. при read error сохранить прежний приоритет post-health error.

`process_writer_healthy_batch()` принимает `&mut VecDeque<DeferredInputEvent>`,
берёт `front().copied()`, проверяет health, вызывает handler и делает
`pop_front()` только после `Ok`.

Добавить:

```rust
fn reconcile_fetched_tail(
    events: &mut VecDeque<DeferredInputEvent>,
    reason: &'static str,
) -> InputTailReconciliation
```

Report внутри процесса сохраняет sequence ids для тестового oracle. Production
log содержит только количество и reason; key codes, значения и sequence ids не
логируются. При ненулевом terminal tail выполнить одну краткую `eprintln!`
запись для journal и расширенную debug-запись без чувствительных данных.

В `drop_active_input_backend()` после фактического writer shutdown вызвать
`reconcile_deferred_input_after_backend_shutdown(outcome)`. Он переводит
оставшийся ledger в reconciled, логирует только counts/request/generation и
ставит phase `Idle`. Последующий `reset_transient_input_state()` видит пустой
ledger и не уничтожает данные.

- [ ] **Step 4: Запустить GREEN и recovery/shutdown tests**

Run:

```bash
cargo test --locked --lib writer_health_failure_after -- --nocapture
cargo test --locked --lib health_failure_between_events -- --nocapture
cargo test --locked --lib deferred_input_worker_failure -- --nocapture
cargo test --locked --lib runtime_recovery -- --nocapture
cargo test --locked --lib writer_stop -- --nocapture
```

Expected: batch tails сохранены до report; release-first shutdown и sticky
unresponsive writer tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/deferred_input.rs src/daemon/service.rs
git commit -m "fix: reconcile accepted input on backend failure"
```

### Task 6: Полная локальная проверка и независимое review

**Files:**

- Modify only if review finds a concrete defect:
  `src/daemon/deferred_input.rs`, `src/daemon/keyboard.rs`,
  `src/daemon/service.rs`, `src/error/mod.rs`

- [ ] **Step 1: Проверить формат, diff и отсутствие scope drift**

Run:

```bash
cargo fmt --all -- --check
git diff --check
git diff 217c12e -- src/daemon/deferred_input.rs src/daemon/keyboard.rs src/daemon/service.rs src/error/mod.rs
rg -n "unsafe" src/daemon/deferred_input.rs src/daemon/keyboard.rs src/daemon/service.rs
rg -n "MAX_DEFERRED|layout_delay_ms|typing_ms|backspace_ms|WRITER_HEALTH_POLL_QUANTUM" src/daemon
```

Expected: format/diff clean; no new unsafe; tuned timing constants unchanged.

- [ ] **Step 2: Запустить обе полные безопасные Rust-матрицы**

Run outside the restricted socket sandbox when required:

```bash
cargo test --locked --lib
cargo test --locked --features settings-ui --lib -j1
cargo test --locked --test dbus_api
```

Expected: все тесты PASS. Никакой тест не открывает реальные host input/uinput
devices и не меняет пользовательскую сессию.

- [ ] **Step 3: Выполнить два review-pass**

Pass A проверяет spec coverage:

- каждое состояние/переход;
- 257-е событие;
- cancel/completion/timeout/stop;
- uinput и XTest cleanup;
- peek-before-ack;
- fetched batch tails;
- все reset/recovery/shutdown пути.

Pass B ищет регрессии:

- duplicate/lost/reordered press/release;
- stale replay после backend generation change;
- modifier normalization;
- новый deadlock terminal gate;
- writer, ошибочно убитый мягкой отменой;
- sensitive logging;
- изменение обычного fast path.

Каждое конкретное замечание сначала воспроизвести тестом, затем исправить
минимально и повторить Step 1–2.

- [ ] **Step 4: Commit review fixes при их наличии**

```bash
git add src/daemon/deferred_input.rs src/daemon/keyboard.rs src/daemon/service.rs src/error/mod.rs
git commit -m "fix: resolve deferred input conservation review"
```

Если исправлений нет, пустой коммит не создавать.

### Task 7: Canonical DEB и VM runtime validation

**Files:**

- Create: `docs/audits/2026-07-23-deferred-input-conservation-validation.md`
- Generated, not committed unless project policy already tracks it:
  `dist/packages/open-switcher_*.deb`

- [ ] **Step 1: Собрать canonical Debian package**

Run:

```bash
./manage.sh package deb
sha256sum dist/packages/open-switcher_*_amd64.deb
```

Expected: package build/desktop validation PASS; записаны точный путь и hash.

- [ ] **Step 2: Установить package в сохранённые VM-профили**

В Mint Cinnamon X11 и Ubuntu GNOME Wayland использовать только гостевые
управляющие каналы. Подтвердить package version, executable hash и PID.
Host systemd, udev, ACL, input, clipboard, layout и сеть не менять.

- [ ] **Step 3: Выполнить обычную и overlap-матрицу**

В обоих профилях проверить:

- F12 current/previous word;
- auto correction;
- accidental Caps Lock и two capitals;
- layout switch;
- Enter/Tab/Space;
- movement/scroll/touch без invalidation;
- physical click с invalidation;
- F12 + немедленное продолжение печати;
- F12 + click в другое окно;
- F12 + Alt+Tab;
- удерживаемые Ctrl/Shift/Alt на границе отмены.

Для воспроизводимого `InFlight` разрешено временно увеличить только гостевые
валидные correction delays. В VM эти значения можно не восстанавливать, если
они не нужны для результата; package/host не меняются.

- [ ] **Step 4: Выполнить bounded fault checks**

Через fake/diagnostic guest seam проверить soft overflow и terminal writer
failure. Независимый bounded grab probe должен подтвердить release физического
гостевого устройства; старый virtual device исчезает, а хвост не появляется в
новом окне. При soft cancel PID/writer/device остаются прежними и принимают
следующий ввод.

- [ ] **Step 5: Записать validation report**

Документ должен содержать:

- commit/package/hash/environment;
- точные команды и результаты;
- RED/GREEN evidence;
- ordinary и overlap matrix;
- reconciliation counts без key data;
- ограничения `SIGKILL`/power loss/kernel loss;
- отдельный остаточный риск operation-wide synthetic ACK.

- [ ] **Step 6: Commit validation report**

```bash
git add -f docs/audits/2026-07-23-deferred-input-conservation-validation.md
git commit -m "docs: validate deferred input conservation"
```

Лабораторию не удалять и не перестраивать без необходимости. Удаление возможно
только по прямой просьбе пользователя.
