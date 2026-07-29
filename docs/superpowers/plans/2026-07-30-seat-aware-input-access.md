# Seat-aware доступ к устройствам ввода — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** заменить blanket ACL bridge на стандартный seat-aware `uaccess` и
гарантированно освобождать уже открытый input backend при потере активной
локальной графической сессии.

**Architecture:** daemon получает отдельный `SessionActivityMonitor`, который
публикует локальную lease и монотонную generation. Input backend и writer
проверяют один session token до/после `EVIOCGRAB` и перед мутациями; смена
generation переводит существующий backend через уже реализованный безопасный
shutdown/recovery. Debian package устанавливает одно правило до
`73-seat-late.rules`, больше не выдаёт blanket ACL и проверяемо очищает
собственный runtime state.

**Tech Stack:** Rust 2021, zbus 3.15 system D-Bus, atomics/RwLock,
libudev 0.2, evdev/uinput, POSIX shell, Debian debhelper, systemd-logind,
udev, ACL, две существующие QEMU/KVM ВМ.

---

## Политика выполнения

- Реализация выполняется в
  `/home/andrey/Projects/OpenSwitcher/.worktrees/seat-aware-input-access`.
- Подобранные задержки коррекции не меняются.
- На каждом этапе запускаются только перечисленные узкие тесты.
- `cargo test --locked --all-targets --features settings-ui
  -- --test-threads=1`, сборка DEB и обе ВМ выполняются один раз в итоговом
  gate.
- Параллельный baseline `cargo test` в sandbox конфликтует в process/socket
  тестах, а последовательный baseline завис на
  `input_target_stop_signal_wakes_idle_waiter`. Перед итоговым gate этот тест
  сначала воспроизводится отдельно с deadline; общий результат не объявляется
  успешным, пока причина не классифицирована как sandbox-only или исправлена.
- Никакие физические устройства хоста, host clipboard, host udev/ACL или
  пользовательская сессия хоста в runtime-тестах не изменяются.

## Карта файлов

### Новые Rust-модули

- `src/daemon/session_activity/mod.rs` — pure policy, publication/lease,
  monitor lifecycle и fake-source тесты.
- `src/daemon/session_activity/logind.rs` — единственная граница system D-Bus
  logind.
- `src/daemon/input_device_identity.rs` — проверка character device,
  major/minor, udev identity и seat.

### Изменяемые Rust-файлы

- `src/daemon/mod.rs` — старт monitor до D-Bus endpoint и безопасный порядок
  shutdown.
- `src/daemon/input_backend.rs` — opener с session publication и recoverable
  inactive state.
- `src/daemon/keyboard.rs` — session token в prepare/activate/writer,
  pre/post-grab проверки и mutation admission.
- `src/daemon/service.rs` — release/reset/recovery на смене generation.
- `src/error/mod.rs` — типизированные session/identity ошибки.
- `Cargo.toml`, `Cargo.lock` — прямая зависимость `libudev = "0.2"`.

### Packaging

- `debian/open-switcher.openswitcher-input.udev` — canonical rule.
- `debian/rules`, `debian/control`, `debian/open-switcher.install`.
- `debian/open-switcher.preinst`, `.prerm`, `.postinst`, `.postrm`.
- `debian/scripts/open-switcher-user-session-stop`.
- новый
  `debian/scripts/open-switcher-input-access-maintenance` — только `apply` и
  `capture`, без path/env overrides.
- удалить `debian/scripts/open-switcher-input-acl-bridge`.
- удалить `dist/udev/80-openswitcher-input.rules`.

### Tests и результат

- `tests/session_activity` остаётся внутри Rust-модуля.
- `tests/debian_package_scripts_test.sh`.
- новый `tests/input_access_package_test.sh`.
- `debian/changelog`.
- новый
  `docs/audits/2026-07-30-seat-aware-input-access-validation.md`.

## Task 1: Pure session policy и атомарная lease

**Files:**

- Create: `src/daemon/session_activity/mod.rs`
- Modify: `src/daemon/mod.rs:1-12`
- Modify: `src/error/mod.rs:94-229`

- [ ] **Step 1: добавить RED-тесты выбора сессии**

В `src/daemon/session_activity/mod.rs` определить тестовые builders и проверки:

```rust
#[test]
fn exactly_one_active_local_graphical_session_is_authorized() {
    let decision = decide_session(
        1000,
        &[record("c2", 1000, "seat0", "x11", "user", true, false)],
    );
    assert_eq!(
        decision,
        SessionDecision::Authorized(AuthorizedSession::new("c2", "seat0"))
    );
}

#[test]
fn remote_tty_inactive_and_other_uid_are_not_candidates() {
    let records = [
        record("remote", 1000, "seat0", "x11", "user", true, true),
        record("tty", 1000, "seat0", "tty", "user", true, false),
        record("inactive", 1000, "seat0", "wayland", "user", false, false),
        record("other", 1001, "seat0", "wayland", "user", true, false),
    ];
    assert_eq!(
        decide_session(1000, &records),
        SessionDecision::Unauthorized(SessionUnavailableReason::NoCandidate)
    );
}

#[test]
fn two_active_graphical_sessions_for_uid_fail_closed() {
    let records = [
        record("c2", 1000, "seat0", "x11", "user", true, false),
        record("c8", 1000, "seat1", "wayland", "user", true, false),
    ];
    assert_eq!(
        decide_session(1000, &records),
        SessionDecision::Unauthorized(SessionUnavailableReason::Ambiguous)
    );
}
```

