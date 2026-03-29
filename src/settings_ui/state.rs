use crate::model::{Settings, UndoKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestState {
    Loading,
    Idle,
    Saving,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainState {
    loaded: Option<Settings>,
    draft: Settings,
    request_state: RequestState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewState {
    pub layout_delay_ms: u32,
    pub undo_key: UndoKey,
    pub loading: bool,
    pub saving: bool,
    pub loaded: bool,
    pub dirty: bool,
    pub form_enabled: bool,
    pub save_enabled: bool,
    pub cancel_enabled: bool,
    pub status_text: &'static str,
}

impl DomainState {
    pub fn new() -> Self {
        let draft = Settings::default();
        Self {
            loaded: None,
            draft,
            request_state: RequestState::Loading,
        }
    }

    pub fn apply_loaded(&mut self, settings: Settings) {
        self.loaded = Some(settings);
        self.draft = settings;
        self.request_state = RequestState::Idle;
    }

    pub fn finish_loading(&mut self) {
        self.request_state = RequestState::Idle;
    }

    pub fn begin_save(&mut self) -> Option<Settings> {
        if self.request_state != RequestState::Idle || self.loaded.is_none() {
            return None;
        }

        self.request_state = RequestState::Saving;
        Some(self.draft)
    }

    pub fn save_succeeded(&mut self) {
        self.loaded = Some(self.draft);
        self.request_state = RequestState::Idle;
    }

    pub fn save_failed(&mut self) {
        self.request_state = RequestState::Idle;
    }

    pub fn update_layout_delay(&mut self, value: u32) -> bool {
        if self.draft.layout_delay_ms == value {
            return false;
        }

        self.draft.layout_delay_ms = value;
        true
    }

    pub fn update_undo_key(&mut self, value: UndoKey) -> bool {
        if self.draft.undo_key == value {
            return false;
        }

        self.draft.undo_key = value;
        true
    }

    pub fn view_state(&self) -> ViewState {
        let loading = self.request_state == RequestState::Loading;
        let saving = self.request_state == RequestState::Saving;
        let loaded = self.loaded.is_some();
        let dirty = self
            .loaded
            .map(|settings| settings != self.draft)
            .unwrap_or(false);

        ViewState {
            layout_delay_ms: self.draft.layout_delay_ms,
            undo_key: self.draft.undo_key,
            loading,
            saving,
            loaded,
            dirty,
            form_enabled: loaded && !saving,
            save_enabled: loaded && !saving && dirty,
            cancel_enabled: !saving,
            status_text: if loading {
                "Загрузка настроек из демона OpenSwitcher..."
            } else if saving {
                "Сохранение настроек через D-Bus..."
            } else {
                ""
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_enabled_only_for_dirty_idle_state() {
        let mut state = DomainState::new();
        state.apply_loaded(Settings::default());
        assert!(!state.view_state().save_enabled);

        state.update_layout_delay(42);
        assert!(state.view_state().save_enabled);

        state.begin_save();
        assert!(!state.view_state().save_enabled);
    }
}
