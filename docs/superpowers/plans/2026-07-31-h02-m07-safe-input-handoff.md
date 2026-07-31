# H-02 residual + M-07 Safe Input Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** открывать runtime fd физической клавиатуры только непосредственно
перед безопасным `EVIOCGRAB`, не пересылать уже доставленные desktop события и
немедленно восстанавливать backend при `POLLHUP`, `POLLERR` или `POLLNVAL`.

**Architecture:** сохранить существующее разделение
`PreparedKeyboardController`/`KeyboardController`, но до activation хранить
только `VerifiedInputDevice`, а не live `evdev::Device`. Один общий
poll-adapter классифицирует timeout/readable/device-loss; bounded quiescent
handoff очищает pre-grab очередь, ждёт отпущенные клавиши и только затем
разрешает grab.

**Tech Stack:** Rust 2021, `evdev 0.12`, Linux `poll(2)`/`EVIOCGKEY`/
`EVIOCGRAB`, существующий input lifecycle, Cargo unit/fault-injection tests,
Debian package, сохранённая QEMU/KVM лаборатория.

---

## Основание

Согласованная спецификация:

- `docs/superpowers/specs/2026-07-31-h02-m07-safe-input-handoff-design.md`

Релевантные текущие исправления, которые нельзя ослабить:

- `c1a998b` — writer/watchers готовы до grab;
- `adab951` — release-first и process fail-stop при неотвечающем writer;
- `370c92a` — session/seat lease до и после grab;
- `317cd4d` — отзыв backend при смене session.

## Граница и структура файлов

Новые production-модули не создаются: lifecycle физического устройства уже
локализован в `src/daemon/keyboard.rs`, а отдельный abstraction ради двух
маленьких state helpers увеличит число границ без самостоятельного владельца.

Изменяемые файлы:

- `src/error/mod.rs:92-169,226-240,tests` — typed busy/device-loss errors и
  recoverability;
- `src/daemon/keyboard.rs:1067-1071,1505-1530,1525-1577,1855-2023,3714-3739,
  tests` — deferred open, poll classification, quiet handoff и grab ordering;
- `src/daemon/input_backend.rs:24-40,tests` — typed runtime recovery и startup
  retry;
- `debian/changelog` — пакет `0.1.0-6`;
- `docs/audits/2026-07-30-audit-remediation-status.md` — статус H-02/M-07
  только после фактических gates;
- `docs/audits/2026-07-31-h02-m07-input-handoff-validation.md` — exact commit,
  DEB SHA и целевое VM evidence.

Не менять алгоритмы коррекции, layout backend, clipboard, guardian, udev/ACL,
systemd units и VM-lab tooling.

## Правило gates

Каждая code task выполняет только свой focused RED/GREEN. Полный Rust suite,
package shell gates и DEB build запускаются один раз в Task 4. Если focused
test обнаруживает соседний blocker, исправляется только воспроизводимая
причина, после чего продолжается текущая task без повторения несвязанных gates.

---

### Task 1: Typed terminal poll и M-07 recovery

**Files:**

- Modify: `src/error/mod.rs`
- Modify: `src/daemon/keyboard.rs`
- Modify: `src/daemon/input_backend.rs`

- [ ] **Step 1: написать failing tests чистой классификации `poll`**

В test module `src/daemon/keyboard.rs` добавить:

```rust
#[test]
fn physical_input_poll_timeout_and_readable_are_distinct() {
    assert_eq!(
        classify_device_poll_result(0, 0),
        DevicePollOutcome::TimedOut
    );
    assert_eq!(
        classify_device_poll_result(1, libc::POLLIN),
        DevicePollOutcome::Readable
    );
}

#[test]
fn physical_input_poll_terminal_flags_are_device_loss() {
    for revents in [libc::POLLHUP, libc::POLLERR, libc::POLLNVAL] {
        assert_eq!(
            classify_device_poll_result(1, revents),
            DevicePollOutcome::DeviceLost { revents }
        );
    }
}

#[test]
fn physical_input_poll_terminal_flag_wins_over_readable() {
    let revents = libc::POLLIN | libc::POLLHUP;
    assert_eq!(
        classify_device_poll_result(1, revents),
        DevicePollOutcome::DeviceLost { revents }
    );
}

#[test]
fn physical_input_poll_unexpected_positive_is_not_a_timeout() {
    assert_eq!(
        classify_device_poll_result(1, 0),
        DevicePollOutcome::DeviceLost { revents: 0 }
    );
}
```

