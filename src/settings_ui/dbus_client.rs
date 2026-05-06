use crate::dbus::OpenSwitcherProxyBlocking;
use crate::error::SettingsClientError;
use crate::model::{LayoutSwitchCaptureState, Settings, SettingsDto, UpdateSettingsResult};
use async_channel::Sender;
use std::thread;
use std::time::Duration;
use zbus::blocking::Connection;

#[derive(Clone, Debug, Default)]
pub struct SettingsDbusClient;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);

impl SettingsDbusClient {
    pub fn load_settings(&self) -> Result<Settings, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        let settings = proxy.get_settings().map_err(SettingsClientError::Daemon)?;
        Settings::try_from(settings).map_err(SettingsClientError::from)
    }

    pub fn save_settings(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        proxy
            .update_settings(SettingsDto::from(settings))
            .map_err(SettingsClientError::Daemon)
    }

    pub fn start_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        proxy
            .start_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn cancel_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        proxy
            .cancel_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn finish_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        proxy
            .finish_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn set_hotkey_capture_inhibited(
        &self,
        inhibited: bool,
    ) -> Result<(), SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        proxy
            .set_hotkey_capture_inhibited(inhibited)
            .map_err(SettingsClientError::Daemon)
    }

    pub fn spawn_capture_listener(&self, tx: Sender<LayoutSwitchCaptureState>) {
        thread::spawn(move || loop {
            let connection = match Connection::session() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[settings] Failed to connect capture listener to D-Bus: {error}");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
            };

            match OpenSwitcherProxyBlocking::new(&connection) {
                Ok(proxy) => {
                    match proxy.get_layout_switch_capture_state() {
                        Ok(state) => {
                            let _ = tx.send_blocking(state);
                        }
                        Err(error) => {
                            eprintln!(
                                "[settings] Failed to fetch current capture session state: {error}"
                            );
                        }
                    }

                    match proxy.receive_layout_switch_capture_state_changed() {
                        Ok(mut stream) => {
                            for signal in &mut stream {
                                match signal.args() {
                                    Ok(args) => {
                                        let _ = tx.send_blocking(args.state);
                                    }
                                    Err(error) => {
                                        eprintln!(
                                            "[settings] Failed to decode capture state signal: {error}"
                                        );
                                    }
                                }
                            }
                            eprintln!("[settings] Capture signal stream ended, reconnecting...");
                        }
                        Err(error) => {
                            eprintln!(
                                "[settings] Failed to subscribe to capture state signals: {error}"
                            );
                        }
                    }
                }
                Err(error) => {
                    eprintln!("[settings] Failed to create capture D-Bus proxy: {error}");
                }
            }

            thread::sleep(RECONNECT_DELAY);
        });
    }
}
