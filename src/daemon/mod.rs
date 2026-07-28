pub mod capture;
pub(crate) mod debug_log;
pub(crate) mod deferred_input;
pub mod input_backend;
pub(crate) mod input_snapshot;
pub mod keyboard;
pub mod layout_switcher;
pub mod runtime;
pub mod selected_text;
pub mod service;
pub mod switch_logic;
pub(crate) mod synthetic_input;
pub(crate) mod uinput_synthetic;
pub(crate) mod x11_wait;

use crate::config::default_config_path;
use crate::dbus::{CaptureOwnerMonitor, OpenSwitcherDbusApi, OBJECT_PATH, SERVICE_NAME};
use crate::error::SwitcherError;
use crate::system::is_dev_runtime_mode;
use keyboard::{log_input_debug, WriterShutdownOutcome};
use runtime::{log_layout_debug, BackendSyncResult, ConfigService, RuntimeState};
use service::DaemonService;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;
use zbus::blocking::ConnectionBuilder;

const TRAY_SERVICE_NAME: &str = "org.oswitch.tray";

struct SessionBusTrayPresenceProbe {
    connection: Connection,
}

impl runtime::TrayPresenceProbe for SessionBusTrayPresenceProbe {
    fn tray_is_present(&self) -> Result<bool, std::io::Error> {
        let proxy = DBusProxy::new(&self.connection).map_err(std::io::Error::other)?;
        let name = TRAY_SERVICE_NAME.try_into().unwrap();
        proxy.name_has_owner(name).map_err(std::io::Error::other)
    }
}

pub fn run() -> Result<(), SwitcherError> {
    let _debug_log_runtime = debug_log::DebugLogRuntime::initialize_from_env();
    let config_service = ConfigService::load(default_config_path())?;
    let runtime = Arc::new(RuntimeState::new(config_service));
    match runtime.config_snapshot() {
        Ok(snapshot) => log_layout_debug(
            "startup-config",
            &format!("layout_switch_combo={:?}", snapshot.layout_switch_combo),
        ),
        Err(error) => log_layout_debug(
            "startup-config",
            &format!("layout_switch_combo=unavailable error={error}"),
        ),
    }
    match runtime.initial_input_refresh_before_grab() {
        BackendSyncResult::Updated { current, .. } => {
            log_layout_debug(
                "startup-sync",
                &format!("source=backend current={current:?}"),
            );
        }
        BackendSyncResult::Unchanged => {
            log_layout_debug("startup-sync", "source=backend unchanged=true");
        }
        BackendSyncResult::Skipped => {
            log_layout_debug("startup-sync", "source=backend skipped=true");
        }
    }
    runtime.start_background_sync_polling();
    if !is_dev_runtime_mode() {
        let tray_probe = SessionBusTrayPresenceProbe {
            connection: Connection::session()?,
        };
        runtime.start_tray_watchdog(tray_probe);
    } else {
        log_layout_debug(
            "tray-watchdog-start",
            "enabled=false reason=dev-runtime-mode",
        );
    }
    let (connection, mut capture_owner_monitor) =
        start_dbus_endpoint(runtime.clone(), SERVICE_NAME)?;

    let mut service = match DaemonService::new(runtime, connection) {
        Ok(service) => service,
        Err(error) => {
            return finalize_service_initialization_error(error, |mode| {
                shutdown_capture_owner_monitor(&mut capture_owner_monitor, mode)
            });
        }
    };
    let (result, input_loop_postmortem) =
        match panic::catch_unwind(AssertUnwindSafe(|| service.run())) {
            Ok(result) => (result, None),
            Err(payload) => {
                let reason = if let Some(text) = payload.downcast_ref::<&str>() {
                    (*text).to_owned()
                } else if let Some(text) = payload.downcast_ref::<String>() {
                    text.clone()
                } else {
                    "unknown panic payload".to_owned()
                };
                log_input_debug("event-loop-panic", &format!("reason={reason}"));
                (Err(SwitcherError::DaemonPanicked), Some(reason))
            }
        };
    finalize_daemon_run_with_postmortem(
        result,
        || service.shutdown(),
        |mode| shutdown_capture_owner_monitor(&mut capture_owner_monitor, mode),
        move || {
            if let Some(reason) = input_loop_postmortem {
                eprintln!("[input] Демон аварийно завершился в input loop: {reason}");
            }
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecondaryShutdownMode {
    Join,
    DetachForProcessFailStop,
}

fn shutdown_capture_owner_monitor(
    monitor: &mut CaptureOwnerMonitor,
    mode: SecondaryShutdownMode,
) -> std::thread::Result<()> {
    match mode {
        SecondaryShutdownMode::Join => monitor.stop(),
        SecondaryShutdownMode::DetachForProcessFailStop => {
            monitor.detach_for_process_fail_stop();
            Ok(())
        }
    }
}

fn finalize_service_initialization_error<StopMonitor>(
    error: SwitcherError,
    stop_monitor: StopMonitor,
) -> Result<(), SwitcherError>
where
    StopMonitor: FnOnce(SecondaryShutdownMode) -> std::thread::Result<()>,
{
    finalize_daemon_run(Err(error), || WriterShutdownOutcome::Stopped, stop_monitor)
}

fn finalize_daemon_run_with_postmortem<Shutdown, StopMonitor, Postmortem>(
    result: Result<(), SwitcherError>,
    shutdown: Shutdown,
    stop_monitor: StopMonitor,
    postmortem: Postmortem,
) -> Result<(), SwitcherError>
where
    Shutdown: FnOnce() -> WriterShutdownOutcome,
    StopMonitor: FnOnce(SecondaryShutdownMode) -> std::thread::Result<()>,
    Postmortem: FnOnce(),
{
    let result = finalize_daemon_run(result, shutdown, stop_monitor);
    postmortem();
    result
}

fn finalize_daemon_run<Shutdown, StopMonitor>(
    result: Result<(), SwitcherError>,
    shutdown: Shutdown,
    stop_monitor: StopMonitor,
) -> Result<(), SwitcherError>
where
    Shutdown: FnOnce() -> WriterShutdownOutcome,
    StopMonitor: FnOnce(SecondaryShutdownMode) -> std::thread::Result<()>,
{
    let prior_fail_stop = matches!(
        &result,
        Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive { .. })
    );
    let shutdown_outcome = shutdown();
    let monitor_mode = if prior_fail_stop
        || matches!(shutdown_outcome, WriterShutdownOutcome::Unresponsive { .. })
    {
        SecondaryShutdownMode::DetachForProcessFailStop
    } else {
        SecondaryShutdownMode::Join
    };
    let result = resolve_daemon_result_after_shutdown(result, shutdown_outcome);

    if stop_monitor(monitor_mode).is_err() {
        log_layout_debug(
            "dbus-capture-owner-monitor-stop-error",
            "worker_panicked=true",
        );
        eprintln!("[dbus] Capture owner monitor worker panicked during shutdown");
    }
    result
}

fn resolve_daemon_result_after_shutdown(
    result: Result<(), SwitcherError>,
    shutdown_outcome: WriterShutdownOutcome,
) -> Result<(), SwitcherError> {
    if matches!(
        &result,
        Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive { .. })
    ) {
        return result;
    }

    match shutdown_outcome {
        WriterShutdownOutcome::Stopped => result,
        WriterShutdownOutcome::Unresponsive { timeout_ms } => {
            let trigger = match result {
                Ok(()) => "daemon run completed before writer shutdown".to_owned(),
                Err(error) => error.to_string(),
            };
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms,
                phase: "daemon-finalize",
                trigger,
            })
        }
    }
}

