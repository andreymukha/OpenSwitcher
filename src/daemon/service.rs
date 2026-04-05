use crate::daemon::keyboard::{
    is_character, is_modifier, log_input_debug, undo_key_to_evdev_key, KeyboardController,
    ModifierState, SharedModifierState, INPUT_EVENT_WAIT_TIMEOUT,
};
use crate::daemon::runtime::{log_layout_debug, BackendSyncResult, RuntimeState};
use crate::daemon::selected_text::{log_selected_text_debug, SelectedTextJobRunner};
use crate::daemon::switch_logic::{manual_correction_plan, should_switch, Keystroke};
use crate::dbus::{emit_layout_switch_capture_state_changed, emit_status_changed};
use crate::error::SwitcherError;
use crate::layout_backend::{AppLayoutKind, CurrentLayoutState};
use evdev::InputEventKind;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;

const EVENT_LOOP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_LOOP_HEARTBEAT_EVENTS: u64 = 500;
const STARTUP_LAYOUT_RESYNC_MAX_ATTEMPTS: u8 = 3;

#[derive(Default)]
struct WordContext {
    valid: bool,
    word_before_cursor: Vec<Keystroke>,
    followed_by_separator: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupLayoutResyncState {
    Pending { attempts_remaining: u8 },
    Completed,
    Exhausted,
}

impl StartupLayoutResyncState {
    fn pending() -> Self {
        Self::Pending {
            attempts_remaining: STARTUP_LAYOUT_RESYNC_MAX_ATTEMPTS,
        }
    }

