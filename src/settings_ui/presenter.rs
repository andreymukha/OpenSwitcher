use super::dbus_client::SettingsDbusClient;
use super::state::{DomainState, ViewState};
use crate::error::{SettingsClientError, UiError};
use crate::model::{HotkeySpec, LayoutSwitchCaptureState, LayoutSwitchCombo, UpdateSettingsResult};
use crate::system::user_services::{CommandRunner, ProcessCommandRunner};
use crate::system::UserServiceController;
use async_channel::Sender;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

pub(crate) const CAPTURE_HEARTBEAT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub enum PresenterEvent {
    ViewStateChanged(ViewState),
    LoadFailed(SettingsClientError),
    SaveFailed(SettingsClientError),
    SaveSucceeded(UpdateSettingsResult),
    CaptureStateChanged {
        generation: u64,
        state: LayoutSwitchCaptureState,
    },
    CaptureRenewFailed {
        generation: u64,
        error: SettingsClientError,
    },
    AutostartFailed(SettingsClientError),
}

pub enum SaveRequest {
    Ignored,
    Accepted(ViewState),
}

pub trait SettingsClientBackend: Clone + Send + Sync + 'static {
    fn load_settings(&self) -> Result<crate::model::Settings, SettingsClientError>;
    fn save_settings(
        &self,
        settings: crate::model::Settings,
    ) -> Result<UpdateSettingsResult, SettingsClientError>;
    fn start_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, SettingsClientError>;
    fn renew_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, SettingsClientError>;
    fn get_layout_switch_capture_state(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError>;
    fn cancel_layout_switch_capture(&self)
        -> Result<LayoutSwitchCaptureState, SettingsClientError>;
    fn finish_layout_switch_capture(&self)
        -> Result<LayoutSwitchCaptureState, SettingsClientError>;
    fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> Result<(), SettingsClientError>;
    fn spawn_capture_listener(&self, tx: Sender<LayoutSwitchCaptureState>);
}

impl SettingsClientBackend for SettingsDbusClient {
    fn load_settings(&self) -> Result<crate::model::Settings, SettingsClientError> {
        SettingsDbusClient::load_settings(self)
    }

    fn save_settings(
        &self,
        settings: crate::model::Settings,
    ) -> Result<UpdateSettingsResult, SettingsClientError> {
        SettingsDbusClient::save_settings(self, settings)
    }

    fn start_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        SettingsDbusClient::start_layout_switch_capture(self)
    }

    fn renew_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        SettingsDbusClient::renew_layout_switch_capture(self)
    }

    fn get_layout_switch_capture_state(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        SettingsDbusClient::get_layout_switch_capture_state(self)
    }

    fn cancel_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        SettingsDbusClient::cancel_layout_switch_capture(self)
    }

    fn finish_layout_switch_capture(
        &self,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        SettingsDbusClient::finish_layout_switch_capture(self)
    }

    fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> Result<(), SettingsClientError> {
        SettingsDbusClient::set_hotkey_capture_inhibited(self, inhibited)
    }

    fn spawn_capture_listener(&self, tx: Sender<LayoutSwitchCaptureState>) {
        SettingsDbusClient::spawn_capture_listener(self, tx);
    }
}

#[derive(Clone)]
pub struct SettingsPresenter<C = SettingsDbusClient, R = ProcessCommandRunner>
where
    C: SettingsClientBackend,
    R: CommandRunner,
{
    inner: Arc<PresenterInner<C, R>>,
}

struct PresenterInner<C, R>
where
    C: SettingsClientBackend,
    R: CommandRunner,
{
    client: C,
    services: UserServiceController<R>,
    state: Mutex<DomainState>,
    event_tx: Sender<PresenterEvent>,
    capture_generation: Arc<AtomicU64>,
    capture_heartbeat: Mutex<Option<CaptureHeartbeat>>,
    capture_heartbeat_interval: Duration,
}

struct CaptureHeartbeat {
    stop_tx: mpsc::Sender<()>,
}

impl SettingsPresenter<SettingsDbusClient, ProcessCommandRunner> {
    pub fn new(client: SettingsDbusClient, event_tx: Sender<PresenterEvent>) -> Self {
        Self::with_services(client, UserServiceController::from_system(), event_tx)
    }
}

