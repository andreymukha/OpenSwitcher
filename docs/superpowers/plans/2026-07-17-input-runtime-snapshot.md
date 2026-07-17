# Input Runtime Snapshot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep normal layout switching and correction behavior while ensuring that grabbed input never waits for configuration persistence, desktop commands, or synchronous layout-backend refresh.

**Architecture:** A focused `daemon::input_snapshot` module owns the immutable input decision snapshot, freshness/generation fences, a nonblocking publication cell, and a capacity-one refresh wakeup. `RuntimeState` publishes committed configuration and background-confirmed layout state; `DaemonService` keeps a service-local copy and fails open for physical events whenever layout state is unconfirmed.

**Tech Stack:** Rust 2021, `std::sync::{RwLock, mpsc::sync_channel}`, atomics, monotonic `Instant`, existing fake layout backends, Cargo tests, Debian package build, retained Ubuntu and Linux Mint VMs.

---

## File map

- Create `src/daemon/input_snapshot.rs`: snapshot model, freshness and authorization rules, publication cell, refresh request queue, and focused unit tests.
- Modify `src/daemon/mod.rs`: register the module and replace public startup sync with an explicit pre-grab initial refresh.
- Modify `src/daemon/runtime.rs`: publish initial/config/layout state, serialize committed settings publication, and run external refresh only in the background coordinator.
- Modify `src/daemon/service.rs`: own a local snapshot, remove synchronous runtime/backend calls, preserve the fresh path, and fence pending corrections.
- Modify `src/dbus/mod.rs`: source status signals from the last confirmed published snapshot.
- Create `docs/audits/2026-07-17-h01-input-runtime-snapshot-validation.md`: exact local/package/VM evidence and remaining bounded-command limitation.

### Task 1: Pure snapshot, freshness, and publication boundary

**Files:**

- Create: `src/daemon/input_snapshot.rs`
- Modify: `src/daemon/mod.rs`

- [ ] **Step 1: Write RED tests for freshness and authorization**

Add focused tests using explicit `Instant` values. They must distinguish normal polling from an invalidation window and must prove that an unchanged confirmation does not invalidate a pending operation:

```rust
#[test]
fn confirmed_snapshot_remains_fresh_between_poll_ticks() {
    let confirmed_at = Instant::now();
    let snapshot = test_snapshot(known_layout(AppLayoutKind::English), Some(confirmed_at), 7);

    assert_eq!(
        snapshot.layout_status_at(confirmed_at + Duration::from_millis(900), 7),
        InputLayoutStatus::Fresh
    );
}

#[test]
fn invalidation_epoch_disables_layout_actions_immediately() {
    let now = Instant::now();
    let snapshot = test_snapshot(known_layout(AppLayoutKind::English), Some(now), 7);

    assert_eq!(
        snapshot.layout_status_at(now + Duration::from_millis(1), 8),
        InputLayoutStatus::AwaitingConfirmation
    );
    assert_eq!(snapshot.layout_kind_for_decision_at(now, 8), None);
}

#[test]
fn confirmation_expires_after_freshness_bound() {
    let confirmed_at = Instant::now();
    let snapshot = test_snapshot(known_layout(AppLayoutKind::Russian), Some(confirmed_at), 3);

    assert_eq!(
        snapshot.layout_status_at(confirmed_at + INPUT_LAYOUT_FRESHNESS + Duration::from_nanos(1), 3),
        InputLayoutStatus::Stale
    );
}

#[test]
fn pending_authorization_survives_same_state_reconfirmation() {
    let now = Instant::now();
    let snapshot = test_snapshot(known_layout(AppLayoutKind::English), Some(now), 4);
    let authorization = snapshot.authorization_at(now, 4).unwrap();
    let reconfirmed = InputRuntimeSnapshot {
        confirmed_at: Some(now + Duration::from_millis(300)),
        ..snapshot.clone()
    };

    assert!(reconfirmed.authorizes_at(
        authorization,
        now + Duration::from_millis(301),
        4
    ));
}
```

- [ ] **Step 2: Run the focused RED test**

Run:

```bash
cargo test --lib input_snapshot -- --nocapture
```

Expected: compile failure because `daemon::input_snapshot` and its types do not exist.

- [ ] **Step 3: Implement the snapshot model**

Register `pub(crate) mod input_snapshot;` and implement the explicit state model. `layout_generation` changes only when the effective layout value changes; a successful unchanged poll updates `confirmed_at` without changing that generation.

```rust
pub(crate) const INPUT_LAYOUT_POLL_INTERVAL: Duration = Duration::from_millis(300);
pub(crate) const INPUT_LAYOUT_FRESHNESS: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputLayoutStatus {
    Fresh,
    AwaitingConfirmation,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotAuthorization {
    pub config_generation: u64,
    pub layout_generation: u64,
    pub layout_epoch: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct InputRuntimeSnapshot {
    pub config: RuntimeConfigSnapshot,
    pub enabled: bool,
    pub features: FeatureAvailability,
    pub session_type: SessionType,
    pub layout_state: CurrentLayoutState,
    pub config_generation: u64,
    pub layout_generation: u64,
    pub confirmed_layout_epoch: u64,
    pub confirmed_at: Option<Instant>,
}

impl InputRuntimeSnapshot {
    pub(crate) fn layout_status_at(
        &self,
        now: Instant,
        current_layout_epoch: u64,
    ) -> InputLayoutStatus {
        if self.confirmed_layout_epoch != current_layout_epoch {
            return InputLayoutStatus::AwaitingConfirmation;
        }
        if matches!(self.layout_state, CurrentLayoutState::Unknown { .. }) {
            return InputLayoutStatus::Unknown;
        }
        let Some(confirmed_at) = self.confirmed_at else {
            return InputLayoutStatus::Unknown;
        };
        if now
            .checked_duration_since(confirmed_at)
            .unwrap_or_default()
            >= INPUT_LAYOUT_FRESHNESS
        {
            return InputLayoutStatus::Stale;
        }
        InputLayoutStatus::Fresh
    }

    pub(crate) fn layout_kind_for_decision_at(
        &self,
        now: Instant,
        current_layout_epoch: u64,
    ) -> Option<AppLayoutKind> {
        (self.layout_status_at(now, current_layout_epoch) == InputLayoutStatus::Fresh)
            .then(|| current_layout_kind(&self.layout_state))
    }

    pub(crate) fn authorization_at(
        &self,
        now: Instant,
        current_layout_epoch: u64,
    ) -> Option<SnapshotAuthorization> {
        self.layout_kind_for_decision_at(now, current_layout_epoch)?;
        Some(SnapshotAuthorization {
            config_generation: self.config_generation,
            layout_generation: self.layout_generation,
            layout_epoch: current_layout_epoch,
        })
    }

    pub(crate) fn authorizes_at(
        &self,
        authorization: SnapshotAuthorization,
        now: Instant,
        current_layout_epoch: u64,
    ) -> bool {
        self.authorization_at(now, current_layout_epoch) == Some(authorization)
    }
}
```

