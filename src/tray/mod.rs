pub mod dbus_listener;
pub mod tray_service;

use crate::error::SwitcherError;
use dbus_listener::DbusListener;
use tray_service::OpenSwitcherTray;

pub use tray_service::TrayState;
pub fn run() -> Result<(), SwitcherError> {
    let client = DbusListener::new()?;
    let initial_state = client.initial_state()?;
    let (tx, rx) = std::sync::mpsc::channel();

    let tray = OpenSwitcherTray::new(client.clone(), initial_state);
    let service = ksni::TrayService::new(tray);
    let handle = service.handle();
    service.spawn();

    client.spawn_listener(tx);

    for state in rx {
        handle.update(|tray| {
            tray.state = state;
        });
    }

    Ok(())
}