- [ ] **Step 2: написать failing tests typed errors и lifecycle**

В `src/error/mod.rs`:

```rust
#[test]
fn physical_keyboard_device_loss_is_recoverable() {
    let error = SwitcherError::PhysicalKeyboardDeviceLost {
        path: PathBuf::from("/dev/input/event5"),
        poll_events: libc::POLLHUP,
    };

    assert!(error.is_recoverable_input_error());
}
```

В `src/daemon/input_backend.rs`:

```rust
#[test]
fn typed_runtime_device_loss_enters_recovering() {
    assert_eq!(
        runtime_failure_recovery_state(
            &SwitcherError::PhysicalKeyboardDeviceLost {
                path: PathBuf::from("/dev/input/event5"),
                poll_events: libc::POLLHUP,
            }
        ),
        Some(InputBackendState::Recovering)
    );
}
```

- [ ] **Step 3: запустить focused tests и подтвердить RED**

Run:

```bash
cargo test --locked --lib physical_input_poll_ -- --nocapture
cargo test --locked --lib physical_keyboard_device_loss_is_recoverable -- --nocapture
cargo test --locked --lib typed_runtime_device_loss_enters_recovering -- --nocapture
```

Expected: compile failure — `DevicePollOutcome`,
`classify_device_poll_result` и `PhysicalKeyboardDeviceLost` ещё не
существуют.

- [ ] **Step 4: добавить typed error и recovery mapping**

В `SwitcherError`:

```rust
#[error(
    "Physical keyboard device became unavailable: {path} \
     (poll events=0x{poll_events:x})"
)]
PhysicalKeyboardDeviceLost {
    path: PathBuf,
    poll_events: libc::c_short,
},
```

Добавить вариант в `is_recoverable_input_error()`:

```rust
SwitcherError::PhysicalKeyboardDeviceLost { .. } => true,
```

Добавить в `runtime_failure_recovery_state()`:

```rust
SwitcherError::PhysicalKeyboardDeviceLost { .. }
| SwitcherError::InputWorkerDisconnected { .. } => {
    Some(InputBackendState::Recovering)
}
```

Существующую совместимость с `io::Error`/`ENODEV` пока сохранить: ошибки
`Device::fetch_events()` на старых путях всё ещё могут приходить как `Io`.

- [ ] **Step 5: реализовать единственную классификацию raw `poll`**

Рядом с `wait_for_device_input()`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevicePollOutcome {
    TimedOut,
    Readable,
    DeviceLost { revents: libc::c_short },
}

fn classify_device_poll_result(
    result: libc::c_int,
    revents: libc::c_short,
) -> DevicePollOutcome {
    debug_assert!(result >= 0);
    if result == 0 {
        return DevicePollOutcome::TimedOut;
    }

    let terminal = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    if revents & terminal != 0 || revents & libc::POLLIN == 0 {
        return DevicePollOutcome::DeviceLost { revents };
    }

    DevicePollOutcome::Readable
}
```

Изменить return type `wait_for_device_input()` на
`io::Result<DevicePollOutcome>` и при `result >= 0` возвращать результат
классификатора. Обработку `EINTR` и остальных syscall errors оставить
существующей.

- [ ] **Step 6: не давать terminal flags попасть в `fetch_events()`**

В `GrabbedKeyboardDevice` добавить mapping с path:

```rust
fn wait_for_input(
    &self,
    timeout: Duration,
) -> Result<InputWaitOutcome, SwitcherError> {
    let device = self.device.as_ref().ok_or(
        SwitcherError::InputWorkerDisconnected {
            worker: "keyboard-device",
        },
    )?;

    match wait_for_device_input(device, timeout)? {
        DevicePollOutcome::TimedOut => Ok(InputWaitOutcome::TimedOut),
        DevicePollOutcome::Readable => Ok(InputWaitOutcome::Readable),
        DevicePollOutcome::DeviceLost { revents } => {
            Err(SwitcherError::PhysicalKeyboardDeviceLost {
                path: self.path.clone(),
                poll_events: revents,
            })
        }
    }
}
```

Определить рядом:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputWaitOutcome {
    TimedOut,
    Readable,
}
```

И заменить `fetch_events_timeout()`:

```rust
match self.wait_for_input(timeout)? {
    InputWaitOutcome::TimedOut => Ok(Vec::new()),
    InputWaitOutcome::Readable => self.fetch_events(),
}
```

- [ ] **Step 7: получить GREEN и проверить формат**

Run:

```bash
cargo test --locked --lib physical_input_poll_ -- --nocapture
cargo test --locked --lib physical_keyboard_device_loss_is_recoverable -- --nocapture
cargo test --locked --lib typed_runtime_device_loss_enters_recovering -- --nocapture
cargo fmt --check
```

Expected: все focused tests PASS; форматирование не меняет код.

- [ ] **Step 8: закоммитить M-07**

```bash
git add src/error/mod.rs src/daemon/keyboard.rs src/daemon/input_backend.rs
git commit -m "fix: recover on physical input poll failure"
```

---

### Task 2: Deferred open и bounded quiescent handoff

**Files:**

- Modify: `src/error/mod.rs`
- Modify: `src/daemon/keyboard.rs`
- Modify: `src/daemon/input_backend.rs`

- [ ] **Step 1: написать failing tests pure quiet policy**

Добавить в `src/daemon/keyboard.rs`:

```rust
#[test]
fn quiet_handoff_accepts_an_idle_device_after_one_window() {
    let start = Instant::now();
    let mut target = ();
    let outcome = wait_for_quiescent_input(
        &mut target,
        Duration::from_millis(20),
        start + Duration::from_millis(100),
        || start,
        |_| Ok::<_, SwitcherError>(0),
        |_| Ok::<_, SwitcherError>(false),
        |_, _| Ok::<_, SwitcherError>(InputWaitOutcome::TimedOut),
    )
    .unwrap();

    assert_eq!(
        outcome,
        QuiescentHandoffOutcome::Ready {
            discarded_events: 0
        }
    );
}

#[test]
fn quiet_handoff_held_key_reaches_deadline_without_grab_permission() {
    let start = Instant::now();
    let mut times = vec![start, start + Duration::from_millis(100)].into_iter();
    let mut target = ();
    let outcome = wait_for_quiescent_input(
        &mut target,
        Duration::from_millis(20),
        start + Duration::from_millis(100),
        || times.next().unwrap_or(start + Duration::from_millis(100)),
        |_| Ok::<_, SwitcherError>(0),
        |_| Ok::<_, SwitcherError>(true),
        |_, _| Ok::<_, SwitcherError>(InputWaitOutcome::TimedOut),
    )
    .unwrap();

    assert!(matches!(
        outcome,
        QuiescentHandoffOutcome::Busy { .. }
    ));
}

#[test]
fn quiet_handoff_discards_pre_grab_activity_and_restarts_window() {
    let start = Instant::now();
    let mut waits = vec![
        InputWaitOutcome::Readable,
        InputWaitOutcome::TimedOut,
        InputWaitOutcome::TimedOut,
    ]
    .into_iter();
    let mut discards = vec![2usize, 3, 0].into_iter();
    let mut target = ();

    let outcome = wait_for_quiescent_input(
        &mut target,
        Duration::from_millis(20),
        start + Duration::from_millis(100),
        || start,
        |_| Ok::<_, SwitcherError>(discards.next().unwrap_or(0)),
        |_| Ok::<_, SwitcherError>(false),
        |_, _| {
            Ok::<_, SwitcherError>(
                waits.next().unwrap_or(InputWaitOutcome::TimedOut),
            )
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        QuiescentHandoffOutcome::Ready {
            discarded_events: 5
        }
    );
}

#[test]
fn quiet_handoff_final_readable_poll_requires_another_discard_cycle() {
    let start = Instant::now();
    let mut waits = vec![
        InputWaitOutcome::TimedOut,
        InputWaitOutcome::Readable,
        InputWaitOutcome::TimedOut,
        InputWaitOutcome::TimedOut,
    ]
    .into_iter();
    let mut discards = vec![0usize, 0, 1, 0].into_iter();
    let mut target = ();

    let outcome = wait_for_quiescent_input(
        &mut target,
        Duration::from_millis(20),
        start + Duration::from_millis(100),
        || start,
        |_| Ok::<_, SwitcherError>(discards.next().unwrap_or(0)),
        |_| Ok::<_, SwitcherError>(false),
        |_, _| {
            Ok::<_, SwitcherError>(
                waits.next().unwrap_or(InputWaitOutcome::TimedOut),
            )
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        QuiescentHandoffOutcome::Ready {
            discarded_events: 1
        }
    );
}
```

- [ ] **Step 2: написать failing ordering/rollback tests**

Сначала зафиксировать отсутствие live fd в prepared состоянии:

```rust
#[test]
fn pending_physical_keyboard_keeps_identity_without_live_fd() {
    let device = GrabbedKeyboardDevice::pending(VerifiedInputDevice {
        canonical_path: PathBuf::from("/dev/input/event5"),
        devnum: 0x0d05,
        seat: Arc::from("seat0"),
    });

    assert_eq!(device.path, PathBuf::from("/dev/input/event5"));
    assert!(device.device.is_none());
    assert!(!device.grabbed);
}
```

Заменить тест `caps_lock_snapshot_is_taken_immediately_before_physical_grab`
на:

```rust
#[test]
fn grab_is_validated_before_caps_lock_snapshot_and_publish() {
    struct FakeKeyboard {
        phases: Vec<&'static str>,
        caps_lock_active: bool,
    }

    let mut keyboard = FakeKeyboard {
        phases: Vec::new(),
        caps_lock_active: true,
    };
    let caps = acquire_grab_then_snapshot(
        &mut keyboard,
        || Ok::<_, SwitcherError>(()),
        |keyboard| {
            keyboard.phases.push("grab");
            Ok::<_, SwitcherError>(())
        },
        || Ok::<_, SwitcherError>(()),
        |keyboard| {
            keyboard.phases.push("post-grab-key-check");
            Ok::<_, SwitcherError>(())
        },
        |keyboard| {
            keyboard.phases.push("caps-snapshot");
            Ok::<_, SwitcherError>(keyboard.caps_lock_active)
        },
        |_| Ok::<_, SwitcherError>(()),
    )
    .unwrap();

    assert!(caps);
    assert_eq!(
        keyboard.phases,
        vec!["grab", "post-grab-key-check", "caps-snapshot"]
    );
}
```

Добавить второй test: ошибка `post-grab-key-check` обязана дать trace
`["grab", "post-grab-key-check", "release"]`, snapshot не вызывается.

- [ ] **Step 3: запустить focused tests и подтвердить RED**

Run:

```bash
cargo test --locked --lib quiet_handoff_ -- --nocapture
cargo test --locked --lib pending_physical_keyboard_ -- --nocapture
cargo test --locked --lib grab_is_validated_before_caps_lock_snapshot -- --nocapture
```

Expected: compile failure — quiet policy и новый grab helper ещё не
существуют.

- [ ] **Step 4: добавить busy error**

В `SwitcherError`:

```rust
#[error("Physical keyboard is active during safe input handoff: {path}")]
PhysicalKeyboardHandoffBusy { path: PathBuf },
```

В `is_recoverable_input_error()`:

```rust
SwitcherError::PhysicalKeyboardHandoffBusy { .. } => true,
```

Добавить unit test, подтверждающий recoverability и отсутствие setup hint.

- [ ] **Step 5: реализовать deterministic quiet policy**

Рядом с grab helpers:

```rust
const INPUT_HANDOFF_QUIET_WINDOW: Duration = Duration::from_millis(20);
const INPUT_HANDOFF_MAX_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuiescentHandoffOutcome {
    Ready { discarded_events: usize },
    Busy { discarded_events: usize },
}

fn wait_for_quiescent_input<T, E>(
    target: &mut T,
    quiet_window: Duration,
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut discard_ready: impl FnMut(&mut T) -> Result<usize, E>,
    mut any_key_pressed: impl FnMut(&mut T) -> Result<bool, E>,
    mut wait_once: impl FnMut(&mut T, Duration) -> Result<InputWaitOutcome, E>,
) -> Result<QuiescentHandoffOutcome, E> {
    let mut discarded_events = 0usize;

    loop {
        discarded_events =
            discarded_events.saturating_add(discard_ready(target)?);
        let observed_now = now();
        if observed_now >= deadline {
            return Ok(QuiescentHandoffOutcome::Busy {
                discarded_events,
            });
        }

        if any_key_pressed(target)? {
            let wait = deadline
                .saturating_duration_since(observed_now)
                .min(quiet_window);
            let _ = wait_once(target, wait)?;
            continue;
        }

        let remaining = deadline.saturating_duration_since(observed_now);
        if remaining < quiet_window {
            return Ok(QuiescentHandoffOutcome::Busy {
                discarded_events,
            });
        }

        match wait_once(target, quiet_window)? {
            InputWaitOutcome::Readable => continue,
            InputWaitOutcome::TimedOut => {
                discarded_events = discarded_events
                    .saturating_add(discard_ready(target)?);
                if any_key_pressed(target)? {
                    continue;
                }
                if matches!(
                    wait_once(target, Duration::ZERO)?,
                    InputWaitOutcome::TimedOut
                ) {
                    return Ok(QuiescentHandoffOutcome::Ready {
                        discarded_events,
                    });
                }
            }
        }
    }
}
```