impl<C, R> SettingsPresenter<C, R>
where
    C: SettingsClientBackend,
    R: CommandRunner + Send + Sync + 'static,
{
    pub fn with_services(
        client: C,
        services: UserServiceController<R>,
        event_tx: Sender<PresenterEvent>,
    ) -> Self {
        Self::with_services_and_heartbeat_interval(client, services, event_tx, CAPTURE_HEARTBEAT)
    }

    fn with_services_and_heartbeat_interval(
        client: C,
        services: UserServiceController<R>,
        event_tx: Sender<PresenterEvent>,
        capture_heartbeat_interval: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(PresenterInner {
                client,
                services,
                state: Mutex::new(DomainState::new()),
                event_tx,
                capture_generation: Arc::new(AtomicU64::new(0)),
                capture_heartbeat: Mutex::new(None),
                capture_heartbeat_interval,
            }),
        }
    }

    pub fn initialize(&self) {
        let (capture_tx, capture_rx) = async_channel::unbounded();
        self.inner.client.spawn_capture_listener(capture_tx);

        let presenter = Arc::downgrade(&self.inner);
        thread::spawn(move || {
            while let Ok(state) = capture_rx.recv_blocking() {
                let Some(inner) = presenter.upgrade() else {
                    break;
                };
                let presenter = SettingsPresenter { inner };
                presenter.observe_layout_switch_capture_state(state);
            }
        });

        self.reload();
    }

    pub fn reload(&self) {
        let should_load = self.with_state(DomainState::begin_loading);
        if !should_load {
            return;
        }

        let _ = self.emit_view_state();
        let presenter = self.clone();
        thread::spawn(move || match presenter.inner.client.load_settings() {
            Ok(settings) => {
                presenter.with_state(|state| state.apply_loaded(settings));
                match presenter.inner.services.is_autostart_enabled() {
                    Ok(enabled) => {
                        presenter.with_state(|state| {
                            state.apply_loaded_autostart(enabled);
                        });
                    }
                    Err(error) => {
                        let _ = presenter.send_event(PresenterEvent::AutostartFailed(
                            SettingsClientError::ServiceManager(error),
                        ));
                    }
                }
                let _ = presenter.emit_view_state();
            }
            Err(error) => {
                presenter.with_state(DomainState::finish_loading);
                let _ = presenter.emit_view_state();
                let _ = presenter.send_event(PresenterEvent::LoadFailed(error));
            }
        });
    }

    pub fn update_manual_correction_hotkey(&self, value: HotkeySpec) {
        let changed = self.with_state(|state| state.update_manual_correction_hotkey(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn update_layout_delay(&self, value: u32) {
        let changed = self.with_state(|state| state.update_layout_delay(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn update_selected_text_hotkey(&self, value: HotkeySpec) {
        let changed = self.with_state(|state| state.update_selected_text_hotkey(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn update_auto_switch_enabled(&self, value: bool) {
        let changed = self.with_state(|state| state.update_auto_switch_enabled(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn update_fix_two_capitals(&self, value: bool) {
        let changed = self.with_state(|state| state.update_fix_two_capitals(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn update_fix_accidental_caps_lock(&self, value: bool) {
        let changed = self.with_state(|state| state.update_fix_accidental_caps_lock(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn set_autostart_enabled(&self, enabled: bool) {
        let changed = self.with_state(|state| state.set_autostart_enabled(enabled));
        if !changed {
            return;
        }

        let _ = self.emit_view_state();
    }

    pub fn unlock_layout_switch_override(&self) {
        let changed = self.with_state(DomainState::unlock_layout_switch_override);
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn start_layout_switch_capture(&self) -> Result<(), SettingsClientError> {
        self.stop_capture_heartbeat();
        let changed = self.with_state(DomainState::start_layout_switch_capture);
        if changed {
            let _ = self.emit_view_state();
        }

        let result = self.inner.client.start_layout_switch_capture();
        match result {
            Ok(state) => {
                let generation = if state.is_active() {
                    let changed =
                        self.with_state(|current| current.set_layout_switch_capture_active(true));
                    if changed {
                        let _ = self.emit_view_state();
                    }
                    self.start_capture_heartbeat()
                } else {
                    let changed =
                        self.with_state(|current| current.set_layout_switch_capture_active(false));
                    if changed {
                        let _ = self.emit_view_state();
                    }
                    self.current_capture_generation()
                };
                let _ = self.send_capture_state_event(generation, state);
                Ok(())
            }
            Err(error) => {
                self.with_state(DomainState::cancel_layout_switch_capture);
                let _ = self.emit_view_state();
                Err(error)
            }
        }
    }

    pub fn cancel_layout_switch_capture(&self) -> Result<(), SettingsClientError> {
        self.stop_capture_heartbeat();
        let changed = self.with_state(DomainState::cancel_layout_switch_capture);
        if changed {
            let _ = self.emit_view_state();
        }

        match self.inner.client.cancel_layout_switch_capture() {
            Ok(state) => {
                let _ = self.send_capture_state_event(self.current_capture_generation(), state);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn confirm_captured_layout_switch(
        &self,
        combo: LayoutSwitchCombo,
    ) -> Result<(), SettingsClientError> {
        self.stop_capture_heartbeat();
        let state = match self.inner.client.finish_layout_switch_capture() {
            Ok(state) => state,
            Err(error) => {
                let changed = self.with_state(DomainState::cancel_layout_switch_capture);
                if changed {
                    let _ = self.emit_view_state();
                }
                return Err(error);
            }
        };
        let changed = self.with_state(|current| current.apply_captured_layout_switch(combo));
        if changed {
            let _ = self.emit_view_state();
        }
        let _ = self.send_capture_state_event(self.current_capture_generation(), state);
        Ok(())
    }

    pub fn discard_changes(&self) {
        self.stop_capture_heartbeat();
        let changed = self.with_state(DomainState::discard_changes);
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub(crate) fn apply_capture_state_event(
        &self,
        generation: u64,
        state: &LayoutSwitchCaptureState,
    ) -> bool {
        if self.current_capture_generation() != generation {
            return false;
        }
        if !state.is_active() {
            self.stop_capture_heartbeat();
            let changed =
                self.with_state(|current| current.set_layout_switch_capture_active(false));
            if changed {
                let _ = self.emit_view_state();
            }
        }
        true
    }

    pub(crate) fn apply_capture_renew_failure(&self, generation: u64) -> bool {
        if self.current_capture_generation() != generation {
            return false;
        }
        self.stop_capture_heartbeat();
        let changed = self.with_state(|current| current.set_layout_switch_capture_active(false));
        if changed {
            let _ = self.emit_view_state();
        }
        true
    }

    pub fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> Result<(), SettingsClientError> {
        self.inner.client.set_hotkey_capture_inhibited(inhibited)
    }

    pub fn save(&self) -> SaveRequest {
        let snapshot = self.with_state(|state| state.begin_save());
        let Some(snapshot) = snapshot else {
            return SaveRequest::Ignored;
        };

        let saving_view_state = self.with_state(|state| state.view_state());
        let _ = self.send_event(PresenterEvent::ViewStateChanged(saving_view_state.clone()));

        if let Err(error) = snapshot.settings.validate() {
            self.with_state(DomainState::save_failed);
            let reset_view_state = self.with_state(|state| state.view_state());
            let _ = self.send_event(PresenterEvent::ViewStateChanged(reset_view_state.clone()));
            let _ = self.send_event(PresenterEvent::SaveFailed(SettingsClientError::from(error)));
            return SaveRequest::Accepted(reset_view_state);
        }

        let presenter = self.clone();
        thread::spawn(
            move || match presenter.inner.client.save_settings(snapshot.settings) {
                Ok(result) => {
                    if let Some(enabled) = snapshot.autostart_change {
                        let service_result = if enabled {
                            presenter.inner.services.enable_autostart()
                        } else {
                            presenter.inner.services.disable_autostart()
                        };

                        if let Err(error) = service_result {
                            let actual_autostart =
                                presenter.inner.services.is_autostart_enabled().ok();
                            presenter.with_state(|state| {
                                state.save_persisted_settings_succeeded(snapshot.settings);
                                if let Some(actual_autostart) = actual_autostart {
                                    state.apply_loaded_autostart(actual_autostart);
                                }
                            });
                            presenter.with_state(DomainState::save_failed);
                            let _ = presenter.emit_view_state();
                            let _ = presenter.send_event(PresenterEvent::SaveFailed(
                                SettingsClientError::ServiceManager(error),
                            ));
                            return;
                        }
                    }

                    presenter.with_state(|state| state.save_succeeded(snapshot));
                    let _ = presenter.emit_view_state();
                    let _ = presenter.send_event(PresenterEvent::SaveSucceeded(result));
                }
                Err(error) => {
                    presenter.with_state(DomainState::save_failed);
                    let _ = presenter.emit_view_state();
                    let _ = presenter.send_event(PresenterEvent::SaveFailed(error));
                }
            },
        );

        SaveRequest::Accepted(saving_view_state)
    }

    fn emit_view_state(&self) -> Result<(), UiError> {
        let view_state = self.with_state(|state| state.view_state());
        self.send_event(PresenterEvent::ViewStateChanged(view_state))
    }

    fn send_event(&self, event: PresenterEvent) -> Result<(), UiError> {
        self.inner
            .event_tx
            .send_blocking(event)
            .map_err(|_| UiError::Dispatch)
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut DomainState) -> T) -> T {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut state)
    }

    fn observe_layout_switch_capture_state(&self, state: LayoutSwitchCaptureState) {
        let generation = self.current_capture_generation();
        let local_capture_active =
            self.with_state(|current| current.view_state().layout_switch.capture_active);
        let state = if local_capture_active {
            match self.inner.client.get_layout_switch_capture_state() {
                Ok(current) => current,
                Err(error) => {
                    eprintln!(
                        "[settings] Failed to reconcile capture state signal; keeping the current lease: {error}"
                    );
                    return;
                }
            }
        } else {
            state
        };

        if self.current_capture_generation() != generation {
            return;
        }
        let _ = self.send_capture_state_event(generation, state);
    }

    fn start_capture_heartbeat(&self) -> u64 {
        self.stop_capture_heartbeat();

        let generation = self
            .inner
            .capture_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let interval = self.inner.capture_heartbeat_interval;
        let client = self.inner.client.clone();
        let capture_generation = Arc::clone(&self.inner.capture_generation);
        let event_tx = self.inner.event_tx.clone();
        let (stop_tx, stop_rx) = mpsc::channel();

        thread::spawn(move || loop {
            match stop_rx.recv_timeout(interval) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            if capture_generation.load(Ordering::SeqCst) != generation {
                break;
            }

            let result = client.renew_layout_switch_capture();

            if capture_generation.load(Ordering::SeqCst) != generation {
                break;
            }

            match result {
                Ok(state) if state.is_active() => {}
                Ok(state) => {
                    let _ = event_tx
                        .send_blocking(PresenterEvent::CaptureStateChanged { generation, state });
                    break;
                }
                Err(error) => {
                    let _ = event_tx
                        .send_blocking(PresenterEvent::CaptureRenewFailed { generation, error });
                    break;
                }
            }
        });

        self.inner
            .capture_heartbeat
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(CaptureHeartbeat { stop_tx });
        generation
    }

    fn stop_capture_heartbeat(&self) -> u64 {
        let generation = self
            .inner
            .capture_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let heartbeat = self
            .inner
            .capture_heartbeat
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(heartbeat) = heartbeat {
            let _ = heartbeat.stop_tx.send(());
        }
        generation
    }

    fn current_capture_generation(&self) -> u64 {
        self.inner.capture_generation.load(Ordering::SeqCst)
    }

    fn send_capture_state_event(
        &self,
        generation: u64,
        state: LayoutSwitchCaptureState,
    ) -> Result<(), UiError> {
        self.send_event(PresenterEvent::CaptureStateChanged { generation, state })
    }
}

impl<C, R> Drop for PresenterInner<C, R>
where
    C: SettingsClientBackend,
    R: CommandRunner,
{
    fn drop(&mut self) {
        self.capture_generation.fetch_add(1, Ordering::SeqCst);
        let heartbeat = self
            .capture_heartbeat
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(heartbeat) = heartbeat {
            let _ = heartbeat.stop_tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ServiceManagerError;
    use crate::error::ValidationError;
    use crate::model::LayoutSwitchCapturePhase;
    use crate::model::{LayoutSwitchSetting, LayoutSwitchSource, Settings, SettingsDto};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    // Test helpers
    #[derive(Clone, Default)]
    struct FakeSettingsClient {
        state: Arc<Mutex<FakeSettingsClientState>>,
    }

    #[derive(Default)]
    struct FakeSettingsClientState {
        save_results: VecDeque<Result<UpdateSettingsResult, SettingsClientError>>,
        saved_settings: Vec<Settings>,
        hotkey_capture_inhibitions: Vec<bool>,
        renew_results: VecDeque<FakeRenewResult>,
        renew_delay: Duration,
        get_state_results: VecDeque<FakeRenewResult>,
        get_state_delay: Duration,
        finish_should_fail: bool,
    }

    #[derive(Clone, Copy)]
    enum FakeRenewResult {
        State(LayoutSwitchCapturePhase),
        Error,
    }

    impl Default for FakeRenewResult {
        fn default() -> Self {
            Self::State(LayoutSwitchCapturePhase::Waiting)
        }
    }

    impl FakeSettingsClient {
        fn push_save_result(&self, result: Result<UpdateSettingsResult, SettingsClientError>) {
            self.state.lock().unwrap().save_results.push_back(result);
        }

        fn saved_settings(&self) -> Vec<Settings> {
            self.state.lock().unwrap().saved_settings.clone()
        }

        fn hotkey_capture_inhibitions(&self) -> Vec<bool> {
            self.state
                .lock()
                .unwrap()
                .hotkey_capture_inhibitions
                .clone()
        }

        fn push_renew_result(&self, result: FakeRenewResult) {
            self.state.lock().unwrap().renew_results.push_back(result);
        }

        fn set_renew_delay(&self, delay: Duration) {
            self.state.lock().unwrap().renew_delay = delay;
        }

        fn push_get_state_result(&self, result: FakeRenewResult) {
            self.state
                .lock()
                .unwrap()
                .get_state_results
                .push_back(result);
        }

        fn set_get_state_delay(&self, delay: Duration) {
            self.state.lock().unwrap().get_state_delay = delay;
        }

        fn fail_finish(&self) {
            self.state.lock().unwrap().finish_should_fail = true;
        }
    }

    impl SettingsClientBackend for FakeSettingsClient {
        fn load_settings(&self) -> Result<Settings, SettingsClientError> {
            Ok(Settings::default())
        }

        fn save_settings(
            &self,
            settings: Settings,
        ) -> Result<UpdateSettingsResult, SettingsClientError> {
            let mut state = self.state.lock().unwrap();
            state.saved_settings.push(settings);
            state
                .save_results
                .pop_front()
                .expect("fake settings client must have queued save result")
        }

        fn start_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            Ok(LayoutSwitchCaptureState::waiting())
        }

        fn renew_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            let (delay, result) = {
                let mut state = self.state.lock().unwrap();
                (
                    state.renew_delay,
                    state.renew_results.pop_front().unwrap_or_default(),
                )
            };
            thread::sleep(delay);
            fake_capture_result(result)
        }

        fn get_layout_switch_capture_state(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            let (delay, result) = {
                let mut state = self.state.lock().unwrap();
                (
                    state.get_state_delay,
                    state.get_state_results.pop_front().unwrap_or_default(),
                )
            };
            thread::sleep(delay);
            fake_capture_result(result)
        }

        fn cancel_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            Ok(LayoutSwitchCaptureState::cancelled())
        }

        fn finish_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            if self.state.lock().unwrap().finish_should_fail {
                Err(capture_test_error())
            } else {
                Ok(LayoutSwitchCaptureState::finished())
            }
        }

        fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> Result<(), SettingsClientError> {
            self.state
                .lock()
                .unwrap()
                .hotkey_capture_inhibitions
                .push(inhibited);
            Ok(())
        }

        fn spawn_capture_listener(&self, _tx: Sender<LayoutSwitchCaptureState>) {}
    }

    #[derive(Clone, Default)]
    struct CountingCaptureClient {
        base: FakeSettingsClient,
        renew_calls: Arc<AtomicUsize>,
        active_renews: Arc<AtomicUsize>,
        max_active_renews: Arc<AtomicUsize>,
    }

    impl CountingCaptureClient {
        fn renew_calls(&self) -> usize {
            self.renew_calls.load(Ordering::SeqCst)
        }

        fn max_active_renews(&self) -> usize {
            self.max_active_renews.load(Ordering::SeqCst)
        }

        fn wait_for_renew_calls(&self, expected: usize) {
            let deadline = Instant::now() + Duration::from_secs(1);
            while self.renew_calls() < expected && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(
                self.renew_calls() >= expected,
                "heartbeat did not renew in time"
            );
        }
    }

    impl SettingsClientBackend for CountingCaptureClient {
        fn load_settings(&self) -> Result<Settings, SettingsClientError> {
            self.base.load_settings()
        }

        fn save_settings(
            &self,
            settings: Settings,
        ) -> Result<UpdateSettingsResult, SettingsClientError> {
            self.base.save_settings(settings)
        }

        fn start_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            self.base.start_layout_switch_capture()
        }

        fn renew_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            self.renew_calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active_renews.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_renews.fetch_max(active, Ordering::SeqCst);
            let result = self.base.renew_layout_switch_capture();
            self.active_renews.fetch_sub(1, Ordering::SeqCst);
            result
        }

        fn get_layout_switch_capture_state(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            self.base.get_layout_switch_capture_state()
        }

        fn cancel_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            self.base.cancel_layout_switch_capture()
        }

        fn finish_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            self.base.finish_layout_switch_capture()
        }

        fn set_hotkey_capture_inhibited(&self, inhibited: bool) -> Result<(), SettingsClientError> {
            self.base.set_hotkey_capture_inhibited(inhibited)
        }

        fn spawn_capture_listener(&self, tx: Sender<LayoutSwitchCaptureState>) {
            self.base.spawn_capture_listener(tx)
        }
    }

    fn capture_test_error() -> SettingsClientError {
        SettingsClientError::Validation(ValidationError::LayoutDelayOutOfRange {
            min: 1,
            max: 2,
            found: 3,
        })
    }

    fn fake_capture_result(
        result: FakeRenewResult,
    ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
        match result {
            FakeRenewResult::State(LayoutSwitchCapturePhase::Idle) => {
                Ok(LayoutSwitchCaptureState::idle())
            }
            FakeRenewResult::State(LayoutSwitchCapturePhase::Waiting) => {
                Ok(LayoutSwitchCaptureState::waiting())
            }
            FakeRenewResult::State(LayoutSwitchCapturePhase::Candidate) => Ok(
                LayoutSwitchCaptureState::candidate(LayoutSwitchCombo::ctrl_shift()),
            ),
            FakeRenewResult::State(LayoutSwitchCapturePhase::Unsupported) => {
                Ok(LayoutSwitchCaptureState::unsupported("unsupported"))
            }
            FakeRenewResult::State(LayoutSwitchCapturePhase::Cancelled) => {
                Ok(LayoutSwitchCaptureState::cancelled())
            }
            FakeRenewResult::State(LayoutSwitchCapturePhase::Finished) => {
                Ok(LayoutSwitchCaptureState::finished())
            }
            FakeRenewResult::Error => Err(capture_test_error()),
        }
    }

    fn capture_presenter(
        client: CountingCaptureClient,
        interval: Duration,
    ) -> (
        SettingsPresenter<CountingCaptureClient, FakeCommandRunner>,
        async_channel::Receiver<PresenterEvent>,
    ) {
        let (event_tx, event_rx) = async_channel::unbounded();
        let presenter = SettingsPresenter::with_services_and_heartbeat_interval(
            client,
            UserServiceController::new(FakeCommandRunner::default()),
            event_tx,
            interval,
        );
        presenter.with_state(|state| state.apply_loaded(Settings::default()));
        (presenter, event_rx)
    }

    fn drain_events(event_rx: &async_channel::Receiver<PresenterEvent>) -> Vec<PresenterEvent> {
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    // Layout switch capture lease
    #[test]
    fn capture_heartbeat_renews_after_successful_start_without_overlap() {
        let client = CountingCaptureClient::default();
        client.base.set_renew_delay(Duration::from_millis(15));
        let (presenter, _event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));

        presenter.start_layout_switch_capture().unwrap();
        client.wait_for_renew_calls(3);
        presenter.cancel_layout_switch_capture().unwrap();

        assert_eq!(client.max_active_renews(), 1);
    }

    #[test]
    fn capture_cancel_finish_and_terminal_state_stop_heartbeat() {
        for stop in [
            LayoutSwitchCapturePhase::Cancelled,
            LayoutSwitchCapturePhase::Finished,
            LayoutSwitchCapturePhase::Unsupported,
            LayoutSwitchCapturePhase::Idle,
        ] {
            let client = CountingCaptureClient::default();
            let (presenter, event_rx) =
                capture_presenter(client.clone(), Duration::from_millis(10));
            presenter.start_layout_switch_capture().unwrap();
            drain_events(&event_rx);
            client.wait_for_renew_calls(1);

            client
                .base
                .push_get_state_result(FakeRenewResult::State(stop));

            presenter.observe_layout_switch_capture_state(match stop {
                LayoutSwitchCapturePhase::Cancelled => LayoutSwitchCaptureState::cancelled(),
                LayoutSwitchCapturePhase::Finished => LayoutSwitchCaptureState::finished(),
                LayoutSwitchCapturePhase::Unsupported => {
                    LayoutSwitchCaptureState::unsupported("unsupported")
                }
                LayoutSwitchCapturePhase::Idle => LayoutSwitchCaptureState::idle(),
                _ => unreachable!(),
            });

            let event = event_rx.recv_blocking().unwrap();
            match event {
                PresenterEvent::CaptureStateChanged { generation, state } => {
                    assert!(presenter.apply_capture_state_event(generation, &state));
                }
                other => panic!("unexpected capture event: {other:?}"),
            }

            let stopped_at = client.renew_calls();
            thread::sleep(Duration::from_millis(35));
            assert_eq!(client.renew_calls(), stopped_at, "terminal phase: {stop:?}");
        }

        let cancel_client = CountingCaptureClient::default();
        let (cancel_presenter, _event_rx) =
            capture_presenter(cancel_client.clone(), Duration::from_millis(10));
        cancel_presenter.start_layout_switch_capture().unwrap();
        cancel_client.wait_for_renew_calls(1);
        cancel_presenter.cancel_layout_switch_capture().unwrap();
        let stopped_at = cancel_client.renew_calls();
        thread::sleep(Duration::from_millis(35));
        assert_eq!(cancel_client.renew_calls(), stopped_at);

        let finish_client = CountingCaptureClient::default();
        let (finish_presenter, _event_rx) =
            capture_presenter(finish_client.clone(), Duration::from_millis(10));
        finish_presenter.start_layout_switch_capture().unwrap();
        finish_client.wait_for_renew_calls(1);
        finish_presenter
            .confirm_captured_layout_switch(LayoutSwitchCombo::alt_shift())
            .unwrap();
        let stopped_at = finish_client.renew_calls();
        thread::sleep(Duration::from_millis(35));
        assert_eq!(finish_client.renew_calls(), stopped_at);
    }

    #[test]
    fn capture_finish_failure_is_terminal_locally_and_stops_heartbeat() {
        let client = CountingCaptureClient::default();
        let (presenter, _event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));
        presenter.start_layout_switch_capture().unwrap();
        client.wait_for_renew_calls(1);
        client.base.fail_finish();

        assert!(presenter
            .confirm_captured_layout_switch(LayoutSwitchCombo::alt_shift())
            .is_err());
        let stopped_at = client.renew_calls();
        thread::sleep(Duration::from_millis(35));

        assert_eq!(client.renew_calls(), stopped_at);
        assert!(
            !presenter
                .with_state(|state| state.view_state())
                .layout_switch
                .capture_active
        );
    }

    #[test]
    fn capture_renew_failure_closes_local_state_and_emits_dedicated_error() {
        let client = CountingCaptureClient::default();
        client.base.push_renew_result(FakeRenewResult::Error);
        let (presenter, event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));

        presenter.start_layout_switch_capture().unwrap();
        drain_events(&event_rx);
        client.wait_for_renew_calls(1);

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut saw_error = false;
        while Instant::now() < deadline {
            if let Ok(event) = event_rx.try_recv() {
                if let PresenterEvent::CaptureRenewFailed { generation, .. } = event {
                    saw_error = presenter.apply_capture_renew_failure(generation);
                    break;
                }
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }

        assert!(saw_error);
        assert!(
            !presenter
                .with_state(|state| state.view_state())
                .layout_switch
                .capture_active
        );
    }

    #[test]
    fn stale_renew_failure_is_ignored_after_cancel_and_new_start() {
        let client = CountingCaptureClient::default();
        client.base.set_renew_delay(Duration::from_millis(40));
        client.base.push_renew_result(FakeRenewResult::Error);
        let (presenter, event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));

        presenter.start_layout_switch_capture().unwrap();
        client.wait_for_renew_calls(1);
        presenter.cancel_layout_switch_capture().unwrap();
        presenter.start_layout_switch_capture().unwrap();
        thread::sleep(Duration::from_millis(60));

        assert!(
            presenter
                .with_state(|state| state.view_state())
                .layout_switch
                .capture_active
        );
        assert!(!drain_events(&event_rx)
            .iter()
            .any(|event| matches!(event, PresenterEvent::CaptureRenewFailed { .. })));
        presenter.cancel_layout_switch_capture().unwrap();
    }

    #[test]
    fn delayed_listener_reconciliation_cannot_stop_a_new_capture_generation() {
        let client = CountingCaptureClient::default();
        let (presenter, event_rx) = capture_presenter(client.clone(), Duration::from_millis(20));
        presenter.start_layout_switch_capture().unwrap();
        drain_events(&event_rx);

        client.base.set_get_state_delay(Duration::from_millis(50));
        client
            .base
            .push_get_state_result(FakeRenewResult::State(LayoutSwitchCapturePhase::Cancelled));
        let observer = presenter.clone();
        let observation = thread::spawn(move || {
            observer.observe_layout_switch_capture_state(LayoutSwitchCaptureState::cancelled());
        });

        thread::sleep(Duration::from_millis(5));
        presenter.cancel_layout_switch_capture().unwrap();
        presenter.start_layout_switch_capture().unwrap();
        observation.join().unwrap();

        assert!(
            presenter
                .with_state(|state| state.view_state())
                .layout_switch
                .capture_active
        );
        for event in drain_events(&event_rx) {
            if let PresenterEvent::CaptureStateChanged { generation, state } = event {
                if state.phase == LayoutSwitchCapturePhase::Cancelled {
                    assert!(!presenter.apply_capture_state_event(generation, &state));
                }
            }
        }
        assert!(
            presenter
                .with_state(|state| state.view_state())
                .layout_switch
                .capture_active
        );
        presenter.cancel_layout_switch_capture().unwrap();
    }

    #[test]
    fn queued_terminal_capture_event_is_rejected_after_a_new_start() {
        let client = CountingCaptureClient::default();
        let (presenter, event_rx) = capture_presenter(client.clone(), Duration::from_millis(20));
        presenter.start_layout_switch_capture().unwrap();
        drain_events(&event_rx);
        client
            .base
            .push_get_state_result(FakeRenewResult::State(LayoutSwitchCapturePhase::Cancelled));

        presenter.observe_layout_switch_capture_state(LayoutSwitchCaptureState::cancelled());
        let old_event = event_rx.recv_blocking().unwrap();
        let PresenterEvent::CaptureStateChanged { generation, state } = old_event else {
            panic!("expected a capture state event");
        };

        presenter.cancel_layout_switch_capture().unwrap();
        presenter.start_layout_switch_capture().unwrap();

        assert!(!presenter.apply_capture_state_event(generation, &state));
        assert!(
            presenter
                .with_state(|current| current.view_state())
                .layout_switch
                .capture_active
        );
        presenter.cancel_layout_switch_capture().unwrap();
    }

    #[test]
    fn cancel_does_not_wait_for_an_in_flight_heartbeat_call() {
        let client = CountingCaptureClient::default();
        client.base.set_renew_delay(Duration::from_millis(200));
        let (presenter, _event_rx) = capture_presenter(client.clone(), Duration::from_millis(5));
        presenter.start_layout_switch_capture().unwrap();
        client.wait_for_renew_calls(1);

        let started = Instant::now();
        presenter.cancel_layout_switch_capture().unwrap();

        assert!(
            started.elapsed() < Duration::from_millis(100),
            "cancel waited behind an in-flight renew"
        );
    }

    #[test]
    fn capture_window_discard_stops_heartbeat() {
        let client = CountingCaptureClient::default();
        let (presenter, _event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));
        presenter.start_layout_switch_capture().unwrap();
        client.wait_for_renew_calls(1);

        presenter.discard_changes();
        let stopped_at = client.renew_calls();
        thread::sleep(Duration::from_millis(35));

        assert_eq!(client.renew_calls(), stopped_at);
    }

    #[test]
    fn failed_listener_reconciliation_keeps_the_current_heartbeat() {
        let client = CountingCaptureClient::default();
        let (presenter, event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));
        presenter.start_layout_switch_capture().unwrap();
        drain_events(&event_rx);
        client.base.push_get_state_result(FakeRenewResult::Error);

        presenter.observe_layout_switch_capture_state(LayoutSwitchCaptureState::cancelled());
        let renews_before = client.renew_calls();
        client.wait_for_renew_calls(renews_before + 1);

        assert!(
            presenter
                .with_state(|state| state.view_state())
                .layout_switch
                .capture_active
        );
        assert!(event_rx.try_recv().is_err());
        presenter.cancel_layout_switch_capture().unwrap();
    }

    #[test]
    fn dropping_presenter_stops_heartbeat_without_retaining_presenter() {
        let client = CountingCaptureClient::default();
        let (presenter, _event_rx) = capture_presenter(client.clone(), Duration::from_millis(10));
        presenter.start_layout_switch_capture().unwrap();
        client.wait_for_renew_calls(1);

        drop(presenter);
        let stopped_at = client.renew_calls();
        thread::sleep(Duration::from_millis(35));

        assert_eq!(client.renew_calls(), stopped_at);
    }

    #[test]
    fn production_capture_heartbeat_is_shorter_than_soft_lease() {
        assert!(CAPTURE_HEARTBEAT < crate::daemon::capture::CAPTURE_SOFT_LEASE);
    }

    #[derive(Clone, Default)]
    struct FakeCommandRunner {
        state: Arc<Mutex<FakeCommandRunnerState>>,
    }

    #[derive(Default)]
    struct FakeCommandRunnerState {
        commands: Vec<Vec<String>>,
        results: VecDeque<Result<String, ServiceManagerError>>,
    }

    impl FakeCommandRunner {
        fn push_err(&self, code: i32, stderr: &str) {
            self.state
                .lock()
                .unwrap()
                .results
                .push_back(Err(ServiceManagerError::CommandFailed {
                    command: Vec::new(),
                    code: Some(code),
                    stderr: stderr.to_string(),
                }));
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, command: &[&str]) -> Result<String, ServiceManagerError> {
            let mut state = self.state.lock().unwrap();
            state
                .commands
                .push(command.iter().map(|part| (*part).to_string()).collect());
            match state.results.pop_front() {
                Some(result) => result,
                None => panic!("fake command runner has no queued result"),
            }
        }
    }

    // Hotkey capture inhibition
    #[test]
    fn presenter_forwards_hotkey_capture_inhibition_to_client() {
        let client = FakeSettingsClient::default();
        let runner = FakeCommandRunner::default();
        let (event_tx, _event_rx) = async_channel::unbounded();
        let presenter = SettingsPresenter::with_services(
            client.clone(),
            UserServiceController::new(runner),
            event_tx,
        );

        presenter.set_hotkey_capture_inhibited(true).unwrap();
        presenter.set_hotkey_capture_inhibited(false).unwrap();

        assert_eq!(client.hotkey_capture_inhibitions(), vec![true, false]);
    }

    // Save flow
    #[test]
    fn save_keeps_persisted_changes_when_autostart_apply_fails_and_reloads_real_state() {
        let client = FakeSettingsClient::default();
        let committed = Settings {
            auto_switch_enabled: false,
            ..Settings::default()
        };
        client.push_save_result(Ok(UpdateSettingsResult {
            message: "saved".to_string(),
            restart_required: false,
            settings: SettingsDto::from(committed),
        }));

        let temp_dir = tempfile::tempdir().unwrap();
        let autostart_file = temp_dir.path().join("open-switcher.desktop");
        let runner = FakeCommandRunner::default();
        runner.push_err(1, "enable failed");

        let (event_tx, event_rx) = async_channel::unbounded();
        let presenter = SettingsPresenter::with_services(
            client.clone(),
            UserServiceController::with_autostart_file(runner, autostart_file),
            event_tx,
        );

        presenter.with_state(|state| {
            state.apply_loaded(Settings::default());
            state.apply_loaded_autostart(false);
        });

        presenter.update_auto_switch_enabled(false);
        presenter.set_autostart_enabled(true);

        assert!(matches!(presenter.save(), SaveRequest::Accepted(_)));

        loop {
            match event_rx.recv_blocking().unwrap() {
                PresenterEvent::SaveFailed(SettingsClientError::ServiceManager(_)) => break,
                PresenterEvent::ViewStateChanged(_) => {}
                other => panic!("unexpected presenter event: {other:?}"),
            }
        }

        let final_view = presenter.with_state(|state| state.view_state());
        assert!(!final_view.auto_switch_enabled);
        assert!(!final_view.autostart_enabled);
        assert!(!final_view.dirty);
        assert!(!final_view.save_enabled);

        let saved_settings = client.saved_settings();
        assert_eq!(saved_settings.len(), 1);
        assert!(!saved_settings[0].auto_switch_enabled);
        assert_eq!(
            saved_settings[0].layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::default(),
                source: LayoutSwitchSource::Unknown,
                auto_detected: Default::default(),
            }
        );
    }
}