- [ ] **Step 4: Add the nonblocking publication cell tests**

```rust
#[test]
fn contended_publication_returns_without_waiting() {
    let publication = InputSnapshotPublication::new(default_test_snapshot());
    let _guard = publication.inner.write().unwrap();

    assert!(matches!(publication.try_load(), SnapshotTryLoad::Contended));
}

#[test]
fn poisoned_publication_is_explicit_and_non_panicking() {
    let publication = Arc::new(InputSnapshotPublication::new(default_test_snapshot()));
    let poison_target = Arc::clone(&publication);
    let _ = thread::spawn(move || {
        let _guard = poison_target.inner.write().unwrap();
        panic!("poison publication");
    })
    .join();

    assert!(matches!(publication.try_load(), SnapshotTryLoad::Poisoned));
}
```

- [ ] **Step 5: Implement the publication cell**

```rust
#[derive(Clone, Debug)]
pub(crate) enum SnapshotTryLoad {
    Loaded(InputRuntimeSnapshot),
    Contended,
    Poisoned,
}

pub(crate) struct InputSnapshotPublication {
    inner: RwLock<InputRuntimeSnapshot>,
}

impl InputSnapshotPublication {
    pub(crate) fn new(initial: InputRuntimeSnapshot) -> Self {
        Self {
            inner: RwLock::new(initial),
        }
    }

    pub(crate) fn load_before_grab(&self) -> InputRuntimeSnapshot {
        self.inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(crate) fn load_for_non_input_consumer(&self) -> InputRuntimeSnapshot {
        self.load_before_grab()
    }

    pub(crate) fn try_load(&self) -> SnapshotTryLoad {
        match self.inner.try_read() {
            Ok(snapshot) => SnapshotTryLoad::Loaded(snapshot.clone()),
            Err(TryLockError::WouldBlock) => SnapshotTryLoad::Contended,
            Err(TryLockError::Poisoned(_)) => SnapshotTryLoad::Poisoned,
        }
    }

    pub(crate) fn update(&self, update: impl FnOnce(&mut InputRuntimeSnapshot)) {
        let mut snapshot = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        update(&mut snapshot);
    }
}
```

- [ ] **Step 6: Run GREEN and commit**

Run:

```bash
cargo test --lib input_snapshot -- --nocapture
cargo check --all-targets
```

Expected: all `input_snapshot` tests pass and all targets compile.

Commit:

```bash
git add src/daemon/input_snapshot.rs src/daemon/mod.rs
git commit -m "feat: add input runtime snapshot model"
```

### Task 2: Publish only committed configuration

**Files:**

- Modify: `src/daemon/input_snapshot.rs`
- Modify: `src/daemon/runtime.rs`

- [ ] **Step 1: Write RED tests for config publication isolation**

Add tests proving the input-facing cell is independent from
`ConfigService::inner` and failed saves do not advance its generation. Place
them in `#[cfg(test)] mod input_snapshot_config_tests` so the focused command
below cannot silently select zero tests:

```rust
#[test]
fn held_config_write_lock_does_not_block_input_snapshot_read() {
    let runtime = test_runtime();
    let _config_guard = runtime.config_service.inner.write().unwrap();

    assert!(matches!(
        runtime.try_input_snapshot(),
        SnapshotTryLoad::Loaded(_)
    ));
}

#[test]
fn failed_settings_save_does_not_publish_new_config_generation() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join("config-as-directory");
    std::fs::create_dir(&config_path).unwrap();
    let runtime = test_runtime_with_config_path(config_path);
    let before = runtime.input_snapshot_before_grab();
    let mut settings = runtime.get_settings().unwrap();
    settings.fix_two_capitals = !settings.fix_two_capitals;

    assert!(runtime.update_settings(settings).is_err());
    let after = runtime.input_snapshot_before_grab();
    assert_eq!(after.config_generation, before.config_generation);
    assert_eq!(after.config.fix_two_capitals, before.config.fix_two_capitals);
}

#[test]
fn successful_settings_save_publishes_one_complete_generation() {
    let temp = TempDir::new().unwrap();
    let runtime = test_runtime_with_config_path(temp.path().join("config.toml"));
    let before = runtime.input_snapshot_before_grab();
    let mut settings = runtime.get_settings().unwrap();
    settings.fix_two_capitals = true;

    runtime.update_settings(settings).unwrap();
    let after = runtime.input_snapshot_before_grab();
    assert_eq!(after.config_generation, before.config_generation + 1);
    assert!(after.config.fix_two_capitals);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib input_snapshot_config -- --nocapture
```

Expected: compile failure because `RuntimeState` does not expose an independent input snapshot publication.

