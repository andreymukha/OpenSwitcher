# План реализации H-06: fail-safe жизненный цикл синтетического ввода

> **Для агентных исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОДНАВЫК: используйте
> `superpowers:subagent-driven-development` (рекомендуется) либо
> `superpowers:executing-plans` и выполняйте план по задачам. Для отметки
> прогресса используются чекбоксы `- [ ]`.

**Цель:** сделать все production-пути синтетического ввода доказуемо
завершаемыми, а Cinnamon/X11 XTEST — устойчивым к одиночной гибели daemon или
guardian без удержания физического `EVIOCGRAB`.

**Архитектура:** backend-neutral `SyntheticKeyLedger` и
`SyntheticOperation` отвечают за temporary synthetic debt. XTEST выполняется
единственным socket-activated guardian в отдельной user-service/cgroup; daemon
держит зеркальный debt и заранее проверенное emergency X11-соединение. Состояние
восстановленных физических модификаторов живёт в отдельном session ledger до
подтверждённого writer write+sync физического release.

**Технологии:** Rust 1.95, `evdev`, `uinput`, `x11rb` XKB/XTEST,
`AF_UNIX/SOCK_SEQPACKET`, `systemd --user`, Debian packaging, shell tests,
сохранённые VM Linux Mint Cinnamon/X11 и Ubuntu GNOME/Wayland.

---

## Зафиксированные границы

- Связанная спецификация:
  `docs/superpowers/specs/2026-07-28-h06-synthetic-input-failsafe-design.md`
  (`a36ab39`).
- Рабочая ветка: `fix/h06-synthetic-input-ledger`.
- Основной артефакт — установленный Debian package, а не `cargo run`.
- Не меняются алгоритмы F12/автокоррекции/Caps Lock/двух заглавных, значения
  `typing_ms`, `backspace_ms`, `layout_delay_ms`, X11 polling/focus/barrier и
  clipboard flow.
- На хосте разрешены только fake/unit/process tests, не открывающие X11,
  `/dev/input`, `/dev/uinput` и не меняющие clipboard/layout/systemd/udev/ACL.
- Реальная инъекция, убийство процессов после XTEST down и package lifecycle
  выполняются только внутри двух сохранённых VM. Лаборатория после работы не
  удаляется.
- Любой ambiguous down запрещает повтор down. Hard failure делает только
  release-only reconciliation, освобождает physical grab и завершает текущий
  процесс.
- В обычных логах допускаются operation/session id и количества, но не текст,
  полный key trace и сырой IPC payload.

## Карта файлов и обязанностей

### Новые Rust-файлы

- `src/daemon/synthetic_input.rs` — общий ledger, operation guard,
  session-scoped modifier ledger, terminal proof и conformance tests.
- `src/daemon/uinput_synthetic.rs` — единственный uinput adapter; он не
  раздувает уже крупный `keyboard.rs` backend-деталями.
- `src/daemon/xtest_guardian/mod.rs` — константы, экспорт client/service и
  безопасная точка входа hidden mode.
- `src/daemon/xtest_guardian/protocol.rs` — ручной bounded wire codec и
  protocol state/sequence validation.
- `src/daemon/xtest_guardian/seqpacket.rs` — `SOCK_SEQPACKET`, socket
  activation, `SO_PEERCRED`/`SCM_CREDENTIALS` и проверка inode packaged binary.
- `src/daemon/xtest_guardian/x11.rs` — authoritative XTEST executor, X11 epoch
  marker и release-only emergency connection.
- `src/daemon/xtest_guardian/service.rs` — guardian session loop,
  authoritative ledgers, EOF/SIGTERM drain.
- `src/daemon/xtest_guardian/client.rs` — daemon-side broker, mirrored ledgers,
  deadlines и emergency fail-stop.
- `src/daemon/xtest_guardian/process_tests.rs` — `cfg(test)` multi-process
  fixtures с fake executor; ни одного production fault API.
- `src/error/input_safety_error.rs` — typed protocol/reconciliation errors без
  чувствительных payload.
- `examples/h06_x11_vm_probe.rs` — узкий VM-only XInput/XQueryKeymap probe,
  который может завершить заданный guest PID на нужном synthetic press.

### Изменяемые Rust-файлы

- `Cargo.toml`, `Cargo.lock` — только необходимые feature `nix`; новых
  сериализаторов protocol не добавлять.
- `src/main.rs` — ровно один скрытый аргумент
  `--internal-xtest-guardian-v1`; остальные аргументы отклоняются.
- `src/daemon/mod.rs` — модули и корректное сохранение terminal postmortem.
- `src/daemon/keyboard.rs` — uinput adapter, Cinnamon XKB controller,
  guardian client, physical event identity, writer readiness/health/shutdown.
- `src/daemon/layout_switcher.rs` — uinput layout combo через общий ledger,
  при сохранении отдельной XKB mutation boundary.
- `src/daemon/service.rs` — не терять `sequence_id` при fast/deferred
  forwarding и освобождать grab до ожидания emergency cleanup.
- `src/daemon/input_backend.rs` — guardian/reconciliation failure никогда не
  считается recoverable in-process reopen.
- `src/error/mod.rs` — `SwitcherError::InputSafety`.

### Packaging и тесты

- Создать:
  `debian/open-switcher.open-switcher-xtest-guardian.user.socket`,
  `debian/open-switcher.open-switcher-xtest-guardian.user.service`,
  `dist/systemd/open-switcher-xtest-guardian.socket`,
  `dist/systemd/open-switcher-xtest-guardian.service`,
  `debian/open-switcher.preinst`.
- Изменить:
  `debian/open-switcher.open-switcher-daemon.user.service`,
  `dist/systemd/open-switcher-daemon.service`, `debian/rules`,
  `debian/open-switcher.prerm`,
  `debian/scripts/open-switcher-user-session-stop`,
  `debian/scripts/open-switcher-user-session-start`,
  `debian/open-switcher.postinst`, `manage.sh`,
  `tests/debian_package_scripts_test.sh`,
  `tests/manage_package_deb_test.sh`.
- Создать итоговый отчёт:
  `docs/audits/2026-07-28-h06-runtime-validation.md`.

## Обязательный порядок интеграции

1. Сначала зафиксировать нынешние normal traces.
2. Затем реализовать и доказать общий ledger на fake backend.
3. Перевести uinput без XTEST и прогнать полную регрессию.
4. Только после этого добавить protocol/transport/guardian с fake executor.
5. Подключить реальный X11 executor последним Rust-слоем.
6. После safe host gates собрать точный DEB.
7. Сначала Mint X11 аварийная кампания, затем Ubuntu Wayland smoke.

Такой порядок оставляет после каждого коммита работающий и проверяемый продукт
и локализует любую регрессию.

### Задача 1: Зафиксировать текущие normal traces до рефакторинга

**Файлы:**

- Изменить: `src/daemon/keyboard.rs:6027-6225`
- Тест: `src/daemon/keyboard.rs` (`daemon::keyboard::tests`)

- [ ] **Шаг 1: добавить XTEST golden trace для shifted stroke**

Добавить рядом с `FakeCinnamonX11XtestReplay`:

```rust
#[test]
fn cinnamon_xtest_shifted_stroke_normal_trace_is_frozen() {
    let mut replay = FakeCinnamonX11XtestReplay::default();
    let plan = CorrectionPlan {
        buffer: vec![crate::daemon::switch_logic::Keystroke {
            key: Key::KEY_G,
            shift: true,
            caps_lock: false,
        }],
        extra_backspaces: 0,
    };

    run_cinnamon_x11_xtest_correction(
        &mut replay,
        &plan,
        &test_runtime_config_snapshot(),
        ModifierState::default(),
        None,
    )
    .unwrap();

    assert_eq!(
        replay.calls,
        [
            "prepare",
            "down:KEY_BACKSPACE",
            "up:KEY_BACKSPACE",
            "down:KEY_LEFTSHIFT",
            "down:KEY_G",
            "up:KEY_G",
            "up:KEY_LEFTSHIFT",
        ]
    );
}
```

- [ ] **Шаг 2: добавить trace удерживаемого physical Shift**

```rust
#[test]
fn cinnamon_xtest_physical_shift_release_restore_trace_is_frozen() {
    let mut replay = FakeCinnamonX11XtestReplay::default();
    let plan = CorrectionPlan {
        buffer: vec![crate::daemon::switch_logic::Keystroke {
            key: Key::KEY_G,
            shift: false,
            caps_lock: false,
        }],
        extra_backspaces: 0,
    };
    let modifiers = ModifierState {
        left_shift: true,
        ..ModifierState::default()
    };

    run_cinnamon_x11_xtest_correction(
        &mut replay,
        &plan,
        &test_runtime_config_snapshot(),
        modifiers,
        None,
    )
    .unwrap();

    assert_eq!(
        replay.calls,
        [
            "prepare",
            "up:KEY_LEFTSHIFT",
            "down:KEY_BACKSPACE",
            "up:KEY_BACKSPACE",
            "down:KEY_G",
            "up:KEY_G",
            "down:KEY_LEFTSHIFT",
        ]
    );
}
```

- [ ] **Шаг 3: выполнить только характеристические тесты**

Команда:

```bash
cargo test --locked --lib cinnamon_xtest_ -- --test-threads=1
```

Ожидается: новые тесты и существующие Cinnamon XTEST unit tests проходят;
никакое реальное устройство не открывается.

- [ ] **Шаг 4: зафиксировать baseline trace**

```bash
git add src/daemon/keyboard.rs
git commit -m "test: freeze synthetic input traces"
```

### Задача 2: Реализовать backend-neutral temporary ledger

**Файлы:**

- Создать: `src/daemon/synthetic_input.rs`
- Изменить: `src/daemon/mod.rs:1-12`
- Создать: `src/error/input_safety_error.rs`
- Изменить: `src/error/mod.rs:1-150`
- Тест: `src/daemon/synthetic_input.rs`

- [ ] **Шаг 1: написать failure-at-N tests, которые пока не компилируются**