- [ ] **Step 2: проверить RED**

Run:

```bash
cargo test --locked --lib session_activity::tests:: -- --test-threads=1
```

Expected: FAIL, модуль и типы ещё отсутствуют.

- [ ] **Step 3: реализовать policy types**

Основной контракт:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionRecord {
    pub(crate) id: String,
    pub(crate) uid: u32,
    pub(crate) seat: String,
    pub(crate) session_type: String,
    pub(crate) class: String,
    pub(crate) active: bool,
    pub(crate) remote: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthorizedSession {
    session_id: Arc<str>,
    seat: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionUnavailableReason {
    NoCandidate,
    Ambiguous,
    SourceUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionDecision {
    Authorized(AuthorizedSession),
    Unauthorized(SessionUnavailableReason),
}

fn decide_session(uid: u32, records: &[SessionRecord]) -> SessionDecision {
    let mut candidates = records.iter().filter(|record| {
        record.uid == uid
            && record.active
            && !record.remote
            && record.class == "user"
            && matches!(record.session_type.as_str(), "x11" | "wayland")
            && !record.seat.is_empty()
    });
    let Some(first) = candidates.next() else {
        return SessionDecision::Unauthorized(SessionUnavailableReason::NoCandidate);
    };
    if candidates.next().is_some() {
        return SessionDecision::Unauthorized(SessionUnavailableReason::Ambiguous);
    }
    SessionDecision::Authorized(AuthorizedSession::new(&first.id, &first.seat))
}
```

- [ ] **Step 4: добавить RED-тесты publication**

Проверить следующие инварианты:

```rust
#[test]
fn same_session_refresh_renews_without_changing_generation() {
    let publication = SessionAccessPublication::new_for_test(3_000);
    publication.publish(authorized("c2", "seat0"), 10);
    let first = publication.backend_lease(11).unwrap();
    publication.publish(authorized("c2", "seat0"), 900);
    let second = publication.backend_lease(901).unwrap();
    assert_eq!(first.generation(), second.generation());
}

#[test]
fn session_change_invalidates_old_token_before_new_token_is_visible() {
    let publication = SessionAccessPublication::new_for_test(3_000);
    publication.publish(authorized("c2", "seat0"), 10);
    let old = publication.backend_lease(11).unwrap();
    publication.publish(authorized("c8", "seat0"), 20);
    assert!(matches!(
        old.ensure_current(21),
        Err(SwitcherError::InputSessionInactive)
    ));
}

#[test]
fn stale_lease_and_dead_worker_fail_closed() {
    let publication = SessionAccessPublication::new_for_test(3_000);
    publication.publish(authorized("c2", "seat0"), 10);
    assert!(publication.backend_lease(3_011).is_err());
    publication.mark_worker_stopped();
    assert!(matches!(
        publication.health_error(3_011),
        Some(SwitcherError::SessionMonitorStopped)
    ));
}
```

- [ ] **Step 5: реализовать publication без mutex на mutation path**

Использовать:

```rust
struct SessionAccessInner {
    authorized: AtomicBool,
    generation: AtomicU64,
    confirmed_at_ms: AtomicU64,
    worker_alive: AtomicBool,
    lease_ttl_ms: u64,
    session: RwLock<Option<AuthorizedSession>>,
}

#[derive(Clone)]
pub(crate) struct SessionAccessPublication {
    inner: Arc<SessionAccessInner>,
}

#[derive(Clone)]
pub(crate) struct SessionLease {
    publication: SessionAccessPublication,
    generation: u64,
    session: AuthorizedSession,
}
```

`publish()` сначала выставляет `authorized=false` при реальной смене решения,
обновляет metadata/generation, затем публикует `authorized=true`.
Подтверждение той же session/seat меняет только `confirmed_at_ms`.
`SessionLease::ensure_current()` читает только atomics. `RwLock` используется
только при создании backend token.

- [ ] **Step 6: добавить ошибки и recovery classification**

В `SwitcherError` добавить:

```rust
#[error("Input session is not currently authorized")]
InputSessionInactive,
#[error("Required logind session monitor stopped")]
SessionMonitorStopped,
#[error("Input device does not belong to the authorized seat")]
InputDeviceSeatMismatch,
#[error("Input device identity could not be verified")]
InputDeviceIdentityUnverified,
```

`InputSessionInactive` включить в `is_recoverable_input_error`;
`SessionMonitorStopped` оставить fatal.

- [ ] **Step 7: запустить узкие тесты и commit**

Run:

```bash
cargo test --locked --lib session_activity::tests:: -- --test-threads=1
cargo test --locked --lib error::tests -- --test-threads=1
cargo fmt --check
```

Expected: PASS.

Commit:

```bash
git add src/daemon/session_activity src/daemon/mod.rs src/error/mod.rs
git commit -m "feat: add fail-closed input session lease"
```

## Task 2: Logind monitor и startup/shutdown daemon

**Files:**

- Modify: `src/daemon/session_activity/mod.rs`
- Create: `src/daemon/session_activity/logind.rs`
- Modify: `src/daemon/mod.rs:48-135`
- Modify: `src/daemon/service.rs:1532-1592`

- [ ] **Step 1: добавить RED fake-source tests**

Определить внутреннюю границу:

```rust
trait SessionSource: Send {
    fn subscribe(&mut self) -> Result<(), SwitcherError>;
    fn snapshot(&mut self, uid: u32) -> Result<Vec<SessionRecord>, SwitcherError>;
    fn wait_for_change(&mut self, timeout: Duration)
        -> Result<SessionSourceEvent, SwitcherError>;
}
```

Тестами доказать:

- `subscribe` вызывается до первого `snapshot`;
- сигнал немедленно вызывает повторный snapshot;
- timeout вызывает authoritative refresh;
- disconnect публикует `SourceUnavailable` и reconnect;
- stop/join идемпотентен;
- panic/неожиданный выход помечает worker dead.

- [ ] **Step 2: проверить RED**

Run:

```bash
cargo test --locked --lib session_activity::monitor_tests -- --test-threads=1
```

Expected: FAIL, monitor loop отсутствует.

- [ ] **Step 3: реализовать worker lifecycle**

Контракт handle:

```rust
pub(crate) struct SessionActivityMonitor {
    publication: SessionAccessPublication,
    stop_tx: async_channel::Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl SessionActivityMonitor {
    pub(crate) fn start() -> Result<Self, SwitcherError>;
    pub(crate) fn publication(&self) -> SessionAccessPublication;
    pub(crate) fn stop(&mut self);
    pub(crate) fn detach(&mut self);
}
```

Worker:

1. публикует `worker_alive=true`;
2. создаёт signal subscriptions;
3. только затем читает snapshot;
4. refresh не реже 1 s;
5. при ошибке немедленно invalidates publication;
6. reconnect выполняется с bounded backoff;
7. RAII guard на любом выходе вызывает `mark_worker_stopped()`.

- [ ] **Step 4: реализовать единственную zbus-границу**

`logind.rs` использует:

```rust
const LOGIN1_DESTINATION: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION: &str = "org.freedesktop.login1.Session";
```

До `ListSessions` создать два `MessageStream::for_match_rule`:

- manager `SessionNew`/`SessionRemoved`;
- `org.freedesktop.DBus.Properties.PropertiesChanged` с
  `path_namespace=/org/freedesktop/login1/session`.

`ListSessions` фильтруется по effective UID, а свойства `Active`, `Remote`,
`Type`, `Class` читаются у соответствующего session object. Содержимое
session id/user name не выводится в production logs.

- [ ] **Step 5: встроить monitor до D-Bus endpoint**

В `daemon::run()` порядок должен стать:

```rust
let mut session_monitor = SessionActivityMonitor::start()?;
let session_access = session_monitor.publication();
let (connection, mut capture_owner_monitor) =
    start_dbus_endpoint(runtime.clone(), SERVICE_NAME)?;
let mut service = DaemonService::new(runtime, connection, session_access)?;
```

`DaemonService` на этом этапе только хранит clone publication; opener начнёт
использовать его в Task 3, а event loop — в Task 4. Это сохраняет компилируемый
checkpoint после Task 2.

При ошибке и shutdown сначала освобождается `DaemonService` input backend,
затем останавливается `CaptureOwnerMonitor`, затем session monitor. Если
writer shutdown unresponsive, оба monitor detach-ятся и daemon возвращает
fatal error для systemd restart.

- [ ] **Step 6: тесты порядка и commit**

Run:

```bash
cargo test --locked --lib session_activity -- --test-threads=1
cargo test --locked --lib daemon::tests::daemon_error_releases_input \
  -- --test-threads=1
cargo test --locked --lib dbus_endpoint_is_published_only \
  -- --test-threads=1
cargo fmt --check
```

Expected: PASS.

Commit:

```bash
git add src/daemon/session_activity src/daemon/mod.rs src/daemon/service.rs
git commit -m "feat: monitor active logind input session"
```

## Task 3: Проверка identity/seat и двойная граница EVIOCGRAB

**Files:**

- Create: `src/daemon/input_device_identity.rs`
- Modify: `src/daemon/mod.rs:1-14`
- Modify: `src/daemon/input_backend.rs:41-146`
- Modify: `src/daemon/keyboard.rs:1476-1889,3377-3687`
- Modify: `src/daemon/service.rs:1557-1592`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: добавить RED identity tests**

Pure policy:

```rust
#[test]
fn missing_id_seat_defaults_to_seat_zero() {
    assert_eq!(normalized_device_seat(None), "seat0");
}

#[test]
fn device_from_other_seat_is_rejected() {
    assert!(matches!(
        verify_authorized_seat("seat1", "seat0"),
        Err(SwitcherError::InputDeviceSeatMismatch)
    ));
}

#[test]
fn non_character_and_changed_devnum_are_rejected() {
    assert!(!identity_matches(
        ExpectedDeviceIdentity::character(0x0d05),
        ObservedDeviceIdentity::regular(0x0d05)
    ));
    assert!(!identity_matches(
        ExpectedDeviceIdentity::character(0x0d05),
        ObservedDeviceIdentity::character(0x0d06)
    ));
}
```

- [ ] **Step 2: проверить RED**

Run:

```bash
cargo test --locked --lib input_device_identity -- --test-threads=1
```

Expected: FAIL, модуль отсутствует.

- [ ] **Step 3: реализовать production resolver**

Добавить прямую зависимость:

```toml
libudev = "0.2"
```

Resolver обязан:

1. canonicalize devnode;
2. проверить `FileTypeExt::is_char_device`;
3. взять `MetadataExt::rdev`;
4. найти инициализированный libudev device с тем же `devnum`;
5. потребовать совпадение canonical devnode;
6. вернуть `ID_SEAT`, либо `seat0`.

Production API:

```rust
pub(crate) struct VerifiedInputDevice {
    pub(crate) canonical_path: PathBuf,
    pub(crate) devnum: u64,
    pub(crate) seat: Arc<str>,
}

pub(crate) fn verify_input_device(
    path: &Path,
    authorized_seat: &str,
) -> Result<VerifiedInputDevice, SwitcherError>;
```

- [ ] **Step 4: добавить RED pre/post-grab tests**

Расширить текущие `activation_rejects_dead_dependencies_before_physical_grab`
и `caps_lock_snapshot_is_taken_immediately_before_physical_grab`:

```rust
#[test]
fn activation_rejects_session_change_before_grab() {
    let mut fixture = ActivationFixture::authorized("c2", "seat0");
    fixture.invalidate_before_grab();
    let result = fixture.activate();
    assert!(matches!(result, Err(SwitcherError::InputSessionInactive)));
    assert_eq!(fixture.trace(), &["precheck"]);
}

#[test]
fn activation_closes_grab_when_generation_changes_after_ioctl() {
    let mut fixture = ActivationFixture::authorized("c2", "seat0");
    fixture.invalidate_immediately_after_grab();
    let result = fixture.activate();
    assert!(matches!(result, Err(SwitcherError::InputSessionInactive)));
    assert_eq!(
        fixture.trace(),
        &["precheck", "grab", "postcheck", "release"]
    );
    assert!(!fixture.ready());
}

#[test]
fn non_seat_zero_never_opens_uinput() {
    let fixture = PreparationFixture::authorized("c8", "seat1");
    let result = fixture.prepare();
    assert!(matches!(result, Err(SwitcherError::InputDeviceSeatMismatch)));
    assert_eq!(fixture.uinput_open_count(), 0);
}
```

`ActivationFixture` и `PreparationFixture` являются локальными test-only
seams: они считают вызовы и делегируют production helper, не открывая
настоящие устройства.

- [ ] **Step 5: передать token через opener и keyboard prepare**

Сигнатуры:

```rust
pub(crate) struct KeyboardInputBackendOpener {
    session_access: SessionAccessPublication,
}

pub fn prepare(
    lease: SessionLease,
) -> Result<PreparedKeyboardController, SwitcherError>;

pub fn activate(
    mut self,
) -> Result<(KeyboardController, bool), SwitcherError>;
```

`reopen_backend()` получает свежую lease до поиска устройств.
`DaemonService::new()` создаёт
`KeyboardInputBackendOpener::new(session_access.clone())`.
`KeyboardController::prepare()`:

- требует `seat0` до `VirtualKeyboardWriter::new`;
- проверяет keyboard и pointer nodes через resolver;
- передаёт lease writer;
- не открывает устройство другого seat.

`activate()` вызывает `lease.ensure_current()` непосредственно до
`snapshot_then_acquire_grab` и сразу после успешного grab. Ошибка postcheck
проходит через существующий `shutdown`, который закрывает grab до ожидания
writer/watchers.

- [ ] **Step 6: узкие тесты и commit**

Run:

```bash
cargo test --locked --lib input_device_identity -- --test-threads=1
cargo test --locked --lib activation_ -- --test-threads=1
cargo test --locked --lib caps_lock_snapshot_ -- --test-threads=1
cargo test --locked --lib input_backend::tests -- --test-threads=1
cargo fmt --check
```

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock src/daemon/mod.rs \
  src/daemon/input_device_identity.rs src/daemon/input_backend.rs \
  src/daemon/keyboard.rs src/daemon/service.rs
git commit -m "fix: bind physical input grab to active seat"
```

## Task 4: Mutation admission, release и автоматическое восстановление

**Files:**

- Modify: `src/daemon/session_activity/mod.rs`
- Modify: `src/daemon/keyboard.rs:415-802,5896-6172`
- Modify: `src/daemon/service.rs:1532-1769,3423-3599`
- Modify: `src/daemon/input_backend.rs:17-35`

- [ ] **Step 1: добавить RED mutation tests**

Доказать отдельно normal и cleanup permits:

```rust
#[test]
fn session_change_denies_next_normal_mutation() {
    let (publication, lease) = authorized_lease();
    let control = WriterTransactionControl::with_session_for_test(lease);
    publication.invalidate(SessionUnavailableReason::NoCandidate);
    assert!(matches!(
        control.authorize_mutation_start(),
        Err(SwitcherError::InputSessionInactive)
    ));
}

#[test]
fn session_change_still_allows_cleanup_release() {
    let (publication, lease) = authorized_lease();
    let control = WriterTransactionControl::with_session_for_test(lease);
    control.authorize_mutation_start().unwrap();
    publication.invalidate(SessionUnavailableReason::NoCandidate);
    assert!(control.authorize_cleanup_mutation_start().is_ok());
}

#[test]
fn fast_physical_forward_is_denied_after_epoch_change() {
    let (publication, lease) = authorized_lease();
    let failure = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let gate = Mutex::new(());
    let calls = AtomicUsize::new(0);
    publication.invalidate(SessionUnavailableReason::NoCandidate);
    let result = authorize_writer_mutation_start(
        &lease,
        &failure,
        &stop,
        &gate,
    )
    .map(|()| calls.fetch_add(1, Ordering::SeqCst));
    assert!(matches!(result, Err(SwitcherError::InputSessionInactive)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
```

- [ ] **Step 2: проверить RED**

Run:

```bash
cargo test --locked --lib session_change_ -- --test-threads=1
```

Expected: FAIL, session token ещё не участвует в writer admission.

- [ ] **Step 3: встроить lease в обе writer admission границы**

`WriterTransactionControl` и production fast writer state получают
`SessionLease`.

Production `new_with_writer_state` всегда требует session lease. Существующие
test-only constructors используют
`SessionLease::always_authorized_for_test()`, поэтому старые unit tests не
получают доступ к production bypass и не требуют механически переписывать
сотни вызовов.

Обычный permit:

```rust
fn authorize_writer_mutation_start(
    session: &SessionLease,
    failure_request_id: &AtomicU64,
    stop_requested: &AtomicBool,
    terminal_gate: &Mutex<()>,
) -> Result<(), SwitcherError> {
    session.ensure_current(monotonic_ms())?;
    ensure_writer_running(failure_request_id, stop_requested, terminal_gate)?;
    session.ensure_current(monotonic_ms())
}
```

`WriterTransactionControl::ensure_active()` также проверяет lease, чтобы
`sleep_interruptibly` прерывался между synthetic steps. Cleanup permit
сохраняет текущую H-06 семантику и не требует active session для key-up/sync.

- [ ] **Step 4: добавить RED service recovery tests**

```rust
#[test]
fn session_deactivation_releases_backend_before_resetting_recovery_state() {
    let mut fixture = ServiceSessionFixture::ready();
    fixture.deactivate();
    fixture.poll_once().unwrap();
    assert_eq!(
        fixture.trace(),
        &["admission-closed", "shutdown", "ungrab", "context-reset", "waiting"]
    );
    assert_eq!(fixture.state(), InputBackendState::WaitingForInputAccess);
}

#[test]
fn old_epoch_tail_is_accounted_but_never_replayed_after_reactivation() {
    let mut fixture = ServiceSessionFixture::ready();
    fixture.enqueue_old_epoch_event(41);
    fixture.deactivate();
    fixture.poll_once().unwrap();
    fixture.reactivate();
    fixture.poll_once().unwrap();
    assert_eq!(fixture.replayed_sequence_ids(), &[]);
    assert_eq!(fixture.reconciled_sequence_ids(), &[41]);
}

#[test]
fn reactivation_reopens_with_empty_context() {
    let mut fixture = ServiceSessionFixture::ready_with_word("руддщ");
    fixture.deactivate();
    fixture.poll_once().unwrap();
    fixture.reactivate();
    fixture.poll_once().unwrap();
    assert_eq!(fixture.state(), InputBackendState::Ready);
    assert_eq!(fixture.word_buffer(), "");
    assert_eq!(fixture.modifiers(), ModifierState::default());
}
```

`ServiceSessionFixture` — test-only wrapper над существующими lifecycle pure
seams; он записывает порядок callbacks и не создаёт evdev/uinput.

- [ ] **Step 5: использовать существующий runtime recovery**

В начале event loop, до writer health и перед каждым batch event:

```rust
if let Some(error) = self.session_access.health_error(monotonic_ms()) {
    route_runtime_health_failure(error, |error| {
        self.handle_runtime_input_failure(error)
    })?;
    continue 'event_loop;
}
```

`InputSessionInactive` переводит lifecycle в
`WaitingForInputAccess`, освобождает backend существующим
`drop_active_input_backend()`, вызывает
`reset_transient_input_state("input-session-changed")` и использует текущий
retry schedule. `SessionMonitorStopped` остаётся fatal для
`Restart=on-failure`.

Удержанный fetched/deferred tail только терминально учитывается после writer
shutdown; replay в следующую generation запрещён.

- [ ] **Step 6: regression tests и commit**

Run:

```bash
cargo test --locked --lib session_change_ -- --test-threads=1
cargo test --locked --lib session_deactivation_ -- --test-threads=1
cargo test --locked --lib reactivation_ -- --test-threads=1
cargo test --locked --lib writer_transaction_cancellation_ \
  -- --test-threads=1
cargo test --locked --lib deferred_input -- --test-threads=1
cargo fmt --check
```

Expected: PASS.

Commit:

```bash
git add src/daemon/session_activity/mod.rs src/daemon/keyboard.rs \
  src/daemon/service.rs src/daemon/input_backend.rs
git commit -m "fix: revoke input runtime on session change"
```

## Task 5: Canonical uaccess rule и удаление blanket bridge

**Files:**

- Modify: `debian/open-switcher.openswitcher-input.udev`
- Modify: `debian/rules`
- Modify: `debian/open-switcher.install`
- Modify: `debian/open-switcher.postinst`
- Create: `debian/scripts/open-switcher-input-access-maintenance`
- Delete: `debian/scripts/open-switcher-input-acl-bridge`
- Delete: `dist/udev/80-openswitcher-input.rules`
- Modify: `tests/debian_package_scripts_test.sh`
- Create: `tests/input_access_package_test.sh`

- [ ] **Step 1: написать RED static package tests**

Новые assertions:

```bash
assert_contains "$rules" \
    'dh_installudev --name=openswitcher-input --priority=70'
assert_contains "$udev_rule" 'TAG+="uaccess", TAG+="openswitcher-input"'
assert_not_exists "$REPO_ROOT/debian/scripts/open-switcher-input-acl-bridge"
assert_not_exists "$REPO_ROOT/dist/udev/80-openswitcher-input.rules"
assert_not_contains "$postinst" "open-switcher-input-acl-bridge"
assert_not_contains "$postinst" \
    "open-switcher-input-access-maintenance apply || true"
assert_not_contains "$postinst" \
    "udevadm control --reload-rules || true"
```

Проверить, что helper не содержит:

```text
OPEN_SWITCHER_LINUX_INPUT_
eval
source
sudo
/tmp
```

- [ ] **Step 2: проверить RED**

Run:

```bash
bash tests/debian_package_scripts_test.sh
bash tests/input_access_package_test.sh
```

Expected: FAIL на priority 80, bridge и отсутствующем helper.

- [ ] **Step 3: заменить правило**

Каждая поддерживаемая строка принимает вид:

```udev
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess", TAG+="openswitcher-input"
```

А `uinput`:

```udev
SUBSYSTEM=="misc", KERNEL=="uinput", TAG+="uaccess", TAG+="openswitcher-input", OPTIONS+="static_node=uinput"
```

`debian/rules` использует priority 70. Старый dist payload и bridge удаляются.

- [ ] **Step 4: реализовать helper `apply`**

На этом этапе helper принимает только:

```text
open-switcher-input-access-maintenance apply
```

Любой другой argc/subcommand возвращает 2. Subcommand `capture` добавляется
полностью в Task 6 вместе с его тестами. Production пути являются
литералами `/dev`, `/sys`, `/run/open-switcher` и не читаются из environment.

`apply`:

1. если `/run/udev/control` отсутствует — печатает
   `open-switcher: udev activation deferred until boot` и возвращает 0;
2. на live udev выполняет reload;
3. trigger-ит `input` и `misc/uinput`;
4. вызывает `udevadm settle --timeout=10`;
5. проверяет `openswitcher-input` и `uaccess` на существующих подходящих
   devices;
6. любая неожиданная live-ошибка возвращается non-zero.

`postinst configure|abort-remove|abort-deconfigure` вызывает `apply` до
`open-switcher-user-session-start`.

- [ ] **Step 5: shell RED/GREEN matrix**

Fake `udevadm` должен покрыть:

- offline success без вызовов;
- live reload → trigger → settle → verify order;
- reload/trigger/settle/verify failure возвращает non-zero;
- start helper не вызывается после failure;
- повторный `apply` производит тот же postcondition.

Run:

```bash
bash -n debian/open-switcher.postinst \
  debian/scripts/open-switcher-input-access-maintenance
bash tests/input_access_package_test.sh
bash tests/debian_package_scripts_test.sh
```

Expected: PASS без реального udev/ACL.

- [ ] **Step 6: commit**

```bash
git add debian tests/debian_package_scripts_test.sh \
  tests/input_access_package_test.sh
git add -u dist/udev
git commit -m "fix: delegate input ACLs to seat-aware uaccess"
```

## Task 6: Bounded package stop и remove/purge ACL cleanup

**Files:**

- Modify: `debian/open-switcher.preinst`
- Modify: `debian/open-switcher.prerm`
- Modify: `debian/open-switcher.postrm`
- Modify: `debian/scripts/open-switcher-user-session-stop`
- Modify: `debian/scripts/open-switcher-input-access-maintenance`
- Modify: `tests/input_access_package_test.sh`
- Modify: `tests/debian_package_scripts_test.sh`

- [ ] **Step 1: добавить RED stop postcondition tests**

Fake matrix:

```text
unit-not-loaded                 -> success
inactive/dead                   -> success
systemctl-stop-fails-no-process -> success with warning
systemctl-stop-fails-live-exe   -> failure
stop-timeout-live-exe           -> failure within deadline
same-name-different-exe         -> ignored
```

Проверить порядок tray → daemon → guardian drain → socket/service и отсутствие
неограниченного ожидания.

- [ ] **Step 2: реализовать bounded stop**

Каждый `systemctl --user stop` выполняется через `timeout --signal=TERM
--kill-after=2s 10s`. После остановки проверяются:

- `ActiveState` unit, если user manager отвечает;
- `/proc/[0-9]*/exe` только для UID рассматриваемой локальной graphical
  session;
- exact canonical `/usr/bin/open-switcher-daemon`.

Если exact daemon жив, helper возвращает non-zero. `preinst upgrade` и
`prerm remove|deconfigure|failed-upgrade` больше не подавляют этот результат.
Legacy fallback в новом `preinst` выполняет ту же postcondition, потому что
старый helper версии `0.1.0-3` её ещё не гарантирует.

- [ ] **Step 3: добавить RED capture/cleanup tests**

Fixture содержит:

- `event4`, tag `openswitcher-input`, seat0, devnum A, ACL UID 1000;
- `event5`, seat1, ACL UID 1001;
- подменённый event node с другим devnum;
- node, которому оставшееся правило всё ещё даёт `uaccess`;
- сторонний named UID и group ACL.

Ожидания:

```text
capture records only active owner of each verified seat
changed devnum is never passed to setfacl
remaining uaccess skips manual removal
only recorded user:<uid> is removed with setfacl -n -x
other UID/group entries remain
second cleanup performs no additional mutation
```

- [ ] **Step 4: реализовать `capture`**

Расширить allowlist helper вторым и последним subcommand:

```text
open-switcher-input-access-maintenance capture
```

Создать `/run/open-switcher` только если это root-owned directory mode 0700 и
не symlink. Manifest создаётся с `umask 077`, без `eval` и shell-source.

Для каждого device с package tag:

1. проверить canonical `/dev/input/event*`, `/dev/uinput` или
   `/dev/input/uinput`;
2. проверить character type и сохранить stat devnum;
3. определить `ID_SEAT`, default `seat0`;
4. через `loginctl show-seat` получить active session и numeric UID;
5. через `getfacl -cpn` убедиться, что `user:<uid>` существует;
6. записать одну tab-separated строку:
   `canonical_path`, `devnum`, `uid`.

- [ ] **Step 5: реализовать postrm cleanup**

Только для `remove|purge`:

1. reload/trigger/settle после удаления package rule;
2. читать manifest как данные через
   `while IFS="$(printf '\t')" read -r path devnum uid`;
3. повторно проверить allowlisted path, character type и exact devnum;
4. если текущие udev tags содержат `uaccess`, ничего не удалять;
5. иначе выполнить `setfacl -n -x "u:$uid" -- "$path"`;
6. подтвердить отсутствие exact entry через `getfacl -cpn`;
7. атомарно удалить manifest.

Неоднозначный/изменившийся node сохраняется и логируется без попытки
«починить» чужой ACL. Upgrade не запускает remove cleanup.

- [ ] **Step 6: проверить abort/idempotency и commit**

Run:

```bash
bash -n debian/open-switcher.preinst debian/open-switcher.prerm \
  debian/open-switcher.postinst debian/open-switcher.postrm \
  debian/scripts/open-switcher-user-session-stop \
  debian/scripts/open-switcher-input-access-maintenance
bash tests/input_access_package_test.sh
bash tests/debian_package_scripts_test.sh
```

Expected: PASS, fake logs содержат только ожидаемые mutations.

Commit:

```bash
git add debian tests/input_access_package_test.sh \
  tests/debian_package_scripts_test.sh
git commit -m "fix: verify package stop and ACL cleanup"
```

## Task 7: Итоговый gate, DEB и две ВМ

**Files:**

- Modify: `debian/changelog`
- Modify: `README.md`
- Modify: `README.ru.md`
- Create:
  `docs/audits/2026-07-30-seat-aware-input-access-validation.md`

- [ ] **Step 1: локальная статическая проверка**

Run:

```bash
git diff --check
cargo fmt --check
cargo test --locked --lib session_activity -- --test-threads=1
cargo test --locked --lib input_device_identity -- --test-threads=1
cargo test --locked --lib session_change_ -- --test-threads=1
bash tests/input_access_package_test.sh
bash tests/debian_package_scripts_test.sh
```

Expected: PASS.

- [ ] **Step 2: отдельно классифицировать baseline hang**

Run:

```bash
timeout --signal=TERM --kill-after=2s 30s \
  cargo test --locked --lib input_target_stop_signal_wakes_idle_waiter \
  -- --test-threads=1 --nocapture
timeout --signal=TERM --kill-after=2s 30s \
  cargo test --locked --lib repeated_input_target_stop_is_idempotent \
  -- --test-threads=1 --nocapture
```

Expected: PASS. Если тест висит только внутри Codex sandbox, повторить ту же
команду вне sandbox без изменения host input state и сохранить точный
результат в validation doc. Реальный code hang исправляется до продолжения.

- [ ] **Step 3: один полный test gate**

Run:

```bash
cargo test --locked --all-targets --features settings-ui \
  -- --test-threads=1
cargo clippy --locked --all-targets --all-features -- -D warnings
bash tests/wayland_diagnostics_test.sh
bash tests/linux_input_setup_test.sh
bash tests/manage_package_deb_test.sh
```

Expected: все non-ignored tests PASS; известные intentional ignored benchmarks
остаются ignored.

- [ ] **Step 4: обновить пакет и собрать**

Добавить `0.1.0-4` в `debian/changelog` с H-08/M-09/L-02. Обновить README:
package использует logind `uaccess`, ручной bootstrap не является
production-путём.

Run:

```bash
./manage.sh package deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-4_amd64.deb
dpkg-deb --contents dist/packages/open-switcher_0.1.0-4_amd64.deb
lintian dist/packages/open-switcher_0.1.0-4_amd64.deb || true
sha256sum dist/packages/open-switcher_0.1.0-4_amd64.deb
```

Expected: пакет содержит `70-openswitcher-input.rules`, новый maintenance
helper и не содержит blanket bridge/duplicate rule.

- [ ] **Step 5: Mint package-first VM**

Start:

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
PYTHONPATH=. python3 -m tools.vm_lab.session mint-installed
```

SSH/scp используют:

```bash
-i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
-o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
-p 22223
openswitcher@127.0.0.1
```

В guest выполнить upgrade `0.1.0-3 → 0.1.0-4`, reinstall, remove и повторный
install. Зафиксировать:

- package ownership и rule priority;
- udev tags/ACL;
- daemon/tray/guardian состояние;
- смену на второго guest user и отсутствие retained grab;
- возврат `openswitcher` и автоматическое восстановление;
- F12, автокоррекцию, Caps Lock, две заглавные;
- lock/unlock без отключения backend;
- purge без остаточного ACL OpenSwitcher.

- [ ] **Step 6: Ubuntu package-first VM**

Повторить с:

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
PYTHONPATH=. python3 -m tools.vm_lab.session ubuntu-installed
```

SSH port: `22222`, остальные key/known-hosts/user те же. Проверить ту же
матрицу в GNOME/Wayland; XTEST-specific шаги не запускать.

- [ ] **Step 7: validation doc и финальный commit**

Документ должен содержать:

- commit/package identity и SHA-256;
- точные команды и counts;
- Mint/Ubuntu результаты;
- latency smoke до/после;
- подтверждение release/recovery;
- остаточные ограничения whole-process hang и non-seat0 uinput;
- перечень закрытых H-08/M-09/L-02.

Commit:

```bash
git add debian/changelog README.md README.ru.md \
  docs/audits/2026-07-30-seat-aware-input-access-validation.md
git commit -m "docs: validate seat-aware input lifecycle"
```

- [ ] **Step 8: запросить финальный review перед merge**

Проверить:

```bash
git status --short
git log --oneline 65eed1d..HEAD
git diff --stat 65eed1d..HEAD
```

Expected: clean worktree; только файлы этого slice. После review применить
`superpowers:finishing-a-development-branch`. ВМ-лабораторию не удалять.