- [ ] **Step 3: Add the publication and settings gate to `RuntimeState`**

Construct the initial value before moving runtime components, and keep publication writes outside filesystem I/O:

```rust
input_snapshot: InputSnapshotPublication,
settings_update_gate: Mutex<()>,
layout_invalidation_epoch: AtomicU64,
```

Build the initial publication with `confirmed_at: None`; the explicit startup
refresh in Task 3 is what first authorizes correction:

```rust
let initial_input_snapshot = InputRuntimeSnapshot {
    config: config_service.snapshot().unwrap_or_else(|_| {
        RuntimeConfigSnapshot::from(&AppConfig::default())
    }),
    enabled,
    features: feature_availability.clone(),
    session_type: system_context.session_type,
    layout_state: layout_state.clone(),
    config_generation: 0,
    layout_generation: 0,
    confirmed_layout_epoch: 0,
    confirmed_at: None,
};
```

Add the input-facing methods:

```rust

pub(crate) fn input_snapshot_before_grab(&self) -> InputRuntimeSnapshot {
    self.input_snapshot.load_before_grab()
}

pub(crate) fn try_input_snapshot(&self) -> SnapshotTryLoad {
    self.input_snapshot.try_load()
}

pub(crate) fn input_layout_epoch(&self) -> u64 {
    self.layout_invalidation_epoch.load(Ordering::Acquire)
}
```

Implement a short publication update that advances the config generation only after `ConfigService::update_settings` succeeds:

```rust
fn publish_committed_config(&self, config: RuntimeConfigSnapshot, enabled: bool) {
    self.input_snapshot.update(|published| {
        published.config = config;
        published.enabled = enabled;
        published.config_generation = published.config_generation.saturating_add(1);
    });
}

pub fn update_settings(
    &self,
    settings: Settings,
) -> Result<UpdateSettingsResult, SettingsError> {
    let _gate = self
        .settings_update_gate
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let result = self.config_service.update_settings(settings)?;
    let snapshot = self.config_service.snapshot()?;
    self.enabled
        .store(settings.auto_switch_enabled, Ordering::SeqCst);
    self.publish_committed_config(snapshot, settings.auto_switch_enabled);
    Ok(result)
}
```

Refactor `toggle_enabled_result` to acquire the same gate once and call a private under-gate helper rather than recursively locking it.

- [ ] **Step 4: Run GREEN and regression tests**

Run:

```bash
cargo test --lib input_snapshot_config -- --nocapture
cargo test --lib runtime -- --nocapture
cargo test --test dbus_api -- --test-threads=1
```

Expected: focused tests and existing runtime/D-Bus settings tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/input_snapshot.rs src/daemon/runtime.rs
git commit -m "fix: publish committed input configuration"
```

### Task 3: Move layout refresh behind a bounded background coordinator

**Files:**

- Modify: `src/daemon/input_snapshot.rs`
- Modify: `src/daemon/runtime.rs`
- Modify: `src/daemon/mod.rs`

- [ ] **Step 1: Write RED request-queue and confirmation tests**

Place these tests in `#[cfg(test)] mod layout_refresh_tests`.

```rust
#[test]
fn refresh_requests_coalesce_at_capacity_one() {
    let (requests, receiver) = LayoutRefreshRequests::for_test();

    assert_eq!(requests.request(), RefreshRequestOutcome::Queued);
    assert_eq!(requests.request(), RefreshRequestOutcome::AlreadyPending);
    assert_eq!(receiver.try_recv(), Ok(()));
}

#[test]
fn disconnected_refresh_request_never_becomes_an_input_error() {
    let (requests, receiver) = LayoutRefreshRequests::for_test();
    drop(receiver);

    assert_eq!(requests.request(), RefreshRequestOutcome::Unavailable);
}

#[test]
fn backend_error_preserves_value_without_extending_freshness() {
    let confirmed_at = Instant::now();
    let runtime = test_runtime_with_backend(
        known_layout_state(english_layout()),
        Box::new(SnapshotBackend { snapshot: SnapshotOutcome::Error }),
    );
    runtime.force_input_confirmation_for_test(confirmed_at);

    assert_eq!(runtime.refresh_and_publish_layout(), BackendSyncResult::Skipped);
    let snapshot = runtime.input_snapshot_before_grab();
    assert_eq!(snapshot.confirmed_at, Some(confirmed_at));
    assert_eq!(current_layout_kind(&snapshot.layout_state), AppLayoutKind::English);
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib layout_refresh -- --nocapture
```

Expected: compile failure because the request queue and publishing refresh cycle do not exist.

