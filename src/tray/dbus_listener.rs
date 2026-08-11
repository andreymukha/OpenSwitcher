use crate::dbus::OpenSwitcherProxyBlocking;
use crate::error::SwitcherError;
use crate::system::UserServiceController;
use crate::tray::single_instance::{
    start_daemon_with_retry, DAEMON_RECOVERY_DELAY, MAX_DAEMON_RECOVERY_ATTEMPTS,
};
use crate::tray::tray_service::TrayCommand;
use crate::tray::TrayState;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;
use zbus::names::BusName;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);
const INITIAL_STATE_RETRY_ATTEMPTS: usize = 3;
const INITIAL_STATE_RETRY_DELAY: Duration = Duration::from_millis(1000);

#[derive(Clone)]
pub struct DbusListener {
    connection: Connection,
    services: UserServiceController,
}

impl DbusListener {
    pub fn new() -> Result<Self, SwitcherError> {
        let connection = Connection::session()?;
        Ok(Self::from_connection(connection))
    }

    pub fn from_connection(connection: Connection) -> Self {
        Self {
            connection,
            services: UserServiceController::from_system(),
        }
    }

    pub fn initial_state(&self) -> Result<TrayState, SwitcherError> {
        let proxy = OpenSwitcherProxyBlocking::new(&self.connection)?;
        Ok(TrayState {
            enabled: proxy.is_enabled()?,
            layout_is_english: proxy.current_layout()?,
        })
    }

    pub fn initial_state_with_retry(&self) -> Result<TrayState, SwitcherError> {
        fetch_initial_state_with_retry(
            || self.daemon_is_available(),
            || self.initial_state(),
            INITIAL_STATE_RETRY_ATTEMPTS,
            INITIAL_STATE_RETRY_DELAY,
        )
    }

    pub fn toggle(&self) -> Result<(), SwitcherError> {
        let proxy = OpenSwitcherProxyBlocking::new(&self.connection)?;
        proxy.toggle()?;
        Ok(())
    }

    pub fn request_exit(&self) -> Result<(), SwitcherError> {
        let proxy = OpenSwitcherProxyBlocking::new(&self.connection)?;
        proxy.request_exit()?;
        Ok(())
    }

    pub fn daemon_is_available(&self) -> Result<bool, SwitcherError> {
        let proxy = DBusProxy::new(&self.connection)?;
        let service_name: BusName<'_> = crate::dbus::SERVICE_NAME.try_into().unwrap();
        proxy
            .name_has_owner(service_name)
            .map_err(zbus::Error::from)
            .map_err(SwitcherError::from)
    }

    pub fn ensure_daemon_running(&self) -> Result<(), SwitcherError> {
        if self.daemon_is_available()? {
            return Ok(());
        }

        start_daemon_with_retry(
            &self.services,
            MAX_DAEMON_RECOVERY_ATTEMPTS,
            DAEMON_RECOVERY_DELAY,
        )
        .map_err(std::io::Error::other)?;

        Ok(())
    }

    pub fn spawn_listener(
        &self,
        tx: mpsc::Sender<TrayState>,
        command_tx: async_channel::Sender<TrayCommand>,
    ) {
        let connection = self.connection.clone();
        let services = self.services.clone();
        thread::spawn(move || loop {
            let daemon_available = match DBusProxy::new(&connection) {
                Ok(proxy) => {
                    match proxy.name_has_owner(crate::dbus::SERVICE_NAME.try_into().unwrap()) {
                        Ok(has_owner) => has_owner,
                        Err(err) => {
                            eprintln!("[tray] Failed to query daemon owner on D-Bus: {err}");
                            false
                        }
                    }
                }
                Err(err) => {
                    eprintln!("[tray] Failed to create org.freedesktop.DBus proxy: {err}");
                    false
                }
            };

            if !daemon_available {
                eprintln!("[tray] Daemon is unavailable, attempting recovery...");
                if let Err(err) = start_daemon_with_retry(
                    &services,
                    MAX_DAEMON_RECOVERY_ATTEMPTS,
                    DAEMON_RECOVERY_DELAY,
                ) {
                    eprintln!("[tray] Failed to recover daemon: {err}");
                    let _ = command_tx.try_send(TrayCommand::Quit);
                    break;
                }
            }

            match OpenSwitcherProxyBlocking::new(&connection) {
                Ok(proxy) => {
                    Self::send_current_state(&proxy, &tx);
                    match proxy.receive_status_changed() {
                        Ok(mut stream) => {
                            eprintln!("[tray] Connected to OpenSwitcher D-Bus signal stream");
                            for signal in &mut stream {
                                match signal.args() {
                                    Ok(args) => {
                                        let _ = tx.send(TrayState {
                                            enabled: args.enabled,
                                            layout_is_english: args.layout,
                                        });
                                    }
                                    Err(err) => {
                                        eprintln!(
                                            "[tray] Failed to decode status_changed signal: {err}"
                                        );
                                    }
                                }
                            }
                            eprintln!("[tray] D-Bus signal stream ended, reconnecting...");
                        }
                        Err(err) => {
                            eprintln!("[tray] Failed to subscribe to D-Bus signals: {err}");
                        }
                    }
                }
                Err(err) => {
                    eprintln!("[tray] Failed to create D-Bus proxy: {err}");
                }
            }

            thread::sleep(RECONNECT_DELAY);
        });
    }

