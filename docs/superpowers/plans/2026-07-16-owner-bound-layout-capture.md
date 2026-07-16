# Owner-Bound Layout Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make layout-switch capture bounded, owner-scoped, and input-balanced so an abandoned or malformed D-Bus session cannot keep swallowing keyboard events.

**Architecture:** A pure capture state machine owns the D-Bus owner lease and a ledger of key-down events suppressed by capture. Runtime exposes one atomic routing call under one mutex; D-Bus supplies the caller unique name and monitors owner loss; the settings UI keeps one connection and renews a short lease. The core, daemon boundary, integration tests, and UI must land together.

**Tech Stack:** Rust 2021, evdev, zbus 3.15.2 blocking/async APIs, GTK/glib, async-channel, futures-util when required for the owner-monitor select loop.

---

## File map

- `src/daemon/capture.rs`: pure owner/lease state machine and suppression ledger.
- `src/model.rs`: public capture phase semantics; `Unsupported` is terminal.
- `src/error/mod.rs`, `src/error/dbus_error.rs`: Busy/NotOwner/NotActive mapping.
- `src/daemon/runtime.rs`: one-lock owner commands and event routing.
- `src/daemon/service.rs`: applies routing dispositions, polls expiry, resets input epochs.
- `src/dbus/mod.rs`: caller identity, renew method, best-effort signals, owner-loss monitor.
- `src/daemon/mod.rs`: starts and cleanly stops the owner-loss monitor.
- `tests/dbus_api.rs`: retained-connection owner-bound integration tests.
- `src/settings_ui/dbus_client.rs`: persistent owner connection and renew call.
- `src/settings_ui/presenter.rs`, `src/settings_ui/state.rs`, `src/settings_ui/ui.rs`: heartbeat lifecycle and terminal UI handling.
- `Cargo.toml`, `Cargo.lock`: direct `futures-util` dependency only if the monitor uses `StreamExt/FutureExt/select!`.

### Task 1: Owner lease in the pure capture session

**Files:**

- Modify: `src/daemon/capture.rs`
- Modify: `src/error/mod.rs`

- [ ] **Step 1: Add owner/lease RED tests**

Add deterministic tests using an explicit `Instant` and no sleeps:

```rust
#[test]
fn different_owner_cannot_replace_live_capture() {
    let now = Instant::now();
    let mut session = LayoutSwitchCaptureSession::default();
    session.start_owned(CaptureOwner::from(":1.10"), now).unwrap();
    assert!(matches!(
        session.start_owned(CaptureOwner::from(":1.11"), now),
        Err(CaptureError::Busy)
    ));
}

#[test]
fn lease_expires_at_soft_deadline_and_never_past_absolute_deadline() {
    let now = Instant::now();
    let mut session = LayoutSwitchCaptureSession::default();
    session.start_owned(CaptureOwner::from(":1.10"), now).unwrap();
    assert!(session.expire_at(now + CAPTURE_SOFT_LEASE).is_some());
}
```

Also add `same_owner_start_renews_without_clearing_candidate`, `non_owner_cannot_renew_cancel_or_finish`, `owner_loss_cancels_only_matching_owner`, and `absolute_deadline_is_not_extended_by_renew`.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib capture -- --nocapture
```

Expected: compilation/test failure because owner-aware types and methods do not exist.

- [ ] **Step 3: Implement owner and bounded lease**

Use these core shapes, keeping zbus types out of the state machine:

```rust
pub const CAPTURE_SOFT_LEASE: Duration = Duration::from_secs(10);
pub const CAPTURE_ABSOLUTE_LEASE: Duration = Duration::from_secs(65);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureOwner(String);

