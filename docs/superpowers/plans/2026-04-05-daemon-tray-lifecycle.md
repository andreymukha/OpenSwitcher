# Daemon + Tray Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add user-level autostart, single-instance protection, and bounded mutual recovery so OpenSwitcher daemon and tray behave as one user-facing application.

**Architecture:** Keep `daemon` and `tray` as separate binaries but make `systemd --user` the official startup path and D-Bus the presence/single-instance mechanism. Add a tiny `systemctl --user` wrapper for autostart and restart actions, keep the existing daemon D-Bus API intact, and add only the minimal presence/recovery glue needed for `daemon + tray` as a pair.

**Tech Stack:** Rust, `std::process::Command`, `std::thread`, blocking `zbus`, GTK/libadwaita settings UI, systemd user units, `.desktop` launcher

---

## File Map

- Create: `src/system/user_services.rs`
  User-level `systemctl --user` wrapper, retryable start helpers, and typed service-management errors.
- Create: `src/tray/single_instance.rs`
  Tray single-instance guard using a dedicated well-known D-Bus name.
- Create: `dist/systemd/open-switcher-daemon.service`
  User unit for daemon.
- Create: `dist/systemd/open-switcher-tray.service`
  User unit for tray.
- Create: `dist/open-switcher.desktop`
  Desktop entry that launches only tray.
- Modify: `src/system/mod.rs`
  Re-export user-service helpers.
- Modify: `src/error/mod.rs`
  Export new service-management error type.
- Modify: `src/error/ui_error.rs`
  Add UI-facing autostart/service-management error variants if needed.
- Modify: `src/daemon/mod.rs`
  Add tray presence watchdog and bounded recovery.
- Modify: `src/tray/mod.rs`
  Acquire tray single-instance name, ensure daemon is running, and shut down tray if daemon cannot be recovered.
- Modify: `src/tray/dbus_listener.rs`
  Add bounded reconnect/restart logic for daemon loss without changing the daemon D-Bus API.
- Modify: `src/settings_ui/state.rs`
  Extend view state with autostart status that is not persisted in `Settings`.
- Modify: `src/settings_ui/presenter.rs`
  Load and toggle autostart through the new system service wrapper.
- Modify: `src/settings_ui/dbus_client.rs`
  Expose sync helpers used by the presenter if a shared client shape is preferable.
- Modify: `src/settings_ui/ui.rs`
  Add the autostart checkbox row and bind it to presenter events.
- Modify: `src/README.md`
  Document the new systemd user units, tray-first startup model, and install flow.
- Test: `tests/dbus_api.rs`
  Extend only if a black-box daemon/tray lifecycle check is possible without making tests flaky.
- Test: `src/system/user_services.rs`
  Unit tests for command/result mapping and retry policy helpers.
- Test: `src/tray/single_instance.rs`
  Unit tests for duplicate-instance detection boundaries if extracted cleanly.
- Test: `src/settings_ui/state.rs`
  View-state tests for autostart checkbox behavior.

### Task 1: User Service Control Layer

**Files:**
- Create: `src/system/user_services.rs`
- Modify: `src/system/mod.rs`
- Modify: `src/error/mod.rs`
- Test: `src/system/user_services.rs`

- [ ] **Step 1: Write the failing tests for systemctl command mapping**

```rust
#[test]
fn enable_autostart_enables_and_starts_daemon_and_tray() {
    let mut runner = FakeCommandRunner::default();
    runner.push_ok("");
    runner.push_ok("");
    runner.push_ok("");
    runner.push_ok("");

    let services = UserServiceController::new(runner.clone());
    services.enable_autostart().unwrap();

    assert_eq!(
        runner.commands(),
        vec![
            vec!["systemctl", "--user", "enable", "open-switcher-daemon.service"],
            vec!["systemctl", "--user", "enable", "open-switcher-tray.service"],
            vec!["systemctl", "--user", "start", "open-switcher-daemon.service"],
            vec!["systemctl", "--user", "start", "open-switcher-tray.service"],
        ]
    );
}

#[test]
fn autostart_checkbox_state_comes_from_daemon_unit_enabled_state() {
    let mut runner = FakeCommandRunner::default();
    runner.push_ok("enabled\n");

    let services = UserServiceController::new(runner);
    assert!(services.is_autostart_enabled().unwrap());
}

#[test]
fn systemctl_failure_is_reported_as_runtime_failure() {
    let mut runner = FakeCommandRunner::default();
    runner.push_err(1, "permission denied");

    let services = UserServiceController::new(runner);
    let err = services.start_daemon_service().unwrap_err();

    assert!(matches!(err, ServiceManagerError::CommandFailed { .. }));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib system::user_services`
