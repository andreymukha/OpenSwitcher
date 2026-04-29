use crate::daemon::input_backend::{
    ActiveInputBackend, InputBackendLifecycle, KeyboardInputBackendOpener, OpenedInputBackend,
};
use crate::daemon::keyboard::{
    is_character, is_modifier, log_input_debug, undo_key_to_evdev_key, KeyboardController,
    ManualCurrentWordCompletion, ManualCurrentWordOutcome, ManualCurrentWordStartOutcome,
    ModifierState, SharedModifierState, INPUT_EVENT_WAIT_TIMEOUT,
};
use crate::daemon::runtime::{log_layout_debug, BackendSyncResult, RuntimeState};
use crate::daemon::selected_text::{log_selected_text_debug, SelectedTextJobRunner};
use crate::daemon::switch_logic::{
    apply_case_fixes_to_strokes, manual_correction_plan, same_layout_case_correction_plan,
    should_switch, CorrectionPlan, Keystroke,
};
use crate::dbus::{emit_layout_switch_capture_state_changed, emit_status_changed};
use crate::error::SwitcherError;
use crate::layout_backend::{AppLayoutKind, CurrentLayoutState};
use evdev::InputEventKind;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use zbus::blocking::Connection;

const EVENT_LOOP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_LOOP_HEARTBEAT_EVENTS: u64 = 500;
const STARTUP_LAYOUT_RESYNC_MAX_ATTEMPTS: u8 = 3;
const MANUAL_CURRENT_WORD_IN_FLIGHT_POLL_TIMEOUT: Duration = Duration::from_millis(10);
const MAX_DEFERRED_MANUAL_INPUT_EVENTS: usize = 256;

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct WordContext {
    valid: bool,
    word_before_cursor: Vec<Keystroke>,
    followed_by_separator: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingWordCommitAction {
    LayoutCorrection,
    SameLayoutCaseCorrection { corrected_buffer: Vec<Keystroke> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CorrectionPath {
    AutoWordCommit,
    ManualHotkey,
}

impl CorrectionPath {
    fn as_str(self) -> &'static str {
        match self {
            Self::AutoWordCommit => "auto-word-commit",
            Self::ManualHotkey => "manual-hotkey",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingWordCommit {
    separator_key: evdev::Key,
    action: PendingWordCommitAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AppliedManualCorrection {
    corrected_buffer: Vec<Keystroke>,
    used_current_buffer: bool,
    extra_backspaces: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedManualCorrection {
    plan: CorrectionPlan,
    used_current_buffer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputOrigin {
    Physical,
    DeferredReplay,
    DeferredRetry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeferredInputEvent {
    key: evdev::Key,
    value: i32,
    timestamp: SystemTime,
}

#[derive(Clone, Debug)]
struct DeferredManualCurrentWordSession {
    request_id: u64,
    undo_key: evdev::Key,
    _frozen_modifiers: ModifierState,
    deferred_input: VecDeque<DeferredInputEvent>,
    seen_real_next_step: bool,
    retry_after_drain_requested: bool,
    started_at: Instant,
    drained_events: usize,
}

#[derive(Clone, Debug)]
enum ManualCurrentWordFlow {
    Idle,
    InFlight { session: DeferredManualCurrentWordSession },
    DrainingDeferredInput { session: DeferredManualCurrentWordSession },
}

fn deferred_manual_current_word_flow_label(flow: &ManualCurrentWordFlow) -> &'static str {
    match flow {
        ManualCurrentWordFlow::Idle => "idle",
        ManualCurrentWordFlow::InFlight { .. } => "in-flight",
        ManualCurrentWordFlow::DrainingDeferredInput { .. } => "draining",
    }
}

fn should_restart_manual_current_word_after_drain(
    deferred_len: usize,
    retry_after_drain_requested: bool,
) -> bool {
    deferred_len == 0 && retry_after_drain_requested
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

fn auto_layout_correction_supported_for_layout(layout_kind: AppLayoutKind) -> bool {
    matches!(layout_kind, AppLayoutKind::English | AppLayoutKind::Russian)
}

fn format_legacy_layout(is_english: bool) -> &'static str {
    if is_english {
        "EN"
    } else {
        "RU"
    }
}

fn pending_word_commit_action_label(pending: Option<&PendingWordCommit>) -> &'static str {
    match pending.map(|pending| &pending.action) {
        Some(PendingWordCommitAction::LayoutCorrection) => "layout-correction",
        Some(PendingWordCommitAction::SameLayoutCaseCorrection { .. }) => {
            "same-layout-correction"
        }
        None => "none",
    }
}

fn pending_word_commit_separator_key(
    pending: Option<&PendingWordCommit>,
) -> Option<evdev::Key> {
    pending.map(|pending| pending.separator_key)
}

fn manual_separator_replay_key(
    used_current_buffer: bool,
    extra_backspaces: usize,
) -> Option<evdev::Key> {
    if !used_current_buffer && extra_backspaces > 0 {
        Some(evdev::Key::KEY_SPACE)
    } else {
        None
    }
}

fn should_swallow_suppressed_separator_release(
    suppressed_separator_key: Option<evdev::Key>,
    key: evdev::Key,
    value: i32,
) -> bool {
    suppressed_separator_key == Some(key) && value == 0
}

fn preserved_separator_after_early_finish(
    pending_word_commit: Option<&PendingWordCommit>,
    key: evdev::Key,
    value: i32,
) -> Option<evdev::Key> {
    if value == 1 && !is_modifier(key) {
        pending_word_commit.map(|pending| pending.separator_key)
    } else {
        None
    }
}

fn should_commit_manually_corrected_current_word(
    current_word_correction_state: CurrentWordCorrectionState,
    key: evdev::Key,
    buffer_len: usize,
) -> bool {
    matches!(
        current_word_correction_state,
        CurrentWordCorrectionState::ManuallyCorrected
    )
        && buffer_len > 0
        && matches!(
            key,
            evdev::Key::KEY_SPACE | evdev::Key::KEY_TAB | evdev::Key::KEY_ENTER
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CurrentWordCorrectionState {
    Raw,
    ManuallyCorrected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManualHotkeyLatch {
    key: evdev::Key,
    armed_at: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManualCurrentWordPhysicalEventAction {
    ProcessImmediately,
    Swallow,
    RequestRetryAfterDrain,
    Enqueue { marks_real_next_step: bool },
}

fn next_current_word_state_after_plain_character_input(
    current_word_correction_state: CurrentWordCorrectionState,
) -> CurrentWordCorrectionState {
    current_word_correction_state
}

fn should_swallow_manual_hotkey_latched_event(
    manual_hotkey_latch: Option<ManualHotkeyLatch>,
    key: evdev::Key,
    value: i32,
) -> bool {
    manual_hotkey_latch.is_some_and(|latch| latch.key == key) && matches!(value, 0..=2)
}

fn should_swallow_suppressed_undo_release(
    suppressed_undo_key: Option<evdev::Key>,
    key: evdev::Key,
    value: i32,
) -> bool {
    suppressed_undo_key == Some(key) && value == 0
}

fn manual_current_word_physical_event_action(
    flow_active: bool,
    undo_key: evdev::Key,
    seen_real_next_step: bool,
    key: evdev::Key,
    value: i32,
    origin: InputOrigin,
) -> ManualCurrentWordPhysicalEventAction {
    if !flow_active || !matches!(origin, InputOrigin::Physical) {
        return ManualCurrentWordPhysicalEventAction::ProcessImmediately;
    }

    if key == undo_key && matches!(value, 0..=2) {
        if value == 1 && seen_real_next_step {
            return ManualCurrentWordPhysicalEventAction::RequestRetryAfterDrain;
        }
        return ManualCurrentWordPhysicalEventAction::Swallow;
    }

    ManualCurrentWordPhysicalEventAction::Enqueue {
        marks_real_next_step: value == 1 && key != undo_key && !is_modifier(key),
    }
}

fn should_clear_manual_hotkey_latch_on_key_press(
    manual_hotkey_latch: Option<ManualHotkeyLatch>,
    undo_key: evdev::Key,
    key: evdev::Key,
    value: i32,
    event_timestamp: SystemTime,
) -> bool {
    manual_hotkey_latch.is_some_and(|latch| {
        value == 1
            && key != undo_key
            && !is_modifier(key)
            && event_timestamp > latch.armed_at
    })
}

fn next_manual_hotkey_latch_after_manual_correction(
    undo_key: evdev::Key,
    correction_path: CorrectionPath,
    used_current_buffer: bool,
    armed_at: SystemTime,
) -> Option<ManualHotkeyLatch> {
    if matches!(correction_path, CorrectionPath::ManualHotkey) && used_current_buffer {
        Some(ManualHotkeyLatch {
            key: undo_key,
            armed_at,
        })
    } else {
        None
    }
}

fn update_word_context_after_manual_correction(
    word_context: &mut WordContext,
    corrected_buffer: &[Keystroke],
    used_current_buffer: bool,
    extra_backspaces: usize,
) {
    word_context.valid = !corrected_buffer.is_empty();
    word_context.word_before_cursor = corrected_buffer.to_vec();
    if used_current_buffer {
        word_context.followed_by_separator = false;
    } else {
        word_context.followed_by_separator = extra_backspaces > 0;
    }
}

fn finalize_manual_correction(
    buffer: &mut Vec<Keystroke>,
    word_context: &mut WordContext,
    applied: &AppliedManualCorrection,
) -> Option<evdev::Key> {
    if applied.used_current_buffer {
        *buffer = applied.corrected_buffer.clone();
    } else {
        buffer.clear();
    }
    update_word_context_after_manual_correction(
        word_context,
        &applied.corrected_buffer,
        applied.used_current_buffer,
        applied.extra_backspaces,
    );
    manual_separator_replay_key(applied.used_current_buffer, applied.extra_backspaces)
}

fn should_abort_manual_current_word_flow_on_queue_overflow(
    deferred_len: usize,
    limit: usize,
) -> bool {
    deferred_len >= limit
}

fn clear_word_context_state(
    buffer: &mut Vec<Keystroke>,
    word_context: &mut WordContext,
    current_word_correction_state: &mut CurrentWordCorrectionState,
    manual_hotkey_latch: &mut Option<ManualHotkeyLatch>,
) {
    buffer.clear();
    *current_word_correction_state = CurrentWordCorrectionState::Raw;
    *manual_hotkey_latch = None;
    word_context.valid = false;
    word_context.word_before_cursor.clear();
    word_context.followed_by_separator = false;
}

pub struct DaemonService {
    runtime: Arc<RuntimeState>,
    connection: Connection,
    input_backend: InputBackendLifecycle<KeyboardInputBackendOpener>,
    keyboard: Option<KeyboardController>,
    modifiers: ModifierState,
    shared_modifiers: SharedModifierState,
    buffer: Vec<Keystroke>,
    current_word_correction_state: CurrentWordCorrectionState,
    word_context: WordContext,
    selected_text_runner: Option<SelectedTextJobRunner>,
    suppressed_hotkey_key: Option<evdev::Key>,
    suppressed_undo_key: Option<evdev::Key>,
    manual_hotkey_latch: Option<ManualHotkeyLatch>,
    manual_current_word_flow: ManualCurrentWordFlow,
    suppressed_separator_key: Option<evdev::Key>,
    layout_shortcut_latched: bool,
    pending_word_commit: Option<PendingWordCommit>,
    pending_selected_text_switch: bool,
    startup_layout_resync: StartupLayoutResyncState,
}

impl DaemonService {
    pub fn new(runtime: Arc<RuntimeState>, connection: Connection) -> Result<Self, SwitcherError> {
        let shared_modifiers = SharedModifierState::default();
        let mut service = Self {
            runtime,
            connection,
            input_backend: InputBackendLifecycle::new(KeyboardInputBackendOpener),
            keyboard: None,
            modifiers: ModifierState::default(),
            shared_modifiers,
            buffer: Vec::new(),
            current_word_correction_state: CurrentWordCorrectionState::Raw,
            word_context: WordContext::default(),
            selected_text_runner: None,
            suppressed_hotkey_key: None,
            suppressed_undo_key: None,
            manual_hotkey_latch: None,
            manual_current_word_flow: ManualCurrentWordFlow::Idle,
            suppressed_separator_key: None,
            layout_shortcut_latched: false,
            pending_word_commit: None,
            pending_selected_text_switch: false,
            startup_layout_resync: StartupLayoutResyncState::pending(),
        };
        service.try_initialize_input_backend()?;
        Ok(service)
    }

    pub fn run(&mut self) -> Result<(), SwitcherError> {
        log_input_debug("event-loop-start", "daemon input loop started");
        let mut processed_events = 0u64;
        let mut last_heartbeat = Instant::now();

        'event_loop: loop {
            if self.runtime.should_exit() {
                self.shutdown();
                return Ok(());
            }

            self.maybe_retry_input_backend()?;
            self.poll_manual_current_word_completion()?;

            let fetch_timeout = self.event_fetch_timeout();
            let events = if let Some(keyboard) = self.keyboard.as_mut() {
                match keyboard.fetch_events_timeout(fetch_timeout) {
                    Ok(events) => events,
                    Err(error) => {
                        log_input_debug("keyboard-read-error", &format!("error={error}"));
                        if self.handle_runtime_input_failure(&error) {
                            continue;
                        }
                        self.shutdown();
                        return Err(error);
                    }
                }
            } else {
                std::thread::sleep(INPUT_EVENT_WAIT_TIMEOUT);
                Vec::new()
            };

            if should_publish_pending_status_change(self.runtime.take_pending_status_change()) {
                self.publish_status_changed()?;
            }

            if self
                .keyboard
                .as_ref()
                .is_some_and(KeyboardController::take_input_target_invalidation)
            {
                self.handle_non_key_invalidation(
                    "input-target-invalidation",
                    "word context invalidated by active input target change",
                );
            }

            if self
                .keyboard
                .as_ref()
                .is_some_and(KeyboardController::take_pointer_click_invalidation)
            {
                self.handle_non_key_invalidation(
                    "pointer-invalidation",
                    "word context invalidated by pointer click",
                );
            }

            for event in events {
                if let InputEventKind::Key(key) = event.kind() {
                    if let Err(error) =
                        self.handle_key_event(key, event.value(), event.timestamp(), InputOrigin::Physical)
                    {
                        log_input_debug(
                            "event-handler-error",
                            &format!("key={key:?} value={} error={error}", event.value()),
                        );
                        if self.handle_runtime_input_failure(&error) {
                            continue 'event_loop;
                        }
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
                                self.selected_text_runner
                                    .as_ref()
                                    .is_some_and(SelectedTextJobRunner::is_in_progress),
                                self.keyboard
                                    .as_ref()
                                    .is_some_and(KeyboardController::is_writer_alive)
                            ),
                        );
                        last_heartbeat = Instant::now();
                    }
                }
            }

            self.drain_one_deferred_input_event()?;
        }
    }

    pub fn shutdown(&mut self) {
        log_input_debug("event-loop-stop", "daemon input loop stopping");
        self.drop_active_input_backend();
    }

    fn event_fetch_timeout(&self) -> Duration {
        match self.manual_current_word_flow {
            ManualCurrentWordFlow::Idle => INPUT_EVENT_WAIT_TIMEOUT,
            ManualCurrentWordFlow::InFlight { .. } => MANUAL_CURRENT_WORD_IN_FLIGHT_POLL_TIMEOUT,
            ManualCurrentWordFlow::DrainingDeferredInput { .. } => Duration::ZERO,
        }
    }

    fn handle_non_key_invalidation(&mut self, stage: &str, message: &str) {
        log_input_debug(stage, message);
        if self.has_active_manual_current_word_flow() {
            self.abort_manual_current_word_flow(stage);
        } else {
            self.invalidate_word_context();
        }
    }

    fn has_active_manual_current_word_flow(&self) -> bool {
        !matches!(self.manual_current_word_flow, ManualCurrentWordFlow::Idle)
    }

    fn poll_manual_current_word_completion(&mut self) -> Result<(), SwitcherError> {
        let Some(keyboard) = self.keyboard.as_mut() else {
            return Ok(());
        };

        let Some(completion) = keyboard.poll_manual_current_word_completion()? else {
            return Ok(());
        };

        self.handle_manual_current_word_completion(completion)
    }

    fn handle_manual_current_word_completion(
        &mut self,
        completion: ManualCurrentWordCompletion,
    ) -> Result<(), SwitcherError> {
        let session = match std::mem::replace(
            &mut self.manual_current_word_flow,
            ManualCurrentWordFlow::Idle,
        ) {
            ManualCurrentWordFlow::InFlight { session } if session.request_id == completion.request_id => session,
            other => {
                self.manual_current_word_flow = other;
                return Ok(());
            }
        };

        match completion.outcome {
            ManualCurrentWordOutcome::Succeeded(plan) => {
                log_input_debug(
                    "manual-current-word-completion",
                    &format!(
                        "request_id={} outcome=success elapsed_ms={} deferred_len={} seen_real_next_step={} retry_after_drain_requested={} buffer_len={} extra_backspaces={}",
                        completion.request_id,
                        session.started_at.elapsed().as_millis(),
                        session.deferred_input.len(),
                        session.seen_real_next_step,
                        session.retry_after_drain_requested,
                        plan.buffer.len(),
                        plan.extra_backspaces,
                    ),
                );
                let applied = AppliedManualCorrection {
                    corrected_buffer: plan.buffer,
                    used_current_buffer: true,
                    extra_backspaces: plan.extra_backspaces,
                };
                let _ = finalize_manual_correction(&mut self.buffer, &mut self.word_context, &applied);
                self.current_word_correction_state = CurrentWordCorrectionState::ManuallyCorrected;
                self.manual_hotkey_latch = None;
                self.manual_current_word_flow =
                    ManualCurrentWordFlow::DrainingDeferredInput { session };
                Ok(())
            }
            ManualCurrentWordOutcome::FailedAfterMutation(error) => {
                log_input_debug(
                    "manual-current-word-completion",
                    &format!(
                        "request_id={} outcome=failed-after-mutation elapsed_ms={} deferred_len={} seen_real_next_step={} retry_after_drain_requested={} error={error}",
                        completion.request_id,
                        session.started_at.elapsed().as_millis(),
                        session.deferred_input.len(),
                        session.seen_real_next_step,
                        session.retry_after_drain_requested,
                    ),
                );
                self.abort_manual_current_word_flow("manual-current-word-failed-after-mutation");
                Ok(())
            }
        }
    }

    fn abort_manual_current_word_flow(&mut self, reason: &str) {
        let flow = std::mem::replace(&mut self.manual_current_word_flow, ManualCurrentWordFlow::Idle);
        let details = match &flow {
            ManualCurrentWordFlow::InFlight { session }
            | ManualCurrentWordFlow::DrainingDeferredInput { session } => format!(
                "reason={reason} state={} request_id={} deferred_len={} seen_real_next_step={} retry_after_drain_requested={} drained_events={} elapsed_ms={}",
                deferred_manual_current_word_flow_label(&flow),
                session.request_id,
                session.deferred_input.len(),
                session.seen_real_next_step,
                session.retry_after_drain_requested,
                session.drained_events,
                session.started_at.elapsed().as_millis(),
            ),
            ManualCurrentWordFlow::Idle => format!("reason={reason} state=idle"),
        };
        log_input_debug("manual-current-word-abort", &details);
        self.manual_current_word_flow = ManualCurrentWordFlow::Idle;
        self.reset_transient_input_state(reason);
    }

    fn drain_one_deferred_input_event(&mut self) -> Result<(), SwitcherError> {
        let event = match &mut self.manual_current_word_flow {
            ManualCurrentWordFlow::DrainingDeferredInput { session } => {
                let event = session.deferred_input.pop_front();
                if event.is_some() {
                    session.drained_events += 1;
                }
                event
            }
            _ => None,
        };

        let Some(event) = event else {
            if let ManualCurrentWordFlow::DrainingDeferredInput { session } = &self.manual_current_word_flow {
                if should_restart_manual_current_word_after_drain(
                    session.deferred_input.len(),
                    session.retry_after_drain_requested,
                ) {
                    let undo_key = session.undo_key;
                    self.manual_current_word_flow = ManualCurrentWordFlow::Idle;
                    let config = self.runtime.config_snapshot()?;
                    if self.begin_deferred_manual_current_word_correction(
                        undo_key,
                        &config,
                        InputOrigin::DeferredRetry,
                    )? {
                        return Ok(());
                    }
                }
                self.manual_current_word_flow = ManualCurrentWordFlow::Idle;
            }
            return Ok(());
        };

        self.handle_key_event(event.key, event.value, event.timestamp, InputOrigin::DeferredReplay)
    }

    fn manual_current_word_flow_seen_real_next_step(&self) -> bool {
        match &self.manual_current_word_flow {
            ManualCurrentWordFlow::InFlight { session }
            | ManualCurrentWordFlow::DrainingDeferredInput { session } => session.seen_real_next_step,
            ManualCurrentWordFlow::Idle => false,
        }
    }

    fn enqueue_deferred_physical_input_event(
        &mut self,
        key: evdev::Key,
        value: i32,
        event_timestamp: SystemTime,
        marks_real_next_step: bool,
    ) -> Result<(), SwitcherError> {
        let session = match &mut self.manual_current_word_flow {
            ManualCurrentWordFlow::InFlight { session } => session,
            ManualCurrentWordFlow::DrainingDeferredInput { session } => session,
            ManualCurrentWordFlow::Idle => return Ok(()),
        };

        if marks_real_next_step {
            session.seen_real_next_step = true;
        }

        if should_abort_manual_current_word_flow_on_queue_overflow(
            session.deferred_input.len(),
            MAX_DEFERRED_MANUAL_INPUT_EVENTS,
        ) {
            self.abort_manual_current_word_flow("manual-current-word-deferred-overflow");
            return Ok(());
        }

        session.deferred_input.push_back(DeferredInputEvent {
            key,
            value,
            timestamp: event_timestamp,
        });
        Ok(())
    }

    fn request_manual_current_word_retry_after_drain(&mut self, _key: evdev::Key) {
        let session = match &mut self.manual_current_word_flow {
            ManualCurrentWordFlow::InFlight { session } => session,
            ManualCurrentWordFlow::DrainingDeferredInput { session } => session,
            ManualCurrentWordFlow::Idle => return,
        };

        if session.retry_after_drain_requested {
            return;
        }

        session.retry_after_drain_requested = true;
    }

    fn handle_key_event(
        &mut self,
        key: evdev::Key,
        value: i32,
        event_timestamp: SystemTime,
        origin: InputOrigin,
    ) -> Result<(), SwitcherError> {
        if self.suppressed_hotkey_key == Some(key) {
            if value == 0 {
                self.suppressed_hotkey_key = None;
            }
            self.maybe_run_pending_selected_text_switch()?;
            return Ok(());
        }

        if should_swallow_suppressed_undo_release(self.suppressed_undo_key, key, value) {
            self.suppressed_undo_key = None;
            return Ok(());
        }

        if self.suppressed_undo_key == Some(key) {
            if value == 0 {
                self.suppressed_undo_key = None;
            }
            return Ok(());
        }

        match manual_current_word_physical_event_action(
            self.has_active_manual_current_word_flow(),
            undo_key_to_evdev_key(self.runtime.config_snapshot()?.undo_key),
            self.manual_current_word_flow_seen_real_next_step(),
            key,
            value,
            origin,
        ) {
            ManualCurrentWordPhysicalEventAction::ProcessImmediately => {}
            ManualCurrentWordPhysicalEventAction::Swallow => return Ok(()),
            ManualCurrentWordPhysicalEventAction::RequestRetryAfterDrain => {
                self.request_manual_current_word_retry_after_drain(key);
                return Ok(());
            }
            ManualCurrentWordPhysicalEventAction::Enqueue {
                marks_real_next_step,
            } => {
                self.enqueue_deferred_physical_input_event(
                    key,
                    value,
                    event_timestamp,
                    marks_real_next_step,
                )?;
                return Ok(());
            }
        }

        if should_swallow_suppressed_separator_release(self.suppressed_separator_key, key, value) {
            log_input_debug(
                "separator-release-swallow",
                &format!(
                    "key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                    self.suppressed_separator_key,
                    pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                    pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                    self.buffer.len(),
                    self.word_context.followed_by_separator,
                ),
            );
            if value == 0 {
                if self
                    .pending_word_commit
                    .as_ref()
                    .is_some_and(|pending| pending.separator_key == key)
                {
                    log_input_debug(
                        "pending-word-commit-take",
                        &format!(
                            "reason=separator-release key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            self.suppressed_separator_key,
                            pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                            pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    let pending = self.pending_word_commit.take().unwrap();
                    self.finish_pending_word_commit(pending, &self.runtime.config_snapshot()?)?;
                }
                log_input_debug(
                    "suppressed-separator-clear",
                    &format!(
                        "reason=separator-release key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                        self.suppressed_separator_key,
                        pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                        pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                        self.buffer.len(),
                        self.word_context.followed_by_separator,
                    ),
                );
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

        let undo_key = undo_key_to_evdev_key(config.undo_key);
        if should_clear_manual_hotkey_latch_on_key_press(
            self.manual_hotkey_latch,
            undo_key,
            key,
            value,
            event_timestamp,
        ) {
            self.manual_hotkey_latch = None;
        }

        if should_swallow_manual_hotkey_latched_event(self.manual_hotkey_latch, key, value) {
            return Ok(());
        }

        if value == 1 && self.pending_word_commit.is_some() && !is_modifier(key) {
            log_input_debug(
                "pending-word-commit-take",
                &format!(
                    "reason=early-finish key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                    self.suppressed_separator_key,
                    pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                    pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                    self.buffer.len(),
                    self.word_context.followed_by_separator,
                ),
            );
            let pending = self.pending_word_commit.take().unwrap();
            // Keep swallowing the physical separator release even if we had to
            // finish the correction early because the next key arrived first.
            // Otherwise a late real key-up can leak back into the normal path
            // after we already replayed the separator virtually.
            let preserved_separator =
                preserved_separator_after_early_finish(Some(&pending), key, value);
            log_input_debug(
                "suppressed-separator-set",
                &format!(
                    "reason=early-finish key={key:?} value={value} next_suppressed_separator_key={preserved_separator:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                    pending_word_commit_action_label(Some(&pending)),
                    Some(pending.separator_key),
                    self.buffer.len(),
                    self.word_context.followed_by_separator,
                ),
            );
            self.suppressed_separator_key = preserved_separator;
            self.finish_pending_word_commit(pending, &config)?;
            if key == undo_key {
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
            let result = self.keyboard_mut()?.forward_event(key, value);
            if result.is_ok() {
                self.maybe_run_pending_selected_text_switch()?;
            }
            return result;
        }

        if value == 1 && key == undo_key {
            self.suppressed_undo_key = Some(key);
            if self.begin_deferred_manual_current_word_correction(undo_key, &config, origin)? {
                return Ok(());
            }
            let word_before_cursor = if self.can_correct_word_before_cursor() {
                self.word_context.word_before_cursor.clone()
            } else {
                Vec::new()
            };
            if let Some(applied) = self.apply_manual_correction(
                &config,
                &word_before_cursor,
                CorrectionPath::ManualHotkey,
            )? {
                let separator_replay_key =
                    finalize_manual_correction(&mut self.buffer, &mut self.word_context, &applied);
                log_input_debug(
                    "manual-separator-replay",
                    &format!(
                        "requested={} key={separator_replay_key:?} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={} used_current_buffer={} extra_backspaces={}",
                        separator_replay_key.is_some(),
                        self.suppressed_separator_key,
                        pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                        pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                        self.buffer.len(),
                        self.word_context.followed_by_separator,
                        applied.used_current_buffer,
                        applied.extra_backspaces,
                    ),
                );
                if let Some(separator_key) = separator_replay_key {
                    self.keyboard_mut()?.type_separator(separator_key)?;
                    log_input_debug(
                        "manual-separator-replay",
                        &format!(
                            "sent=true key={separator_key:?} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={} used_current_buffer={} extra_backspaces={}",
                            self.suppressed_separator_key,
                            pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                            pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                            applied.used_current_buffer,
                            applied.extra_backspaces,
                        ),
                    );
                }
                self.current_word_correction_state = if applied.used_current_buffer {
                    CurrentWordCorrectionState::ManuallyCorrected
                } else {
                    CurrentWordCorrectionState::Raw
                };
                self.manual_hotkey_latch = next_manual_hotkey_latch_after_manual_correction(
                    undo_key,
                    CorrectionPath::ManualHotkey,
                    applied.used_current_buffer,
                    SystemTime::now(),
                );
            }
            return Ok(());
        }

        match key {
            evdev::Key::KEY_SPACE => {
                if should_commit_manually_corrected_current_word(
                    self.current_word_correction_state,
                    key,
                    self.buffer.len(),
                ) {
                    self.current_word_correction_state = CurrentWordCorrectionState::Raw;
                    self.word_context.valid = !self.buffer.is_empty();
                    self.word_context.word_before_cursor = self.buffer.clone();
                    self.word_context.followed_by_separator = true;
                    self.buffer.clear();
                    return self.keyboard_mut()?.forward_event(key, value);
                }
                let startup_sync_ready = self.refresh_startup_layout_before_autocorrect()?;
                let features = self.runtime.feature_availability();
                let cached_layout_state = self.runtime.current_layout_state();
                let current_layout_kind = self.current_layout_kind();
                let effective_layout_kind = self.runtime.auto_correction_layout_kind();
                if effective_layout_kind != current_layout_kind {
                    let cached_layout_trustworthy = matches!(
                        cached_layout_state,
                        CurrentLayoutState::Known {
                            trustworthy: true,
                            ..
                        }
                    );
                    log_layout_debug(
                        "space-layout-cache",
                        &format!(
                            "cached_layout_kind={current_layout_kind:?} effective_layout_kind={effective_layout_kind:?} trustworthy={cached_layout_trustworthy} source=runtime-cache",
                        ),
                    );
                }
                let should_switch_word = should_switch(&self.buffer, effective_layout_kind);
                let same_layout_plan = same_layout_case_correction_plan(
                    &self.buffer,
                    effective_layout_kind,
                    config.fix_two_capitals,
                    config.fix_accidental_caps_lock,
                );
                let corrected = self.runtime.is_enabled()
                    && config.auto_switch_enabled
                    && features.auto_switch
                    && startup_sync_ready
                    && auto_layout_correction_supported_for_layout(effective_layout_kind)
                    && should_switch_word;

                let selected_path = if corrected {
                    "layout-correction"
                } else if same_layout_plan.is_some() {
                    "same-layout-correction"
                } else {
                    "no-correction"
                };
                log_layout_debug(
                    "space-correction-decision",
                    &format!(
                        "enabled={} auto_switch_enabled={} feature_auto_switch={} startup_sync_ready={} current_layout_kind={current_layout_kind:?} effective_layout_kind={effective_layout_kind:?} should_switch={} same_layout_case_fix={} selected_path={} buffer_len={}",
                        self.runtime.is_enabled(),
                        config.auto_switch_enabled,
                        features.auto_switch,
                        startup_sync_ready,
                        should_switch_word,
                        same_layout_plan.is_some(),
                        selected_path,
                        self.buffer.len(),
                    ),
                );

                if corrected {
                    log_input_debug(
                        "suppressed-separator-set",
                        &format!(
                            "reason=space-layout-correction key={key:?} value={value} next_suppressed_separator_key={:?} pending_action=layout-correction pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            Some(key),
                            Some(key),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    self.suppressed_separator_key = Some(key);
                    self.pending_word_commit = Some(PendingWordCommit {
                        separator_key: key,
                        action: PendingWordCommitAction::LayoutCorrection,
                    });
                    log_input_debug(
                        "pending-word-commit-set",
                        &format!(
                            "reason=space-layout-correction key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            self.suppressed_separator_key,
                            pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                            pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    return Ok(());
                }

                if let Some(plan) = same_layout_plan {
                    log_input_debug(
                        "suppressed-separator-set",
                        &format!(
                            "reason=space-same-layout-correction key={key:?} value={value} next_suppressed_separator_key={:?} pending_action=same-layout-correction pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            Some(key),
                            Some(key),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    self.suppressed_separator_key = Some(key);
                    self.pending_word_commit = Some(PendingWordCommit {
                        separator_key: key,
                        action: PendingWordCommitAction::SameLayoutCaseCorrection {
                            corrected_buffer: plan.buffer,
                        },
                    });
                    log_input_debug(
                        "pending-word-commit-set",
                        &format!(
                            "reason=space-same-layout-correction key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            self.suppressed_separator_key,
                            pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                            pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    return Ok(());
                }

                self.word_context.valid = !self.buffer.is_empty();
                self.word_context.word_before_cursor = self.buffer.clone();
                self.word_context.followed_by_separator = true;
                self.buffer.clear();
                self.keyboard_mut()?.forward_event(key, value)
            }
            evdev::Key::KEY_ENTER | evdev::Key::KEY_TAB => {
                if should_commit_manually_corrected_current_word(
                    self.current_word_correction_state,
                    key,
                    self.buffer.len(),
                ) {
                    self.current_word_correction_state = CurrentWordCorrectionState::Raw;
                    self.invalidate_word_context();
                    return self.keyboard_mut()?.forward_event(key, value);
                }
                if let Some(plan) = same_layout_case_correction_plan(
                    &self.buffer,
                    self.current_layout_kind(),
                    config.fix_two_capitals,
                    config.fix_accidental_caps_lock,
                ) {
                    log_input_debug(
                        "suppressed-separator-set",
                        &format!(
                            "reason=commit-same-layout-correction key={key:?} value={value} next_suppressed_separator_key={:?} pending_action=same-layout-correction pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            Some(key),
                            Some(key),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    self.suppressed_separator_key = Some(key);
                    self.pending_word_commit = Some(PendingWordCommit {
                        separator_key: key,
                        action: PendingWordCommitAction::SameLayoutCaseCorrection {
                            corrected_buffer: plan.buffer,
                        },
                    });
                    log_input_debug(
                        "pending-word-commit-set",
                        &format!(
                            "reason=commit-same-layout-correction key={key:?} value={value} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                            self.suppressed_separator_key,
                            pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                            pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                            self.buffer.len(),
                            self.word_context.followed_by_separator,
                        ),
                    );
                    return Ok(());
                }
                self.invalidate_word_context();
                self.keyboard_mut()?.forward_event(key, value)
            }
            evdev::Key::KEY_BACKSPACE => {
                self.current_word_correction_state = CurrentWordCorrectionState::Raw;
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                } else if self.word_context.valid && self.word_context.followed_by_separator {
                    self.buffer = self.word_context.word_before_cursor.clone();
                    self.word_context.followed_by_separator = false;
                }
                self.keyboard_mut()?.forward_event(key, value)
            }
            _ => {
                let current_stroke = Keystroke {
                    key,
                    shift: self.modifiers.is_shift_pressed(),
                    caps_lock: self.modifiers.is_caps_lock_active(),
                };

                let plain_character_input = is_character(key)
                    && !self.modifiers.is_ctrl_pressed()
                    && !self.modifiers.is_alt_pressed()
                    && !self.modifiers.is_meta_pressed();

                if plain_character_input {
                    self.current_word_correction_state = next_current_word_state_after_plain_character_input(
                        self.current_word_correction_state,
                    );
                    // Once we are typing the current word again, the cursor is no longer
                    // "after a finished word". Keep only the active buffer state.
                    self.word_context.valid = true;
                    self.word_context.followed_by_separator = false;
                    self.word_context.word_before_cursor.clear();
                    self.buffer.push(current_stroke);
                } else if !is_modifier(key) {
                    self.invalidate_word_context();
                }
                let result = self.keyboard_mut()?.forward_event(key, value);
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

        let Some(selected_text_runner) = self.selected_text_runner.as_ref() else {
            log_selected_text_debug("hotkey-skip", "reason=input-backend-unavailable");
            return Ok(());
        };

        if !selected_text_runner.try_start()? {
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
        let corrected_buffer = self
            .apply_manual_correction(config, &[], CorrectionPath::AutoWordCommit)?
            .map(|applied| applied.corrected_buffer)
            .unwrap_or_else(|| self.buffer.clone());
        self.commit_corrected_word(separator_key, corrected_buffer)
    }

    fn finish_pending_same_layout_case_correction(
        &mut self,
        separator_key: evdev::Key,
        corrected_buffer: Vec<Keystroke>,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
    ) -> Result<(), SwitcherError> {
        let plan = CorrectionPlan {
            buffer: corrected_buffer.clone(),
            extra_backspaces: 0,
        };
        let modifiers = self.modifiers;
        self.keyboard_mut()?
            .apply_same_layout_correction(&plan, config, modifiers)?;
        self.commit_corrected_word(separator_key, corrected_buffer)
    }

    fn finish_pending_word_commit(
        &mut self,
        pending: PendingWordCommit,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
    ) -> Result<(), SwitcherError> {
        let action_label = pending_word_commit_action_label(Some(&pending));
        let separator_key = pending.separator_key;
        log_input_debug(
            "finish-pending-word-commit",
            &format!(
                "phase=start separator_key={:?} action={} suppressed_separator_key={:?} buffer_len={} followed_by_separator={}",
                separator_key,
                action_label,
                self.suppressed_separator_key,
                self.buffer.len(),
                self.word_context.followed_by_separator,
            ),
        );
        match pending.action {
            PendingWordCommitAction::LayoutCorrection => {
                self.finish_pending_auto_correction(separator_key, config)?
            }
            PendingWordCommitAction::SameLayoutCaseCorrection { corrected_buffer } => self
                .finish_pending_same_layout_case_correction(
                    separator_key,
                    corrected_buffer,
                    config,
                )?,
        }
        log_input_debug(
            "finish-pending-word-commit",
            &format!(
                "phase=finish separator_key={:?} action={} suppressed_separator_key={:?} buffer_len={} followed_by_separator={}",
                separator_key,
                action_label,
                self.suppressed_separator_key,
                self.buffer.len(),
                self.word_context.followed_by_separator,
            ),
        );
        Ok(())
    }

    fn commit_corrected_word(
        &mut self,
        separator_key: evdev::Key,
        corrected_buffer: Vec<Keystroke>,
    ) -> Result<(), SwitcherError> {
        log_input_debug(
            "commit-corrected-word",
            &format!(
                "phase=start separator_key={separator_key:?} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} corrected_buffer_len={} followed_by_separator={}",
                self.suppressed_separator_key,
                pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                self.buffer.len(),
                corrected_buffer.len(),
                self.word_context.followed_by_separator,
            ),
        );
        if separator_key == evdev::Key::KEY_SPACE {
            self.word_context.valid = !corrected_buffer.is_empty();
            self.word_context.word_before_cursor = corrected_buffer;
            self.word_context.followed_by_separator = true;
            self.buffer.clear();
        } else {
            self.invalidate_word_context();
        }
        log_input_debug(
            "commit-corrected-word",
            &format!(
                "phase=type-separator separator_key={separator_key:?} suppressed_separator_key={:?} pending_action={} pending_separator_key={:?} buffer_len={} followed_by_separator={}",
                self.suppressed_separator_key,
                pending_word_commit_action_label(self.pending_word_commit.as_ref()),
                pending_word_commit_separator_key(self.pending_word_commit.as_ref()),
                self.buffer.len(),
                self.word_context.followed_by_separator,
            ),
        );
        self.keyboard_mut()?.type_separator(separator_key)
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
        correction_path: CorrectionPath,
    ) -> Result<Option<AppliedManualCorrection>, SwitcherError> {
        let Some(prepared) =
            self.prepare_manual_correction(config, fallback_buffer, correction_path)?
        else {
            return Ok(None);
        };
        let modifiers = self.modifiers;
        self.keyboard_mut()?
            .apply_correction(&prepared.plan, config, modifiers)?;
        self.finish_manual_correction_sync(config, correction_path)?;
        Ok(Some(AppliedManualCorrection {
            corrected_buffer: prepared.plan.buffer,
            used_current_buffer: prepared.used_current_buffer,
            extra_backspaces: prepared.plan.extra_backspaces,
        }))
    }

    fn prepare_manual_correction(
        &mut self,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
        fallback_buffer: &[Keystroke],
        correction_path: CorrectionPath,
    ) -> Result<Option<PreparedManualCorrection>, SwitcherError> {
        let features = self.runtime.feature_availability();
        if !features.manual_word_fix {
            return Ok(None);
        }

        let cached_layout_before = self.runtime.current_layout_state();
        let legacy_layout_before = self.runtime.current_layout();
        let current_layout_kind_before = self.current_layout_kind();
        let pre_correction_sync = self.runtime.sync_with_backend();
        log_layout_debug(
            "correction-start",
            &format!(
                "path={} combo={:?} pre_sync={pre_correction_sync:?} cached_layout_before={cached_layout_before:?} legacy_layout_before={} current_layout_kind_before={current_layout_kind_before:?} buffer_len={} fallback_buffer_len={} followed_by_separator={}",
                correction_path.as_str(),
                config.layout_switch_combo,
                format_legacy_layout(legacy_layout_before),
                self.buffer.len(),
                fallback_buffer.len(),
                self.word_context.followed_by_separator,
            ),
        );
        if !automatic_layout_actions_allowed(&pre_correction_sync) {
            log_layout_debug(
                "manual-correction-sync",
                &format!(
                    "path={} combo={:?} source=backend skipped=true phase=before-correction",
                    correction_path.as_str(),
                    config.layout_switch_combo,
                ),
            );
            return Ok(None);
        }

        if !matches!(
            current_layout_kind_before,
            AppLayoutKind::English | AppLayoutKind::Russian
        ) {
            return Ok(None);
        }

        let used_current_buffer = !self.buffer.is_empty();
        let Some(mut plan) = manual_correction_plan(
            &self.buffer,
            fallback_buffer,
            self.word_context.followed_by_separator,
            current_layout_kind_before,
        ) else {
            return Ok(None);
        };
        plan.buffer = apply_case_fixes_to_strokes(
            &plan.buffer,
            config.fix_two_capitals,
            config.fix_accidental_caps_lock,
        );
        log_layout_debug(
            "correction-plan",
            &format!(
                "path={} combo={:?} buffer_len={} extra_backspaces={}",
                correction_path.as_str(),
                config.layout_switch_combo,
                plan.buffer.len(),
                plan.extra_backspaces,
            ),
        );
        log_input_debug(
            "manual-correction-separator",
            &format!(
                "path={} requested={} key={:?} used_current_buffer={} extra_backspaces={} followed_by_separator={} buffer_len={} fallback_buffer_len={}",
                correction_path.as_str(),
                manual_separator_replay_key(used_current_buffer, plan.extra_backspaces).is_some(),
                manual_separator_replay_key(used_current_buffer, plan.extra_backspaces),
                used_current_buffer,
                plan.extra_backspaces,
                self.word_context.followed_by_separator,
                self.buffer.len(),
                fallback_buffer.len(),
            ),
        );
        Ok(Some(PreparedManualCorrection {
            plan,
            used_current_buffer,
        }))
    }

    fn finish_manual_correction_sync(
        &mut self,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
        correction_path: CorrectionPath,
    ) -> Result<(), SwitcherError> {
        let cached_layout_before = self.runtime.current_layout_state();
        let post_correction_sync = self.runtime.sync_with_backend();
        let cached_layout_after = self.runtime.current_layout_state();
        let legacy_layout_after = self.runtime.current_layout();
        let current_layout_kind_after = self.current_layout_kind();
        log_layout_debug(
            "correction-finish",
            &format!(
                "path={} combo={:?} post_sync={post_correction_sync:?} cached_layout_after={cached_layout_after:?} legacy_layout_after={} current_layout_kind_after={current_layout_kind_after:?} cached_layout_changed={}",
                correction_path.as_str(),
                config.layout_switch_combo,
                format_legacy_layout(legacy_layout_after),
                cached_layout_before != cached_layout_after,
            ),
        );
        match post_correction_sync {
            BackendSyncResult::Updated { current, .. } => {
                log_layout_debug(
                    "manual-correction-sync",
                    &format!(
                        "path={} combo={:?} source=backend updated=true current={current:?}",
                        correction_path.as_str(),
                        config.layout_switch_combo,
                    ),
                );
                self.startup_layout_resync.complete();
                self.publish_status_changed()?;
            }
            BackendSyncResult::Unchanged => {
                log_layout_debug(
                    "manual-correction-sync",
                    &format!(
                        "path={} combo={:?} source=backend updated=false current=unchanged",
                        correction_path.as_str(),
                        config.layout_switch_combo,
                    ),
                );
                self.startup_layout_resync.complete();
                self.publish_status_changed()?;
            }
            BackendSyncResult::Skipped => {
                log_layout_debug(
                    "manual-correction-sync",
                    &format!(
                        "path={} combo={:?} source=backend skipped=true phase=after-correction fallback=disabled",
                        correction_path.as_str(),
                        config.layout_switch_combo,
                    ),
                );
            }
        }
        Ok(())
    }

    fn begin_deferred_manual_current_word_correction(
        &mut self,
        undo_key: evdev::Key,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
        origin: InputOrigin,
    ) -> Result<bool, SwitcherError> {
        let Some(prepared) =
            self.prepare_manual_correction(config, &[], CorrectionPath::ManualHotkey)?
        else {
            return Ok(false);
        };

        if !prepared.used_current_buffer {
            return Ok(false);
        }

        let frozen_modifiers = self.modifiers;
        let start_outcome = self.keyboard_mut()?.begin_manual_current_word_correction(
            &prepared.plan,
            config,
            frozen_modifiers,
        )?;
        let request_id = match start_outcome {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            ManualCurrentWordStartOutcome::RejectedBeforeMutation(reason) => {
                log_input_debug(
                    "manual-current-word-start-rejected",
                    &format!(
                        "reason={reason} origin={} buffer_len={} extra_backspaces={}",
                        match origin {
                            InputOrigin::Physical => "physical",
                            InputOrigin::DeferredReplay => "deferred-replay",
                            InputOrigin::DeferredRetry => "deferred-retry",
                        },
                        prepared.plan.buffer.len(),
                        prepared.plan.extra_backspaces,
                    ),
                );
                return Ok(true);
            }
        };
        let deferred_input = VecDeque::new();
        let carried_deferred_len = deferred_input.len();
        log_input_debug(
            "manual-current-word-inflight-start",
            &format!(
                "request_id={} origin={} undo_key={undo_key:?} buffer_len={} extra_backspaces={} carried_deferred_len={} modifiers={frozen_modifiers:?}",
                request_id,
                match origin {
                    InputOrigin::Physical => "physical",
                    InputOrigin::DeferredReplay => "deferred-replay",
                    InputOrigin::DeferredRetry => "deferred-retry",
                },
                prepared.plan.buffer.len(),
                prepared.plan.extra_backspaces,
                carried_deferred_len,
            ),
        );
        self.manual_current_word_flow = ManualCurrentWordFlow::InFlight {
            session: DeferredManualCurrentWordSession {
                request_id,
                undo_key,
                _frozen_modifiers: frozen_modifiers,
                deferred_input,
                seen_real_next_step: false,
                retry_after_drain_requested: false,
                started_at: Instant::now(),
                drained_events: 0,
            },
        };
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

    fn try_initialize_input_backend(&mut self) -> Result<(), SwitcherError> {
        if let Some(opened) = self
            .input_backend
            .initialize(self.shared_modifiers.clone(), Instant::now())?
        {
            self.install_opened_input_backend(opened);
        }
        Ok(())
    }

    fn maybe_retry_input_backend(&mut self) -> Result<(), SwitcherError> {
        if let Some(opened) = self
            .input_backend
            .try_recover(self.shared_modifiers.clone(), Instant::now())?
        {
            self.install_opened_input_backend(opened);
        }
        Ok(())
    }

    fn install_opened_input_backend(
        &mut self,
        opened: OpenedInputBackend<ActiveInputBackend>,
    ) {
        let ActiveInputBackend {
            keyboard,
            selected_text_runner,
        } = opened.backend;
        let mut modifiers = ModifierState::default();
        modifiers.set_caps_lock_active(keyboard.caps_lock_active());
        self.modifiers = modifiers;
        self.shared_modifiers.store(self.modifiers);
        self.keyboard = Some(keyboard);
        self.selected_text_runner = Some(selected_text_runner);
    }

    fn handle_runtime_input_failure(&mut self, error: &SwitcherError) -> bool {
        if self.input_backend.record_runtime_failure(error, Instant::now()) {
            self.reset_transient_input_state("input-backend-unavailable");
            self.drop_active_input_backend();
            return true;
        }

        false
    }

    fn drop_active_input_backend(&mut self) {
        if let Some(mut keyboard) = self.keyboard.take() {
            keyboard.release_grab_best_effort();
            keyboard.shutdown();
        }
        // Selected-text jobs do not own the physical keyboard grab, so for this
        // critical input-lock fix it is sufficient to drop the runner after
        // keyboard control has already been returned to the user.
        self.selected_text_runner = None;
    }

    fn reset_transient_input_state(&mut self, reason: &str) {
        log_input_debug("transient-input-reset", &format!("reason={reason}"));
        clear_word_context_state(
            &mut self.buffer,
            &mut self.word_context,
            &mut self.current_word_correction_state,
            &mut self.manual_hotkey_latch,
        );
        self.suppressed_hotkey_key = None;
        self.suppressed_undo_key = None;
        self.manual_hotkey_latch = None;
        self.manual_current_word_flow = ManualCurrentWordFlow::Idle;
        self.suppressed_separator_key = None;
        self.layout_shortcut_latched = false;
        self.pending_word_commit = None;
        self.pending_selected_text_switch = false;
        self.modifiers = ModifierState::default();
        self.shared_modifiers.store(self.modifiers);
    }

    fn keyboard_mut(&mut self) -> Result<&mut KeyboardController, SwitcherError> {
        self.keyboard.as_mut().ok_or(SwitcherError::KeyboardNotFound)
    }

    fn invalidate_word_context(&mut self) {
        clear_word_context_state(
            &mut self.buffer,
            &mut self.word_context,
            &mut self.current_word_correction_state,
            &mut self.manual_hotkey_latch,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::Key;

    fn stroke(key: Key) -> Keystroke {
        Keystroke {
            key,
            shift: false,
            caps_lock: false,
        }
    }

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

    #[test]
    fn automatic_layout_correction_can_be_scheduled_for_english_and_russian_layouts() {
        assert!(auto_layout_correction_supported_for_layout(
            AppLayoutKind::English
        ));
        assert!(auto_layout_correction_supported_for_layout(
            AppLayoutKind::Russian
        ));
        assert!(!auto_layout_correction_supported_for_layout(
            AppLayoutKind::Other
        ));
        assert!(!auto_layout_correction_supported_for_layout(
            AppLayoutKind::Unknown
        ));
    }

    #[test]
    fn manual_previous_word_correction_requests_separator_replay() {
        let applied = AppliedManualCorrection {
            corrected_buffer: vec![stroke(Key::KEY_G), stroke(Key::KEY_H)],
            used_current_buffer: false,
            extra_backspaces: 1,
        };
        let mut buffer = vec![stroke(Key::KEY_A)];
        let mut word_context = WordContext::default();

        let replay = finalize_manual_correction(&mut buffer, &mut word_context, &applied);

        assert_eq!(replay, Some(Key::KEY_SPACE));
        assert!(buffer.is_empty());
        assert!(word_context.valid);
        assert_eq!(word_context.word_before_cursor, applied.corrected_buffer);
        assert!(word_context.followed_by_separator);
    }

    #[test]
    fn manual_current_word_correction_does_not_request_separator_replay() {
        let applied = AppliedManualCorrection {
            corrected_buffer: vec![stroke(Key::KEY_G), stroke(Key::KEY_H)],
            used_current_buffer: true,
            extra_backspaces: 0,
        };
        let mut buffer = vec![stroke(Key::KEY_A), stroke(Key::KEY_B)];
        let mut word_context = WordContext::default();

        let replay = finalize_manual_correction(&mut buffer, &mut word_context, &applied);

        assert_eq!(replay, None);
        assert_eq!(buffer, applied.corrected_buffer);
        assert!(word_context.valid);
        assert_eq!(word_context.word_before_cursor, applied.corrected_buffer);
        assert!(!word_context.followed_by_separator);
    }

    #[test]
    fn manual_previous_word_correction_updates_word_context_with_separator() {
        let applied = AppliedManualCorrection {
            corrected_buffer: vec![stroke(Key::KEY_H), stroke(Key::KEY_E), stroke(Key::KEY_Y)],
            used_current_buffer: false,
            extra_backspaces: 1,
        };
        let mut buffer = Vec::new();
        let mut word_context = WordContext::default();

        finalize_manual_correction(&mut buffer, &mut word_context, &applied);

        assert!(word_context.valid);
        assert_eq!(word_context.word_before_cursor, applied.corrected_buffer);
        assert!(word_context.followed_by_separator);
    }

    #[test]
    fn manual_current_word_correction_replaces_stale_active_buffer() {
        let applied = AppliedManualCorrection {
            corrected_buffer: vec![stroke(Key::KEY_H), stroke(Key::KEY_E), stroke(Key::KEY_L)],
            used_current_buffer: true,
            extra_backspaces: 0,
        };
        let mut buffer = vec![stroke(Key::KEY_Q), stroke(Key::KEY_W), stroke(Key::KEY_E)];
        let mut word_context = WordContext::default();

        finalize_manual_correction(&mut buffer, &mut word_context, &applied);

        assert_eq!(buffer, applied.corrected_buffer);
        assert_eq!(word_context.word_before_cursor, applied.corrected_buffer);
        assert!(!word_context.followed_by_separator);
    }

    #[test]
    fn suppressed_separator_logic_swallows_only_matching_release() {
        assert!(!should_swallow_suppressed_separator_release(
            Some(Key::KEY_SPACE),
            Key::KEY_SPACE,
            1,
        ));
        assert!(should_swallow_suppressed_separator_release(
            Some(Key::KEY_SPACE),
            Key::KEY_SPACE,
            0,
        ));
        assert!(!should_swallow_suppressed_separator_release(
            Some(Key::KEY_SPACE),
            Key::KEY_ENTER,
            0,
        ));
    }

    #[test]
    fn early_finish_preserves_one_swallowed_release_for_original_separator() {
        let pending = PendingWordCommit {
            separator_key: Key::KEY_SPACE,
            action: PendingWordCommitAction::LayoutCorrection,
        };

        assert_eq!(
            preserved_separator_after_early_finish(Some(&pending), Key::KEY_A, 1),
            Some(Key::KEY_SPACE)
        );
        assert_eq!(
            preserved_separator_after_early_finish(Some(&pending), Key::KEY_A, 0),
            None
        );
        assert_eq!(
            preserved_separator_after_early_finish(None, Key::KEY_A, 1),
            None
        );
    }

    #[test]
    fn manually_corrected_current_word_requires_plain_separator_commit() {
        assert!(should_commit_manually_corrected_current_word(
            CurrentWordCorrectionState::ManuallyCorrected,
            Key::KEY_SPACE,
            3,
        ));
        assert!(should_commit_manually_corrected_current_word(
            CurrentWordCorrectionState::ManuallyCorrected,
            Key::KEY_ENTER,
            3,
        ));
        assert!(should_commit_manually_corrected_current_word(
            CurrentWordCorrectionState::ManuallyCorrected,
            Key::KEY_TAB,
            3,
        ));
    }

    #[test]
    fn manually_corrected_current_word_does_not_commit_without_separator_or_buffer() {
        assert!(!should_commit_manually_corrected_current_word(
            CurrentWordCorrectionState::Raw,
            Key::KEY_SPACE,
            3,
        ));
        assert!(!should_commit_manually_corrected_current_word(
            CurrentWordCorrectionState::ManuallyCorrected,
            Key::KEY_SPACE,
            0,
        ));
        assert!(!should_commit_manually_corrected_current_word(
            CurrentWordCorrectionState::ManuallyCorrected,
            Key::KEY_A,
            3,
        ));
    }

    #[test]
    fn manually_corrected_current_word_state_survives_plain_character_input() {
        let state = next_current_word_state_after_plain_character_input(
            CurrentWordCorrectionState::ManuallyCorrected,
        );

        assert_eq!(state, CurrentWordCorrectionState::ManuallyCorrected);
        assert!(should_commit_manually_corrected_current_word(
            state,
            Key::KEY_SPACE,
            4,
        ));
    }

    #[test]
    fn raw_current_word_state_stays_raw_after_plain_character_input() {
        let state =
            next_current_word_state_after_plain_character_input(CurrentWordCorrectionState::Raw);

        assert_eq!(state, CurrentWordCorrectionState::Raw);
        assert!(!should_commit_manually_corrected_current_word(
            state,
            Key::KEY_SPACE,
            4,
        ));
    }

    #[test]
    fn queued_pause_events_are_swallowed_while_manual_hotkey_is_latched() {
        let latched_key = Some(ManualHotkeyLatch {
            key: Key::KEY_PAUSE,
            armed_at: SystemTime::UNIX_EPOCH,
        });

        assert!(should_swallow_manual_hotkey_latched_event(
            latched_key,
            Key::KEY_PAUSE,
            1,
        ));
        assert!(should_swallow_manual_hotkey_latched_event(
            latched_key,
            Key::KEY_PAUSE,
            0,
        ));
        assert!(should_swallow_manual_hotkey_latched_event(
            latched_key,
            Key::KEY_PAUSE,
            2,
        ));
        assert!(!should_swallow_manual_hotkey_latched_event(
            latched_key,
            Key::KEY_A,
            1,
        ));
    }

    #[test]
    fn pause_then_letter_then_pause_clears_latch_and_allows_new_correction() {
        let latched_key = Some(ManualHotkeyLatch {
            key: Key::KEY_PAUSE,
            armed_at: SystemTime::UNIX_EPOCH,
        });

        assert!(should_clear_manual_hotkey_latch_on_key_press(
            latched_key,
            Key::KEY_PAUSE,
            Key::KEY_A,
            1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        ));

        let latched_key = None::<ManualHotkeyLatch>;
        assert!(!should_swallow_manual_hotkey_latched_event(
            latched_key,
            Key::KEY_PAUSE,
            1,
        ));
    }

    #[test]
    fn pause_then_space_clears_latch_without_repeating_correction() {
        assert!(should_clear_manual_hotkey_latch_on_key_press(
            Some(ManualHotkeyLatch {
                key: Key::KEY_PAUSE,
                armed_at: SystemTime::UNIX_EPOCH,
            }),
            Key::KEY_PAUSE,
            Key::KEY_SPACE,
            1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        ));
    }

    #[test]
    fn previous_word_manual_correction_does_not_latch_hotkey() {
        assert_eq!(
            next_manual_hotkey_latch_after_manual_correction(
                Key::KEY_PAUSE,
                CorrectionPath::ManualHotkey,
                false,
                SystemTime::UNIX_EPOCH,
            ),
            None,
        );
    }

    #[test]
    fn pause_then_backspace_then_pause_clears_latch() {
        assert!(should_clear_manual_hotkey_latch_on_key_press(
            Some(ManualHotkeyLatch {
                key: Key::KEY_PAUSE,
                armed_at: SystemTime::UNIX_EPOCH,
            }),
            Key::KEY_PAUSE,
            Key::KEY_BACKSPACE,
            1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        ));
    }

    #[test]
    fn modifier_press_does_not_clear_manual_hotkey_latch() {
        assert!(!should_clear_manual_hotkey_latch_on_key_press(
            Some(ManualHotkeyLatch {
                key: Key::KEY_PAUSE,
                armed_at: SystemTime::UNIX_EPOCH,
            }),
            Key::KEY_PAUSE,
            Key::KEY_LEFTSHIFT,
            1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        ));
    }

    #[test]
    fn current_word_manual_correction_sets_hotkey_latch() {
        assert_eq!(
            next_manual_hotkey_latch_after_manual_correction(
                Key::KEY_PAUSE,
                CorrectionPath::ManualHotkey,
                true,
                SystemTime::UNIX_EPOCH + Duration::from_secs(3),
            ),
            Some(ManualHotkeyLatch {
                key: Key::KEY_PAUSE,
                armed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(3),
            }),
        );
    }

    #[test]
    fn queued_letter_from_during_correction_does_not_clear_latch() {
        assert!(!should_clear_manual_hotkey_latch_on_key_press(
            Some(ManualHotkeyLatch {
                key: Key::KEY_PAUSE,
                armed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(5),
            }),
            Key::KEY_PAUSE,
            Key::KEY_A,
            1,
            SystemTime::UNIX_EPOCH + Duration::from_secs(4),
        ));
    }

    #[test]
    fn initiating_pause_release_is_swallowed_by_suppressed_undo_key() {
        assert!(should_swallow_suppressed_undo_release(
            Some(Key::KEY_PAUSE),
            Key::KEY_PAUSE,
            0,
        ));
        assert!(!should_swallow_suppressed_undo_release(
            Some(Key::KEY_PAUSE),
            Key::KEY_PAUSE,
            1,
        ));
        assert!(!should_swallow_suppressed_undo_release(
            Some(Key::KEY_PAUSE),
            Key::KEY_A,
            0,
        ));
    }

    #[test]
    fn modifier_press_is_enqueued_without_marking_real_next_step_during_in_flight() {
        assert_eq!(
            manual_current_word_physical_event_action(
                true,
                Key::KEY_PAUSE,
                false,
                Key::KEY_LEFTSHIFT,
                1,
                InputOrigin::Physical,
            ),
            ManualCurrentWordPhysicalEventAction::Enqueue {
                marks_real_next_step: false,
            },
        );
    }

    #[test]
    fn ordinary_press_marks_real_next_step_when_enqueued_during_in_flight() {
        assert_eq!(
            manual_current_word_physical_event_action(
                true,
                Key::KEY_PAUSE,
                false,
                Key::KEY_A,
                1,
                InputOrigin::Physical,
            ),
            ManualCurrentWordPhysicalEventAction::Enqueue {
                marks_real_next_step: true,
            },
        );
    }

    #[test]
    fn pause_press_after_real_next_step_requests_retry_after_drain() {
        assert_eq!(
            manual_current_word_physical_event_action(
                true,
                Key::KEY_PAUSE,
                true,
                Key::KEY_PAUSE,
                1,
                InputOrigin::Physical,
            ),
            ManualCurrentWordPhysicalEventAction::RequestRetryAfterDrain,
        );
    }

    #[test]
    fn pause_release_after_real_next_step_is_still_swallowed() {
        assert_eq!(
            manual_current_word_physical_event_action(
                true,
                Key::KEY_PAUSE,
                true,
                Key::KEY_PAUSE,
                0,
                InputOrigin::Physical,
            ),
            ManualCurrentWordPhysicalEventAction::Swallow,
        );
    }

    #[test]
    fn deferred_replay_modifier_is_processed_immediately() {
        assert_eq!(
            manual_current_word_physical_event_action(
                true,
                Key::KEY_PAUSE,
                false,
                Key::KEY_LEFTSHIFT,
                1,
                InputOrigin::DeferredReplay,
            ),
            ManualCurrentWordPhysicalEventAction::ProcessImmediately,
        );
    }

    #[test]
    fn deferred_retry_origin_is_processed_immediately() {
        assert_eq!(
            manual_current_word_physical_event_action(
                true,
                Key::KEY_PAUSE,
                true,
                Key::KEY_PAUSE,
                1,
                InputOrigin::DeferredRetry,
            ),
            ManualCurrentWordPhysicalEventAction::ProcessImmediately,
        );
    }

    #[test]
    fn queue_overflow_policy_aborts_when_limit_is_reached() {
        assert!(should_abort_manual_current_word_flow_on_queue_overflow(
            MAX_DEFERRED_MANUAL_INPUT_EVENTS,
            MAX_DEFERRED_MANUAL_INPUT_EVENTS,
        ));
        assert!(!should_abort_manual_current_word_flow_on_queue_overflow(
            MAX_DEFERRED_MANUAL_INPUT_EVENTS - 1,
            MAX_DEFERRED_MANUAL_INPUT_EVENTS,
        ));
    }

    #[test]
    fn drain_restart_requires_retry_request_and_empty_queue() {
        assert!(should_restart_manual_current_word_after_drain(0, true));
        assert!(!should_restart_manual_current_word_after_drain(1, true));
        assert!(!should_restart_manual_current_word_after_drain(0, false));
    }

    #[test]
    fn clear_word_context_state_clears_word_tracking_without_flow_state() {
        let mut buffer = vec![stroke(Key::KEY_Z)];
        let mut word_context = WordContext {
            valid: true,
            word_before_cursor: vec![stroke(Key::KEY_Z)],
            followed_by_separator: true,
        };
        let mut correction_state = CurrentWordCorrectionState::ManuallyCorrected;
        let mut latch = Some(ManualHotkeyLatch {
            key: Key::KEY_PAUSE,
            armed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(5),
        });

        clear_word_context_state(
            &mut buffer,
            &mut word_context,
            &mut correction_state,
            &mut latch,
        );

        assert!(buffer.is_empty());
        assert_eq!(correction_state, CurrentWordCorrectionState::Raw);
        assert_eq!(latch, None);
        assert_eq!(word_context, WordContext::default());
    }

}