Helper не вызывает grab и поэтому чисто решает только выдачу разрешения.

- [ ] **Step 6: заменить early open на pending identity**

Расширить `GrabbedKeyboardDevice`:

```rust
struct GrabbedKeyboardDevice {
    verified: VerifiedInputDevice,
    path: PathBuf,
    device: Option<Device>,
    grabbed: bool,
}
```

Заменить `open()` двумя переходами:

```rust
fn pending(verified: VerifiedInputDevice) -> Self {
    Self {
        path: verified.canonical_path.clone(),
        verified,
        device: None,
        grabbed: false,
    }
}

fn open_for_handoff(&mut self) -> Result<(), SwitcherError> {
    if self.device.is_some() || self.grabbed {
        return Err(SwitcherError::input_safety(
            "physical keyboard opened twice during handoff",
        ));
    }
    let device = Device::open(&self.path)
        .map_err(|error| map_keyboard_open_error(&self.path, error))?;
    verify_open_device_identity(&device, &self.verified)?;
    self.device = Some(device);
    Ok(())
}

fn close_ungrabbed(&mut self) {
    debug_assert!(!self.grabbed);
    drop(self.device.take());
}
```

В `KeyboardController::prepare()` заменить
`GrabbedKeyboardDevice::open(keyboard_device)?` на
`GrabbedKeyboardDevice::pending(keyboard_device)`. Сообщение с именем
клавиатуры перенести в activation после `open_for_handoff()`.

- [ ] **Step 7: добавить реальные adapter operations для handoff**

В `GrabbedKeyboardDevice`:

```rust
fn any_key_pressed(&self) -> Result<bool, SwitcherError> {
    self.device
        .as_ref()
        .ok_or(SwitcherError::InputWorkerDisconnected {
            worker: "keyboard-device",
        })?
        .get_key_state()
        .map(|keys| keys.iter().next().is_some())
        .map_err(|error| map_keyboard_open_error(&self.path, error))
}

fn discard_ready_events(&mut self) -> Result<usize, SwitcherError> {
    let mut discarded = 0usize;
    loop {
        match self.wait_for_input(Duration::ZERO)? {
            InputWaitOutcome::TimedOut => return Ok(discarded),
            InputWaitOutcome::Readable => {
                discarded =
                    discarded.saturating_add(self.fetch_events()?.len());
            }
        }
    }
}
```

Не переводить fd в `O_NONBLOCK`: `evdev_poll()` выставляет readable только
для готового полного пакета, а существующий runtime уже использует
`poll -> fetch_events`.

- [ ] **Step 8: заменить grab helper на post-grab snapshot**

Реализовать:

```rust
fn acquire_grab_then_snapshot<T, Snapshot, Error>(
    target: &mut T,
    precheck: impl FnOnce() -> Result<(), Error>,
    acquire_grab: impl FnOnce(&mut T) -> Result<(), Error>,
    postcheck: impl FnOnce() -> Result<(), Error>,
    validate_grab: impl FnOnce(&mut T) -> Result<(), Error>,
    snapshot: impl FnOnce(&mut T) -> Result<Snapshot, Error>,
    release_grab: impl FnOnce(&mut T) -> Result<(), Error>,
) -> Result<Snapshot, Error> {
    precheck()?;
    acquire_grab(target)?;

    let result = postcheck()
        .and_then(|()| validate_grab(target))
        .and_then(|()| snapshot(target));
    match result {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => match release_grab(target) {
            Ok(()) => Err(error),
            Err(release_error) => Err(release_error),
        },
    }
}
```

Сохранить существующие session-generation tests, адаптировав ожидаемый trace
к новому порядку.

- [ ] **Step 9: встроить handoff в `PreparedKeyboardController::activate()`**

Всю последовательность выполнить внутри одного `activation_result`, чтобы ни
один новый ранний `return` не обходил существующий writer shutdown:

```rust
let activation_result = (|| -> Result<(bool, usize), SwitcherError> {
    self.lease.ensure_current(monotonic_ms())?;
    ensure_input_dependencies_ready(
        self.controller.virtual_device.handle().is_alive(),
        self.controller.pointer_watcher.is_ready(),
        self.controller.input_target_watcher.is_ready(),
    )?;
    self.controller.real_device.open_for_handoff()?;

    let deadline = Instant::now() + INPUT_HANDOFF_MAX_WAIT;
    let handoff = wait_for_quiescent_input(
        &mut self.controller.real_device,
        INPUT_HANDOFF_QUIET_WINDOW,
        deadline,
        Instant::now,
        GrabbedKeyboardDevice::discard_ready_events,
        GrabbedKeyboardDevice::any_key_pressed,
        GrabbedKeyboardDevice::wait_for_input,
    )?;

    let discarded_events = match handoff {
        QuiescentHandoffOutcome::Ready { discarded_events } => {
            discarded_events
        }
        QuiescentHandoffOutcome::Busy { .. } => {
            let path = self.controller.real_device.path.clone();
            self.controller.real_device.close_ungrabbed();
            return Err(SwitcherError::PhysicalKeyboardHandoffBusy { path });
        }
    };

    ensure_input_dependencies_ready(
        self.controller.virtual_device.handle().is_alive(),
        self.controller.pointer_watcher.is_ready(),
        self.controller.input_target_watcher.is_ready(),
    )?;
    let path = self.controller.real_device.path.clone();
    let caps_lock_active = acquire_grab_then_snapshot(
        &mut self.controller.real_device,
        || self.lease.ensure_current(monotonic_ms()),
        GrabbedKeyboardDevice::grab,
        || self.lease.ensure_current(monotonic_ms()),
        |device| {
            if device.any_key_pressed()? {
                Err(SwitcherError::PhysicalKeyboardHandoffBusy {
                    path: path.clone(),
                })
            } else {
                Ok(())
            }
        },
        |device| {
            Ok::<_, SwitcherError>(
                device.caps_lock_active().unwrap_or(false),
            )
        },
        GrabbedKeyboardDevice::release_grab,
    )?;
    Ok((caps_lock_active, discarded_events))
})();

let (caps_lock_active, discarded_events) = match activation_result {
    Ok(active) => active,
    Err(error) => {
        let outcome = self.controller.shutdown();
        return Err(resolve_error_after_writer_shutdown(
            error,
            "keyboard-activate-handoff",
            outcome,
        ));
    }
};
```

Логировать только outcome, `discarded_events`, path и причину; keycodes и
содержимое событий не логировать.

- [ ] **Step 10: получить GREEN**

Run:

```bash
cargo test --locked --lib quiet_handoff_ -- --nocapture
cargo test --locked --lib pending_physical_keyboard_ -- --nocapture
cargo test --locked --lib grab_is_validated_before_caps_lock_snapshot -- --nocapture
cargo test --locked --lib activation_ -- --nocapture
cargo fmt --check
```

Expected: focused tests PASS; старые session/grab rollback tests также PASS.

- [ ] **Step 11: закоммитить H-02 residual**

```bash
git add src/error/mod.rs src/daemon/keyboard.rs
git commit -m "fix: acquire physical input only after quiet handoff"
```

---

### Task 3: Lifecycle regressions и package identity

**Files:**

- Modify: `src/daemon/input_backend.rs`
- Modify: `src/daemon/keyboard.rs`
- Modify: `debian/changelog`

- [ ] **Step 1: покрыть startup busy retry**

В test `FakeOutcome` добавить:

```rust
HandoffBusy,
```

В `FakeOpener::reopen_backend()`:

```rust
FakeOutcome::HandoffBusy => {
    Err(SwitcherError::PhysicalKeyboardHandoffBusy {
        path: PathBuf::from("/dev/input/event5"),
    })
}
```

Test:

```rust
#[test]
fn busy_physical_handoff_schedules_retry_without_active_backend() {
    let now = Instant::now();
    let mut lifecycle = InputBackendLifecycle::new(FakeOpener {
        outcome: FakeOutcome::HandoffBusy,
    });

    let opened = lifecycle
        .initialize(SharedModifierState::default(), now)
        .unwrap();

    assert!(opened.is_none());
    assert_eq!(lifecycle.state(), InputBackendState::WaitingForInputAccess);
    assert!(lifecycle.retry_deadline().is_some_and(|deadline| deadline > now));
    assert!(lifecycle
        .last_error()
        .is_some_and(|error| error.contains("safe input handoff")));
}
```

- [ ] **Step 2: покрыть post-grab failure и unplug release**

В `src/daemon/keyboard.rs` добавить:

