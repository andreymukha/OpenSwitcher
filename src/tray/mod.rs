pub mod dbus_listener;
pub mod single_instance;
pub mod tray_service;

use crate::error::SwitcherError;
use crate::settings_ui::SettingsWindowController;
use adw::prelude::*;
use dbus_listener::DbusListener;
use gtk::gio;
use gtk::glib;
use single_instance::{acquire_tray_instance, TrayInstanceError};
use tray_service::{OpenSwitcherTray, TrayCommand};
use zbus::blocking::Connection;

pub use tray_service::TrayState;
pub const TRAY_APPLICATION_ID: &str = "org.oswitch.tray.app";

pub fn run() -> Result<(), SwitcherError> {
    let connection = Connection::session()?;
    match acquire_tray_instance(&connection) {
        Ok(()) => {}
        Err(TrayInstanceError::AlreadyRunning) => {
            eprintln!("[tray] Another tray instance is already running, exiting.");
            return Ok(());
        }
        Err(TrayInstanceError::Dbus(error)) => {
            eprintln!("[tray] Failed to acquire tray single-instance guard: {error}");
            return Ok(());
        }
    }

    let client = DbusListener::from_connection(connection);
    if let Err(err) = client.ensure_daemon_running() {
        eprintln!("[tray] Failed to ensure daemon is running: {err}");
        return Ok(());
    }

    let initial_state = match client.initial_state_with_retry() {
        Ok(state) => state,
        Err(err) => {
            eprintln!("[tray] Failed to fetch initial daemon state: {err}");
            return Ok(());
        }
    };
    let initialized = std::rc::Rc::new(std::cell::Cell::new(false));
    let hold_guard = std::rc::Rc::new(std::cell::RefCell::new(None::<gio::ApplicationHoldGuard>));
    let app = adw::Application::builder()
        .application_id(TRAY_APPLICATION_ID)
        .build();

    {
        let initialized = initialized.clone();
        let hold_guard = hold_guard.clone();
        let client = client.clone();

        app.connect_activate(move |app| {
            if initialized.replace(true) {
                return;
            }

            hold_guard.borrow_mut().replace(app.hold());

            let settings_controller = SettingsWindowController::embedded();
            let (command_tx, command_rx) = async_channel::unbounded();

            {
                let app = app.clone();
                let settings_controller = settings_controller.clone();
                glib::MainContext::default().spawn_local(async move {
                    while let Ok(command) = command_rx.recv().await {
                        match command {
                            TrayCommand::ShowSettings => settings_controller.present(&app),
                            TrayCommand::Quit => {
                                app.quit();
                                break;
                            }
                        }
                    }
                });
            }

            spawn_tray_backend(client.clone(), initial_state, command_tx);
        });
    }

    app.run();

    Ok(())
}

fn spawn_tray_backend(
    client: DbusListener,
    initial_state: TrayState,
    command_tx: async_channel::Sender<TrayCommand>,
) {
    let (state_tx, state_rx) = std::sync::mpsc::channel();
    let tray = OpenSwitcherTray::new(client.clone(), initial_state, command_tx.clone());
    let service = ksni::TrayService::new(tray);
    let handle = service.handle();
    service.spawn();

    client.spawn_listener(state_tx, command_tx.clone());

    std::thread::spawn(move || {
        for state in state_rx {
            handle.update(|tray| {
                tray.state = state;
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::TRAY_APPLICATION_ID;
    use crate::tray::single_instance::TRAY_SERVICE_NAME;

    // Tray app identity

    #[test]
    fn tray_application_id_does_not_reuse_single_instance_dbus_name() {
        assert_ne!(TRAY_APPLICATION_ID, TRAY_SERVICE_NAME);
    }
}
