use crate::daemon::runtime::log_layout_debug;
use crate::daemon::runtime::RuntimeState;
use crate::error::{DbusError, SettingsError};
use crate::model::{LayoutSwitchCaptureState, Settings, SettingsDto, UpdateSettingsResult};
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
    fn request_exit(&self) -> zbus::Result<()>;
    fn get_settings(&self) -> zbus::Result<SettingsDto>;
    fn update_settings(&self, settings: SettingsDto) -> zbus::Result<UpdateSettingsResult>;
    fn start_layout_switch_capture(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn cancel_layout_switch_capture(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn finish_layout_switch_capture(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn get_layout_switch_capture_state(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    #[dbus_proxy(property)]
    fn is_enabled(&self) -> zbus::Result<bool>;
    #[dbus_proxy(property)]
    fn current_layout(&self) -> zbus::Result<bool>;
    #[dbus_proxy(signal)]
    fn status_changed(&self, enabled: bool, layout: bool) -> zbus::Result<()>;
    #[dbus_proxy(signal)]
    fn layout_switch_capture_state_changed(
        &self,
        state: LayoutSwitchCaptureState,
    ) -> zbus::Result<()>;
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
        let enabled = self
            .runtime
            .toggle_enabled_result()
            .map_err(|err| fdo::Error::from(DbusError::from(err)))?;
        let layout = self.runtime.current_layout();
        zbus::block_on(Self::status_changed(&ctxt, enabled, layout))
            .map_err(|err| fdo::Error::from(DbusError::Signal(err)))
    }

    pub fn request_exit(&self) {
        self.runtime.request_exit();
    }

    pub fn get_settings(&self) -> fdo::Result<SettingsDto> {
        self.runtime
            .get_settings()
            .map(SettingsDto::from)
            .map_err(|err| fdo::Error::from(DbusError::from(err)))
    }

    pub fn update_settings(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        settings: SettingsDto,
    ) -> fdo::Result<UpdateSettingsResult> {
        let settings = Settings::try_from(settings)
            .map_err(|err| fdo::Error::from(DbusError::from(SettingsError::from(err))))?;
        let enabled_before = self.runtime.is_enabled();
        let result = self
            .runtime
            .update_settings(settings)
            .map_err(|err| fdo::Error::from(DbusError::from(err)))?;

        if self.runtime.is_enabled() != enabled_before {
            zbus::block_on(Self::status_changed(
                &ctxt,
                self.runtime.is_enabled(),
                self.runtime.current_layout(),
            ))
            .map_err(|err| fdo::Error::from(DbusError::Signal(err)))?;
        }

        Ok(result)
    }

    pub fn start_layout_switch_capture(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let state = self
            .runtime
            .start_layout_switch_capture()
            .map_err(DbusError::from)?;
        zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        ))
        .map_err(|err| fdo::Error::from(DbusError::Signal(err)))?;
        Ok(state)
    }

    pub fn cancel_layout_switch_capture(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let state = self
            .runtime
            .cancel_layout_switch_capture()
            .map_err(DbusError::from)?;
        zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        ))
        .map_err(|err| fdo::Error::from(DbusError::Signal(err)))?;
        Ok(state)
    }

    pub fn finish_layout_switch_capture(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let state = self
            .runtime
            .finish_layout_switch_capture()
            .map_err(DbusError::from)?;
        zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        ))
        .map_err(|err| fdo::Error::from(DbusError::Signal(err)))?;
        Ok(state)
    }

    pub fn get_layout_switch_capture_state(&self) -> fdo::Result<LayoutSwitchCaptureState> {
        self.runtime
            .layout_switch_capture_state()
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

    #[dbus_interface(signal)]
    pub async fn layout_switch_capture_state_changed(
        ctxt: &SignalContext<'_>,
        state: LayoutSwitchCaptureState,
    ) -> zbus::Result<()>;
}

pub fn emit_status_changed(
    connection: &Connection,
    runtime: &RuntimeState,
) -> Result<(), DbusError> {
    log_layout_debug(
        "dbus-emit-status",
        &format!(
            "enabled={} current_layout={}",
            runtime.is_enabled(),
            if runtime.current_layout() { "EN" } else { "RU" }
        ),
    );
    let ctxt = SignalContext::new(connection.inner(), OBJECT_PATH).map_err(DbusError::Signal)?;
    zbus::block_on(OpenSwitcherDbusApi::status_changed(
        &ctxt,
        runtime.is_enabled(),
        runtime.current_layout(),
    ))
    .map_err(DbusError::Signal)
}

pub fn emit_layout_switch_capture_state_changed(
    connection: &Connection,
    state: &LayoutSwitchCaptureState,
) -> Result<(), DbusError> {
    let ctxt = SignalContext::new(connection.inner(), OBJECT_PATH).map_err(DbusError::Signal)?;
    zbus::block_on(OpenSwitcherDbusApi::layout_switch_capture_state_changed(
        &ctxt,
        state.clone(),
    ))
    .map_err(DbusError::Signal)
}
