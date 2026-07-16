use crate::dbus::OpenSwitcherProxyBlocking;
use crate::error::SettingsClientError;
use crate::model::{LayoutSwitchCaptureState, Settings, SettingsDto, UpdateSettingsResult};
use async_channel::Sender;
use std::thread;
use std::time::Duration;
use zbus::blocking::Connection;

#[derive(Clone, Debug)]
pub struct SettingsDbusClient {
    connection: Connection,
}

const RECONNECT_DELAY: Duration = Duration::from_millis(500);

impl SettingsDbusClient {
    pub fn connect() -> Result<Self, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        Ok(Self { connection })
    }

    fn proxy(&self) -> Result<OpenSwitcherProxyBlocking<'_>, SettingsClientError> {
        OpenSwitcherProxyBlocking::new(&self.connection).map_err(SettingsClientError::Proxy)
    }

    pub fn load_settings(&self) -> Result<Settings, SettingsClientError> {
        let settings = self
            .proxy()?
            .get_settings()
            .map_err(SettingsClientError::Daemon)?;
        Settings::try_from(settings).map_err(SettingsClientError::from)
    }

    pub fn save_settings(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsClientError> {
        self.proxy()?
            .update_settings(SettingsDto::from(settings))
            .map_err(SettingsClientError::Daemon)
    }

    pub fn start_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        self.proxy()?
            .start_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn renew_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        self.proxy()?
            .renew_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn cancel_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        self.proxy()?
            .cancel_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn finish_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        self.proxy()?
            .finish_layout_switch_capture()
            .map_err(SettingsClientError::Daemon)
    }

    pub fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> Result<(), SettingsClientError> {
        self.proxy()?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_client_retains_connection_state() {
        assert_ne!(std::mem::size_of::<SettingsDbusClient>(), 0);
    }
}
