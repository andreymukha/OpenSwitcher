use crate::daemon::runtime::log_layout_debug;
use crate::daemon::runtime::RuntimeState;
use crate::error::{DbusError, SettingsError};
use crate::model::{LayoutSwitchCaptureState, Settings, SettingsDto, UpdateSettingsResult};
use std::sync::{
    mpsc::{self, SyncSender},
    Arc,
};
use std::thread;
use zbus::blocking::Connection;
use zbus::{dbus_interface, dbus_proxy, fdo, SignalContext};

pub const SERVICE_NAME: &str = "org.oswitch.core";
pub const OBJECT_PATH: &str = "/org/oswitch/core";
pub const INTERFACE_NAME: &str = "org.oswitch.core";
pub(crate) const DBUS_SIGNAL_QUEUE_CAPACITY: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DbusSignalEvent {
    StatusChanged { enabled: bool, layout: bool },
    LayoutSwitchCaptureStateChanged(LayoutSwitchCaptureState),
}

#[derive(Clone)]
pub(crate) struct DbusSignalPublisher {
    sender: SyncSender<DbusSignalEvent>,
}

impl DbusSignalPublisher {
    pub(crate) fn spawn(connection: Connection) -> Self {
        let (sender, receiver) = mpsc::sync_channel(DBUS_SIGNAL_QUEUE_CAPACITY);
        let _ = thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    DbusSignalEvent::StatusChanged { enabled, layout } => {
                        if let Err(error) =
                            emit_status_changed_payload(&connection, enabled, layout)
                        {
                            log_layout_debug(
                                "dbus-publisher-status-error",
                                &format!("error={error}"),
                            );
                            eprintln!(
                                "[dbus] Failed to emit queued StatusChanged signal: {error}"
                            );
                        }
                    }
                    DbusSignalEvent::LayoutSwitchCaptureStateChanged(state) => {
                        if let Err(error) =
                            emit_layout_switch_capture_state_changed(&connection, &state)
                        {
                            log_layout_debug(
                                "dbus-publisher-capture-error",
                                &format!("error={error}"),
                            );
                            eprintln!(
                                "[dbus] Failed to emit queued LayoutSwitchCaptureStateChanged signal: {error}"
                            );
                        }
                    }
                }
            }
        });

        Self { sender }
    }

    pub(crate) fn try_publish(&self, event: DbusSignalEvent) -> bool {
        self.sender.try_send(event).is_ok()
    }

    #[cfg(test)]
    fn from_sender(sender: SyncSender<DbusSignalEvent>) -> Self {
        Self { sender }
    }
}

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
        emit_status_changed_from_context_best_effort("toggle", &ctxt, enabled, layout);
        Ok(())
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
            emit_status_changed_from_context_best_effort(
                "update-settings",
                &ctxt,
                self.runtime.is_enabled(),
                self.runtime.current_layout(),
            );
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

fn emit_status_changed_from_context_best_effort(
    context: &str,
    ctxt: &SignalContext<'_>,
    enabled: bool,
    layout: bool,
) {
    status_signal_best_effort(
        context,
        zbus::block_on(OpenSwitcherDbusApi::status_changed(ctxt, enabled, layout))
            .map_err(DbusError::Signal),
    );
}

pub fn emit_status_changed(
    connection: &Connection,
    runtime: &RuntimeState,
) -> Result<(), DbusError> {
    let enabled = runtime.is_enabled();
    let layout = runtime.current_layout();
    log_layout_debug(
        "dbus-emit-status",
        &format!(
            "enabled={} current_layout={}",
            enabled,
            if layout { "EN" } else { "RU" }
        ),
    );
    emit_status_changed_payload(connection, enabled, layout)
}

fn emit_status_changed_payload(
    connection: &Connection,
    enabled: bool,
    layout: bool,
) -> Result<(), DbusError> {
    let ctxt = SignalContext::new(connection.inner(), OBJECT_PATH).map_err(DbusError::Signal)?;
    zbus::block_on(OpenSwitcherDbusApi::status_changed(&ctxt, enabled, layout))
        .map_err(DbusError::Signal)
}

pub fn emit_status_changed_best_effort(
    connection: &Connection,
    runtime: &RuntimeState,
    context: &str,
) {
    status_signal_best_effort(context, emit_status_changed(connection, runtime));
}

fn status_signal_best_effort<E: std::fmt::Display>(context: &str, result: Result<(), E>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            log_layout_debug(
                "dbus-status-signal-error",
                &format!("context={context} error={error}"),
            );
            eprintln!("[dbus] Failed to emit StatusChanged signal ({context}): {error}");
            false
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn status_signal_best_effort_treats_success_as_ok() {
        assert!(status_signal_best_effort("test-success", Ok::<(), &str>(())));
    }

    #[test]
    fn status_signal_best_effort_treats_failure_as_non_fatal() {
        assert!(!status_signal_best_effort(
            "test-failure",
            Err::<(), &str>("signal failed")
        ));
    }

    #[test]
    fn publisher_enqueue_success_sends_exact_status_payload() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let publisher = DbusSignalPublisher::from_sender(sender);
        let event = DbusSignalEvent::StatusChanged {
            enabled: true,
            layout: false,
        };

        assert!(publisher.try_publish(event.clone()));
        assert_eq!(receiver.try_recv(), Ok(event));
    }

    #[test]
    fn publisher_enqueue_success_sends_exact_capture_payload() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let publisher = DbusSignalPublisher::from_sender(sender);
        let state =
            LayoutSwitchCaptureState::candidate(crate::model::LayoutSwitchCombo::alt_shift());
        let event = DbusSignalEvent::LayoutSwitchCaptureStateChanged(state);

        assert!(publisher.try_publish(event.clone()));
        assert_eq!(receiver.try_recv(), Ok(event));
    }

    #[test]
    fn publisher_try_publish_returns_false_when_queue_is_full() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let publisher = DbusSignalPublisher::from_sender(sender);

        assert!(publisher.try_publish(DbusSignalEvent::StatusChanged {
            enabled: true,
            layout: true,
        }));
        assert!(!publisher.try_publish(DbusSignalEvent::StatusChanged {
            enabled: false,
            layout: false,
        }));
    }

    #[test]
    fn publisher_try_publish_returns_false_when_receiver_is_dropped() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let publisher = DbusSignalPublisher::from_sender(sender);

        assert!(!publisher.try_publish(DbusSignalEvent::StatusChanged {
            enabled: true,
            layout: true,
        }));
    }
}
