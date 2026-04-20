pub mod capture;
pub mod input_backend;
pub mod keyboard;
pub mod layout_switcher;
pub mod runtime;
pub mod selected_text;
pub mod service;
pub mod switch_logic;

use crate::config::default_config_path;
use crate::dbus::{OpenSwitcherDbusApi, OBJECT_PATH, SERVICE_NAME};
use crate::error::SwitcherError;
use crate::system::is_dev_runtime_mode;
use keyboard::log_input_debug;
use runtime::{log_layout_debug, BackendSyncResult, ConfigService, RuntimeState};
use service::DaemonService;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;
use zbus::blocking::ConnectionBuilder;

const TRAY_SERVICE_NAME: &str = "org.oswitch.tray";

struct SessionBusTrayPresenceProbe {
    connection: Connection,
}

impl runtime::TrayPresenceProbe for SessionBusTrayPresenceProbe {
    fn tray_is_present(&self) -> Result<bool, std::io::Error> {
        let proxy = DBusProxy::new(&self.connection).map_err(std::io::Error::other)?;
        let name = TRAY_SERVICE_NAME.try_into().unwrap();
        proxy.name_has_owner(name).map_err(std::io::Error::other)
    }
}

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
    match runtime.sync_with_backend() {
        BackendSyncResult::Updated { current, .. } => {
            log_layout_debug(
                "startup-sync",
                &format!("source=backend current={current:?}"),
            );
        }
        BackendSyncResult::Unchanged => {
            log_layout_debug("startup-sync", "source=backend unchanged=true");
        }
        BackendSyncResult::Skipped => {
            log_layout_debug("startup-sync", "source=backend skipped=true");
        }
    }
    runtime.start_background_sync_polling();
    if !is_dev_runtime_mode() {
        let tray_probe = SessionBusTrayPresenceProbe {
            connection: Connection::session()?,
        };
        runtime.start_tray_watchdog(tray_probe);
    } else {
        log_layout_debug(
            "tray-watchdog-start",
            "enabled=false reason=dev-runtime-mode",
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
