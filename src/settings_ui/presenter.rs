use super::dbus_client::SettingsDbusClient;
use super::state::{DomainState, ViewState};
use crate::error::{SettingsClientError, UiError};
use crate::model::{UndoKey, UpdateSettingsResult};
use async_channel::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug)]
pub enum PresenterEvent {
    ViewStateChanged(ViewState),
    LoadFailed(SettingsClientError),
    SaveFailed(SettingsClientError),
    SaveSucceeded(UpdateSettingsResult),
}

pub enum SaveRequest {
    Ignored,
    Accepted(ViewState),
}

#[derive(Clone)]
pub struct SettingsPresenter {
    inner: Arc<PresenterInner>,
}

struct PresenterInner {
    client: SettingsDbusClient,
    state: Mutex<DomainState>,
    event_tx: Sender<PresenterEvent>,
}

impl SettingsPresenter {
    pub fn new(client: SettingsDbusClient, event_tx: Sender<PresenterEvent>) -> Self {
        Self {
            inner: Arc::new(PresenterInner {
                client,
                state: Mutex::new(DomainState::new()),
                event_tx,
            }),
        }
    }

    pub fn initialize(&self) {
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
                let _ = presenter.emit_view_state();
            }
            Err(error) => {
                presenter.with_state(DomainState::finish_loading);
                let _ = presenter.emit_view_state();
                let _ = presenter.send_event(PresenterEvent::LoadFailed(error));
            }
        });
    }

    pub fn update_layout_delay(&self, value: u32) {
        let changed = self.with_state(|state| state.update_layout_delay(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn update_undo_key(&self, value: UndoKey) {
        let changed = self.with_state(|state| state.update_undo_key(value));
        if changed {
            let _ = self.emit_view_state();
        }
    }

    pub fn discard_changes(&self) {
        let changed = self.with_state(DomainState::discard_changes);
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

        if let Err(error) = snapshot.validate() {
            self.with_state(DomainState::save_failed);
            let reset_view_state = self.with_state(|state| state.view_state());
            let _ = self.send_event(PresenterEvent::ViewStateChanged(reset_view_state.clone()));
            let _ = self.send_event(PresenterEvent::SaveFailed(SettingsClientError::from(error)));
            return SaveRequest::Accepted(reset_view_state);
        }

        let presenter = self.clone();
        thread::spawn(
            move || match presenter.inner.client.save_settings(snapshot) {
                Ok(result) => {
                    presenter.with_state(DomainState::save_succeeded);
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
