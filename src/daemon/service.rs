use crate::daemon::capture::CaptureEventDisposition;
use crate::daemon::input_backend::{
    ActiveInputBackend, InputBackendHandle, InputBackendLifecycle, KeyboardInputBackendOpener,
    OpenedInputBackend,
};
use crate::daemon::input_snapshot::{
    InputLayoutStatus, InputRuntimeSnapshot, SnapshotAuthorization, SnapshotTryLoad,
};
use crate::daemon::keyboard::{
    hotkey_trigger_to_evdev_key, is_character, is_modifier, is_wayland_focus_switch_shortcut,
    log_input_debug, CorrectionLayoutSwitchOutcome, KeyboardController,
    ManualCurrentWordCompletion, ManualCurrentWordOutcome, ManualCurrentWordStartOutcome,
    ModifierState, SharedModifierState, INPUT_EVENT_WAIT_TIMEOUT,
};
use crate::daemon::runtime::{log_layout_debug, RuntimeState};
use crate::daemon::selected_text::{log_selected_text_debug, SelectedTextJobRunner};
use crate::daemon::switch_logic::{
    apply_case_fixes_to_strokes, manual_correction_plan, same_layout_case_correction_plan,
    should_switch, CorrectionPlan, Keystroke,
};
use crate::dbus::{DbusSignalEvent, DbusSignalPublisher};
use crate::error::{CaptureError, SwitcherError};
use crate::layout_backend::{legacy_current_layout_bool, AppLayoutKind, CurrentLayoutState};
use crate::model::{
    HotkeyModifiers, HotkeySpec, LayoutSwitchCaptureState, SessionType, MAX_CORRECTION_KEYSTROKES,
};
use evdev::InputEventKind;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use zbus::blocking::Connection;

const EVENT_LOOP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_LOOP_HEARTBEAT_EVENTS: u64 = 500;
const WRITER_HEALTH_POLL_QUANTUM: Duration = Duration::from_millis(5);
const MANUAL_CURRENT_WORD_IN_FLIGHT_POLL_TIMEOUT: Duration = WRITER_HEALTH_POLL_QUANTUM;
const MAX_DEFERRED_MANUAL_INPUT_EVENTS: usize = 256;

// Word tracking / correction state

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
    authorization: SnapshotAuthorization,
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

// Deferred manual current-word flow state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputOrigin {
    Physical,
    DeferredReplay,
    DeferredRetry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureRoutingControl {
    Continue,
    Return,
}

fn apply_capture_routing_disposition<F>(
    disposition: CaptureEventDisposition,
    modifiers: &mut ModifierState,
    shared_modifiers: &SharedModifierState,
    key: evdev::Key,
    value: i32,
    mut forward_event: F,
) -> Result<CaptureRoutingControl, SwitcherError>
where
    F: FnMut(evdev::Key, i32) -> Result<(), SwitcherError>,
{
    match disposition {
        CaptureEventDisposition::Suppress => Ok(CaptureRoutingControl::Return),
        CaptureEventDisposition::ForwardDirect => {
            modifiers.update(key, value);
            shared_modifiers.store(*modifiers);
            forward_event(key, value)?;
            Ok(CaptureRoutingControl::Return)
        }
        CaptureEventDisposition::PassThrough => Ok(CaptureRoutingControl::Continue),
    }
}

fn should_route_capture_before_manual_current_word(origin: InputOrigin) -> bool {
    matches!(origin, InputOrigin::Physical)
}

fn bounded_capture_poll_timeout(timeout: Duration) -> Duration {
    timeout.min(INPUT_EVENT_WAIT_TIMEOUT)
}

fn active_keyboard_fetch_timeout(timeout: Duration, keyboard_attached: bool) -> Duration {
    if keyboard_attached {
        timeout.min(WRITER_HEALTH_POLL_QUANTUM)
    } else {
        timeout
    }
}