#[derive(Clone, Debug)]
struct CaptureLease {
    owner: CaptureOwner,
    soft_deadline: Instant,
    absolute_deadline: Instant,
}
```

Add `Busy`, `NotOwner`, and `NotActive` to `CaptureError`. Implement:

```rust
start_owned(owner, now)
renew_owned(&owner, now)
cancel_owned(&owner, now)
finish_owned(&owner, now)
owner_disappeared(&owner, now)
expire_at(now)
```

Every mutator checks expiry first. Same-owner Start is an idempotent renew and preserves progress; a new Start after a terminal/expired session creates both deadlines. Renew may move only the soft deadline and clamps it to the original absolute deadline. `now >= deadline` is expired.

- [ ] **Step 4: Run GREEN and commit**

```bash
cargo test --lib capture -- --nocapture
git add src/daemon/capture.rs src/error/mod.rs
git commit -m "fix: bind layout capture to a bounded owner lease"
```

### Task 2: Terminal semantics and balanced suppression ledger

**Files:**

- Modify: `src/model.rs`
- Modify: `src/daemon/capture.rs`

- [ ] **Step 1: Add routing RED tests**

Add the exact behavioral matrix:

```rust
assert!(!LayoutSwitchCaptureState::unsupported("x").is_active());
assert_eq!(session.route_event_at(now, Key::KEY_A, 0).disposition,
           CaptureEventDisposition::ForwardDirect);
assert_eq!(session.route_event_at(now, Key::KEY_LEFTCTRL, 1).disposition,
           CaptureEventDisposition::Suppress);
assert_eq!(session.route_event_at(now, Key::KEY_LEFTCTRL, 0).disposition,
           CaptureEventDisposition::Suppress);
```

Cover: pre-held repeat/release; captured press/repeat/release; unsupported triggering press; Escape press/release; unrelated event after terminal state; debt surviving cancel, finish, owner loss, and both expiries; `reset_input_epoch` clearing debt.

- [ ] **Step 2: Run RED**

```bash
cargo test --lib capture -- --nocapture
cargo test --lib unsupported_capture_state_is_terminal -- --nocapture
```

Expected: current `Unsupported` remains active and current capture has no routing disposition or ledger.

- [ ] **Step 3: Implement one routing decision**

Add:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureEventDisposition { PassThrough, ForwardDirect, Suppress }

pub struct CaptureEventOutcome {
    pub disposition: CaptureEventDisposition,
    pub state_change: Option<LayoutSwitchCaptureState>,
}
```

Store suppressed evdev key codes in `BTreeSet<u16>`. A release/repeat without a suppressed press is `ForwardDirect` while capture is active. Supported capture presses enter the ledger and are suppressed through release. Unsupported becomes terminal and forwards the triggering press. Escape press becomes terminal Cancelled but both its press and release are suppressed. Do not clear debt on terminal commands or lease loss; clear it only on `reset_input_epoch`.

- [ ] **Step 4: Run GREEN and commit**

```bash
cargo test --lib capture -- --nocapture
cargo test --lib unsupported_capture_state_is_terminal -- --nocapture
git add src/model.rs src/daemon/capture.rs
git commit -m "fix: balance key suppression during layout capture"
```

### Task 3: Atomic runtime and service routing

**Files:**

- Modify: `src/daemon/runtime.rs`
- Modify: `src/daemon/service.rs`

- [ ] **Step 1: Add runtime/service RED tests**

Add tests that prove one call returns both routing and optional state change, `Suppress` does not update modifier state, `ForwardDirect` updates and writes exactly once, expiry is observed without an input event, and backend replacement/shutdown resets the ledger.

```rust
let outcome = runtime.route_layout_switch_capture_event_at(now, Key::KEY_A, 0).unwrap();
assert_eq!(outcome.disposition, CaptureEventDisposition::ForwardDirect);
```

- [ ] **Step 2: Run RED**

```bash
cargo test --lib runtime_capture -- --nocapture
cargo test --lib capture_routing -- --nocapture
```

Expected: the runtime still exposes split `is_capture_active` and `handle_capture_key_event` calls.

- [ ] **Step 3: Replace the split API**

Remove the service sequence `is_capture_active()` followed by `handle_capture_key_event()`. Add owner-aware runtime wrappers and exactly one `route_layout_switch_capture_event_at` lock acquisition. Service behavior is:

```rust
match outcome.disposition {
    CaptureEventDisposition::Suppress => return Ok(()),
    CaptureEventDisposition::ForwardDirect => {
        self.update_forwarded_modifier_state(key, value);
        self.forward_event_direct(key, value)?;
        return Ok(());
    }
    CaptureEventDisposition::PassThrough => {}
}
```

