use crate::daemon::keyboard::{
    is_character, is_modifier, is_russian_layout, undo_key_to_evdev_key, KeyboardController, ModifierState,
    SharedModifierState,
};
use crate::daemon::runtime::{log_layout_debug, RuntimeState};
use crate::daemon::selected_text::{
    log_selected_text_debug, SelectedTextJobRunner,
};
use crate::daemon::switch_logic::{manual_correction_plan, should_switch, Keystroke};
use crate::dbus::{emit_layout_switch_capture_state_changed, emit_status_changed};
use crate::error::SwitcherError;
use evdev::InputEventKind;
use std::sync::Arc;
use zbus::blocking::Connection;

#[derive(Default)]
struct WordContext {
    valid: bool,
    word_before_cursor: Vec<Keystroke>,
    followed_by_separator: bool,
}

pub struct DaemonService {
    runtime: Arc<RuntimeState>,
    connection: Connection,
    keyboard: KeyboardController,
    modifiers: ModifierState,
    shared_modifiers: SharedModifierState,
    buffer: Vec<Keystroke>,
    word_context: WordContext,
    selected_text_runner: SelectedTextJobRunner,
    suppressed_hotkey_key: Option<evdev::Key>,
    suppressed_separator_key: Option<evdev::Key>,
    pending_auto_correction_separator: Option<evdev::Key>,
    pending_selected_text_switch: bool,
}

impl DaemonService {
    pub fn new(runtime: Arc<RuntimeState>, connection: Connection) -> Result<Self, SwitcherError> {
        let keyboard = KeyboardController::open()?;
        let shared_modifiers = SharedModifierState::default();
        let selected_text_runner =
            SelectedTextJobRunner::new(keyboard.selection_transport(shared_modifiers.clone()))?;

        Ok(Self {
            runtime,
            connection,
            keyboard,
            modifiers: ModifierState::default(),
            shared_modifiers,
            buffer: Vec::new(),
            word_context: WordContext::default(),
            selected_text_runner,
            suppressed_hotkey_key: None,
            suppressed_separator_key: None,
            pending_auto_correction_separator: None,
            pending_selected_text_switch: false,
        })
    }

    pub fn run(&mut self) -> Result<(), SwitcherError> {
        loop {
            for event in self.keyboard.fetch_events()? {
                if let InputEventKind::Key(key) = event.kind() {
                    self.handle_key_event(key, event.value())?;
                }
            }
        }
    }

