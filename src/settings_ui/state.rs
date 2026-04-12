use crate::model::{
    AutoDetectedLayoutSwitch, LayoutSwitchCombo, LayoutSwitchSource, SelectedTextHotkey, Settings,
    UndoKey,
};

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
    loaded_autostart_enabled: Option<bool>,
    autostart_enabled: bool,
    layout_switch_manual_override: bool,
    layout_switch_capture_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSave {
    pub settings: Settings,
    pub autostart_change: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewState {
    pub autostart_enabled: bool,
    pub auto_switch_enabled: bool,
    pub fix_two_capitals: bool,
    pub fix_accidental_caps_lock: bool,
    pub layout_delay_ms: u32,
    pub undo_key: UndoKey,
    pub selected_text_hotkey: SelectedTextHotkey,
    pub layout_switch: LayoutSwitchViewState,
    pub loading: bool,
    pub saving: bool,
    pub loaded: bool,
    pub dirty: bool,
    pub form_enabled: bool,
    pub save_enabled: bool,
    pub cancel_enabled: bool,
    pub status_text: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSwitchViewState {
    pub combo: LayoutSwitchCombo,
    pub combo_label: String,
    pub source: LayoutSwitchSource,
    pub editable: bool,
    pub manual_override_active: bool,
    pub show_unlock_hint: bool,
    pub show_fallback_hint: bool,
    pub capture_active: bool,
    pub capture_hint: &'static str,
    pub actions: LayoutSwitchActionsState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSwitchActionsState {
    pub can_capture: bool,
}

impl DomainState {
    pub fn new() -> Self {
        let draft = Settings::default();
        Self {
            loaded: None,
            draft,
            request_state: RequestState::Loading,
            loaded_autostart_enabled: None,
            autostart_enabled: false,
            layout_switch_manual_override: false,
            layout_switch_capture_active: false,
        }
    }

    pub fn apply_loaded(&mut self, settings: Settings) {
        self.loaded = Some(settings);
        self.draft = settings;
        self.request_state = RequestState::Idle;
        self.layout_switch_manual_override = false;
        self.layout_switch_capture_active = false;
    }

    pub fn apply_loaded_autostart(&mut self, enabled: bool) {
        self.loaded_autostart_enabled = Some(enabled);
        self.autostart_enabled = enabled;
    }

    pub fn begin_loading(&mut self) -> bool {
        if self.request_state == RequestState::Saving {
            return false;
        }

        self.request_state = RequestState::Loading;
        true
    }

    pub fn finish_loading(&mut self) {
        self.request_state = RequestState::Idle;
    }

    pub fn begin_save(&mut self) -> Option<PendingSave> {
        if self.request_state != RequestState::Idle || self.loaded.is_none() {
            return None;
        }

        self.request_state = RequestState::Saving;
        Some(PendingSave {
            settings: self.settings_for_save(),
            autostart_change: self
                .loaded_autostart_enabled
                .filter(|loaded| *loaded != self.autostart_enabled)
                .map(|_| self.autostart_enabled),
        })
    }

    pub fn save_succeeded(&mut self, snapshot: PendingSave) {
        self.save_persisted_settings_succeeded(snapshot.settings);
        if let Some(enabled) = snapshot.autostart_change {
            self.loaded_autostart_enabled = Some(enabled);
            self.autostart_enabled = enabled;
        }
    }

    pub fn save_failed(&mut self) {
        self.request_state = RequestState::Idle;
    }

    pub fn save_persisted_settings_succeeded(&mut self, settings: Settings) {
        self.loaded = Some(settings);
        self.draft = settings;
        self.request_state = RequestState::Idle;
        self.layout_switch_manual_override = false;
        self.layout_switch_capture_active = false;
    }

    pub fn set_autostart_enabled(&mut self, enabled: bool) -> bool {
        if self.autostart_enabled == enabled {
            return false;
        }

        self.autostart_enabled = enabled;
        true
    }

    pub fn discard_changes(&mut self) -> bool {
        let Some(loaded) = self.loaded else {
            return false;
        };

        let autostart_dirty = self
            .loaded_autostart_enabled
            .map(|enabled| enabled != self.autostart_enabled)
            .unwrap_or(false);

        if self.draft == loaded && !autostart_dirty {
            self.layout_switch_manual_override = false;
            self.layout_switch_capture_active = false;
            return false;
        }

        self.draft = loaded;
        if let Some(enabled) = self.loaded_autostart_enabled {
            self.autostart_enabled = enabled;
        }
        self.layout_switch_manual_override = false;
        self.layout_switch_capture_active = false;
        true
    }

    pub fn update_undo_key(&mut self, value: UndoKey) -> bool {
        if self.draft.undo_key == value {
            return false;
        }

        self.draft.undo_key = value;
        true
    }

    pub fn update_layout_delay(&mut self, value: u32) -> bool {
        if self.draft.layout_delay_ms == value {
            return false;
        }

        self.draft.layout_delay_ms = value;
        true
    }

    pub fn update_selected_text_hotkey(&mut self, value: SelectedTextHotkey) -> bool {
        if self.draft.selected_text_hotkey == value {
            return false;
        }

        self.draft.selected_text_hotkey = value;
        true
    }

    pub fn update_auto_switch_enabled(&mut self, value: bool) -> bool {
        if self.draft.auto_switch_enabled == value {
            return false;
        }

        self.draft.auto_switch_enabled = value;
        true
    }

    pub fn update_fix_two_capitals(&mut self, value: bool) -> bool {
        if self.draft.fix_two_capitals == value {
            return false;
        }

        self.draft.fix_two_capitals = value;
        true
    }

    pub fn update_fix_accidental_caps_lock(&mut self, value: bool) -> bool {
        if self.draft.fix_accidental_caps_lock == value {
            return false;
        }

        self.draft.fix_accidental_caps_lock = value;
        true
    }

    pub fn unlock_layout_switch_override(&mut self) -> bool {
        if self.layout_switch_manual_override
            || !self
                .loaded
                .map(|settings| settings.layout_switch.is_locked_by_auto_detection())
                .unwrap_or(false)
        {
            return false;
        }

        self.layout_switch_manual_override = true;
        true
    }

    pub fn start_layout_switch_capture(&mut self) -> bool {
        if self.request_state == RequestState::Saving || self.loaded.is_none() {
            return false;
        }

        self.layout_switch_manual_override = true;
        self.layout_switch_capture_active = true;
        true
    }

    pub fn cancel_layout_switch_capture(&mut self) -> bool {
        if !self.layout_switch_capture_active {
            return false;
        }

        self.layout_switch_capture_active = false;
        true
    }

    pub fn apply_captured_layout_switch(&mut self, combo: LayoutSwitchCombo) -> bool {
        self.layout_switch_capture_active = false;
        self.layout_switch_manual_override = true;

        if self.draft.layout_switch.combo == combo {
            return true;
        }

        self.draft.layout_switch.combo = combo;
        true
    }

    pub fn set_layout_switch_capture_active(&mut self, active: bool) -> bool {
        if self.layout_switch_capture_active == active {
            return false;
        }

        self.layout_switch_capture_active = active;
        true
    }

    pub fn view_state(&self) -> ViewState {
        let loading = self.request_state == RequestState::Loading;
        let saving = self.request_state == RequestState::Saving;
        let loaded = self.loaded.is_some();
        let settings_dirty = self
            .loaded
            .map(|settings| settings != self.draft)
            .unwrap_or(false);
        let autostart_dirty = self
            .loaded_autostart_enabled
            .map(|enabled| enabled != self.autostart_enabled)
            .unwrap_or(false);
        let dirty = settings_dirty || autostart_dirty;

        ViewState {
            autostart_enabled: self.autostart_enabled,
            auto_switch_enabled: self.draft.auto_switch_enabled,
            fix_two_capitals: self.draft.fix_two_capitals,
            fix_accidental_caps_lock: self.draft.fix_accidental_caps_lock,
            layout_delay_ms: self.draft.layout_delay_ms,
            undo_key: self.draft.undo_key,
            selected_text_hotkey: self.draft.selected_text_hotkey,
            layout_switch: LayoutSwitchViewState {
                combo: self.draft.layout_switch.combo,
                combo_label: self.draft.layout_switch.combo.short_label().to_string(),
                source: if self.layout_switch_manual_override {
                    LayoutSwitchSource::Manual
                } else {
                    self.draft.layout_switch.source
                },
                editable: loaded
                    && !saving
                    && (!self.draft.layout_switch.is_locked_by_auto_detection()
                        || self.layout_switch_manual_override),
                manual_override_active: self.layout_switch_manual_override,
                show_unlock_hint: loaded
                    && self.draft.layout_switch.is_locked_by_auto_detection()
                    && !self.layout_switch_manual_override,
                show_fallback_hint: loaded
                    && self.draft.layout_switch.source == LayoutSwitchSource::AutoFallback,
                capture_active: self.layout_switch_capture_active,
                capture_hint: if self.layout_switch_capture_active {
                    "Нажмите желаемую комбинацию. Esc — отмена."
                } else {
                    ""
                },
                actions: LayoutSwitchActionsState {
                    can_capture: loaded && !saving,
                },
            },
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

    fn settings_for_save(&self) -> Settings {
        let mut settings = self.draft;
        if self.layout_switch_manual_override {
            settings.layout_switch.source = LayoutSwitchSource::Manual;
            settings.layout_switch.auto_detected = AutoDetectedLayoutSwitch::default();
        }
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_enabled_only_for_dirty_idle_state() {
        let mut state = DomainState::new();
        state.apply_loaded(Settings::default());
        state.apply_loaded_autostart(false);
        assert!(!state.view_state().save_enabled);

        state.update_auto_switch_enabled(false);
        assert!(state.view_state().save_enabled);

        state.begin_save();
        assert!(!state.view_state().save_enabled);
    }

    #[test]
    fn autostart_checkbox_is_part_of_dirty_settings_state() {
        let mut state = DomainState::new();
        state.apply_loaded(Settings::default());
        state.apply_loaded_autostart(false);
        state.set_autostart_enabled(true);

        let view = state.view_state();
        assert!(view.autostart_enabled);
        assert!(view.dirty);
        assert!(view.save_enabled);
    }

    #[test]
    fn begin_save_includes_new_persisted_fix_settings_and_autostart_change() {
        let mut state = DomainState::new();
        state.apply_loaded(Settings::default());
        state.apply_loaded_autostart(false);
        state.set_autostart_enabled(true);
        state.update_auto_switch_enabled(false);
        state.update_fix_two_capitals(true);
        state.update_fix_accidental_caps_lock(true);

        let snapshot = state.begin_save().expect("save snapshot should exist");

        assert_eq!(snapshot.autostart_change, Some(true));
        assert!(!snapshot.settings.auto_switch_enabled);
        assert!(snapshot.settings.fix_two_capitals);
        assert!(snapshot.settings.fix_accidental_caps_lock);
        assert!(state.view_state().saving);
    }

    #[test]
    fn discard_changes_restores_persisted_settings_and_autostart_checkbox() {
        let mut state = DomainState::new();
        state.apply_loaded(Settings::default());
        state.apply_loaded_autostart(false);
        state.set_autostart_enabled(true);
        state.update_fix_two_capitals(true);

        assert!(state.discard_changes());
        assert!(!state.view_state().autostart_enabled);
        assert!(!state.view_state().fix_two_capitals);
        assert!(!state.view_state().dirty);
    }
}
