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
use zbus::blocking::Connection;
use zbus::blocking::fdo::DBusProxy;
use zbus::names::BusName;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);

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

    pub fn spawn_listener(&self, tx: mpsc::Sender<TrayState>, command_tx: async_channel::Sender<TrayCommand>) {
        let connection = self.connection.clone();
        let services = self.services.clone();
        thread::spawn(move || loop {
            let daemon_available = match DBusProxy::new(&connection) {
                Ok(proxy) => match proxy.name_has_owner(
                    crate::dbus::SERVICE_NAME.try_into().unwrap(),
                ) {
                    Ok(has_owner) => has_owner,
                    Err(err) => {
                        eprintln!("[tray] Failed to query daemon owner on D-Bus: {err}");
                        false
                    }
                },
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
