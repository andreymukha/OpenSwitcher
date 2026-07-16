use crate::daemon::capture::CaptureOwner;
use crate::daemon::runtime::log_layout_debug;
use crate::daemon::runtime::RuntimeState;
use crate::error::{DbusError, SettingsError, SwitcherError};
use crate::model::{LayoutSwitchCaptureState, Settings, SettingsDto, UpdateSettingsResult};
use futures_util::{
    future::{select, Either},
    pin_mut, StreamExt,
};
use std::sync::{
    mpsc::{self, SyncSender},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use zbus::blocking::Connection;
use zbus::{dbus_interface, dbus_proxy, fdo, AsyncDrop, MessageHeader, SignalContext};

pub const SERVICE_NAME: &str = "org.oswitch.core";
pub const OBJECT_PATH: &str = "/org/oswitch/core";
pub const INTERFACE_NAME: &str = "org.oswitch.core";
pub(crate) const DBUS_SIGNAL_QUEUE_CAPACITY: usize = 16;

pub(crate) struct CaptureOwnerMonitor {
    stop_sender: async_channel::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl CaptureOwnerMonitor {
    pub(crate) fn start(
        connection: &Connection,
        runtime: Arc<RuntimeState>,
    ) -> Result<Self, SwitcherError> {
        let connection = connection.inner().clone();
        Self::spawn_worker(move |stop_receiver, ready_sender| {
            async_io::block_on(run_capture_owner_monitor(
                connection,
                runtime,
                stop_receiver,
                ready_sender,
            ));
        })
    }

    fn spawn_worker<F>(worker_main: F) -> Result<Self, SwitcherError>
    where
        F: FnOnce(async_channel::Receiver<()>, mpsc::SyncSender<zbus::Result<()>>) + Send + 'static,
    {
        let (stop_sender, stop_receiver) = async_channel::bounded(1);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("openswitcher-capture-owner".to_owned())
            .spawn(move || worker_main(stop_receiver, ready_sender))?;

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                stop_sender,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error.into())
            }
            Err(error) => {
                let _ = worker.join();
                Err(std::io::Error::other(format!(
                    "capture owner monitor failed before subscription was ready: {error}"
                ))
                .into())
            }
        }
    }

    pub(crate) fn stop(&mut self) -> thread::Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> thread::Result<()> {
        let _ = self.stop_sender.try_send(());
        match self.worker.take() {
            Some(worker) => worker.join(),
            None => Ok(()),
        }
    }
}

impl Drop for CaptureOwnerMonitor {
    fn drop(&mut self) {
        if self.stop_and_join().is_err() {
            log_layout_debug(
                "dbus-capture-owner-monitor-stop-error",
                "worker_panicked=true",
            );
            eprintln!("[dbus] Capture owner monitor worker panicked");
        }
    }
}

async fn run_capture_owner_monitor(
    connection: zbus::Connection,
    runtime: Arc<RuntimeState>,
    stop_receiver: async_channel::Receiver<()>,
    ready_sender: mpsc::SyncSender<zbus::Result<()>>,
) {
    let proxy = match zbus::fdo::DBusProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            let _ = ready_sender.send(Err(error));
            return;
        }
    };
    let mut owner_changes = match proxy.receive_name_owner_changed().await {
        Ok(owner_changes) => owner_changes,
        Err(error) => {
            let _ = ready_sender.send(Err(error));
            return;
        }
    };
    if ready_sender.send(Ok(())).is_err() {
        return;
    }

    loop {
        let stop = stop_receiver.recv();
        let owner_change = owner_changes.next();
        pin_mut!(stop, owner_change);
        match select(stop, owner_change).await {
            Either::Left((_stop, _pending_owner_change)) => break,
            Either::Right((signal, _pending_stop)) => {
                let Some(signal) = signal else {
                    log_layout_debug(
                        "dbus-capture-owner-monitor-ended",
                        "soft_lease_fallback=true",
                    );
                    eprintln!(
                        "[dbus] Capture owner monitor ended; the bounded lease remains active"
                    );
                    break;
                };
                let args = match signal.args() {
                    Ok(args) => args,
                    Err(error) => {
                        log_layout_debug(
                            "dbus-capture-owner-monitor-signal-error",
                            &format!("error={error}"),
                        );
                        eprintln!("[dbus] Failed to read NameOwnerChanged signal: {error}");
                        continue;
                    }
                };
                if args.new_owner().as_ref().is_some() {
                    continue;
                }
                let owner = CaptureOwner::from(args.name().as_str());
                drop(args);

                match runtime.layout_switch_capture_owner_disappeared_at(&owner, Instant::now()) {
                    Ok(Some(state)) => {
                        emit_capture_state_changed_async_best_effort(&connection, &state).await;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        log_layout_debug(
                            "dbus-capture-owner-monitor-runtime-error",
                            &format!("error={error} soft_lease_fallback=true"),
                        );
                        eprintln!("[dbus] Failed to cancel capture for a vanished owner: {error}");
                    }
                }
            }
        }
    }
    owner_changes.async_drop().await;
}