Expected: FAIL because `user_services` module and controller types do not exist yet.

- [ ] **Step 3: Implement the minimal service-management layer**

```rust
pub const DAEMON_UNIT: &str = "open-switcher-daemon.service";
pub const TRAY_UNIT: &str = "open-switcher-tray.service";

#[derive(Clone)]
pub struct UserServiceController<R = ProcessCommandRunner> {
    runner: R,
}

impl<R: CommandRunner> UserServiceController<R> {
    pub fn enable_autostart(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "enable", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "enable", TRAY_UNIT])?;
        self.run(["systemctl", "--user", "start", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "start", TRAY_UNIT])?;
        Ok(())
    }

    pub fn disable_autostart(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "disable", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "disable", TRAY_UNIT])?;
        self.run(["systemctl", "--user", "stop", TRAY_UNIT])?;
        self.run(["systemctl", "--user", "stop", DAEMON_UNIT])?;
        Ok(())
    }

    pub fn is_autostart_enabled(&self) -> Result<bool, ServiceManagerError> {
        let output = self.run(["systemctl", "--user", "is-enabled", DAEMON_UNIT])?;
        Ok(output.trim() == "enabled")
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib system::user_services`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/system/mod.rs src/system/user_services.rs src/error/mod.rs
git commit -m "feat: add user service control layer"
```

### Task 2: Settings UI Autostart Checkbox

**Files:**
- Modify: `src/settings_ui/state.rs`
- Modify: `src/settings_ui/presenter.rs`
- Modify: `src/settings_ui/ui.rs`
- Modify: `src/error/ui_error.rs`
- Test: `src/settings_ui/state.rs`

- [ ] **Step 1: Write the failing view-state tests for autostart**

```rust
#[test]
fn autostart_checkbox_is_not_part_of_dirty_settings_state() {
    let mut state = DomainState::new();
    state.apply_loaded(Settings::default());
    state.set_autostart_enabled(true);

    let view = state.view_state();
    assert!(view.autostart_enabled);
    assert!(!view.dirty);
    assert!(!view.save_enabled);
}