```rust
#[test]
fn post_grab_pressed_key_releases_without_caps_snapshot() {
    let trace = RefCell::new(Vec::new());
    let mut target = ();
    let result = acquire_grab_then_snapshot(
        &mut target,
        || Ok::<_, SwitcherError>(()),
        |_| {
            trace.borrow_mut().push("grab");
            Ok::<_, SwitcherError>(())
        },
        || Ok::<_, SwitcherError>(()),
        |_| {
            trace.borrow_mut().push("post-grab-key-check");
            Err(SwitcherError::PhysicalKeyboardHandoffBusy {
                path: PathBuf::from("/dev/input/event5"),
            })
        },
        |_| {
            trace.borrow_mut().push("caps-snapshot");
            Ok::<_, SwitcherError>(false)
        },
        |_| {
            trace.borrow_mut().push("release");
            Ok::<_, SwitcherError>(())
        },
    );

    assert!(matches!(
        result,
        Err(SwitcherError::PhysicalKeyboardHandoffBusy { .. })
    ));
    assert_eq!(
        *trace.borrow(),
        vec!["grab", "post-grab-key-check", "release"]
    );
}

#[test]
fn failed_release_overrides_post_grab_validation_error() {
    let mut target = ();
    let result = acquire_grab_then_snapshot(
        &mut target,
        || Ok::<_, SwitcherError>(()),
        |_| Ok::<_, SwitcherError>(()),
        || Ok::<_, SwitcherError>(()),
        |_| Err(SwitcherError::InputSessionInactive),
        |_| Ok::<_, SwitcherError>(false),
        |_| {
            Err::<(), _>(SwitcherError::input_safety(
                "post-grab release failed",
            ))
        },
    );

    assert!(matches!(
        result,
        Err(SwitcherError::InputSafety(
            InputSafetyError::Invariant {
                context: "post-grab release failed"
            }
        ))
    ));
}
```

Отдельно проверить close-on-`ENODEV`:

```rust
#[test]
fn unplug_release_error_closes_the_physical_fd_state() {
    let mut device = Some(());
    let mut grabbed = true;
    let result = release_grab_or_close_device(
        &mut device,
        &mut grabbed,
        |_| Err::<(), _>(io::Error::from_raw_os_error(libc::ENODEV)),
    );

    assert_eq!(
        result.unwrap_err().raw_os_error(),
        Some(libc::ENODEV)
    );
    assert!(device.is_none());
    assert!(!grabbed);
}
```

- [ ] **Step 3: выполнить lifecycle focused gate**

Run:

```bash
cargo test --locked --lib busy_physical_handoff_ -- --nocapture
cargo test --locked --lib unplug_release_error_ -- --nocapture
cargo test --locked --lib typed_runtime_device_loss_ -- --nocapture
cargo test --locked --lib input_backend -- --nocapture
```

Expected: PASS без новых detached threads/fds в fake counters.

- [ ] **Step 4: подготовить отдельную версию DEB**

Добавить верхнюю запись `debian/changelog`:

```text
open-switcher (0.1.0-6) unstable; urgency=high

  * Close the remaining H-02 startup handoff gap by opening and grabbing the
    physical keyboard only after the input pipeline is ready and input is
    briefly quiescent.
  * Recover immediately when evdev poll reports HUP, ERR, or an invalid file
    descriptor.

 -- Andrey Mukha <6871314+andreymukha@users.noreply.github.com>  Fri, 31 Jul 2026 12:09:35 +0300
```

- [ ] **Step 5: закоммитить lifecycle tests и package version**

```bash
git add src/daemon/input_backend.rs src/daemon/keyboard.rs debian/changelog
git commit -m "test: close physical input recovery regressions"
```

---

### Task 4: Один общий gate, exact DEB и целевая VM-проверка

**Files:**

- Create: `docs/audits/2026-07-31-h02-m07-input-handoff-validation.md`
- Modify: `docs/audits/2026-07-30-audit-remediation-status.md`

- [ ] **Step 1: выполнить полный безопасный source gate один раз**

Run:

```bash
cargo fmt --check
cargo test --locked --all-targets
git diff --check
bash tests/debian_package_scripts_test.sh
bash tests/input_access_package_test.sh
bash tests/manage_package_deb_test.sh
```

Expected:

- Rust: 0 failed; существующие explicitly ignored tests остаются ignored;
- каждый shell test печатает PASS/успешный итог и завершает exit 0;
- `git diff --check` не печатает ошибок.

Если `tests/manage_package_deb_test.sh` внутри sandbox снова не может создать
свои mock parent artifacts, повторить только этот test вне sandbox и
зафиксировать обе команды/exit codes. Не менять production код ради sandbox
ограничения теста.

- [ ] **Step 2: собрать canonical package**

Run:

```bash
./manage.sh package deb
```

