use super::dbus_client::SettingsDbusClient;
use super::state::{DomainState, ViewState};
use crate::error::{SettingsClientError, UiError};
use crate::model::{
    LayoutSwitchCaptureState, LayoutSwitchCombo, SelectedTextHotkey, UndoKey, UpdateSettingsResult,
};
use crate::system::user_services::{CommandRunner, ProcessCommandRunner};
use crate::system::UserServiceController;
use async_channel::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug)]
pub enum PresenterEvent {
    ViewStateChanged(ViewState),
    LoadFailed(SettingsClientError),
    SaveFailed(SettingsClientError),
    SaveSucceeded(UpdateSettingsResult),
    CaptureStateChanged(LayoutSwitchCaptureState),
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
    fn cancel_layout_switch_capture(&self)
        -> Result<LayoutSwitchCaptureState, SettingsClientError>;
    fn finish_layout_switch_capture(&self)
        -> Result<LayoutSwitchCaptureState, SettingsClientError>;
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
        Self {
            inner: Arc::new(PresenterInner {
                client,
                services,
                state: Mutex::new(DomainState::new()),
                event_tx,
            }),
        }
    }

    pub fn initialize(&self) {
        let (capture_tx, capture_rx) = async_channel::unbounded();
        self.inner.client.spawn_capture_listener(capture_tx);

        let presenter = self.clone();
        thread::spawn(move || {
            while let Ok(state) = capture_rx.recv_blocking() {
                let _ = presenter.send_event(PresenterEvent::CaptureStateChanged(state));
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

    pub fn update_undo_key(&self, value: UndoKey) {
        let changed = self.with_state(|state| state.update_undo_key(value));
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

    pub fn update_selected_text_hotkey(&self, value: SelectedTextHotkey) {
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
        let changed = self.with_state(DomainState::start_layout_switch_capture);
        if changed {
            let _ = self.emit_view_state();
        }

        match self.inner.client.start_layout_switch_capture() {
            Ok(state) => {
                let _ = self.send_event(PresenterEvent::CaptureStateChanged(state));
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
        let changed = self.with_state(DomainState::cancel_layout_switch_capture);
        if changed {
            let _ = self.emit_view_state();
        }

        match self.inner.client.cancel_layout_switch_capture() {
            Ok(state) => {
                let _ = self.send_event(PresenterEvent::CaptureStateChanged(state));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn confirm_captured_layout_switch(
        &self,
        combo: LayoutSwitchCombo,
    ) -> Result<(), SettingsClientError> {
        let state = self.inner.client.finish_layout_switch_capture()?;
        let changed = self.with_state(|current| current.apply_captured_layout_switch(combo));
        if changed {
            let _ = self.emit_view_state();
        }
        let _ = self.send_event(PresenterEvent::CaptureStateChanged(state));
        Ok(())
    }

    pub fn discard_changes(&self) {
        let changed = self.with_state(DomainState::discard_changes);
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn sync_layout_switch_capture_active(&self, active: bool) {
        let changed = self.with_state(|state| state.set_layout_switch_capture_active(active));
        if changed {
            let _ = self.emit_view_state();
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ServiceManagerError;
    use crate::model::{LayoutSwitchSetting, LayoutSwitchSource, Settings};
    use std::collections::VecDeque;

    // Test helpers
    #[derive(Clone, Default)]
    struct FakeSettingsClient {
        state: Arc<Mutex<FakeSettingsClientState>>,
    }

    #[derive(Default)]
    struct FakeSettingsClientState {
        save_results: VecDeque<Result<UpdateSettingsResult, SettingsClientError>>,
        saved_settings: Vec<Settings>,
    }

    impl FakeSettingsClient {
        fn push_save_result(&self, result: Result<UpdateSettingsResult, SettingsClientError>) {
            self.state.lock().unwrap().save_results.push_back(result);
        }

        fn saved_settings(&self) -> Vec<Settings> {
            self.state.lock().unwrap().saved_settings.clone()
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
            panic!("capture is not used in this test")
        }

        fn cancel_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            panic!("capture is not used in this test")
        }

        fn finish_layout_switch_capture(
            &self,
        ) -> Result<LayoutSwitchCaptureState, SettingsClientError> {
            panic!("capture is not used in this test")
        }

        fn spawn_capture_listener(&self, _tx: Sender<LayoutSwitchCaptureState>) {}
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
        fn push_ok(&self, stdout: &str) {
            self.state
                .lock()
                .unwrap()
                .results
                .push_back(Ok(stdout.to_string()));
        }

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

    // Save flow
    #[test]
    fn save_keeps_persisted_changes_when_autostart_apply_fails_and_reloads_real_state() {
        let client = FakeSettingsClient::default();
        client.push_save_result(Ok(UpdateSettingsResult {
            message: "saved".to_string(),
            restart_required: false,
        }));

        let runner = FakeCommandRunner::default();
        runner.push_err(1, "enable failed");
        runner.push_ok("disabled\n");

        let (event_tx, event_rx) = async_channel::unbounded();
        let presenter = SettingsPresenter::with_services(
            client.clone(),
            UserServiceController::new(runner),
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
