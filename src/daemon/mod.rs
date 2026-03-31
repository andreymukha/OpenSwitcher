pub mod capture;
pub mod keyboard;
pub mod runtime;
pub mod selected_text;
pub mod service;
pub mod switch_logic;

use crate::config::default_config_path;
use crate::dbus::{OpenSwitcherDbusApi, OBJECT_PATH, SERVICE_NAME};
use crate::error::SwitcherError;
use runtime::{ConfigService, RuntimeState};
use service::DaemonService;
use std::sync::Arc;
use zbus::blocking::ConnectionBuilder;

pub fn run() -> Result<(), SwitcherError> {
    let config_service = ConfigService::load(default_config_path())?;
    let runtime = Arc::new(RuntimeState::new(config_service));
    let dbus_api = OpenSwitcherDbusApi::new(runtime.clone());

    let connection = ConnectionBuilder::session()?
        .name(SERVICE_NAME)?
        .serve_at(OBJECT_PATH, dbus_api)?
        .build()?;

    let mut service = DaemonService::new(runtime, connection)?;
    service.run()
}