- [ ] **Step 3: Implement capacity-one refresh requests**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshRequestOutcome {
    Queued,
    AlreadyPending,
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct LayoutRefreshRequests {
    sender: mpsc::SyncSender<()>,
}

impl LayoutRefreshRequests {
    pub(crate) fn new() -> (Self, mpsc::Receiver<()>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        (Self { sender }, receiver)
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, mpsc::Receiver<()>) {
        Self::new()
    }

    pub(crate) fn request(&self) -> RefreshRequestOutcome {
        match self.sender.try_send(()) {
            Ok(()) => RefreshRequestOutcome::Queued,
            Err(mpsc::TrySendError::Full(())) => RefreshRequestOutcome::AlreadyPending,
            Err(mpsc::TrySendError::Disconnected(())) => RefreshRequestOutcome::Unavailable,
        }
    }
}
```

- [ ] **Step 4: Implement one publish-after-refresh cycle**

Keep `sync_with_backend`, `periodic_sync_tick`, and desktop observation functions private to `runtime.rs`. The public startup method is explicitly named as pre-grab:

```rust
pub(crate) fn initial_input_refresh_before_grab(&self) -> BackendSyncResult {
    self.refresh_and_publish_layout()
}

fn refresh_and_publish_layout(&self) -> BackendSyncResult {
    let epoch_before = self.input_layout_epoch();
    let result = self.periodic_sync_tick();
    let epoch_after = self.input_layout_epoch();
    let confirmed = !matches!(result, BackendSyncResult::Skipped)
        && epoch_before == epoch_after;
    self.publish_layout_snapshot(confirmed.then_some((Instant::now(), epoch_after)));
    result
}
```

`publish_layout_snapshot` clones context/features/effective layout before taking
the publication write lock. It increments `layout_generation` only when a
successful confirmation changes the effective layout value. A failed refresh
does not replace the published layout/context/features or extend
`confirmed_at`; the last confirmed value remains available for status display
while its age independently disables input transformations.

- [ ] **Step 5: Replace sleeping polling with a wakeable coordinator**

Use the capacity-one receiver with `INPUT_LAYOUT_POLL_INTERVAL`:

```rust
fn run_layout_refresh_loop(runtime: Arc<RuntimeState>, receiver: mpsc::Receiver<()>) {
    loop {
        match receiver.recv_timeout(INPUT_LAYOUT_POLL_INTERVAL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if runtime.should_exit() {
            break;
        }
        let _ = runtime.refresh_and_publish_layout();
    }
    runtime.background_sync_started.store(false, Ordering::Release);
}
```

Store the sender directly in `RuntimeState` and the not-yet-started receiver as
`Mutex<Option<mpsc::Receiver<()>>>`. `start_background_sync_polling` takes that
receiver exactly once before `DaemonService::new` can grab input. Spawn failure
drops the receiver, so later `try_send` calls return `Unavailable`; repeated
start attempts cannot create multiple coordinators.

```rust
layout_refresh_requests: LayoutRefreshRequests,
layout_refresh_receiver: Mutex<Option<mpsc::Receiver<()>>>,
```

Create both with `LayoutRefreshRequests::new()` in every production/test
`RuntimeState` constructor and remove the old
`BACKGROUND_SYNC_POLL_INTERVAL` constant in favor of the shared
`INPUT_LAYOUT_POLL_INTERVAL`.

Wrap the loop in an RAII alive guard that clears `background_sync_started`
during normal return and unwinding. Add injected spawn-failure and
panicking-backend tests proving both cases leave snapshot reads available and
make refresh requests return `Unavailable` after the receiver disappears.

Use this injection boundary so tests never replace the process-wide thread
API:

```rust
fn start_layout_refresh_coordinator_with(
    self: &Arc<Self>,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
) -> io::Result<()> {
    let receiver = self
        .layout_refresh_receiver
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .ok_or_else(|| io::Error::other("layout refresh already started"))?;
    self.background_sync_started.store(true, Ordering::Release);
    let runtime = Arc::clone(self);
    let job = Box::new(move || run_layout_refresh_loop(runtime, receiver));
    if let Err(error) = spawn(job) {
        self.background_sync_started.store(false, Ordering::Release);
        return Err(error);
    }
    Ok(())
}
```

```rust
#[test]
fn layout_refresh_spawn_failure_disconnects_without_blocking_snapshot() {
    let runtime = Arc::new(test_runtime());
    let error = runtime
        .start_layout_refresh_coordinator_with(|job| {
            drop(job);
            Err(io::Error::other("injected spawn failure"))
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(matches!(runtime.try_input_snapshot(), SnapshotTryLoad::Loaded(_)));
    assert_eq!(runtime.request_layout_refresh(), RefreshRequestOutcome::Unavailable);
}

#[test]
fn panicking_refresh_coordinator_degrades_to_snapshot_only() {
    let runtime = Arc::new(test_runtime_with_backend(
        known_layout_state(english_layout()),
        Box::new(PanickingSnapshotBackend),
    ));
    runtime
        .start_layout_refresh_coordinator_with(|job| {
            thread::Builder::new()
                .name("test-layout-refresh-panic".to_string())
                .spawn(job)
                .map(|_| ())
        })
        .unwrap();
    assert_eq!(runtime.request_layout_refresh(), RefreshRequestOutcome::Queued);

    let deadline = Instant::now() + Duration::from_secs(1);
    while runtime.background_sync_started.load(Ordering::Acquire)
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }

    assert!(!runtime.background_sync_started.load(Ordering::Acquire));
    assert!(matches!(runtime.try_input_snapshot(), SnapshotTryLoad::Loaded(_)));
    assert_eq!(runtime.request_layout_refresh(), RefreshRequestOutcome::Unavailable);
}
```

`request_layout_refresh` calls only `try_send`. `invalidate_layout_and_request_refresh` increments the atomic epoch before requesting refresh and never takes a lock:

```rust
pub(crate) fn request_layout_refresh(&self) -> RefreshRequestOutcome {
    self.layout_refresh_requests.request()
}

pub(crate) fn invalidate_layout_and_request_refresh(&self, reason: &str) {
    let epoch = self
        .layout_invalidation_epoch
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);
    let outcome = self.request_layout_refresh();
    log_layout_debug(
        "input-layout-invalidated",
        &format!("reason={reason} epoch={epoch} request={outcome:?}"),
    );
}
```

- [ ] **Step 6: Add a blocked-backend isolation test**

Use a barrier-controlled fake backend and release it before joining the test thread:

```rust
#[test]
fn blocked_background_backend_does_not_block_snapshot_reads() {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runtime = Arc::new(test_runtime_with_backend(
        known_layout_state(english_layout()),
        Box::new(BlockingSnapshotBackend::new(
            Arc::clone(&entered),
            Arc::clone(&release),
        )),
    ));
    let worker_runtime = Arc::clone(&runtime);
    let worker = thread::spawn(move || worker_runtime.refresh_and_publish_layout());
    entered.wait();

    assert!(matches!(
        runtime.try_input_snapshot(),
        SnapshotTryLoad::Loaded(_)
    ));

    release.wait();
    worker.join().unwrap();
}
```

- [ ] **Step 7: Run GREEN and commit**

Run:

```bash
cargo test --lib layout_refresh -- --nocapture
cargo test --lib runtime -- --nocapture
cargo check --all-targets
```

Expected: all tests pass; blocked fake backend is released by the test and leaves no detached test thread.

Commit:

```bash
git add src/daemon/input_snapshot.rs src/daemon/runtime.rs src/daemon/mod.rs
git commit -m "feat: refresh input snapshots off the input path"
```

### Task 4: Consume only the service-local snapshot under grab

**Files:**

- Modify: `src/daemon/service.rs`

- [ ] **Step 1: Add fresh-path behavior RED tests**

Extract pure decision helpers and encode the behavior that must not regress.
Place the tests in `#[cfg(test)] mod service_snapshot_fresh_tests`:

```rust
#[test]
fn fresh_snapshot_keeps_layout_auto_correction_enabled() {
    let now = Instant::now();
    let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);

    assert_eq!(
        layout_correction_decision(&snapshot, now, snapshot.confirmed_layout_epoch),
        LayoutCorrectionAvailability::Available(AppLayoutKind::English)
    );
}

#[test]
fn fresh_snapshot_keeps_caps_and_two_capitals_fixes_enabled() {
    let now = Instant::now();
    let mut snapshot = fresh_service_snapshot(AppLayoutKind::Russian, now);
    snapshot.config.fix_two_capitals = true;
    snapshot.config.fix_accidental_caps_lock = true;

    assert!(same_layout_fixes_allowed(
        &snapshot,
        now,
        snapshot.confirmed_layout_epoch
    ));
}

#[test]
fn fresh_snapshot_keeps_manual_correction_enabled() {
    let now = Instant::now();
    let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);

    assert!(manual_correction_allowed(
        &snapshot,
        now,
        snapshot.confirmed_layout_epoch
    ));
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib service_snapshot_fresh -- --nocapture
```

Expected: compile failure because `DaemonService` still reads runtime state synchronously.

- [ ] **Step 3: Add the service-local snapshot**

Load once before `try_initialize_input_backend`, then adopt only nonblocking publications:

```rust
pub struct DaemonService {
    runtime: Arc<RuntimeState>,
    input_snapshot: InputRuntimeSnapshot,
    // existing fields
}

pub fn new(runtime: Arc<RuntimeState>, connection: Connection) -> Result<Self, SwitcherError> {
    let input_snapshot = runtime.input_snapshot_before_grab();
    let mut service = Self {
        runtime,
        input_snapshot,
        // existing initialization
    };
    service.try_initialize_input_backend()?;
    Ok(service)
}

fn adopt_input_snapshot_nonblocking(&mut self) {
    match self.runtime.try_input_snapshot() {
        SnapshotTryLoad::Loaded(snapshot) => self.input_snapshot = snapshot,
        SnapshotTryLoad::Contended => {}
        SnapshotTryLoad::Poisoned => {
            log_layout_debug("input-snapshot-read", "result=poisoned action=keep-local");
        }
    }
}
```

Call the adoption method before each `handle_key_event` and before finishing delayed/pending corrections. Use `self.input_snapshot.config.clone()` rather than `runtime.config_snapshot()`.

- [ ] **Step 4: Replace runtime reads with local fields**

Add the pure decision boundary used by both fresh and stale tests:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutCorrectionAvailability {
    Available(AppLayoutKind),
    Unavailable(InputLayoutStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordBoundaryAction {
    Evaluate {
        layout_kind: AppLayoutKind,
        authorization: SnapshotAuthorization,
    },
    ForwardUncorrected(evdev::Key),
}

fn layout_correction_decision(
    snapshot: &InputRuntimeSnapshot,
    now: Instant,
    epoch: u64,
) -> LayoutCorrectionAvailability {
    match snapshot.layout_kind_for_decision_at(now, epoch) {
        Some(kind) => LayoutCorrectionAvailability::Available(kind),
        None => LayoutCorrectionAvailability::Unavailable(
            snapshot.layout_status_at(now, epoch),
        ),
    }
}

fn same_layout_fixes_allowed(
    snapshot: &InputRuntimeSnapshot,
    now: Instant,
    epoch: u64,
) -> bool {
    (snapshot.config.fix_two_capitals || snapshot.config.fix_accidental_caps_lock)
        && matches!(
            layout_correction_decision(snapshot, now, epoch),
            LayoutCorrectionAvailability::Available(
                AppLayoutKind::English | AppLayoutKind::Russian
            )
        )
}

fn manual_correction_allowed(
    snapshot: &InputRuntimeSnapshot,
    now: Instant,
    epoch: u64,
) -> bool {
    snapshot.features.manual_word_fix
        && matches!(
            layout_correction_decision(snapshot, now, epoch),
            LayoutCorrectionAvailability::Available(
                AppLayoutKind::English | AppLayoutKind::Russian
            )
        )
}

fn word_boundary_action(
    snapshot: &InputRuntimeSnapshot,
    now: Instant,
    epoch: u64,
    key: evdev::Key,
) -> WordBoundaryAction {
    match (
        snapshot.layout_kind_for_decision_at(now, epoch),
        snapshot.authorization_at(now, epoch),
    ) {
        (Some(layout_kind), Some(authorization)) => WordBoundaryAction::Evaluate {
            layout_kind,
            authorization,
        },
        _ => WordBoundaryAction::ForwardUncorrected(key),
    }
}
```

Replace these input-path calls:

- `runtime.is_enabled()` -> `input_snapshot.enabled`;
- `runtime.feature_availability()` -> `input_snapshot.features`;
- `runtime.current_layout_state()` / `current_layout()` / `auto_correction_layout_kind()` -> `input_snapshot.layout_state`;
- `runtime.session_type()` -> `input_snapshot.session_type`;
- `runtime.config_snapshot()` -> `input_snapshot.config`.

Keep only atomics, capture routing, writer health, nonblocking logging, snapshot `try_read`, and refresh `try_send` on the input control path.

- [ ] **Step 5: Remove synchronous service refreshes**

Delete service calls to:

```text
RuntimeState::sync_with_backend
RuntimeState::periodic_sync_tick
RuntimeState::refresh_current_layout_observation
RuntimeState::optimistic_gnome_wayland_uinput_layout_switch
```

Remove `StartupLayoutResyncState` and `refresh_startup_layout_before_autocorrect`; an unavailable startup layout is represented by `InputLayoutStatus::Unknown` and recovered by the coordinator.

When a correction outcome reports any applied layout switch, call
`invalidate_layout_and_request_refresh` only after the bounded writer operation
returns:

```rust
if matches!(
    outcome.layout_switch,
    CorrectionLayoutSwitchOutcome::AppliedUinput
        | CorrectionLayoutSwitchOutcome::AppliedX11
        | CorrectionLayoutSwitchOutcome::AppliedCinnamonDbusXtest
) {
    self.runtime
        .invalidate_layout_and_request_refresh("writer-layout-switch");
}
```

The deferred manual-current-word path has no detailed switch enum. Its
`Succeeded` completion and `FailedAfterMutation` completion both invalidate and
request confirmation because either outcome may follow a backend mutation.

- [ ] **Step 6: Add a source-boundary regression test**

Use split literals so the test does not match its own forbidden strings:

```rust
#[test]
fn daemon_service_has_no_synchronous_runtime_refresh_calls() {
    let source = include_str!("service.rs");
    for forbidden in [
        ["sync_", "with_backend"].concat(),
        ["periodic_", "sync_tick"].concat(),
        ["refresh_current_layout_", "observation"].concat(),
        ["optimistic_gnome_wayland_", "uinput_layout_switch"].concat(),
        ["config_", "snapshot()?"].concat(),
    ] {
        assert!(!source.contains(&forbidden), "forbidden input-path call: {forbidden}");
    }
}
```

- [ ] **Step 7: Run GREEN and commit**

Run:

```bash
cargo test --lib service_snapshot_fresh -- --nocapture
cargo test --lib daemon_service_has_no_synchronous_runtime_refresh_calls -- --nocapture
cargo test --lib service -- --nocapture
cargo check --all-targets
```

Expected: fresh-path tests preserve layout correction, Caps Lock/two-capitals fixes, and manual correction; source boundary passes.

Commit:

```bash
git add src/daemon/service.rs
git commit -m "fix: consume snapshots in the grabbed input path"
```

### Task 5: Fail open on stale state and fence pending corrections

**Files:**

- Modify: `src/daemon/service.rs`

- [ ] **Step 1: Add stale-path RED tests**

Place stale decision tests in
`#[cfg(test)] mod service_snapshot_stale_tests`.

```rust
#[test]
fn stale_layout_disables_all_layout_dependent_corrections() {
    let confirmed_at = Instant::now();
    let snapshot = fresh_service_snapshot(AppLayoutKind::English, confirmed_at);
    let stale_at = confirmed_at + INPUT_LAYOUT_FRESHNESS + Duration::from_millis(1);

    assert_eq!(
        layout_correction_decision(
            &snapshot,
            stale_at,
            snapshot.confirmed_layout_epoch
        ),
        LayoutCorrectionAvailability::Unavailable(InputLayoutStatus::Stale)
    );
    assert!(!same_layout_fixes_allowed(
        &snapshot,
        stale_at,
        snapshot.confirmed_layout_epoch
    ));
    assert!(!manual_correction_allowed(
        &snapshot,
        stale_at,
        snapshot.confirmed_layout_epoch
    ));
}

#[test]
fn stale_separator_path_forwards_instead_of_suppressing() {
    let confirmed_at = Instant::now();
    let snapshot = fresh_service_snapshot(AppLayoutKind::English, confirmed_at);
    let stale_at = confirmed_at + INPUT_LAYOUT_FRESHNESS;
    let outcome = word_boundary_action(
        &snapshot,
        stale_at,
        snapshot.confirmed_layout_epoch,
        Key::KEY_SPACE,
    );
    assert_eq!(outcome, WordBoundaryAction::ForwardUncorrected(Key::KEY_SPACE));
}
```

- [ ] **Step 2: Add pending-generation RED tests**

Extend `PendingWordCommit` with `SnapshotAuthorization` and test every
invalidation dimension. Place these tests in
`#[cfg(test)] mod pending_snapshot_authorization_tests`:

```rust
#[test]
fn pending_commit_is_cancelled_after_layout_generation_change() {
    let now = Instant::now();
    let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);
    let authorization = snapshot.authorization_at(now, 1).unwrap();
    let changed = InputRuntimeSnapshot {
        layout_generation: snapshot.layout_generation + 1,
        ..snapshot
    };

    assert!(!changed.authorizes_at(authorization, now, 1));
}

#[test]
fn cancelled_pending_commit_replays_separator_once() {
    let mut ledger = Vec::new();
    cancel_pending_commit_with(Key::KEY_SPACE, |key| {
        ledger.push(key);
        Ok::<(), ()>(())
    })
    .unwrap();

    assert_eq!(ledger, vec![Key::KEY_SPACE]);
}
```

- [ ] **Step 3: Run RED**

Run:

```bash
cargo test --lib service_snapshot_stale -- --nocapture
cargo test --lib pending_snapshot_authorization -- --nocapture
```

Expected: compile failure because stale routing and pending authorization are not implemented.

- [ ] **Step 4: Implement fail-open decisions**

At every layout-dependent decision, compute status from the local snapshot and current atomic epoch. When unavailable:

```rust
fn layout_kind_for_current_decision(&self, reason: &str) -> Option<AppLayoutKind> {
    let now = Instant::now();
    let epoch = self.runtime.input_layout_epoch();
    let kind = self.input_snapshot.layout_kind_for_decision_at(now, epoch);
    if kind.is_none() {
        let status = self.input_snapshot.layout_status_at(now, epoch);
        let request = self.runtime.request_layout_refresh();
        log_layout_debug(
            "layout-dependent-action-skip",
            &format!("reason={reason} status={status:?} request={request:?}"),
        );
    }
    kind
}
```

For Space/Enter/Tab with no eligible layout, commit the raw word state and call `keyboard.forward_event(key, value)`; do not set `suppressed_separator_key` or `pending_word_commit`.

For a matched physical layout shortcut, preserve latching and normal OS forwarding, invalidate word context, and request confirmation without guessing or publishing a provisional status.

- [ ] **Step 5: Fence and cancel pending commits**

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingWordCommit {
    separator_key: evdev::Key,
    action: PendingWordCommitAction,
    authorization: SnapshotAuthorization,
}

fn pending_commit_is_authorized(&self, pending: &PendingWordCommit) -> bool {
    self.input_snapshot.authorizes_at(
        pending.authorization,
        Instant::now(),
        self.runtime.input_layout_epoch(),
    )
}
```

Before executing either release-time or early-finish mutation, adopt a new snapshot and validate. On failure, reuse the existing raw commit state and separator typing path:

```rust
fn cancel_pending_commit_with<E>(
    separator_key: evdev::Key,
    replay: impl FnOnce(evdev::Key) -> Result<(), E>,
) -> Result<(), E> {
    replay(separator_key)
}

fn cancel_pending_word_commit(
    &mut self,
    separator_key: evdev::Key,
) -> Result<(), SwitcherError> {
    let raw_buffer = self.buffer.clone();
    cancel_pending_commit_with(separator_key, |key| {
        self.commit_corrected_word(key, raw_buffer)
    })
}
```

This emits the already-suppressed separator once; the physical release remains swallowed by the existing suppression ledger.

- [ ] **Step 6: Run GREEN and conservation regressions**

Run:

```bash
cargo test --lib service_snapshot_stale -- --nocapture
cargo test --lib pending_snapshot_authorization -- --nocapture
cargo test --lib separator -- --nocapture
cargo test --lib keyboard_ -- --nocapture
```

Expected: stale paths perform no correction, fresh paths remain enabled, and separator tests show one synthetic replay with no duplicate physical release.

- [ ] **Step 7: Commit**

```bash
git add src/daemon/service.rs
git commit -m "fix: fence corrections by snapshot freshness"
```

### Task 6: Publish status only from confirmed snapshots

**Files:**

- Modify: `src/daemon/service.rs`
- Modify: `src/daemon/runtime.rs`
- Modify: `src/dbus/mod.rs`

- [ ] **Step 1: Add confirmed-status RED tests**

When the background refresh changes effective layout, publication must precede
the pending-status flag. Provisional or stale snapshots are not signal sources.
Place these tests in `#[cfg(test)] mod status_snapshot_tests`:

```rust
#[test]
fn provisional_layout_does_not_publish_status() {
    assert!(!status_snapshot_is_publishable(
        InputLayoutStatus::AwaitingConfirmation
    ));
}

#[test]
fn confirmed_fresh_layout_publishes_status() {
    assert!(status_snapshot_is_publishable(InputLayoutStatus::Fresh));
}

fn status_snapshot_is_publishable(status: InputLayoutStatus) -> bool {
    matches!(status, InputLayoutStatus::Fresh)
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib status_snapshot -- --nocapture
```

Expected: the provisional-status test fails against current direct runtime
cache reads.

- [ ] **Step 3: Implement confirmed-only status publication**

In `DaemonService`, adopt the publication before sending `StatusChanged`; use
`input_snapshot.enabled` and the confirmed `input_snapshot.layout_state`. If
snapshot adoption is contended, restore/defer the pending atomic instead of
emitting an old value.

`OpenSwitcherDbusApi::toggle` and the enabled-change branch of
`update_settings` use a `RuntimeState::last_confirmed_status()` accessor instead
of `RuntimeState::current_layout()`, so a concurrent D-Bus call cannot publish
provisional runtime cache state. Enabled-state signals may carry the last
confirmed layout even when it is old; only a new layout-change signal requires
a fresh confirmation.

```rust
pub(crate) fn last_confirmed_status(&self) -> (bool, bool) {
    let snapshot = self.input_snapshot.load_for_non_input_consumer();
    (
        snapshot.enabled,
        legacy_current_layout_bool(&snapshot.layout_state),
    )
}
```

`load_for_non_input_consumer` uses the publication read lock and poison
recovery like `load_before_grab`; it is never called by `DaemonService` after
grab.

The selected-text worker is intentionally unchanged: its current transport
converts text through clipboard copy/paste and does not switch the system
layout.

- [ ] **Step 4: Run focused and module regression tests**

Run:

```bash
cargo test --lib status_snapshot -- --nocapture
cargo test --lib runtime -- --nocapture
cargo test --lib service -- --nocapture
cargo test --test dbus_api -- --test-threads=1
```

Expected: all focused and existing module/D-Bus tests pass; selected-text code
has no diff in this slice.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/service.rs src/daemon/runtime.rs src/dbus/mod.rs
git commit -m "fix: publish only confirmed layout status"
```

### Task 7: Full verification, Debian package, and two-profile validation

**Files:**

- Create: `docs/audits/2026-07-17-h01-input-runtime-snapshot-validation.md`

- [ ] **Step 1: Run formatting and complete safe local verification**

Run:

```bash
rustup run stable rustfmt --edition 2021 --check src/daemon/input_snapshot.rs src/daemon/runtime.rs src/daemon/service.rs src/dbus/mod.rs src/daemon/mod.rs
cargo check --all-targets
cargo check --all-targets --features settings-ui
cargo test --lib -- --test-threads=1
cargo test --lib --features settings-ui -- --test-threads=1
cargo test --test dbus_api -- --test-threads=1
bash tests/linux_input_setup_test.sh
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
git diff --check
```

Expected: all commands pass, except already known host-sandbox D-Bus/Unix-socket
`EPERM` cases if the command is run inside the restricted host sandbox. Any
such case must pass unchanged in the Mint guest before completion; the same
compiled/package code is also exercised in Ubuntu.

- [ ] **Step 2: Perform two-pass code review**

Pass 1 checks the approved spec invariants and searches the complete service source for synchronous external/config/layout calls. Pass 2 checks race behavior, generation semantics, separator conservation, and fresh-path compatibility. Resolve every Critical/High/Medium finding before package build.

Run the static searches:

```bash
rg -n "sync_with_backend|periodic_sync_tick|refresh_current_layout_observation|optimistic_gnome_wayland_uinput_layout_switch|config_snapshot" src/daemon/service.rs
rg -n "Command::new|\.output\(\)|\.status\(\)" src/daemon/service.rs src/daemon/input_snapshot.rs
```

Expected: the first command finds no synchronous runtime refresh call in `DaemonService`; the second finds no external command execution in either input-facing file.

- [ ] **Step 3: Build the canonical Debian package**

Run from the remediation worktree:

```bash
./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-1_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-1_amd64.deb
```

Expected: package build and embedded tests pass; record the exact SHA-256 and package metadata.

- [ ] **Step 4: Install the exact package in both retained VMs**

Use the retained SSH identity and loopback forwarding after starting the
existing Ubuntu (`127.0.0.1:22222`) and Mint (`127.0.0.1:22223`) guests. Install
the byte-identical package in both; the command below shows Mint and is repeated
with port `22222` for Ubuntu:

```bash
scp -P 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 \
  -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts \
  dist/packages/open-switcher_0.1.0-1_amd64.deb \
  openswitcher@127.0.0.1:/tmp/open-switcher-h01-snapshot.deb

ssh -p 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 \
  -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts \
  openswitcher@127.0.0.1 \
  'sudo dpkg -i /tmp/open-switcher-h01-snapshot.deb && systemctl --user restart open-switcher-daemon.service && systemctl --user is-active open-switcher-daemon.service'
```

Expected: package install succeeds in both profiles and each daemon unit reports
`active` from `/usr/bin/open-switcher-daemon`.

- [ ] **Step 5: Verify normal installed-package behavior**

Through the existing QEMU virtual keyboard and a harmless guest editor, verify
the fresh path in both Ubuntu/GNOME/Wayland and Mint/Cinnamon/X11:

- configured layout shortcut changes EN/RU;
- automatic wrong-layout word correction still works;
- manual correction hotkey still works;
- `fix_two_capitals` corrects a two-capital word;
- `fix_accidental_caps_lock` corrects an accidental Caps Lock word;
- ordinary text reaches the editor exactly once.

Capture the editor result and journal window under:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h01-snapshot-fresh-path.ppm
/home/andrey/VMs/OpenSwitcherLab/runs/ubuntu-cloud-provision-v1/h01-snapshot-fresh-journal.txt
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h01-snapshot-fresh-path.ppm
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h01-snapshot-fresh-journal.txt
```

Expected: behavior matches the pre-change package except for confirmed-status latency after an explicit layout change.

- [ ] **Step 6: Inject a post-start hanging `xset` in the guest only**

Create `/tmp/openswitcher-h01-bin/xset` in the guest so it delegates normally until `/tmp/openswitcher-h01-arm-hang` exists, then blocks. Put that directory first in the user-manager PATH, restart while unarmed, confirm the daemon is active, then arm the hang:

```bash
ssh -p 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 \
  -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts \
  openswitcher@127.0.0.1 \
  'mkdir -p /tmp/openswitcher-h01-bin && printf "%s\n" "#!/bin/sh" "if [ -e /tmp/openswitcher-h01-arm-hang ]; then sleep 600; fi" "exec /usr/bin/xset \"\$@\"" > /tmp/openswitcher-h01-bin/xset && chmod 755 /tmp/openswitcher-h01-bin/xset && rm -f /tmp/openswitcher-h01-arm-hang && systemctl --user set-environment PATH=/tmp/openswitcher-h01-bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin && systemctl --user restart open-switcher-daemon.service && systemctl --user is-active open-switcher-daemon.service && touch /tmp/openswitcher-h01-arm-hang'
```

Expected: the daemon starts normally before the marker exists; a later background refresh blocks in the guest-only fake command.

- [ ] **Step 7: Prove fail-open input and bounded release during the hang**

While SSH remains the independent control path:

1. Send a unique harmless string through the QEMU virtual keyboard and verify it appears exactly once in the editor.
2. Verify a layout-dependent correction is skipped after freshness expires rather than mutating text incorrectly.
3. Run `systemctl --user stop open-switcher-daemon.service` and measure elapsed monotonic time.
4. Send a second unique string through the same QEMU keyboard after stop.

Expected: both strings arrive, the stale correction is skipped, and stop releases input in less than one second even though the background `xset` remains blocked. Record screenshot/journal/timing evidence under `mint-install-v1` with prefix `h01-snapshot-hung-backend-`.

- [ ] **Step 8: Record validation and commit**

The report must include commit identity, dirty state, package SHA, exact test counts, local sandbox-only failures, VM package identity, fresh-path results, hang timing, evidence paths, and the explicit limitation that this slice does not kill an indefinitely hung external child.

Commit:

```bash
git add -f docs/audits/2026-07-17-h01-input-runtime-snapshot-validation.md
git commit -m "docs: record input snapshot validation"
```

Do not delete the VM lab, images, package artifacts, or evidence. Lab removal remains forbidden without a direct user request.