Use the existing input-loop timeout (at most 100 ms) to call `expire_layout_switch_capture_at(Instant::now())` and publish any state transition best-effort. Backend replacement, recovery epoch change, and shutdown call `reset_layout_switch_capture_input_epoch`; shutdown also terminates an active lease.

- [ ] **Step 4: Run GREEN and commit**

```bash
cargo test --lib runtime_capture -- --nocapture
cargo test --lib capture_routing -- --nocapture
git add src/daemon/runtime.rs src/daemon/service.rs
git commit -m "fix: route capture events atomically"
```

### Task 4: D-Bus owner boundary and disconnect monitor

**Files:**

- Modify: `src/dbus/mod.rs`
- Modify: `src/daemon/mod.rs`
- Modify: `tests/dbus_api.rs`
- Modify if needed: `Cargo.toml`, `Cargo.lock`

- [ ] **Step 1: Add retained-connection RED tests**

Split the old `start -> cancel -> finish` test into valid sessions. Hold two `zbus::blocking::Connection` values for the entire test and assert:

```rust
let started = owner_a.start_layout_switch_capture()?;
assert_eq!(started.phase, LayoutSwitchCapturePhase::Waiting);
assert!(owner_b.cancel_layout_switch_capture().is_err());
let renewed = owner_a.renew_layout_switch_capture()?;
assert!(renewed.is_active());
```

Add Start/Cancel, Start/Finish, other-owner rejection, same-owner Renew, owner-A disconnect cancellation, and command success despite signal-send failure. Run D-Bus integration serially.

- [ ] **Step 2: Run RED**

```bash
cargo test --test dbus_api capture -- --test-threads=1 --nocapture
```

Expected: sender is not captured, Renew is absent, and disconnect does not cancel.

- [ ] **Step 3: Bind methods to `MessageHeader` sender**

Add `RenewLayoutSwitchCapture` to the proxy/interface. Each mutator takes:

```rust
#[zbus(header)] header: zbus::MessageHeader<'_>
```

Extract `header.sender()?.ok_or(...)?.as_str()` into `CaptureOwner`. Read-only GetState remains unowned. Commit state first, then emit `LayoutSwitchCaptureStateChanged` best-effort; signal failure must not turn a successful command into an error.

- [ ] **Step 4: Add clean owner-loss monitoring**

After `ConnectionBuilder::build()` in `src/daemon/mod.rs`, start one monitor for `org.freedesktop.DBus.NameOwnerChanged`. Do not block or subscribe from a synchronous D-Bus handler. The worker selects between the signal stream and a stop receiver; on `new_owner == None`, call the runtime owner-loss method and emit a transition best-effort. Store a stop sender and `JoinHandle`; shutdown sends stop and joins it.

Use a direct `futures-util = "0.3"` dependency only when the implementation imports `StreamExt`, `FutureExt`, or `select!`; do not rely on `zbus::export` or hidden `zbus::Task`.

- [ ] **Step 5: Run GREEN and commit**

```bash
cargo test --lib dbus::tests::capture -- --nocapture
cargo test --test dbus_api capture -- --test-threads=1 --nocapture
git add src/dbus/mod.rs src/daemon/mod.rs tests/dbus_api.rs Cargo.toml Cargo.lock
git commit -m "fix: enforce dbus ownership for layout capture"
```

### Task 5: Persistent settings owner and bounded heartbeat

**Files:**

- Modify: `src/settings_ui/dbus_client.rs`
- Modify: `src/settings_ui/presenter.rs`
- Modify: `src/settings_ui/state.rs`
- Modify: `src/settings_ui/ui.rs`

- [ ] **Step 1: Add client/presenter/UI RED tests**

Require one retained `zbus::blocking::Connection` for Start/Renew/Cancel/Finish. Add presenter tests for one heartbeat at three seconds, no overlapping renew, stale generation ignored, heartbeat stopped by Cancel/Finish/Unsupported/window close, and renewal failure closing local capture state.

```rust
const CAPTURE_HEARTBEAT: Duration = Duration::from_secs(3);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
assert!(CAPTURE_HEARTBEAT < CAPTURE_SOFT_LEASE);
assert!(CAPTURE_TIMEOUT < CAPTURE_ABSOLUTE_LEASE);
```