    fn is_pending(self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    fn complete(&mut self) {
        *self = Self::Completed;
    }

    fn record_failure(&mut self) -> u8 {
        match self {
            Self::Pending { attempts_remaining } if *attempts_remaining > 1 => {
                *attempts_remaining -= 1;
                *attempts_remaining
            }
            Self::Pending { .. } => {
                *self = Self::Exhausted;
                0
            }
            Self::Completed | Self::Exhausted => 0,
        }
    }
}

fn automatic_layout_actions_allowed(sync_result: &BackendSyncResult) -> bool {
    !matches!(sync_result, BackendSyncResult::Skipped)
}

fn next_layout_for_user_shortcut(
    sync_result: &BackendSyncResult,
    current_layout_kind: AppLayoutKind,
    legacy_layout_is_english: bool,
) -> Option<bool> {
    match sync_result {
        BackendSyncResult::Updated { .. } | BackendSyncResult::Unchanged => {
            match current_layout_kind {
                AppLayoutKind::English => Some(false),
                AppLayoutKind::Russian => Some(true),
                AppLayoutKind::Other | AppLayoutKind::Unknown => None,
            }
        }
        BackendSyncResult::Skipped => match current_layout_kind {
            AppLayoutKind::English | AppLayoutKind::Russian => Some(!legacy_layout_is_english),
            AppLayoutKind::Other | AppLayoutKind::Unknown => None,
        },
    }
}

fn should_publish_pending_status_change(has_pending_status_change: bool) -> bool {
    has_pending_status_change
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
    suppressed_undo_key: Option<evdev::Key>,
    suppressed_separator_key: Option<evdev::Key>,
    layout_shortcut_latched: bool,
    pending_auto_correction_separator: Option<evdev::Key>,
    pending_selected_text_switch: bool,
    startup_layout_resync: StartupLayoutResyncState,
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
            suppressed_undo_key: None,
            suppressed_separator_key: None,
            layout_shortcut_latched: false,
            pending_auto_correction_separator: None,
            pending_selected_text_switch: false,
            startup_layout_resync: StartupLayoutResyncState::pending(),
        })
    }

    pub fn run(&mut self) -> Result<(), SwitcherError> {
        log_input_debug("event-loop-start", "daemon input loop started");
        let mut processed_events = 0u64;
        let mut last_heartbeat = Instant::now();

        loop {
            let events = match self.keyboard.fetch_events_timeout(INPUT_EVENT_WAIT_TIMEOUT) {
                Ok(events) => events,
                Err(error) => {
                    log_input_debug("keyboard-read-error", &format!("error={error}"));
                    self.shutdown();
                    return Err(error);
                }
            };

            if should_publish_pending_status_change(self.runtime.take_pending_status_change()) {
                self.publish_status_changed()?;
            }

            if self.keyboard.take_input_target_invalidation() {
                log_input_debug(
                    "input-target-invalidation",
                    "word context invalidated by active input target change",
                );
                self.invalidate_word_context();
            }

            if self.keyboard.take_pointer_click_invalidation() {
                log_input_debug(
                    "pointer-invalidation",
                    "word context invalidated by pointer click",
                );
                self.invalidate_word_context();
            }

            for event in events {
                if let InputEventKind::Key(key) = event.kind() {
                    if let Err(error) = self.handle_key_event(key, event.value()) {
                        log_input_debug(
                            "event-handler-error",
                            &format!("key={key:?} value={} error={error}", event.value()),
                        );
                        self.shutdown();
                        return Err(error);
                    }
                    processed_events += 1;
                    if processed_events.is_multiple_of(EVENT_LOOP_HEARTBEAT_EVENTS)
                        || last_heartbeat.elapsed() >= EVENT_LOOP_HEARTBEAT_INTERVAL
                    {
                        log_input_debug(
                            "event-loop-heartbeat",
                            &format!(
                                "events_processed={processed_events} selected_text_in_progress={} writer_alive={}",
                                self.selected_text_runner.is_in_progress(),
                                self.keyboard.is_writer_alive()
                            ),
                        );
                        last_heartbeat = Instant::now();
                    }
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        log_input_debug("event-loop-stop", "daemon input loop stopping");
        self.keyboard.shutdown();
    }

    fn handle_key_event(&mut self, key: evdev::Key, value: i32) -> Result<(), SwitcherError> {
        if self.suppressed_hotkey_key == Some(key) {
            if value == 0 {
                self.suppressed_hotkey_key = None;
            }
            self.maybe_run_pending_selected_text_switch()?;
            return Ok(());
        }

        if self.suppressed_undo_key == Some(key) {
            if value == 0 {
                self.suppressed_undo_key = None;
            }
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

        if self.layout_shortcut_latched
            && !self
                .modifiers
                .keeps_layout_switch_combo_active(config.layout_switch_combo)
        {
            self.layout_shortcut_latched = false;
            log_layout_debug(
                "layout-shortcut-unlatched",
                &format!("combo={:?}", config.layout_switch_combo),
            );
        }

        if value == 1 && self.pending_auto_correction_separator.is_some() && !is_modifier(key) {
            let separator_key = self.pending_auto_correction_separator.take().unwrap();
            // Keep swallowing the physical separator release even if we had to
            // finish the correction early because the next key arrived first.
            // Otherwise a late real key-up can leak back into the normal path
            // after we already replayed the separator virtually.
            self.suppressed_separator_key = Some(separator_key);
            self.finish_pending_auto_correction(separator_key, &config)?;
            if key == undo_key_to_evdev_key(config.undo_key) {
                return Ok(());
            }
        }

        if self
            .modifiers
            .matches_layout_switch_combo(config.layout_switch_combo, key, value)
        {
            if self.layout_shortcut_latched {
                log_layout_debug(
                    "layout-shortcut-repeat-ignored",
                    &format!(
                        "combo={:?} key={key:?} value={value}",
                        config.layout_switch_combo
                    ),
                );
                return Ok(());
            }

            self.layout_shortcut_latched = true;
            let shortcut_sync = self.runtime.sync_with_backend();
            let current_layout_kind = self.current_layout_kind();
            let legacy_layout_is_english = self.runtime.current_layout();
            log_layout_debug(
                "observed-layout-shortcut",
                &format!(
                    "combo={:?} key={key:?} value={value} shift={} ctrl={} alt={} layout_before={}",
                    config.layout_switch_combo,
                    self.modifiers.is_shift_pressed(),
                    self.modifiers.is_ctrl_pressed(),
                    self.modifiers.is_alt_pressed(),
                    if self.runtime.current_layout() {
                        "EN"
                    } else {
                        "RU"
                    }
                ),
            );
            let Some(next_layout_is_english) = next_layout_for_user_shortcut(
                &shortcut_sync,
                current_layout_kind,
                legacy_layout_is_english,
            ) else {
                log_layout_debug(
                    "layout-shortcut-skip",
                    &format!("sync={shortcut_sync:?} current_layout_kind={current_layout_kind:?}"),
                );
                return Ok(());
            };
            self.runtime
                .set_layout_with_reason(next_layout_is_english, "user-layout-shortcut");
            self.startup_layout_resync.complete();
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

        if value == 0 {
            let result = self.keyboard.forward_event(key, value);
            if result.is_ok() {
                self.maybe_run_pending_selected_text_switch()?;
            }
            return result;
        }

        if value == 1 && key == undo_key_to_evdev_key(config.undo_key) {
            self.suppressed_undo_key = Some(key);
            let word_before_cursor = if self.can_correct_word_before_cursor() {
                self.word_context.word_before_cursor.clone()
            } else {
                Vec::new()
            };
            let used_current_buffer = !self.buffer.is_empty();
            if self.apply_manual_correction(&config, &word_before_cursor)? && used_current_buffer {
                self.word_context.valid = true;
                self.word_context.word_before_cursor.clear();
                self.word_context.followed_by_separator = false;
            }
            return Ok(());
        }

        match key {
            evdev::Key::KEY_SPACE => {
                let startup_sync_ready = self.refresh_startup_layout_before_autocorrect()?;
                let features = self.runtime.feature_availability();
                let corrected = self.runtime.is_enabled()
                    && features.auto_switch
                    && startup_sync_ready
                    && matches!(self.current_layout_kind(), AppLayoutKind::English)
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
                self.keyboard.forward_event(key, value)
            }
            evdev::Key::KEY_ENTER | evdev::Key::KEY_TAB => {
                self.invalidate_word_context();
                self.keyboard.forward_event(key, value)
            }
            evdev::Key::KEY_BACKSPACE => {
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                } else if self.word_context.valid && self.word_context.followed_by_separator {
                    self.buffer = self.word_context.word_before_cursor.clone();
                    self.word_context.followed_by_separator = false;
                }
                self.keyboard.forward_event(key, value)
            }
            _ => {
                let plain_character_input = is_character(key)
                    && !self.modifiers.is_ctrl_pressed()
                    && !self.modifiers.is_alt_pressed()
                    && !self.modifiers.is_meta_pressed();

                if plain_character_input {
                    // Once we are typing the current word again, the cursor is no longer
                    // "after a finished word". Keep only the active buffer state.
                    self.word_context.valid = true;
                    self.word_context.followed_by_separator = false;
                    self.word_context.word_before_cursor.clear();
                    let stroke = Keystroke {
                        key,
                        shift: self.modifiers.is_shift_pressed(),
                    };
                    self.buffer.push(stroke);
                } else if !is_modifier(key) {
                    self.invalidate_word_context();
                }
                let result = self.keyboard.forward_event(key, value);
                if result.is_ok() {
                    self.maybe_run_pending_selected_text_switch()?;
                }
                result
            }
        }
    }

    fn apply_selected_text_switch(&mut self) -> Result<(), SwitcherError> {
        if !self.runtime.feature_availability().selected_text_switch {
            log_selected_text_debug(
                "hotkey-skip",
                "selected-text switching disabled by backend policy",
            );
            return Ok(());
        }

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
        let features = self.runtime.feature_availability();
        if !features.manual_word_fix {
            return Ok(false);
        }

        let pre_correction_sync = self.runtime.sync_with_backend();
        if !automatic_layout_actions_allowed(&pre_correction_sync) {
            log_layout_debug(
                "manual-correction-sync",
                "source=backend skipped=true phase=before-correction",
            );
            return Ok(false);
        }

        if !matches!(
            self.current_layout_kind(),
            AppLayoutKind::English | AppLayoutKind::Russian
        ) {
            return Ok(false);
        }

        let Some(plan) = manual_correction_plan(
            &self.buffer,
            fallback_buffer,
            self.word_context.followed_by_separator,
        ) else {
            return Ok(false);
        };
        self.keyboard
            .apply_correction(&plan, config, self.modifiers)?;
        match self.runtime.sync_with_backend() {
            BackendSyncResult::Updated { current, .. } => {
                log_layout_debug(
                    "manual-correction-sync",
                    &format!("source=backend updated=true current={current:?}"),
                );
                self.startup_layout_resync.complete();
                self.publish_status_changed()?;
            }
            BackendSyncResult::Unchanged => {
                log_layout_debug(
                    "manual-correction-sync",
                    "source=backend updated=false current=unchanged",
                );
                self.startup_layout_resync.complete();
                self.publish_status_changed()?;
            }
            BackendSyncResult::Skipped => {
                log_layout_debug(
                    "manual-correction-sync",
                    "source=backend skipped=true phase=after-correction fallback=disabled",
                );
            }
        }
        Ok(true)
    }

    fn publish_status_changed(&self) -> Result<(), SwitcherError> {
        log_layout_debug(
            "status-signal",
            &format!(
                "enabled={} current_layout={}",
                self.runtime.is_enabled(),
                if self.runtime.current_layout() {
                    "EN"
                } else {
                    "RU"
                }
            ),
        );
        emit_status_changed(&self.connection, &self.runtime)?;
        self.runtime.clear_pending_status_change();
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

    fn refresh_startup_layout_before_autocorrect(&mut self) -> Result<bool, SwitcherError> {
        if !self.startup_layout_resync.is_pending() {
            return Ok(true);
        }

        let layout_before = self.runtime.current_layout();

        match self.runtime.sync_with_backend() {
            BackendSyncResult::Updated { current, .. } => {
                self.startup_layout_resync.complete();
                log_layout_debug(
                    "startup-resync",
                    &format!("source=backend updated=true current={current:?}"),
                );
                if self.runtime.current_layout() != layout_before {
                    self.publish_status_changed()?;
                }
                Ok(true)
            }
            BackendSyncResult::Unchanged => {
                self.startup_layout_resync.complete();
                log_layout_debug(
                    "startup-resync",
                    "source=backend updated=false current=unchanged",
                );
                Ok(true)
            }
            BackendSyncResult::Skipped => {
                let attempts_remaining = self.startup_layout_resync.record_failure();
                log_layout_debug(
                    "startup-resync",
                    &format!("source=backend skipped=true attempts_remaining={attempts_remaining}"),
                );
                Ok(false)
            }
        }
    }

    fn current_layout_kind(&self) -> AppLayoutKind {
        match self.runtime.current_layout_state() {
            CurrentLayoutState::Known { layout, .. } => layout.kind,
            CurrentLayoutState::Unknown { .. } => AppLayoutKind::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_layout_resync_starts_pending() {
        let state = StartupLayoutResyncState::pending();

        assert_eq!(
            state,
            StartupLayoutResyncState::Pending {
                attempts_remaining: STARTUP_LAYOUT_RESYNC_MAX_ATTEMPTS,
            }
        );
        assert!(state.is_pending());
    }

    #[test]
    fn startup_layout_resync_failures_decrement_until_exhausted() {
        let mut state = StartupLayoutResyncState::pending();

        assert_eq!(
            state.record_failure(),
            STARTUP_LAYOUT_RESYNC_MAX_ATTEMPTS - 1
        );
        assert_eq!(
            state,
            StartupLayoutResyncState::Pending {
                attempts_remaining: STARTUP_LAYOUT_RESYNC_MAX_ATTEMPTS - 1,
            }
        );

        assert_eq!(state.record_failure(), 1);
        assert_eq!(
            state,
            StartupLayoutResyncState::Pending {
                attempts_remaining: 1,
            }
        );

        assert_eq!(state.record_failure(), 0);
        assert_eq!(state, StartupLayoutResyncState::Exhausted);
        assert!(!state.is_pending());
    }

    #[test]
    fn startup_layout_resync_completion_stops_future_retries() {
        let mut state = StartupLayoutResyncState::pending();

        state.complete();

        assert_eq!(state, StartupLayoutResyncState::Completed);
        assert!(!state.is_pending());
        assert_eq!(state.record_failure(), 0);
        assert_eq!(state, StartupLayoutResyncState::Completed);
    }

    #[test]
    fn automatic_layout_actions_are_blocked_when_backend_sync_is_skipped() {
        assert!(!automatic_layout_actions_allowed(
            &BackendSyncResult::Skipped
        ));
        assert!(automatic_layout_actions_allowed(
            &BackendSyncResult::Unchanged
        ));
    }

    #[test]
    fn shortcut_fallback_is_allowed_only_for_known_en_ru_layouts() {
        assert_eq!(
            next_layout_for_user_shortcut(
                &BackendSyncResult::Skipped,
                AppLayoutKind::English,
                true,
            ),
            Some(false)
        );
        assert_eq!(
            next_layout_for_user_shortcut(
                &BackendSyncResult::Skipped,
                AppLayoutKind::Russian,
                false,
            ),
            Some(true)
        );
        assert_eq!(
            next_layout_for_user_shortcut(
                &BackendSyncResult::Skipped,
                AppLayoutKind::Unknown,
                true,
            ),
            None
        );
    }

    #[test]
    fn shortcut_does_not_guess_layout_for_other_when_sync_succeeds() {
        assert_eq!(
            next_layout_for_user_shortcut(
                &BackendSyncResult::Unchanged,
                AppLayoutKind::Other,
                true,
            ),
            None
        );
    }

    #[test]
    fn pending_status_change_requests_publish_from_service() {
        assert!(should_publish_pending_status_change(true));
        assert!(!should_publish_pending_status_change(false));
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