Создать test table с точными фазами:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FailAt {
        Prepare,
        Down,
        DownSync,
        Up,
        UpSync,
        CleanupUp,
        CleanupSync,
    }

    #[test]
    fn ambiguous_down_is_recorded_before_backend_call_and_never_repeated() {
        let mut sink = FakeSink::fail_after_applying(FailAt::Down);
        let latch = SyntheticFailureLatch::default();
        let mut operation =
            SyntheticOperation::new_for_test(OperationId(41), &mut sink, latch.clone());

        let primary = operation.press(Key::KEY_A).unwrap_err();
        let proof = operation.finish_hard_failure(primary).proof;

        assert_eq!(sink.down_count(Key::KEY_A), 1);
        assert_eq!(sink.up_count(Key::KEY_A), 1);
        assert_eq!(proof, TerminalProof::Reconciled);
        assert!(!latch.is_failed());
    }

    #[test]
    fn cleanup_continues_after_first_release_error() {
        let mut sink = FakeSink::fail_cleanup_for(Key::KEY_A);
        let latch = SyntheticFailureLatch::default();
        let mut operation =
            SyntheticOperation::new_for_test(OperationId(42), &mut sink, latch.clone());

        operation.press(Key::KEY_A).unwrap();
        operation.press(Key::KEY_B).unwrap();
        let mut restored = FakeRestoredModifierTarget::default();
        let result = operation.finish_soft_cancel(&mut restored);

        assert_eq!(
            sink.release_order(),
            &[Key::KEY_B, Key::KEY_A],
        );
        assert!(matches!(
            result.proof,
            TerminalProof::Unreconciled { remaining: 1 }
        ));
        assert!(latch.is_failed());
    }
}
```

- [ ] **Шаг 2: убедиться в ожидаемом RED**

```bash
cargo test --locked --lib synthetic_input::tests -- --test-threads=1
```

Ожидается: compile failure из-за отсутствующих
`SyntheticOperation`/`SyntheticFailureLatch`/`FakeSink`.

- [ ] **Шаг 3: реализовать точные public-in-crate типы ledger**

Основной API в `src/daemon/synthetic_input.rs`:

```rust
use crate::error::SwitcherError;
use evdev::Key;
use std::fmt::Debug;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OperationId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PressId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownState {
    AttemptingDown,
    PossiblyDown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalProof {
    Reconciled,
    OwnerGenerationDestroyed { generation: u64 },
    Unreconciled { remaining: usize },
}

pub(crate) trait SyntheticKeySink {
    type Token: Clone + Debug + Eq;

    fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError>;
    fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError>;
    fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError>;
    fn synchronize(&mut self) -> Result<(), SwitcherError>;
    fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof;
}

#[derive(Clone, Debug)]
struct SyntheticDebt<T> {
    press_id: PressId,
    token: T,
    state: DownState,
}

#[derive(Clone, Debug)]
pub(crate) struct SyntheticKeyLedger<T> {
    next_press_id: u64,
    debts: Vec<SyntheticDebt<T>>,
    terminal: bool,
}

impl<T: Clone + Eq> SyntheticKeyLedger<T> {
    pub(crate) fn new() -> Self {
        Self {
            next_press_id: 1,
            debts: Vec::new(),
            terminal: false,
        }
    }

    pub(crate) fn begin_down(&mut self, token: T) -> Result<PressId, SwitcherError> {
        if self.terminal {
            return Err(SwitcherError::input_safety("mutation after terminal state"));
        }
        let press_id = PressId(self.next_press_id);
        self.next_press_id = self.next_press_id.checked_add(1)
            .ok_or_else(|| SwitcherError::input_safety("press id exhausted"))?;
        self.debts.push(SyntheticDebt {
            press_id,
            token,
            state: DownState::AttemptingDown,
        });
        Ok(press_id)
    }

    pub(crate) fn mark_possibly_down(&mut self, press_id: PressId) {
        self.debt_mut(press_id).state = DownState::PossiblyDown;
    }

    pub(crate) fn acknowledge_up(&mut self, press_id: PressId) {
        let index = self.debts.iter()
            .position(|debt| debt.press_id == press_id)
            .expect("acknowledged press must exist");
        self.debts.remove(index);
    }

    pub(crate) fn begin_terminal(&mut self) {
        self.terminal = true;
    }

    fn debt_mut(&mut self, press_id: PressId) -> &mut SyntheticDebt<T> {
        self.debts.iter_mut()
            .find(|debt| debt.press_id == press_id)
            .expect("press id must belong to this operation")
    }
}
```

`InputSafetyError` должен иметь отдельные варианты для protocol violation,
timeout и reconciliation, а `SwitcherError::input_safety()` создаёт
`InputSafetyError::Invariant`. Тексты ошибок не содержат `Key` и текст
коррекции.

- [ ] **Шаг 4: реализовать operation guard и exhaustive cleanup**

```rust
#[derive(Clone, Default)]
pub(crate) struct SyntheticFailureLatch(
    Arc<Mutex<Option<DropCleanupReport>>>,
);

impl SyntheticFailureLatch {
    pub(crate) fn fail(&self, report: DropCleanupReport) {
        let mut slot = self.0.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(report);
        }
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.0.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
}

pub(crate) struct FrozenPhysicalSnapshot {
    modifier_bits: u16,
    caps_lock_active: bool,
}

pub(crate) struct PhysicalRestorePlan {
    temporarily_released: Vec<Key>,
}

pub(crate) struct PendingTransfer<T> {
    token: Option<T>,
    release_only_fallback: Arc<Mutex<Vec<T>>>,
}

pub(crate) trait RestoredModifierTarget<T> {
    fn adopt_restored(
        &mut self,
        key: Key,
        transfer: PendingTransfer<T>,
    ) -> Result<(), SwitcherError>;
}

pub(crate) struct OperationControl {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

pub(crate) enum OperationOutcome {
    Success,
    SoftCancelled,
    HardFailed,
}

pub(crate) struct OperationTerminalReport {
    outcome: OperationOutcome,
    primary: Option<SwitcherError>,
    cleanup: Option<SwitcherError>,
    proof: TerminalProof,
}

pub(crate) struct DropCleanupReport {
    cleanup: Option<SwitcherError>,
    proof: TerminalProof,
}

pub(crate) struct SyntheticOperation<'a, S: SyntheticKeySink> {
    id: OperationId,
    sink: &'a mut S,
    ledger: SyntheticKeyLedger<S::Token>,
    control: OperationControl,
    frozen_physical: FrozenPhysicalSnapshot,
    restore_plan: PhysicalRestorePlan,
    failure_latch: SyntheticFailureLatch,
    terminal_report: Option<OperationTerminalReport>,
    finalized: bool,
}
```

`FrozenPhysicalSnapshot` — backend-neutral bitset восьми modifiers плюс
Caps Lock, снятый один раз до первой synthetic mutation. `OperationControl`
содержит `Instant` deadline и `Arc<AtomicBool>` cancellation token; проверки
выполняются перед/после каждого backend-вызова и interruptible wait. Production
constructor требует оба объекта. Для unit tests существует только
`new_for_test`, который явно создаёт bounded control и snapshot.

Реализовать `press()` в строгом порядке:

```text
prepare_down
ledger.begin_down(AttemptingDown)
sink.attempt_down
ledger.mark_possibly_down — независимо от Ok/Err
sink.synchronize
```

Реализовать `release()` как `attempt_up -> synchronize -> acknowledge_up`;
при любой ошибке debt остаётся. `finish_hard_failure(primary)` сначала вызывает
`begin_terminal()`, затем идёт по snapshot debt в обратном порядке, продолжает
после первой ошибки и возвращает `Unreconciled { remaining }`, если хотя бы один
up+sync не подтверждён.

Добавить три явных finalizer:

```text
finish_success(&mut impl RestoredModifierTarget<S::Token>)
finish_soft_cancel(&mut impl RestoredModifierTarget<S::Token>)
finish_hard_failure(primary: SwitcherError)
```

Каждый сначала закрывает gate обычных мутаций. После разрешённой ниже
restore-фазы он закрывает ledger terminal gate, выполняет reverse cleanup и
final `synchronize()`, затем вызывает
`sink.terminal_proof(remaining_debt)`.
Все три explicit finalizer принимают `mut self`, возвращают report и тем самым
не оставляют вызывающему коду наполовину завершённую operation.
`Success`/`SoftCancelled` публикуются только при `Reconciled`; иначе outcome
становится `HardFailed`. `OperationTerminalReport` раздельно хранит
`outcome`, `primary: Option<SwitcherError>`,
`cleanup: Option<SwitcherError>` и `proof`.
Cleanup/Drop failure записывается в `SyntheticFailureLatch`; исходная ошибка,
успешно reconciled явным hard-failure finalizer, возвращается через report и не
маскируется отдельным latch-событием.

`Drop` вызывает тот же release-only finalizer только если `finalized == false`;
ошибка и proof записываются в `SyntheticFailureLatch`, но panic из `Drop`
запрещён.

До закрытия normal gate операция отмечает каждый физически удерживаемый
modifier, для которого она отправила временный synthetic `up`, в
`PhysicalRestorePlan`. Явные `finish_success` и `finish_soft_cancel` после
запрета новых рабочих down, но до terminal proof выполняют специальную
cleanup-only фазу:

```text
для каждого modifier, held во FrozenPhysicalSnapshot и временно released:
    prepare restore token
    ledger.begin_cleanup_restore_down (только key из PhysicalRestorePlan)
    attempt_down -> synchronize
    PendingTransfer -> RestoredModifierTarget
убрать все остальные temporary synthetic keys
проверить terminal proof
```

`RestoredModifierTarget<S::Token>` — backend-neutral trait; в задаче 2 его
реализует fake, а в задаче 3 — `SessionModifierLedger`. `PendingTransfer`
гарантирует, что token либо принят session ledger, либо остаётся в release-only
cleanup: его `Drop` возвращает непринятый token в bounded
`release_only_fallback`, который finalizer обязательно дренирует до terminal
proof. Произвольный cleanup down, key которого нет одновременно во frozen
snapshot и restore plan, запрещён. Restore down остаётся wire mutation с
исходным ещё действующим `MutationDeadlineNs`; `CleanupDeadlineNs` права на down
не даёт. Если operation deadline уже истёк, soft cancel повышается до hard
failure без restore down. Hard-failure finalizer новых down не делает: он
только освобождает debt и честно возвращает остаточный terminal outcome.

- [ ] **Шаг 5: реализовать fake sink и полную table-driven matrix**

Матрица обязана перечислять `Prepare`, `DownBeforeApply`, `DownAfterApply`,
`DownSync`, `UpBeforeApply`, `UpAfterApply`, `UpSync`, cancel/timeout между
каждым переходом и две cleanup-ошибки подряд. Для каждой строки проверять:

```rust
assert!(sink.no_down_after_terminal());
assert!(sink.down_count(key) <= 1);
assert_eq!(
    result.proof == TerminalProof::Reconciled,
    sink.possibly_down().is_empty(),
);
```

Отдельно проверить, что:

- expiry/cancel между backend calls запрещает следующий normal down;
- изменение live `ModifierState` после constructor не меняет
  `FrozenPhysicalSnapshot` операции;
- `finish_success` и `finish_soft_cancel` всегда ставят `finalized=true`;
- soft cancel после temporary physical modifier up делает matching restore
  down и передаёт token fake `RestoredModifierTarget`;
- ошибка restore/transfer не публикует `SoftCancelled` и оставляет token
  доступным release-only cleanup;
- выход без явного finalizer активирует `Drop`, но обычный success никогда не
  полагается только на `Drop`;
- cleanup error переводит success/soft cancel в `HardFailed` и сохраняет
  primary/cleanup раздельно.

Ту же matrix без ветвей в ledger запустить второй раз через отдельный
`FakeThirdBackend`, у которого token имеет другую структуру и terminal proof
получается через owner-generation teardown:

```rust
#[test]
fn fake_third_backend_passes_unmodified_sink_contract() {
    run_synthetic_sink_conformance(FakeThirdBackend::new());
}
```

Ни `SyntheticKeyLedger`, ни `SyntheticOperation` не получают `match` по типу
backend. Этот test является прямым release gate архитектурной расширяемости.

- [ ] **Шаг 6: выполнить ledger tests**

```bash
cargo test --locked --lib synthetic_input::tests -- --test-threads=1
```

Ожидается: все failure-at-N tests проходят, реальный input не используется.

- [ ] **Шаг 7: зафиксировать общий ledger**

```bash
git add src/daemon/synthetic_input.rs src/daemon/mod.rs src/error/input_safety_error.rs src/error/mod.rs
git commit -m "feat: add synthetic input safety ledger"
```

### Задача 3: Добавить session-scoped modifier ledger

**Файлы:**

- Изменить: `src/daemon/synthetic_input.rs`
- Тест: `src/daemon/synthetic_input.rs`

- [ ] **Шаг 1: написать state-machine tests**

```rust
#[test]
fn physical_release_is_committed_only_after_matching_generation_and_sequence() {
    let token = FakeToken::new(Key::KEY_LEFTSHIFT, 7);
    let mut ledger = SessionModifierLedger::new(InputGeneration(19));
    ledger.adopt_restored(Key::KEY_LEFTSHIFT, token.clone()).unwrap();

    assert!(ledger.commit_physical_release(
        PhysicalReleaseCommit {
            generation: InputGeneration(18),
            sequence: PhysicalSequence(91),
            key: Key::KEY_LEFTSHIFT,
        }
    ).is_err());
    assert!(ledger.contains(Key::KEY_LEFTSHIFT));

    assert_eq!(
        ledger.commit_physical_release(PhysicalReleaseCommit {
            generation: InputGeneration(19),
            sequence: PhysicalSequence(92),
            key: Key::KEY_LEFTSHIFT,
        }).unwrap(),
        Some(token),
    );
    assert!(!ledger.contains(Key::KEY_LEFTSHIFT));
}

#[test]
fn repeated_correction_while_modifier_is_held_preserves_one_owned_debt() {
    let mut ledger = owned_shift_ledger();
    ledger.mark_temporarily_released(Key::KEY_LEFTSHIFT).unwrap();
    ledger.mark_restoring(Key::KEY_LEFTSHIFT).unwrap();
    ledger.mark_owned_down(Key::KEY_LEFTSHIFT).unwrap();

    assert_eq!(
        ledger.state(Key::KEY_LEFTSHIFT),
        Some(SessionModifierState::OwnedDown),
    );
    assert_eq!(ledger.len(), 1);
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib session_modifier -- --test-threads=1
```

Ожидается: compile failure из-за отсутствующего session ledger.

- [ ] **Шаг 3: реализовать типы и допустимые переходы**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InputGeneration(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PhysicalSequence(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionModifierState {
    OwnedDown,
    TemporarilyReleased,
    RestoringPossiblyDown,
}

pub(crate) struct PhysicalReleaseCommit {
    pub(crate) generation: InputGeneration,
    pub(crate) sequence: PhysicalSequence,
    pub(crate) key: Key,
}

pub(crate) struct SessionModifierLedger<T> {
    generation: InputGeneration,
    last_release_sequence: u64,
    entries: Vec<SessionModifierDebt<T>>,
}
```

Разрешить только:

```text
OwnedDown -> TemporarilyReleased
TemporarilyReleased -> RestoringPossiblyDown
RestoringPossiblyDown -> OwnedDown
OwnedDown -> removed после matching physical release ACK
```

`commit_physical_release` возвращает `Ok(None)`, если по key нет synthetic
debt, `Ok(Some(token))` только для matching generation и строго нового physical
release sequence, и typed terminal error для stale/mismatched commit. Любой
повторный transfer или новый press того же key до commit ACK также возвращает
typed terminal error.

Реализовать для `SessionModifierLedger` общий
`RestoredModifierTarget<UinputKeyToken/PreparedToken>`.
`SyntheticOperation::transfer(press_id)` удаляет debt из temporary ledger без
key-up только внутрь введённого в задаче 2 RAII `PendingTransfer`. Единственный
допустимый consumer — `SessionModifierLedger::adopt_restored`; `Drop`
непринятого transfer возвращает token в release-only cleanup.

- [ ] **Шаг 4: добавить тесты daemon death и lost release ACK**

Проверить:

- `OwnedDown` попадает в release-only snapshot;
- `TemporarilyReleased` не создаёт лишний key-up;
- lost ACK сохраняет `OwnedDown`;
- commit старого generation не удаляет новый debt;
- второй press того же key запрещён до commit.

- [ ] **Шаг 5: выполнить весь модуль**

```bash
cargo test --locked --lib synthetic_input::tests -- --test-threads=1
```

Ожидается: temporary и session state-machine matrices проходят.

- [ ] **Шаг 6: зафиксировать session ledger**

```bash
git add src/daemon/synthetic_input.rs
git commit -m "feat: track synthetic modifier ownership"
```

### Задача 4: Перевести uinput paths на общий ledger без изменения поведения

**Файлы:**

- Создать: `src/daemon/uinput_synthetic.rs`
- Изменить: `src/daemon/mod.rs`
- Изменить: `src/daemon/keyboard.rs:171-315`
- Изменить: `src/daemon/keyboard.rs:2208-2460`
- Изменить: `src/daemon/keyboard.rs:3450-3735`
- Изменить: `src/daemon/keyboard.rs:4450-5050`
- Изменить: `src/daemon/keyboard.rs:5157-5385`
- Изменить: `src/daemon/layout_switcher.rs:77-242`
- Тест: `src/daemon/keyboard.rs`

- [ ] **Шаг 1: написать failing conformance tests для uinput adapter**

Добавить fake raw sink и проверить точный write/sync trace:

```rust
#[test]
fn uinput_adapter_keeps_down_until_write_and_sync_release_are_acknowledged() {
    let mut raw = FakeUinputRawSink::default();
    let generation = InputGeneration(11);
    let mut sink = UinputSyntheticSink::new(&mut raw, generation);
    let latch = SyntheticFailureLatch::default();
    let mut operation =
        SyntheticOperation::new_for_test(OperationId(5), &mut sink, latch);

    let press = operation.press(Key::KEY_A).unwrap();
    operation.release(press).unwrap();
    assert_eq!(
        raw.trace,
        [
            UinputTrace::Write(Key::KEY_A, 1),
            UinputTrace::Synchronize,
            UinputTrace::Write(Key::KEY_A, 0),
            UinputTrace::Synchronize,
        ]
    );
}

#[test]
fn uinput_owner_generation_is_the_only_crash_cleanup_proof() {
    let proof = finish_destroyed_uinput_generation_for_test(
        InputGeneration(12),
        &[Key::KEY_LEFTSHIFT],
    );
    assert_eq!(
        proof,
        TerminalProof::OwnerGenerationDestroyed { generation: 12 }
    );
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib uinput_adapter_ -- --test-threads=1
```

Ожидается: compile failure из-за отсутствующего `UinputSyntheticSink`.

- [ ] **Шаг 3: создать один raw uinput adapter**

Вынести из `keyboard.rs` параллельные
`UinputStrokeSink`/`UinputShortcutSink` и заменить их в
`uinput_synthetic.rs`:

```rust
trait UinputRawSink {
    fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError>;
    fn synchronize_keys(&mut self) -> Result<(), SwitcherError>;
}

impl UinputRawSink for uinput::Device {
    fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)
            .map_err(Into::into)
    }

    fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
        self.synchronize().map_err(Into::into)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UinputKeyToken {
    generation: InputGeneration,
    token_id: u64,
    key: Key,
}

struct UinputSyntheticSink<'a> {
    raw: &'a mut dyn UinputRawSink,
    generation: InputGeneration,
    next_token_id: u64,
}

impl SyntheticKeySink for UinputSyntheticSink<'_> {
    type Token = UinputKeyToken;

    fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
        let token = UinputKeyToken {
            generation: self.generation,
            token_id: self.next_token_id,
            key,
        };
        self.next_token_id = self.next_token_id.checked_add(1)
            .ok_or_else(|| SwitcherError::input_safety("uinput token id exhausted"))?;
        Ok(token)
    }

    fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.raw.write_key(token.key, 1)
    }

    fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.raw.write_key(token.key, 0)
    }

    fn synchronize(&mut self) -> Result<(), SwitcherError> {
        self.raw.synchronize_keys()
    }

    fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof {
        if remaining_debt == 0 {
            TerminalProof::Reconciled
        } else {
            TerminalProof::Unreconciled {
                remaining: remaining_debt,
            }
        }
    }
}
```

`OwnerGenerationDestroyed` здесь намеренно не возвращается: это proof только
после фактического `Drop` owning `uinput::Device`, что отдельно проверяется в
задаче 11.

- [ ] **Шаг 4: ввести persistent `UinputSyntheticRuntime`**

`UinputSyntheticRuntime` владеет `uinput::Device`, одним
`InputGeneration`, `SessionModifierLedger<UinputKeyToken>` и строго возрастающим
token id. Generation выделяется до запуска writer thread и не переиспользует
ноль.

Его physical forwarding API:

```rust
fn forward_physical(
    &mut self,
    sequence: PhysicalSequence,
    key: Key,
    value: i32,
) -> Result<Option<UinputKeyToken>, SwitcherError> {
    self.device.write_key(key, value)?;
    self.device.synchronize_keys()?;
    if value != 0 {
        return Ok(None);
    }
    self.session_modifiers.commit_physical_release(
        PhysicalReleaseCommit {
            generation: self.generation,
            sequence,
            key,
        },
    )
}
```

Возвращаемый token позже понадобится XTEST guardian; до его подключения он
только подтверждает локальное удаление debt.

- [ ] **Шаг 5: переписать tap/stroke/shortcut через `SyntheticOperation`**

На входе каждой correction/shortcut transaction один раз преобразовать
текущий `ModifierState` в `FrozenPhysicalSnapshot`, передать исходный
transaction deadline и общий cancellation token в production constructor.
Повторное чтение live modifier state внутри операции запрещено.

Сохранить deliberate waits на прежних местах:

```rust
fn replay_synthetic_tap<S: SyntheticKeySink>(
    operation: &mut SyntheticOperation<'_, S>,
    key: Key,
    transition_wait: impl FnOnce() -> Result<(), SwitcherError>,
    final_wait: impl FnOnce() -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    let press = operation.press(key)?;
    let wait_result = transition_wait();
    let release_result = operation.release(press);
    wait_result?;
    release_result?;
    final_wait()
}
```

- обычный tap сохраняет `down -> sync -> 2 ms -> up -> sync -> typing/backspace`;
- shifted stroke сохраняет `Shift down -> sync -> 1 ms -> key tap -> Shift up`;
- shortcut сохраняет нынешний `LAYOUT_SWITCH_DELAY_MS`;
- layout combo остаётся за `UinputLayoutSwitcher`, но его key mutations идут
  через тот же operation/sink;
- restored physical modifiers перед success переводятся через
  `PendingTransfer` в persistent session ledger.

- [ ] **Шаг 6: заменить fast/deferred physical forwarding**

Добавить `sequence: PhysicalSequence` в:

```rust
WriterFastCommand::ForwardEvent
WriterTransactionKind::ForwardDeferredEvent
KeyboardController::forward_event
KeyboardController::forward_deferred_event
```

Writer обязан выполнить `write -> synchronize -> local commit` внутри одного
последовательного command dispatch. Следующая mutation того же key не может
начаться раньше возврата этого dispatch.

- [ ] **Шаг 7: прогнать targeted uinput и trace tests**

```bash
cargo test --locked --lib uinput_ -- --test-threads=1
cargo test --locked --lib shortcut_ -- --test-threads=1
cargo test --locked --lib correction_ -- --test-threads=1
```

Ожидается: trace tests сохраняют прежний порядок и задержки; failure tests
возвращают `Reconciled` либо `OwnerGenerationDestroyed`, но не ложный success.

- [ ] **Шаг 8: прогнать всю библиотеку вне ограниченной sandbox**

```bash
cargo test --locked --lib -- --test-threads=1
```

Ожидается: не менее baseline `711` tests и все новые tests проходят. Если
`input_target_stop_signal_wakes_idle_waiter` зависает только в restricted
sandbox, повторить точную команду вне неё и сохранить оба результата; timeout
не считать кодовым PASS.

- [ ] **Шаг 9: зафиксировать uinput migration**

```bash
git add src/daemon/uinput_synthetic.rs src/daemon/mod.rs \
  src/daemon/keyboard.rs src/daemon/layout_switcher.rs
git commit -m "refactor: route uinput through safety ledger"
```

### Задача 5: Реализовать bounded versioned protocol

**Файлы:**

- Создать: `src/daemon/xtest_guardian/mod.rs`
- Создать: `src/daemon/xtest_guardian/protocol.rs`
- Изменить: `src/daemon/mod.rs:1-12`
- Тест: `src/daemon/xtest_guardian/protocol.rs`

- [ ] **Шаг 1: написать codec и ordering tests**

Обязательные tests:

```rust
#[test]
fn codec_round_trip_preserves_every_v1_message() {
    for message in all_v1_test_messages() {
        let frame = encode_frame(Sequence(7), &message).unwrap();
        assert!(frame.len() <= MAX_FRAME_BYTES);
        assert_eq!(decode_frame(&frame).unwrap().message, message);
    }
}

#[test]
fn parser_rejects_oversize_unknown_version_and_trailing_bytes() {
    assert!(decode_frame(&vec![0; MAX_FRAME_BYTES + 1]).is_err());
    assert!(decode_frame(&frame_with_version(2)).is_err());
    assert!(decode_frame(&frame_with_trailing_byte()).is_err());
}

#[test]
fn protocol_state_rejects_stale_sequence_operation_and_epoch() {
    let mut state = ProtocolState::ready(test_session());
    assert!(state.accept(request(Sequence(4), OperationId(9))).is_ok());
    assert!(state.accept(request(Sequence(4), OperationId(9))).is_err());
    assert!(state.accept(request(Sequence(5), OperationId(8))).is_err());
    assert!(state.accept(request_with_stale_epoch(Sequence(5))).is_err());
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib xtest_guardian::protocol::tests -- --test-threads=1
```

Ожидается: compile failure, protocol ещё отсутствует.

- [ ] **Шаг 3: реализовать фиксированный wire header**

Не использовать `serde`, `bincode` или length-prefixed allocation:

```rust
const MAGIC: [u8; 4] = *b"OSXG";
pub(crate) const PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_FRAME_BYTES: usize = 128;
pub(crate) const MAX_PREPARED_TOKENS: usize = 512;
pub(crate) const MAX_ACTIVE_DEBTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Sequence(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionId(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ServerEpoch(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MutationDeadlineNs(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CleanupDeadlineNs(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReleaseDeadline {
    Mutation(MutationDeadlineNs),
    Cleanup(CleanupDeadlineNs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PreparedToken {
    pub(crate) session: SessionId,
    pub(crate) epoch: ServerEpoch,
    pub(crate) token_id: u64,
    pub(crate) evdev_code: u16,
    pub(crate) x11_keycode: u8,
}
```

Header содержит только `magic`, `version`, `kind`, `payload_len`, `sequence`.
Каждый integer кодируется big-endian вручную. Decoder сначала проверяет размер,
magic/version/kind и exact payload length, затем строит enum без allocation.

- [ ] **Шаг 4: определить полный V1 message enum**

```rust
pub(crate) enum Request {
    Hello {
        daemon_nonce: [u8; 16],
        deadline: MutationDeadlineNs,
    },
    PrepareKey {
        operation: OperationId,
        evdev_code: u16,
        deadline: MutationDeadlineNs,
    },
    ExecuteDown {
        operation: OperationId,
        token: PreparedToken,
        deadline: MutationDeadlineNs,
    },
    KeyUp {
        operation: OperationId,
        token: PreparedToken,
        deadline: ReleaseDeadline,
    },
    Synchronize {
        operation: OperationId,
        token_id: u64,
        deadline: ReleaseDeadline,
    },
    TransferToPhysicalDebt {
        operation: OperationId,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: MutationDeadlineNs,
    },
    PhysicalReleaseCommitted {
        sequence: PhysicalSequence,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: MutationDeadlineNs,
    },
    CancelAndDrain {
        operation: OperationId,
        deadline: CleanupDeadlineNs,
    },
    ReleaseAllAndExit {
        deadline: CleanupDeadlineNs,
    },
}

pub(crate) enum Response {
    Ready {
        session: SessionId,
        epoch: ServerEpoch,
        epoch_window: u32,
        epoch_nonce: [u8; 16],
    },
    Prepared { operation: OperationId, token: PreparedToken },
    DownAck { operation: OperationId, token_id: u64 },
    UpAck { operation: OperationId, token_id: u64 },
    SyncAck { operation: OperationId, token_id: u64 },
    TransferAck { operation: OperationId, token_id: u64 },
    ReleaseCommitAck { sequence: PhysicalSequence, token_id: u64 },
    Drained { operation: OperationId, proof: WireTerminalProof },
    Stopped { proof: WireTerminalProof },
    Fatal { code: FatalCode },
}
```

`FatalCode` — closed enum без произвольной строки. Один request находится в
flight; response обязан повторять sequence и соответствующий id.

Оба deadline-типа — не сериализованный Rust `Instant`, а абсолютное значение
Linux `CLOCK_MONOTONIC`, общее для двух процессов одного kernel.
`MutationDeadlineNs` вычисляется из исходного transaction/writer-command
deadline и никогда не продлевается. Guardian до каждого normal backend call
отклоняет expired deadline и значение дальше чем
`MAX_TRANSACTION_TIMEOUT=5s` от своего текущего `clock_gettime`; так сообщение,
задержавшееся в socket queue, не начинает mutation после timeout daemon.

При terminal transition один раз создаётся отдельный
`CleanupDeadlineNs <= now + MAX_RELEASE_CLEANUP=1s`. Guardian запоминает первый
cleanup deadline session и запрещает его продление. Первый принятый
`ReleaseDeadline::Cleanup` атомарно закрывает normal-mutation gate до попытки
key-up; отдельный `CancelAndDrain` не требуется посылать раньше release.
`Cleanup` разрешён только
для matching `KeyUp`, `Synchronize`, `CancelAndDrain` и
`ReleaseAllAndExit`; `PrepareKey`, `ExecuteDown`, transfer и любой новый down с
ним отклоняются. На expiry cleanup продолжается только до уже начатого
backend-вызова и заканчивается `Unreconciled`, но исходный transaction timeout
не блокирует обязательную release-only попытку. При EOF/SIGTERM guardian сам
создаёт один такой bounded cleanup deadline. Overflow, zero и backwards values
являются protocol violation.

- [ ] **Шаг 5: реализовать protocol state transitions**

`ProtocolState` допускает один active operation, строго возрастающий sequence,
до 512 prepared tokens и до 32 debt. `PrepareKey` не мутирует X11.
`ExecuteDown` принимает только token этой session/epoch и сохраняет possible
debt до down+sync. `KeyUp` не удаляет debt до matching `SyncAck`.

Tests отдельно проверяют expired mutation deadline, deadline слишком далеко в
будущем, разрешённый release после transaction expiry, запрет cleanup-scoped
down, попытку продлить cleanup deadline, expiry между двумя cleanup steps и
отсутствие normal mutation после terminal transition.

- [ ] **Шаг 6: выполнить protocol tests**

```bash
cargo test --locked --lib xtest_guardian::protocol::tests -- --test-threads=1
```

Ожидается: все codec, bounds, sequence и stale-token tests проходят.

- [ ] **Шаг 7: зафиксировать protocol**

```bash
git add src/daemon/xtest_guardian src/daemon/mod.rs
git commit -m "feat: define bounded XTEST guardian protocol"
```

### Задача 6: Реализовать `SOCK_SEQPACKET` и socket activation boundary

**Файлы:**

- Изменить: `Cargo.toml`, `Cargo.lock`
- Создать: `src/daemon/xtest_guardian/seqpacket.rs`
- Тест: `src/daemon/xtest_guardian/seqpacket.rs`

- [ ] **Шаг 1: написать transport/security tests**

```rust
#[test]
fn seqpacket_preserves_one_frame_per_datagram() {
    let (left, right) = Seqpacket::pair().unwrap();
    left.send_frame(b"one").unwrap();
    left.send_frame(b"two").unwrap();
    assert_eq!(right.recv_frame().unwrap(), b"one");
    assert_eq!(right.recv_frame().unwrap(), b"two");
}

#[test]
fn oversized_datagram_is_rejected_not_truncated() {
    let (left, right) = Seqpacket::pair().unwrap();
    left.send_unchecked(&vec![0x41; MAX_FRAME_BYTES + 1]).unwrap();
    assert!(matches!(
        right.recv_frame(),
        Err(InputSafetyError::OversizedFrame { .. })
    ));
}

#[test]
fn activated_listener_rejects_wrong_pid_fd_count_and_peer_inode() {
    assert!(ActivatedListener::from_env(fake_env(0, 1)).is_err());
    assert!(ActivatedListener::from_env(fake_env(current_pid(), 2)).is_err());
    assert!(validate_peer_binary(test_peer_with_other_inode()).is_err());
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib xtest_guardian::seqpacket::tests -- --test-threads=1
```

Ожидается: compile failure.

- [ ] **Шаг 3: включить только требуемые `nix` features**

```toml
nix = { version = "0.26.4", default-features = false, features = [
    "poll",
    "signal",
    "socket",
    "time",
    "uio",
    "user",
] }
```

Не добавлять отдельную serialization/runtime framework dependency.

- [ ] **Шаг 4: реализовать frame-safe wrapper**

Использовать `nix::sys::socket::{socket, connect, accept4, send, recvmsg,
getsockopt}` с `SockType::SeqPacket` и `SOCK_CLOEXEC`. Receive buffer имеет
`MAX_FRAME_BYTES + 1`; результат больше `MAX_FRAME_BYTES` всегда rejected.
Partial send считается terminal transport error.

- [ ] **Шаг 5: валидировать socket activation**

`ActivatedListener::from_env` принимает ровно:

```text
LISTEN_PID == getpid()
LISTEN_FDS == 1
fd == 3
SO_TYPE == SOCK_SEQPACKET
AF_UNIX
```

После принятия fd удалить `LISTEN_PID`, `LISTEN_FDS`, `LISTEN_FDNAMES` из
process environment и установить `FD_CLOEXEC`.

- [ ] **Шаг 6: проверять обе стороны через kernel credentials**

Guardian проверяет подключившийся daemon через `SO_PEERCRED`: текущий UID и
`(st_dev, st_ino)` `/proc/<peer-pid>/exe` должны совпасть с
`/proc/self/exe`. На стороне daemon нельзя использовать ту же проверку
симметрично: при systemd socket activation `SO_PEERCRED` у клиента указывает на
процесс, вызвавший `listen(2)`, то есть на user manager, а не на guardian,
который позднее вызвал `accept(2)`.

Поэтому daemon включает `SO_PASSCRED` до `connect(2)`, проверяет UID владельца
listener через `SO_PEERCRED`, а фактический UID, PID и inode guardian проверяет
по добавленному ядром `SCM_CREDENTIALS` каждого непустого response frame.
Отсутствующие, усечённые, повторные или неожиданные ancillary data считаются
terminal transport error; полученные через `SCM_RIGHTS` descriptors немедленно
закрываются. Это сохраняет запрет смешивать старый guardian и новый daemon при
package update даже при одинаковой protocol version.

- [ ] **Шаг 7: выполнить transport tests и compile check**

```bash
cargo test --locked --lib xtest_guardian::seqpacket::tests -- --test-threads=1
cargo check --locked --all-targets --features settings-ui
```

Ожидается: transport tests и все targets проходят.

- [ ] **Шаг 8: зафиксировать transport**

```bash
git add Cargo.toml Cargo.lock src/daemon/xtest_guardian/seqpacket.rs
git commit -m "feat: add authenticated guardian transport"
```

### Задача 7: Реализовать guardian core с fake executor

**Файлы:**

- Создать: `src/daemon/xtest_guardian/service.rs`
- Создать: `src/daemon/xtest_guardian/process_tests.rs`
- Изменить: `src/daemon/xtest_guardian/mod.rs`
- Тест: оба новых файла

- [ ] **Шаг 1: написать unit tests authoritative ledger**

Использовать `FakeXtestExecutor`, который отдельно различает «применил событие»
и «вернул ACK»:

```rust
#[test]
fn eof_drains_authoritative_temporary_and_session_debt() {
    let mut executor = FakeXtestExecutor::default();
    let mut session = GuardianSession::ready(test_identity(), &mut executor);
    let temporary = session.apply_down(test_down_request(Key::KEY_A)).unwrap();
    let modifier = session.apply_restored_modifier(Key::KEY_LEFTSHIFT).unwrap();

    let proof = session.on_peer_eof();

    assert_eq!(
        executor.release_order(),
        &[modifier.x11_keycode, temporary.x11_keycode],
    );
    assert_eq!(proof, TerminalProof::Reconciled);
}

#[test]
fn cleanup_continues_after_first_executor_error_and_reports_unreconciled() {
    let mut executor = FakeXtestExecutor::fail_release_number(1);
    let mut session = session_with_two_debts(&mut executor);
    let proof = session.on_peer_eof();

    assert_eq!(executor.release_attempts(), 2);
    assert_eq!(proof, TerminalProof::Unreconciled { remaining: 1 });
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib xtest_guardian::service::tests -- --test-threads=1
```

Ожидается: compile failure.

- [ ] **Шаг 3: определить узкий executor contract**

```rust
trait XtestExecutor {
    fn server_identity(&self) -> &X11ServerIdentity;
    fn prepare_key(&mut self, evdev_code: u16) -> Result<(u8, ServerEpoch), InputSafetyError>;
    fn key_down(&mut self, keycode: u8) -> Result<(), InputSafetyError>;
    fn key_up(&mut self, keycode: u8) -> Result<(), InputSafetyError>;
    fn synchronize(&mut self) -> Result<(), InputSafetyError>;
}
```

Guardian ledger записывает `AttemptingDown` до `key_down`. Любой возврат
`key_down`, включая error, переводит token в `PossiblyDown`. После `key_up`
token остаётся debt до успешного `synchronize`.

- [ ] **Шаг 4: реализовать request loop и EOF drain**

Loop проверяет protocol state до executor call, отвечает только после
подтверждённого перехода и на EOF/SIGTERM:

```rust
fn finish_session(
    session: &mut GuardianSession<'_>,
    reason: StopReason,
) -> TerminalProof {
    session.reject_new_mutations();
    let proof = session.release_all_reverse();
    session.record_terminal(reason, proof.clone());
    proof
}
```

При protocol violation отправить bounded `FatalCode`, закрыть channel и
выполнить тот же drain. Ни одна ошибка не разрешает следующий down.

- [ ] **Шаг 5: добавить process fixture без production fault API**

`process_tests.rs` компилируется только под `cfg(test)`. Test binary запускает
сам себя через `current_exe()` и `--exact` в ролях, заданных test-only env.
Fake executor пишет bounded trace в `tempfile`; production binary не читает эти
env и не содержит fault branches.

- [ ] **Шаг 6: проверить смерть daemon surrogate**

```rust
#[test]
fn sigkill_daemon_surrogate_after_down_ack_makes_guardian_release_and_exit() {
    let fixture = ProcessFixture::spawn();
    fixture.wait_for_trace("down-ack").unwrap();
    fixture.kill_daemon_surrogate().unwrap();
    fixture.wait_for_trace("release").unwrap();
    assert!(fixture.guardian_exited_within(GUARDIAN_CLEANUP_DEADLINE));
    assert_eq!(fixture.remaining_debt(), 0);
}
```

- [ ] **Шаг 7: проверить lost ACK, panic и cleanup failure**

Добавить отдельные tests:

- executor применил down, response не доставлен — down count остаётся `1`;
- panic daemon surrogate закрывает channel и guardian выходит;
- daemon death после `TransferToPhysicalDebt` вызывает modifier up;
- cleanup error даёт только `Unreconciled`, не `Stopped(Reconciled)`;
- после каждого case не остаётся child/zombie fixture.

- [ ] **Шаг 8: выполнить process tests**

```bash
cargo test --locked --lib xtest_guardian::process_tests -- --test-threads=1 --nocapture
```

Ожидается: fake-only processes проходят; X11/input/uinput не открываются.

- [ ] **Шаг 9: зафиксировать guardian core**

```bash
git add src/daemon/xtest_guardian
git commit -m "feat: reconcile guardian debt after daemon loss"
```

### Задача 8: Реализовать реальный X11 executor и доказательство server epoch

**Файлы:**

- Создать: `src/daemon/xtest_guardian/x11.rs`
- Изменить: `src/daemon/keyboard.rs:3782-4195`
- Тест: `src/daemon/xtest_guardian/x11.rs`

- [ ] **Шаг 1: написать pure/fake tests X11 boundary**

```rust
#[test]
fn prepared_token_contains_validated_keycode_and_current_epoch() {
    let mut connection = FakeX11Connection::with_mapping(Key::KEY_A, 38);
    let identity = test_server_identity();
    let mut executor = GuardianX11Executor::from_fake(connection, identity.clone());

    let prepared = executor.prepare_key(Key::KEY_A.code()).unwrap();

    assert_eq!(prepared, (38, identity.epoch));
}

#[test]
fn emergency_connection_rejects_mismatched_epoch_property() {
    let expected = test_server_identity();
    let connection = FakeX11Connection::with_epoch(
        ServerEpoch([0xEE; 16]),
        expected.epoch_window,
        [0xDD; 16],
    );
    assert!(EmergencyX11Releaser::verify_fake(connection, &expected).is_err());
}

#[test]
fn release_is_not_acknowledged_until_real_round_trip() {
    let mut connection = FakeX11Connection::default();
    connection.fail_round_trip = true;
    let mut executor =
        GuardianX11Executor::from_fake(connection, test_server_identity());

    executor.key_up(38).unwrap();
    assert!(executor.synchronize().is_err());
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib xtest_guardian::x11::tests -- --test-threads=1
```

Ожидается: compile failure.

- [ ] **Шаг 3: создать guardian-owned epoch marker**

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X11ServerIdentity {
    pub(crate) epoch: ServerEpoch,
    pub(crate) root: u32,
    pub(crate) epoch_window: u32,
    pub(crate) epoch_nonce: [u8; 16],
}
```

Guardian:

1. читает 32 random bytes из `/dev/urandom` через `read_exact`;
2. создаёт маленькое `INPUT_ONLY` window на root;
3. записывает nonce в property `_OPEN_SWITCHER_XTEST_GUARDIAN_EPOCH_V1`;
4. делает `get_property(...).reply()` и сверяет exact bytes;
5. только после этого отправляет `Ready`.

`ServerEpoch` вычисляется из случайных bytes и setup fingerprint. XID/property
нужны не как глобальный секрет, а как доказательство, что уже открытые guardian,
XKB и emergency connections указывают на один живой X server.

- [ ] **Шаг 4: реализовать XTEST executor**

`key_down`/`key_up` вызывают только:

```rust
connection
    .xtest_fake_input(event_type, keycode, x11rb::CURRENT_TIME, root, 0, 0, 0)?
    .check()?;
connection.flush()?;
```

`synchronize()` выполняет настоящий round-trip:

```rust
connection.get_input_focus()?.reply()?;
connection.flush()?;
```

`flush()` без reply не считается terminal proof.

- [ ] **Шаг 5: открыть две независимые daemon-side X11 connections**

- `CinnamonXkbController` выполняет только `xkb_get_*` и
  `xkb_latch_lock_state`; он не импортирует `x11rb::protocol::xtest`.
- `EmergencyX11Releaser` после startup verification не используется в normal
  path и хранится в `EmergencyCoordinator`.

Обе читают guardian epoch property до writer-ready. Новое emergency connection
после failure открывать запрещено.

- [ ] **Шаг 6: разделить старый monolith**

Из `CinnamonX11XtestReplayer` удалить прямые `emit_fake_key`,
`fake_key_down_attempt`, `fake_key`, `type_key`. Оставить XKB group calculation
как daemon-side `CinnamonXkbController`; validation и все XTEST key mutations
перенести guardian executor.

- [ ] **Шаг 7: выполнить X11 pure tests и compile check**

```bash
cargo test --locked --lib xtest_guardian::x11::tests -- --test-threads=1
cargo check --locked --all-targets --features settings-ui
```

Ожидается: tests используют fake connection и не подключаются к host X11.

- [ ] **Шаг 8: зафиксировать X11 boundary**

```bash
git add src/daemon/xtest_guardian/x11.rs src/daemon/keyboard.rs
git commit -m "feat: isolate XTEST execution in guardian"
```

### Задача 9: Реализовать daemon client, mirrored debt и emergency job

**Файлы:**

- Создать: `src/daemon/xtest_guardian/client.rs`
- Изменить: `src/daemon/xtest_guardian/mod.rs`
- Изменить: `src/error/input_safety_error.rs`
- Тест: `src/daemon/xtest_guardian/client.rs`
- Тест: `src/daemon/xtest_guardian/process_tests.rs`

- [ ] **Шаг 1: написать tests mirror-before-mutation**

```rust
#[test]
fn mirror_is_written_before_execute_down_can_reach_transport() {
    let transport = RecordingTransport::fail_after_receiving_execute_down();
    let coordinator = EmergencyCoordinator::for_test();
    let mut client = GuardianClient::from_test_transport(transport, coordinator.clone());
    let token = client.prepare_key(OperationId(3), Key::KEY_A).unwrap();

    assert!(client.execute_down(OperationId(3), token).is_err());
    assert_eq!(coordinator.possible_tokens(), &[token]);
    assert_eq!(client.execute_down_count(token), 1);
}

#[test]
fn successful_up_and_sync_remove_only_matching_mirror() {
    let mut client = ready_test_client();
    let first = client.prepare_key(OperationId(3), Key::KEY_A).unwrap();
    let second = client.prepare_key(OperationId(3), Key::KEY_B).unwrap();
    client.execute_down(OperationId(3), first).unwrap();
    client.execute_down(OperationId(3), second).unwrap();
    client.key_up(OperationId(3), second).unwrap();

    assert_eq!(client.mirrored_tokens(), &[first]);
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib xtest_guardian::client::tests -- --test-threads=1
```

Ожидается: compile failure.

- [ ] **Шаг 3: реализовать один broker thread**

Только broker владеет seqpacket fd и читает responses. Writer отправляет
bounded `GuardianCommand` и ждёт one-shot reply до исходного локального
transaction deadline. Rust `Instant` не сериализуется: wire получает
`MutationDeadlineNs`, вычисленный из того же абсолютного `CLOCK_MONOTONIC`
deadline; broker не создаёт новый budget для каждого normal request.
Release-only finalizer создаёт один отдельный `CleanupDeadlineNs` и передаёт
его всем cleanup commands без продления.

Shared health:

```rust
#[derive(Clone)]
pub(crate) struct GuardianHealth {
    failed: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<InputSafetyError>>>,
}
```

Broker polling одновременно видит command wakeup, guardian response/HUP и stop.
При HUP он сначала закрывает terminal gate, затем публикует mirror snapshot в
`EmergencyCoordinator`, затем помечает health failed.

Broker также ведёт bounded кольцо последних 512 request/response durations.
Только при включённом `OPEN_SWITCHER_INPUT_DEBUG` на shutdown публикуются
`count`, `p50_us`, `p95_us`, `max_us`; key, token и payload в запись не входят.
Эта телеметрия нужна для Mint release gate и не создаёт отдельного production
benchmark API.

- [ ] **Шаг 4: реализовать mirror ordering**

Для down:

```text
PrepareKey -> Prepared
mirror.insert(token as possible)
ExecuteDown -> DownAck
Synchronize -> SyncAck
```

Для up:

```text
KeyUp -> UpAck
Synchronize -> SyncAck
mirror.remove(token)
```

Ни timeout, ни lost ACK не вызывают повтор down. Они запускают
`CancelAndDrain`; если channel уже ненадёжен — только emergency.

`GuardianSyntheticSink` реализует тот же `SyntheticKeySink`: `prepare_down`,
`attempt_down`, `attempt_up` и `synchronize` только ставят typed broker
commands, а `terminal_proof(remaining)` возвращает подтверждённый
`Drained/Stopped` proof guardian. При HUP/timeout и ненулевом mirror он
возвращает `Unreconciled` до завершения `EmergencyCoordinator`; backend-ветвь
в общий ledger не добавляется.

- [ ] **Шаг 5: реализовать `EmergencyCoordinator` без ранней инъекции**

```rust
enum EmergencyState {
    Armed {
        connection: EmergencyX11Releaser,
        epoch: ServerEpoch,
        mirrored: Vec<PreparedToken>,
    },
    Pending {
        connection: EmergencyX11Releaser,
        mirrored: Vec<PreparedToken>,
    },
    Running,
    Finished(TerminalProof),
}
```

Guardian failure переводит `Armed -> Pending`, но не начинает X11 mutation.
Только `KeyboardController` после `release_grab_best_effort()` имеет право
вызвать `start_pending_release()`. Worker делает reverse key-up по точным
tokens, один round-trip и публикует `Reconciled`/`Unreconciled`.

- [ ] **Шаг 6: проверить hard-bounded wait**

Controller ждёт worker не более
`GUARDIAN_EMERGENCY_DEADLINE = Duration::from_secs(1)`. Timeout публикует
`Unreconciled`; worker не join-ится бесконечно, а daemon возвращает fatal error
и завершает весь процесс.

- [ ] **Шаг 7: добавить guardian SIGKILL process test**

```rust
#[test]
fn sigkill_guardian_after_down_starts_emergency_only_after_ungrab_signal() {
    let fixture = ProcessFixture::spawn_with_fake_emergency();
    fixture.wait_for_trace("down-applied").unwrap();
    fixture.kill_guardian().unwrap();
    fixture.wait_for_trace("grab-released").unwrap();
    fixture.wait_for_trace("emergency-up").unwrap();

    assert!(fixture.trace_before("grab-released", "emergency-up"));
    assert!(fixture.daemon_surrogate_failed_within(Duration::from_secs(2)));
    assert_eq!(fixture.down_retries(), 0);
}
```

- [ ] **Шаг 8: выполнить client/process tests**

```bash
cargo test --locked --lib xtest_guardian::client::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::process_tests -- --test-threads=1 --nocapture
```

Ожидается: mirror ordering, guardian death, mismatched epoch, timeout и
no-fallback tests проходят без real X11/input.

- [ ] **Шаг 9: зафиксировать client/emergency**

```bash
git add src/daemon/xtest_guardian src/error/input_safety_error.rs
git commit -m "feat: fail stop after XTEST guardian loss"
```

### Задача 10: Подключить guardian к Cinnamon writer до physical grab

**Файлы:**

- Изменить: `src/daemon/keyboard.rs:1351-1473`
- Изменить: `src/daemon/keyboard.rs:2208-2460`
- Изменить: `src/daemon/keyboard.rs:3782-4908`
- Изменить: `src/daemon/keyboard.rs:5128-5385`
- Изменить: `src/daemon/input_backend.rs:85-92,239-251,348-371`
- Тест: `src/daemon/keyboard.rs`
- Тест: `src/daemon/input_backend.rs`

- [ ] **Шаг 1: написать startup/fallback tests**

```rust
#[test]
fn cinnamon_writer_is_not_ready_until_guardian_and_both_daemon_x11_connections_are_verified() {
    let steps = Arc::new(Mutex::new(Vec::new()));
    let result = prepare_writer_with_fake_dependencies(
        guardian_ready_after(steps.clone()),
        verified_xkb_after(steps.clone()),
        verified_emergency_after(steps.clone()),
    );
    assert!(result.is_ok());
    assert_eq!(
        *steps.lock().unwrap(),
        ["guardian-ready", "xkb-verified", "emergency-verified", "writer-ready"],
    );
}

#[test]
fn guardian_startup_failure_aborts_before_physical_grab() {
    let trace = prepare_keyboard_with_guardian_failure();
    assert_eq!(trace, ["guardian-connect", "writer-stop"]);
    assert!(!trace.contains(&"physical-grab"));
}

#[test]
fn available_guardian_runtime_failure_never_falls_back_to_uinput() {
    let result = finish_fast_separator_replay(
        Some(Err(SwitcherError::input_safety("guardian lost"))),
        || panic!("fallback must not run after XTEST was selected"),
    );
    assert!(result.is_err());
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib guardian_startup_ -- --test-threads=1
cargo test --locked --lib available_guardian_ -- --test-threads=1
```

Ожидается: новые tests падают.

- [ ] **Шаг 3: заменить runtime enum**

```rust
enum CinnamonX11XtestRuntime {
    NotSelected,
    Available(CinnamonGuardianReplay),
}
```

На Cinnamon/X11 ошибка socket/handshake/XKB/emergency verification возвращает
writer startup error. `Unavailable` с последующим grab и runtime fallback
удаляется. На Wayland/не-Cinnamon остаётся `NotSelected` и прежний uinput path.

- [ ] **Шаг 4: подготовить весь correction trace до XKB mutation**

`CinnamonGuardianReplay::prepare_operation` создаёт exact очередь tokens для:

- physical modifier releases/restores;
- `buffer.len() + extra_backspaces` Backspace taps;
- каждого stroke и temporary Shift;
- separator fast path.

Maximum — не более `MAX_PREPARED_TOKENS`. Каждый token guardian повторно
проверяет перед `ExecuteDown`. Только после успешного preflight
`CinnamonXkbController` меняет group.

- [ ] **Шаг 5: перевести correction/separator на guardian sink**

`run_cinnamon_x11_xtest_correction`, `replay_xtest_stroke`, tap и modifier
helpers используют `SyntheticOperation<GuardianClient>`. Прямых
`xtest_fake_input` в daemon больше нет:

```bash
rg -n "xtest_fake_input|protocol::xtest::ConnectionExt" src \
  -g '*.rs'
```

Ожидается: production-вызов находится только в
`src/daemon/xtest_guardian/x11.rs`; тестовый VM probe может лишь наблюдать
XInput и `XQueryKeymap`.

- [ ] **Шаг 6: связать guardian health с writer health**

`VirtualKeyboardHandle::health_error()` сначала проверяет transaction failure,
затем guardian health. `InputSafetyError::Guardian*` и
`Unreconciled` возвращают `false` из
`SwitcherError::is_recoverable_input_error`; `InputBackendLifecycle` latch
запрещает reopen в том же процессе.

- [ ] **Шаг 7: прогнать прежние и новые Cinnamon tests**

```bash
cargo test --locked --lib cinnamon_ -- --test-threads=1
cargo test --locked --lib xtest_ -- --test-threads=1
cargo test --locked --lib available_guardian_ -- --test-threads=1
```

Ожидается: обе golden traces из задачи 1 совпадают, deliberate delays не
изменены, ambiguous errors не маскируются.

- [ ] **Шаг 8: зафиксировать writer integration**

```bash
git add src/daemon/keyboard.rs src/daemon/input_backend.rs
git commit -m "feat: require guardian before Cinnamon input grab"
```

### Задача 11: Провести physical sequence до session debt и упорядочить fail-stop

**Файлы:**

- Изменить: `src/daemon/service.rs:94-122`
- Изменить: `src/daemon/service.rs:495-540`
- Изменить: `src/daemon/service.rs:905-930`
- Изменить: `src/daemon/service.rs:1520-1765`
- Изменить: `src/daemon/service.rs:2245-2906`
- Изменить: `src/daemon/keyboard.rs:142-199,1451-1492,2697-2719`
- Изменить: `src/daemon/mod.rs:138-222`
- Тест: эти три модуля

- [x] **Шаг 1: написать sequence/commit tests**

```rust
#[test]
fn deferred_physical_release_commits_only_after_writer_write_and_sync() {
    let event = DeferredInputEvent {
        sequence_id: 77,
        key: Key::KEY_LEFTSHIFT,
        value: 0,
        timestamp: SystemTime::UNIX_EPOCH,
    };
    let trace = forward_deferred_release_with_fake_writer(event).unwrap();
    assert_eq!(
        trace,
        [
            "uinput-write-release:77",
            "uinput-sync:77",
            "guardian-physical-release-commit:77",
            "guardian-release-commit-ack:77",
            "service-deferred-ack:77",
        ]
    );
}

#[test]
fn lost_release_commit_ack_forbids_next_press_of_same_modifier() {
    let mut writer = writer_with_lost_release_commit_ack(Key::KEY_LEFTSHIFT);
    assert!(writer.forward_release(PhysicalSequence(8)).is_err());
    assert!(writer.forward_press(PhysicalSequence(9)).is_err());
    assert_eq!(writer.press_count(Key::KEY_LEFTSHIFT), 0);
}
```

- [x] **Шаг 2: проверить RED**

```bash
cargo test --locked --lib physical_release_ -- --test-threads=1
cargo test --locked --lib lost_release_commit_ -- --test-threads=1
```

Ожидается: tests падают, потому что `sequence_id` пока теряется.

- [x] **Шаг 3: сохранять event identity во всех routing ветвях**

`handle_key_event` сохраняет:

```rust
let PhysicalEventIdentity {
    sequence: PhysicalSequence(event.sequence_id),
    generation: keyboard.input_generation(),
};
```

`forward_event_for_origin`, capture routing, boundary/backspace/plain-character
closures и fast/deferred writer commands получают identity целиком. Нельзя
создавать новый sequence при retry; deferred ledger владеет исходным id до
полного ACK.

- [x] **Шаг 4: выполнить XTEST physical release commit**

После uinput `write + synchronize` writer:

1. находит matching XTEST session token;
2. отправляет `PhysicalReleaseCommitted(sequence, token, generation)`;
3. ждёт `ReleaseCommitAck`;
4. удаляет daemon mirror;
5. только затем отвечает service/deferred ledger.

Lost/mismatched ACK закрывает terminal gate. Следующий command, включая press
того же key, не dispatch-ится.

- [x] **Шаг 5: добавить shutdown phase между ungrab и wait**

```rust
enum KeyboardShutdownPhase {
    RequestWriterStop,
    ReleaseGrab,
    StartPendingEmergencyRelease,
    FinishWriterStop,
    StopAndJoinWatchers,
    DetachWatchers,
}
```

Точный порядок:

```text
terminal gate / writer stop requested
physical EVIOCGRAB release
EmergencyCoordinator::start_pending_release
bounded emergency result
writer/device teardown
watchers join либо detach
```

- [x] **Шаг 6: публиковать proof уничтожения uinput только после Drop device**

`WriterExitReport` отправляется после выхода closure, владеющей
`uinput::Device`. Только там разрешён
`OwnerGenerationDestroyed { generation }`. Transaction completion до teardown
не может использовать этот proof.

- [x] **Шаг 7: сохранять primary и cleanup error раздельно**

`WriterTerminalReport` содержит:

```rust
struct WriterTerminalReport {
    primary: Option<SwitcherError>,
    cleanup: Option<SwitcherError>,
    proof: TerminalProof,
}
```

`daemon::finalize_daemon_run` не заменяет primary cleanup-ошибкой. Логи выводят
категории и `remaining`, но не tokens/keys/text. Typed
`InputSafetyError` остаётся вариантом `SwitcherError`, поэтому protocol и
reconciliation причины не теряются, но report также без downcast принимает
uinput I/O, transaction timeout/cancel и исходную daemon error.

- [x] **Шаг 8: выполнить lifecycle tests**

```bash
cargo test --locked --lib guardian_failure_releases_grab_before_emergency_wait -- --test-threads=1
cargo test --locked --lib writer_exit_notification_follows_owned_device_drop -- --test-threads=1
cargo test --locked --lib deferred_forward_ -- --test-threads=1
cargo test --locked --lib unresponsive_writer_forbids_second_backend_install -- --test-threads=1
```

Ожидается: порядок ungrab/emergency доказан, stale ACK не проходит, in-process
reopen после guardian failure отсутствует.

- [x] **Шаг 9: зафиксировать physical-debt integration**

```bash
git add src/daemon/service.rs src/daemon/keyboard.rs src/daemon/mod.rs
git commit -m "feat: reconcile restored modifiers after physical release"
```

### Задача 12: Добавить безопасный hidden entrypoint и SIGTERM drain

**Файлы:**

- Изменить: `src/main.rs:1-14`
- Изменить: `src/daemon/xtest_guardian/mod.rs`
- Изменить: `src/daemon/xtest_guardian/service.rs`
- Тест: `src/main.rs`
- Тест: `src/daemon/xtest_guardian/service.rs`

- [ ] **Шаг 1: написать argument/activation tests**

```rust
#[test]
fn no_arguments_selects_normal_daemon() {
    assert_eq!(
        select_entrypoint(["open-switcher-daemon"]),
        Ok(Entrypoint::Daemon),
    );
}

#[test]
fn exact_hidden_argument_requires_socket_activation_before_x11() {
    assert_eq!(
        select_entrypoint([
            "open-switcher-daemon",
            "--internal-xtest-guardian-v1",
        ]),
        Ok(Entrypoint::XtestGuardian),
    );
    let trace = run_internal_mode_with_fake_activation(None);
    assert_eq!(trace, ["activation-rejected"]);
    assert!(!trace.contains(&"x11-connect"));
}

#[test]
fn extra_or_unknown_arguments_are_rejected() {
    assert!(select_entrypoint([
        "open-switcher-daemon",
        "--internal-xtest-guardian-v1",
        "extra",
    ]).is_err());
    assert!(select_entrypoint(["open-switcher-daemon", "--help"]).is_err());
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --bin open-switcher entrypoint -- --test-threads=1
```

Ожидается: compile failure.

- [ ] **Шаг 3: реализовать строгий dispatcher**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entrypoint {
    Daemon,
    XtestGuardian,
}

fn select_entrypoint(
    args: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<Entrypoint, InputSafetyError> {
    let mut args = args.into_iter();
    let _program = args.next();
    match (args.next(), args.next()) {
        (None, None) => Ok(Entrypoint::Daemon),
        (Some(argument), None)
            if argument.as_ref() == "--internal-xtest-guardian-v1" =>
        {
            Ok(Entrypoint::XtestGuardian)
        }
        _ => Err(InputSafetyError::InvalidEntrypoint),
    }
}
```

`Entrypoint::XtestGuardian` сначала валидирует activation fd и peer boundary,
и лишь затем открывает X11. Linux input setup hints для internal mode не
печатаются.

- [ ] **Шаг 4: реализовать signal wakeup**

Только hidden process блокирует `SIGTERM`/`SIGINT` через
`nix::sys::signal::pthread_sigmask`, запускает один `SigSet::wait()` thread и
будит guardian loop через private socketpair. Получив wakeup, loop запрещает
новые down и выполняет `finish_session(..., StopReason::Signal)`.

Normal daemon signal semantics этим кодом не меняются.

- [ ] **Шаг 5: проверить shutdown ordering**

```rust
#[test]
fn sigterm_path_drains_before_guardian_process_returns() {
    let trace = run_guardian_with_fake_signal_and_debt();
    assert_eq!(
        trace,
        ["signal", "terminal-gate", "key-up", "round-trip", "return"],
    );
}
```

- [ ] **Шаг 6: выполнить entrypoint/service tests**

```bash
cargo test --locked --bin open-switcher entrypoint -- --test-threads=1
cargo test --locked --lib sigterm_path_drains -- --test-threads=1
```

Ожидается: exact argument работает только с activation fixture; manual internal
mode не достигает X11.

- [ ] **Шаг 7: зафиксировать entrypoint**

```bash
git add src/main.rs src/daemon/xtest_guardian
git commit -m "feat: add guarded XTEST service entrypoint"
```

### Задача 13: Добавить отдельные systemd units и безопасный package lifecycle

**Файлы:**

- Создать: `debian/open-switcher.open-switcher-xtest-guardian.user.socket`
- Создать: `debian/open-switcher.open-switcher-xtest-guardian.user.service`
- Создать: `dist/systemd/open-switcher-xtest-guardian.socket`
- Создать: `dist/systemd/open-switcher-xtest-guardian.service`
- Создать: `debian/open-switcher.preinst`
- Изменить: `debian/open-switcher.open-switcher-daemon.user.service`
- Изменить: `dist/systemd/open-switcher-daemon.service`
- Изменить: `debian/rules:46-48`
- Изменить: `debian/open-switcher.prerm`
- Изменить: `debian/open-switcher.postinst`
- Изменить: `debian/scripts/open-switcher-user-session-start`
- Изменить: `debian/scripts/open-switcher-user-session-stop`
- Изменить: `manage.sh:49-55,654-735,833-868`
- Изменить: `tests/debian_package_scripts_test.sh`
- Изменить: `tests/manage_package_deb_test.sh`

- [ ] **Шаг 1: написать failing static package tests**

Добавить assertions:

```bash
assert_file_exists "$REPO_ROOT/debian/open-switcher.open-switcher-xtest-guardian.user.socket"
assert_file_exists "$REPO_ROOT/debian/open-switcher.open-switcher-xtest-guardian.user.service"
assert_contains "$daemon_unit" "Wants=open-switcher-xtest-guardian.socket"
assert_contains "$daemon_unit" "After=open-switcher-xtest-guardian.socket"
assert_not_contains "$daemon_unit" "PartOf=open-switcher-xtest-guardian"
assert_not_contains "$daemon_unit" "BindsTo=open-switcher-xtest-guardian"
assert_contains "$rules" \
  "dh_installsystemduser --no-enable --name=open-switcher-xtest-guardian"
assert_contains "$preinst" "upgrade"
assert_contains "$prerm" "failed-upgrade"
```

Отдельный test разбирает строки stop helper и проверяет строгий порядок:
tray, daemon, bounded guardian wait, guardian socket, guardian service,
daemon-reload.

- [ ] **Шаг 2: проверить RED**

```bash
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
```

Ожидается: FAIL на отсутствующих guardian units/lifecycle.

- [ ] **Шаг 3: создать socket unit в обеих копиях**

```ini
[Unit]
Description=OpenSwitcher XTEST guardian socket

[Socket]
ListenSequentialPacket=%t/open-switcher/xtest-guardian.sock
Accept=no
Backlog=1
DirectoryMode=0700
SocketMode=0600
RemoveOnStop=yes
FileDescriptorName=xtest-guardian
```

У socket нет `[Install]`: его поднимает daemon через `Wants=`.

- [ ] **Шаг 4: создать service unit в обеих копиях**

Debian:

```ini
[Unit]
Description=OpenSwitcher XTEST guardian

[Service]
Type=exec
ExecStart=/usr/bin/open-switcher-daemon --internal-xtest-guardian-v1
Restart=no
KillMode=control-group
TimeoutStopSec=7s
UMask=0077
NoNewPrivileges=yes
PrivateDevices=yes
RestrictAddressFamilies=AF_UNIX
```

В `dist` отличается только:

```ini
ExecStart=open-switcher-daemon --internal-xtest-guardian-v1
```

`7s` выводится из `MAX_TRANSACTION_TIMEOUT=5s`,
`GUARDIAN_EMERGENCY_DEADLINE=1s` и 1s manager margin. До merge process stress
из задачи 14 обязан подтвердить p99 cleanup менее 500ms; иначе unit timeout не
увеличивать автоматически, а исправить зависшую cleanup-ветвь.

- [ ] **Шаг 5: связать daemon только с socket**

Добавить в Debian/dist daemon unit:

```ini
[Unit]
Description=OpenSwitcher daemon
Wants=open-switcher-xtest-guardian.socket
After=open-switcher-xtest-guardian.socket
```

Не добавлять `PartOf=`, `BindsTo=` или dependency на guardian service.

- [ ] **Шаг 6: подключить debhelper ровно один раз**

```make
override_dh_installsystemduser:
	dh_installsystemduser --no-enable --name=open-switcher-daemon
	dh_installsystemduser --no-enable --name=open-switcher-tray
	dh_installsystemduser --no-enable --name=open-switcher-xtest-guardian
```

Не вызывать helper отдельно для `.socket`/`.service`.

- [ ] **Шаг 7: закрыть первый upgrade со старой версии**

Новый `preinst` на `upgrade` вызывает уже установленный
`/usr/lib/open-switcher/open-switcher-user-session-stop`, если он существует,
до распаковки нового binary. Это нужно, потому что старый `prerm` текущей версии
не обрабатывает upgrade.

Старый helper ограничен `Active=yes`, поэтому полагаться только на него нельзя.
После его вызова новый `preinst` самостоятельно перечисляет локальные
`class=user` X11/Wayland sessions, дедуплицирует UID, проверяет
`/run/user/$uid/bus` и bounded останавливает старые tray/daemon units также у
inactive sessions. Эта минимальная совместимая stop-логика находится прямо в
`preinst`, потому что новый helper ещё не распакован. Static test моделирует
inactive session со старым helper и требует stop старого daemon до unpack.

Новый `prerm` вызывает stop helper для:

```text
upgrade
remove
deconfigure
failed-upgrade
```

`postinst` на `configure`, а также корректные abort-сценарии, вызывает
session-start после `daemon-reload`.

- [ ] **Шаг 8: реализовать последовательный stop для каждого user manager**

Не передавать несколько units одному `systemctl stop`. Для каждого уникального
локального UID с `class=user`, X11/Wayland session и `/run/user/$uid/bus`:

```text
systemctl --user stop open-switcher-tray.service
systemctl --user stop open-switcher-daemon.service
до 7s ждать inactive guardian.service
systemctl --user stop open-switcher-xtest-guardian.socket
systemctl --user stop open-switcher-xtest-guardian.service
systemctl --user daemon-reload
```

Stop helper дедуплицирует UID и не ограничивается только `Active=yes`: inactive
локальная сессия тоже может иметь живой user manager/guardian. Start helper
остаётся ограниченным active graphical session.

- [ ] **Шаг 9: обновить `manage.sh` без расхождения с package policy**

Dev install копирует обе guardian units. Переписывая `ExecStart`, он сохраняет
`--internal-xtest-guardian-v1`. `dev/systemd stop|restart` соблюдает тот же
daemon-before-guardian порядок. Удаление dev units не затрагивает packaged units
и не удаляет VM laboratory.

- [ ] **Шаг 10: проверить shell syntax и static tests**

```bash
bash -n \
  debian/open-switcher.preinst \
  debian/open-switcher.postinst \
  debian/open-switcher.prerm \
  debian/scripts/open-switcher-user-session-start \
  debian/scripts/open-switcher-user-session-stop
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
```

Ожидается: shell syntax и package policy tests проходят.

- [ ] **Шаг 11: проверить units через временный search path**

```bash
tmp="$(mktemp -d)"
cp dist/systemd/open-switcher-daemon.service "$tmp/"
cp dist/systemd/open-switcher-xtest-guardian.socket "$tmp/"
cp dist/systemd/open-switcher-xtest-guardian.service "$tmp/"
SYSTEMD_UNIT_PATH="$tmp" systemd-analyze --user verify \
  open-switcher-daemon.service \
  open-switcher-xtest-guardian.socket \
  open-switcher-xtest-guardian.service
```

Ожидается: exit `0`, ошибок unit dependency/директив нет. Временный каталог
после проверки удалить; VM lab не затрагивается.

- [ ] **Шаг 12: зафиксировать packaging**

```bash
git add debian dist/systemd manage.sh tests/debian_package_scripts_test.sh tests/manage_package_deb_test.sh
git commit -m "feat: package isolated XTEST guardian service"
```

### Задача 14: Создать узкий внешний VM probe

**Файлы:**

- Создать: `examples/h06_x11_vm_probe.rs`
- Тест: `examples/h06_x11_vm_probe.rs`

- [ ] **Шаг 1: написать pure tests guard/trace classifier**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_lab_marker_is_rejected_before_x11_or_signal() {
        let environment = FakeEnvironment::host();
        assert_eq!(
            validate_vm_boundary(&environment),
            Err(ProbeError::NotOpenSwitcherVm),
        );
        assert_eq!(environment.x11_connects(), 0);
        assert_eq!(environment.signals_sent(), 0);
    }

    #[test]
    fn target_press_is_followed_until_query_keymap_reports_up() {
        let events = [
            ProbeSample::press(22, 10),
            ProbeSample::query_down(22, 11),
            ProbeSample::query_up(22, 21),
        ];
        assert_eq!(
            classify_samples(&events, 22),
            ProbeOutcome::Released { elapsed_ms: 11 },
        );
    }
}
```

- [ ] **Шаг 2: проверить RED**

```bash
cargo test --locked --example h06_x11_vm_probe
```

Ожидается: compile failure.

- [ ] **Шаг 3: реализовать обязательную VM boundary**

Probe требует одновременно:

```text
--confirm-openswitcher-vm-lab
/etc/openswitcher-lab-guest — regular root-owned file
/sys/class/dmi/id/product_name содержит QEMU/KVM
target PID принадлежит текущему UID
/proc/<pid>/exe basename == open-switcher-daemon
```

До прохождения всех проверок он не открывает X11 и не посылает сигнал.

- [ ] **Шаг 4: реализовать три режима**

```text
observe --target-keycode N --output FILE
kill-on-press --target-keycode N --pid PID --output FILE
assert-key-up --target-keycode N --timeout-ms 2000 --output FILE
```

`observe` подписывается на XI2 raw key press/release и пишет bounded JSONL
`{kind,keycode,monotonic_us}`. `kill-on-press` на первом matching press делает
ровно один `SIGKILL` guest PID. `assert-key-up` вызывает `XQueryKeymap` каждые
10ms и успешно завершается только после up.

Probe не создаёт input device и сам не вводит клавиши; физический guest input
поступает через QEMU.

- [ ] **Шаг 5: выполнить example tests/check**

```bash
cargo test --locked --example h06_x11_vm_probe
cargo check --locked --example h06_x11_vm_probe
```

Ожидается: pure tests проходят, host boundary не позволяет signal.

- [ ] **Шаг 6: зафиксировать probe**

```bash
git add examples/h06_x11_vm_probe.rs
git commit -m "test: add guarded X11 VM safety probe"
```

### Задача 15: Выполнить все безопасные gates и собрать точный DEB

**Файлы:**

- Проверить: весь репозиторий
- Артефакт: точный путь
  `dist/packages/${package}_${version}_${arch}.deb`, вычисленный в шаге 7

- [ ] **Шаг 1: проверить формат и diff**

```bash
cargo fmt --check
git diff --check
```

Ожидается: exit `0`, whitespace errors нет.

- [ ] **Шаг 2: выполнить focused safety suites**

```bash
cargo test --locked --lib synthetic_input::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::protocol::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::seqpacket::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::service::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::client::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::process_tests -- --test-threads=1 --nocapture
```

Ожидается: все PASS, real X11/input/uinput не открываются.

- [ ] **Шаг 3: измерить fake maximum-debt cleanup**

Ignored release-mode test создаёт 32 possible debts, повторяет lifecycle 200
раз и печатает p50/p95/p99/max:

```bash
cargo test --release --locked --lib \
  guardian_cleanup_latency_for_maximum_debt \
  -- --ignored --nocapture --test-threads=1
```

Gate: p99 `< 500 ms`, каждый run `< 1 s`, remaining debt `0`. Это подтверждает
cleanup budget, из которого выведен `TimeoutStopSec=7s`.

- [ ] **Шаг 4: выполнить полную Rust-регрессию вне restricted sandbox**

```bash
cargo test --locked --all-targets --features settings-ui -- --test-threads=1
cargo check --locked --all-targets --features settings-ui
```

Ожидается: все targets/tests PASS. Известный sandbox-only hang
`input_target_stop_signal_wakes_idle_waiter` не обходить skip; полную команду
запускать в разрешённой среде, где baseline дал `711/711`.

- [ ] **Шаг 5: выполнить shell/package tests**

```bash
bash tests/wayland_diagnostics_test.sh
bash tests/linux_input_setup_test.sh
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
```

Ожидается: четыре script suites завершаются `ok`.

- [ ] **Шаг 6: собрать production package**

```bash
./manage.sh package deb
```

Ожидается: новый exact artifact в `dist/packages/`; сборка использует locked
Rust 1.95 toolchain и `settings-ui`.

- [ ] **Шаг 7: зафиксировать identity артефакта**

В этой и следующих командах одной shell-сессии определить путь без wildcard:

```bash
package="$(dpkg-parsechangelog -S Source)"
version="$(dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
CANDIDATE_DEB="$(realpath "dist/packages/${package}_${version}_${arch}.deb")"
test -f "$CANDIDATE_DEB"
sha256sum "$CANDIDATE_DEB"
dpkg-deb -f "$CANDIDATE_DEB" Package Version Architecture
```

Сохранить `CANDIDATE_DEB` и один SHA-256 в evidence. В дальнейших VM шагах
использовать только этот файл; glob с несколькими артефактами не допускается.

- [ ] **Шаг 8: извлечь и проверить точное содержимое DEB**

```bash
tmp="$(mktemp -d)"
dpkg-deb -x "$CANDIDATE_DEB" "$tmp/root"
dpkg-deb -e "$CANDIDATE_DEB" "$tmp/control"
find "$tmp/root/usr/lib/systemd/user" -maxdepth 1 -type f -printf '%f\n' | sort
test -x "$tmp/root/usr/bin/open-switcher-daemon"
test ! -e "$tmp/root/usr/bin/open-switcher-xtest-guardian"
stop_helper="$tmp/root/usr/lib/open-switcher/open-switcher-user-session-stop"
test -x "$stop_helper"
cmp -s debian/scripts/open-switcher-user-session-stop "$stop_helper"
for script in preinst prerm postinst; do
  test -f "$tmp/control/$script"
  sh -n "$tmp/control/$script"
done
rg -F "upgrade" "$tmp/control/preinst"
rg -F "open-switcher-daemon.service" "$tmp/control/preinst"
rg -F "open-switcher-user-session-stop" "$tmp/control/prerm"
rg -F "open-switcher-user-session-start" "$tmp/control/postinst"
```

Ожидается: присутствуют daemon/tray units и ровно guardian `.socket` +
`.service`; отдельного guardian binary нет. Извлечённый stop helper byte-for-byte
совпадает с прошедшим static order test, а фактические maintainer scripts
синтаксически корректны и содержат upgrade/stop/start lifecycle.

- [ ] **Шаг 9: проверить hidden mode извлечённого binary без X11**

```bash
env -u LISTEN_PID -u LISTEN_FDS -u LISTEN_FDNAMES \
  timeout 2s "$tmp/root/usr/bin/open-switcher-daemon" \
  --internal-xtest-guardian-v1
```

Ожидается: быстрый non-zero exit до X11; timeout `124` недопустим.

- [ ] **Шаг 10: проверить extracted units**

```bash
SYSTEMD_UNIT_PATH="$tmp/root/usr/lib/systemd/user" \
  systemd-analyze --user verify \
  open-switcher-daemon.service \
  open-switcher-xtest-guardian.socket \
  open-switcher-xtest-guardian.service
```

Ожидается: exit `0`.

- [ ] **Шаг 11: построить baseline package для сравнения производительности**

Во временном worktree собрать ровно исходный H-06 base:

```bash
BASE_COMMIT=9e894b2c00a57b9f74a5ccb293a21b1bcebc04d1
BASELINE_WT=/tmp/openswitcher-h06-baseline
BASELINE_DEB=/home/andrey/VMs/OpenSwitcherLab/artifacts/open-switcher_h06-baseline_9e894b2c00a57b9f74a5ccb293a21b1bcebc04d1_amd64.deb
test ! -e "$BASELINE_WT"
git worktree add --detach "$BASELINE_WT" "$BASE_COMMIT"
(
  cd "$BASELINE_WT"
  ./manage.sh package deb
  package="$(dpkg-parsechangelog -S Source)"
  version="$(dpkg-parsechangelog -S Version)"
  arch="$(dpkg --print-architecture)"
  built="$(realpath "dist/packages/${package}_${version}_${arch}.deb")"
  test -f "$built"
  if test -e "$BASELINE_DEB"; then
    cmp -s "$built" "$BASELINE_DEB"
  else
    install -m 0644 "$built" "$BASELINE_DEB"
  fi
)
sha256sum "$BASELINE_DEB"
git worktree remove "$BASELINE_WT"
```

Рабочую VM laboratory, её disks, artifact и evidence не удалять.

### Задача 16: Провести Mint/Cinnamon/X11 package-first кампанию

**Среда:**

- Lab controller:
  `/home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab`
- Сохранённая VM: `mint-installed`, SSH forward `127.0.0.1:22223`
- Evidence:
  `/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/`
- Артефакты: exact baseline и candidate DEB из задачи 15

- [ ] **Шаг 1: запустить сохранённую VM без удаления overlay**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session mint-installed
```

Ожидается: JSON с profile `mint-installed`, PID, QMP path и port `22223`.
Сессию не запускать одновременно с Ubuntu.

- [ ] **Шаг 2: передать exact packages и probe**

Собрать probe:

```bash
H06_WT=/home/andrey/Projects/OpenSwitcher/.worktrees/h06-synthetic-input-ledger
(
  cd "$H06_WT"
  cargo build --release --locked --example h06_x11_vm_probe
)
```

Передать candidate/baseline DEB и
`target/release/examples/h06_x11_vm_probe` через loopback SSH/SCP с ключом:

```bash
KEY=/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
KNOWN_HOSTS=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
BASELINE_DEB=/home/andrey/VMs/OpenSwitcherLab/artifacts/open-switcher_h06-baseline_9e894b2c00a57b9f74a5ccb293a21b1bcebc04d1_amd64.deb
package="$(cd "$H06_WT" && dpkg-parsechangelog -S Source)"
version="$(cd "$H06_WT" && dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
CANDIDATE_DEB="$H06_WT/dist/packages/${package}_${version}_${arch}.deb"
PROBE="$H06_WT/target/release/examples/h06_x11_vm_probe"
test -f "$CANDIDATE_DEB"
test -x "$PROBE"
ssh -i "$KEY" -p 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes \
  openswitcher@127.0.0.1 'install -d -m 0700 /home/openswitcher/h06'
scp -i "$KEY" -P 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes \
  "$CANDIDATE_DEB" "$BASELINE_DEB" "$PROBE" \
  openswitcher@127.0.0.1:/home/openswitcher/h06/
ssh -i "$KEY" -p 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'cd /home/openswitcher/h06 && sha256sum -- *'
```

Перед установкой в guest сверить оба SHA-256 с host evidence.

- [ ] **Шаг 3: снять baseline normal trace и timing**

Установить baseline DEB. В guest Cinnamon/X11 запустить probe `observe`, а
через QMP physical keyboard выполнить 30 одинаковых коррекций короткого слова
через F12. Сохранить:

```text
h06-baseline-trace.jsonl
h06-baseline-timing.json
h06-baseline-package.sha256
```

Сравниваются exact raw press/release order и медиана от первого correction
Backspace press до последнего replay release.

- [ ] **Шаг 4: установить candidate как upgrade/reinstall**

Во время активного baseline daemon установить exact candidate DEB. Проверить:

- old daemon/guardian отсутствуют;
- новый daemon PID отличается;
- `/proc/<pid>/exe` не содержит `(deleted)`;
- guardian PID запущен только после первого Cinnamon XTEST подключения;
- daemon и guardian имеют разные `ControlGroup`;
- socket mode `0600`, parent directory `0700`.

- [ ] **Шаг 5: пройти normal functional matrix**

Через QEMU physical keyboard проверить:

1. F12 current/last word correction;
2. auto correction;
3. Caps Lock correction;
4. исправление двух заглавных;
5. separator;
6. shifted symbol;
7. EN/RU XKB group switch;
8. Copy/Paste selected-text smoke;
9. Enter/Tab/click context reset без регрессии pointer motion/touch.

После каждой операции probe `assert-key-up` подтверждает отсутствие лишнего
synthetic down.

- [ ] **Шаг 6: проверить trace и performance gates**

Повторить 30 коррекций и сохранить candidate JSON. Требования:

```text
candidate press/release sequence == baseline sequence
candidate median end-to-end <= baseline median * 1.10
guardian debug p95_us <= 1000
max correction plan не получает transaction timeout
```

Включать только input debug:

```text
OPEN_SWITCHER_INPUT_DEBUG=1
OPEN_SWITCHER_INPUT_DEBUG_FILE=/tmp/h06-input-debug.log
```

Лог должен содержать только aggregate protocol latency, без текста и полного
key trace.

Если `p95_us > 1000` либо медиана хуже baseline более чем на 10%, merge
запрещён. Сохранить evidence и выполнить обязательную bounded batching ветку:

1. в задаче 5 добавить ручные bounded сообщения
   `PrepareKeys` (не более 32 logical keys) и `ExecuteSegment` (не более восьми
   ссылок `{Down|Up, token_id}` плюс один final X11 barrier);
2. размер `ExecuteSegment` по-прежнему не превышает `MAX_FRAME_BYTES=128`,
   heap-sized payload и произвольная строка не появляются;
3. daemon до отправки segment записывает в mirror все token, для которых
   segment содержит `Down`; после lost/partial ACK не удаляет ни один
   неоднозначный debt и никогда не повторяет down;
4. guardian перед каждой normal mutation сначала обновляет authoritative
   ledger, проверяет исходный `MutationDeadlineNs`, а debt после cleanup `Up`
   проверяет неизменный `CleanupDeadlineNs` и удаляет только
   после общего подтверждённого barrier;
5. segment объединяет только соседние mutations, между которыми сейчас нет
   deliberate wait; существующие 1/2/N ms waits остаются daemon-side границами
   segment и не переносятся;
6. добавить codec bounds, partial-failure-at-N, lost ACK, expired-mid-segment,
   mirror-before-batch и golden trace tests;
7. повторить задачи 5, 7, 9 и все safe gates задачи 15, собрать новый exact
   DEB, затем заново выполнить текущую Mint campaign от baseline и Ubuntu smoke.

Эта ветка не считается необязательной оптимизацией после превышения порога:
без её зелёных повторных gates H-06 не передаётся на merge.

- [ ] **Шаг 7: уничтожить daemon после реального XTEST down**

Probe `kill-on-press` следит за synthetic Backspace keycode и посылает
`SIGKILL` daemon PID внутри guest. Gate:

- guardian делает matching up и round-trip;
- key становится up не позднее 2s;
- QEMU physical marker после аварии вводится;
- systemd запускает новый daemon;
- новый guardian session не пересекается со старой.

- [ ] **Шаг 8: уничтожить guardian после реального XTEST down**

Повторить с guardian PID. Gate:

- daemon terminal gate запрещает новые mutations;
- physical grab освобождён раньше emergency up;
- заранее открытое emergency connection отпускает точный token либо журнал
  честно фиксирует `Unreconciled`;
- daemon завершается с ошибкой, а не продолжает через uinput fallback;
- после systemd restart physical input работает.

- [ ] **Шаг 9: проверить systemd/package lifecycle**

Выполнить paced серию 20 `restart` и 20 `stop/start`. После каждого цикла:

- не более одного daemon и одного active guardian;
- нет orphan/zombie;
- разные cgroup;
- keyboard keymap чист;
- physical marker вводится.

Затем повторить same-version reinstall и убедиться, что новый PID использует
текущий inode. Remove/purge должны сначала остановить daemon, дождаться guardian
drain, затем остановить socket/service.

- [ ] **Шаг 10: сохранить evidence и корректно выключить VM**

Сохранить JSONL, timing, journal, unit/cgroup/PID snapshots и screenshots с
префиксом `h06-20260728-`. Выключить guest через существующий runner/QMP.
Overlay, artifacts, evidence и всю лабораторию оставить на месте.

### Задача 17: Провести Ubuntu/GNOME/Wayland smoke тем же DEB

**Среда:**

- Сохранённая VM: `ubuntu-installed`, SSH forward `127.0.0.1:22222`
- Evidence:
  `/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/`

- [ ] **Шаг 1: запустить Ubuntu отдельно от Mint**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session ubuntu-installed
```

Ожидается: profile `ubuntu-installed`, port `22222`.

- [ ] **Шаг 2: установить тот же candidate SHA-256**

Передать тот же host-файл без новой сборки:

```bash
KEY=/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
KNOWN_HOSTS=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
H06_WT=/home/andrey/Projects/OpenSwitcher/.worktrees/h06-synthetic-input-ledger
package="$(cd "$H06_WT" && dpkg-parsechangelog -S Source)"
version="$(cd "$H06_WT" && dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
CANDIDATE_DEB="$H06_WT/dist/packages/${package}_${version}_${arch}.deb"
test -f "$CANDIDATE_DEB"
ssh -i "$KEY" -p 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes \
  openswitcher@127.0.0.1 'install -d -m 0700 /home/openswitcher/h06'
scp -i "$KEY" -P 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$CANDIDATE_DEB" \
  openswitcher@127.0.0.1:/home/openswitcher/h06/
ssh -i "$KEY" -p 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'cd /home/openswitcher/h06 && sha256sum -- *.deb'
```

Сверить guest SHA-256 с Mint и host. Новый package build между профилями
запрещён.

- [ ] **Шаг 3: проверить отсутствие лишнего guardian process**

На GNOME/Wayland:

- guardian socket может быть active из-за daemon `Wants=`;
- guardian service/process должен оставаться inactive;
- daemon writer-ready и uinput path работают без X11/XTEST handshake.

- [ ] **Шаг 4: выполнить полный uinput functional smoke**

Через QEMU physical keyboard проверить F12, auto correction, Caps Lock, две
заглавные, separator, shifted symbol, layout switch и selected-text Copy/Paste.
Проверить stop/restart и ввод после daemon SIGKILL.

- [ ] **Шаг 5: проверить package upgrade/remove order**

Повторить active reinstall и remove/purge. Старый daemon не остаётся из
`(deleted)` inode, guardian socket удаляется, а обычная package input/ACL policy
остаётся в границах отдельных findings аудита.

- [ ] **Шаг 6: сохранить evidence и выключить VM**

Сохранить package SHA, journal, PID/unit snapshots, functional results и
screenshots с префиксом `h06-20260728-`. VM/lab не удалять.

### Задача 18: Итоговая проверка, отчёт и передача на merge

**Файлы:**

- Создать: `docs/audits/2026-07-28-h06-runtime-validation.md`
- При необходимости изменить: только доказанно ошибочные H-06 файлы

- [ ] **Шаг 1: написать русский отчёт по фактическим результатам**

Отчёт обязан содержать:

- source commit и exact DEB SHA-256;
- safe host test commands и counts;
- Mint/Ubuntu окружения;
- normal trace/timing/p95;
- daemon/guardian death outcomes;
- stop/restart/upgrade/remove results;
- все `Reconciled` и `Unreconciled`;
- непроверенные сценарии и остаточные риски;
- явное подтверждение, что лаборатория сохранена.

- [ ] **Шаг 2: выполнить self-review против спецификации**

Для каждого release gate 1–11 из спецификации указать task/evidence. Пробел не
маскировать формулировкой «в целом работает»: либо добавить проверку, либо
оставить gate failed и не merge.

- [ ] **Шаг 3: выполнить placeholder/type scan плана и кода**

```bash
rg -n "CommitPhysicalState" src/daemon src/error
rg -n "xtest_fake_input|protocol::xtest::ConnectionExt" src -g '*.rs'
git diff --check
```

Ожидается: первый поиск не находит незавершённого H-06 контракта; второй
показывает production XTEST mutation только в guardian X11 executor.

- [ ] **Шаг 4: запросить двухступенчатое code review**

Сначала проверить соответствие спецификации, затем качество/безопасность
реализации. Исправлять только конкретные найденные дефекты; после каждого
исправления повторять targeted test и соответствующий release gate.

- [ ] **Шаг 5: повторить финальные safe gates**

```bash
cargo fmt --check
cargo test --locked --all-targets --features settings-ui -- --test-threads=1
cargo check --locked --all-targets --features settings-ui
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
git diff --check
```

Ожидается: всё PASS на том же source commit, из которого построен проверенный
DEB. Если review изменил production code, прежний DEB evidence устаревает:
пересобрать artifact и повторить обе package-first VM gates.

- [ ] **Шаг 6: зафиксировать отчёт**

```bash
git add docs/audits/2026-07-28-h06-runtime-validation.md
git commit -m "docs: record H-06 runtime validation"
```

- [ ] **Шаг 7: остановиться перед интеграцией**

Показать пользователю:

- список H-06 commits;
- test/VM evidence;
- exact package path и SHA-256;
- остаточные риски;
- чистый status worktree.

Merge в `master`, push и удаление worktree выполняются только после отдельного
подтверждения пользователя. VM laboratory не удаляется ни при каком варианте
завершения H-06 без отдельной прямой просьбы.