- [ ] **Step 2: Run RED**

```bash
cargo test --lib --features settings-ui settings_ui::presenter::tests::capture -- --nocapture
cargo test --lib --features settings-ui settings_ui::dbus_client::tests -- --nocapture
```

Expected: current zero-sized client creates a new session connection for every command and has no Renew/heartbeat.

- [ ] **Step 3: Keep one owner connection**

Replace the zero-sized Default client with:

```rust
#[derive(Clone, Debug)]
pub struct SettingsDbusClient { connection: zbus::blocking::Connection }

impl SettingsDbusClient {
    pub fn connect() -> Result<Self, SettingsClientError> {
        let connection = zbus::blocking::Connection::session()
            .map_err(SettingsClientError::Connection)?;
        Ok(Self { connection })
    }

    fn proxy(&self) -> Result<OpenSwitcherProxyBlocking<'_>, SettingsClientError> {
        OpenSwitcherProxyBlocking::new(&self.connection)
            .map_err(SettingsClientError::Proxy)
    }
}
```

Create it explicitly in `ui.rs`; all four owner mutators reuse this connection. The signal-listener connection may remain separate because it is read-only.

- [ ] **Step 4: Implement heartbeat lifecycle**

After successful Start, schedule single-flight Renew every three seconds with a monotonically increasing local generation. Stop it on Cancel, Finish, Unsupported, timeout, focus/window close, and presenter drop. A late response from an old generation is ignored. Renewal failure transitions the local UI out of capture; daemon expiry remains the safety backstop. Unsupported disarms timeout/heartbeat and sets `capture_active=false` while preserving the error message.

- [ ] **Step 5: Run GREEN and commit**

```bash
cargo test --lib --features settings-ui settings_ui::presenter::tests::capture -- --nocapture
cargo test --lib --features settings-ui settings_ui::dbus_client::tests -- --nocapture
git add src/settings_ui/dbus_client.rs src/settings_ui/presenter.rs src/settings_ui/state.rs src/settings_ui/ui.rs
git commit -m "fix: keep settings capture lease alive on one connection"
```

### Task 6: Safe verification and release gate

**Files:**

- Verify all files changed by Tasks 1-5.

- [ ] **Step 1: Format and run focused suites**

```bash
cargo fmt --check
cargo test --lib capture -- --nocapture
cargo test --lib runtime_capture -- --nocapture
cargo test --lib dbus::tests::capture -- --nocapture
cargo test --test dbus_api capture -- --test-threads=1 --nocapture
cargo test --lib --features settings-ui settings_ui::presenter::tests::capture -- --nocapture
```

If the pinned 1.95.0 toolchain still lacks `cargo-fmt`, do not install a component during the audit. Run the already-installed formatter explicitly instead:

```bash
rustup run stable rustfmt --edition 2021 --check \
  src/daemon/capture.rs src/model.rs src/error/mod.rs \
  src/daemon/runtime.rs src/daemon/service.rs src/dbus/mod.rs src/daemon/mod.rs \
  src/settings_ui/dbus_client.rs src/settings_ui/presenter.rs \
  src/settings_ui/state.rs src/settings_ui/ui.rs
```

- [ ] **Step 2: Run the full safe baseline**

```bash
cargo test --lib
cargo test --lib --features settings-ui
cargo check --all-targets
bash tests/linux_input_setup_test.sh
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
git diff --check
```

These commands must not start the daemon, grab `/dev/input`, create a real uinput device, change clipboard/layout, or modify systemd/udev/ACL.

- [ ] **Step 3: Review and integration gate**

Run spec review and code-quality review against the complete Task 1-5 range. Do not ship or build the acceptance `.deb` from a partial core-only commit. The gate requires owner tests, ledger balance, owner-loss/TTL, D-Bus retained connections, and UI heartbeat all green together.

- [ ] **Step 4: Commit any verification-only corrections**

```bash
git status --short
git log --oneline --decorate -10
```

Expected: clean worktree and the complete H-05 change-set ready for fresh `.deb` build and targeted VM regression.
