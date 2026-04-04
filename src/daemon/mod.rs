pub mod capture;
pub mod keyboard;
pub mod layout_switcher;
pub mod runtime;
pub mod selected_text;
pub mod service;
pub mod switch_logic;

use crate::config::default_config_path;
use crate::dbus::{OpenSwitcherDbusApi, OBJECT_PATH, SERVICE_NAME};
use crate::error::SwitcherError;
use keyboard::{is_russian_layout, log_input_debug};
use runtime::{log_layout_debug, ConfigService, RuntimeState};
use service::DaemonService;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use zbus::blocking::ConnectionBuilder;

pub fn run() -> Result<(), SwitcherError> {
    let config_service = ConfigService::load(default_config_path())?;
    let runtime = Arc::new(RuntimeState::new(config_service));
    match runtime.config_snapshot() {
        Ok(snapshot) => log_layout_debug(
            "startup-config",
            &format!("layout_switch_combo={:?}", snapshot.layout_switch_combo),
        ),
        Err(error) => log_layout_debug(
            "startup-config",
            &format!("layout_switch_combo=unavailable error={error}"),
        ),
    }
    if let Ok(is_russian) = is_russian_layout() {
        log_layout_debug(
            "startup-sync",
            &format!("source=xset is_russian={is_russian}"),
        );
        runtime.set_layout_with_reason(!is_russian, "startup-xset-sync");
    } else {
        log_layout_debug("startup-sync", "source=xset failed=true");
        eprintln!(
            "[layout] Не удалось определить текущую раскладку на старте. Использую cached default."
        );
    }
    let dbus_api = OpenSwitcherDbusApi::new(runtime.clone());

    let connection = ConnectionBuilder::session()?
        .name(SERVICE_NAME)?
        .serve_at(OBJECT_PATH, dbus_api)?
        .build()?;

    let mut service = DaemonService::new(runtime, connection)?;
    match panic::catch_unwind(AssertUnwindSafe(|| service.run())) {
        Ok(result) => result,
        Err(payload) => {
            let reason = if let Some(text) = payload.downcast_ref::<&str>() {
                *text
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.as_str()
            } else {
                "unknown panic payload"
            };
            log_input_debug("event-loop-panic", &format!("reason={reason}"));
            eprintln!("[input] Демон аварийно завершился в input loop: {reason}");
            service.shutdown();
            Err(SwitcherError::DaemonPanicked)
        }
    }
}