fn start_dbus_endpoint(
    runtime: Arc<RuntimeState>,
    service_name: &str,
) -> Result<(Connection, CaptureOwnerMonitor), SwitcherError> {
    let (connection, monitor) = prepare_dbus_endpoint(runtime)?;
    connection.request_name(service_name)?;
    Ok((connection, monitor))
}

fn prepare_dbus_endpoint(
    runtime: Arc<RuntimeState>,
) -> Result<(Connection, CaptureOwnerMonitor), SwitcherError> {
    let connection = ConnectionBuilder::session()?.build()?;
    let monitor = CaptureOwnerMonitor::start(&connection, runtime.clone())?;
    connection
        .object_server()
        .at(OBJECT_PATH, OpenSwitcherDbusApi::new(runtime))?;
    Ok((connection, monitor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::keyboard::WriterShutdownOutcome;
    use crate::dbus::INTERFACE_NAME;
    use crate::model::{LayoutSwitchCapturePhase, LayoutSwitchCaptureState};
    use std::cell::RefCell;
    use std::error::Error;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;
    use zbus::blocking::Proxy;

    #[test]
    fn daemon_error_releases_input_before_potentially_blocking_monitor_stop() {
        let input_released = Arc::new(AtomicBool::new(false));
        let observed_input_released = Arc::clone(&input_released);
        let (monitor_entered_tx, monitor_entered_rx) = mpsc::channel();
        let (allow_monitor_stop_tx, allow_monitor_stop_rx) = mpsc::channel();

        let finalizer = std::thread::spawn(move || {
            finalize_daemon_run(
                Err(SwitcherError::KeyboardNotFound),
                || {
                    input_released.store(true, Ordering::SeqCst);
                    WriterShutdownOutcome::Stopped
                },
                |mode| {
                    assert_eq!(mode, SecondaryShutdownMode::Join);
                    monitor_entered_tx.send(()).unwrap();
                    allow_monitor_stop_rx.recv().unwrap();
                    Ok(())
                },
            )
        });

        monitor_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("monitor stop must be entered");
        assert!(
            observed_input_released.load(Ordering::SeqCst),
            "input must be released before monitor stop can block"
        );

        allow_monitor_stop_tx.send(()).unwrap();
        assert!(matches!(
            finalizer.join().unwrap(),
            Err(SwitcherError::KeyboardNotFound)
        ));
    }

    #[test]
    fn clean_shutdown_preserves_primary_daemon_error_and_stops_monitor() {
        let phases = RefCell::new(Vec::new());

        let result = finalize_daemon_run(
            Err(SwitcherError::KeyboardNotFound),
            || {
                phases.borrow_mut().push("release-input");
                WriterShutdownOutcome::Stopped
            },
            |mode| {
                assert_eq!(mode, SecondaryShutdownMode::Join);
                phases.borrow_mut().push("join-monitor");
                Ok(())
            },
        );

        assert!(matches!(result, Err(SwitcherError::KeyboardNotFound)));
        assert_eq!(*phases.borrow(), vec!["release-input", "join-monitor"]);
    }

    #[test]
    fn unresponsive_shutdown_skips_monitor_join_and_returns_fatal_error() {
        let (join_entered_tx, join_entered_rx) = mpsc::channel();

        let result = finalize_daemon_run(
            Ok(()),
            || WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 },
            |mode| {
                if mode == SecondaryShutdownMode::Join {
                    join_entered_tx.send(()).unwrap();
                }
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 1_000,
                phase: "daemon-finalize",
                ..
            })
        ));
        assert!(matches!(
            join_entered_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn unresponsive_shutdown_preserves_primary_error_in_trigger() {
        let result = finalize_daemon_run(
            Err(SwitcherError::KeyboardNotFound),
            || WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 },
            |mode| {
                assert_eq!(mode, SecondaryShutdownMode::DetachForProcessFailStop);
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 1_000,
                phase: "daemon-finalize",
                ref trigger,
            }) if trigger == "Keyboard device was not found"
        ));
    }

    #[test]
    fn prior_fail_stop_is_not_masked_by_repeated_shutdown() {
        let result = finalize_daemon_run(
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 900,
                phase: "backend-open",
                trigger: "startup cleanup".to_owned(),
            }),
            || WriterShutdownOutcome::Stopped,
            |mode| {
                assert_eq!(mode, SecondaryShutdownMode::DetachForProcessFailStop);
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 900,
                phase: "backend-open",
                ref trigger,
            }) if trigger == "startup cleanup"
        ));
    }

    #[test]
    fn service_initialization_fail_stop_detaches_monitor() {
        let result = finalize_service_initialization_error(
            SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 800,
                phase: "backend-open",
                trigger: "dependent worker startup failed".to_owned(),
            },
            |mode| {
                assert_eq!(mode, SecondaryShutdownMode::DetachForProcessFailStop);
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 800,
                phase: "backend-open",
                ..
            })
        ));
    }

    #[test]
    fn input_loop_postmortem_is_reported_only_after_backend_shutdown() {
        let phases = RefCell::new(Vec::new());

        let result = finalize_daemon_run_with_postmortem(
            Err(SwitcherError::DaemonPanicked),
            || {
                phases.borrow_mut().push("release-input");
                WriterShutdownOutcome::Stopped
            },
            |mode| {
                assert_eq!(mode, SecondaryShutdownMode::Join);
                phases.borrow_mut().push("stop-monitor");
                Ok(())
            },
            || phases.borrow_mut().push("report-panic"),
        );

        assert!(matches!(result, Err(SwitcherError::DaemonPanicked)));
        assert_eq!(
            *phases.borrow(),
            vec!["release-input", "stop-monitor", "report-panic"]
        );
    }

    #[test]
    fn dbus_endpoint_is_published_only_after_monitor_and_api_are_ready(
    ) -> Result<(), Box<dyn Error>> {
        let temp_dir = TempDir::new()?;
        let runtime = Arc::new(RuntimeState::new(ConfigService::load(
            temp_dir.path().join("config.toml"),
        )?));
        let service_name = unique_service_name();

        let (service, mut monitor) = prepare_dbus_endpoint(runtime)?;
        let client = Connection::session()?;
        let bus = DBusProxy::new(&client)?;
        assert!(!bus.name_has_owner(service_name.as_str().try_into()?)?);

        service.request_name(service_name.as_str())?;
        assert!(bus.name_has_owner(service_name.as_str().try_into()?)?);
        let proxy = Proxy::new(&client, service_name.as_str(), OBJECT_PATH, INTERFACE_NAME)?;
        let state: LayoutSwitchCaptureState = proxy.call("GetLayoutSwitchCaptureState", &())?;

        assert_eq!(state.phase, LayoutSwitchCapturePhase::Idle);
        assert!(monitor.stop().is_ok());
        Ok(())
    }

    fn unique_service_name() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        format!(
            "org.oswitch.core.endpoint_test.p{}.n{nanos}",
            std::process::id()
        )
    }
}