fn writer_health_result(error: Option<SwitcherError>) -> Result<(), SwitcherError> {
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

enum WriterHealthyBatchError<E> {
    Health(E),
    Event(E),
}

enum WriterHealthyOperationError<E> {
    Health(E),
    Operation(E),
}

enum CompletionBeforeWriterHealthError<E> {
    Completion(E),
    Health(E),
}

fn poll_completion_before_writer_health<State, E, PollCompletion, EnsureHealth>(
    state: &mut State,
    poll_completion: PollCompletion,
    ensure_health: EnsureHealth,
) -> Result<(), CompletionBeforeWriterHealthError<E>>
where
    PollCompletion: FnOnce(&mut State) -> Result<(), E>,
    EnsureHealth: FnOnce(&mut State) -> Result<(), E>,
{
    poll_completion(state).map_err(CompletionBeforeWriterHealthError::Completion)?;
    ensure_health(state).map_err(CompletionBeforeWriterHealthError::Health)
}

fn run_writer_healthy_operation<State, Output, E, Health, Operation>(
    state: &mut State,
    mut ensure_healthy: Health,
    operation: Operation,
) -> Result<Output, WriterHealthyOperationError<E>>
where
    Health: FnMut(&mut State) -> Result<(), E>,
    Operation: FnOnce(&mut State) -> Result<Output, E>,
{
    ensure_healthy(state).map_err(WriterHealthyOperationError::Health)?;
    let operation_result = operation(state);
    ensure_healthy(state).map_err(WriterHealthyOperationError::Health)?;
    operation_result.map_err(WriterHealthyOperationError::Operation)
}

fn process_writer_healthy_batch<State, Events, Event, E, Health, Handle>(
    state: &mut State,
    events: Events,
    mut ensure_healthy: Health,
    mut handle_event: Handle,
) -> Result<(), WriterHealthyBatchError<E>>
where
    Events: IntoIterator<Item = Event>,
    Health: FnMut(&mut State) -> Result<(), E>,
    Handle: FnMut(&mut State, Event) -> Result<(), E>,
{
    for event in events {
        ensure_healthy(state).map_err(WriterHealthyBatchError::Health)?;
        handle_event(state, event).map_err(WriterHealthyBatchError::Event)?;
    }
    Ok(())
}

fn reset_capture_epoch_then_shutdown_backend<F>(
    reset_result: Result<Option<LayoutSwitchCaptureState>, CaptureError>,
    shutdown_backend: F,
) -> Option<LayoutSwitchCaptureState>
where
    F: FnOnce(),
{
    let state_change = match reset_result {
        Ok(state_change) => state_change,
        Err(error) => {
            log_input_debug("capture-input-epoch-reset-error", &format!("error={error}"));
            None
        }
    };

    shutdown_backend();
    state_change
}

fn admit_input_backend_install<Backend>(
    mut backend: Backend,
    slot_occupied: bool,
    release_backend: impl FnOnce(&mut Backend),
) -> Result<Backend, SwitcherError> {
    if slot_occupied {
        release_backend(&mut backend);
        return Err(SwitcherError::InputBackendAlreadyActive);
    }
    Ok(backend)
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
    InFlight {
        session: DeferredManualCurrentWordSession,
    },
    DrainingDeferredInput {
        session: DeferredManualCurrentWordSession,
    },
}

// D-Bus signal publishing diagnostics

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DbusSignalEventKind {
    StatusChanged,
    CaptureStateChanged,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DbusSignalDropCounts {
    status_changed: u64,
    capture_state_changed: u64,
}

impl DbusSignalDropCounts {
    fn heartbeat_fields(self) -> Option<String> {
        if self == Self::default() {
            return None;
        }

        Some(format!(
            "dbus_signal_drops_status={} dbus_signal_drops_capture={}",
            self.status_changed, self.capture_state_changed,
        ))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DbusSignalDropCounters {
    counts: DbusSignalDropCounts,
}

impl DbusSignalDropCounters {
    fn record(&mut self, event: DbusSignalEventKind) {
        match event {
            DbusSignalEventKind::StatusChanged => {
                self.counts.status_changed += 1;
            }
            DbusSignalEventKind::CaptureStateChanged => {
                self.counts.capture_state_changed += 1;
            }
        }
    }

    fn take_counts(&mut self) -> DbusSignalDropCounts {
        let counts = self.counts;
        self.counts = DbusSignalDropCounts::default();
        counts
    }
}

// Deferred manual current-word helpers

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

fn failed_manual_current_word_completion_error(request_id: u64, reason: String) -> SwitcherError {
    SwitcherError::VirtualKeyboardWriterTransactionFailed { request_id, reason }
}

// Layout/action decision helpers

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutCorrectionAvailability {
    Available(AppLayoutKind),
    Unavailable(InputLayoutStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordBoundaryAction {
    Evaluate {
        layout_kind: AppLayoutKind,
        authorization: SnapshotAuthorization,
    },
    ForwardUncorrected(evdev::Key),
}

fn layout_correction_decision(
    snapshot: &InputRuntimeSnapshot,
    now: Instant,
    epoch: u64,
) -> LayoutCorrectionAvailability {
    match snapshot.layout_kind_for_decision_at(now, epoch) {
        Some(kind) => LayoutCorrectionAvailability::Available(kind),
        None => LayoutCorrectionAvailability::Unavailable(snapshot.layout_status_at(now, epoch)),
    }
}

fn same_layout_fixes_allowed(snapshot: &InputRuntimeSnapshot, now: Instant, epoch: u64) -> bool {
    (snapshot.config.fix_two_capitals || snapshot.config.fix_accidental_caps_lock)
        && matches!(
            layout_correction_decision(snapshot, now, epoch),
            LayoutCorrectionAvailability::Available(
                AppLayoutKind::English | AppLayoutKind::Russian
            )
        )
}

fn manual_correction_allowed(snapshot: &InputRuntimeSnapshot, now: Instant, epoch: u64) -> bool {
    snapshot.features.manual_word_fix
        && matches!(
            layout_correction_decision(snapshot, now, epoch),
            LayoutCorrectionAvailability::Available(
                AppLayoutKind::English | AppLayoutKind::Russian
            )
        )
}

fn word_boundary_action(
    snapshot: &InputRuntimeSnapshot,
    now: Instant,
    epoch: u64,
    key: evdev::Key,
) -> WordBoundaryAction {
    match (
        snapshot.layout_kind_for_decision_at(now, epoch),
        snapshot.authorization_at(now, epoch),
    ) {
        (Some(layout_kind), Some(authorization)) => WordBoundaryAction::Evaluate {
            layout_kind,
            authorization,
        },
        _ => WordBoundaryAction::ForwardUncorrected(key),
    }
}

fn should_publish_pending_status_change(has_pending_status_change: bool) -> bool {
    has_pending_status_change
}

fn status_snapshot_is_publishable(status: InputLayoutStatus) -> bool {
    matches!(status, InputLayoutStatus::Fresh)
}

fn auto_layout_correction_supported_for_layout(layout_kind: AppLayoutKind) -> bool {
    matches!(layout_kind, AppLayoutKind::English | AppLayoutKind::Russian)
}

// Pending word commit helpers

fn pending_word_commit_action_label(pending: Option<&PendingWordCommit>) -> &'static str {
    match pending.map(|pending| &pending.action) {
        Some(PendingWordCommitAction::LayoutCorrection) => "layout-correction",
        Some(PendingWordCommitAction::SameLayoutCaseCorrection { .. }) => "same-layout-correction",
        None => "none",
    }
}

fn pending_word_commit_separator_key(pending: Option<&PendingWordCommit>) -> Option<evdev::Key> {
    pending.map(|pending| pending.separator_key)
}

fn cancel_pending_commit_with<E>(
    separator_key: evdev::Key,
    replay: impl FnOnce(evdev::Key) -> Result<(), E>,
) -> Result<(), E> {
    replay(separator_key)
}

fn pending_commit_authorized_after_adoption(
    snapshot_adopted: bool,
    snapshot: &InputRuntimeSnapshot,
    authorization: SnapshotAuthorization,
    now: Instant,
    epoch: u64,
) -> bool {
    snapshot_adopted && snapshot.authorizes_at(authorization, now, epoch)
}

// Separator suppression helpers

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SuppressedSeparatorReleaseState {
    pending_to_finish: Option<PendingWordCommit>,
}

fn take_suppressed_separator_release_state(
    suppressed_separator_key: Option<evdev::Key>,
    pending_word_commit: &mut Option<PendingWordCommit>,
    key: evdev::Key,
    value: i32,
) -> Option<SuppressedSeparatorReleaseState> {
    if !should_swallow_suppressed_separator_release(suppressed_separator_key, key, value) {
        return None;
    }

    let pending_to_finish = if pending_word_commit
        .as_ref()
        .is_some_and(|pending| pending.separator_key == key)
    {
        pending_word_commit.take()
    } else {
        None
    };

    Some(SuppressedSeparatorReleaseState { pending_to_finish })
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingWordCommitEarlyFinishState {
    pending: PendingWordCommit,
    preserved_separator_key: Option<evdev::Key>,
}

fn take_pending_word_commit_for_early_finish(
    pending_word_commit: &mut Option<PendingWordCommit>,
    key: evdev::Key,
    value: i32,
) -> Option<PendingWordCommitEarlyFinishState> {
    if value != 1 || is_modifier(key) {
        return None;
    }

    let pending = pending_word_commit.take()?;
    let preserved_separator_key =
        preserved_separator_after_early_finish(Some(&pending), key, value);
    Some(PendingWordCommitEarlyFinishState {
        pending,
        preserved_separator_key,
    })
}

// Manual correction state helpers

fn should_commit_manually_corrected_current_word(
    current_word_correction_state: CurrentWordCorrectionState,
    key: evdev::Key,
    buffer_len: usize,
) -> bool {
    matches!(
        current_word_correction_state,
        CurrentWordCorrectionState::ManuallyCorrected
    ) && buffer_len > 0
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

fn should_clear_stale_suppressed_undo_on_press(
    suppressed_undo_key: Option<evdev::Key>,
    key: evdev::Key,
    value: i32,
) -> bool {
    suppressed_undo_key == Some(key) && value == 1
}

fn should_clear_stale_suppressed_selected_hotkey_on_press(
    suppressed_hotkey_key: Option<evdev::Key>,
    key: evdev::Key,
    value: i32,
) -> bool {
    suppressed_hotkey_key == Some(key) && value == 1
}

fn selected_text_hotkey_runs_on_press(key: evdev::Key) -> bool {
    key == evdev::Key::KEY_PAUSE
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
        value == 1 && key != undo_key && !is_modifier(key) && event_timestamp > latch.armed_at
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

// Reset / invalidation helpers

enum WordTrackingEvent {
    PlainCharacter(Keystroke),
    Backspace,
    Boundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WordTrackingUpdate {
    correction_tracking_enabled: bool,
    overflow_started: bool,
}

fn apply_word_tracking_event_and_forward(
    buffer: &mut Vec<Keystroke>,
    overflowed: &mut bool,
    word_context: &mut WordContext,
    current_word_correction_state: &mut CurrentWordCorrectionState,
    manual_hotkey_latch: &mut Option<ManualHotkeyLatch>,
    event: WordTrackingEvent,
    forward: impl FnOnce() -> Result<(), SwitcherError>,
) -> Result<WordTrackingUpdate, SwitcherError> {
    let update = match event {
        WordTrackingEvent::PlainCharacter(stroke) => {
            let overflow_started = !*overflowed && buffer.len() >= MAX_CORRECTION_KEYSTROKES;
            if overflow_started {
                buffer.clear();
                *overflowed = true;
            }

            if *overflowed {
                *current_word_correction_state = CurrentWordCorrectionState::Raw;
                *manual_hotkey_latch = None;
                word_context.valid = false;
                word_context.followed_by_separator = false;
                word_context.word_before_cursor.clear();
                WordTrackingUpdate {
                    correction_tracking_enabled: false,
                    overflow_started,
                }
            } else {
                buffer.push(stroke);
                *current_word_correction_state =
                    next_current_word_state_after_plain_character_input(
                        *current_word_correction_state,
                    );
                word_context.valid = true;
                word_context.followed_by_separator = false;
                word_context.word_before_cursor.clear();
                WordTrackingUpdate {
                    correction_tracking_enabled: true,
                    overflow_started: false,
                }
            }
        }
        WordTrackingEvent::Backspace => {
            *current_word_correction_state = CurrentWordCorrectionState::Raw;
            if !*overflowed {
                if !buffer.is_empty() {
                    buffer.pop();
                } else if word_context.valid && word_context.followed_by_separator {
                    *buffer = word_context.word_before_cursor.clone();
                    word_context.followed_by_separator = false;
                }
            }
            WordTrackingUpdate {
                correction_tracking_enabled: !*overflowed && !buffer.is_empty(),
                overflow_started: false,
            }
        }
        WordTrackingEvent::Boundary => {
            clear_word_context_state(
                buffer,
                word_context,
                current_word_correction_state,
                manual_hotkey_latch,
            );
            *overflowed = false;
            WordTrackingUpdate {
                correction_tracking_enabled: false,
                overflow_started: false,
            }
        }
    };

    forward()?;
    Ok(update)
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

fn apply_corrected_word_commit_state(
    buffer: &mut Vec<Keystroke>,
    word_context: &mut WordContext,
    current_word_correction_state: &mut CurrentWordCorrectionState,
    manual_hotkey_latch: &mut Option<ManualHotkeyLatch>,
    separator_key: evdev::Key,
    corrected_buffer: Vec<Keystroke>,
) -> evdev::Key {
    if separator_key == evdev::Key::KEY_SPACE {
        word_context.valid = !corrected_buffer.is_empty();
        word_context.word_before_cursor = corrected_buffer;
        word_context.followed_by_separator = true;
        buffer.clear();
    } else {
        clear_word_context_state(
            buffer,
            word_context,
            current_word_correction_state,
            manual_hotkey_latch,
        );
    }
    separator_key
}

pub struct DaemonService {
    runtime: Arc<RuntimeState>,
    input_snapshot: InputRuntimeSnapshot,
    signal_publisher: DbusSignalPublisher,
    input_backend: InputBackendLifecycle<KeyboardInputBackendOpener>,
    keyboard: Option<KeyboardController>,
    modifiers: ModifierState,
    shared_modifiers: SharedModifierState,
    buffer: Vec<Keystroke>,
    word_buffer_overflowed: bool,
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
    dbus_signal_drop_counters: DbusSignalDropCounters,
}

impl DaemonService {
    pub fn new(runtime: Arc<RuntimeState>, connection: Connection) -> Result<Self, SwitcherError> {
        let input_snapshot = runtime.input_snapshot_before_grab();
        let shared_modifiers = SharedModifierState::default();
        let signal_publisher = DbusSignalPublisher::spawn(connection);
        let mut service = Self {
            runtime,
            input_snapshot,
            signal_publisher,
            input_backend: InputBackendLifecycle::new(KeyboardInputBackendOpener),
            keyboard: None,
            modifiers: ModifierState::default(),
            shared_modifiers,
            buffer: Vec::new(),
            word_buffer_overflowed: false,
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
            dbus_signal_drop_counters: DbusSignalDropCounters::default(),
        };
        log_input_debug("event-loop-start", "daemon input loop starting");
        service.try_initialize_input_backend()?;
        Ok(service)
    }

    pub fn run(&mut self) -> Result<(), SwitcherError> {
        let mut processed_events = 0u64;
        let mut last_heartbeat = Instant::now();

        'event_loop: loop {
            if self.runtime.should_exit() {
                self.shutdown();
                return Ok(());
            }

            self.poll_manual_completion_and_ensure_writer_healthy()?;
            self.maybe_retry_input_backend()?;

            let fetch_timeout = bounded_capture_poll_timeout(self.event_fetch_timeout());
            let fetch_result = run_writer_healthy_operation(
                self,
                DaemonService::poll_manual_completion_and_ensure_writer_healthy,
                |service| {
                    if let Some(keyboard) = service.keyboard.as_mut() {
                        keyboard.fetch_events_timeout(fetch_timeout)
                    } else {
                        std::thread::sleep(INPUT_EVENT_WAIT_TIMEOUT);
                        Ok(Vec::new())
                    }
                },
            );
            let events = match fetch_result {
                Ok(events) => events,
                Err(WriterHealthyOperationError::Health(error)) => return Err(error),
                Err(WriterHealthyOperationError::Operation(error)) => {
                    log_input_debug("keyboard-read-error", &format!("error={error}"));
                    if self.handle_runtime_input_failure(&error) {
                        continue;
                    }
                    self.shutdown();
                    return Err(error);
                }
            };

            if let Err(error) = self.poll_layout_switch_capture_expiry() {
                log_input_debug("capture-expiry-poll-error", &format!("error={error}"));
                self.shutdown();
                return Err(error);
            }

            if should_publish_pending_status_change(self.runtime.take_pending_status_change()) {
                self.publish_status_changed();
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

            let batch_result = process_writer_healthy_batch(
                self,
                events,
                DaemonService::poll_manual_completion_and_ensure_writer_healthy,
                |service, event| {
                    if let InputEventKind::Key(key) = event.kind() {
                        if let Err(error) = service.handle_key_event(
                            key,
                            event.value(),
                            event.timestamp(),
                            InputOrigin::Physical,
                        ) {
                            log_input_debug(
                                "event-handler-error",
                                &format!("key={key:?} value={} error={error}", event.value()),
                            );
                            return Err(error);
                        }
                        processed_events += 1;
                        if processed_events.is_multiple_of(EVENT_LOOP_HEARTBEAT_EVENTS)
                            || last_heartbeat.elapsed() >= EVENT_LOOP_HEARTBEAT_INTERVAL
                        {
                            let dbus_signal_drop_fields = service
                                .dbus_signal_drop_counters
                                .take_counts()
                                .heartbeat_fields();
                            if let Some(fields) = dbus_signal_drop_fields.as_deref() {
                                log_layout_debug("dbus-signal-drop-summary", fields);
                            }
                            let dbus_signal_drop_fields = dbus_signal_drop_fields
                                .map(|fields| format!(" {fields}"))
                                .unwrap_or_default();
                            log_input_debug(
                                "event-loop-heartbeat",
                                &format!(
                                    "events_processed={processed_events} selected_text_in_progress={} writer_alive={}{}",
                                    service
                                        .selected_text_runner
                                        .as_ref()
                                        .is_some_and(SelectedTextJobRunner::is_in_progress),
                                    service
                                        .keyboard
                                        .as_ref()
                                        .is_some_and(KeyboardController::is_writer_alive),
                                    dbus_signal_drop_fields,
                                ),
                            );
                            last_heartbeat = Instant::now();
                        }
                    }
                    Ok(())
                },
            );
            match batch_result {
                Ok(()) => {}
                Err(WriterHealthyBatchError::Health(error)) => return Err(error),
                Err(WriterHealthyBatchError::Event(error)) => {
                    if self.handle_runtime_input_failure(&error) {
                        continue 'event_loop;
                    }
                    self.shutdown();
                    return Err(error);
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
        let timeout = match self.manual_current_word_flow {
            ManualCurrentWordFlow::Idle => INPUT_EVENT_WAIT_TIMEOUT,
            ManualCurrentWordFlow::InFlight { .. } => MANUAL_CURRENT_WORD_IN_FLIGHT_POLL_TIMEOUT,
            ManualCurrentWordFlow::DrainingDeferredInput { .. } => Duration::ZERO,
        };
        active_keyboard_fetch_timeout(timeout, self.keyboard.is_some())
    }

    fn adopt_input_snapshot_nonblocking(&mut self) -> bool {
        match self.runtime.try_input_snapshot() {
            SnapshotTryLoad::Loaded(snapshot) => {
                self.input_snapshot = snapshot;
                true
            }
            SnapshotTryLoad::Contended => false,
            SnapshotTryLoad::Poisoned => {
                log_layout_debug("input-snapshot-read", "result=poisoned action=keep-local");
                false
            }
        }
    }

    fn ensure_writer_healthy(&mut self) -> Result<(), SwitcherError> {
        let error = self
            .keyboard
            .as_ref()
            .and_then(KeyboardController::writer_health_error);
        if let Err(error) = writer_health_result(error) {
            log_input_debug("writer-health-error", &format!("error={error}"));
            self.shutdown();
            return Err(error);
        }
        Ok(())
    }

    fn poll_manual_completion_and_ensure_writer_healthy(&mut self) -> Result<(), SwitcherError> {
        match poll_completion_before_writer_health(
            self,
            DaemonService::poll_manual_current_word_completion,
            DaemonService::ensure_writer_healthy,
        ) {
            Ok(()) => Ok(()),
            Err(CompletionBeforeWriterHealthError::Completion(error)) => {
                self.shutdown();
                Err(error)
            }
            Err(CompletionBeforeWriterHealthError::Health(error)) => Err(error),
        }
    }

    fn observe_layout_switch_capture_state_change(
        &mut self,
        state_change: Option<LayoutSwitchCaptureState>,
    ) {
        let Some(state) = state_change else {
            return;
        };
        let terminal = !state.is_active();
        self.try_publish_signal_event(
            DbusSignalEvent::LayoutSwitchCaptureStateChanged(state),
            DbusSignalEventKind::CaptureStateChanged,
        );
        if terminal {
            self.invalidate_word_context();
        }
    }

    fn poll_layout_switch_capture_expiry(&mut self) -> Result<(), SwitcherError> {
        let state_change = self
            .runtime
            .expire_layout_switch_capture_at(Instant::now())?;
        self.observe_layout_switch_capture_state_change(state_change);
        Ok(())
    }

    fn route_layout_switch_capture_event(
        &mut self,
        key: evdev::Key,
        value: i32,
    ) -> Result<CaptureRoutingControl, SwitcherError> {
        let outcome =
            self.runtime
                .route_layout_switch_capture_event_at(Instant::now(), key, value)?;
        self.observe_layout_switch_capture_state_change(outcome.state_change);

        let keyboard = &mut self.keyboard;
        apply_capture_routing_disposition(
            outcome.disposition,
            &mut self.modifiers,
            &self.shared_modifiers,
            key,
            value,
            |key, value| {
                keyboard
                    .as_mut()
                    .ok_or(SwitcherError::KeyboardNotFound)?
                    .forward_event(key, value)
            },
        )
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
            ManualCurrentWordFlow::InFlight { session }
                if session.request_id == completion.request_id =>
            {
                session
            }
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
                let _ =
                    finalize_manual_correction(&mut self.buffer, &mut self.word_context, &applied);
                self.current_word_correction_state = CurrentWordCorrectionState::ManuallyCorrected;
                self.manual_hotkey_latch = None;
                self.runtime
                    .invalidate_layout_and_request_refresh("manual-current-word-succeeded");
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
                self.runtime.invalidate_layout_and_request_refresh(
                    "manual-current-word-failed-after-mutation",
                );
                self.abort_manual_current_word_flow("manual-current-word-failed-after-mutation");
                Err(failed_manual_current_word_completion_error(
                    completion.request_id,
                    error,
                ))
            }
        }
    }

    fn abort_manual_current_word_flow(&mut self, reason: &str) {
        let flow = std::mem::replace(
            &mut self.manual_current_word_flow,
            ManualCurrentWordFlow::Idle,
        );
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
            if let ManualCurrentWordFlow::DrainingDeferredInput { session } =
                &self.manual_current_word_flow
            {
                if should_restart_manual_current_word_after_drain(
                    session.deferred_input.len(),
                    session.retry_after_drain_requested,
                ) {
                    let undo_key = session.undo_key;
                    self.manual_current_word_flow = ManualCurrentWordFlow::Idle;
                    self.adopt_input_snapshot_nonblocking();
                    let config = self.input_snapshot.config.clone();
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

        self.handle_key_event(
            event.key,
            event.value,
            event.timestamp,
            InputOrigin::DeferredReplay,
        )
    }

    fn manual_current_word_flow_seen_real_next_step(&self) -> bool {
        match &self.manual_current_word_flow {
            ManualCurrentWordFlow::InFlight { session }
            | ManualCurrentWordFlow::DrainingDeferredInput { session } => {
                session.seen_real_next_step
            }
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
        self.adopt_input_snapshot_nonblocking();

        if self.suppressed_hotkey_key == Some(key) {
            if value == 0 {
                self.suppressed_hotkey_key = None;
                self.maybe_run_pending_selected_text_switch()?;
                return Ok(());
            }

            if should_clear_stale_suppressed_selected_hotkey_on_press(
                self.suppressed_hotkey_key,
                key,
                value,
            ) {
                log_input_debug(
                    "suppressed-selected-hotkey-clear",
                    &format!("reason=stale-press key={key:?} value={value}"),
                );
                self.suppressed_hotkey_key = None;
                if self.pending_selected_text_switch {
                    self.maybe_run_pending_selected_text_switch()?;
                    return Ok(());
                }
            } else {
                self.maybe_run_pending_selected_text_switch()?;
                return Ok(());
            }
        }

        if should_swallow_suppressed_undo_release(self.suppressed_undo_key, key, value) {
            self.suppressed_undo_key = None;
            return Ok(());
        }

        if should_clear_stale_suppressed_undo_on_press(self.suppressed_undo_key, key, value) {
            log_input_debug(
                "suppressed-undo-clear",
                &format!("reason=stale-press key={key:?} value={value}"),
            );
            self.suppressed_undo_key = None;
        }

        if self.suppressed_undo_key == Some(key) {
            if value == 0 {
                self.suppressed_undo_key = None;
            }
            return Ok(());
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
                let should_log_pending_take = self
                    .pending_word_commit
                    .as_ref()
                    .is_some_and(|pending| pending.separator_key == key);
                if should_log_pending_take {
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
                }
                let release_state = take_suppressed_separator_release_state(
                    self.suppressed_separator_key,
                    &mut self.pending_word_commit,
                    key,
                    value,
                )
                .expect("suppressed separator release already matched");
                if let Some(pending) = release_state.pending_to_finish {
                    let config = self.input_snapshot.config.clone();
                    self.finish_pending_word_commit(pending, &config)?;
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

        if should_route_capture_before_manual_current_word(origin)
            && self.route_layout_switch_capture_event(key, value)? == CaptureRoutingControl::Return
        {
            return Ok(());
        }

        match manual_current_word_physical_event_action(
            self.has_active_manual_current_word_flow(),
            hotkey_trigger_to_evdev_key(
                self.input_snapshot.config.manual_correction_hotkey.trigger,
            ),
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

        let config = self.input_snapshot.config.clone();
        self.modifiers.update(key, value);
        self.shared_modifiers.store(self.modifiers);

        if should_invalidate_for_wayland_focus_switch_shortcut(
            self.input_snapshot.session_type,
            self.modifiers,
            key,
            value,
        ) {
            self.handle_non_key_invalidation(
                "wayland-focus-switch-invalidation",
                "word context invalidated by Wayland focus-switch shortcut",
            );
        }

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

        let manual_correction_key =
            hotkey_trigger_to_evdev_key(config.manual_correction_hotkey.trigger);
        if should_clear_manual_hotkey_latch_on_key_press(
            self.manual_hotkey_latch,
            manual_correction_key,
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
            let early_finish = take_pending_word_commit_for_early_finish(
                &mut self.pending_word_commit,
                key,
                value,
            )
            .expect("pending early finish already matched");
            let pending = early_finish.pending;
            let preserved_separator = early_finish.preserved_separator_key;
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
            if manual_correction_hotkey_matches(
                config.manual_correction_hotkey,
                self.modifiers,
                key,
                value,
            ) {
                return Ok(());
            }
        }

        if self.handle_layout_shortcut_if_matched(&config, key, value)? {
            return Ok(());
        }

        let settings_hotkey_capture_inhibited = self.runtime.settings_hotkey_capture_inhibited();

        if selected_text_hotkey_matches_when_allowed(
            config.selected_text_hotkey,
            self.modifiers,
            key,
            value,
            settings_hotkey_capture_inhibited,
        ) {
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
            if selected_text_hotkey_runs_on_press(key) {
                self.suppressed_hotkey_key = None;
                self.maybe_run_pending_selected_text_switch()?;
                self.suppressed_hotkey_key = Some(key);
            }
            return Ok(());
        }

        if value == 0 {
            let result = self.keyboard_mut()?.forward_event(key, value);
            if result.is_ok() {
                self.maybe_run_pending_selected_text_switch()?;
            }
            return result;
        }

        if manual_correction_hotkey_matches_when_allowed(
            config.manual_correction_hotkey,
            self.modifiers,
            key,
            value,
            settings_hotkey_capture_inhibited,
        ) {
            self.suppressed_undo_key = Some(key);
            if self.begin_deferred_manual_current_word_correction(
                manual_correction_key,
                &config,
                origin,
            )? {
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
                    manual_correction_key,
                    CorrectionPath::ManualHotkey,
                    applied.used_current_buffer,
                    SystemTime::now(),
                );
            }
            return Ok(());
        }

        match key {
            evdev::Key::KEY_SPACE => {
                if self.word_buffer_overflowed {
                    let keyboard = self
                        .keyboard
                        .as_mut()
                        .ok_or(SwitcherError::KeyboardNotFound)?;
                    apply_word_tracking_event_and_forward(
                        &mut self.buffer,
                        &mut self.word_buffer_overflowed,
                        &mut self.word_context,
                        &mut self.current_word_correction_state,
                        &mut self.manual_hotkey_latch,
                        WordTrackingEvent::Boundary,
                        || keyboard.forward_event(key, value),
                    )?;
                    return Ok(());
                }
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
                let now = Instant::now();
                let epoch = self.runtime.input_layout_epoch();
                let (effective_layout_kind, authorization) =
                    match word_boundary_action(&self.input_snapshot, now, epoch, key) {
                        WordBoundaryAction::Evaluate {
                            layout_kind,
                            authorization,
                        } => (layout_kind, authorization),
                        WordBoundaryAction::ForwardUncorrected(_) => {
                            let status = self.input_snapshot.layout_status_at(now, epoch);
                            let request = self.runtime.request_layout_refresh();
                            log_layout_debug(
                                "space-correction-skip",
                                &format!("status={status:?} request={request:?}"),
                            );
                            self.word_context.valid = !self.buffer.is_empty();
                            self.word_context.word_before_cursor = self.buffer.clone();
                            self.word_context.followed_by_separator = true;
                            self.buffer.clear();
                            self.word_buffer_overflowed = false;
                            return self.keyboard_mut()?.forward_event(key, value);
                        }
                    };
                let same_layout_plan =
                    if same_layout_fixes_allowed(&self.input_snapshot, now, epoch) {
                        same_layout_case_correction_plan(
                            &self.buffer,
                            effective_layout_kind,
                            config.fix_two_capitals,
                            config.fix_accidental_caps_lock,
                        )
                    } else {
                        None
                    };
                let should_switch_word = should_switch(&self.buffer, effective_layout_kind);
                let corrected = self.input_snapshot.enabled
                    && config.auto_switch_enabled
                    && self.input_snapshot.features.auto_switch
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
                        "enabled={} auto_switch_enabled={} feature_auto_switch={} effective_layout_kind={effective_layout_kind:?} should_switch={} same_layout_case_fix={} selected_path={} buffer_len={}",
                        self.input_snapshot.enabled,
                        config.auto_switch_enabled,
                        self.input_snapshot.features.auto_switch,
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
                        authorization,
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
                        authorization,
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
                self.word_buffer_overflowed = false;
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
                let now = Instant::now();
                let epoch = self.runtime.input_layout_epoch();
                let (layout_kind, authorization) =
                    match word_boundary_action(&self.input_snapshot, now, epoch, key) {
                        WordBoundaryAction::Evaluate {
                            layout_kind,
                            authorization,
                        } => (layout_kind, authorization),
                        WordBoundaryAction::ForwardUncorrected(_) => {
                            let status = self.input_snapshot.layout_status_at(now, epoch);
                            let request = self.runtime.request_layout_refresh();
                            log_layout_debug(
                                "boundary-case-correction-skip",
                                &format!("key={key:?} status={status:?} request={request:?}"),
                            );
                            self.invalidate_word_context();
                            return self.keyboard_mut()?.forward_event(key, value);
                        }
                    };
                let same_layout_plan = same_layout_fixes_allowed(&self.input_snapshot, now, epoch)
                    .then(|| {
                        same_layout_case_correction_plan(
                            &self.buffer,
                            layout_kind,
                            config.fix_two_capitals,
                            config.fix_accidental_caps_lock,
                        )
                    })
                    .flatten();
                if let Some(plan) = same_layout_plan {
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
                        authorization,
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
                let keyboard = self
                    .keyboard
                    .as_mut()
                    .ok_or(SwitcherError::KeyboardNotFound)?;
                apply_word_tracking_event_and_forward(
                    &mut self.buffer,
                    &mut self.word_buffer_overflowed,
                    &mut self.word_context,
                    &mut self.current_word_correction_state,
                    &mut self.manual_hotkey_latch,
                    WordTrackingEvent::Backspace,
                    || keyboard.forward_event(key, value),
                )?;
                Ok(())
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
                    let keyboard = self
                        .keyboard
                        .as_mut()
                        .ok_or(SwitcherError::KeyboardNotFound)?;
                    let update = apply_word_tracking_event_and_forward(
                        &mut self.buffer,
                        &mut self.word_buffer_overflowed,
                        &mut self.word_context,
                        &mut self.current_word_correction_state,
                        &mut self.manual_hotkey_latch,
                        WordTrackingEvent::PlainCharacter(current_stroke),
                        || keyboard.forward_event(key, value),
                    )?;
                    if update.overflow_started {
                        log_input_debug(
                            "word-buffer-limit",
                            &format!(
                                "limit={} correction_tracking=false",
                                MAX_CORRECTION_KEYSTROKES
                            ),
                        );
                    }
                    self.maybe_run_pending_selected_text_switch()?;
                    return Ok(());
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

    fn handle_layout_shortcut_if_matched(
        &mut self,
        config: &crate::daemon::runtime::RuntimeConfigSnapshot,
        key: evdev::Key,
        value: i32,
    ) -> Result<bool, SwitcherError> {
        if !self
            .modifiers
            .matches_layout_switch_combo(config.layout_switch_combo, key, value)
        {
            return Ok(false);
        }

        if self.layout_shortcut_latched {
            log_layout_debug(
                "layout-shortcut-repeat-ignored",
                &format!(
                    "combo={:?} key={key:?} value={value}",
                    config.layout_switch_combo
                ),
            );
            return Ok(true);
        }

        self.layout_shortcut_latched = true;
        let current_layout_kind = self.current_layout_kind();
        log_layout_debug(
            "observed-layout-shortcut",
            &format!(
                "combo={:?} key={key:?} value={value} shift={} ctrl={} alt={} layout_before={current_layout_kind:?}",
                config.layout_switch_combo,
                self.modifiers.is_shift_pressed(),
                self.modifiers.is_ctrl_pressed(),
                self.modifiers.is_alt_pressed(),
            ),
        );
        self.runtime
            .invalidate_layout_and_request_refresh("physical-layout-shortcut");
        self.invalidate_word_context();
        // Preserve normal OS handling: the matched physical shortcut continues
        // through the ordinary forwarding path while confirmation happens off-thread.
        Ok(false)
    }

    fn apply_selected_text_switch(&mut self) -> Result<(), SwitcherError> {
        if !self.input_snapshot.features.selected_text_switch {
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
        let snapshot_adopted = self.adopt_input_snapshot_nonblocking();
        if !self.pending_commit_is_authorized_after_adoption(snapshot_adopted, &pending) {
            log_layout_debug(
                "pending-word-commit-cancel",
                &format!(
                    "separator_key={:?} authorization={:?} current_epoch={}",
                    pending.separator_key,
                    pending.authorization,
                    self.runtime.input_layout_epoch(),
                ),
            );
            return self.cancel_pending_word_commit(pending.separator_key);
        }
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

    fn pending_commit_is_authorized_after_adoption(
        &self,
        snapshot_adopted: bool,
        pending: &PendingWordCommit,
    ) -> bool {
        pending_commit_authorized_after_adoption(
            snapshot_adopted,
            &self.input_snapshot,
            pending.authorization,
            Instant::now(),
            self.runtime.input_layout_epoch(),
        )
    }

    fn cancel_pending_word_commit(
        &mut self,
        separator_key: evdev::Key,
    ) -> Result<(), SwitcherError> {
        let raw_buffer = self.buffer.clone();
        cancel_pending_commit_with(separator_key, |key| {
            self.commit_corrected_word(key, raw_buffer)
        })
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
        let replay_separator_key = apply_corrected_word_commit_state(
            &mut self.buffer,
            &mut self.word_context,
            &mut self.current_word_correction_state,
            &mut self.manual_hotkey_latch,
            separator_key,
            corrected_buffer,
        );
        self.word_buffer_overflowed = false;
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
        self.keyboard_mut()?.type_separator(replay_separator_key)
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
            "running selected-text switch after hotkey trigger",
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
        let outcome = self
            .keyboard_mut()?
            .apply_correction(&prepared.plan, config, modifiers)?;
        if matches!(
            outcome.layout_switch,
            CorrectionLayoutSwitchOutcome::AppliedUinput
                | CorrectionLayoutSwitchOutcome::AppliedX11
                | CorrectionLayoutSwitchOutcome::AppliedCinnamonXkbXtest
        ) {
            self.runtime
                .invalidate_layout_and_request_refresh("writer-layout-switch");
            log_layout_debug(
                "correction-layout-invalidation",
                &format!(
                    "path={} outcome={:?}",
                    correction_path.as_str(),
                    outcome.layout_switch
                ),
            );
        }
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
        let now = Instant::now();
        let epoch = self.runtime.input_layout_epoch();
        if !manual_correction_allowed(&self.input_snapshot, now, epoch) {
            if self.input_snapshot.features.manual_word_fix {
                let _ = self.layout_kind_for_current_decision("manual-correction");
            }
            return Ok(None);
        }

        let Some(current_layout_kind) = self.layout_kind_for_current_decision("manual-correction")
        else {
            return Ok(None);
        };
        log_layout_debug(
            "correction-start",
            &format!(
                "path={} combo={:?} current_layout_kind={current_layout_kind:?} buffer_len={} fallback_buffer_len={} followed_by_separator={}",
                correction_path.as_str(),
                config.layout_switch_combo,
                self.buffer.len(),
                fallback_buffer.len(),
                self.word_context.followed_by_separator,
            ),
        );
        if !matches!(
            current_layout_kind,
            AppLayoutKind::English | AppLayoutKind::Russian
        ) {
            return Ok(None);
        }

        let used_current_buffer = !self.buffer.is_empty();
        let Some(mut plan) = manual_correction_plan(
            &self.buffer,
            fallback_buffer,
            self.word_context.followed_by_separator,
            current_layout_kind,
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

    fn publish_status_changed(&mut self) {
        if !self.adopt_input_snapshot_nonblocking() {
            self.runtime.defer_pending_status_change();
            return;
        }
        let epoch = self.runtime.input_layout_epoch();
        let status = self.input_snapshot.layout_status_at(Instant::now(), epoch);
        if !status_snapshot_is_publishable(status) {
            self.runtime.defer_pending_status_change();
            log_layout_debug(
                "status-signal-deferred",
                &format!("status={status:?} refresh=background-coordinator"),
            );
            return;
        }
        let enabled = self.input_snapshot.enabled;
        let layout = legacy_current_layout_bool(&self.input_snapshot.layout_state);
        log_layout_debug(
            "status-signal",
            &format!(
                "enabled={} current_layout={}",
                enabled,
                if layout { "EN" } else { "RU" }
            ),
        );
        self.try_publish_signal_event(
            DbusSignalEvent::StatusChanged { enabled, layout },
            DbusSignalEventKind::StatusChanged,
        );
    }

    fn try_publish_signal_event(
        &mut self,
        event: DbusSignalEvent,
        event_kind: DbusSignalEventKind,
    ) {
        if self.signal_publisher.try_publish(event) {
            return;
        }

        self.dbus_signal_drop_counters.record(event_kind);
    }

    // Input backend lifecycle

    fn try_initialize_input_backend(&mut self) -> Result<(), SwitcherError> {
        if let Some(opened) = self
            .input_backend
            .initialize(self.shared_modifiers.clone(), Instant::now())?
        {
            self.install_opened_input_backend(opened)?;
        }
        Ok(())
    }

    fn maybe_retry_input_backend(&mut self) -> Result<(), SwitcherError> {
        if let Some(opened) = self
            .input_backend
            .try_recover(self.shared_modifiers.clone(), Instant::now())?
        {
            self.install_opened_input_backend(opened)?;
        }
        Ok(())
    }

    fn install_opened_input_backend(
        &mut self,
        opened: OpenedInputBackend<ActiveInputBackend>,
    ) -> Result<(), SwitcherError> {
        let backend = admit_input_backend_install(
            opened.backend,
            self.keyboard.is_some() || self.selected_text_runner.is_some(),
            InputBackendHandle::shutdown,
        )?;
        let ActiveInputBackend {
            keyboard,
            selected_text_runner,
            initial_caps_lock_active,
        } = backend;
        let mut modifiers = ModifierState::default();
        modifiers.set_caps_lock_active(initial_caps_lock_active);
        self.modifiers = modifiers;
        self.shared_modifiers.store(self.modifiers);
        self.keyboard = Some(keyboard);
        self.selected_text_runner = Some(selected_text_runner);
        Ok(())
    }

    fn handle_runtime_input_failure(&mut self, error: &SwitcherError) -> bool {
        if self
            .input_backend
            .record_runtime_failure(error, Instant::now())
        {
            self.reset_transient_input_state("input-backend-unavailable");
            self.drop_active_input_backend();
            return true;
        }

        false
    }

    fn drop_active_input_backend(&mut self) {
        let reset_result = self.runtime.reset_layout_switch_capture_input_epoch();
        let state_change = reset_capture_epoch_then_shutdown_backend(reset_result, || {
            // Drop selected-text transport clones before stopping the writer so the
            // virtual keyboard shutdown is not held open by an idle helper worker.
            self.selected_text_runner = None;

            if let Some(mut keyboard) = self.keyboard.take() {
                keyboard.shutdown();
            }
        });
        self.observe_layout_switch_capture_state_change(state_change);
    }

    // Transient input state reset / invalidation

    fn reset_transient_input_state(&mut self, reason: &str) {
        log_input_debug("transient-input-reset", &format!("reason={reason}"));
        clear_word_context_state(
            &mut self.buffer,
            &mut self.word_context,
            &mut self.current_word_correction_state,
            &mut self.manual_hotkey_latch,
        );
        self.word_buffer_overflowed = false;
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
        self.keyboard
            .as_mut()
            .ok_or(SwitcherError::KeyboardNotFound)
    }

    fn invalidate_word_context(&mut self) {
        clear_word_context_state(
            &mut self.buffer,
            &mut self.word_context,
            &mut self.current_word_correction_state,
            &mut self.manual_hotkey_latch,
        );
        self.word_buffer_overflowed = false;
    }

    fn can_correct_word_before_cursor(&self) -> bool {
        self.word_context.valid
            && self.buffer.is_empty()
            && self.word_context.followed_by_separator
            && !self.word_context.word_before_cursor.is_empty()
    }

    fn layout_kind_for_current_decision(&self, reason: &str) -> Option<AppLayoutKind> {
        let now = Instant::now();
        let epoch = self.runtime.input_layout_epoch();
        let kind = self.input_snapshot.layout_kind_for_decision_at(now, epoch);
        if kind.is_none() {
            let status = self.input_snapshot.layout_status_at(now, epoch);
            let request = self.runtime.request_layout_refresh();
            log_layout_debug(
                "layout-dependent-action-skip",
                &format!("reason={reason} status={status:?} request={request:?}"),
            );
        }
        kind
    }

    fn current_layout_kind(&self) -> AppLayoutKind {
        match &self.input_snapshot.layout_state {
            CurrentLayoutState::Known { layout, .. } => layout.kind,
            CurrentLayoutState::Unknown { .. } => AppLayoutKind::Unknown,
        }
    }
}

fn selected_text_hotkey_matches(
    hotkey: HotkeySpec,
    modifiers: ModifierState,
    key: evdev::Key,
    value: i32,
) -> bool {
    if value != 1 || key != hotkey_trigger_to_evdev_key(hotkey.trigger) {
        return false;
    }

    hotkey.matches(hotkey.trigger, hotkey_modifiers_from_state(modifiers))
}

fn selected_text_hotkey_matches_when_allowed(
    hotkey: HotkeySpec,
    modifiers: ModifierState,
    key: evdev::Key,
    value: i32,
    settings_hotkey_capture_inhibited: bool,
) -> bool {
    !settings_hotkey_capture_inhibited
        && selected_text_hotkey_matches(hotkey, modifiers, key, value)
}

fn manual_correction_hotkey_matches(
    hotkey: HotkeySpec,
    modifiers: ModifierState,
    key: evdev::Key,
    value: i32,
) -> bool {
    value == 1
        && key == hotkey_trigger_to_evdev_key(hotkey.trigger)
        && hotkey.matches(hotkey.trigger, hotkey_modifiers_from_state(modifiers))
}

fn manual_correction_hotkey_matches_when_allowed(
    hotkey: HotkeySpec,
    modifiers: ModifierState,
    key: evdev::Key,
    value: i32,
    settings_hotkey_capture_inhibited: bool,
) -> bool {
    !settings_hotkey_capture_inhibited
        && manual_correction_hotkey_matches(hotkey, modifiers, key, value)
}

fn hotkey_modifiers_from_state(modifiers: ModifierState) -> HotkeyModifiers {
    HotkeyModifiers::new(
        modifiers.is_shift_pressed(),
        modifiers.is_ctrl_pressed(),
        modifiers.is_alt_pressed(),
    )
}

pub(crate) fn should_invalidate_for_wayland_focus_switch_shortcut(
    session_type: SessionType,
    modifiers: ModifierState,
    key: evdev::Key,
    value: i32,
) -> bool {
    session_type == SessionType::Wayland && is_wayland_focus_switch_shortcut(modifiers, key, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SelectedTextHotkey, SessionType, UndoKey};
    use evdev::Key;

    // Test helpers

    fn stroke(key: Key) -> Keystroke {
        Keystroke {
            key,
            shift: false,
            caps_lock: false,
        }
    }

    fn test_snapshot_authorization() -> SnapshotAuthorization {
        SnapshotAuthorization {
            config_generation: 1,
            layout_generation: 1,
            layout_epoch: 1,
        }
    }

    fn test_pending_word_commit(separator_key: Key) -> PendingWordCommit {
        PendingWordCommit {
            separator_key,
            action: PendingWordCommitAction::LayoutCorrection,
            authorization: test_snapshot_authorization(),
        }
    }

    #[test]
    fn word_tracking_reducer_forwards_overflow_until_a_real_boundary_resets_it() {
        let mut buffer = Vec::new();
        let mut overflowed = false;
        let mut word_context = WordContext::default();
        let mut correction_state = CurrentWordCorrectionState::Raw;
        let mut manual_hotkey_latch = None;
        let mut forwarded_physical_characters = 0usize;

        for _ in 0..MAX_CORRECTION_KEYSTROKES {
            let update = apply_word_tracking_event_and_forward(
                &mut buffer,
                &mut overflowed,
                &mut word_context,
                &mut correction_state,
                &mut manual_hotkey_latch,
                WordTrackingEvent::PlainCharacter(stroke(Key::KEY_A)),
                || {
                    forwarded_physical_characters += 1;
                    Ok::<(), SwitcherError>(())
                },
            )
            .unwrap();
            assert!(update.correction_tracking_enabled);
            assert!(!update.overflow_started);
        }
        assert_eq!(buffer.len(), MAX_CORRECTION_KEYSTROKES);
        assert!(!overflowed);

        let overflow = apply_word_tracking_event_and_forward(
            &mut buffer,
            &mut overflowed,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            WordTrackingEvent::PlainCharacter(stroke(Key::KEY_B)),
            || {
                forwarded_physical_characters += 1;
                Ok::<(), SwitcherError>(())
            },
        )
        .unwrap();
        assert!(buffer.is_empty());
        assert!(overflowed);
        assert!(overflow.overflow_started);
        assert!(!overflow.correction_tracking_enabled);

        let suppressed = apply_word_tracking_event_and_forward(
            &mut buffer,
            &mut overflowed,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            WordTrackingEvent::PlainCharacter(stroke(Key::KEY_C)),
            || {
                forwarded_physical_characters += 1;
                Ok::<(), SwitcherError>(())
            },
        )
        .unwrap();
        assert!(buffer.is_empty());
        assert!(!suppressed.overflow_started);
        assert!(!suppressed.correction_tracking_enabled);

        let backspace = apply_word_tracking_event_and_forward(
            &mut buffer,
            &mut overflowed,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            WordTrackingEvent::Backspace,
            || {
                forwarded_physical_characters += 1;
                Ok::<(), SwitcherError>(())
            },
        )
        .unwrap();
        assert!(buffer.is_empty());
        assert!(overflowed, "backspace must not re-enable lost tracking");
        assert!(!backspace.correction_tracking_enabled);

        apply_word_tracking_event_and_forward(
            &mut buffer,
            &mut overflowed,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            WordTrackingEvent::Boundary,
            || {
                forwarded_physical_characters += 1;
                Ok::<(), SwitcherError>(())
            },
        )
        .unwrap();
        assert!(!overflowed, "a real boundary must clear the overflow latch");

        let next_word = apply_word_tracking_event_and_forward(
            &mut buffer,
            &mut overflowed,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            WordTrackingEvent::PlainCharacter(stroke(Key::KEY_D)),
            || {
                forwarded_physical_characters += 1;
                Ok::<(), SwitcherError>(())
            },
        )
        .unwrap();
        assert_eq!(buffer.len(), 1, "a boundary starts a fresh tracked word");
        assert!(!overflowed);
        assert!(next_word.correction_tracking_enabled);
        assert_eq!(forwarded_physical_characters, MAX_CORRECTION_KEYSTROKES + 5);
    }

    fn modifiers_with(pressed_keys: &[Key]) -> ModifierState {
        let mut modifiers = ModifierState::default();
        for key in pressed_keys {
            modifiers.update(*key, 1);
        }
        modifiers
    }

    #[test]
    fn capture_reset_backend_shutdown_runs_once_when_reset_lock_is_poisoned() {
        let mut shutdown_count = 0;

        let state_change = reset_capture_epoch_then_shutdown_backend(
            Err(crate::error::CaptureError::LockPoisoned),
            || shutdown_count += 1,
        );

        assert_eq!(shutdown_count, 1);
        assert_eq!(state_change, None);
    }

    #[test]
    fn capture_reset_backend_shutdown_preserves_terminal_transition() {
        let mut shutdown_count = 0;
        let cancelled = LayoutSwitchCaptureState::cancelled();

        let state_change =
            reset_capture_epoch_then_shutdown_backend(Ok(Some(cancelled.clone())), || {
                shutdown_count += 1
            });

        assert_eq!(shutdown_count, 1);
        assert_eq!(state_change, Some(cancelled));
    }

    #[test]
    fn occupied_install_slot_releases_new_grab_before_rejecting_backend() {
        let mut phases = Vec::new();

        let result = admit_input_backend_install("new-backend", true, |backend| {
            phases.push(format!("release-{backend}"));
        });

        assert!(matches!(
            result,
            Err(SwitcherError::InputBackendAlreadyActive)
        ));
        assert_eq!(phases, vec!["release-new-backend"]);
    }

    #[test]
    fn capture_routing_suppress_has_no_modifier_or_forward_side_effect() {
        let mut modifiers = ModifierState::default();
        let shared_modifiers = SharedModifierState::default();
        let mut forwarded = Vec::new();

        let control = apply_capture_routing_disposition(
            crate::daemon::capture::CaptureEventDisposition::Suppress,
            &mut modifiers,
            &shared_modifiers,
            Key::KEY_LEFTSHIFT,
            1,
            |key, value| {
                forwarded.push((key, value));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(control, CaptureRoutingControl::Return);
        assert!(!modifiers.is_shift_pressed());
        assert!(!shared_modifiers.snapshot().is_shift_pressed());
        assert!(forwarded.is_empty());
    }

    #[test]
    fn capture_routing_forward_direct_updates_both_modifiers_and_forwards_once() {
        let mut modifiers = ModifierState::default();
        let shared_modifiers = SharedModifierState::default();
        let mut forwarded = Vec::new();

        let control = apply_capture_routing_disposition(
            crate::daemon::capture::CaptureEventDisposition::ForwardDirect,
            &mut modifiers,
            &shared_modifiers,
            Key::KEY_LEFTSHIFT,
            1,
            |key, value| {
                forwarded.push((key, value));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(control, CaptureRoutingControl::Return);
        assert!(modifiers.is_shift_pressed());
        assert!(shared_modifiers.snapshot().is_shift_pressed());
        assert_eq!(forwarded, vec![(Key::KEY_LEFTSHIFT, 1)]);
    }

    #[test]
    fn capture_routing_pass_through_has_no_early_side_effect() {
        let mut modifiers = ModifierState::default();
        let shared_modifiers = SharedModifierState::default();
        let mut forwarded = Vec::new();

        let control = apply_capture_routing_disposition(
            crate::daemon::capture::CaptureEventDisposition::PassThrough,
            &mut modifiers,
            &shared_modifiers,
            Key::KEY_LEFTSHIFT,
            1,
            |key, value| {
                forwarded.push((key, value));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(control, CaptureRoutingControl::Continue);
        assert!(!modifiers.is_shift_pressed());
        assert!(!shared_modifiers.snapshot().is_shift_pressed());
        assert!(forwarded.is_empty());
    }

    #[test]
    fn capture_routing_precedes_manual_flow_only_for_physical_input() {
        assert!(should_route_capture_before_manual_current_word(
            InputOrigin::Physical
        ));
        assert!(!should_route_capture_before_manual_current_word(
            InputOrigin::DeferredReplay
        ));
        assert!(!should_route_capture_before_manual_current_word(
            InputOrigin::DeferredRetry
        ));
    }

    #[test]
    fn capture_expiry_poll_timeout_is_never_over_one_hundred_milliseconds() {
        assert_eq!(
            bounded_capture_poll_timeout(Duration::from_secs(1)),
            Duration::from_millis(100)
        );
        assert_eq!(
            bounded_capture_poll_timeout(Duration::from_millis(10)),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn clone_writer_work_bounds_event_fetch_to_health_quantum() {
        assert!(WRITER_HEALTH_POLL_QUANTUM <= Duration::from_millis(5));
        assert_eq!(
            active_keyboard_fetch_timeout(INPUT_EVENT_WAIT_TIMEOUT, true),
            WRITER_HEALTH_POLL_QUANTUM
        );
        assert_eq!(
            active_keyboard_fetch_timeout(INPUT_EVENT_WAIT_TIMEOUT, false),
            INPUT_EVENT_WAIT_TIMEOUT
        );
        assert!(MANUAL_CURRENT_WORD_IN_FLIGHT_POLL_TIMEOUT <= WRITER_HEALTH_POLL_QUANTUM);
    }

    #[test]
    fn writer_health_failure_after_fetch_stops_batch_before_first_event() {
        let mut handled = Vec::new();
        let result = process_writer_healthy_batch(
            &mut handled,
            [1, 2],
            |_handled| {
                Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 301 })
            },
            |handled, event| {
                handled.push(event);
                Ok::<(), SwitcherError>(())
            },
        );

        assert!(handled.is_empty());
        assert!(matches!(
            result,
            Err(WriterHealthyBatchError::Health(
                SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 301 }
            ))
        ));
    }

    #[test]
    fn writer_health_failure_after_failed_fetch_takes_priority() {
        #[derive(Default)]
        struct State {
            trace: Vec<&'static str>,
            health_checks: usize,
        }

        let mut state = State::default();
        let result = run_writer_healthy_operation(
            &mut state,
            |state| {
                state.trace.push("health");
                state.health_checks += 1;
                if state.health_checks == 2 {
                    Err(SwitcherError::VirtualKeyboardWriterDisconnected)
                } else {
                    Ok(())
                }
            },
            |state| {
                state.trace.push("fetch");
                Err::<Vec<evdev::InputEvent>, _>(SwitcherError::Io(std::io::Error::other(
                    "recoverable fetch failure",
                )))
            },
        );

        assert!(matches!(
            result,
            Err(WriterHealthyOperationError::Health(
                SwitcherError::VirtualKeyboardWriterDisconnected
            ))
        ));
        assert_eq!(state.trace, vec!["health", "fetch", "health"]);
    }

    #[test]
    fn writer_health_failure_after_successful_fetch_discards_events() {
        #[derive(Default)]
        struct State {
            trace: Vec<&'static str>,
            health_checks: usize,
        }

        let mut state = State::default();
        let result = run_writer_healthy_operation(
            &mut state,
            |state| {
                state.trace.push("health");
                state.health_checks += 1;
                if state.health_checks == 2 {
                    Err(SwitcherError::VirtualKeyboardWriterDisconnected)
                } else {
                    Ok(())
                }
            },
            |state| {
                state.trace.push("fetch");
                Ok::<_, SwitcherError>(vec![1, 2])
            },
        );

        assert!(matches!(
            result,
            Err(WriterHealthyOperationError::Health(
                SwitcherError::VirtualKeyboardWriterDisconnected
            ))
        ));
        assert_eq!(state.trace, vec!["health", "fetch", "health"]);
    }

    #[test]
    fn completion_gate_applies_success_before_writer_health() {
        #[derive(Default)]
        struct State {
            trace: Vec<&'static str>,
            completion_applied: bool,
        }

        let mut state = State::default();
        let result = poll_completion_before_writer_health(
            &mut state,
            |state| {
                state.trace.push("completion");
                state.completion_applied = true;
                Ok::<(), SwitcherError>(())
            },
            |state| {
                state.trace.push("health");
                if state.completion_applied {
                    Ok(())
                } else {
                    Err(SwitcherError::VirtualKeyboardWriterDisconnected)
                }
            },
        );

        assert!(result.is_ok());
        assert_eq!(state.trace, vec!["completion", "health"]);
    }

    #[test]
    fn failed_completion_precedes_dead_health_with_detailed_error() {
        #[derive(Default)]
        struct State {
            trace: Vec<&'static str>,
            health_called: bool,
        }

        let mut state = State::default();
        let result = poll_completion_before_writer_health(
            &mut state,
            |state| {
                state.trace.push("completion-log");
                Err(failed_manual_current_word_completion_error(
                    701,
                    "detailed post-mutation failure".to_string(),
                ))
            },
            |state| {
                state.health_called = true;
                Err(SwitcherError::VirtualKeyboardWriterDisconnected)
            },
        );

        assert!(matches!(
            result,
            Err(CompletionBeforeWriterHealthError::Completion(
                SwitcherError::VirtualKeyboardWriterTransactionFailed {
                    request_id: 701,
                    reason,
                }
            )) if reason == "detailed post-mutation failure"
        ));
        assert_eq!(state.trace, vec!["completion-log"]);
        assert!(!state.health_called);
    }

    #[test]
    fn post_operation_gate_polls_completion_before_health() {
        #[derive(Default)]
        struct State {
            trace: Vec<&'static str>,
        }

        let mut state = State::default();
        let result = run_writer_healthy_operation(
            &mut state,
            |state| match poll_completion_before_writer_health(
                state,
                |state| {
                    state.trace.push("completion");
                    Ok::<(), SwitcherError>(())
                },
                |state| {
                    state.trace.push("health");
                    Ok::<(), SwitcherError>(())
                },
            ) {
                Ok(()) => Ok(()),
                Err(CompletionBeforeWriterHealthError::Completion(error))
                | Err(CompletionBeforeWriterHealthError::Health(error)) => Err(error),
            },
            |state| {
                state.trace.push("operation");
                Ok::<_, SwitcherError>(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(
            state.trace,
            vec!["completion", "health", "operation", "completion", "health"]
        );
    }

    #[test]
    fn batch_gate_polls_completion_before_health_for_every_event() {
        #[derive(Default)]
        struct State {
            trace: Vec<&'static str>,
        }

        let mut state = State::default();
        let result = process_writer_healthy_batch(
            &mut state,
            ["event-1", "event-2"],
            |state| match poll_completion_before_writer_health(
                state,
                |state| {
                    state.trace.push("completion");
                    Ok::<(), SwitcherError>(())
                },
                |state| {
                    state.trace.push("health");
                    Ok::<(), SwitcherError>(())
                },
            ) {
                Ok(()) => Ok(()),
                Err(CompletionBeforeWriterHealthError::Completion(error))
                | Err(CompletionBeforeWriterHealthError::Health(error)) => Err(error),
            },
            |state, event| {
                state.trace.push(event);
                Ok::<(), SwitcherError>(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(
            state.trace,
            vec![
                "completion",
                "health",
                "event-1",
                "completion",
                "health",
                "event-2"
            ]
        );
    }

    #[test]
    fn writer_health_failure_between_events_stops_before_second_event() {
        let mut handled = Vec::new();
        let mut health_checks = 0usize;
        let result = process_writer_healthy_batch(
            &mut handled,
            [1, 2, 3],
            |_handled| {
                health_checks += 1;
                if health_checks == 2 {
                    Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 302 })
                } else {
                    Ok(())
                }
            },
            |handled, event| {
                handled.push(event);
                Ok::<(), SwitcherError>(())
            },
        );

        assert_eq!(handled, vec![1]);
        assert!(matches!(
            result,
            Err(WriterHealthyBatchError::Health(
                SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 302 }
            ))
        ));
    }

    // Pending status / status publishing helpers

    #[test]
    fn pending_status_change_requests_publish_from_service() {
        assert!(should_publish_pending_status_change(true));
        assert!(!should_publish_pending_status_change(false));
    }

    #[test]
    fn dbus_signal_drop_counters_accumulate_and_reset() {
        let mut counters = DbusSignalDropCounters::default();

        counters.record(DbusSignalEventKind::StatusChanged);
        counters.record(DbusSignalEventKind::StatusChanged);
        counters.record(DbusSignalEventKind::CaptureStateChanged);

        assert_eq!(
            counters.take_counts(),
            DbusSignalDropCounts {
                status_changed: 2,
                capture_state_changed: 1,
            }
        );
        assert_eq!(counters.take_counts(), DbusSignalDropCounts::default());
    }

    #[test]
    fn empty_dbus_signal_drop_counts_have_no_heartbeat_fields() {
        assert_eq!(DbusSignalDropCounts::default().heartbeat_fields(), None);
    }

    #[test]
    fn dbus_signal_drop_counts_format_heartbeat_fields() {
        let counts = DbusSignalDropCounts {
            status_changed: 3,
            capture_state_changed: 4,
        };

        assert_eq!(
            counts.heartbeat_fields(),
            Some("dbus_signal_drops_status=3 dbus_signal_drops_capture=4".to_string())
        );
    }

    // Selected-text hotkey matching

    #[test]
    fn selected_text_hotkey_matches_shift_pause_press_only_with_exact_modifiers() {
        assert!(selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::ShiftPause),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_PAUSE,
            1,
        ));
        assert!(!selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::ShiftPause),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_PAUSE,
            0,
        ));
        assert!(!selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::ShiftPause),
            ModifierState::default(),
            Key::KEY_PAUSE,
            1,
        ));
        assert!(!selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::ShiftPause),
            modifiers_with(&[Key::KEY_LEFTCTRL]),
            Key::KEY_PAUSE,
            1,
        ));
        assert!(!selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::ShiftPause),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
        ));
    }

    #[test]
    fn selected_text_hotkey_matches_configured_f12_and_scroll_lock_variants() {
        let shift_ctrl_alt_f12 = HotkeySpec::new(
            HotkeyModifiers::shift_ctrl_alt(),
            crate::model::HotkeyTrigger::F12,
        );

        assert!(selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::ShiftF12),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
        ));
        assert!(selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::CtrlF12),
            modifiers_with(&[Key::KEY_LEFTCTRL]),
            Key::KEY_F12,
            1,
        ));
        assert!(selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::AltScrollLock),
            modifiers_with(&[Key::KEY_LEFTALT]),
            Key::KEY_SCROLLLOCK,
            1,
        ));
        assert!(!selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::CtrlF12),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
        ));
        assert!(!selected_text_hotkey_matches(
            HotkeySpec::from(SelectedTextHotkey::AltScrollLock),
            modifiers_with(&[Key::KEY_LEFTALT]),
            Key::KEY_SCROLLLOCK,
            0,
        ));
        assert!(selected_text_hotkey_matches(
            shift_ctrl_alt_f12,
            modifiers_with(&[Key::KEY_LEFTSHIFT, Key::KEY_LEFTCTRL, Key::KEY_LEFTALT]),
            Key::KEY_F12,
            1,
        ));
        assert!(!selected_text_hotkey_matches(
            shift_ctrl_alt_f12,
            modifiers_with(&[Key::KEY_LEFTSHIFT, Key::KEY_LEFTCTRL]),
            Key::KEY_F12,
            1,
        ));
    }

    #[test]
    fn selected_text_hotkey_does_not_match_when_settings_capture_is_inhibited() {
        assert!(selected_text_hotkey_matches_when_allowed(
            HotkeySpec::from(SelectedTextHotkey::ShiftF12),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
            false,
        ));
        assert!(!selected_text_hotkey_matches_when_allowed(
            HotkeySpec::from(SelectedTextHotkey::ShiftF12),
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
            true,
        ));
    }

    #[test]
    fn manual_correction_hotkey_matches_exact_modifiers() {
        let f12 = HotkeySpec::from(UndoKey::F12);
        let shift_f12 = HotkeySpec::from(SelectedTextHotkey::ShiftF12);
        let ctrl_alt_f12 = HotkeySpec::new(
            HotkeyModifiers::ctrl_alt(),
            crate::model::HotkeyTrigger::F12,
        );
        let shift_ctrl_alt_insert = HotkeySpec::new(
            HotkeyModifiers::shift_ctrl_alt(),
            crate::model::HotkeyTrigger::Insert,
        );

        assert!(manual_correction_hotkey_matches(
            f12,
            ModifierState::default(),
            Key::KEY_F12,
            1,
        ));
        assert!(!manual_correction_hotkey_matches(
            f12,
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
        ));
        assert!(manual_correction_hotkey_matches(
            shift_f12,
            modifiers_with(&[Key::KEY_LEFTSHIFT]),
            Key::KEY_F12,
            1,
        ));
        assert!(manual_correction_hotkey_matches(
            ctrl_alt_f12,
            modifiers_with(&[Key::KEY_LEFTCTRL, Key::KEY_LEFTALT]),
            Key::KEY_F12,
            1,
        ));
        assert!(!manual_correction_hotkey_matches(
            f12,
            modifiers_with(&[Key::KEY_LEFTCTRL]),
            Key::KEY_F12,
            1,
        ));
        assert!(!manual_correction_hotkey_matches(
            f12,
            modifiers_with(&[Key::KEY_LEFTALT]),
            Key::KEY_F12,
            1,
        ));
        assert!(!manual_correction_hotkey_matches(
            f12,
            ModifierState::default(),
            Key::KEY_F12,
            0,
        ));
        assert!(!manual_correction_hotkey_matches(
            f12,
            ModifierState::default(),
            Key::KEY_F11,
            1,
        ));
        assert!(manual_correction_hotkey_matches(
            shift_ctrl_alt_insert,
            modifiers_with(&[Key::KEY_LEFTSHIFT, Key::KEY_LEFTCTRL, Key::KEY_LEFTALT]),
            Key::KEY_INSERT,
            1,
        ));
        assert!(!manual_correction_hotkey_matches(
            shift_ctrl_alt_insert,
            modifiers_with(&[Key::KEY_LEFTSHIFT, Key::KEY_LEFTALT]),
            Key::KEY_INSERT,
            1,
        ));
    }

    #[test]
    fn manual_correction_hotkey_does_not_match_when_settings_capture_is_inhibited() {
        assert!(manual_correction_hotkey_matches_when_allowed(
            HotkeySpec::from(UndoKey::F12),
            ModifierState::default(),
            Key::KEY_F12,
            1,
            false,
        ));
        assert!(!manual_correction_hotkey_matches_when_allowed(
            HotkeySpec::from(UndoKey::F12),
            ModifierState::default(),
            Key::KEY_F12,
            1,
            true,
        ));
    }

    // Auto-correction scheduling helpers

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

    // Manual correction finalization

    #[test]
    fn manual_previous_word_correction_with_punctuation_tail_replays_separator() {
        let corrected_previous_word = vec![
            stroke(Key::KEY_G),
            stroke(Key::KEY_H),
            stroke(Key::KEY_COMMA),
        ];
        let applied = AppliedManualCorrection {
            corrected_buffer: corrected_previous_word.clone(),
            used_current_buffer: false,
            extra_backspaces: 1,
        };
        let mut buffer = vec![stroke(Key::KEY_A), stroke(Key::KEY_B)];
        let mut word_context = WordContext::default();

        let replay = finalize_manual_correction(&mut buffer, &mut word_context, &applied);

        assert_eq!(replay, Some(Key::KEY_SPACE));
        assert!(buffer.is_empty());
        assert!(word_context.valid);
        assert_eq!(word_context.word_before_cursor, corrected_previous_word);
        assert!(word_context.followed_by_separator);
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
    fn corrected_word_commit_state_for_space_updates_context_and_replays_separator() {
        let corrected_buffer = vec![stroke(Key::KEY_H), stroke(Key::KEY_I)];
        let mut buffer = vec![stroke(Key::KEY_A), stroke(Key::KEY_B)];
        let mut word_context = WordContext {
            valid: true,
            word_before_cursor: vec![stroke(Key::KEY_Q)],
            followed_by_separator: false,
        };
        let mut correction_state = CurrentWordCorrectionState::ManuallyCorrected;
        let latch = Some(ManualHotkeyLatch {
            key: Key::KEY_PAUSE,
            armed_at: SystemTime::UNIX_EPOCH,
        });
        let mut manual_hotkey_latch = latch;

        let replay_key = apply_corrected_word_commit_state(
            &mut buffer,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            Key::KEY_SPACE,
            corrected_buffer.clone(),
        );

        assert_eq!(replay_key, Key::KEY_SPACE);
        assert!(buffer.is_empty());
        assert!(word_context.valid);
        assert_eq!(word_context.word_before_cursor, corrected_buffer);
        assert!(word_context.followed_by_separator);
        assert_eq!(
            correction_state,
            CurrentWordCorrectionState::ManuallyCorrected
        );
        assert_eq!(manual_hotkey_latch, latch);
    }

    #[test]
    fn corrected_word_commit_state_for_enter_invalidates_context_and_replays_separator() {
        let corrected_buffer = vec![stroke(Key::KEY_H), stroke(Key::KEY_I)];
        let mut buffer = vec![stroke(Key::KEY_A), stroke(Key::KEY_B)];
        let mut word_context = WordContext {
            valid: true,
            word_before_cursor: vec![stroke(Key::KEY_Q)],
            followed_by_separator: true,
        };
        let mut correction_state = CurrentWordCorrectionState::ManuallyCorrected;
        let mut manual_hotkey_latch = Some(ManualHotkeyLatch {
            key: Key::KEY_PAUSE,
            armed_at: SystemTime::UNIX_EPOCH,
        });

        let replay_key = apply_corrected_word_commit_state(
            &mut buffer,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            Key::KEY_ENTER,
            corrected_buffer,
        );

        assert_eq!(replay_key, Key::KEY_ENTER);
        assert!(buffer.is_empty());
        assert_eq!(word_context, WordContext::default());
        assert_eq!(correction_state, CurrentWordCorrectionState::Raw);
        assert_eq!(manual_hotkey_latch, None);
    }

    #[test]
    fn corrected_word_commit_state_for_tab_invalidates_context_and_replays_separator() {
        let corrected_buffer = vec![stroke(Key::KEY_H), stroke(Key::KEY_I)];
        let mut buffer = vec![stroke(Key::KEY_A), stroke(Key::KEY_B)];
        let mut word_context = WordContext {
            valid: true,
            word_before_cursor: vec![stroke(Key::KEY_Q)],
            followed_by_separator: true,
        };
        let mut correction_state = CurrentWordCorrectionState::ManuallyCorrected;
        let mut manual_hotkey_latch = Some(ManualHotkeyLatch {
            key: Key::KEY_PAUSE,
            armed_at: SystemTime::UNIX_EPOCH,
        });

        let replay_key = apply_corrected_word_commit_state(
            &mut buffer,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            Key::KEY_TAB,
            corrected_buffer,
        );

        assert_eq!(replay_key, Key::KEY_TAB);
        assert!(buffer.is_empty());
        assert_eq!(word_context, WordContext::default());
        assert_eq!(correction_state, CurrentWordCorrectionState::Raw);
        assert_eq!(manual_hotkey_latch, None);
    }

    #[test]
    fn same_layout_pending_commit_state_uses_corrected_buffer() {
        let corrected_buffer = vec![stroke(Key::KEY_H), stroke(Key::KEY_E), stroke(Key::KEY_Y)];
        let original_buffer = vec![stroke(Key::KEY_A), stroke(Key::KEY_B)];
        let mut buffer = original_buffer.clone();
        let mut word_context = WordContext::default();
        let mut correction_state = CurrentWordCorrectionState::Raw;
        let mut manual_hotkey_latch = None;

        let replay_key = apply_corrected_word_commit_state(
            &mut buffer,
            &mut word_context,
            &mut correction_state,
            &mut manual_hotkey_latch,
            Key::KEY_SPACE,
            corrected_buffer.clone(),
        );

        assert_eq!(replay_key, Key::KEY_SPACE);
        assert_ne!(word_context.word_before_cursor, original_buffer);
        assert_eq!(word_context.word_before_cursor, corrected_buffer);
        assert!(word_context.followed_by_separator);
    }

    // Separator suppression / early finish

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
    fn suppressed_separator_release_state_takes_matching_pending_once() {
        let mut suppressed_separator_key = Some(Key::KEY_SPACE);
        let mut pending_word_commit = Some(test_pending_word_commit(Key::KEY_SPACE));

        let release_state = take_suppressed_separator_release_state(
            suppressed_separator_key,
            &mut pending_word_commit,
            Key::KEY_SPACE,
            0,
        )
        .unwrap();
        suppressed_separator_key = None;
        let repeated_release = take_suppressed_separator_release_state(
            suppressed_separator_key,
            &mut pending_word_commit,
            Key::KEY_SPACE,
            0,
        );

        assert_eq!(
            release_state.pending_to_finish,
            Some(test_pending_word_commit(Key::KEY_SPACE))
        );
        assert_eq!(pending_word_commit, None);
        assert_eq!(repeated_release, None);
    }

    #[test]
    fn suppressed_separator_release_state_ignores_non_matching_key() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);
        let mut pending_word_commit = Some(pending.clone());

        let release_state = take_suppressed_separator_release_state(
            Some(Key::KEY_SPACE),
            &mut pending_word_commit,
            Key::KEY_ENTER,
            0,
        );

        assert_eq!(release_state, None);
        assert_eq!(pending_word_commit, Some(pending));
    }

    #[test]
    fn suppressed_separator_release_state_ignores_key_press() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);
        let mut pending_word_commit = Some(pending.clone());

        let release_state = take_suppressed_separator_release_state(
            Some(Key::KEY_SPACE),
            &mut pending_word_commit,
            Key::KEY_SPACE,
            1,
        );

        assert_eq!(release_state, None);
        assert_eq!(pending_word_commit, Some(pending));
    }

    #[test]
    fn suppressed_separator_release_state_preserves_mismatched_pending() {
        let pending = test_pending_word_commit(Key::KEY_ENTER);
        let mut pending_word_commit = Some(pending.clone());

        let release_state = take_suppressed_separator_release_state(
            Some(Key::KEY_SPACE),
            &mut pending_word_commit,
            Key::KEY_SPACE,
            0,
        )
        .unwrap();

        assert_eq!(release_state.pending_to_finish, None);
        assert_eq!(pending_word_commit, Some(pending));
    }

    #[test]
    fn early_finish_does_not_preserve_separator_when_next_event_is_release_or_modifier() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);

        assert_eq!(
            preserved_separator_after_early_finish(Some(&pending), Key::KEY_A, 1),
            Some(Key::KEY_SPACE)
        );
        assert_eq!(
            preserved_separator_after_early_finish(Some(&pending), Key::KEY_A, 0),
            None
        );
        assert_eq!(
            preserved_separator_after_early_finish(Some(&pending), Key::KEY_LEFTSHIFT, 1),
            None
        );
    }

    #[test]
    fn early_finish_preserves_one_swallowed_release_for_original_separator() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);

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
    fn early_finish_state_takes_pending_on_non_modifier_press() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);
        let mut pending_word_commit = Some(pending.clone());

        let early_finish =
            take_pending_word_commit_for_early_finish(&mut pending_word_commit, Key::KEY_A, 1)
                .unwrap();
        let repeated =
            take_pending_word_commit_for_early_finish(&mut pending_word_commit, Key::KEY_B, 1);

        assert_eq!(
            early_finish,
            PendingWordCommitEarlyFinishState {
                pending,
                preserved_separator_key: Some(Key::KEY_SPACE),
            }
        );
        assert_eq!(pending_word_commit, None);
        assert_eq!(repeated, None);
    }

    #[test]
    fn early_finish_state_ignores_key_release_and_preserves_pending() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);
        let mut pending_word_commit = Some(pending.clone());

        let early_finish =
            take_pending_word_commit_for_early_finish(&mut pending_word_commit, Key::KEY_A, 0);

        assert_eq!(early_finish, None);
        assert_eq!(pending_word_commit, Some(pending));
    }

    #[test]
    fn early_finish_state_ignores_modifier_press_and_preserves_pending() {
        let pending = test_pending_word_commit(Key::KEY_SPACE);
        let mut pending_word_commit = Some(pending.clone());

        let early_finish = take_pending_word_commit_for_early_finish(
            &mut pending_word_commit,
            Key::KEY_LEFTSHIFT,
            1,
        );

        assert_eq!(early_finish, None);
        assert_eq!(pending_word_commit, Some(pending));
    }

    #[test]
    fn early_finish_state_ignores_missing_pending_commit() {
        let mut pending_word_commit = None;

        let early_finish =
            take_pending_word_commit_for_early_finish(&mut pending_word_commit, Key::KEY_A, 1);

        assert_eq!(early_finish, None);
        assert_eq!(pending_word_commit, None);
    }

    // Manual current-word commit state

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

    // Manual hotkey latch / undo suppression

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
    fn stale_suppressed_pause_press_is_cleared_for_keyboards_without_pause_release() {
        assert!(should_clear_stale_suppressed_undo_on_press(
            Some(Key::KEY_PAUSE),
            Key::KEY_PAUSE,
            1,
        ));
        assert!(!should_clear_stale_suppressed_undo_on_press(
            Some(Key::KEY_PAUSE),
            Key::KEY_PAUSE,
            2,
        ));
        assert!(!should_clear_stale_suppressed_undo_on_press(
            Some(Key::KEY_PAUSE),
            Key::KEY_A,
            1,
        ));
    }

    #[test]
    fn selected_text_pause_hotkey_runs_without_waiting_for_release() {
        assert!(selected_text_hotkey_runs_on_press(Key::KEY_PAUSE));
        assert!(!selected_text_hotkey_runs_on_press(Key::KEY_F12));
        assert!(!selected_text_hotkey_runs_on_press(Key::KEY_SCROLLLOCK));
    }

    #[test]
    fn stale_suppressed_selected_hotkey_press_is_cleared_without_swallowing_current_press() {
        assert!(should_clear_stale_suppressed_selected_hotkey_on_press(
            Some(Key::KEY_PAUSE),
            Key::KEY_PAUSE,
            1,
        ));
        assert!(!should_clear_stale_suppressed_selected_hotkey_on_press(
            Some(Key::KEY_PAUSE),
            Key::KEY_PAUSE,
            0,
        ));
        assert!(!should_clear_stale_suppressed_selected_hotkey_on_press(
            Some(Key::KEY_PAUSE),
            Key::KEY_F12,
            1,
        ));
    }

    // Deferred manual current-word flow

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

    // Reset / invalidation helpers

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

    #[test]
    fn wayland_focus_switch_policy_enables_shortcut_only_on_wayland() {
        let modifiers = modifiers_with(&[Key::KEY_LEFTALT]);

        assert!(should_invalidate_for_wayland_focus_switch_shortcut(
            SessionType::Wayland,
            modifiers,
            Key::KEY_TAB,
            1,
        ));
        assert!(!should_invalidate_for_wayland_focus_switch_shortcut(
            SessionType::X11,
            modifiers,
            Key::KEY_TAB,
            1,
        ));
        assert!(!should_invalidate_for_wayland_focus_switch_shortcut(
            SessionType::Unknown,
            modifiers,
            Key::KEY_TAB,
            1,
        ));
    }

    #[test]
    fn wayland_focus_switch_policy_rejects_ctrl_modified_tab() {
        assert!(!should_invalidate_for_wayland_focus_switch_shortcut(
            SessionType::Wayland,
            modifiers_with(&[Key::KEY_LEFTCTRL, Key::KEY_LEFTALT]),
            Key::KEY_TAB,
            1,
        ));
        assert!(!should_invalidate_for_wayland_focus_switch_shortcut(
            SessionType::Wayland,
            modifiers_with(&[Key::KEY_LEFTCTRL, Key::KEY_LEFTMETA]),
            Key::KEY_TAB,
            1,
        ));
    }
}

#[cfg(test)]
fn fresh_service_snapshot(
    layout_kind: AppLayoutKind,
    confirmed_at: Instant,
) -> InputRuntimeSnapshot {
    use crate::config::AppConfig;
    use crate::daemon::runtime::RuntimeConfigSnapshot;
    use crate::layout_backend::{FeatureAvailability, LayoutCode, SystemLayout};

    let normalized_code = match layout_kind {
        AppLayoutKind::English => LayoutCode::Us,
        AppLayoutKind::Russian => LayoutCode::Ru,
        AppLayoutKind::Other | AppLayoutKind::Unknown => LayoutCode::Unknown,
    };
    InputRuntimeSnapshot {
        config: RuntimeConfigSnapshot::from(&AppConfig::default()),
        enabled: true,
        features: FeatureAvailability {
            auto_switch: true,
            manual_word_fix: true,
            selected_text_switch: true,
            reason: None,
        },
        session_type: SessionType::X11,
        layout_state: CurrentLayoutState::Known {
            layout: SystemLayout {
                backend_key: "test".to_string(),
                normalized_code,
                display_name: "Test".to_string(),
                kind: layout_kind,
                index: Some(0),
            },
            trustworthy: true,
        },
        config_generation: 1,
        layout_generation: 1,
        confirmed_layout_epoch: 1,
        confirmed_at: Some(confirmed_at),
    }
}

#[cfg(test)]
mod service_snapshot_fresh_tests {
    use super::*;

    #[test]
    fn fresh_snapshot_keeps_layout_auto_correction_enabled() {
        let now = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);

        assert_eq!(
            layout_correction_decision(&snapshot, now, snapshot.confirmed_layout_epoch),
            LayoutCorrectionAvailability::Available(AppLayoutKind::English)
        );
    }

    #[test]
    fn fresh_snapshot_keeps_caps_and_two_capitals_fixes_enabled() {
        let now = Instant::now();
        let mut snapshot = fresh_service_snapshot(AppLayoutKind::Russian, now);
        snapshot.config.fix_two_capitals = true;
        snapshot.config.fix_accidental_caps_lock = true;

        assert!(same_layout_fixes_allowed(
            &snapshot,
            now,
            snapshot.confirmed_layout_epoch
        ));
    }

    #[test]
    fn fresh_snapshot_keeps_manual_correction_enabled() {
        let now = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);

        assert!(manual_correction_allowed(
            &snapshot,
            now,
            snapshot.confirmed_layout_epoch
        ));
    }

    #[test]
    fn daemon_service_has_no_synchronous_runtime_refresh_calls() {
        let source = include_str!("service.rs");
        for forbidden in [
            ["sync_", "with_backend"].concat(),
            ["periodic_", "sync_tick"].concat(),
            ["refresh_current_layout_", "observation"].concat(),
            ["optimistic_gnome_wayland_", "uinput_layout_switch"].concat(),
            ["config_", "snapshot()?"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "forbidden input-path call: {forbidden}"
            );
        }
    }
}

#[cfg(test)]
mod service_snapshot_stale_tests {
    use super::*;
    use crate::daemon::input_snapshot::INPUT_LAYOUT_FRESHNESS;

    #[test]
    fn stale_layout_disables_all_layout_dependent_corrections() {
        let confirmed_at = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, confirmed_at);
        let stale_at = confirmed_at + INPUT_LAYOUT_FRESHNESS + Duration::from_millis(1);

        assert_eq!(
            layout_correction_decision(&snapshot, stale_at, snapshot.confirmed_layout_epoch),
            LayoutCorrectionAvailability::Unavailable(InputLayoutStatus::Stale)
        );
        assert!(!same_layout_fixes_allowed(
            &snapshot,
            stale_at,
            snapshot.confirmed_layout_epoch
        ));
        assert!(!manual_correction_allowed(
            &snapshot,
            stale_at,
            snapshot.confirmed_layout_epoch
        ));
    }

    #[test]
    fn stale_separator_path_forwards_instead_of_suppressing() {
        let confirmed_at = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, confirmed_at);
        let stale_at = confirmed_at + INPUT_LAYOUT_FRESHNESS;
        let outcome = word_boundary_action(
            &snapshot,
            stale_at,
            snapshot.confirmed_layout_epoch,
            evdev::Key::KEY_SPACE,
        );

        assert_eq!(
            outcome,
            WordBoundaryAction::ForwardUncorrected(evdev::Key::KEY_SPACE)
        );
    }
}