async fn emit_capture_state_changed_async_best_effort(
    connection: &zbus::Connection,
    state: &LayoutSwitchCaptureState,
) {
    let result = match SignalContext::new(connection, OBJECT_PATH) {
        Ok(context) => {
            OpenSwitcherDbusApi::layout_switch_capture_state_changed(&context, state.clone()).await
        }
        Err(error) => Err(error),
    };
    capture_signal_best_effort(result);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DbusSignalEvent {
    StatusChanged { enabled: bool, layout: bool },
    LayoutSwitchCaptureStateChanged(LayoutSwitchCaptureState),
}

// Best-effort, non-blocking signal publisher for paths that may run while
// the real keyboard is grabbed. The worker exits when all senders are dropped.
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
                            eprintln!("[dbus] Failed to emit queued StatusChanged signal: {error}");
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
    fn renew_layout_switch_capture(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn cancel_layout_switch_capture(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn finish_layout_switch_capture(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn get_layout_switch_capture_state(&self) -> zbus::Result<LayoutSwitchCaptureState>;
    fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> zbus::Result<()>;
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
        #[zbus(header)] header: MessageHeader<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let owner = capture_owner_from_header(&header)?;
        let state = self
            .runtime
            .start_layout_switch_capture_owned_at(owner, Instant::now())
            .map_err(DbusError::from)?;
        capture_signal_best_effort(zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        )));
        Ok(state)
    }

    pub fn renew_layout_switch_capture(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(header)] header: MessageHeader<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let owner = capture_owner_from_header(&header)?;
        let state = self
            .runtime
            .renew_layout_switch_capture_owned_at(&owner, Instant::now())
            .map_err(DbusError::from)?;
        capture_signal_best_effort(zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        )));
        Ok(state)
    }

    pub fn cancel_layout_switch_capture(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(header)] header: MessageHeader<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let owner = capture_owner_from_header(&header)?;
        let state = self
            .runtime
            .cancel_layout_switch_capture_owned_at(&owner, Instant::now())
            .map_err(DbusError::from)?;
        capture_signal_best_effort(zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        )));
        Ok(state)
    }

    pub fn finish_layout_switch_capture(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        #[zbus(header)] header: MessageHeader<'_>,
    ) -> fdo::Result<LayoutSwitchCaptureState> {
        let owner = capture_owner_from_header(&header)?;
        let state = self
            .runtime
            .finish_layout_switch_capture_owned_at(&owner, Instant::now())
            .map_err(DbusError::from)?;
        capture_signal_best_effort(zbus::block_on(Self::layout_switch_capture_state_changed(
            &ctxt,
            state.clone(),
        )));
        Ok(state)
    }

    pub fn get_layout_switch_capture_state(&self) -> fdo::Result<LayoutSwitchCaptureState> {
        self.runtime
            .layout_switch_capture_state()
            .map_err(|err| DbusError::from(err).into())
    }

    pub fn set_hotkey_capture_inhibited(&self, inhibited: bool) {
        self.runtime
            .set_settings_hotkey_capture_inhibited(inhibited);
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

fn capture_owner_from_header(header: &MessageHeader<'_>) -> fdo::Result<CaptureOwner> {
    let sender = header
        .sender()
        .map_err(|_| fdo::Error::Failed("D-Bus caller identity is unavailable".to_owned()))?
        .ok_or_else(|| fdo::Error::Failed("D-Bus caller identity is unavailable".to_owned()))?;
    let sender = sender.as_str();
    if sender.is_empty() {
        return Err(fdo::Error::Failed(
            "D-Bus caller identity is unavailable".to_owned(),
        ));
    }

    Ok(CaptureOwner::from(sender))
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

fn capture_signal_best_effort<E: std::fmt::Display>(result: Result<(), E>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            log_layout_debug("dbus-capture-signal-error", &format!("error={error}"));
            eprintln!("[dbus] Failed to emit LayoutSwitchCaptureStateChanged signal: {error}");
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
    use crate::daemon::runtime::ConfigService;
    use crate::model::LayoutSwitchCapturePhase;
    use std::error::Error;
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;
    use zbus::blocking::{ConnectionBuilder, Proxy};

    #[test]
    fn status_signal_best_effort_treats_success_as_ok() {
        assert!(status_signal_best_effort(
            "test-success",
            Ok::<(), &str>(())
        ));
    }

    #[test]
    fn status_signal_best_effort_treats_failure_as_non_fatal() {
        assert!(!status_signal_best_effort(
            "test-failure",
            Err::<(), &str>("signal failed")
        ));
    }

    #[test]
    fn capture_signal_best_effort_treats_success_as_ok() {
        assert!(capture_signal_best_effort(Ok::<(), &str>(())))
    }

    #[test]
    fn capture_signal_best_effort_treats_failure_as_non_fatal() {
        assert!(!capture_signal_best_effort(Err::<(), &str>(
            "signal failed"
        )))
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

    #[test]
    fn capture_owner_monitor_cancels_when_starting_connection_disappears(
    ) -> Result<(), Box<dyn Error>> {
        let temp_dir = TempDir::new()?;
        let runtime = test_runtime(temp_dir.path())?;
        let service_name = unique_test_service_name("owner_loss");
        let service = ConnectionBuilder::session()?
            .name(service_name.as_str())?
            .build()?;
        let mut monitor = CaptureOwnerMonitor::start(&service, runtime.clone())?;
        service
            .object_server()
            .at(OBJECT_PATH, OpenSwitcherDbusApi::new(runtime.clone()))?;

        {
            let owner_connection = Connection::session()?;
            let owner_proxy = Proxy::new(
                &owner_connection,
                service_name.as_str(),
                OBJECT_PATH,
                INTERFACE_NAME,
            )?;
            let started: LayoutSwitchCaptureState =
                owner_proxy.call("StartLayoutSwitchCapture", &())?;
            assert_eq!(started.phase, LayoutSwitchCapturePhase::Waiting);
        }

        wait_for_capture_phase(&runtime, LayoutSwitchCapturePhase::Cancelled)?;
        assert!(monitor.stop().is_ok(), "monitor worker must join cleanly");

        Ok(())
    }

    #[test]
    fn capture_owner_monitor_ignores_an_unrelated_connection_disappearing(
    ) -> Result<(), Box<dyn Error>> {
        let temp_dir = TempDir::new()?;
        let runtime = test_runtime(temp_dir.path())?;
        let service_name = unique_test_service_name("unrelated_owner_loss");
        let service = ConnectionBuilder::session()?
            .name(service_name.as_str())?
            .build()?;
        let mut monitor = CaptureOwnerMonitor::start(&service, runtime.clone())?;
        service
            .object_server()
            .at(OBJECT_PATH, OpenSwitcherDbusApi::new(runtime.clone()))?;
        let owner_connection = Connection::session()?;
        let owner_proxy = Proxy::new(
            &owner_connection,
            service_name.as_str(),
            OBJECT_PATH,
            INTERFACE_NAME,
        )?;
        let started: LayoutSwitchCaptureState =
            owner_proxy.call("StartLayoutSwitchCapture", &())?;
        assert_eq!(started.phase, LayoutSwitchCapturePhase::Waiting);

        let unrelated_connection = Connection::session()?;
        assert_ne!(
            owner_connection.unique_name(),
            unrelated_connection.unique_name()
        );
        drop(unrelated_connection);
        std::thread::sleep(Duration::from_millis(100));

        assert_eq!(
            runtime.layout_switch_capture_state()?.phase,
            LayoutSwitchCapturePhase::Waiting
        );
        let cancelled: LayoutSwitchCaptureState =
            owner_proxy.call("CancelLayoutSwitchCapture", &())?;
        assert_eq!(cancelled.phase, LayoutSwitchCapturePhase::Cancelled);
        assert!(monitor.stop().is_ok(), "monitor worker must join cleanly");

        Ok(())
    }

    #[test]
    fn capture_owner_monitor_stop_is_idempotent_and_joins_cleanly() -> Result<(), Box<dyn Error>> {
        let temp_dir = TempDir::new()?;
        let runtime = test_runtime(temp_dir.path())?;
        let connection = Connection::session()?;
        let mut monitor = CaptureOwnerMonitor::start(&connection, runtime)?;

        assert!(monitor.stop().is_ok());
        assert!(monitor.stop().is_ok());

        Ok(())
    }

    #[test]
    fn capture_owner_monitor_propagates_subscription_startup_failure() {
        let result = CaptureOwnerMonitor::spawn_worker(|_stop_receiver, ready_sender| {
            ready_sender
                .send(Err(zbus::Error::Failure(
                    "injected subscription failure".to_owned(),
                )))
                .expect("startup result receiver must still be present");
        });

        match result {
            Err(SwitcherError::Dbus(zbus::Error::Failure(message))) => {
                assert_eq!(message, "injected subscription failure");
            }
            Err(error) => panic!("unexpected startup error: {error}"),
            Ok(_) => panic!("subscription failure must prevent monitor startup"),
        }
    }

    fn test_runtime(temp_dir: &Path) -> Result<Arc<RuntimeState>, Box<dyn Error>> {
        Ok(Arc::new(RuntimeState::new(ConfigService::load(
            temp_dir.join("config.toml"),
        )?)))
    }

    fn unique_test_service_name(suffix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        format!(
            "org.oswitch.core.monitor_test.{suffix}.p{}.n{nanos}",
            std::process::id()
        )
    }

    fn wait_for_capture_phase(
        runtime: &RuntimeState,
        expected: LayoutSwitchCapturePhase,
    ) -> Result<(), Box<dyn Error>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let state = runtime.layout_switch_capture_state()?;
            if state.phase == expected {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for capture phase {expected:?}; current={:?}",
                    state.phase
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
