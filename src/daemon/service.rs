use crate::daemon::keyboard::{
    is_character, is_modifier, is_russian_layout, undo_key_to_evdev_key, KeyboardController,
    ModifierState,
};
use crate::daemon::runtime::RuntimeState;
use crate::daemon::switch_logic::{manual_correction_plan, should_switch, Keystroke};
use crate::dbus::{emit_layout_switch_capture_state_changed, emit_status_changed};
use crate::error::SwitcherError;
use evdev::InputEventKind;
use std::sync::Arc;
use zbus::blocking::Connection;

pub struct DaemonService {
    runtime: Arc<RuntimeState>,
    connection: Connection,
    keyboard: KeyboardController,
    modifiers: ModifierState,
    buffer: Vec<Keystroke>,
    last_word_buffer: Vec<Keystroke>,
}

impl DaemonService {
    pub fn new(runtime: Arc<RuntimeState>, connection: Connection) -> Result<Self, SwitcherError> {
        Ok(Self {
            runtime,
            connection,
            keyboard: KeyboardController::open()?,
            modifiers: ModifierState::default(),
            buffer: Vec::new(),
            last_word_buffer: Vec::new(),
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
        if self.runtime.is_capture_active()? {
            self.modifiers.update(key, value);

            if let Some(state) = self.runtime.handle_capture_key_event(key, value)? {
                emit_layout_switch_capture_state_changed(&self.connection, &state)?;
            }

            if !self.runtime.is_capture_active()? {
                self.buffer.clear();
                self.last_word_buffer.clear();
            }

            return Ok(());
        }

        let config = self.runtime.config_snapshot()?;
        self.modifiers.update(key, value);

        if self.modifiers.should_toggle_layout_shortcut(key, value) {
            self.runtime.set_layout(!self.runtime.current_layout());
            self.publish_status_changed()?;
        }

        if value != 1 {
            return self.keyboard.forward_event(key, value);
        }

        if key == undo_key_to_evdev_key(config.undo_key) {
            let last_word_buffer = self.last_word_buffer.clone();
            if self.apply_manual_correction(&config, &last_word_buffer)? && !self.buffer.is_empty()
            {
                self.last_word_buffer = self.buffer.clone();
                self.buffer.clear();
            }
            return Ok(());
        }

        match key {
            evdev::Key::KEY_SPACE | evdev::Key::KEY_ENTER | evdev::Key::KEY_TAB => {
                let is_russian = self.refresh_runtime_layout()?;

                if self.runtime.is_enabled() && !is_russian && should_switch(&self.buffer) {
                    self.apply_manual_correction(&config, &[])?;
                }

                self.last_word_buffer = self.buffer.clone();
                self.buffer.clear();
                self.keyboard.forward_event(key, 1)
            }
            evdev::Key::KEY_BACKSPACE => {
                self.buffer.pop();
                self.keyboard.forward_event(key, 1)
            }
            _ => {
                if is_character(key) {
                    self.buffer.push(Keystroke {
                        key,
                        shift: self.modifiers.is_shift_pressed(),
                    });
                } else if !is_modifier(key) {
                    self.buffer.clear();
                }
                self.keyboard.forward_event(key, 1)
            }
        }
    }

    fn apply_manual_correction(
        &mut self,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
        fallback_buffer: &[Keystroke],
    ) -> Result<bool, SwitcherError> {
        let Some(plan) = manual_correction_plan(&self.buffer, fallback_buffer) else {
            return Ok(false);
        };

        self.keyboard
            .apply_correction(&plan, config, self.modifiers)?;
        self.refresh_runtime_layout()?;
        self.publish_status_changed()?;
        Ok(true)
    }

    fn refresh_runtime_layout(&self) -> Result<bool, SwitcherError> {
        let is_russian = is_russian_layout()?;
        self.runtime.set_layout(!is_russian);
        Ok(is_russian)
    }

    fn publish_status_changed(&self) -> Result<(), SwitcherError> {
        emit_status_changed(&self.connection, &self.runtime)?;
        Ok(())
    }
}
