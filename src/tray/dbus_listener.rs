use crate::dbus::OpenSwitcherProxyBlocking;
use crate::error::SwitcherError;
use crate::tray::TrayState;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use zbus::blocking::Connection;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct DbusListener {
    connection: Connection,
}

impl DbusListener {
    pub fn new() -> Result<Self, SwitcherError> {
        Ok(Self {
            connection: Connection::session()?,
        })
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

    pub fn spawn_listener(&self, tx: mpsc::Sender<TrayState>) {
        let connection = self.connection.clone();
        thread::spawn(move || loop {
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
