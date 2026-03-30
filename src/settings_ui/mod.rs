mod dbus_client;
pub(crate) mod presenter;
mod state;
mod ui;

pub(crate) use ui::SettingsWindowController;

pub fn run() {
    ui::run_standalone();
}