#[test]
fn autostart_toggle_can_be_temporarily_busy_without_disabling_form_dirty_logic() {
    let mut state = DomainState::new();
    state.apply_loaded(Settings::default());
    state.begin_autostart_change();

    let view = state.view_state();
    assert!(view.autostart_busy);
    assert!(view.cancel_enabled);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib settings_ui::state`
Expected: FAIL because `autostart_enabled` and `autostart_busy` are missing from state/view models.

- [ ] **Step 3: Implement minimal UI state and presenter flow**

```rust
pub struct ViewState {
    pub autostart_enabled: bool,
    pub autostart_busy: bool,
    // existing fields...
}

impl SettingsPresenter {
    pub fn reload(&self) {
        // existing settings load...
        match self.inner.client.is_autostart_enabled() {
            Ok(enabled) => self.with_state(|state| state.set_autostart_enabled(enabled)),
            Err(error) => { /* emit non-fatal UI error */ }
        }
    }

    pub fn set_autostart_enabled(&self, enabled: bool) {
        // spawn thread, call enable_autostart()/disable_autostart(), emit updated view state
    }
}
```

- [ ] **Step 4: Add the checkbox row to the settings UI**

```rust
let autostart_row = adw::ActionRow::builder()
    .title("Автозапуск")
    .subtitle("Запускать daemon и tray через systemd --user")
    .build();
let autostart_switch = gtk::Switch::builder()
    .valign(gtk::Align::Center)
    .build();
autostart_row.add_suffix(&autostart_switch);
autostart_row.set_activatable_widget(Some(&autostart_switch));
group.add(&autostart_row);
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib settings_ui::state`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/settings_ui/state.rs src/settings_ui/presenter.rs src/settings_ui/ui.rs src/error/ui_error.rs
git commit -m "feat: add autostart checkbox to settings"
```

### Task 3: Tray Single-Instance and Daemon Startup Recovery

**Files:**
- Create: `src/tray/single_instance.rs`
- Modify: `src/tray/mod.rs`
- Modify: `src/tray/dbus_listener.rs`
- Modify: `src/tray/tray_service.rs`
- Test: `src/tray/single_instance.rs`

- [ ] **Step 1: Write the failing single-instance and recovery tests**

```rust
#[test]
fn second_tray_instance_is_rejected_when_name_is_taken() {
    let bus = FakeSessionBus::with_name_taken("org.oswitch.tray");
    let err = acquire_tray_instance(bus).unwrap_err();
    assert!(matches!(err, TrayInstanceError::AlreadyRunning));
}

#[test]
fn daemon_restart_is_attempted_three_times_before_tray_gives_up() {
    let mut services = FakeUserServices::default();
    services.start_daemon_results = vec![Err(fake_err()), Err(fake_err()), Err(fake_err())];

    let result = recover_daemon(&services, 3, Duration::from_millis(1));
    assert!(result.is_err());
    assert_eq!(services.start_daemon_calls(), 3);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tray::single_instance`
Expected: FAIL because tray guard and bounded recovery helpers do not exist yet.

- [ ] **Step 3: Implement the tray instance guard and startup checks**

```rust
pub const TRAY_SERVICE_NAME: &str = "org.oswitch.tray";

pub fn acquire_tray_instance(connection: &Connection) -> Result<OwnedNameGuard, TrayInstanceError> {
    connection.request_name(TRAY_SERVICE_NAME)?;
    Ok(OwnedNameGuard::new(connection.clone(), TRAY_SERVICE_NAME))
}

pub fn ensure_daemon_available(
    services: &UserServiceController,
    listener: &DbusListener,
) -> Result<(), SwitcherError> {
    if listener.daemon_is_available()? {
        return Ok(());
    }
    retry_start(|| services.start_daemon_service())
}
```

- [ ] **Step 4: Extend tray reconnect logic without changing the daemon D-Bus API**

```rust
match OpenSwitcherProxyBlocking::new(&connection) {
    Ok(proxy) => { /* existing stream */ }
    Err(err) => {
        eprintln!("[tray] daemon unavailable: {err}");
        if let Err(start_err) = services.start_daemon_service() {
            eprintln!("[tray] failed to recover daemon: {start_err}");
            let _ = command_tx.send(TrayCommand::Quit);
            break;
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib tray::single_instance`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/tray/single_instance.rs src/tray/mod.rs src/tray/dbus_listener.rs src/tray/tray_service.rs
git commit -m "feat: add tray single-instance and daemon recovery"
```

### Task 4: Daemon Tray Watchdog and Self-Termination

**Files:**
- Modify: `src/daemon/mod.rs`
- Modify: `src/daemon/runtime.rs`
- Modify: `src/dbus/mod.rs` (only if a helper is needed for `NameHasOwner`; otherwise leave unchanged)
- Test: `src/daemon/runtime.rs`

- [ ] **Step 1: Write the failing watchdog tests**

```rust
#[test]
fn daemon_attempts_to_restart_tray_three_times_then_requests_exit() {
    let mut services = FakeUserServices::default();
    services.start_tray_results = vec![Err(fake_err()), Err(fake_err()), Err(fake_err())];
    let runtime = RuntimeState::new_for_tests();

    run_tray_watchdog_iteration(&runtime, false, &services, 3, Duration::from_millis(1));

    assert!(runtime.exit_requested());
    assert_eq!(services.start_tray_calls(), 3);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon::runtime daemon::mod`
Expected: FAIL because watchdog helpers do not exist yet.

- [ ] **Step 3: Implement minimal tray-presence checking and bounded recovery**

```rust
fn spawn_tray_watchdog(runtime: Arc<RuntimeState>) {
    std::thread::spawn(move || loop {
        if runtime.exit_requested() {
            break;
        }

        if tray_name_has_owner() {
            reset_attempts();
            std::thread::sleep(TRAY_WATCHDOG_INTERVAL);
            continue;
        }

        for _ in 0..3 {
            if user_services.start_tray_service().is_ok() && tray_name_has_owner() {
                continue 'outer;
            }
            std::thread::sleep(RECOVERY_DELAY);
        }

        log_input_debug("tray-watchdog-exit", "tray recovery failed");
        runtime.request_exit();
        break;
    });
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib daemon::runtime daemon::mod`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/daemon/mod.rs src/daemon/runtime.rs
git commit -m "feat: add daemon tray watchdog"
```

### Task 5: Assets, Documentation, and Install Flow

**Files:**
- Create: `dist/systemd/open-switcher-daemon.service`
- Create: `dist/systemd/open-switcher-tray.service`
- Create: `dist/open-switcher.desktop`
- Modify: `README.md`

- [ ] **Step 1: Write the failing documentation/asset check**

```bash
test -f dist/systemd/open-switcher-daemon.service
test -f dist/systemd/open-switcher-tray.service
test -f dist/open-switcher.desktop
rg -n "systemd --user|open-switcher-tray|open-switcher-daemon.service" README.md
```

Expected: FAIL because the assets do not exist yet and README does not document the lifecycle model.

- [ ] **Step 2: Add the user unit files**

```ini
[Unit]
Description=OpenSwitcher daemon

[Service]
ExecStart=open-switcher-daemon
Restart=on-failure

[Install]
WantedBy=default.target
```

```ini
[Unit]
Description=OpenSwitcher tray
After=open-switcher-daemon.service

[Service]
ExecStart=open-switcher-tray
Restart=on-failure

[Install]
WantedBy=default.target
```

- [ ] **Step 3: Add the desktop entry**

```ini
[Desktop Entry]
Type=Application
Name=OpenSwitcher
Exec=open-switcher-tray
Categories=Utility;
```

- [ ] **Step 4: Document install and runtime behavior**

```markdown
1. Install `open-switcher-daemon`, `open-switcher-tray`, `open-switcher.desktop`, and both user unit files.
2. Enable autostart from the settings UI or with `systemctl --user enable --now open-switcher-daemon.service open-switcher-tray.service`.
3. Launching from the desktop entry starts tray; tray ensures daemon is present.
```

- [ ] **Step 5: Run the check to verify it passes**

Run:

```bash
test -f dist/systemd/open-switcher-daemon.service
test -f dist/systemd/open-switcher-tray.service
test -f dist/open-switcher.desktop
rg -n "systemd --user|open-switcher-tray|open-switcher-daemon.service" README.md
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add dist/systemd/open-switcher-daemon.service dist/systemd/open-switcher-tray.service dist/open-switcher.desktop README.md
git commit -m "docs: add lifecycle install assets"
```

### Task 6: Full Verification

**Files:**
- Verify: `src/system/user_services.rs`
- Verify: `src/tray/mod.rs`
- Verify: `src/tray/dbus_listener.rs`
- Verify: `src/daemon/mod.rs`
- Verify: `src/settings_ui/*`
- Verify: `dist/systemd/*`
- Verify: `dist/open-switcher.desktop`

- [ ] **Step 1: Run focused library tests**

Run: `cargo test --lib system::user_services tray::single_instance settings_ui::state daemon::runtime`
Expected: PASS

- [ ] **Step 2: Run the full test suite**

Run: `cargo test -q`
Expected: PASS

- [ ] **Step 3: Run a build check for feature-complete binaries**

Run: `cargo check --all-targets --features settings-ui`
Expected: PASS

- [ ] **Step 4: Manual runtime sanity checks**

Run:

```bash
systemctl --user daemon-reload
systemctl --user start open-switcher-daemon.service
systemctl --user start open-switcher-tray.service
systemctl --user status open-switcher-daemon.service
systemctl --user status open-switcher-tray.service
gdbus call --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner org.oswitch.core
gdbus call --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.NameHasOwner org.oswitch.tray
```

Expected:
- both services are active or restarted successfully
- `org.oswitch.core` returns `(true,)`
- `org.oswitch.tray` returns `(true,)`

- [ ] **Step 5: Final commit**

```bash
git status --short
```

Expected: clean tree or only the intended lifecycle changes staged and committed already.

