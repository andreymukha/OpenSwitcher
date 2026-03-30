pub mod dbus_listener;
pub mod tray_service;

use crate::error::SwitcherError;
use crate::settings_ui::SettingsWindowController;
use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use dbus_listener::DbusListener;
use tray_service::{OpenSwitcherTray, TrayCommand};

pub use tray_service::TrayState;
pub fn run() -> Result<(), SwitcherError> {
    let client = DbusListener::new()?;
    let initial_state = client.initial_state()?;
    let initialized = std::rc::Rc::new(std::cell::Cell::new(false));
    let hold_guard = std::rc::Rc::new(std::cell::RefCell::new(None::<gio::ApplicationHoldGuard>));
    let app = adw::Application::builder()
        .application_id("org.oswitch.tray")
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
    let tray = OpenSwitcherTray::new(client.clone(), initial_state, command_tx);
    let service = ksni::TrayService::new(tray);
    let handle = service.handle();
    service.spawn();

    client.spawn_listener(state_tx);

    std::thread::spawn(move || {
        for state in state_rx {
            handle.update(|tray| {
                tray.state = state;
            });
        }
    });
}