    fn handle_key_event(&mut self, key: evdev::Key, value: i32) -> Result<(), SwitcherError> {
        if self.keyboard.drain_pointer_clicks()? {
            self.invalidate_word_context();
        }

        if self.suppressed_hotkey_key == Some(key) {
            if value == 0 {
                self.suppressed_hotkey_key = None;
            }
            self.maybe_run_pending_selected_text_switch()?;
            return Ok(());
        }

        if self.suppressed_separator_key == Some(key) {
            if value == 0 {
                if self.pending_auto_correction_separator == Some(key) {
                    self.pending_auto_correction_separator = None;
                    self.finish_pending_auto_correction(key, &self.runtime.config_snapshot()?)?;
                }
                self.suppressed_separator_key = None;
            }
            return Ok(());
        }

        if self.runtime.is_capture_active()? {
            self.modifiers.update(key, value);

            if let Some(state) = self.runtime.handle_capture_key_event(key, value)? {
                emit_layout_switch_capture_state_changed(&self.connection, &state)?;
            }

            if !self.runtime.is_capture_active()? {
                self.invalidate_word_context();
            }

            return Ok(());
        }

        let config = self.runtime.config_snapshot()?;
        self.modifiers.update(key, value);
        self.shared_modifiers.store(self.modifiers);

        if value == 1 && self.pending_auto_correction_separator.is_some() && !is_modifier(key) {
            let separator_key = self.pending_auto_correction_separator.take().unwrap();
            self.suppressed_separator_key = None;
            self.finish_pending_auto_correction(separator_key, &config)?;
        }

        if self
            .modifiers
            .matches_layout_switch_combo(config.layout_switch_combo, key, value)
        {
            log_layout_debug(
                "observed-layout-shortcut",
                &format!(
                    "combo={:?} key={key:?} value={value} shift={} ctrl={} alt={} layout_before={}",
                    config.layout_switch_combo,
                    self.modifiers.is_shift_pressed(),
                    self.modifiers.is_ctrl_pressed(),
                    self.modifiers.is_alt_pressed(),
                    if self.runtime.current_layout() { "EN" } else { "RU" }
                ),
            );
            self.runtime.set_layout_with_reason(
                !self.runtime.current_layout(),
                "user-layout-shortcut",
            );
            self.publish_status_changed()?;
            self.invalidate_word_context();
        }

        if selected_text_hotkey_matches(config.selected_text_hotkey, self.modifiers, key, value) {
            log_selected_text_debug(
                "hotkey-matched",
                &format!(
                    "key={key:?} shift={} ctrl={} alt={}",
                    self.modifiers.is_shift_pressed(),
                    self.modifiers.is_ctrl_pressed(),
                    self.modifiers.is_alt_pressed()
                ),
            );
            self.suppressed_hotkey_key = Some(key);
            self.pending_selected_text_switch = true;
            return Ok(());
        }

        if value != 1 {
            let result = self.keyboard.forward_event(key, value);
            if result.is_ok() {
                self.maybe_run_pending_selected_text_switch()?;
            }
            return result;
        }

        if key == undo_key_to_evdev_key(config.undo_key) {
            let word_before_cursor = if self.can_correct_word_before_cursor() {
                self.word_context.word_before_cursor.clone()
            } else {
                Vec::new()
            };
            let used_current_buffer = !self.buffer.is_empty();
            if self.apply_manual_correction(&config, &word_before_cursor)? {
                if used_current_buffer {
                    self.word_context.valid = true;
                    self.word_context.word_before_cursor.clear();
                    self.word_context.followed_by_separator = false;
                }
            }
            return Ok(());
        }

        match key {
            evdev::Key::KEY_SPACE => {
                let is_russian = !self.runtime.current_layout();
                let corrected = self.runtime.is_enabled()
                    && !is_russian
                    && should_switch(&self.buffer);

                if corrected {
                    self.suppressed_separator_key = Some(key);
                    self.pending_auto_correction_separator = Some(key);
                    return Ok(());
                }

                self.word_context.valid = !self.buffer.is_empty();
                self.word_context.word_before_cursor = self.buffer.clone();
                self.word_context.followed_by_separator = true;
                self.buffer.clear();
                self.keyboard.forward_event(key, 1)
            }
            evdev::Key::KEY_ENTER | evdev::Key::KEY_TAB => {
                self.invalidate_word_context();
                self.keyboard.forward_event(key, 1)
            }
            evdev::Key::KEY_BACKSPACE => {
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                } else if self.word_context.valid && self.word_context.followed_by_separator {
                    self.buffer = self.word_context.word_before_cursor.clone();
                    self.word_context.followed_by_separator = false;
                }
                self.keyboard.forward_event(key, 1)
            }
            _ => {
                let plain_character_input = is_character(key)
                    && !self.modifiers.is_ctrl_pressed()
                    && !self.modifiers.is_alt_pressed()
                    && !self.modifiers.is_meta_pressed();

                if plain_character_input {
                    self.word_context.valid = true;
                    self.buffer.push(Keystroke {
                        key,
                        shift: self.modifiers.is_shift_pressed(),
                    });
                } else if !is_modifier(key) {
                    self.invalidate_word_context();
                }
                let result = self.keyboard.forward_event(key, 1);
                if result.is_ok() {
                    self.maybe_run_pending_selected_text_switch()?;
                }
                result
            }
        }
    }

    fn apply_selected_text_switch(&mut self) -> Result<(), SwitcherError> {
        if !self.selected_text_runner.try_start()? {
            log_selected_text_debug("hotkey-skip", "selected-text job already running");
            return Ok(());
        }

        self.invalidate_word_context();
        log_selected_text_debug("job-started", "selected-text job dispatched to worker");
        Ok(())
    }

    fn finish_pending_auto_correction(
        &mut self,
        separator_key: evdev::Key,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
    ) -> Result<(), SwitcherError> {
        self.apply_manual_correction(config, &[])?;
        self.word_context.valid = !self.buffer.is_empty();
        self.word_context.word_before_cursor = self.buffer.clone();
        self.word_context.followed_by_separator = true;
        self.buffer.clear();
        self.keyboard.type_separator(separator_key)
    }

    fn maybe_run_pending_selected_text_switch(&mut self) -> Result<(), SwitcherError> {
        if !self.pending_selected_text_switch {
            return Ok(());
        }

        if self.suppressed_hotkey_key.is_some() {
            return Ok(());
        }

        self.pending_selected_text_switch = false;
        log_selected_text_debug(
            "hotkey-trigger",
            "running selected-text switch after trigger key release",
        );
        self.apply_selected_text_switch()
    }

    fn apply_manual_correction(
        &mut self,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
        fallback_buffer: &[Keystroke],
    ) -> Result<bool, SwitcherError> {
        let Some(plan) = manual_correction_plan(
            &self.buffer,
            fallback_buffer,
            self.word_context.followed_by_separator,
        ) else {
            return Ok(false);
        };

        self.keyboard
            .apply_correction(&plan, config, self.modifiers)?;
        match is_russian_layout() {
            Ok(is_russian) => {
                log_layout_debug(
                    "manual-correction-sync",
                    &format!("source=xset is_russian={is_russian}"),
                );
                self.runtime
                    .set_layout_with_reason(!is_russian, "manual-correction-xset-sync");
            }
            Err(error) => {
                log_layout_debug(
                    "manual-correction-sync",
                    &format!("source=xset failed=true error={error}"),
                );
                self.runtime
                    .set_layout_with_reason(!self.runtime.current_layout(), "manual-correction-fallback-toggle");
            }
        }
        self.publish_status_changed()?;
        Ok(true)
    }

    fn publish_status_changed(&self) -> Result<(), SwitcherError> {
        log_layout_debug(
            "status-signal",
            &format!(
                "enabled={} current_layout={}",
                self.runtime.is_enabled(),
                if self.runtime.current_layout() { "EN" } else { "RU" }
            ),
        );
        emit_status_changed(&self.connection, &self.runtime)?;
        Ok(())
    }

    fn invalidate_word_context(&mut self) {
        self.buffer.clear();
        self.word_context.valid = false;
        self.word_context.word_before_cursor.clear();
        self.word_context.followed_by_separator = false;
    }

    fn can_correct_word_before_cursor(&self) -> bool {
        self.word_context.valid
            && self.buffer.is_empty()
            && self.word_context.followed_by_separator
            && !self.word_context.word_before_cursor.is_empty()
    }
}

fn selected_text_hotkey_matches(
    hotkey: crate::model::SelectedTextHotkey,
    modifiers: ModifierState,
    key: evdev::Key,
    value: i32,
) -> bool {
    if value != 1 || key != undo_key_to_evdev_key(hotkey.trigger_key()) {
        return false;
    }

    let shift = modifiers.is_shift_pressed();
    let ctrl = modifiers.is_ctrl_pressed();
    let alt = modifiers.is_alt_pressed();

    shift == hotkey.uses_shift() && ctrl == hotkey.uses_ctrl() && alt == hotkey.uses_alt()
}