Expected:

- package version `0.1.0-6`;
- canonical artifact
  `dist/packages/open-switcher_0.1.0-6_amd64.deb`;
- desktop validation PASS;
- временные parent-directory artifacts удалены.

Зафиксировать identity:

```bash
dpkg-deb --field dist/packages/open-switcher_0.1.0-6_amd64.deb \
  Package Version Architecture
sha256sum dist/packages/open-switcher_0.1.0-6_amd64.deb
git rev-parse HEAD
```

- [ ] **Step 3: выполнить targeted package-first VM case**

Использовать сохранённый Mint/Cinnamon/X11 overlay
`/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/disk.qcow2` и
существующий VM-lab launch/SSH/QMP путь. Не создавать новый универсальный
runner и не менять host network.

Внутри гостя установить exact SHA package через:

```bash
sudo apt install --reinstall ./open-switcher_0.1.0-6_amd64.deb
```

Проверить последовательно:

1. запустить daemon при удерживаемой QEMU keyboard key; до release в debug log
   нет `grab-acquired`, прямой desktop input остаётся жив;
2. отпустить key; не позднее следующего lifecycle retry появляется active
   backend и обычный текст проходит ровно один раз;
3. начать QMP input burst длительностью больше `100 ms` одновременно с
   restart daemon; burst до grab не повторяется через virtual device;
4. hot-unplug только виртуальной QEMU keyboard; daemon фиксирует typed
   device-loss и переходит в recovery;
5. hot-replug; backend восстанавливается без restart daemon;
6. ввести `Ctrl`, `Alt`, `Shift`, обычный текст и F12 smoke; модификаторы не
   остаются нажатыми, основные функции работают;
7. повторить unplug/replug три раза и проверить отсутствие второго active
   backend и роста открытых event fd после каждого завершённого recovery.

Сохранить exact QEMU/QMP/guest commands, journal, input debug log, fd counts и
package SHA в новый каталог evidence внутри сохранённого run. Никакой
физический host input device в гостя не передавать.

- [ ] **Step 4: классифицировать результат без подмены evidence**

Pass разрешён только если одновременно:

- pre-grab burst не дублируется;
- held key не захватывается посередине;
- terminal poll немедленно вызывает recovery;
- replug восстанавливает тот же daemon;
- нет stuck modifiers и второго backend.

Если hot-unplug не даёт управляемо воспроизвести `POLLHUP/ERR` на выбранной
модели QEMU, результат M-07 runtime пометить `inconclusive`, сохранив unit
evidence. Не объявлять его runtime-pass только по отсутствию сбоя.

- [ ] **Step 5: записать validation report и обновить audit status**

Создать
`docs/audits/2026-07-31-h02-m07-input-handoff-validation.md` со следующими
обязательными полями:

```text
source commit
DEB path/version/size/SHA-256
guest profile/kernel/session
exact commands
H-02 held-key result
H-02 pre-grab duplicate count
M-07 unplug flags and recovery timing
replug/fd/backend-count result
functional smoke result
limitations/inconclusive cases
evidence paths
```

В `docs/audits/2026-07-30-audit-remediation-status.md`:

- H-02 -> `Закрыто`, только если unit + targeted VM handoff прошли;
- M-07 -> `Закрыто`, если terminal flags runtime подтверждены;
- при inconclusive VM оставить формулировку
  `исходный дефект исправлен и unit-покрыт; runtime hot-unplug не подтверждён`;
- обновить общий test count и следующий приоритет без изменения истории
  остальных findings.

- [ ] **Step 6: выполнить финальную diff/commit проверку**

Run:

```bash
git diff --check
git status --short
git diff --stat master...HEAD
git log --oneline master..HEAD
```

Убедиться, что пользовательские `.gitignore` и старые untracked audit/VM
документы не staged и не входят в commits.

- [ ] **Step 7: закоммитить evidence**

```bash
git add \
  docs/audits/2026-07-31-h02-m07-input-handoff-validation.md \
  docs/audits/2026-07-30-audit-remediation-status.md
git commit -m "docs: validate safe physical input handoff"
```

- [ ] **Step 8: запросить code review перед merge**

Использовать `superpowers:requesting-code-review`, затем исправить только
конкретные воспроизводимые замечания. После review повторить affected focused
tests; полный gate повторять лишь при изменении production behavior после
первого полного gate.

После зелёного review использовать
`superpowers:finishing-a-development-branch`. Не merge и не push без
отдельного подтверждения пользователя после package-first результата.