#[cfg(test)]
mod pending_snapshot_authorization_tests {
    use super::*;

    #[test]
    fn pending_commit_carries_snapshot_authorization() {
        let now = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);
        let authorization = snapshot.authorization_at(now, 1).unwrap();
        let pending = PendingWordCommit {
            separator_key: evdev::Key::KEY_SPACE,
            action: PendingWordCommitAction::LayoutCorrection,
            authorization,
        };

        assert_eq!(pending.authorization, authorization);
    }

    #[test]
    fn pending_commit_is_cancelled_after_layout_generation_change() {
        let now = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);
        let authorization = snapshot.authorization_at(now, 1).unwrap();
        let changed = InputRuntimeSnapshot {
            layout_generation: snapshot.layout_generation + 1,
            ..snapshot
        };

        assert!(!changed.authorizes_at(authorization, now, 1));
    }

    #[test]
    fn pending_commit_requires_successful_snapshot_adoption() {
        let now = Instant::now();
        let snapshot = fresh_service_snapshot(AppLayoutKind::English, now);
        let authorization = snapshot.authorization_at(now, 1).unwrap();

        assert!(!pending_commit_authorized_after_adoption(
            false,
            &snapshot,
            authorization,
            now,
            1,
        ));
        assert!(pending_commit_authorized_after_adoption(
            true,
            &snapshot,
            authorization,
            now,
            1,
        ));
    }

    #[test]
    fn cancelled_pending_commit_replays_separator_once() {
        let mut ledger = Vec::new();
        cancel_pending_commit_with(evdev::Key::KEY_SPACE, |key| {
            ledger.push(key);
            Ok::<(), ()>(())
        })
        .unwrap();

        assert_eq!(ledger, vec![evdev::Key::KEY_SPACE]);
    }
}

#[cfg(test)]
mod status_snapshot_tests {
    use super::*;

    #[test]
    fn provisional_layout_does_not_publish_status() {
        assert!(!status_snapshot_is_publishable(
            InputLayoutStatus::AwaitingConfirmation
        ));
    }

    #[test]
    fn stale_or_unknown_layout_does_not_publish_status() {
        assert!(!status_snapshot_is_publishable(InputLayoutStatus::Stale));
        assert!(!status_snapshot_is_publishable(InputLayoutStatus::Unknown));
    }

    #[test]
    fn confirmed_fresh_layout_publishes_status() {
        assert!(status_snapshot_is_publishable(InputLayoutStatus::Fresh));
    }
}
