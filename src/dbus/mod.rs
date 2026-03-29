use crate::daemon::runtime::RuntimeState;
use crate::error::{DbusError, SettingsError};
use crate::model::{Settings, SettingsDto, UpdateSettingsResult};
use std::sync::Arc;
use zbus::blocking::Connection;
use zbus::{dbus_interface, dbus_proxy, fdo, SignalContext};

pub const SERVICE_NAME: &str = "org.oswitch.core";
pub const OBJECT_PATH: &str = "/org/oswitch/core";
pub const INTERFACE_NAME: &str = "org.oswitch.core";

#[dbus_proxy(
    interface = "org.oswitch.core",
    default_service = "org.oswitch.core",
    default_path = "/org/oswitch/core"
)]
pub trait OpenSwitcher {
    fn toggle(&self) -> zbus::Result<()>;
    fn get_settings(&self) -> zbus::Result<SettingsDto>;
    fn update_settings(&self, settings: SettingsDto) -> zbus::Result<UpdateSettingsResult>;
    #[dbus_proxy(property)]
    fn is_enabled(&self) -> zbus::Result<bool>;
    #[dbus_proxy(property)]
    fn current_layout(&self) -> zbus::Result<bool>;
    #[dbus_proxy(signal)]
    fn status_changed(&self, enabled: bool, layout: bool) -> zbus::Result<()>;
}

pub struct OpenSwitcherDbusApi {
    runtime: Arc<RuntimeState>,
}

impl OpenSwitcherDbusApi {
    pub fn new(runtime: Arc<RuntimeState>) -> Self {
        Self { runtime }
    }
}

#[dbus_interface(name = "org.oswitch.core")]
impl OpenSwitcherDbusApi {
    pub fn toggle(&self, #[zbus(signal_context)] ctxt: SignalContext<'_>) -> fdo::Result<()> {
        let enabled = self.runtime.toggle_enabled();
        let layout = self.runtime.current_layout();
        zbus::block_on(Self::status_changed(&ctxt, enabled, layout))
            .map_err(|err| DbusError::Signal(err).into())
    }

    pub fn get_settings(&self) -> fdo::Result<SettingsDto> {
        self.runtime
            .get_settings()
            .map(SettingsDto::from)
            .map_err(|err| DbusError::from(err).into())
    }

    pub fn update_settings(&self, settings: SettingsDto) -> fdo::Result<UpdateSettingsResult> {
        let settings = Settings::try_from(settings)
            .map_err(|err| fdo::Error::from(DbusError::from(SettingsError::from(err))))?;
        self.runtime
            .update_settings(settings)
            .map_err(|err| DbusError::from(err).into())
    }

    #[dbus_interface(property)]
    pub fn is_enabled(&self) -> bool {
        self.runtime.is_enabled()
    }

    #[dbus_interface(property)]
    pub fn current_layout(&self) -> bool {
        self.runtime.current_layout()
    }

    #[dbus_interface(signal)]
    pub async fn status_changed(
        ctxt: &SignalContext<'_>,
        enabled: bool,
        layout: bool,
    ) -> zbus::Result<()>;
}

pub fn emit_status_changed(
    connection: &Connection,
    runtime: &RuntimeState,
) -> Result<(), DbusError> {
    let ctxt = SignalContext::new(connection.inner(), OBJECT_PATH).map_err(DbusError::Signal)?;
    zbus::block_on(OpenSwitcherDbusApi::status_changed(
        &ctxt,
        runtime.is_enabled(),
        runtime.current_layout(),
    ))
    .map_err(DbusError::Signal)
}