    fn send_current_state(proxy: &OpenSwitcherProxyBlocking<'_>, tx: &mpsc::Sender<TrayState>) {
        match (proxy.is_enabled(), proxy.current_layout()) {
            (Ok(enabled), Ok(layout_is_english)) => {
                let _ = tx.send(TrayState {
                    enabled,
                    layout_is_english,
                });
            }
            (enabled, layout) => {
                let enabled_error = enabled.err();
                let layout_error = layout.err();
                eprintln!(
                    "[tray] Failed to refresh current daemon state: enabled={enabled_error:?}, layout={layout_error:?}"
                );
            }
        }
    }
}

fn fetch_initial_state_with_retry<FCheck, FFetch>(
    mut daemon_is_available: FCheck,
    mut initial_state: FFetch,
    attempts: usize,
    delay: Duration,
) -> Result<TrayState, SwitcherError>
where
    FCheck: FnMut() -> Result<bool, SwitcherError>,
    FFetch: FnMut() -> Result<TrayState, SwitcherError>,
{
    assert!(
        attempts > 0,
        "initial state retry requires at least one attempt"
    );

    let mut last_error = None;

    for attempt in 0..attempts {
        match daemon_is_available() {
            Ok(true) => match initial_state() {
                Ok(state) => return Ok(state),
                Err(error) => last_error = Some(error),
            },
            Ok(false) => {}
            Err(error) => last_error = Some(error),
        }

        if attempt + 1 < attempts && !delay.is_zero() {
            thread::sleep(delay);
        }
    }

    Err(last_error.unwrap_or_else(|| {
        std::io::Error::other("OpenSwitcher daemon did not become available in time").into()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    // Initial state retry

    #[test]
    fn initial_state_retry_waits_for_daemon_to_appear() {
        let availability = RefCell::new(VecDeque::from([Ok(false), Ok(false), Ok(true)]));
        let fetch_calls = Cell::new(0);

        let state = fetch_initial_state_with_retry(
            || {
                availability
                    .borrow_mut()
                    .pop_front()
                    .expect("availability result must be queued")
            },
            || {
                fetch_calls.set(fetch_calls.get() + 1);
                Ok(TrayState {
                    enabled: true,
                    layout_is_english: false,
                })
            },
            3,
            Duration::ZERO,
        )
        .unwrap();

        assert!(state.enabled);
        assert!(!state.layout_is_english);
        assert_eq!(fetch_calls.get(), 1);
        assert!(availability.borrow().is_empty());
    }

    #[test]
    fn initial_state_retry_stops_after_bounded_attempts_when_daemon_is_missing() {
        let availability_calls = Cell::new(0);
        let fetch_calls = Cell::new(0);

        let error = fetch_initial_state_with_retry(
            || {
                availability_calls.set(availability_calls.get() + 1);
                Ok(false)
            },
            || {
                fetch_calls.set(fetch_calls.get() + 1);
                Ok(TrayState {
                    enabled: true,
                    layout_is_english: true,
                })
            },
            3,
            Duration::ZERO,
        )
        .unwrap_err();

        assert!(matches!(error, SwitcherError::Io(_)));
        assert_eq!(availability_calls.get(), 3);
        assert_eq!(fetch_calls.get(), 0);
    }
}
