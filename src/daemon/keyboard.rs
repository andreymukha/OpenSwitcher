use crate::daemon::debug_log::{format_input, try_debug_line, DebugLogKind};
use crate::daemon::runtime::RuntimeConfigSnapshot;
use crate::daemon::switch_logic::CorrectionPlan;
use crate::daemon::x11_wait::{wait_for_x11_or_stop, X11WaitOutcome};
use crate::error::SwitcherError;
use crate::model::{
    DesktopEnvironment, DistroKind, HotkeyTrigger, LayoutSwitchCombo, SessionType, SystemContext,
    UndoKey,
};
use crate::system::SystemContextDetector;
use evdev::{enumerate, Device, InputEvent, Key, LedType};
use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{net::Shutdown, os::unix::net::UnixStream};

const INPUT_EVENT_KEYBOARD: i32 = 0x01;
const MODIFIER_SYNC_DELAY_MS: u64 = 20;
const LAYOUT_SWITCH_DELAY_MS: u64 = 20;
const KEYBOARD_PATH_ENV: &str = "OPEN_SWITCHER_KEYBOARD_PATH";
const KEYBOARD_SYMLINK_SUFFIX: &str = "-event-kbd";
const KEYBOARD_SYMLINK_DIRS: [&str; 2] = ["/dev/input/by-path", "/dev/input/by-id"];
const UINPUT_PATHS: [&str; 2] = ["/dev/uinput", "/dev/input/uinput"];
pub const INPUT_EVENT_WAIT_TIMEOUT: Duration = Duration::from_millis(100);
const POINTER_POLL_INTERVAL: Duration = Duration::from_millis(20);
// Fast-path writer queue is bounded to avoid unbounded memory growth under load.
// Transactional commands use the same total-order queue, but are represented as
// single indivisible commands and get a larger bounded enqueue window because
// correctness matters more than shaving a few microseconds there.
const WRITER_QUEUE_CAPACITY: usize = 1024;
const FAST_PATH_SATURATION_RETRY_WINDOW: Duration = Duration::from_millis(2);
const TRANSACTION_SEND_RETRY_WINDOW: Duration = Duration::from_millis(50);
const TRANSACTION_BACKEND_GRACE: Duration = Duration::from_secs(1);
const MAX_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(5);
const SHORTCUT_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(1);
const TRANSACTION_SLEEP_QUANTUM: Duration = Duration::from_millis(5);
const WRITER_STARTUP_READY_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_WORKER_STARTUP_READY_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_SEND_RETRY_WINDOW: Duration = Duration::from_millis(50);
const WRITER_SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(1);
const X11_EVDEV_KEYCODE_OFFSET: u16 = 8;
const CINNAMON_XKB_SWITCH_TIMEOUT: Duration = Duration::from_millis(350);
const CINNAMON_XKB_SWITCH_POLL_INTERVAL: Duration = Duration::from_millis(5);
static NEXT_WRITER_TRANSACTION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_writer_transaction_request_id() -> u64 {
    loop {
        let request_id = NEXT_WRITER_TRANSACTION_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
        if request_id != 0 {
            return request_id;
        }
    }
}

fn take_next_nonzero_request_id(next_request_id: &mut u64) -> u64 {
    loop {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        if request_id != 0 {
            return request_id;
        }
    }
}

// Backend readiness state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputBackendReadiness {
    pub keyboard_open: bool,
    pub writer_ready: bool,
    pub watchers_ready: bool,
    pub event_processing_ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterShutdownOutcome {
    Stopped,
    Unresponsive { timeout_ms: u64 },
}

impl InputBackendReadiness {
    pub fn is_ready(self) -> bool {
        self.keyboard_open
            && self.writer_ready
            && self.watchers_ready
            && self.event_processing_ready
    }
}

// Keyboard controller

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyboardShutdownPhase {
    RequestWriterStop,
    ReleaseGrab,
    FinishWriterStop,
    StopAndJoinWatchers,
    DetachWatchers,
}

fn run_keyboard_shutdown_sequence(
    mut run_phase: impl FnMut(KeyboardShutdownPhase) -> Option<WriterShutdownOutcome>,
) -> WriterShutdownOutcome {
    let _ = run_phase(KeyboardShutdownPhase::RequestWriterStop);
    let _ = run_phase(KeyboardShutdownPhase::ReleaseGrab);
    let outcome = run_phase(KeyboardShutdownPhase::FinishWriterStop)
        .expect("writer shutdown phase must return an outcome");
    let watcher_phase = match outcome {
        WriterShutdownOutcome::Stopped => KeyboardShutdownPhase::StopAndJoinWatchers,
        WriterShutdownOutcome::Unresponsive { .. } => KeyboardShutdownPhase::DetachWatchers,
    };
    let _ = run_phase(watcher_phase);
    outcome
}

pub(crate) fn resolve_error_after_writer_shutdown(
    trigger: SwitcherError,
    phase: &'static str,
    outcome: WriterShutdownOutcome,
) -> SwitcherError {
    match outcome {
        WriterShutdownOutcome::Stopped => trigger,
        WriterShutdownOutcome::Unresponsive { timeout_ms } => {
            SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms,
                phase,
                trigger: trigger.to_string(),
            }
        }
    }
}

pub struct KeyboardController {
    real_device: GrabbedKeyboardDevice,
    pointer_watcher: PointerWatcher,
    input_target_watcher: InputTargetWatcher,
    virtual_device: VirtualKeyboardWriter,
}

pub struct PreparedKeyboardController {
    controller: KeyboardController,
}

// Selection keyboard transport

pub struct SelectionKeyboardTransport {
    virtual_device: VirtualKeyboardHandle,
    modifiers: SharedModifierState,
}

// Virtual keyboard writer

#[derive(Clone)]
struct VirtualKeyboardHandle {
    command_tx: mpsc::SyncSender<WriterCommand>,
    alive: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    transaction_failure_request_id: Arc<AtomicU64>,
    transaction_terminal_gate: Arc<Mutex<()>>,
}

struct VirtualKeyboardWriter {
    handle: VirtualKeyboardHandle,
    join_handle: Option<JoinHandle<()>>,
    exit_rx: mpsc::Receiver<()>,
    shutdown_started_at: Option<Instant>,
    shutdown_outcome: Option<WriterShutdownOutcome>,
    completion_rx: mpsc::Receiver<ManualCurrentWordCompletion>,
    next_request_id: u64,
    pending_manual_current_word: Option<PendingManualCurrentWordTransaction>,
}

struct WriterExitNotifier {
    exit_tx: mpsc::SyncSender<()>,
}

impl Drop for WriterExitNotifier {
    fn drop(&mut self) {
        let _ = self.exit_tx.try_send(());
    }
}

fn run_writer_thread_with_exit_notification<T, R>(
    owned_device: T,
    exit_tx: mpsc::SyncSender<()>,
    run: impl FnOnce(T) -> R,
) -> R {
    let _exit_notifier = WriterExitNotifier { exit_tx };
    run(owned_device)
}

struct PendingManualCurrentWordTransaction {
    control: WriterTransactionControl,
    completion: Option<ManualCurrentWordCompletion>,
}

#[cfg(test)]
thread_local! {
    static DEFERRED_POLL_BEFORE_PENDING_ACTIVE_CHECK_HOOK:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static DEFERRED_HEALTH_BEFORE_HANDLE_CHECK_HOOK:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
fn install_deferred_poll_before_pending_active_check_hook(hook: impl FnOnce() + 'static) {
    DEFERRED_POLL_BEFORE_PENDING_ACTIVE_CHECK_HOOK.with(|slot| {
        assert!(slot.replace(Some(Box::new(hook))).is_none());
    });
}

#[cfg(test)]
fn run_deferred_poll_before_pending_active_check_hook() {
    let hook = DEFERRED_POLL_BEFORE_PENDING_ACTIVE_CHECK_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
fn install_deferred_health_before_handle_check_hook(hook: impl FnOnce() + 'static) {
    DEFERRED_HEALTH_BEFORE_HANDLE_CHECK_HOOK.with(|slot| {
        assert!(slot.replace(Some(Box::new(hook))).is_none());
    });
}

#[cfg(test)]
fn run_deferred_health_before_handle_check_hook() {
    let hook = DEFERRED_HEALTH_BEFORE_HANDLE_CHECK_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

enum WriterCommand {
    Shutdown,
    Fast(WriterFastCommand),
    Transaction(WriterTransaction),
    DeferredManualCurrentWordCorrection {
        control: WriterTransactionControl,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    },
}

#[derive(Clone)]
enum WriterFastCommand {
    ForwardEvent { key: Key, value: i32 },
    TypeSeparator { key: Key },
}

enum WriterTransactionKind {
    ApplyCorrection {
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    },
    ApplySameLayoutCorrection {
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    },
    CopyShortcut {
        modifiers: ModifierState,
    },
    PasteShortcut {
        modifiers: ModifierState,
    },
}

enum WriterTransaction {
    Execute {
        control: WriterTransactionControl,
        kind: WriterTransactionKind,
        reply: mpsc::Sender<Result<CorrectionExecutionOutcome, SwitcherError>>,
    },
}

#[derive(Clone)]
struct WriterTransactionControl {
    request_id: u64,
    deadline: Instant,
    state: Arc<AtomicU8>,
    failure_request_id: Arc<AtomicU64>,
    stop_requested: Arc<AtomicBool>,
    terminal_gate: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum WriterTransactionState {
    Pending = 0,
    Completed = 1,
    TimedOut = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterCompletionPublication {
    Completed,
    Cancelled,
    ReceiverDisconnected,
}

impl WriterTransactionState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Completed as u8 => Self::Completed,
            value if value == Self::TimedOut as u8 => Self::TimedOut,
            _ => Self::Pending,
        }
    }
}

impl WriterTransactionControl {
    #[cfg(test)]
    fn new(request_id: u64, timeout: Duration, failure_request_id: Arc<AtomicU64>) -> Self {
        Self::new_with_terminal_gate(
            request_id,
            timeout,
            failure_request_id,
            Arc::new(Mutex::new(())),
        )
    }

    #[cfg(test)]
    fn new_with_terminal_gate(
        request_id: u64,
        timeout: Duration,
        failure_request_id: Arc<AtomicU64>,
        terminal_gate: Arc<Mutex<()>>,
    ) -> Self {
        Self::new_with_writer_state(
            request_id,
            timeout,
            failure_request_id,
            Arc::new(AtomicBool::new(false)),
            terminal_gate,
        )
    }

    fn new_with_writer_state(
        request_id: u64,
        timeout: Duration,
        failure_request_id: Arc<AtomicU64>,
        stop_requested: Arc<AtomicBool>,
        terminal_gate: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            request_id,
            deadline: Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
            state: Arc::new(AtomicU8::new(WriterTransactionState::Pending as u8)),
            failure_request_id,
            stop_requested,
            terminal_gate,
        }
    }

    #[cfg(test)]
    fn with_timeout_for_test(request_id: u64, timeout: Duration) -> Self {
        Self::new(request_id, timeout, Arc::new(AtomicU64::new(0)))
    }

    fn request_id(&self) -> u64 {
        self.request_id
    }

    fn state(&self) -> WriterTransactionState {
        WriterTransactionState::from_raw(self.state.load(Ordering::SeqCst))
    }

    fn is_cancelled(&self) -> bool {
        self.state() == WriterTransactionState::TimedOut
            || self.stop_requested.load(Ordering::SeqCst)
            || self.failure_request_id.load(Ordering::SeqCst) != 0
    }

    fn timed_out_error(&self) -> SwitcherError {
        let failed_request_id = match self.failure_request_id.load(Ordering::SeqCst) {
            0 => self.request_id(),
            request_id => request_id,
        };
        SwitcherError::VirtualKeyboardWriterTransactionTimedOut {
            request_id: failed_request_id,
        }
    }

    fn cancellation_error(&self) -> SwitcherError {
        let _terminal_guard = self
            .terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.state() == WriterTransactionState::TimedOut
            || self.failure_request_id.load(Ordering::SeqCst) != 0
        {
            self.timed_out_error()
        } else if self.stop_requested.load(Ordering::SeqCst) {
            SwitcherError::VirtualKeyboardWriterDisconnected
        } else {
            self.timed_out_error()
        }
    }

    fn try_mark_timed_out(&self) -> bool {
        let _terminal_guard = self
            .terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.try_mark_timed_out_while_terminal_gate_is_held()
    }

    fn try_mark_timed_out_while_terminal_gate_is_held(&self) -> bool {
        if self.stop_requested.load(Ordering::SeqCst) {
            return false;
        }
        if self
            .state
            .compare_exchange(
                WriterTransactionState::Pending as u8,
                WriterTransactionState::TimedOut as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }

        let _ = self.failure_request_id.compare_exchange(
            0,
            self.request_id,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        true
    }

    fn mark_timed_out(&self) -> SwitcherError {
        let _ = self.try_mark_timed_out();
        self.cancellation_error()
    }

    fn publish_completed_with(
        &self,
        publish_reply: impl FnOnce() -> bool,
    ) -> WriterCompletionPublication {
        let _terminal_guard = self
            .terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop_requested.load(Ordering::SeqCst)
            || self.failure_request_id.load(Ordering::SeqCst) != 0
            || self.state() != WriterTransactionState::Pending
        {
            return WriterCompletionPublication::Cancelled;
        }
        if Instant::now() >= self.deadline {
            let _ = self.try_mark_timed_out_while_terminal_gate_is_held();
            return WriterCompletionPublication::Cancelled;
        }
        if !publish_reply() {
            return WriterCompletionPublication::ReceiverDisconnected;
        }

        let completed = self
            .state
            .compare_exchange(
                WriterTransactionState::Pending as u8,
                WriterTransactionState::Completed as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok();
        debug_assert!(completed, "terminal gate must serialize completion");
        if completed {
            WriterCompletionPublication::Completed
        } else {
            WriterCompletionPublication::Cancelled
        }
    }

    fn publish_completed(&self) -> bool {
        matches!(
            self.publish_completed_with(|| true),
            WriterCompletionPublication::Completed
        )
    }

    fn ensure_active_while_terminal_gate_is_held(&self) -> Result<(), SwitcherError> {
        if self.stop_requested.load(Ordering::SeqCst) {
            return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
        }
        if self.failure_request_id.load(Ordering::SeqCst) != 0 {
            let _ = self.try_mark_timed_out_while_terminal_gate_is_held();
            return Err(self.timed_out_error());
        }
        match self.state() {
            WriterTransactionState::Pending => {}
            WriterTransactionState::TimedOut => return Err(self.timed_out_error()),
            WriterTransactionState::Completed => {
                return Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                    request_id: self.request_id(),
                    reason: "transaction is already completed".to_string(),
                });
            }
        }
        if Instant::now() >= self.deadline {
            let _ = self.try_mark_timed_out_while_terminal_gate_is_held();
            return Err(self.timed_out_error());
        }
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), SwitcherError> {
        let _terminal_guard = self
            .terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_active_while_terminal_gate_is_held()
    }

    fn authorize_mutation_start(&self) -> Result<(), SwitcherError> {
        let _terminal_guard = self
            .terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_active_while_terminal_gate_is_held()
    }

    fn try_request_stop_for_input_worker_loss(&self) -> bool {
        let _terminal_guard = self
            .terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop_requested.load(Ordering::SeqCst)
            || self.failure_request_id.load(Ordering::SeqCst) != 0
            || self.state() != WriterTransactionState::Pending
        {
            return false;
        }
        if Instant::now() >= self.deadline {
            let _ = self.try_mark_timed_out_while_terminal_gate_is_held();
            return false;
        }

        self.stop_requested.store(true, Ordering::SeqCst);
        true
    }

    #[cfg(test)]
    fn wait_for_reply(
        &self,
        reply_rx: mpsc::Receiver<Result<CorrectionExecutionOutcome, SwitcherError>>,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        self.wait_for_reply_with_input_health(reply_rx, || None)
    }

    fn wait_for_reply_with_input_health(
        &self,
        reply_rx: mpsc::Receiver<Result<CorrectionExecutionOutcome, SwitcherError>>,
        mut input_worker_health_error: impl FnMut() -> Option<SwitcherError>,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        let mut received_reply = None;

        loop {
            match self.state() {
                WriterTransactionState::Completed => {
                    let reply = match received_reply {
                        Some(reply) => reply,
                        None => reply_rx
                            .try_recv()
                            .map_err(|_| SwitcherError::VirtualKeyboardWriterDisconnected)?,
                    };
                    return reply;
                }
                WriterTransactionState::TimedOut => return Err(self.timed_out_error()),
                WriterTransactionState::Pending => {}
            }

            if self.stop_requested.load(Ordering::SeqCst) {
                return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
            }

            if self.failure_request_id.load(Ordering::SeqCst) != 0
                || Instant::now() >= self.deadline
            {
                if self.try_mark_timed_out() {
                    return Err(self.timed_out_error());
                }
                continue;
            }

            if received_reply.is_none() {
                match reply_rx.try_recv() {
                    Ok(reply) => received_reply = Some(reply),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if self.state() == WriterTransactionState::Pending
                            && self.failure_request_id.load(Ordering::SeqCst) == 0
                        {
                            return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
                        }
                    }
                }
            }

            if let Some(input_worker_error) = input_worker_health_error() {
                if received_reply.as_ref().is_some_and(Result::is_err) {
                    return received_reply.expect("error reply was checked above");
                }
                if self.try_request_stop_for_input_worker_loss() {
                    return Err(input_worker_error);
                }
                continue;
            }

            let wait = self
                .deadline
                .saturating_duration_since(Instant::now())
                .min(TRANSACTION_SLEEP_QUANTUM);
            match reply_rx.recv_timeout(wait) {
                Ok(reply) => received_reply = Some(reply),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if self.state() == WriterTransactionState::Pending
                        && self.failure_request_id.load(Ordering::SeqCst) == 0
                    {
                        return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
                    }
                }
            }
        }
    }

    fn sleep_interruptibly(&self, duration: Duration) -> Result<(), SwitcherError> {
        let sleep_deadline = Instant::now()
            .checked_add(duration)
            .unwrap_or(self.deadline);
        loop {
            self.ensure_active()?;
            let now = Instant::now();
            if now >= sleep_deadline {
                return Ok(());
            }

            let until_sleep_done = sleep_deadline.saturating_duration_since(now);
            let until_transaction_deadline = self.deadline.saturating_duration_since(now);
            let step = until_sleep_done
                .min(until_transaction_deadline)
                .min(TRANSACTION_SLEEP_QUANTUM);
            if step.is_zero() {
                return Err(self.mark_timed_out());
            }
            thread::sleep(step);
        }
    }
}

impl WriterTransactionKind {
    fn execution_timeout(&self) -> Result<Duration, SwitcherError> {
        match self {
            WriterTransactionKind::ApplyCorrection { plan, config, .. } => {
                correction_transaction_timeout(plan, config, true)
            }
            WriterTransactionKind::ApplySameLayoutCorrection { plan, config, .. } => {
                correction_transaction_timeout(plan, config, false)
            }
            WriterTransactionKind::CopyShortcut { .. }
            | WriterTransactionKind::PasteShortcut { .. } => Ok(SHORTCUT_TRANSACTION_TIMEOUT),
        }
    }
}

impl WriterTransaction {
    fn control(&self) -> &WriterTransactionControl {
        match self {
            Self::Execute { control, .. } => control,
        }
    }
}

fn correction_transaction_timeout(
    plan: &CorrectionPlan,
    config: &RuntimeConfigSnapshot,
    switch_layout: bool,
) -> Result<Duration, SwitcherError> {
    let timeout = config
        .estimated_correction_schedule(plan.buffer.len(), plan.extra_backspaces, switch_layout)?
        .saturating_add(TRANSACTION_BACKEND_GRACE);
    debug_assert!(timeout <= MAX_TRANSACTION_TIMEOUT);
    Ok(timeout)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualCurrentWordCompletion {
    pub request_id: u64,
    pub outcome: ManualCurrentWordOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualCurrentWordOutcome {
    Succeeded(CorrectionPlan),
    FailedAfterMutation(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualCurrentWordStartOutcome {
    Started(u64),
    RejectedBeforeMutation(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CorrectionExecutionOutcome {
    pub layout_switch: CorrectionLayoutSwitchOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CorrectionLayoutSwitchOutcome {
    NotNeeded,
    AppliedUinput,
    AppliedX11,
    AppliedCinnamonXkbXtest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CorrectionReplayStrategy {
    Generic,
    CinnamonXkbXtest,
    CinnamonXkbXtestUnavailable,
}

// Real keyboard device

struct GrabbedKeyboardDevice {
    path: PathBuf,
    device: Device,
    grabbed: bool,
}

// Pointer watcher

struct PointerWatcher {
    click_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    required: bool,
    handle: Option<JoinHandle<()>>,
}

// Input target watcher

struct InputTargetWatcher {
    changed_flag: Arc<AtomicBool>,
    pointer_click_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    required: bool,
    handle: Option<JoinHandle<()>>,
    stop_wakeup: Option<UnixStream>,
}

struct PointerDeviceState {
    device: Device,
    pressed_buttons: HashSet<Key>,
}

// X11 active window monitor

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum X11ContextEvent {
    ActiveWindowChanged {
        previous: Option<u32>,
        current: Option<u32>,
    },
    PointerClick {
        detail: u32,
    },
}

struct ActiveWindowMonitor {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
    active_window_atom: u32,
    current_window: Option<u32>,
}

fn xinput_pointer_click_event_mask() -> x11rb::protocol::xinput::EventMask {
    use x11rb::protocol::xinput::{Device, EventMask, XIEventMask};

    EventMask {
        deviceid: Device::ALL_MASTER.into(),
        mask: vec![XIEventMask::RAW_BUTTON_PRESS],
    }
}

fn x11_pointer_click_event(event: &x11rb::protocol::Event) -> Option<X11ContextEvent> {
    match event {
        x11rb::protocol::Event::XinputRawButtonPress(event)
            if is_x11_pointer_click(event.detail) =>
        {
            Some(X11ContextEvent::PointerClick {
                detail: event.detail,
            })
        }
        _ => None,
    }
}

fn publish_x11_context_event(
    event: X11ContextEvent,
    changed_flag: &AtomicBool,
    pointer_click_flag: &AtomicBool,
) {
    match event {
        X11ContextEvent::ActiveWindowChanged { previous, current } => {
            changed_flag.store(true, Ordering::SeqCst);
            log_input_debug(
                "input-target-changed",
                &format!(
                    "source=_NET_ACTIVE_WINDOW previous={} current={}",
                    format_x11_window(previous),
                    format_x11_window(current)
                ),
            );
        }
        X11ContextEvent::PointerClick { detail } => {
            pointer_click_flag.store(true, Ordering::SeqCst);
            log_input_debug("pointer-click", &format!("source=xinput2 detail={detail}"));
        }
    }
}

fn input_target_stop_wakeup_pair() -> io::Result<(UnixStream, UnixStream)> {
    UnixStream::pair()
}

fn signal_input_target_stop(stop_wakeup: Option<&UnixStream>) {
    if let Some(stop_wakeup) = stop_wakeup {
        let _ = stop_wakeup.shutdown(Shutdown::Write);
    }
}

fn run_x11_event_cycle<Event>(
    stop_requested: &AtomicBool,
    mut next_event: impl FnMut() -> io::Result<Option<Event>>,
    mut handle_event: impl FnMut(Event),
    mut wait: impl FnMut() -> io::Result<X11WaitOutcome>,
) -> io::Result<bool> {
    while let Some(event) = next_event()? {
        handle_event(event);
    }

    if stop_requested.load(Ordering::SeqCst) {
        return Ok(false);
    }

    match wait()? {
        X11WaitOutcome::X11Ready => Ok(true),
        X11WaitOutcome::StopRequested => Ok(false),
    }
}

// Watcher worker lifecycle

struct WorkerAliveGuard {
    alive: Arc<AtomicBool>,
}

impl WorkerAliveGuard {
    fn new(alive: Arc<AtomicBool>) -> Self {
        Self { alive }
    }
}

impl Drop for WorkerAliveGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}

// Modifier state

#[derive(Clone, Default)]
pub struct SharedModifierState {
    bits: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierState {
    left_ctrl: bool,
    right_ctrl: bool,
    left_shift: bool,
    right_shift: bool,
    left_alt: bool,
    right_alt: bool,
    left_meta: bool,
    right_meta: bool,
    caps_lock_active: bool,
}

impl ModifierState {
    pub fn update(&mut self, key: Key, value: i32) {
        let pressed = value == 1 || value == 2;
        match key {
            Key::KEY_LEFTCTRL => self.left_ctrl = pressed,
            Key::KEY_RIGHTCTRL => self.right_ctrl = pressed,
            Key::KEY_LEFTSHIFT => self.left_shift = pressed,
            Key::KEY_RIGHTSHIFT => self.right_shift = pressed,
            Key::KEY_LEFTALT => self.left_alt = pressed,
            Key::KEY_RIGHTALT => self.right_alt = pressed,
            Key::KEY_LEFTMETA => self.left_meta = pressed,
            Key::KEY_RIGHTMETA => self.right_meta = pressed,
            Key::KEY_CAPSLOCK if value == 1 => self.caps_lock_active = !self.caps_lock_active,
            _ => {}
        }
    }

    pub fn set_caps_lock_active(&mut self, active: bool) {
        self.caps_lock_active = active;
    }

    pub fn is_caps_lock_active(&self) -> bool {
        self.caps_lock_active
    }

    pub fn is_shift_pressed(&self) -> bool {
        self.left_shift || self.right_shift
    }

    pub fn is_ctrl_pressed(&self) -> bool {
        self.left_ctrl || self.right_ctrl
    }

    pub fn is_alt_pressed(&self) -> bool {
        self.left_alt || self.right_alt
    }

    pub fn is_meta_pressed(&self) -> bool {
        self.left_meta || self.right_meta
    }

    pub fn matches_layout_switch_combo(
        &self,
        combo: LayoutSwitchCombo,
        key: Key,
        value: i32,
    ) -> bool {
        if value != 1 {
            return false;
        }

        match combo {
            LayoutSwitchCombo::CtrlShift => {
                matches!(
                    key,
                    Key::KEY_LEFTCTRL
                        | Key::KEY_RIGHTCTRL
                        | Key::KEY_LEFTSHIFT
                        | Key::KEY_RIGHTSHIFT
                ) && self.is_ctrl_pressed()
                    && self.is_shift_pressed()
            }
            LayoutSwitchCombo::AltShift => {
                matches!(
                    key,
                    Key::KEY_LEFTALT | Key::KEY_RIGHTALT | Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT
                ) && self.is_alt_pressed()
                    && self.is_shift_pressed()
            }
            LayoutSwitchCombo::CapsLock => key == Key::KEY_CAPSLOCK,
            LayoutSwitchCombo::CtrlSpace => key == Key::KEY_SPACE && self.is_ctrl_pressed(),
            LayoutSwitchCombo::SuperSpace => key == Key::KEY_SPACE && self.is_meta_pressed(),
            LayoutSwitchCombo::LeftCtrlLeftShift => {
                matches!(key, Key::KEY_LEFTCTRL | Key::KEY_LEFTSHIFT)
                    && self.left_ctrl
                    && self.left_shift
            }
            LayoutSwitchCombo::RightCtrlRightShift => {
                matches!(key, Key::KEY_RIGHTCTRL | Key::KEY_RIGHTSHIFT)
                    && self.right_ctrl
                    && self.right_shift
            }
            LayoutSwitchCombo::LeftAltLeftShift => {
                matches!(key, Key::KEY_LEFTALT | Key::KEY_LEFTSHIFT)
                    && self.left_alt
                    && self.left_shift
            }
            LayoutSwitchCombo::RightAltRightShift => {
                matches!(key, Key::KEY_RIGHTALT | Key::KEY_RIGHTSHIFT)
                    && self.right_alt
                    && self.right_shift
            }
        }
    }

    pub fn keeps_layout_switch_combo_active(&self, combo: LayoutSwitchCombo) -> bool {
        match combo {
            LayoutSwitchCombo::CtrlShift => self.is_ctrl_pressed() && self.is_shift_pressed(),
            LayoutSwitchCombo::AltShift => self.is_alt_pressed() && self.is_shift_pressed(),
            LayoutSwitchCombo::CapsLock => false,
            LayoutSwitchCombo::CtrlSpace => self.is_ctrl_pressed(),
            LayoutSwitchCombo::SuperSpace => self.is_meta_pressed(),
            LayoutSwitchCombo::LeftCtrlLeftShift => self.left_ctrl && self.left_shift,
            LayoutSwitchCombo::RightCtrlRightShift => self.right_ctrl && self.right_shift,
            LayoutSwitchCombo::LeftAltLeftShift => self.left_alt && self.left_shift,
            LayoutSwitchCombo::RightAltRightShift => self.right_alt && self.right_shift,
        }
    }

    fn to_bits(self) -> u8 {
        (self.left_ctrl as u8)
            | ((self.right_ctrl as u8) << 1)
            | ((self.left_shift as u8) << 2)
            | ((self.right_shift as u8) << 3)
            | ((self.left_alt as u8) << 4)
            | ((self.right_alt as u8) << 5)
            | ((self.left_meta as u8) << 6)
            | ((self.right_meta as u8) << 7)
    }

    fn from_bits(bits: u8) -> Self {
        Self {
            left_ctrl: bits & 0b000001 != 0,
            right_ctrl: bits & 0b000010 != 0,
            left_shift: bits & 0b000100 != 0,
            right_shift: bits & 0b001000 != 0,
            left_alt: bits & 0b010000 != 0,
            right_alt: bits & 0b100000 != 0,
            left_meta: bits & 0b1000000 != 0,
            right_meta: bits & 0b10000000 != 0,
            caps_lock_active: false,
        }
    }
}

// Keyboard controller

fn publish_writer_ready(
    alive: &AtomicBool,
    ready_tx: mpsc::SyncSender<()>,
) -> Result<(), SwitcherError> {
    alive.store(true, Ordering::SeqCst);
    if ready_tx.send(()).is_err() {
        alive.store(false, Ordering::SeqCst);
        return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
    }
    Ok(())
}

fn wait_for_writer_startup_ready(
    ready_rx: &mpsc::Receiver<()>,
    timeout: Duration,
) -> Result<(), SwitcherError> {
    match ready_rx.recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(SwitcherError::VirtualKeyboardWriterStartupTimedOut {
                timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            })
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        }
    }
}

fn publish_input_worker_ready(
    alive: &AtomicBool,
    ready_tx: mpsc::SyncSender<()>,
    worker: &'static str,
) -> Result<(), SwitcherError> {
    alive.store(true, Ordering::SeqCst);
    if ready_tx.send(()).is_err() {
        alive.store(false, Ordering::SeqCst);
        return Err(SwitcherError::InputWorkerDisconnected { worker });
    }
    Ok(())
}

fn wait_for_input_worker_startup_ready(
    ready_rx: &mpsc::Receiver<()>,
    worker: &'static str,
    timeout: Duration,
) -> Result<(), SwitcherError> {
    match ready_rx.recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(SwitcherError::InputWorkerStartupTimedOut {
            worker,
            timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(SwitcherError::InputWorkerDisconnected { worker })
        }
    }
}

fn abort_input_worker_startup<T>(ready_rx: mpsc::Receiver<T>, request_stop: impl FnOnce()) {
    drop(ready_rx);
    request_stop();
}

fn run_input_worker_poll_loop(
    stop_requested: &AtomicBool,
    worker: &'static str,
    on_ready: impl FnOnce() -> Result<(), SwitcherError>,
    mut poll_once: impl FnMut() -> bool,
) -> Result<(), SwitcherError> {
    if stop_requested.load(Ordering::SeqCst) {
        return Err(SwitcherError::InputWorkerDisconnected { worker });
    }
    on_ready()?;
    while !stop_requested.load(Ordering::SeqCst) {
        if !poll_once() {
            break;
        }
    }
    Ok(())
}

fn ensure_input_dependencies_ready(
    writer_ready: bool,
    pointer_watcher_ready: bool,
    input_target_watcher_ready: bool,
) -> Result<(), SwitcherError> {
    if !writer_ready {
        return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
    }
    ensure_input_watchers_ready(pointer_watcher_ready, input_target_watcher_ready)
}

fn ensure_input_watchers_ready(
    pointer_watcher_ready: bool,
    input_target_watcher_ready: bool,
) -> Result<(), SwitcherError> {
    if !pointer_watcher_ready {
        return Err(SwitcherError::InputWorkerDisconnected {
            worker: "pointer-watcher",
        });
    }
    if !input_target_watcher_ready {
        return Err(SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        });
    }
    Ok(())
}

fn take_pointer_click_flags(physical: &AtomicBool, logical: &AtomicBool) -> bool {
    let physical = physical.swap(false, Ordering::SeqCst);
    let logical = logical.swap(false, Ordering::SeqCst);
    physical || logical
}

fn snapshot_then_acquire_grab<Target, Snapshot, Error>(
    target: &mut Target,
    snapshot: impl FnOnce(&mut Target) -> Result<Snapshot, Error>,
    acquire_grab: impl FnOnce(&mut Target) -> Result<(), Error>,
) -> Result<Snapshot, Error> {
    let snapshot = snapshot(target)?;
    acquire_grab(target)?;
    Ok(snapshot)
}

impl KeyboardController {
    pub fn prepare() -> Result<PreparedKeyboardController, SwitcherError> {
        let keyboard_path = resolve_keyboard_path()?;
        let pointer_paths = find_pointer_devices(&keyboard_path);
        let real_device = GrabbedKeyboardDevice::open(keyboard_path)?;
        println!(
            "[INFO] Клавиатура: {}",
            real_device.name().unwrap_or("Unknown")
        );
        let session_type = detect_current_session_type();
        let mut virtual_device = VirtualKeyboardWriter::new("Open-Switcher Virtual Device")?;
        let mut pointer_watcher = match PointerWatcher::spawn(pointer_paths) {
            Ok(watcher) => watcher,
            Err(error) => {
                let outcome = virtual_device.stop();
                return Err(resolve_error_after_writer_shutdown(
                    error,
                    "keyboard-prepare-pointer-watcher",
                    outcome,
                ));
            }
        };
        let input_target_watcher = match InputTargetWatcher::spawn(session_type) {
            Ok(watcher) => watcher,
            Err(error) => {
                let outcome = virtual_device.stop();
                match outcome {
                    WriterShutdownOutcome::Stopped => pointer_watcher.stop_and_join(),
                    WriterShutdownOutcome::Unresponsive { .. } => {
                        pointer_watcher.detach_for_process_fail_stop();
                    }
                }
                return Err(resolve_error_after_writer_shutdown(
                    error,
                    "keyboard-prepare-input-target-watcher",
                    outcome,
                ));
            }
        };
        log_input_debug(
            "input-pipeline-prepared",
            "writer and watchers prepared before physical keyboard grab",
        );
        println!("[OK] Open-Switcher подготовлен к безопасному захвату клавиатуры.");

        Ok(PreparedKeyboardController {
            controller: Self {
                real_device,
                pointer_watcher,
                input_target_watcher,
                virtual_device,
            },
        })
    }

    pub fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        self.real_device.fetch_events()
    }

    pub fn fetch_events_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<InputEvent>, SwitcherError> {
        self.real_device.fetch_events_timeout(timeout)
    }

    pub fn take_pointer_click_invalidation(&self) -> bool {
        take_pointer_click_flags(
            &self.pointer_watcher.click_flag,
            &self.input_target_watcher.pointer_click_flag,
        )
    }

    pub fn take_input_target_invalidation(&self) -> bool {
        self.input_target_watcher.take_change_invalidation()
    }

    pub fn release_grab_best_effort(&mut self) {
        if !self.real_device.is_ready() {
            return;
        }

        if let Err(error) = self.real_device.release_grab() {
            log_input_debug("grab-release-error", &format!("error={error}"));
        } else {
            log_input_debug("grab-released", "keyboard grab released during shutdown");
        }
    }

    pub(crate) fn shutdown(&mut self) -> WriterShutdownOutcome {
        run_keyboard_shutdown_sequence(|phase| match phase {
            KeyboardShutdownPhase::RequestWriterStop => {
                self.virtual_device.request_stop();
                None
            }
            KeyboardShutdownPhase::ReleaseGrab => {
                self.release_grab_best_effort();
                None
            }
            KeyboardShutdownPhase::FinishWriterStop => Some(self.virtual_device.finish_stop()),
            KeyboardShutdownPhase::StopAndJoinWatchers => {
                self.pointer_watcher.stop_and_join();
                self.input_target_watcher.stop_and_join();
                None
            }
            KeyboardShutdownPhase::DetachWatchers => {
                self.pointer_watcher.detach_for_process_fail_stop();
                self.input_target_watcher.detach_for_process_fail_stop();
                None
            }
        })
    }

    pub fn forward_event(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.virtual_device.handle().forward_event(key, value)
    }

    pub fn type_separator(&mut self, key: Key) -> Result<(), SwitcherError> {
        log_input_debug("type-separator-send", &format!("key={key:?}"));
        self.virtual_device.handle().type_separator(key)
    }

    pub(crate) fn apply_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        let virtual_device = self.virtual_device.handle();
        let pointer_watcher = &self.pointer_watcher;
        let input_target_watcher = &self.input_target_watcher;
        virtual_device.apply_correction_with_input_health(
            plan.clone(),
            config.clone(),
            modifiers,
            || {
                ensure_input_watchers_ready(
                    pointer_watcher.is_ready(),
                    input_target_watcher.is_ready(),
                )
                .err()
            },
        )
    }

    pub fn begin_manual_current_word_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<ManualCurrentWordStartOutcome, SwitcherError> {
        self.virtual_device.begin_manual_current_word_correction(
            plan.clone(),
            config.clone(),
            modifiers,
        )
    }

    pub fn poll_manual_current_word_completion(
        &mut self,
    ) -> Result<Option<ManualCurrentWordCompletion>, SwitcherError> {
        self.virtual_device.poll_manual_current_word_completion()
    }

    pub fn apply_same_layout_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        let virtual_device = self.virtual_device.handle();
        let pointer_watcher = &self.pointer_watcher;
        let input_target_watcher = &self.input_target_watcher;
        virtual_device.apply_same_layout_correction_with_input_health(
            plan.clone(),
            config.clone(),
            modifiers,
            || {
                ensure_input_watchers_ready(
                    pointer_watcher.is_ready(),
                    input_target_watcher.is_ready(),
                )
                .err()
            },
        )
    }

    pub fn selection_transport(
        &self,
        modifiers: SharedModifierState,
    ) -> SelectionKeyboardTransport {
        SelectionKeyboardTransport {
            virtual_device: self.virtual_device.handle(),
            modifiers,
        }
    }

    pub fn is_writer_alive(&self) -> bool {
        self.virtual_device.handle().is_alive()
    }

    pub fn writer_health_error(&self) -> Option<SwitcherError> {
        self.virtual_device.health_error()
    }

    pub fn input_worker_health_error(&self) -> Option<SwitcherError> {
        ensure_input_watchers_ready(
            self.pointer_watcher.is_ready(),
            self.input_target_watcher.is_ready(),
        )
        .err()
    }

    pub fn readiness(&self) -> InputBackendReadiness {
        let keyboard_open = self.real_device.is_ready();
        let writer_ready = self.virtual_device.handle().is_alive();
        let watchers_ready =
            self.pointer_watcher.is_ready() && self.input_target_watcher.is_ready();

        InputBackendReadiness {
            keyboard_open,
            writer_ready,
            watchers_ready,
            event_processing_ready: keyboard_open && writer_ready && watchers_ready,
        }
    }
}

impl PreparedKeyboardController {
    pub fn selection_transport(
        &self,
        modifiers: SharedModifierState,
    ) -> SelectionKeyboardTransport {
        self.controller.selection_transport(modifiers)
    }

    pub(crate) fn shutdown(&mut self) -> WriterShutdownOutcome {
        self.controller.shutdown()
    }

    pub fn activate(mut self) -> Result<(KeyboardController, bool), SwitcherError> {
        if let Err(error) = ensure_input_dependencies_ready(
            self.controller.virtual_device.handle().is_alive(),
            self.controller.pointer_watcher.is_ready(),
            self.controller.input_target_watcher.is_ready(),
        ) {
            let outcome = self.controller.shutdown();
            return Err(resolve_error_after_writer_shutdown(
                error,
                "keyboard-activate-readiness",
                outcome,
            ));
        }
        let caps_lock_active = match snapshot_then_acquire_grab(
            &mut self.controller.real_device,
            |device| Ok::<_, SwitcherError>(device.caps_lock_active().unwrap_or(false)),
            GrabbedKeyboardDevice::grab,
        ) {
            Ok(caps_lock_active) => caps_lock_active,
            Err(error) => {
                let outcome = self.controller.shutdown();
                return Err(resolve_error_after_writer_shutdown(
                    error,
                    "keyboard-activate-grab",
                    outcome,
                ));
            }
        };
        Ok((self.controller, caps_lock_active))
    }
}

impl Drop for KeyboardController {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

// Real keyboard device

impl GrabbedKeyboardDevice {
    fn open(path: PathBuf) -> Result<Self, SwitcherError> {
        let device = Device::open(&path).map_err(|error| map_keyboard_open_error(&path, error))?;
        Ok(Self {
            path,
            device,
            grabbed: false,
        })
    }

    fn name(&self) -> Option<&str> {
        self.device.name()
    }

    fn grab(&mut self) -> Result<(), SwitcherError> {
        if self.grabbed {
            return Ok(());
        }

        self.device
            .grab()
            .map_err(|error| map_keyboard_open_error(&self.path, error))?;
        self.grabbed = true;
        Ok(())
    }

    fn release_grab(&mut self) -> Result<(), SwitcherError> {
        if !self.grabbed {
            return Ok(());
        }

        self.device
            .ungrab()
            .map_err(|error| map_keyboard_open_error(&self.path, error))?;
        self.grabbed = false;
        Ok(())
    }

    fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        self.device
            .fetch_events()
            .map(|events| events.collect())
            .map_err(|error| map_keyboard_open_error(&self.path, error))
    }

    fn fetch_events_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<InputEvent>, SwitcherError> {
        if !wait_for_device_input(&self.device, timeout)? {
            return Ok(Vec::new());
        }

        self.fetch_events()
    }

    fn caps_lock_active(&self) -> Result<bool, SwitcherError> {
        self.device
            .get_led_state()
            .map(|state| state.contains(LedType::LED_CAPSL))
            .map_err(|error| map_keyboard_open_error(&self.path, error))
    }

    fn is_ready(&self) -> bool {
        self.grabbed
    }
}

impl Drop for GrabbedKeyboardDevice {
    fn drop(&mut self) {
        if !self.grabbed {
            return;
        }

        match self.device.ungrab() {
            Ok(()) => log_input_debug("grab-released", "keyboard grab released in Drop"),
            Err(error) => {
                log_input_debug(
                    "grab-release-error",
                    &format!("during_drop=true error={error}"),
                );
            }
        }
        self.grabbed = false;
    }
}

// Pointer watcher

fn retain_available_pointer_devices<T>(
    devices: &mut Vec<T>,
    mut keep_after_poll: impl FnMut(&mut T) -> bool,
) -> bool {
    let mut index = 0usize;
    while index < devices.len() {
        if keep_after_poll(&mut devices[index]) {
            index += 1;
        } else {
            devices.remove(index);
        }
    }
    !devices.is_empty()
}

fn poll_pointer_device_until_idle(
    device: &mut PointerDeviceState,
    click_flag: &AtomicBool,
) -> bool {
    let device_name = device.device.name().unwrap_or("unknown").to_string();
    loop {
        match device.device.fetch_events() {
            Ok(events) => {
                let mut had_events = false;
                for event in events {
                    had_events = true;
                    if let evdev::InputEventKind::Key(key) = event.kind() {
                        if is_pointer_click(key)
                            && event.value() == 1
                            && device.pressed_buttons.insert(key)
                        {
                            click_flag.store(true, Ordering::SeqCst);
                            log_input_debug(
                                "pointer-click",
                                &format!("device={device_name} key={key:?}"),
                            );
                        } else if is_pointer_click(key) && event.value() == 0 {
                            device.pressed_buttons.remove(&key);
                        }
                    }
                }

                if !had_events {
                    return true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
            Err(error) => {
                log_input_debug(
                    "pointer-read-error",
                    &format!("device={device_name} error={error}"),
                );
                return false;
            }
        }
    }
}

impl PointerWatcher {
    fn spawn(paths: Vec<PathBuf>) -> Result<Self, SwitcherError> {
        let click_flag = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(false));

        if paths.is_empty() {
            log_input_debug("pointer-watcher-start", "devices=0 mode=disabled");
            return Ok(Self {
                click_flag,
                stop_flag,
                alive,
                required: false,
                handle: None,
            });
        }

        let devices = open_pointer_devices(paths);
        if devices.is_empty() {
            log_input_debug(
                "pointer-watcher-start",
                "devices=0 mode=disabled reason=no-readable-pointer-devices",
            );
            return Ok(Self {
                click_flag,
                stop_flag,
                alive,
                required: false,
                handle: None,
            });
        }

        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let worker_click_flag = Arc::clone(&click_flag);
        let worker_stop_flag = Arc::clone(&stop_flag);
        let worker_alive = Arc::clone(&alive);
        let handle = thread::spawn(move || {
            let _alive_guard = WorkerAliveGuard::new(Arc::clone(&worker_alive));
            let mut devices = devices;
            log_input_debug(
                "pointer-watcher-start",
                &format!("devices={}", devices.len()),
            );
            let loop_result = run_input_worker_poll_loop(
                &worker_stop_flag,
                "pointer-watcher",
                || publish_input_worker_ready(&worker_alive, ready_tx, "pointer-watcher"),
                || {
                    if !retain_available_pointer_devices(&mut devices, |device| {
                        poll_pointer_device_until_idle(device, &worker_click_flag)
                    }) {
                        log_input_debug("pointer-watcher-stop", "reason=all-started-devices-lost");
                        return false;
                    }

                    thread::sleep(POINTER_POLL_INTERVAL);
                    true
                },
            );
            if let Err(error) = loop_result {
                log_input_debug("pointer-watcher-stop", &format!("reason={error}"));
            }

            log_input_debug("pointer-watcher-stop", "reason=shutdown");
        });
        if let Err(error) = wait_for_input_worker_startup_ready(
            &ready_rx,
            "pointer-watcher",
            INPUT_WORKER_STARTUP_READY_TIMEOUT,
        ) {
            abort_input_worker_startup(ready_rx, || {
                stop_flag.store(true, Ordering::SeqCst);
                alive.store(false, Ordering::SeqCst);
            });
            let _ = handle.join();
            return Err(error);
        }

        Ok(Self {
            click_flag,
            stop_flag,
            alive,
            required: true,
            handle: Some(handle),
        })
    }

    fn request_stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.alive.store(false, Ordering::SeqCst);
    }

    fn stop_and_join(&mut self) {
        self.request_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    fn detach_for_process_fail_stop(&mut self) {
        self.request_stop();
        drop(self.handle.take());
    }

    fn is_ready(&self) -> bool {
        !self.required || self.alive.load(Ordering::SeqCst)
    }
}

// Input target watcher

fn prepare_input_target_monitor<T>(
    session_type: SessionType,
    connect: impl FnOnce() -> io::Result<T>,
) -> Result<Option<T>, SwitcherError> {
    if !should_enable_x11_input_target_watcher(session_type) {
        return Ok(None);
    }

    connect().map(Some).map_err(|error| {
        log_input_debug(
            "input-target-watcher-start-error",
            &format!("source=x11 error={error}"),
        );
        SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        }
    })
}

impl InputTargetWatcher {
    fn spawn(session_type: SessionType) -> Result<Self, SwitcherError> {
        let changed_flag = Arc::new(AtomicBool::new(false));
        let pointer_click_flag = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(false));

        let Some(mut monitor) =
            prepare_input_target_monitor(session_type, ActiveWindowMonitor::connect)?
        else {
            log_input_debug(
                "input-target-watcher-disabled",
                &format!(
                    "reason=non-x11-session session_type={}",
                    format_session_type(session_type)
                ),
            );
            return Ok(Self::disabled(
                changed_flag,
                pointer_click_flag,
                stop_flag,
                alive,
            ));
        };

        let (stop_wakeup, worker_stop_wakeup) = input_target_stop_wakeup_pair()?;
        let stop_wakeup = Some(stop_wakeup);
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let worker_changed_flag = Arc::clone(&changed_flag);
        let worker_pointer_click_flag = Arc::clone(&pointer_click_flag);
        let worker_stop_flag = Arc::clone(&stop_flag);
        let worker_alive = Arc::clone(&alive);
        let handle = thread::spawn(move || {
            let _alive_guard = WorkerAliveGuard::new(Arc::clone(&worker_alive));
            let x11_fd = monitor.connection_fd();
            let stop_fd = worker_stop_wakeup.as_raw_fd();
            log_input_debug(
                "input-target-watcher-start",
                &format!(
                    "source=_NET_ACTIVE_WINDOW initial_window={}",
                    format_x11_window(monitor.current_window)
                ),
            );
            let loop_result = run_input_worker_poll_loop(
                &worker_stop_flag,
                "input-target-watcher",
                || publish_input_worker_ready(&worker_alive, ready_tx, "input-target-watcher"),
                || match run_x11_event_cycle(
                    &worker_stop_flag,
                    || monitor.poll_context_event(),
                    |event| {
                        publish_x11_context_event(
                            event,
                            &worker_changed_flag,
                            &worker_pointer_click_flag,
                        );
                    },
                    || wait_for_x11_or_stop(x11_fd, stop_fd),
                ) {
                    Ok(keep_running) => keep_running,
                    Err(error) => {
                        log_input_debug(
                            "input-target-read-error",
                            &format!("source=x11 error={error}"),
                        );
                        log_input_debug("input-target-watcher-stop", "reason=watcher-error");
                        false
                    }
                },
            );
            if let Err(error) = loop_result {
                log_input_debug("input-target-watcher-stop", &format!("reason={error}"));
            }

            log_input_debug("input-target-watcher-stop", "reason=shutdown");
        });
        if let Err(error) = wait_for_input_worker_startup_ready(
            &ready_rx,
            "input-target-watcher",
            INPUT_WORKER_STARTUP_READY_TIMEOUT,
        ) {
            abort_input_worker_startup(ready_rx, || {
                stop_flag.store(true, Ordering::SeqCst);
                alive.store(false, Ordering::SeqCst);
                signal_input_target_stop(stop_wakeup.as_ref());
            });
            let _ = handle.join();
            return Err(error);
        }

        Ok(Self {
            changed_flag,
            pointer_click_flag,
            stop_flag,
            alive,
            required: true,
            handle: Some(handle),
            stop_wakeup,
        })
    }

    fn disabled(
        changed_flag: Arc<AtomicBool>,
        pointer_click_flag: Arc<AtomicBool>,
        stop_flag: Arc<AtomicBool>,
        alive: Arc<AtomicBool>,
    ) -> Self {
        Self {
            changed_flag,
            pointer_click_flag,
            stop_flag,
            alive,
            required: false,
            handle: None,
            stop_wakeup: None,
        }
    }

    fn take_change_invalidation(&self) -> bool {
        self.changed_flag.swap(false, Ordering::SeqCst)
    }

    fn request_stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.alive.store(false, Ordering::SeqCst);
        signal_input_target_stop(self.stop_wakeup.as_ref());
    }

    fn stop_and_join(&mut self) {
        self.request_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    fn detach_for_process_fail_stop(&mut self) {
        self.request_stop();
        drop(self.handle.take());
    }

    fn is_ready(&self) -> bool {
        !self.required || self.alive.load(Ordering::SeqCst)
    }
}

fn detect_current_session_type() -> SessionType {
    SystemContextDetector::detect_current()
        .map(|context| context.session_type)
        .unwrap_or(SessionType::Unknown)
}

fn should_enable_x11_input_target_watcher(session_type: SessionType) -> bool {
    session_type == SessionType::X11
}

pub(crate) fn is_wayland_focus_switch_shortcut(
    modifiers: ModifierState,
    key: Key,
    value: i32,
) -> bool {
    if value != 1 || key != Key::KEY_TAB || modifiers.is_ctrl_pressed() {
        return false;
    }

    let alt_pressed = modifiers.is_alt_pressed();
    let meta_pressed = modifiers.is_meta_pressed();

    (alt_pressed && !meta_pressed) || (meta_pressed && !alt_pressed)
}

fn format_session_type(session_type: SessionType) -> &'static str {
    match session_type {
        SessionType::X11 => "x11",
        SessionType::Wayland => "wayland",
        SessionType::Unknown => "unknown",
    }
}

fn initialize_x11_switcher_for_session<T, F>(session_type: SessionType, init_x11: F) -> Option<T>
where
    F: FnOnce() -> Result<T, SwitcherError>,
{
    let strategy = LayoutSwitchStrategy::for_session_type(session_type);

    match strategy {
        LayoutSwitchStrategy::X11 => match init_x11() {
            Ok(switcher) => {
                log_input_debug(
                    "layout-switch-strategy",
                    &format!(
                        "session_type={} strategy={}",
                        format_session_type(session_type),
                        strategy.as_str()
                    ),
                );
                Some(switcher)
            }
            Err(error) => {
                log_input_debug(
                    "layout-switch-strategy",
                    &format!(
                        "session_type={} strategy=uinput reason=x11-init-failed error={error}",
                        format_session_type(session_type)
                    ),
                );
                None
            }
        },
        LayoutSwitchStrategy::UinputFallback => {
            log_input_debug(
                "layout-switch-strategy",
                &format!(
                    "session_type={} strategy={}",
                    format_session_type(session_type),
                    strategy.as_str()
                ),
            );
            None
        }
    }
}

// Virtual keyboard writer

impl VirtualKeyboardWriter {
    fn new(name: &str) -> Result<Self, SwitcherError> {
        let device = create_virtual_keyboard(name)?;
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let alive = Arc::new(AtomicBool::new(false));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let transaction_failure_request_id = Arc::new(AtomicU64::new(0));
        let transaction_terminal_gate = Arc::new(Mutex::new(()));
        let worker_alive = Arc::clone(&alive);
        let worker_stop_requested = Arc::clone(&stop_requested);
        let worker_transaction_failure_request_id = Arc::clone(&transaction_failure_request_id);
        let worker_transaction_terminal_gate = Arc::clone(&transaction_terminal_gate);
        let worker_ready_alive = Arc::clone(&alive);

        let join_handle = thread::spawn(move || {
            run_writer_thread_with_exit_notification(device, exit_tx, |device| {
                log_input_debug("writer-start", "virtual keyboard writer thread started");
                let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_virtual_keyboard_writer_loop(
                        device,
                        command_rx,
                        completion_tx,
                        worker_transaction_failure_request_id,
                        worker_stop_requested,
                        worker_transaction_terminal_gate,
                        worker_ready_alive,
                        ready_tx,
                    )
                }));
                worker_alive.store(false, Ordering::SeqCst);

                match loop_result {
                    Ok(Ok(())) => {
                        log_input_debug("writer-stop", "virtual keyboard writer thread stopped");
                    }
                    Ok(Err(error)) => {
                        log_input_debug("writer-error", &format!("error={error}"));
                        eprintln!("[input] Ошибка writer path виртуальной клавиатуры: {error}");
                    }
                    Err(payload) => {
                        let reason = if let Some(text) = payload.downcast_ref::<&str>() {
                            *text
                        } else if let Some(text) = payload.downcast_ref::<String>() {
                            text.as_str()
                        } else {
                            "unknown panic payload"
                        };
                        log_input_debug("writer-panic", &format!("reason={reason}"));
                        eprintln!(
                            "[input] Writer path виртуальной клавиатуры аварийно завершился: {reason}"
                        );
                    }
                }
            });
        });

        let mut writer = Self {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive,
                stop_requested,
                transaction_failure_request_id,
                transaction_terminal_gate,
            },
            join_handle: Some(join_handle),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        writer.finish_startup(
            ready_rx,
            WRITER_STARTUP_READY_TIMEOUT,
            WRITER_SHUTDOWN_ACK_TIMEOUT,
        )?;
        Ok(writer)
    }

    fn handle(&self) -> VirtualKeyboardHandle {
        self.handle.clone()
    }

    fn finish_startup(
        &mut self,
        ready_rx: mpsc::Receiver<()>,
        startup_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<(), SwitcherError> {
        let startup_error = match wait_for_writer_startup_ready(&ready_rx, startup_timeout) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        drop(ready_rx);
        let trigger = startup_error.to_string();

        match self.stop_with_timeout(shutdown_timeout) {
            WriterShutdownOutcome::Stopped => Err(startup_error),
            WriterShutdownOutcome::Unresponsive { timeout_ms } => {
                Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                    timeout_ms,
                    phase: "writer-startup",
                    trigger,
                })
            }
        }
    }

    fn request_stop(&mut self) {
        self.shutdown_started_at.get_or_insert_with(Instant::now);
        let _terminal_guard = self
            .handle
            .transaction_terminal_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.handle.stop_requested.store(true, Ordering::SeqCst);
        self.handle.alive.store(false, Ordering::SeqCst);
    }

    fn finish_stop(&mut self) -> WriterShutdownOutcome {
        self.finish_stop_with_timeout(WRITER_SHUTDOWN_ACK_TIMEOUT)
    }

    fn finish_stop_with_timeout(&mut self, timeout: Duration) -> WriterShutdownOutcome {
        if let Some(outcome) = self.shutdown_outcome {
            return outcome;
        }

        if self.join_handle.is_some() {
            self.send_shutdown_command_with_timeout(timeout);
        }
        self.join_writer_thread_with_timeout(timeout)
    }

    fn stop(&mut self) -> WriterShutdownOutcome {
        self.stop_with_timeout(WRITER_SHUTDOWN_ACK_TIMEOUT)
    }

    fn stop_with_timeout(&mut self, timeout: Duration) -> WriterShutdownOutcome {
        self.request_stop();
        self.finish_stop_with_timeout(timeout)
    }

    fn send_shutdown_command_with_timeout(&self, timeout: Duration) {
        let started = Instant::now();
        let shutdown_started = self.shutdown_started_at.unwrap_or(started);
        let shutdown_deadline = shutdown_started
            .checked_add(timeout)
            .unwrap_or(shutdown_started);
        let mut yielded = false;
        let mut command = WriterCommand::Shutdown;

        loop {
            match self.handle.command_tx.try_send(command) {
                Ok(()) => {
                    if yielded {
                        log_input_debug(
                            "writer-shutdown-backpressure-recovered",
                            &format!("elapsed_us={}", started.elapsed().as_micros()),
                        );
                    }
                    return;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    log_input_debug("writer-shutdown-disconnected", "receiver_closed=true");
                    return;
                }
                Err(mpsc::TrySendError::Full(returned_command)) => {
                    if Instant::now() >= shutdown_deadline
                        || started.elapsed() >= SHUTDOWN_SEND_RETRY_WINDOW
                    {
                        log_input_debug(
                            "writer-shutdown-backpressure-failed",
                            &format!(
                                "elapsed_us={} retry_window_us={}",
                                started.elapsed().as_micros(),
                                SHUTDOWN_SEND_RETRY_WINDOW.as_micros()
                            ),
                        );
                        return;
                    }

                    if !yielded {
                        log_input_debug(
                            "writer-shutdown-backpressure",
                            &format!("retry_window_us={}", SHUTDOWN_SEND_RETRY_WINDOW.as_micros()),
                        );
                        yielded = true;
                    }
                    command = returned_command;
                    thread::yield_now();
                }
            }
        }
    }

    fn join_writer_thread_with_timeout(&mut self, timeout: Duration) -> WriterShutdownOutcome {
        let timeout_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        let started = self.shutdown_started_at.unwrap_or_else(Instant::now);
        let deadline = started.checked_add(timeout).unwrap_or(started);
        let mut wait_logged = false;

        loop {
            let Some(join_handle) = self.join_handle.as_ref() else {
                self.shutdown_outcome = Some(WriterShutdownOutcome::Stopped);
                return WriterShutdownOutcome::Stopped;
            };

            if join_handle.is_finished() {
                let join_handle = self
                    .join_handle
                    .take()
                    .expect("finished writer handle must still be owned");
                let _ = join_handle.join();
                self.shutdown_outcome = Some(WriterShutdownOutcome::Stopped);
                return WriterShutdownOutcome::Stopped;
            }

            let now = Instant::now();
            if now >= deadline {
                let outcome = WriterShutdownOutcome::Unresponsive { timeout_ms };
                self.shutdown_outcome = Some(outcome);
                log_input_debug(
                    "writer-shutdown-join-timeout",
                    &format!(
                        "elapsed_us={} deadline_us={}",
                        started.elapsed().as_micros(),
                        timeout.as_micros()
                    ),
                );
                return outcome;
            }

            if !wait_logged {
                log_input_debug(
                    "writer-shutdown-join-wait",
                    &format!("deadline_us={}", timeout.as_micros()),
                );
                wait_logged = true;
            }

            let remaining = deadline.saturating_duration_since(now);
            match self.exit_rx.recv_timeout(remaining) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    thread::park_timeout(remaining.min(Duration::from_millis(1)));
                }
            }
        }
    }

    fn begin_manual_current_word_correction(
        &mut self,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<ManualCurrentWordStartOutcome, SwitcherError> {
        let timeout = correction_transaction_timeout(&plan, &config, true)?;
        self.begin_manual_current_word_correction_with_timeout(plan, config, modifiers, timeout)
    }

    fn begin_manual_current_word_correction_with_timeout(
        &mut self,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
        timeout: Duration,
    ) -> Result<ManualCurrentWordStartOutcome, SwitcherError> {
        self.handle.ensure_alive()?;
        if self.pending_manual_current_word.is_some() {
            return Ok(ManualCurrentWordStartOutcome::RejectedBeforeMutation(
                "manual-current-word-already-in-progress".to_string(),
            ));
        }
        let request_id = take_next_nonzero_request_id(&mut self.next_request_id);
        let control = WriterTransactionControl::new_with_writer_state(
            request_id,
            timeout.min(MAX_TRANSACTION_TIMEOUT),
            Arc::clone(&self.handle.transaction_failure_request_id),
            Arc::clone(&self.handle.stop_requested),
            Arc::clone(&self.handle.transaction_terminal_gate),
        );

        match self
            .handle
            .command_tx
            .try_send(WriterCommand::DeferredManualCurrentWordCorrection {
                control: control.clone(),
                plan,
                config,
                modifiers,
            }) {
            Ok(()) => {
                self.pending_manual_current_word = Some(PendingManualCurrentWordTransaction {
                    control,
                    completion: None,
                });
                Ok(ManualCurrentWordStartOutcome::Started(request_id))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(SwitcherError::VirtualKeyboardWriterDisconnected)
            }
            Err(mpsc::TrySendError::Full(_)) => {
                Ok(ManualCurrentWordStartOutcome::RejectedBeforeMutation(
                    "virtual-keyboard-writer-saturated".to_string(),
                ))
            }
        }
    }

    fn poll_manual_current_word_completion(
        &mut self,
    ) -> Result<Option<ManualCurrentWordCompletion>, SwitcherError> {
        let (received, completion_channel_disconnected) = match self.completion_rx.try_recv() {
            Ok(completion) => (Some(completion), false),
            Err(mpsc::TryRecvError::Empty) => (None, false),
            Err(mpsc::TryRecvError::Disconnected) => (None, true),
        };

        let Some(pending) = self.pending_manual_current_word.as_mut() else {
            if received.is_some() {
                return Ok(received);
            }
            if completion_channel_disconnected {
                return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
            }
            self.handle.ensure_alive()?;
            return Ok(None);
        };
        if let Some(completion) = received {
            if completion.request_id != pending.control.request_id() {
                return Err(SwitcherError::Io(io::Error::other(format!(
                    "deferred completion request mismatch: pending={} received={}",
                    pending.control.request_id(),
                    completion.request_id,
                ))));
            }
            pending.completion = Some(completion);
        }

        loop {
            match pending.control.state() {
                WriterTransactionState::Pending => {
                    #[cfg(test)]
                    run_deferred_poll_before_pending_active_check_hook();
                    if let Err(error) = pending.control.ensure_active() {
                        if pending.control.state() == WriterTransactionState::Completed {
                            continue;
                        }
                        return Err(error);
                    }
                    if completion_channel_disconnected {
                        if pending.control.state() == WriterTransactionState::Completed {
                            continue;
                        }
                        return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
                    }
                    let health_error = self.handle.health_error();
                    match pending.control.state() {
                        WriterTransactionState::Completed => continue,
                        WriterTransactionState::TimedOut => {
                            return Err(pending.control.timed_out_error());
                        }
                        WriterTransactionState::Pending => {}
                    }
                    return match health_error {
                        Some(error) => Err(error),
                        None => Ok(None),
                    };
                }
                WriterTransactionState::TimedOut => {
                    return Err(pending.control.timed_out_error());
                }
                WriterTransactionState::Completed => {
                    let completion = match pending.completion.take() {
                        Some(completion) => completion,
                        None => self
                            .completion_rx
                            .try_recv()
                            .map_err(|_| SwitcherError::VirtualKeyboardWriterDisconnected)?,
                    };
                    if completion.request_id != pending.control.request_id() {
                        return Err(SwitcherError::Io(io::Error::other(format!(
                            "deferred completion request mismatch: pending={} received={}",
                            pending.control.request_id(),
                            completion.request_id,
                        ))));
                    }
                    self.pending_manual_current_word = None;
                    return Ok(Some(completion));
                }
            }
        }
    }

    fn health_error(&self) -> Option<SwitcherError> {
        if let Some(pending) = self.pending_manual_current_word.as_ref() {
            loop {
                match pending.control.state() {
                    WriterTransactionState::Completed => return None,
                    WriterTransactionState::TimedOut => {
                        return Some(pending.control.timed_out_error());
                    }
                    WriterTransactionState::Pending => {
                        if let Err(error) = pending.control.ensure_active() {
                            if pending.control.state() == WriterTransactionState::Completed {
                                continue;
                            }
                            return Some(error);
                        }
                        #[cfg(test)]
                        run_deferred_health_before_handle_check_hook();
                        let health_error = self.handle.health_error();
                        match pending.control.state() {
                            WriterTransactionState::Completed => continue,
                            WriterTransactionState::TimedOut => {
                                return Some(pending.control.timed_out_error());
                            }
                            WriterTransactionState::Pending => return health_error,
                        }
                    }
                }
            }
        }
        self.handle.health_error()
    }
}

impl Drop for VirtualKeyboardWriter {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl VirtualKeyboardHandle {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && self.transaction_failure_request_id().is_none()
    }

    fn transaction_failure_request_id(&self) -> Option<u64> {
        match self.transaction_failure_request_id.load(Ordering::SeqCst) {
            0 => None,
            request_id => Some(request_id),
        }
    }

    fn ensure_alive(&self) -> Result<(), SwitcherError> {
        if let Some(error) = self.health_error() {
            return Err(error);
        }
        Ok(())
    }

    fn health_error(&self) -> Option<SwitcherError> {
        if let Some(request_id) = self.transaction_failure_request_id() {
            return Some(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id });
        }
        if !self.alive.load(Ordering::SeqCst) {
            return Some(SwitcherError::VirtualKeyboardWriterDisconnected);
        }
        None
    }

    fn forward_event(&self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.ensure_alive()?;
        self.send_fast_command(WriterFastCommand::ForwardEvent { key, value })
    }

    fn type_separator(&self, key: Key) -> Result<(), SwitcherError> {
        self.ensure_alive()?;
        log_input_debug("type-separator-queue", &format!("key={key:?}"));
        self.send_fast_command(WriterFastCommand::TypeSeparator { key })
    }

    fn apply_correction_with_input_health(
        &self,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
        input_worker_health_error: impl FnMut() -> Option<SwitcherError>,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        self.run_transaction_with_input_health(
            WriterTransactionKind::ApplyCorrection {
                plan,
                config,
                modifiers,
            },
            input_worker_health_error,
        )
    }

    fn apply_same_layout_correction_with_input_health(
        &self,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
        input_worker_health_error: impl FnMut() -> Option<SwitcherError>,
    ) -> Result<(), SwitcherError> {
        self.run_transaction_with_input_health(
            WriterTransactionKind::ApplySameLayoutCorrection {
                plan,
                config,
                modifiers,
            },
            input_worker_health_error,
        )
        .map(|_| ())
    }

    fn send_copy_shortcut(&self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.run_transaction(WriterTransactionKind::CopyShortcut { modifiers })
            .map(|_| ())
    }

    fn send_paste_shortcut(&self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.run_transaction(WriterTransactionKind::PasteShortcut { modifiers })
            .map(|_| ())
    }

    fn run_transaction(
        &self,
        kind: WriterTransactionKind,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        let timeout = kind.execution_timeout()?;
        self.run_transaction_with_timeout(kind, timeout)
    }

    fn run_transaction_with_input_health(
        &self,
        kind: WriterTransactionKind,
        input_worker_health_error: impl FnMut() -> Option<SwitcherError>,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        let timeout = kind.execution_timeout()?;
        self.run_transaction_with_timeout_and_input_health(kind, timeout, input_worker_health_error)
    }

    fn run_transaction_with_timeout(
        &self,
        kind: WriterTransactionKind,
        timeout: Duration,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        self.run_transaction_with_timeout_and_input_health(kind, timeout, || None)
    }

    fn run_transaction_with_timeout_and_input_health(
        &self,
        kind: WriterTransactionKind,
        timeout: Duration,
        mut input_worker_health_error: impl FnMut() -> Option<SwitcherError>,
    ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
        self.ensure_alive()?;
        if let Some(error) = input_worker_health_error() {
            return Err(error);
        }
        let request_id = next_writer_transaction_request_id();
        let control = WriterTransactionControl::new_with_writer_state(
            request_id,
            timeout,
            Arc::clone(&self.transaction_failure_request_id),
            Arc::clone(&self.stop_requested),
            Arc::clone(&self.transaction_terminal_gate),
        );
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send_transaction_command(WriterTransaction::Execute {
            control: control.clone(),
            kind,
            reply: reply_tx,
        })?;
        control.wait_for_reply_with_input_health(reply_rx, input_worker_health_error)
    }

    fn send_transaction_command(
        &self,
        transaction: WriterTransaction,
    ) -> Result<(), SwitcherError> {
        let control = transaction.control().clone();
        let started = Instant::now();
        let retry_deadline = started
            .checked_add(TRANSACTION_SEND_RETRY_WINDOW)
            .unwrap_or(control.deadline)
            .min(control.deadline);
        let mut yielded = false;
        let mut command = WriterCommand::Transaction(transaction);

        loop {
            control.ensure_active()?;
            self.ensure_alive()?;
            if yielded && Instant::now() >= retry_deadline {
                control.ensure_active()?;
                log_input_debug(
                    "writer-transaction-backpressure-failed",
                    &format!(
                        "elapsed_us={} retry_window_us={}",
                        started.elapsed().as_micros(),
                        TRANSACTION_SEND_RETRY_WINDOW.as_micros()
                    ),
                );
                return Err(SwitcherError::VirtualKeyboardWriterSaturated);
            }

            match self.command_tx.try_send(command) {
                Ok(()) => {
                    if yielded {
                        log_input_debug(
                            "writer-transaction-backpressure-recovered",
                            &format!("elapsed_us={}", started.elapsed().as_micros()),
                        );
                    }
                    return Ok(());
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
                }
                Err(mpsc::TrySendError::Full(returned_command)) => {
                    if !yielded {
                        log_input_debug(
                            "writer-transaction-backpressure",
                            &format!(
                                "retry_window_us={}",
                                TRANSACTION_SEND_RETRY_WINDOW.as_micros()
                            ),
                        );
                        yielded = true;
                    }
                    command = returned_command;
                    thread::yield_now();
                }
            }
        }
    }

    fn send_fast_command(&self, command: WriterFastCommand) -> Result<(), SwitcherError> {
        self.ensure_alive()?;
        let started = Instant::now();
        let mut yielded = false;

        loop {
            match self
                .command_tx
                .try_send(WriterCommand::Fast(command.clone()))
            {
                Ok(()) => {
                    if yielded {
                        log_input_debug(
                            "writer-backpressure-recovered",
                            &format!("elapsed_us={}", started.elapsed().as_micros()),
                        );
                    }
                    return Ok(());
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    self.ensure_alive()?;
                    if started.elapsed() >= FAST_PATH_SATURATION_RETRY_WINDOW {
                        log_input_debug(
                            "writer-backpressure-failed",
                            &format!(
                                "elapsed_us={} retry_window_us={}",
                                started.elapsed().as_micros(),
                                FAST_PATH_SATURATION_RETRY_WINDOW.as_micros()
                            ),
                        );
                        return Err(SwitcherError::VirtualKeyboardWriterSaturated);
                    }

                    if !yielded {
                        log_input_debug(
                            "writer-backpressure",
                            &format!(
                                "retry_window_us={}",
                                FAST_PATH_SATURATION_RETRY_WINDOW.as_micros()
                            ),
                        );
                        yielded = true;
                    }
                    thread::yield_now();
                }
            }
        }
    }
}

impl Drop for PointerWatcher {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

impl Drop for InputTargetWatcher {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

impl ActiveWindowMonitor {
    fn connection_fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    fn connect() -> io::Result<Self> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _, EventMask};

        let (conn, screen_num) =
            x11rb::connect(None).map_err(|error| io::Error::other(error.to_string()))?;
        let root = conn.setup().roots[screen_num].root;
        let active_window_atom = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?
            .atom;

        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|error| io::Error::other(error.to_string()))?
        .check()
        .map_err(|error| io::Error::other(error.to_string()))?;
        conn.flush()
            .map_err(|error| io::Error::other(error.to_string()))?;

        match Self::subscribe_pointer_clicks(&conn, root) {
            Ok(()) => log_input_debug(
                "input-target-xinput2",
                "enabled=true source=raw-button-press",
            ),
            Err(error) => log_input_debug(
                "input-target-xinput2",
                &format!("enabled=false reason=subscription-failed error={error}"),
            ),
        }

        let current_window = Self::query_active_window(&conn, root, active_window_atom)?;
        Ok(Self {
            conn,
            root,
            active_window_atom,
            current_window,
        })
    }

    fn subscribe_pointer_clicks(
        conn: &x11rb::rust_connection::RustConnection,
        root: u32,
    ) -> io::Result<()> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xinput::ConnectionExt as _;

        conn.xinput_xi_query_version(2, 0)
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?;
        conn.xinput_xi_select_events(root, &[xinput_pointer_click_event_mask()])
            .map_err(|error| io::Error::other(error.to_string()))?
            .check()
            .map_err(|error| io::Error::other(error.to_string()))?;
        conn.flush()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn poll_context_event(&mut self) -> io::Result<Option<X11ContextEvent>> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::Event;

        loop {
            let Some(event) = self
                .conn
                .poll_for_event()
                .map_err(|error| io::Error::other(error.to_string()))?
            else {
                return Ok(None);
            };

            if let Some(event) = x11_pointer_click_event(&event) {
                return Ok(Some(event));
            }

            match event {
                Event::PropertyNotify(property)
                    if property.window == self.root && property.atom == self.active_window_atom =>
                {
                    let previous_window = self.current_window;
                    let current_window =
                        Self::query_active_window(&self.conn, self.root, self.active_window_atom)?;
                    if current_window != previous_window {
                        self.current_window = current_window;
                        return Ok(Some(X11ContextEvent::ActiveWindowChanged {
                            previous: previous_window,
                            current: current_window,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    fn query_active_window(
        conn: &x11rb::rust_connection::RustConnection,
        root: u32,
        active_window_atom: u32,
    ) -> io::Result<Option<u32>> {
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

        let reply = conn
            .get_property(false, root, active_window_atom, AtomEnum::WINDOW, 0, 1)
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?;

        Ok(reply.value32().and_then(|mut values| values.next()))
    }
}

// Device discovery

fn configured_keyboard_path() -> Option<PathBuf> {
    let raw = env::var_os(KEYBOARD_PATH_ENV)?;
    if raw.is_empty() {
        return None;
    }

    Some(PathBuf::from(raw))
}

fn resolve_keyboard_path() -> Result<PathBuf, SwitcherError> {
    if let Some(path) = configured_keyboard_path() {
        log_input_debug(
            "keyboard-detect",
            &format!("source=env path={}", path.display()),
        );
        return Ok(path);
    }

    if let Some(path) = find_keyboard_via_symlinks()? {
        return Ok(path);
    }

    match find_keyboard() {
        Some(path) => {
            log_input_debug(
                "keyboard-detect",
                &format!("source=enumerate path={}", path.display()),
            );
            Ok(path)
        }
        None => {
            log_input_debug("keyboard-detect", "source=enumerate result=not-found");
            Err(SwitcherError::KeyboardNotFound)
        }
    }
}

fn find_keyboard_via_symlinks() -> Result<Option<PathBuf>, SwitcherError> {
    let search_roots: Vec<&Path> = KEYBOARD_SYMLINK_DIRS.iter().map(Path::new).collect();
    let candidates = collect_keyboard_symlink_candidates_from_dirs(&search_roots);
    log_input_debug(
        "keyboard-detect-symlinks",
        &format!("candidates={}", candidates.len()),
    );

    let mut first_access_denied: Option<(PathBuf, io::Error)> = None;

    for path in candidates {
        match Device::open(&path) {
            Ok(_) => {
                log_input_debug(
                    "keyboard-detect-symlinks",
                    &format!("result=selected path={}", path.display()),
                );
                return Ok(Some(path));
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                log_input_debug(
                    "keyboard-detect-symlinks",
                    &format!("result=access-denied path={} error={error}", path.display()),
                );
                if first_access_denied.is_none() {
                    first_access_denied = Some((path, error));
                }
            }
            Err(error) => {
                log_input_debug(
                    "keyboard-detect-symlinks",
                    &format!("result=skip path={} error={error}", path.display()),
                );
            }
        }
    }

    if let Some((path, error)) = first_access_denied {
        return Err(map_keyboard_open_error(&path, error));
    }

    Ok(None)
}

fn collect_keyboard_symlink_candidates_from_dirs(dirs: &[&Path]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };

        let mut dir_candidates: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(KEYBOARD_SYMLINK_SUFFIX))
            })
            .collect();
        dir_candidates.sort();
        candidates.extend(dir_candidates);
    }

    candidates
}

fn map_keyboard_open_error(path: &Path, error: io::Error) -> SwitcherError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        SwitcherError::KeyboardAccessDenied {
            path: path.to_path_buf(),
            source: error,
        }
    } else {
        SwitcherError::Io(error)
    }
}

fn open_pointer_devices(paths: Vec<PathBuf>) -> Vec<PointerDeviceState> {
    let mut devices = Vec::new();

    for path in paths {
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        if set_nonblocking(&device).is_ok() {
            devices.push(PointerDeviceState {
                device,
                pressed_buttons: HashSet::new(),
            });
        }
    }

    devices
}

fn create_virtual_keyboard(name: &str) -> Result<uinput::Device, SwitcherError> {
    ensure_uinput_writable()?;

    let virtual_device = uinput::default()?
        .name(name)?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(500));
    Ok(virtual_device)
}

fn ensure_uinput_writable() -> Result<PathBuf, SwitcherError> {
    let mut first_existing_path: Option<PathBuf> = None;
    let mut last_error: Option<io::Error> = None;

    for raw_path in UINPUT_PATHS {
        let path = PathBuf::from(raw_path);
        match OpenOptions::new().write(true).open(&path) {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                last_error = Some(error);
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Err(SwitcherError::UinputAccessDenied {
                    path,
                    source: error,
                });
            }
            Err(error) => {
                if first_existing_path.is_none() && path.exists() {
                    first_existing_path = Some(path);
                }
                last_error = Some(error);
            }
        }
    }

    if let Some(path) = first_existing_path {
        if let Some(error) = last_error {
            return Err(SwitcherError::Io(io::Error::new(
                error.kind(),
                format!("failed to open {}: {}", path.display(), error),
            )));
        }
    }

    Err(last_error.map(SwitcherError::Io).unwrap_or_else(|| {
        SwitcherError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "uinput device not found",
        ))
    }))
}

fn set_nonblocking(device: &Device) -> io::Result<()> {
    let fd = device.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn wait_for_device_input(device: &Device, timeout: Duration) -> io::Result<bool> {
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    let mut poll_fd = libc::pollfd {
        fd: device.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result > 0 {
            return Ok((poll_fd.revents & libc::POLLIN) != 0);
        }
        if result == 0 {
            return Ok(false);
        }

        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

pub fn is_russian_layout() -> Result<bool, SwitcherError> {
    let display = env::var("DISPLAY").unwrap_or_else(|_| ":0.0".to_string());
    let xauthority = env::var_os("XAUTHORITY").unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".Xauthority")
            .into_os_string()
    });

    let output = Command::new("xset")
        .env("DISPLAY", display)
        .env("XAUTHORITY", xauthority)
        .arg("-q")
        .output()
        .map_err(SwitcherError::Xset)?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("LED mask:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(mask_str) = parts.last() {
                if let Ok(mask) = u32::from_str_radix(mask_str, 16) {
                    return Ok((mask & 0x1000) != 0);
                }
            }
        }
    }

    Ok(false)
}

pub fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::KEY_LEFTCTRL
            | Key::KEY_RIGHTCTRL
            | Key::KEY_LEFTSHIFT
            | Key::KEY_RIGHTSHIFT
            | Key::KEY_LEFTALT
            | Key::KEY_RIGHTALT
            | Key::KEY_LEFTMETA
            | Key::KEY_RIGHTMETA
            | Key::KEY_CAPSLOCK
    )
}

pub fn is_character(key: Key) -> bool {
    let code = key.code();
    (code >= Key::KEY_1.code() && code <= Key::KEY_EQUAL.code())
        || (code >= Key::KEY_Q.code() && code <= Key::KEY_RIGHTBRACE.code())
        || (code >= Key::KEY_A.code() && code <= Key::KEY_GRAVE.code())
        || (code >= Key::KEY_BACKSLASH.code() && code <= Key::KEY_SLASH.code())
}

pub fn hotkey_trigger_to_evdev_key(trigger: HotkeyTrigger) -> Key {
    match trigger {
        HotkeyTrigger::F9 => Key::KEY_F9,
        HotkeyTrigger::F10 => Key::KEY_F10,
        HotkeyTrigger::F12 => Key::KEY_F12,
        HotkeyTrigger::Pause => Key::KEY_PAUSE,
        HotkeyTrigger::ScrollLock => Key::KEY_SCROLLLOCK,
        HotkeyTrigger::Insert => Key::KEY_INSERT,
        HotkeyTrigger::Menu => Key::KEY_MENU,
    }
}

pub fn undo_key_to_evdev_key(key: UndoKey) -> Key {
    hotkey_trigger_to_evdev_key(HotkeyTrigger::from(key))
}

fn find_keyboard() -> Option<PathBuf> {
    for (path, device) in enumerate() {
        let name = device.name().unwrap_or("");
        if name.contains("Virtual") || name.contains("Button") || name.contains("Camera") {
            continue;
        }

        if let Some(keys) = device.supported_keys() {
            if keys.contains(Key::KEY_ENTER)
                && keys.contains(Key::KEY_SPACE)
                && keys.contains(Key::KEY_A)
            {
                return Some(path);
            }
        }
    }

    None
}

fn find_pointer_devices(excluded_keyboard_path: &PathBuf) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for (path, device) in enumerate() {
        if &path == excluded_keyboard_path {
            continue;
        }

        let name = device.name().unwrap_or("");
        if name.contains("Camera") {
            continue;
        }

        let Some(keys) = device.supported_keys() else {
            continue;
        };

        if keys.contains(Key::BTN_LEFT)
            || keys.contains(Key::BTN_RIGHT)
            || keys.contains(Key::BTN_MIDDLE)
            || keys.contains(Key::BTN_SIDE)
            || keys.contains(Key::BTN_EXTRA)
            || keys.contains(Key::BTN_TOUCH)
            || keys.contains(Key::BTN_TOOL_FINGER)
            || keys.contains(Key::BTN_TOOL_DOUBLETAP)
        {
            paths.push(path);
        }
    }

    paths
}

fn is_pointer_click(key: Key) -> bool {
    matches!(
        key,
        Key::BTN_LEFT
            | Key::BTN_RIGHT
            | Key::BTN_MIDDLE
            | Key::BTN_SIDE
            | Key::BTN_EXTRA
            | Key::BTN_FORWARD
            | Key::BTN_BACK
            | Key::BTN_TASK
    )
}

fn is_x11_pointer_click(detail: u32) -> bool {
    matches!(detail, 1 | 2 | 3 | 8 | 9)
}

fn format_x11_window(window: Option<u32>) -> String {
    match window {
        Some(window) => format!("0x{window:x}"),
        None => "none".to_string(),
    }
}

// Selection keyboard transport

impl SelectionKeyboardTransport {
    pub fn send_copy_shortcut(&mut self) -> Result<(), SwitcherError> {
        // Take a fresh modifier snapshot for each shortcut so copy/paste does not
        // replay stale modifier state across the whole selected-text operation.
        self.virtual_device
            .send_copy_shortcut(self.modifiers.snapshot())
    }

    pub fn send_paste_shortcut(&mut self) -> Result<(), SwitcherError> {
        self.virtual_device
            .send_paste_shortcut(self.modifiers.snapshot())
    }
}

// Correction replay

fn ensure_transaction_active(
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    match control {
        Some(control) => control.ensure_active(),
        None => Ok(()),
    }
}

fn authorize_transaction_mutation(
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    match control {
        Some(control) => control.authorize_mutation_start(),
        None => Ok(()),
    }
}

fn sleep_for_transaction(
    control: Option<&WriterTransactionControl>,
    duration: Duration,
) -> Result<(), SwitcherError> {
    match control {
        Some(control) => control.sleep_interruptibly(duration),
        None => {
            thread::sleep(duration);
            Ok(())
        }
    }
}

fn release_modifiers(
    device: &mut uinput::Device,
    modifiers: ModifierState,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    authorize_transaction_mutation(control)?;
    release_uinput_modifiers_exhaustively(device, modifiers)?;
    sleep_for_transaction(control, Duration::from_millis(MODIFIER_SYNC_DELAY_MS))?;
    Ok(())
}

fn restore_modifiers(
    device: &mut uinput::Device,
    modifiers: ModifierState,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    let mut restored = Vec::new();
    for key in pressed_uinput_modifier_keys(modifiers) {
        if let Err(error) = authorize_transaction_mutation(control) {
            release_uinput_keys_best_effort(device, &restored);
            return Err(error);
        }
        if let Err(error) = device.press(&key) {
            release_uinput_keys_best_effort(device, &restored);
            return Err(error.into());
        }
        restored.push(key);
    }
    if let Err(error) = device.synchronize() {
        release_uinput_keys_best_effort(device, &restored);
        return Err(error.into());
    }
    if let Err(error) = ensure_transaction_active(control) {
        release_uinput_keys_best_effort(device, &restored);
        return Err(error);
    }
    Ok(())
}

fn pressed_uinput_modifier_keys(modifiers: ModifierState) -> Vec<uinput::event::keyboard::Key> {
    use uinput::event::keyboard::Key;

    let mut keys = Vec::new();
    if modifiers.left_shift {
        keys.push(Key::LeftShift);
    }
    if modifiers.right_shift {
        keys.push(Key::RightShift);
    }
    if modifiers.left_ctrl {
        keys.push(Key::LeftControl);
    }
    if modifiers.right_ctrl {
        keys.push(Key::RightControl);
    }
    if modifiers.left_alt {
        keys.push(Key::LeftAlt);
    }
    if modifiers.right_alt {
        keys.push(Key::RightAlt);
    }
    if modifiers.left_meta {
        keys.push(Key::LeftMeta);
    }
    if modifiers.right_meta {
        keys.push(Key::RightMeta);
    }
    keys
}

fn release_uinput_keys_best_effort(
    device: &mut uinput::Device,
    keys: &[uinput::event::keyboard::Key],
) {
    for key in keys.iter().rev() {
        let _ = device.release(key);
    }
    let _ = device.synchronize();
}

trait UinputShortcutSink {
    fn release_shortcut_key(
        &mut self,
        key: &uinput::event::keyboard::Key,
    ) -> Result<(), SwitcherError>;
    fn synchronize_shortcut_keys(&mut self) -> Result<(), SwitcherError>;
}

impl UinputShortcutSink for uinput::Device {
    fn release_shortcut_key(
        &mut self,
        key: &uinput::event::keyboard::Key,
    ) -> Result<(), SwitcherError> {
        self.release(key).map_err(Into::into)
    }

    fn synchronize_shortcut_keys(&mut self) -> Result<(), SwitcherError> {
        self.synchronize().map_err(Into::into)
    }
}

fn release_uinput_modifiers_exhaustively(
    device: &mut dyn UinputShortcutSink,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    let pressed = pressed_uinput_modifier_keys(modifiers);
    let pressed = pressed.iter().collect::<Vec<_>>();
    release_shortcut_keys(device, None, &pressed)
}

fn run_shortcut(
    device: &mut uinput::Device,
    modifiers: ModifierState,
    shortcut_modifiers: &[uinput::event::keyboard::Key],
    trigger_key: Option<&uinput::event::keyboard::Key>,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    release_modifiers(device, modifiers, control)?;

    let mut pressed_shortcut_modifiers = Vec::new();
    for modifier in shortcut_modifiers {
        if let Err(error) = authorize_transaction_mutation(control) {
            release_shortcut_keys_best_effort(device, None, &pressed_shortcut_modifiers);
            return Err(error);
        }
        if let Err(error) = device.press(modifier) {
            release_shortcut_keys_best_effort(device, None, &pressed_shortcut_modifiers);
            return Err(error.into());
        }
        pressed_shortcut_modifiers.push(modifier);
    }

    let mut trigger_pressed = None;
    if let Some(key) = trigger_key {
        if let Err(error) = authorize_transaction_mutation(control) {
            release_shortcut_keys_best_effort(device, None, &pressed_shortcut_modifiers);
            return Err(error);
        }
        if let Err(error) = device.press(key) {
            release_shortcut_keys_best_effort(device, None, &pressed_shortcut_modifiers);
            return Err(error.into());
        }
        trigger_pressed = Some(key);
    }

    if let Err(error) = device.synchronize() {
        release_shortcut_keys_best_effort(device, trigger_pressed, &pressed_shortcut_modifiers);
        return Err(error.into());
    }
    let wait_result = sleep_for_transaction(control, Duration::from_millis(LAYOUT_SWITCH_DELAY_MS));
    let cleanup_result =
        release_shortcut_keys(device, trigger_pressed, &pressed_shortcut_modifiers);
    wait_result?;
    cleanup_result?;
    restore_modifiers(device, modifiers, control)?;
    Ok(())
}

fn release_shortcut_keys(
    device: &mut dyn UinputShortcutSink,
    trigger_pressed: Option<&uinput::event::keyboard::Key>,
    pressed_shortcut_modifiers: &[&uinput::event::keyboard::Key],
) -> Result<(), SwitcherError> {
    let mut first_error = None;
    if let Some(key) = trigger_pressed {
        if let Err(error) = device.release_shortcut_key(key) {
            first_error = Some(error);
        }
    }
    for modifier in pressed_shortcut_modifiers.iter().rev() {
        if let Err(error) = device.release_shortcut_key(modifier) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Err(error) = device.synchronize_shortcut_keys() {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn release_shortcut_keys_best_effort(
    device: &mut dyn UinputShortcutSink,
    trigger_pressed: Option<&uinput::event::keyboard::Key>,
    pressed_shortcut_modifiers: &[&uinput::event::keyboard::Key],
) {
    let _ = release_shortcut_keys(device, trigger_pressed, pressed_shortcut_modifiers);
}

use crate::daemon::layout_switcher::{
    LayoutSwitchHooks, LayoutSwitchStrategy, LayoutSwitcher, UinputLayoutSwitcher,
    X11LayoutSwitcher,
};

fn replay_shift_for_stroke(
    stroke: &crate::daemon::switch_logic::Keystroke,
    caps_lock: bool,
) -> bool {
    if is_case_sensitive_letter_key(stroke.key) {
        stroke.shift ^ caps_lock
    } else {
        stroke.shift
    }
}

fn is_case_sensitive_letter_key(key: Key) -> bool {
    matches!(
        key,
        Key::KEY_A
            | Key::KEY_B
            | Key::KEY_C
            | Key::KEY_D
            | Key::KEY_E
            | Key::KEY_F
            | Key::KEY_G
            | Key::KEY_H
            | Key::KEY_I
            | Key::KEY_J
            | Key::KEY_K
            | Key::KEY_L
            | Key::KEY_M
            | Key::KEY_N
            | Key::KEY_O
            | Key::KEY_P
            | Key::KEY_Q
            | Key::KEY_R
            | Key::KEY_S
            | Key::KEY_T
            | Key::KEY_U
            | Key::KEY_V
            | Key::KEY_W
            | Key::KEY_X
            | Key::KEY_Y
            | Key::KEY_Z
            | Key::KEY_GRAVE
            | Key::KEY_LEFTBRACE
            | Key::KEY_RIGHTBRACE
            | Key::KEY_SEMICOLON
            | Key::KEY_APOSTROPHE
            | Key::KEY_COMMA
            | Key::KEY_DOT
    )
}

trait CinnamonX11XtestReplay {
    fn prepare_for_layout_correction(
        &mut self,
        plan: &CorrectionPlan,
        control: Option<&WriterTransactionControl>,
    ) -> Result<(), SwitcherError>;
    fn key_down(
        &mut self,
        key: Key,
        control: Option<&WriterTransactionControl>,
    ) -> Result<(), SwitcherError>;
    fn key_up(&mut self, key: Key) -> Result<(), SwitcherError>;
}

enum CinnamonX11XtestRuntime {
    NotSelected,
    Unavailable(String),
    Available(Box<CinnamonX11XtestReplayer>),
}

struct CinnamonX11XtestReplayer {
    x11: x11rb::rust_connection::RustConnection,
    root: u32,
}

fn validate_cinnamon_plan_keycodes_with(
    plan: &CorrectionPlan,
    control: Option<&WriterTransactionControl>,
    mut validate: impl FnMut(Key) -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    let required_keys = [Key::KEY_BACKSPACE, Key::KEY_LEFTSHIFT, Key::KEY_SPACE];
    for key in required_keys
        .into_iter()
        .chain(plan.buffer.iter().map(|stroke| stroke.key))
    {
        ensure_transaction_active(control)?;
        let result = validate(key);
        ensure_transaction_active(control)?;
        result?;
    }
    Ok(())
}

fn run_checked_external_call<T>(
    control: Option<&WriterTransactionControl>,
    call: impl FnOnce() -> Result<T, SwitcherError>,
) -> Result<T, SwitcherError> {
    ensure_transaction_active(control)?;
    let result = call();
    ensure_transaction_active(control)?;
    result
}

fn cinnamon_xkb_target_group(num_groups: u8, current_group: u8) -> Result<u8, SwitcherError> {
    if num_groups != 2 {
        return Err(SwitcherError::Io(io::Error::other(format!(
            "cinnamon-xkb-xtest-before-mutation: expected exactly two XKB groups, found {num_groups}",
        ))));
    }
    if current_group >= num_groups {
        return Err(SwitcherError::Io(io::Error::other(format!(
            "cinnamon-xkb-xtest-before-mutation: current XKB group {current_group} is outside configured groups {num_groups}",
        ))));
    }
    Ok((current_group + 1) % num_groups)
}

impl CinnamonX11XtestReplayer {
    fn new() -> Result<Self, SwitcherError> {
        let (x11, screen_num) = x11rb::connect(None)
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?;
        use x11rb::connection::Connection as _;

        use x11rb::protocol::xkb::ConnectionExt as _;
        x11.xkb_use_extension(1, 0)
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?
            .reply()
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?;

        use x11rb::protocol::xtest::ConnectionExt as _;
        x11.xtest_get_version(2, 2)
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?
            .reply()
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?;

        let root = x11.setup().roots[screen_num].root;
        Ok(Self { x11, root })
    }

    fn num_xkb_groups(
        &self,
        control: Option<&WriterTransactionControl>,
    ) -> Result<u8, SwitcherError> {
        use x11rb::protocol::xkb::{self, ConnectionExt as _};
        let controls = run_checked_external_call(control, || {
            self.x11
                .xkb_get_controls(xkb::ID::USE_CORE_KBD.into())
                .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?
                .reply()
                .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))
        })?;
        Ok(controls.num_groups)
    }

    fn target_xkb_group(
        &self,
        control: Option<&WriterTransactionControl>,
    ) -> Result<u8, SwitcherError> {
        let num_groups = self.num_xkb_groups(control)?;
        let current_group = self.xkb_group(control)?;
        cinnamon_xkb_target_group(num_groups, current_group)
    }

    fn activate_and_verify_group(
        &self,
        target_group: u8,
        control: Option<&WriterTransactionControl>,
    ) -> Result<(), SwitcherError> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xkb::{self, ConnectionExt as _};

        authorize_transaction_mutation(control)?;
        let activation_result = self
            .x11
            .xkb_latch_lock_state(
                xkb::ID::USE_CORE_KBD.into(),
                0u8.into(),
                0u8.into(),
                true,
                target_group.into(),
                0u8.into(),
                false,
                0,
            )
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))
            .and_then(|_| {
                self.x11
                    .flush()
                    .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))
            });
        ensure_transaction_active(control)?;
        activation_result?;
        let started = Instant::now();
        let mut last_xkb_group = None;

        while started.elapsed() < CINNAMON_XKB_SWITCH_TIMEOUT {
            ensure_transaction_active(control)?;
            let xkb_group = self.xkb_group(control)?;
            ensure_transaction_active(control)?;
            last_xkb_group = Some(xkb_group);
            if xkb_group == target_group {
                return Ok(());
            }
            sleep_for_transaction(control, CINNAMON_XKB_SWITCH_POLL_INTERVAL)?;
        }

        Err(SwitcherError::Io(io::Error::other(format!(
            "cinnamon-xkb-xtest-before-mutation: activation did not settle target_group={target_group} xkb_group={:?}",
            last_xkb_group,
        ))))
    }

    fn xkb_group(&self, control: Option<&WriterTransactionControl>) -> Result<u8, SwitcherError> {
        use x11rb::protocol::xkb::{self, ConnectionExt as _};
        let state = run_checked_external_call(control, || {
            self.x11
                .xkb_get_state(xkb::ID::USE_CORE_KBD.into())
                .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?
                .reply()
                .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))
        })?;

        Ok(u8::from(state.group))
    }

    fn validate_plan_keycodes(
        &self,
        plan: &CorrectionPlan,
        control: Option<&WriterTransactionControl>,
    ) -> Result<(), SwitcherError> {
        validate_cinnamon_plan_keycodes_with(plan, control, |key| {
            self.validate_keycode(key, control).map(|_| ())
        })
    }

    fn validate_keycode(
        &self,
        key: Key,
        control: Option<&WriterTransactionControl>,
    ) -> Result<u8, SwitcherError> {
        let keycode = evdev_key_to_x11_keycode(key)?;
        use x11rb::connection::Connection as _;
        let setup = self.x11.setup();
        if keycode < setup.min_keycode || keycode > setup.max_keycode {
            return Err(SwitcherError::Io(io::Error::other(format!(
                "cinnamon-xkb-xtest-before-mutation: keycode out of X11 range key={key:?} keycode={keycode} min={} max={}",
                setup.min_keycode, setup.max_keycode
            ))));
        }

        use x11rb::protocol::xproto::ConnectionExt as _;
        let mapping = run_checked_external_call(control, || {
            self.x11
                .get_keyboard_mapping(keycode, 1)
                .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?
                .reply()
                .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))
        })?;
        if mapping.keysyms.iter().all(|keysym| *keysym == 0) {
            return Err(SwitcherError::Io(io::Error::other(format!(
                "cinnamon-xkb-xtest-before-mutation: keycode has empty mapping key={key:?} keycode={keycode}",
            ))));
        }

        Ok(keycode)
    }

    fn emit_fake_key(&self, keycode: u8, pressed: bool) -> Result<(), SwitcherError> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto;
        use x11rb::protocol::xtest::ConnectionExt as _;
        self.x11
            .xtest_fake_input(
                if pressed {
                    xproto::KEY_PRESS_EVENT
                } else {
                    xproto::KEY_RELEASE_EVENT
                },
                keycode,
                x11rb::CURRENT_TIME,
                self.root,
                0,
                0,
                0,
            )
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?
            .check()
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?;
        self.x11
            .flush()
            .map_err(|error| SwitcherError::Io(io::Error::other(error.to_string())))?;
        Ok(())
    }

    fn fake_key(
        &self,
        key: Key,
        pressed: bool,
        control: Option<&WriterTransactionControl>,
        failure_request_id: Option<&AtomicU64>,
        stop_requested: Option<&AtomicBool>,
        terminal_gate: Option<&Mutex<()>>,
    ) -> Result<(), SwitcherError> {
        validate_and_emit_xtest_key(
            key,
            control,
            failure_request_id,
            stop_requested,
            terminal_gate,
            |key| self.validate_keycode(key, control),
            |keycode| self.emit_fake_key(keycode, pressed),
        )
    }

    fn type_key(
        &mut self,
        key: Key,
        failure_request_id: &AtomicU64,
        stop_requested: &AtomicBool,
        terminal_gate: &Mutex<()>,
    ) -> Result<(), SwitcherError> {
        let key_down_result = self.fake_key(
            key,
            true,
            None,
            Some(failure_request_id),
            Some(stop_requested),
            Some(terminal_gate),
        );
        finish_fast_xtest_tap_attempt(key_down_result, || self.key_up(key))
    }
}

fn validate_and_emit_xtest_key(
    key: Key,
    control: Option<&WriterTransactionControl>,
    failure_request_id: Option<&AtomicU64>,
    stop_requested: Option<&AtomicBool>,
    terminal_gate: Option<&Mutex<()>>,
    validate: impl FnOnce(Key) -> Result<u8, SwitcherError>,
    emit: impl FnOnce(u8) -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    ensure_transaction_active(control)?;
    if let Some(failure_request_id) = failure_request_id {
        ensure_writer_not_failed(failure_request_id)?;
    }
    let keycode = validate(key);
    ensure_transaction_active(control)?;
    if let Some(failure_request_id) = failure_request_id {
        ensure_writer_not_failed(failure_request_id)?;
    }
    let keycode = keycode?;
    match (control, failure_request_id, stop_requested, terminal_gate) {
        (Some(control), _, _, _) => control.authorize_mutation_start()?,
        (None, Some(failure_request_id), Some(stop_requested), Some(terminal_gate)) => {
            authorize_writer_mutation_start(failure_request_id, stop_requested, terminal_gate)?;
        }
        (None, Some(failure_request_id), _, None) => {
            ensure_writer_not_failed(failure_request_id)?;
        }
        (None, None, _, _) => {}
        (None, Some(_), None, Some(_)) => {
            return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
        }
    }
    emit(keycode)
}

impl CinnamonX11XtestReplay for CinnamonX11XtestReplayer {
    fn prepare_for_layout_correction(
        &mut self,
        plan: &CorrectionPlan,
        control: Option<&WriterTransactionControl>,
    ) -> Result<(), SwitcherError> {
        ensure_transaction_active(control)?;
        self.validate_plan_keycodes(plan, control)?;
        ensure_transaction_active(control)?;
        let target_group = self.target_xkb_group(control)?;
        ensure_transaction_active(control)?;
        self.activate_and_verify_group(target_group, control)
    }

    fn key_down(
        &mut self,
        key: Key,
        control: Option<&WriterTransactionControl>,
    ) -> Result<(), SwitcherError> {
        self.fake_key(key, true, control, None, None, None)
    }

    fn key_up(&mut self, key: Key) -> Result<(), SwitcherError> {
        self.fake_key(key, false, None, None, None, None)
    }
}

fn evdev_key_to_x11_keycode(key: Key) -> Result<u8, SwitcherError> {
    let raw = key.code() + X11_EVDEV_KEYCODE_OFFSET;
    u8::try_from(raw).map_err(|_| {
        SwitcherError::Io(io::Error::other(format!(
            "cinnamon-xkb-xtest-before-mutation: evdev keycode cannot fit in X11 keycode key={key:?} raw={raw}",
        )))
    })
}

fn select_correction_replay_strategy(
    context: SystemContext,
    switch_layout: bool,
    cinnamon_xtest_available: bool,
) -> CorrectionReplayStrategy {
    if !switch_layout {
        return CorrectionReplayStrategy::Generic;
    }

    if context.session_type == SessionType::X11
        && context.desktop_environment == DesktopEnvironment::Cinnamon
    {
        if cinnamon_xtest_available {
            CorrectionReplayStrategy::CinnamonXkbXtest
        } else {
            CorrectionReplayStrategy::CinnamonXkbXtestUnavailable
        }
    } else {
        CorrectionReplayStrategy::Generic
    }
}

fn release_modifiers_xtest(
    replay: &mut dyn CinnamonX11XtestReplay,
    modifiers: ModifierState,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    authorize_transaction_mutation(control)?;
    let cleanup_result = release_xtest_modifiers_exhaustively(replay, modifiers);
    let active_result = ensure_transaction_active(control);
    cleanup_result?;
    active_result
}

fn release_xtest_modifiers_exhaustively(
    replay: &mut dyn CinnamonX11XtestReplay,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    let mut first_error = None;
    for key in pressed_modifier_keys(modifiers) {
        if let Err(error) = replay.key_up(key) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn restore_modifiers_xtest(
    replay: &mut dyn CinnamonX11XtestReplay,
    modifiers: ModifierState,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    let mut restored = Vec::new();
    for key in pressed_modifier_keys(modifiers) {
        if let Err(error) = ensure_transaction_active(control) {
            release_xtest_keys_best_effort(replay, &restored);
            return Err(error);
        }
        if let Err(error) = xtest_key_down_exception_safe(replay, key, control) {
            release_xtest_keys_best_effort(replay, &restored);
            return Err(error);
        }
        restored.push(key);
    }
    if let Err(error) = ensure_transaction_active(control) {
        release_xtest_keys_best_effort(replay, &restored);
        return Err(error);
    }
    Ok(())
}

fn xtest_key_down_exception_safe(
    replay: &mut dyn CinnamonX11XtestReplay,
    key: Key,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    match replay.key_down(key, control) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = replay.key_up(key);
            Err(error)
        }
    }
}

fn release_xtest_keys_best_effort(replay: &mut dyn CinnamonX11XtestReplay, keys: &[Key]) {
    for key in keys.iter().rev() {
        let _ = replay.key_up(*key);
    }
}

fn pressed_modifier_keys(modifiers: ModifierState) -> Vec<Key> {
    let mut keys = Vec::new();
    if modifiers.left_ctrl {
        keys.push(Key::KEY_LEFTCTRL);
    }
    if modifiers.right_ctrl {
        keys.push(Key::KEY_RIGHTCTRL);
    }
    if modifiers.left_shift {
        keys.push(Key::KEY_LEFTSHIFT);
    }
    if modifiers.right_shift {
        keys.push(Key::KEY_RIGHTSHIFT);
    }
    if modifiers.left_alt {
        keys.push(Key::KEY_LEFTALT);
    }
    if modifiers.right_alt {
        keys.push(Key::KEY_RIGHTALT);
    }
    if modifiers.left_meta {
        keys.push(Key::KEY_LEFTMETA);
    }
    if modifiers.right_meta {
        keys.push(Key::KEY_RIGHTMETA);
    }
    keys
}

fn xtest_key_tap(
    replay: &mut dyn CinnamonX11XtestReplay,
    key: Key,
    delay: Duration,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    ensure_transaction_active(control)?;
    let key_down_result = replay.key_down(key, control);
    complete_xtest_tap_attempt(
        key_down_result,
        || sleep_for_transaction(control, Duration::from_millis(2)),
        || replay.key_up(key),
        || sleep_for_transaction(control, delay),
    )
}

fn complete_xtest_tap_attempt(
    key_down_result: Result<(), SwitcherError>,
    transition_wait: impl FnOnce() -> Result<(), SwitcherError>,
    key_up: impl FnOnce() -> Result<(), SwitcherError>,
    final_wait: impl FnOnce() -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    let transition_result = transition_wait();
    let key_up_result = key_up();
    key_down_result?;
    transition_result?;
    key_up_result?;
    final_wait()
}

fn finish_fast_xtest_tap_attempt(
    key_down_result: Result<(), SwitcherError>,
    key_up: impl FnOnce() -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    complete_xtest_tap_attempt(
        key_down_result,
        || {
            thread::sleep(Duration::from_millis(2));
            Ok(())
        },
        key_up,
        || Ok(()),
    )
}

fn replay_xtest_stroke(
    replay: &mut dyn CinnamonX11XtestReplay,
    key: Key,
    effective_shift: bool,
    typing_delay: Duration,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    if !effective_shift {
        return xtest_key_tap(replay, key, typing_delay, control);
    }

    ensure_transaction_active(control)?;
    xtest_key_down_exception_safe(replay, Key::KEY_LEFTSHIFT, control)?;
    if let Err(error) = sleep_for_transaction(control, Duration::from_millis(1)) {
        let _ = replay.key_up(Key::KEY_LEFTSHIFT);
        return Err(error);
    }
    let tap_result = xtest_key_tap(replay, key, typing_delay, control);
    let shift_release_result = replay.key_up(Key::KEY_LEFTSHIFT);
    tap_result?;
    shift_release_result
}

fn run_cinnamon_x11_xtest_correction(
    replay: &mut dyn CinnamonX11XtestReplay,
    plan: &CorrectionPlan,
    config: &RuntimeConfigSnapshot,
    modifiers: ModifierState,
    control: Option<&WriterTransactionControl>,
) -> Result<CorrectionExecutionOutcome, SwitcherError> {
    ensure_transaction_active(control)?;
    replay.prepare_for_layout_correction(plan, control)?;
    ensure_transaction_active(control)?;
    release_modifiers_xtest(replay, modifiers, control)?;

    for _ in 0..(plan.buffer.len() + plan.extra_backspaces) {
        xtest_key_tap(
            replay,
            Key::KEY_BACKSPACE,
            Duration::from_millis(config.backspace_ms),
            control,
        )?;
    }

    for stroke in &plan.buffer {
        let effective_shift = replay_shift_for_stroke(stroke, modifiers.is_caps_lock_active());
        replay_xtest_stroke(
            replay,
            stroke.key,
            effective_shift,
            Duration::from_millis(config.typing_ms),
            control,
        )?;
    }

    restore_modifiers_xtest(replay, modifiers, control)?;
    Ok(CorrectionExecutionOutcome {
        layout_switch: CorrectionLayoutSwitchOutcome::AppliedCinnamonXkbXtest,
    })
}

fn detect_current_system_context() -> SystemContext {
    SystemContextDetector::detect_current().unwrap_or(SystemContext {
        session_type: SessionType::Unknown,
        desktop_environment: DesktopEnvironment::Unknown,
        distro: DistroKind::Unknown,
    })
}

fn initialize_cinnamon_x11_xtest_runtime(context: SystemContext) -> CinnamonX11XtestRuntime {
    if select_correction_replay_strategy(context, true, true)
        != CorrectionReplayStrategy::CinnamonXkbXtest
    {
        return CinnamonX11XtestRuntime::NotSelected;
    }

    match CinnamonX11XtestReplayer::new() {
        Ok(replayer) => {
            log_input_debug(
                "correction-replay-strategy",
                "session_type=x11 desktop=cinnamon strategy=cinnamon-xkb-xtest result=ready",
            );
            CinnamonX11XtestRuntime::Available(Box::new(replayer))
        }
        Err(error) => {
            log_input_debug(
                "correction-replay-strategy",
                &format!(
                    "session_type=x11 desktop=cinnamon strategy=cinnamon-xkb-xtest result=unavailable error={error}"
                ),
            );
            CinnamonX11XtestRuntime::Unavailable(error.to_string())
        }
    }
}

trait UinputStrokeSink {
    fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError>;
    fn synchronize_keys(&mut self) -> Result<(), SwitcherError>;
}

impl UinputStrokeSink for uinput::Device {
    fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)
            .map_err(Into::into)
    }

    fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
        self.synchronize().map_err(Into::into)
    }
}

fn release_uinput_stroke_keys(
    sink: &mut dyn UinputStrokeSink,
    pressed: &[Key],
) -> Result<(), SwitcherError> {
    let mut first_error = None;
    for key in pressed.iter().rev() {
        if let Err(error) = sink.write_key(*key, 0) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Err(error) = sink.synchronize_keys() {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn replay_uinput_stroke(
    sink: &mut dyn UinputStrokeSink,
    key: Key,
    effective_shift: bool,
    typing_delay: Duration,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    let mut pressed = Vec::with_capacity(2);

    if effective_shift {
        authorize_transaction_mutation(control)?;
        sink.write_key(Key::KEY_LEFTSHIFT, 1)?;
        pressed.push(Key::KEY_LEFTSHIFT);
        if let Err(error) = sink.synchronize_keys() {
            let _ = release_uinput_stroke_keys(sink, &pressed);
            return Err(error);
        }
        if let Err(error) = sleep_for_transaction(control, Duration::from_millis(1)) {
            let _ = release_uinput_stroke_keys(sink, &pressed);
            return Err(error);
        }
    }

    if let Err(error) = authorize_transaction_mutation(control) {
        let _ = release_uinput_stroke_keys(sink, &pressed);
        return Err(error);
    }
    if let Err(error) = sink.write_key(key, 1) {
        let _ = release_uinput_stroke_keys(sink, &pressed);
        return Err(error);
    }
    pressed.push(key);
    if let Err(error) = sink.synchronize_keys() {
        let _ = release_uinput_stroke_keys(sink, &pressed);
        return Err(error);
    }

    let transition_wait = sleep_for_transaction(control, Duration::from_millis(2));
    let release_result = release_uinput_stroke_keys(sink, &pressed);
    transition_wait?;
    release_result?;
    sleep_for_transaction(control, typing_delay)
}

fn replay_uinput_backspaces(
    sink: &mut dyn UinputStrokeSink,
    count: usize,
    backspace_delay: Duration,
    control: Option<&WriterTransactionControl>,
) -> Result<(), SwitcherError> {
    for _ in 0..count {
        replay_uinput_stroke(sink, Key::KEY_BACKSPACE, false, backspace_delay, control)?;
    }
    Ok(())
}

fn run_correction(
    device: &mut uinput::Device,
    plan: &CorrectionPlan,
    config: &RuntimeConfigSnapshot,
    modifiers: ModifierState,
    x11_switcher: &mut Option<X11LayoutSwitcher>,
    cinnamon_x11_xtest: &mut CinnamonX11XtestRuntime,
    switch_layout: bool,
    control: Option<&WriterTransactionControl>,
) -> Result<CorrectionExecutionOutcome, SwitcherError> {
    log_input_debug(
        "correction-transaction",
        &format!(
            "switch_layout={} combo={:?} buffer_len={} extra_backspaces={} layout_delay_ms={} typing_ms={} backspace_ms={}",
            switch_layout,
            config.layout_switch_combo,
            plan.buffer.len(),
            plan.extra_backspaces,
            config.layout_delay_ms,
            config.typing_ms,
            config.backspace_ms,
        ),
    );

    ensure_transaction_active(control)?;

    if switch_layout {
        match cinnamon_x11_xtest {
            CinnamonX11XtestRuntime::Available(replay) => {
                log_input_debug(
                    "correction-layout-switch",
                    "strategy=cinnamon-xkb-xtest phase=prepare",
                );
                let outcome = run_cinnamon_x11_xtest_correction(
                    replay.as_mut(),
                    plan,
                    config,
                    modifiers,
                    control,
                )?;
                log_input_debug(
                    "correction-layout-switch",
                    "strategy=cinnamon-xkb-xtest result=ok",
                );
                return Ok(outcome);
            }
            CinnamonX11XtestRuntime::Unavailable(reason) => {
                log_input_debug(
                    "correction-layout-switch",
                    &format!("strategy=cinnamon-xkb-xtest result=error reason={reason}"),
                );
                return Err(SwitcherError::Io(io::Error::other(format!(
                    "cinnamon-xkb-xtest-before-mutation: {reason}"
                ))));
            }
            CinnamonX11XtestRuntime::NotSelected => {}
        }
    }

    release_modifiers(device, modifiers, control)?;
    replay_uinput_backspaces(
        device,
        plan.buffer.len() + plan.extra_backspaces,
        Duration::from_millis(config.backspace_ms),
        control,
    )?;

    let layout_switch = if switch_layout {
        if x11_switcher.is_some() {
            log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=x11 hold_ms={}",
                    config.layout_switch_combo, config.layout_delay_ms
                ),
            );
        } else {
            log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=uinput hold_ms={}",
                    config.layout_switch_combo, config.layout_delay_ms
                ),
            );
        }

        let layout_waiter = |duration: Duration| {
            if duration.is_zero() {
                authorize_transaction_mutation(control)
            } else {
                sleep_for_transaction(control, duration)
            }
        };
        let mut uinput_switcher =
            UinputLayoutSwitcher::new_with_waiter(device, config.layout_delay_ms, &layout_waiter);
        let x11 = x11_switcher
            .as_mut()
            .map(|switcher| switcher as &mut dyn LayoutSwitcher);
        ensure_transaction_active(control)?;
        let outcome = switch_layout_with_fallback(
            x11,
            &mut uinput_switcher,
            config.layout_switch_combo,
            control,
        )?;
        ensure_transaction_active(control)?;
        match outcome {
            CorrectionLayoutSwitchOutcome::AppliedX11 => log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=x11 result=ok",
                    config.layout_switch_combo
                ),
            ),
            CorrectionLayoutSwitchOutcome::AppliedUinput => log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=uinput result=ok hold_ms={}",
                    config.layout_switch_combo, config.layout_delay_ms
                ),
            ),
            CorrectionLayoutSwitchOutcome::AppliedCinnamonXkbXtest => log_input_debug(
                "correction-layout-switch",
                "strategy=cinnamon-xkb-xtest result=ok",
            ),
            CorrectionLayoutSwitchOutcome::NotNeeded => {}
        }
        sleep_for_transaction(control, Duration::from_millis(config.layout_delay_ms))?;
        outcome
    } else {
        CorrectionLayoutSwitchOutcome::NotNeeded
    };

    for stroke in &plan.buffer {
        let effective_shift = replay_shift_for_stroke(stroke, modifiers.is_caps_lock_active());
        replay_uinput_stroke(
            device,
            stroke.key,
            effective_shift,
            Duration::from_millis(config.typing_ms),
            control,
        )?;
    }

    restore_modifiers(device, modifiers, control)?;
    Ok(CorrectionExecutionOutcome { layout_switch })
}

fn switch_layout_with_fallback(
    x11_switcher: Option<&mut dyn LayoutSwitcher>,
    uinput_switcher: &mut dyn LayoutSwitcher,
    combo: LayoutSwitchCombo,
    control: Option<&WriterTransactionControl>,
) -> Result<CorrectionLayoutSwitchOutcome, SwitcherError> {
    ensure_transaction_active(control)?;
    let checkpoint = || ensure_transaction_active(control);
    let authorize_mutation = || authorize_transaction_mutation(control);
    let hooks = LayoutSwitchHooks::new(&checkpoint, &authorize_mutation);
    if let Some(switcher) = x11_switcher {
        if let Err(e) = switcher.switch_layout_with_hooks(combo, &hooks) {
            log_input_debug("x11-layout-switcher", &format!("failed: {}", e));
            if hooks.mutation_was_authorized() {
                log_input_debug(
                    "correction-layout-switch",
                    &format!(
                        "combo={:?} strategy=x11 result=error fallback=disabled reason=mutation-authorized",
                        combo
                    ),
                );
                return Err(e);
            }
            log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=x11 result=error fallback=uinput",
                    combo
                ),
            );
            ensure_transaction_active(control)?;
            uinput_switcher.switch_layout_with_hooks(combo, &hooks)?;
            return Ok(CorrectionLayoutSwitchOutcome::AppliedUinput);
        }

        return Ok(CorrectionLayoutSwitchOutcome::AppliedX11);
    }

    ensure_transaction_active(control)?;
    uinput_switcher.switch_layout_with_hooks(combo, &hooks)?;
    Ok(CorrectionLayoutSwitchOutcome::AppliedUinput)
}

// Virtual keyboard writer loop

fn ensure_writer_not_failed(failure_request_id: &AtomicU64) -> Result<(), SwitcherError> {
    let request_id = failure_request_id.load(Ordering::SeqCst);
    if request_id == 0 {
        Ok(())
    } else {
        Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id })
    }
}

fn authorize_writer_mutation_start(
    failure_request_id: &AtomicU64,
    stop_requested: &AtomicBool,
    terminal_gate: &Mutex<()>,
) -> Result<(), SwitcherError> {
    ensure_writer_running(failure_request_id, stop_requested, terminal_gate)
}

fn ensure_writer_running(
    failure_request_id: &AtomicU64,
    stop_requested: &AtomicBool,
    terminal_gate: &Mutex<()>,
) -> Result<(), SwitcherError> {
    let _terminal_guard = terminal_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if stop_requested.load(Ordering::SeqCst) {
        return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
    }
    ensure_writer_not_failed(failure_request_id)
}

fn publish_writer_transaction_result(
    control: &WriterTransactionControl,
    reply: mpsc::Sender<Result<CorrectionExecutionOutcome, SwitcherError>>,
    result: Result<CorrectionExecutionOutcome, SwitcherError>,
) -> Result<(), SwitcherError> {
    let failure_reason = result.as_ref().err().map(ToString::to_string);
    match control.publish_completed_with(|| reply.send(result).is_ok()) {
        WriterCompletionPublication::Completed => match failure_reason {
            Some(reason) => Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: control.request_id(),
                reason,
            }),
            None => Ok(()),
        },
        WriterCompletionPublication::Cancelled => Err(control.cancellation_error()),
        WriterCompletionPublication::ReceiverDisconnected => {
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        }
    }
}

fn publish_deferred_manual_completion(
    control: &WriterTransactionControl,
    completion_tx: &mpsc::Sender<ManualCurrentWordCompletion>,
    completion: ManualCurrentWordCompletion,
    failure_reason: Option<String>,
) -> Result<(), SwitcherError> {
    match control.publish_completed_with(|| completion_tx.send(completion).is_ok()) {
        WriterCompletionPublication::Completed => match failure_reason {
            Some(reason) => Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: control.request_id(),
                reason,
            }),
            None => Ok(()),
        },
        WriterCompletionPublication::Cancelled => Err(control.cancellation_error()),
        WriterCompletionPublication::ReceiverDisconnected => {
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        }
    }
}

#[cfg(test)]
fn run_writer_command_loop_with(
    command_rx: mpsc::Receiver<WriterCommand>,
    failure_request_id: &AtomicU64,
    mut dispatch: impl FnMut(WriterCommand) -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    for command in command_rx {
        if matches!(&command, WriterCommand::Shutdown) {
            break;
        }
        ensure_writer_not_failed(failure_request_id)?;
        dispatch(command)?;
    }
    Ok(())
}

fn run_writer_command_loop_with_stop(
    command_rx: mpsc::Receiver<WriterCommand>,
    failure_request_id: &AtomicU64,
    stop_requested: &AtomicBool,
    terminal_gate: &Mutex<()>,
    on_ready: impl FnOnce() -> Result<(), SwitcherError>,
    mut dispatch: impl FnMut(WriterCommand) -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    ensure_writer_running(failure_request_id, stop_requested, terminal_gate)?;
    on_ready()?;
    for command in command_rx {
        if matches!(&command, WriterCommand::Shutdown) {
            break;
        }
        ensure_writer_running(failure_request_id, stop_requested, terminal_gate)?;
        dispatch(command)?;
    }
    Ok(())
}

fn finish_fast_separator_replay(
    xtest_attempt: Option<Result<(), SwitcherError>>,
    uinput_fallback: impl FnOnce() -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    match xtest_attempt {
        Some(result) => result,
        None => uinput_fallback(),
    }
}

fn run_virtual_keyboard_writer_loop(
    mut device: uinput::Device,
    command_rx: mpsc::Receiver<WriterCommand>,
    completion_tx: mpsc::Sender<ManualCurrentWordCompletion>,
    failure_request_id: Arc<AtomicU64>,
    stop_requested: Arc<AtomicBool>,
    terminal_gate: Arc<Mutex<()>>,
    writer_alive: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<()>,
) -> Result<(), SwitcherError> {
    let context = detect_current_system_context();
    let mut x11_switcher =
        initialize_x11_switcher_for_session(context.session_type, X11LayoutSwitcher::new);
    let mut cinnamon_x11_xtest = initialize_cinnamon_x11_xtest_runtime(context);

    run_writer_command_loop_with_stop(
        command_rx,
        &failure_request_id,
        &stop_requested,
        &terminal_gate,
        || publish_writer_ready(&writer_alive, ready_tx),
        |command| {
            match command {
                WriterCommand::Shutdown => unreachable!("shutdown handled before circuit breaker"),
                WriterCommand::Fast(command) => match command {
                    WriterFastCommand::ForwardEvent { key, value } => {
                        authorize_writer_mutation_start(
                            &failure_request_id,
                            &stop_requested,
                            &terminal_gate,
                        )?;
                        device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)?;
                        device.synchronize()?;
                    }
                    WriterFastCommand::TypeSeparator { key } => {
                        log_input_debug("type-separator-execute", &format!("key={key:?}"));
                        let xtest_attempt = match &mut cinnamon_x11_xtest {
                            CinnamonX11XtestRuntime::Available(replay) => {
                                let result = replay.type_key(
                                    key,
                                    &failure_request_id,
                                    &stop_requested,
                                    &terminal_gate,
                                );
                                match &result {
                                    Ok(()) => {
                                        log_input_debug(
                                            "type-separator-execute",
                                            &format!(
                                                "key={key:?} strategy=cinnamon-xkb-xtest result=ok"
                                            ),
                                        );
                                    }
                                    Err(error) => {
                                        log_input_debug(
                                    "type-separator-execute",
                                    &format!(
                                        "key={key:?} strategy=cinnamon-xkb-xtest result=error action=fail-stop error={error}"
                                    ),
                                );
                                    }
                                }
                                Some(result)
                            }
                            CinnamonX11XtestRuntime::Unavailable(reason) => {
                                log_input_debug(
                                "type-separator-execute",
                                &format!(
                                    "key={key:?} strategy=cinnamon-xkb-xtest result=unavailable fallback=uinput reason={reason}"
                                    ),
                                );
                                None
                            }
                            CinnamonX11XtestRuntime::NotSelected => None,
                        };
                        finish_fast_separator_replay(xtest_attempt, || {
                            authorize_writer_mutation_start(
                                &failure_request_id,
                                &stop_requested,
                                &terminal_gate,
                            )?;
                            replay_uinput_stroke(&mut device, key, false, Duration::ZERO, None)
                        })?;
                    }
                },
                WriterCommand::DeferredManualCurrentWordCorrection {
                    control,
                    plan,
                    config,
                    modifiers,
                } => {
                    let started = Instant::now();
                    let request_id = control.request_id();
                    let result = control.ensure_active().and_then(|_| {
                        run_correction(
                            &mut device,
                            &plan,
                            &config,
                            modifiers,
                            &mut x11_switcher,
                            &mut cinnamon_x11_xtest,
                            true,
                            Some(&control),
                        )
                    });
                    let result = match result {
                        Ok(outcome) => control.ensure_active().map(|_| outcome),
                        Err(error) => Err(error),
                    };
                    let failure_reason = result.as_ref().err().map(ToString::to_string);
                    let outcome = match result {
                        Ok(_) => ManualCurrentWordOutcome::Succeeded(plan),
                        Err(error) => {
                            log_input_debug(
                                "manual-current-word-writer-error",
                                &format!(
                                    "request_id={} elapsed_ms={} error={error}",
                                    request_id,
                                    started.elapsed().as_millis(),
                                ),
                            );
                            ManualCurrentWordOutcome::FailedAfterMutation(error.to_string())
                        }
                    };
                    publish_deferred_manual_completion(
                        &control,
                        &completion_tx,
                        ManualCurrentWordCompletion {
                            request_id,
                            outcome,
                        },
                        failure_reason,
                    )?;
                }
                WriterCommand::Transaction(transaction) => match transaction {
                    WriterTransaction::Execute {
                        control,
                        kind,
                        reply,
                    } => {
                        let result = control.ensure_active().and_then(|_| match kind {
                            WriterTransactionKind::ApplyCorrection {
                                plan,
                                config,
                                modifiers,
                            } => run_correction(
                                &mut device,
                                &plan,
                                &config,
                                modifiers,
                                &mut x11_switcher,
                                &mut cinnamon_x11_xtest,
                                true,
                                Some(&control),
                            ),
                            WriterTransactionKind::ApplySameLayoutCorrection {
                                plan,
                                config,
                                modifiers,
                            } => run_correction(
                                &mut device,
                                &plan,
                                &config,
                                modifiers,
                                &mut x11_switcher,
                                &mut cinnamon_x11_xtest,
                                false,
                                Some(&control),
                            ),
                            WriterTransactionKind::CopyShortcut { modifiers } => run_shortcut(
                                &mut device,
                                modifiers,
                                &[uinput::event::keyboard::Key::LeftControl],
                                Some(&uinput::event::keyboard::Key::C),
                                Some(&control),
                            )
                            .map(|_| CorrectionExecutionOutcome {
                                layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
                            }),
                            WriterTransactionKind::PasteShortcut { modifiers } => run_shortcut(
                                &mut device,
                                modifiers,
                                &[uinput::event::keyboard::Key::LeftControl],
                                Some(&uinput::event::keyboard::Key::V),
                                Some(&control),
                            )
                            .map(|_| CorrectionExecutionOutcome {
                                layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
                            }),
                        });
                        let result = match result {
                            Ok(outcome) => control.ensure_active().map(|_| outcome),
                            Err(error) => Err(error),
                        };
                        publish_writer_transaction_result(&control, reply, result)?;
                    }
                },
            }
            Ok(())
        },
    )
}

pub(crate) fn log_input_debug(stage: &str, details: &str) {
    let _ = try_debug_line(DebugLogKind::Input, || format_input(stage, details));
}

impl SharedModifierState {
    pub fn store(&self, modifiers: ModifierState) {
        self.bits.store(modifiers.to_bits(), Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> ModifierState {
        ModifierState::from_bits(self.bits.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{default_manual_correction_hotkey, default_selected_text_hotkey};
    use crate::model::{DesktopEnvironment, DistroKind, SystemContext};
    use std::cell::Cell;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    // Test helpers

    fn pressed(keys: &[(Key, i32)]) -> ModifierState {
        let mut state = ModifierState::default();
        for (key, value) in keys {
            state.update(*key, *value);
        }
        state
    }

    fn test_runtime_config_snapshot() -> RuntimeConfigSnapshot {
        RuntimeConfigSnapshot {
            auto_switch_enabled: true,
            fix_two_capitals: true,
            fix_accidental_caps_lock: true,
            layout_switch_combo: LayoutSwitchCombo::AltShift,
            layout_delay_ms: 0,
            backspace_ms: 0,
            typing_ms: 0,
            manual_correction_hotkey: default_manual_correction_hotkey(),
            selected_text_hotkey: default_selected_text_hotkey(),
        }
    }

    fn test_writer_handle(
        capacity: usize,
        alive: bool,
    ) -> (VirtualKeyboardHandle, mpsc::Receiver<WriterCommand>) {
        let (command_tx, command_rx) = mpsc::sync_channel(capacity);
        (
            VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(alive)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            command_rx,
        )
    }

    fn copy_shortcut_transaction() -> WriterTransactionKind {
        WriterTransactionKind::CopyShortcut {
            modifiers: ModifierState::default(),
        }
    }

    fn oversized_correction_transaction() -> WriterTransactionKind {
        WriterTransactionKind::ApplyCorrection {
            plan: CorrectionPlan {
                buffer: vec![
                    crate::daemon::switch_logic::Keystroke {
                        key: Key::KEY_A,
                        shift: false,
                        caps_lock: false,
                    };
                    crate::model::MAX_CORRECTION_KEYSTROKES + 1
                ],
                extra_backspaces: 0,
            },
            config: test_runtime_config_snapshot(),
            modifiers: ModifierState::default(),
        }
    }

    #[test]
    fn writer_ready_is_published_only_when_command_loop_is_entered() {
        let alive = Arc::new(AtomicBool::new(false));
        let worker_alive = Arc::clone(&alive);
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let (initializer_entered_tx, initializer_entered_rx) = mpsc::channel();
        let (allow_initializer_tx, allow_initializer_rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            initializer_entered_tx.send(()).unwrap();
            allow_initializer_rx.recv().unwrap();
            let failure = AtomicU64::new(0);
            let stop_requested = AtomicBool::new(false);
            let terminal_gate = Mutex::new(());
            let (command_tx, command_rx) = mpsc::channel();
            command_tx.send(WriterCommand::Shutdown).unwrap();

            run_writer_command_loop_with_stop(
                command_rx,
                &failure,
                &stop_requested,
                &terminal_gate,
                || {
                    worker_alive.store(true, Ordering::SeqCst);
                    ready_tx
                        .send(())
                        .map_err(|_| SwitcherError::VirtualKeyboardWriterDisconnected)
                },
                |_| unreachable!("shutdown is handled by the command loop"),
            )
        });

        initializer_entered_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initializer must start");
        assert!(matches!(
            ready_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!alive.load(Ordering::SeqCst));

        allow_initializer_tx.send(()).unwrap();
        ready_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer readiness must be published at command-loop entry");
        assert!(alive.load(Ordering::SeqCst));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn writer_startup_wait_is_bounded_when_initializer_never_publishes_ready() {
        let (_ready_tx, ready_rx) = mpsc::sync_channel(1);

        let error = wait_for_writer_startup_ready(&ready_rx, Duration::ZERO).unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterStartupTimedOut { timeout_ms: 0 }
        ));
    }

    #[test]
    fn input_worker_startup_wait_is_bounded_when_worker_never_becomes_ready() {
        let (_ready_tx, ready_rx) = mpsc::sync_channel(1);

        let error =
            wait_for_input_worker_startup_ready(&ready_rx, "pointer-watcher", Duration::ZERO)
                .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkerStartupTimedOut {
                worker: "pointer-watcher",
                timeout_ms: 0
            }
        ));
    }

    #[test]
    fn startup_abort_drops_ready_receiver_before_requesting_worker_stop() {
        let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(0);
        let observed = Cell::new(false);

        abort_input_worker_startup(ready_rx, || {
            observed.set(matches!(
                ready_tx.try_send(()),
                Err(mpsc::TrySendError::Disconnected(()))
            ));
        });

        assert!(observed.get());
    }

    #[test]
    fn input_worker_ready_is_published_at_poll_loop_entry() {
        let alive = Arc::new(AtomicBool::new(false));
        let worker_alive = Arc::clone(&alive);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let poll_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let (setup_entered_tx, setup_entered_rx) = mpsc::channel();
        let (allow_setup_tx, allow_setup_rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            setup_entered_tx.send(()).unwrap();
            allow_setup_rx.recv().unwrap();
            run_input_worker_poll_loop(
                &worker_stop,
                "pointer-watcher",
                || publish_input_worker_ready(&worker_alive, ready_tx, "pointer-watcher"),
                || {
                    poll_stop.store(true, Ordering::SeqCst);
                    true
                },
            )
        });

        setup_entered_rx.recv().unwrap();
        assert!(matches!(
            ready_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!alive.load(Ordering::SeqCst));

        allow_setup_tx.send(()).unwrap();
        ready_rx.recv().unwrap();
        assert!(alive.load(Ordering::SeqCst));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn activation_rejects_dead_dependencies_before_physical_grab() {
        assert!(matches!(
            ensure_input_dependencies_ready(false, true, true),
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        ));
        assert!(matches!(
            ensure_input_dependencies_ready(true, false, true),
            Err(SwitcherError::InputWorkerDisconnected {
                worker: "pointer-watcher"
            })
        ));
        assert!(matches!(
            ensure_input_dependencies_ready(true, true, false),
            Err(SwitcherError::InputWorkerDisconnected {
                worker: "input-target-watcher"
            })
        ));
        assert!(ensure_input_dependencies_ready(true, true, true).is_ok());
    }

    #[test]
    fn runtime_input_watcher_health_names_dead_required_worker() {
        assert!(matches!(
            ensure_input_watchers_ready(false, true),
            Err(SwitcherError::InputWorkerDisconnected {
                worker: "pointer-watcher"
            })
        ));
        assert!(matches!(
            ensure_input_watchers_ready(true, false),
            Err(SwitcherError::InputWorkerDisconnected {
                worker: "input-target-watcher"
            })
        ));
        assert!(ensure_input_watchers_ready(true, true).is_ok());
    }

    #[test]
    fn caps_lock_snapshot_is_taken_immediately_before_physical_grab() {
        struct FakeKeyboard {
            caps_lock_active: bool,
            phases: Vec<&'static str>,
        }

        let mut keyboard = FakeKeyboard {
            caps_lock_active: true,
            phases: Vec::new(),
        };
        let caps_lock_active = snapshot_then_acquire_grab(
            &mut keyboard,
            |keyboard| {
                keyboard.phases.push("caps-snapshot");
                Ok::<_, SwitcherError>(keyboard.caps_lock_active)
            },
            |keyboard| {
                keyboard.phases.push("grab");
                Ok::<_, SwitcherError>(())
            },
        )
        .unwrap();

        assert!(caps_lock_active);
        assert_eq!(keyboard.phases, vec!["caps-snapshot", "grab"]);
    }

    #[test]
    fn correction_transaction_timeout_is_estimated_work_plus_bounded_backend_grace() {
        let mut config = test_runtime_config_snapshot();
        config.layout_delay_ms = 500;
        config.backspace_ms = 4;
        config.typing_ms = 5;
        let transaction = WriterTransactionKind::ApplyCorrection {
            plan: CorrectionPlan {
                buffer: vec![
                    crate::daemon::switch_logic::Keystroke {
                        key: Key::KEY_A,
                        shift: false,
                        caps_lock: false,
                    };
                    crate::model::MAX_CORRECTION_KEYSTROKES
                ],
                extra_backspaces: crate::model::MAX_CORRECTION_EXTRA_BACKSPACES,
            },
            config,
            modifiers: ModifierState::default(),
        };

        assert_eq!(
            transaction.execution_timeout().unwrap(),
            Duration::from_millis(3_818)
        );
        assert!(transaction.execution_timeout().unwrap() <= Duration::from_secs(5));
        assert!(
            Duration::from_millis(crate::model::MAX_CORRECTION_SCHEDULE_MS)
                .saturating_add(TRANSACTION_BACKEND_GRACE)
                <= MAX_TRANSACTION_TIMEOUT
        );
        assert!(TRANSACTION_SLEEP_QUANTUM <= Duration::from_millis(5));
    }

    #[test]
    fn deferred_request_id_wrap_skips_zero_sentinel() {
        let mut next = u64::MAX;

        assert_eq!(take_next_nonzero_request_id(&mut next), u64::MAX);
        assert_eq!(take_next_nonzero_request_id(&mut next), 1);
        assert_eq!(next, 2);
    }

    // Modifier state / layout shortcuts

    #[test]
    fn matches_alt_shift_combo_with_right_alt() {
        let state = pressed(&[(Key::KEY_RIGHTALT, 1), (Key::KEY_LEFTSHIFT, 1)]);
        assert!(state.matches_layout_switch_combo(
            LayoutSwitchCombo::AltShift,
            Key::KEY_LEFTSHIFT,
            1
        ));
    }

    #[test]
    fn matches_right_ctrl_right_shift_combo() {
        let state = pressed(&[(Key::KEY_RIGHTCTRL, 1), (Key::KEY_RIGHTSHIFT, 1)]);
        assert!(state.matches_layout_switch_combo(
            LayoutSwitchCombo::RightCtrlRightShift,
            Key::KEY_RIGHTSHIFT,
            1
        ));
    }

    #[test]
    fn matches_super_space_combo() {
        let state = pressed(&[(Key::KEY_LEFTMETA, 1)]);
        assert!(state.matches_layout_switch_combo(
            LayoutSwitchCombo::SuperSpace,
            Key::KEY_SPACE,
            1
        ));
    }

    #[test]
    fn modifier_state_roundtrips_pressed_modifiers_through_bits() {
        let state = pressed(&[
            (Key::KEY_LEFTCTRL, 1),
            (Key::KEY_RIGHTCTRL, 1),
            (Key::KEY_LEFTSHIFT, 1),
            (Key::KEY_RIGHTSHIFT, 1),
            (Key::KEY_LEFTALT, 1),
            (Key::KEY_RIGHTALT, 1),
            (Key::KEY_LEFTMETA, 1),
            (Key::KEY_RIGHTMETA, 1),
        ]);

        let restored = ModifierState::from_bits(state.to_bits());

        assert_eq!(restored.to_bits(), state.to_bits());
        assert!(restored.is_ctrl_pressed());
        assert!(restored.is_shift_pressed());
        assert!(restored.is_alt_pressed());
        assert!(restored.is_meta_pressed());
    }

    #[test]
    fn caps_lock_toggles_only_on_key_press() {
        let mut state = ModifierState::default();

        state.update(Key::KEY_CAPSLOCK, 1);
        assert!(state.is_caps_lock_active());

        state.update(Key::KEY_CAPSLOCK, 0);
        assert!(state.is_caps_lock_active());

        state.update(Key::KEY_CAPSLOCK, 2);
        assert!(state.is_caps_lock_active());

        state.update(Key::KEY_CAPSLOCK, 1);
        assert!(!state.is_caps_lock_active());
    }

    #[test]
    fn shared_modifier_state_stores_and_loads_snapshot() {
        let shared = SharedModifierState::default();
        let state = pressed(&[
            (Key::KEY_RIGHTCTRL, 1),
            (Key::KEY_LEFTSHIFT, 1),
            (Key::KEY_RIGHTALT, 1),
            (Key::KEY_LEFTMETA, 1),
        ]);

        shared.store(state);

        let snapshot = shared.snapshot();
        assert_eq!(snapshot.to_bits(), state.to_bits());
        assert!(snapshot.is_ctrl_pressed());
        assert!(snapshot.is_shift_pressed());
        assert!(snapshot.is_alt_pressed());
        assert!(snapshot.is_meta_pressed());
    }

    #[test]
    fn modifier_state_aggregate_helpers_track_left_and_right_keys() {
        assert!(pressed(&[(Key::KEY_LEFTSHIFT, 1)]).is_shift_pressed());
        assert!(pressed(&[(Key::KEY_RIGHTSHIFT, 1)]).is_shift_pressed());
        assert!(pressed(&[(Key::KEY_LEFTCTRL, 1)]).is_ctrl_pressed());
        assert!(pressed(&[(Key::KEY_RIGHTCTRL, 1)]).is_ctrl_pressed());
        assert!(pressed(&[(Key::KEY_LEFTALT, 1)]).is_alt_pressed());
        assert!(pressed(&[(Key::KEY_RIGHTALT, 1)]).is_alt_pressed());
        assert!(pressed(&[(Key::KEY_LEFTMETA, 1)]).is_meta_pressed());
        assert!(pressed(&[(Key::KEY_RIGHTMETA, 1)]).is_meta_pressed());

        assert!(!pressed(&[(Key::KEY_LEFTSHIFT, 0)]).is_shift_pressed());
        assert!(!pressed(&[(Key::KEY_LEFTCTRL, 0)]).is_ctrl_pressed());
        assert!(!pressed(&[(Key::KEY_LEFTALT, 0)]).is_alt_pressed());
        assert!(!pressed(&[(Key::KEY_LEFTMETA, 0)]).is_meta_pressed());
    }

    #[test]
    fn wayland_focus_switch_shortcut_matches_alt_or_super_tab() {
        assert!(is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTALT, 1)]),
            Key::KEY_TAB,
            1,
        ));
        assert!(is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_RIGHTALT, 1), (Key::KEY_LEFTSHIFT, 1)]),
            Key::KEY_TAB,
            1,
        ));
        assert!(is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTMETA, 1)]),
            Key::KEY_TAB,
            1,
        ));
        assert!(is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_RIGHTMETA, 1), (Key::KEY_RIGHTSHIFT, 1)]),
            Key::KEY_TAB,
            1,
        ));
    }

    #[test]
    fn wayland_focus_switch_shortcut_rejects_non_focus_switch_events() {
        assert!(!is_wayland_focus_switch_shortcut(
            ModifierState::default(),
            Key::KEY_TAB,
            1,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTSHIFT, 1)]),
            Key::KEY_TAB,
            1,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTCTRL, 1), (Key::KEY_LEFTALT, 1)]),
            Key::KEY_TAB,
            1,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTCTRL, 1), (Key::KEY_LEFTMETA, 1)]),
            Key::KEY_TAB,
            1,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTALT, 1)]),
            Key::KEY_A,
            1,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTMETA, 1)]),
            Key::KEY_A,
            1,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTALT, 1)]),
            Key::KEY_TAB,
            0,
        ));
        assert!(!is_wayland_focus_switch_shortcut(
            pressed(&[(Key::KEY_LEFTALT, 1)]),
            Key::KEY_TAB,
            2,
        ));
    }

    // Correction replay helpers

    #[test]
    fn replay_shift_uses_caps_lock_to_preserve_visible_case() {
        let lowercase_target = crate::daemon::switch_logic::Keystroke {
            key: Key::KEY_H,
            shift: false,
            caps_lock: false,
        };
        let uppercase_target = crate::daemon::switch_logic::Keystroke {
            key: Key::KEY_H,
            shift: true,
            caps_lock: false,
        };

        assert!(replay_shift_for_stroke(&lowercase_target, true));
        assert!(!replay_shift_for_stroke(&uppercase_target, true));
        assert!(!replay_shift_for_stroke(&lowercase_target, false));
        assert!(replay_shift_for_stroke(&uppercase_target, false));
    }

    struct FakeLayoutSwitcher {
        calls: usize,
        fail: bool,
        cancel_on_switch: Option<WriterTransactionControl>,
    }

    #[derive(Default)]
    struct FakeUinputStrokeSink {
        events: Vec<String>,
        cancel_on_sync: Option<WriterTransactionControl>,
        fail_sync_on_call: Option<usize>,
        sync_calls: usize,
    }

    impl UinputStrokeSink for FakeUinputStrokeSink {
        fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
            self.events.push(format!("key:{key:?}:{value}"));
            Ok(())
        }

        fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
            self.sync_calls += 1;
            self.events.push("sync".to_string());
            if let Some(control) = self.cancel_on_sync.take() {
                let _ = control.mark_timed_out();
            }
            if self.fail_sync_on_call == Some(self.sync_calls) {
                return Err(SwitcherError::Io(io::Error::other("stroke sync failed")));
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeUinputShortcutSink {
        events: Vec<String>,
        release_calls: usize,
        fail_first_release: bool,
    }

    impl UinputShortcutSink for FakeUinputShortcutSink {
        fn release_shortcut_key(
            &mut self,
            key: &uinput::event::keyboard::Key,
        ) -> Result<(), SwitcherError> {
            self.release_calls += 1;
            self.events.push(format!("up:{key:?}"));
            if self.fail_first_release && self.release_calls == 1 {
                Err(SwitcherError::Io(io::Error::other("first release failed")))
            } else {
                Ok(())
            }
        }

        fn synchronize_shortcut_keys(&mut self) -> Result<(), SwitcherError> {
            self.events.push("sync".to_string());
            Ok(())
        }
    }

    impl LayoutSwitcher for FakeLayoutSwitcher {
        fn switch_layout(&mut self, _combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
            self.calls += 1;
            if let Some(control) = &self.cancel_on_switch {
                let _ = control.mark_timed_out();
            }
            if self.fail {
                Err(SwitcherError::Io(io::Error::other("switch failed")))
            } else {
                Ok(())
            }
        }
    }

    struct MutationAuthorizedFailingLayoutSwitcher {
        calls: usize,
    }

    impl LayoutSwitcher for MutationAuthorizedFailingLayoutSwitcher {
        fn switch_layout(&mut self, _combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
            unreachable!("phase-aware test switcher uses switch_layout_with_hooks")
        }

        fn switch_layout_with_hooks(
            &mut self,
            _combo: LayoutSwitchCombo,
            hooks: &LayoutSwitchHooks<'_>,
        ) -> Result<(), SwitcherError> {
            self.calls += 1;
            hooks.authorize_mutation()?;
            Err(SwitcherError::Io(io::Error::other(
                "ambiguous failure after X11 mutation authorization",
            )))
        }
    }

    #[derive(Default)]
    struct FakeCinnamonX11XtestReplay {
        prepare_error: Option<&'static str>,
        calls: Vec<String>,
        cancel_on_call: Option<usize>,
        control: Option<WriterTransactionControl>,
        key_down_calls: usize,
        fail_key_down_on_call: Option<usize>,
        key_up_calls: usize,
        fail_key_up_on_call: Option<usize>,
    }

    impl FakeCinnamonX11XtestReplay {
        fn record_call(&mut self, call: String) {
            self.calls.push(call);
            if self.cancel_on_call == Some(self.calls.len()) {
                if let Some(control) = &self.control {
                    let _ = control.mark_timed_out();
                }
            }
        }
    }

    impl CinnamonX11XtestReplay for FakeCinnamonX11XtestReplay {
        fn prepare_for_layout_correction(
            &mut self,
            _plan: &CorrectionPlan,
            _control: Option<&WriterTransactionControl>,
        ) -> Result<(), SwitcherError> {
            self.record_call("prepare".to_string());
            if let Some(error) = self.prepare_error.take() {
                return Err(SwitcherError::Io(io::Error::other(error)));
            }
            Ok(())
        }

        fn key_down(
            &mut self,
            key: Key,
            _control: Option<&WriterTransactionControl>,
        ) -> Result<(), SwitcherError> {
            self.key_down_calls += 1;
            self.record_call(format!("down:{key:?}"));
            if self.fail_key_down_on_call == Some(self.key_down_calls) {
                Err(SwitcherError::Io(io::Error::other("key down failed")))
            } else {
                Ok(())
            }
        }

        fn key_up(&mut self, key: Key) -> Result<(), SwitcherError> {
            self.key_up_calls += 1;
            self.record_call(format!("up:{key:?}"));
            if self.fail_key_up_on_call == Some(self.key_up_calls) {
                Err(SwitcherError::Io(io::Error::other("key up failed")))
            } else {
                Ok(())
            }
        }
    }

    fn test_context(
        session_type: SessionType,
        desktop_environment: DesktopEnvironment,
    ) -> SystemContext {
        SystemContext {
            session_type,
            desktop_environment,
            distro: DistroKind::Unknown,
        }
    }

    #[test]
    fn cinnamon_x11_switching_correction_selects_cinnamon_xkb_xtest_replay() {
        let strategy = select_correction_replay_strategy(
            test_context(SessionType::X11, DesktopEnvironment::Cinnamon),
            true,
            true,
        );

        assert_eq!(strategy, CorrectionReplayStrategy::CinnamonXkbXtest);
    }

    #[test]
    fn cinnamon_xkb_target_group_toggles_an_exact_pair() {
        assert_eq!(cinnamon_xkb_target_group(2, 0).unwrap(), 1);
        assert_eq!(cinnamon_xkb_target_group(2, 1).unwrap(), 0);
    }

    #[test]
    fn cinnamon_xkb_target_group_rejects_unsupported_state() {
        assert!(cinnamon_xkb_target_group(1, 0).is_err());
        assert!(cinnamon_xkb_target_group(3, 0).is_err());
        assert!(cinnamon_xkb_target_group(2, 2).is_err());
    }

    #[test]
    fn non_cinnamon_x11_switching_correction_keeps_generic_replay() {
        let strategy = select_correction_replay_strategy(
            test_context(SessionType::X11, DesktopEnvironment::Xfce),
            true,
            true,
        );

        assert_eq!(strategy, CorrectionReplayStrategy::Generic);
    }

    #[test]
    fn gnome_wayland_switching_correction_keeps_generic_replay() {
        let strategy = select_correction_replay_strategy(
            test_context(SessionType::Wayland, DesktopEnvironment::Gnome),
            true,
            true,
        );

        assert_eq!(strategy, CorrectionReplayStrategy::Generic);
    }

    #[test]
    fn cinnamon_x11_replay_readiness_failure_aborts_before_backspace() {
        let mut replay = FakeCinnamonX11XtestReplay {
            prepare_error: Some("XKB unavailable"),
            calls: Vec::new(),
            ..Default::default()
        };

        let result = run_cinnamon_x11_xtest_correction(
            &mut replay,
            &CorrectionPlan {
                buffer: vec![crate::daemon::switch_logic::Keystroke {
                    key: Key::KEY_G,
                    shift: true,
                    caps_lock: false,
                }],
                extra_backspaces: 0,
            },
            &test_runtime_config_snapshot(),
            ModifierState::default(),
            None,
        );

        assert!(result.is_err());
        assert_eq!(replay.calls, vec!["prepare"]);
        assert!(!replay.calls.iter().any(|call| call.contains("BACKSPACE")));
    }

    #[test]
    fn cinnamon_x11_replay_keycode_mapping_failure_aborts_before_backspace() {
        let mut replay = FakeCinnamonX11XtestReplay {
            prepare_error: Some("keycode unavailable"),
            calls: Vec::new(),
            ..Default::default()
        };

        let result = run_cinnamon_x11_xtest_correction(
            &mut replay,
            &CorrectionPlan {
                buffer: vec![crate::daemon::switch_logic::Keystroke {
                    key: Key::KEY_H,
                    shift: false,
                    caps_lock: false,
                }],
                extra_backspaces: 0,
            },
            &test_runtime_config_snapshot(),
            ModifierState::default(),
            None,
        );

        assert!(result.is_err());
        assert_eq!(replay.calls, vec!["prepare"]);
        assert!(!replay.calls.iter().any(|call| call.contains("BACKSPACE")));
    }

    #[test]
    fn writer_transaction_cancellation_balances_current_tap_and_starts_no_new_key_down() {
        let control = WriterTransactionControl::with_timeout_for_test(88, Duration::from_secs(1));
        let mut replay = FakeCinnamonX11XtestReplay {
            cancel_on_call: Some(2),
            control: Some(control.clone()),
            ..Default::default()
        };
        let plan = CorrectionPlan {
            buffer: vec![
                crate::daemon::switch_logic::Keystroke {
                    key: Key::KEY_A,
                    shift: false,
                    caps_lock: false,
                },
                crate::daemon::switch_logic::Keystroke {
                    key: Key::KEY_B,
                    shift: false,
                    caps_lock: false,
                },
            ],
            extra_backspaces: 0,
        };

        let error = run_cinnamon_x11_xtest_correction(
            &mut replay,
            &plan,
            &test_runtime_config_snapshot(),
            ModifierState::default(),
            Some(&control),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 88 }
        ));
        assert_eq!(
            replay.calls,
            vec![
                "prepare".to_string(),
                "down:KEY_BACKSPACE".to_string(),
                "up:KEY_BACKSPACE".to_string(),
            ]
        );
    }

    #[test]
    fn transactional_xtest_tap_releases_after_ambiguous_key_down_error_and_keeps_primary_error() {
        let mut replay = FakeCinnamonX11XtestReplay {
            fail_key_down_on_call: Some(1),
            fail_key_up_on_call: Some(1),
            ..Default::default()
        };

        let error = xtest_key_tap(&mut replay, Key::KEY_SPACE, Duration::ZERO, None).unwrap_err();

        assert!(error.to_string().contains("key down failed"));
        assert_eq!(replay.calls, vec!["down:KEY_SPACE", "up:KEY_SPACE"]);
    }

    #[test]
    fn transactional_xtest_tap_returns_key_up_error_when_no_primary_error_exists() {
        let mut replay = FakeCinnamonX11XtestReplay {
            fail_key_up_on_call: Some(1),
            ..Default::default()
        };

        let error = xtest_key_tap(&mut replay, Key::KEY_SPACE, Duration::ZERO, None).unwrap_err();

        assert!(error.to_string().contains("key up failed"));
        assert_eq!(replay.calls, vec!["down:KEY_SPACE", "up:KEY_SPACE"]);
    }

    #[test]
    fn shifted_replay_releases_ambiguous_shift_down_and_preserves_primary_error() {
        let mut replay = FakeCinnamonX11XtestReplay {
            fail_key_down_on_call: Some(2),
            fail_key_up_on_call: Some(2),
            ..Default::default()
        };
        let plan = CorrectionPlan {
            buffer: vec![crate::daemon::switch_logic::Keystroke {
                key: Key::KEY_A,
                shift: true,
                caps_lock: false,
            }],
            extra_backspaces: 0,
        };

        let error = run_cinnamon_x11_xtest_correction(
            &mut replay,
            &plan,
            &test_runtime_config_snapshot(),
            ModifierState::default(),
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("key down failed"));
        assert_eq!(
            replay.calls,
            vec![
                "prepare",
                "down:KEY_BACKSPACE",
                "up:KEY_BACKSPACE",
                "down:KEY_LEFTSHIFT",
                "up:KEY_LEFTSHIFT"
            ]
        );
    }

    #[test]
    fn shifted_replay_preserves_tap_error_over_shift_release_error() {
        let mut replay = FakeCinnamonX11XtestReplay {
            fail_key_down_on_call: Some(3),
            fail_key_up_on_call: Some(3),
            ..Default::default()
        };
        let plan = CorrectionPlan {
            buffer: vec![crate::daemon::switch_logic::Keystroke {
                key: Key::KEY_A,
                shift: true,
                caps_lock: false,
            }],
            extra_backspaces: 0,
        };

        let error = run_cinnamon_x11_xtest_correction(
            &mut replay,
            &plan,
            &test_runtime_config_snapshot(),
            ModifierState::default(),
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("key down failed"));
        assert_eq!(
            replay.calls,
            vec![
                "prepare",
                "down:KEY_BACKSPACE",
                "up:KEY_BACKSPACE",
                "down:KEY_LEFTSHIFT",
                "down:KEY_A",
                "up:KEY_A",
                "up:KEY_LEFTSHIFT"
            ]
        );
    }

    #[test]
    fn fast_xtest_tap_releases_after_ambiguous_key_down_error_and_keeps_primary_error() {
        let key_up_called = Cell::new(false);

        let error = finish_fast_xtest_tap_attempt(
            Err(SwitcherError::Io(io::Error::other("fast key down failed"))),
            || {
                key_up_called.set(true);
                Err(SwitcherError::Io(io::Error::other("fast key up failed")))
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("fast key down failed"));
        assert!(key_up_called.get());
    }

    #[test]
    fn fast_xtest_tap_returns_key_up_error_when_key_down_succeeds() {
        let error = finish_fast_xtest_tap_attempt(Ok(()), || {
            Err(SwitcherError::Io(io::Error::other("fast key up failed")))
        })
        .unwrap_err();

        assert!(error.to_string().contains("fast key up failed"));
    }

    #[test]
    fn modifier_release_finishes_all_key_ups_after_cancellation() {
        let control = WriterTransactionControl::with_timeout_for_test(92, Duration::from_secs(1));
        let mut replay = FakeCinnamonX11XtestReplay {
            cancel_on_call: Some(1),
            control: Some(control.clone()),
            ..Default::default()
        };
        let modifiers = ModifierState {
            left_ctrl: true,
            right_ctrl: true,
            ..Default::default()
        };

        let error = release_modifiers_xtest(&mut replay, modifiers, Some(&control)).unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 92 }
        ));
        assert_eq!(replay.calls, vec!["up:KEY_LEFTCTRL", "up:KEY_RIGHTCTRL"]);
    }

    #[test]
    fn modifier_cleanup_attempts_every_xtest_release_after_first_error() {
        let mut replay = FakeCinnamonX11XtestReplay {
            fail_key_up_on_call: Some(1),
            ..Default::default()
        };
        let modifiers = ModifierState {
            left_ctrl: true,
            right_ctrl: true,
            left_shift: true,
            ..Default::default()
        };

        let error = release_modifiers_xtest(&mut replay, modifiers, None).unwrap_err();

        assert!(error.to_string().contains("key up failed"));
        assert_eq!(
            replay.calls,
            vec!["up:KEY_LEFTCTRL", "up:KEY_RIGHTCTRL", "up:KEY_LEFTSHIFT"]
        );
    }

    #[test]
    fn restore_modifiers_releases_ambiguous_current_down_and_previously_restored_keys() {
        let mut replay = FakeCinnamonX11XtestReplay {
            fail_key_down_on_call: Some(2),
            fail_key_up_on_call: Some(1),
            ..Default::default()
        };
        let modifiers = ModifierState {
            left_ctrl: true,
            right_ctrl: true,
            ..Default::default()
        };

        let error = restore_modifiers_xtest(&mut replay, modifiers, None).unwrap_err();

        assert!(error.to_string().contains("key down failed"));
        assert_eq!(
            replay.calls,
            vec![
                "down:KEY_LEFTCTRL",
                "down:KEY_RIGHTCTRL",
                "up:KEY_RIGHTCTRL",
                "up:KEY_LEFTCTRL"
            ]
        );
    }

    #[test]
    fn shortcut_cleanup_attempts_every_release_and_sync_after_first_error() {
        use uinput::event::keyboard::Key as UinputKey;

        let mut sink = FakeUinputShortcutSink {
            fail_first_release: true,
            ..Default::default()
        };
        let trigger = UinputKey::C;
        let modifiers = [UinputKey::LeftControl, UinputKey::LeftShift];

        let error =
            release_shortcut_keys(&mut sink, Some(&trigger), &[&modifiers[0], &modifiers[1]])
                .unwrap_err();

        assert!(error.to_string().contains("first release failed"));
        assert_eq!(
            sink.events,
            vec!["up:C", "up:LeftShift", "up:LeftControl", "sync"]
        );
    }

    #[test]
    fn modifier_cleanup_attempts_every_uinput_release_and_final_sync_after_first_error() {
        let mut sink = FakeUinputShortcutSink {
            fail_first_release: true,
            ..Default::default()
        };
        let modifiers = ModifierState {
            left_shift: true,
            left_ctrl: true,
            right_ctrl: true,
            ..Default::default()
        };

        let error = release_uinput_modifiers_exhaustively(&mut sink, modifiers).unwrap_err();

        assert!(error.to_string().contains("first release failed"));
        assert_eq!(
            sink.events,
            vec!["up:RightControl", "up:LeftControl", "up:LeftShift", "sync"]
        );
    }

    #[test]
    fn generic_shifted_typing_cancellation_releases_shift_before_returning() {
        let control = WriterTransactionControl::with_timeout_for_test(90, Duration::from_secs(1));
        let mut sink = FakeUinputStrokeSink {
            cancel_on_sync: Some(control.clone()),
            ..Default::default()
        };

        let error =
            replay_uinput_stroke(&mut sink, Key::KEY_A, true, Duration::ZERO, Some(&control))
                .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 90 }
        ));
        assert_eq!(
            sink.events,
            vec!["key:KEY_LEFTSHIFT:1", "sync", "key:KEY_LEFTSHIFT:0", "sync",]
        );
    }

    #[test]
    fn generic_backspace_sync_failure_releases_issued_key_and_syncs_cleanup() {
        let mut sink = FakeUinputStrokeSink {
            fail_sync_on_call: Some(1),
            ..Default::default()
        };

        let error = replay_uinput_backspaces(&mut sink, 1, Duration::ZERO, None).unwrap_err();

        assert!(error.to_string().contains("stroke sync failed"));
        assert_eq!(
            sink.events,
            vec!["key:KEY_BACKSPACE:1", "sync", "key:KEY_BACKSPACE:0", "sync",]
        );
    }

    #[test]
    fn xtest_cancellation_after_keycode_validation_prevents_fake_key_down() {
        let control = WriterTransactionControl::with_timeout_for_test(89, Duration::from_secs(1));
        let emitted = Cell::new(false);

        let error = validate_and_emit_xtest_key(
            Key::KEY_A,
            Some(&control),
            None,
            None,
            None,
            |_| {
                let _ = control.mark_timed_out();
                Ok(38)
            },
            |_| {
                emitted.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 89 }
        ));
        assert!(!emitted.get());
    }

    #[test]
    fn cinnamon_mapping_cancellation_stops_before_next_mapping() {
        let control = WriterTransactionControl::with_timeout_for_test(93, Duration::from_secs(1));
        let plan = CorrectionPlan {
            buffer: vec![crate::daemon::switch_logic::Keystroke {
                key: Key::KEY_A,
                shift: false,
                caps_lock: false,
            }],
            extra_backspaces: 0,
        };
        let mapping_calls = Cell::new(0usize);

        let error = validate_cinnamon_plan_keycodes_with(&plan, Some(&control), |_key| {
            mapping_calls.set(mapping_calls.get() + 1);
            if mapping_calls.get() == 1 {
                let _ = control.mark_timed_out();
            }
            Ok(())
        })
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 93 }
        ));
        assert_eq!(mapping_calls.get(), 1);
    }

    #[test]
    fn xtest_fast_path_shared_failure_after_validation_prevents_fake_key_down() {
        let failure = AtomicU64::new(0);
        let stop_requested = AtomicBool::new(false);
        let terminal_gate = Mutex::new(());
        let emitted = Cell::new(false);

        let error = validate_and_emit_xtest_key(
            Key::KEY_A,
            None,
            Some(&failure),
            Some(&stop_requested),
            Some(&terminal_gate),
            |_| {
                failure.store(91, Ordering::SeqCst);
                Ok(38)
            },
            |_| {
                emitted.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 91 }
        ));
        assert!(!emitted.get());
    }

    #[test]
    fn layout_switch_outcome_reports_applied_x11_when_x11_succeeds() {
        let mut x11 = FakeLayoutSwitcher {
            calls: 0,
            fail: false,
            cancel_on_switch: None,
        };
        let mut uinput = FakeLayoutSwitcher {
            calls: 0,
            fail: false,
            cancel_on_switch: None,
        };

        let outcome = switch_layout_with_fallback(
            Some(&mut x11),
            &mut uinput,
            LayoutSwitchCombo::super_space(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, CorrectionLayoutSwitchOutcome::AppliedX11);
        assert_eq!(x11.calls, 1);
        assert_eq!(uinput.calls, 0);
    }

    #[test]
    fn layout_switch_outcome_reports_applied_uinput_for_uinput_strategy() {
        let mut uinput = FakeLayoutSwitcher {
            calls: 0,
            fail: false,
            cancel_on_switch: None,
        };

        let outcome =
            switch_layout_with_fallback(None, &mut uinput, LayoutSwitchCombo::super_space(), None)
                .unwrap();

        assert_eq!(outcome, CorrectionLayoutSwitchOutcome::AppliedUinput);
        assert_eq!(uinput.calls, 1);
    }

    #[test]
    fn layout_switch_outcome_reports_applied_uinput_after_x11_fallback() {
        let mut x11 = FakeLayoutSwitcher {
            calls: 0,
            fail: true,
            cancel_on_switch: None,
        };
        let mut uinput = FakeLayoutSwitcher {
            calls: 0,
            fail: false,
            cancel_on_switch: None,
        };

        let outcome = switch_layout_with_fallback(
            Some(&mut x11),
            &mut uinput,
            LayoutSwitchCombo::super_space(),
            None,
        )
        .unwrap();

        assert_eq!(outcome, CorrectionLayoutSwitchOutcome::AppliedUinput);
        assert_eq!(x11.calls, 1);
        assert_eq!(uinput.calls, 1);
    }

    #[test]
    fn layout_fallback_does_not_retry_after_x11_mutation_was_authorized() {
        let mut x11 = MutationAuthorizedFailingLayoutSwitcher { calls: 0 };
        let mut uinput = FakeLayoutSwitcher {
            calls: 0,
            fail: false,
            cancel_on_switch: None,
        };

        let error = switch_layout_with_fallback(
            Some(&mut x11),
            &mut uinput,
            LayoutSwitchCombo::super_space(),
            None,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("ambiguous failure after X11 mutation authorization"));
        assert_eq!(x11.calls, 1);
        assert_eq!(uinput.calls, 0);
    }

    #[test]
    fn layout_fallback_starts_no_uinput_mutation_after_transaction_timeout() {
        let control = WriterTransactionControl::with_timeout_for_test(99, Duration::from_secs(1));
        let mut x11 = FakeLayoutSwitcher {
            calls: 0,
            fail: true,
            cancel_on_switch: Some(control.clone()),
        };
        let mut uinput = FakeLayoutSwitcher {
            calls: 0,
            fail: false,
            cancel_on_switch: None,
        };

        let error = switch_layout_with_fallback(
            Some(&mut x11),
            &mut uinput,
            LayoutSwitchCombo::super_space(),
            Some(&control),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 99 }
        ));
        assert_eq!(x11.calls, 1);
        assert_eq!(uinput.calls, 0);
    }

    // Device discovery

    #[test]
    fn keyboard_symlink_candidates_prioritize_by_path_before_by_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let by_path = temp_dir.path().join("by-path");
        let by_id = temp_dir.path().join("by-id");
        fs::create_dir_all(&by_path).unwrap();
        fs::create_dir_all(&by_id).unwrap();

        let by_path_keyboard = by_path.join("platform-i8042-serio-0-event-kbd");
        let by_id_keyboard = by_id.join("usb-Logitech_USB_Keyboard-event-kbd");
        fs::write(&by_path_keyboard, "").unwrap();
        fs::write(&by_id_keyboard, "").unwrap();

        let candidates =
            collect_keyboard_symlink_candidates_from_dirs(&[by_path.as_path(), by_id.as_path()]);

        assert_eq!(
            candidates,
            vec![by_path_keyboard, by_id_keyboard],
            "stable symlink search must prefer /dev/input/by-path before /dev/input/by-id",
        );
    }

    #[test]
    fn keyboard_open_permission_denied_maps_to_keyboard_access_denied() {
        let error = map_keyboard_open_error(
            Path::new("/dev/input/event4"),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );

        assert!(matches!(error, SwitcherError::KeyboardAccessDenied { .. }));
    }

    // X11/session policy

    #[test]
    fn x11_input_target_watcher_is_enabled_only_for_x11_sessions() {
        assert!(should_enable_x11_input_target_watcher(SessionType::X11));
        assert!(!should_enable_x11_input_target_watcher(
            SessionType::Wayland
        ));
        assert!(!should_enable_x11_input_target_watcher(
            SessionType::Unknown
        ));
    }

    #[test]
    fn layout_switcher_initializer_skips_x11_outside_x11_sessions() {
        let init_called = Cell::new(false);

        let switcher = initialize_x11_switcher_for_session(SessionType::Wayland, || {
            init_called.set(true);
            Ok::<_, SwitcherError>("x11")
        });

        assert_eq!(switcher, None);
        assert!(
            !init_called.get(),
            "Wayland strategy selection must not initialize the X11 switcher"
        );
    }

    #[test]
    fn layout_switcher_initializer_uses_x11_in_x11_sessions() {
        let init_called = Cell::new(false);

        let switcher = initialize_x11_switcher_for_session(SessionType::X11, || {
            init_called.set(true);
            Ok::<_, SwitcherError>("x11")
        });

        assert_eq!(switcher, Some("x11"));
        assert!(
            init_called.get(),
            "X11 strategy selection must keep using the X11 switcher"
        );
    }

    #[test]
    fn layout_switcher_initializer_falls_back_to_uinput_when_x11_init_fails() {
        let init_called = Cell::new(false);

        let switcher = initialize_x11_switcher_for_session(SessionType::X11, || {
            init_called.set(true);
            Err::<&str, _>(SwitcherError::Io(io::Error::other("x11 unavailable")))
        });

        assert_eq!(switcher, None);
        assert!(
            init_called.get(),
            "X11 session should attempt X11 init before falling back to uinput"
        );
    }

    // Writer lifecycle / queue behavior

    #[test]
    fn keyboard_shutdown_releases_grab_before_waiting_for_writer_ack() {
        let mut phases = Vec::new();

        let outcome = run_keyboard_shutdown_sequence(|phase| {
            phases.push(phase);
            match phase {
                KeyboardShutdownPhase::FinishWriterStop => Some(WriterShutdownOutcome::Stopped),
                _ => None,
            }
        });

        assert_eq!(outcome, WriterShutdownOutcome::Stopped);
        assert_eq!(
            phases,
            vec![
                KeyboardShutdownPhase::RequestWriterStop,
                KeyboardShutdownPhase::ReleaseGrab,
                KeyboardShutdownPhase::FinishWriterStop,
                KeyboardShutdownPhase::StopAndJoinWatchers,
            ]
        );
    }

    #[test]
    fn keyboard_shutdown_joins_watchers_only_after_writer_stopped() {
        let mut phases = Vec::new();

        let outcome = run_keyboard_shutdown_sequence(|phase| {
            phases.push(phase);
            match phase {
                KeyboardShutdownPhase::FinishWriterStop => Some(WriterShutdownOutcome::Stopped),
                _ => None,
            }
        });

        assert_eq!(outcome, WriterShutdownOutcome::Stopped);
        assert_eq!(
            phases,
            vec![
                KeyboardShutdownPhase::RequestWriterStop,
                KeyboardShutdownPhase::ReleaseGrab,
                KeyboardShutdownPhase::FinishWriterStop,
                KeyboardShutdownPhase::StopAndJoinWatchers,
            ]
        );
    }

    #[test]
    fn keyboard_shutdown_detaches_watchers_after_writer_unresponsive() {
        let mut phases = Vec::new();

        let outcome = run_keyboard_shutdown_sequence(|phase| {
            phases.push(phase);
            match phase {
                KeyboardShutdownPhase::FinishWriterStop => {
                    Some(WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 })
                }
                _ => None,
            }
        });

        assert_eq!(
            outcome,
            WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 }
        );
        assert_eq!(
            phases,
            vec![
                KeyboardShutdownPhase::RequestWriterStop,
                KeyboardShutdownPhase::ReleaseGrab,
                KeyboardShutdownPhase::FinishWriterStop,
                KeyboardShutdownPhase::DetachWatchers,
            ]
        );
    }

    #[test]
    fn partial_prepare_error_is_preserved_after_stopped_writer() {
        let trigger = SwitcherError::InputWorkerDisconnected {
            worker: "pointer-watcher",
        };

        let error = resolve_error_after_writer_shutdown(
            trigger,
            "keyboard-prepare-pointer-watcher",
            WriterShutdownOutcome::Stopped,
        );

        assert!(matches!(
            error,
            SwitcherError::InputWorkerDisconnected {
                worker: "pointer-watcher"
            }
        ));
    }

    #[test]
    fn partial_prepare_unresponsive_writer_returns_fail_stop() {
        let trigger = SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        };

        let error = resolve_error_after_writer_shutdown(
            trigger,
            "keyboard-prepare-input-target-watcher",
            WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 },
        );

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 1_000,
                phase: "keyboard-prepare-input-target-watcher",
                ref trigger,
            } if trigger == "Input worker input-target-watcher is unavailable"
        ));
    }

    #[test]
    fn writer_stop_marks_alive_false_before_shutdown_completes() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        command_tx
            .send(WriterCommand::Fast(WriterFastCommand::TypeSeparator {
                key: Key::KEY_SPACE,
            }))
            .expect("queue should accept initial command");

        let alive = Arc::new(AtomicBool::new(true));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: alive.clone(),
                stop_requested: Arc::clone(&stop_requested),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_receiver_alive = command_rx;
                let _ = release_rx.recv();
            })),
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let stopper = thread::spawn(move || {
            let mut writer = writer;
            writer.stop();
        });

        let deadline = Instant::now() + Duration::from_millis(250);
        while alive.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !alive.load(Ordering::SeqCst),
            "writer must stop advertising itself as alive immediately on teardown"
        );
        assert!(
            stop_requested.load(Ordering::SeqCst),
            "writer must publish cancellation before waiting for shutdown completion"
        );

        drop(release_tx);
        stopper.join().expect("stop thread should finish");
    }

    #[test]
    fn writer_stop_request_denies_transaction_and_fast_mutation_permits() {
        let (command_tx, _command_rx) = mpsc::sync_channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(AtomicU64::new(0));
        let terminal_gate = Arc::new(Mutex::new(()));
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::clone(&alive),
                stop_requested: Arc::clone(&stop_requested),
                transaction_failure_request_id: Arc::clone(&failure),
                transaction_terminal_gate: Arc::clone(&terminal_gate),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let control = WriterTransactionControl::new_with_writer_state(
            109,
            Duration::from_secs(1),
            Arc::clone(&failure),
            Arc::clone(&stop_requested),
            Arc::clone(&terminal_gate),
        );
        control.authorize_mutation_start().unwrap();
        authorize_writer_mutation_start(&failure, &stop_requested, &terminal_gate).unwrap();

        writer.request_stop();

        assert!(!alive.load(Ordering::SeqCst));
        assert!(stop_requested.load(Ordering::SeqCst));
        assert!(matches!(
            control.authorize_mutation_start(),
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        ));
        assert!(matches!(
            authorize_writer_mutation_start(&failure, &stop_requested, &terminal_gate),
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        ));
    }

    #[test]
    fn writer_stop_returns_when_shutdown_queue_is_full() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        command_tx
            .send(WriterCommand::Fast(WriterFastCommand::TypeSeparator {
                key: Key::KEY_SPACE,
            }))
            .expect("queue should accept initial command");

        let alive = Arc::new(AtomicBool::new(true));
        let writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: alive.clone(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(|| {})),
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let (done_tx, done_rx) = mpsc::channel();

        let stopper = thread::spawn(move || {
            let mut writer = writer;
            writer.stop();
            let _ = done_tx.send(());
        });

        match done_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(()) => {}
            Err(error) => {
                drop(command_rx);
                stopper.join().expect("stop thread should finish");
                panic!("writer stop should return without blocking on a full queue: {error:?}");
            }
        }

        drop(command_rx);
        stopper.join().expect("stop thread should finish");
        assert!(
            !alive.load(Ordering::SeqCst),
            "writer must stop advertising itself as alive during teardown"
        );
    }

    #[test]
    fn writer_stop_handles_disconnected_shutdown_receiver() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        drop(command_rx);
        let alive = Arc::new(AtomicBool::new(true));
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: alive.clone(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        writer.stop();

        assert!(
            !alive.load(Ordering::SeqCst),
            "writer must stop advertising itself as alive even when shutdown cannot be sent"
        );
    }

    #[test]
    fn writer_stop_without_thread_exit_returns_unresponsive() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: alive.clone(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_receiver_alive = command_rx;
                let _ = release_rx.recv();
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let outcome = writer.stop_with_timeout(Duration::from_millis(10));

        assert_eq!(
            outcome,
            WriterShutdownOutcome::Unresponsive { timeout_ms: 10 }
        );
        assert!(
            !alive.load(Ordering::SeqCst),
            "writer must stop advertising itself as alive during teardown"
        );

        release_tx.send(()).unwrap();
        writer
            .join_handle
            .take()
            .expect("timed-out writer handle must remain owned")
            .join()
            .unwrap();
    }

    #[test]
    fn writer_stop_timeout_retains_join_handle() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_receiver_alive = command_rx;
                let _ = release_rx.recv();
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let outcome = writer.stop_with_timeout(Duration::from_millis(10));

        assert!(matches!(
            outcome,
            WriterShutdownOutcome::Unresponsive { .. }
        ));
        assert!(
            writer.join_handle.is_some(),
            "a timed-out writer must retain ownership of its JoinHandle"
        );

        release_tx.send(()).unwrap();
        writer.join_handle.take().unwrap().join().unwrap();
    }

    #[test]
    fn writer_stop_joins_after_exit_notification() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested,
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_receiver_alive = command_rx;
                while !worker_stop_requested.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let outcome = writer.stop_with_timeout(Duration::from_millis(250));

        assert_eq!(outcome, WriterShutdownOutcome::Stopped);
        assert!(
            writer.join_handle.is_none(),
            "successful shutdown must join and consume the writer handle"
        );
    }

    #[test]
    fn writer_stop_with_full_data_queue_acks_after_stop_check() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        command_tx
            .send(WriterCommand::Fast(WriterFastCommand::TypeSeparator {
                key: Key::KEY_SPACE,
            }))
            .unwrap();
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested,
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_full_queue = command_rx;
                while !worker_stop_requested.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let outcome = writer.stop_with_timeout(Duration::from_millis(250));

        assert_eq!(outcome, WriterShutdownOutcome::Stopped);
        assert!(writer.join_handle.is_none());
    }

    #[test]
    fn writer_shutdown_deadline_includes_full_queue_wakeup_attempt() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        command_tx
            .send(WriterCommand::Fast(WriterFastCommand::TypeSeparator {
                key: Key::KEY_SPACE,
            }))
            .unwrap();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_full_queue = command_rx;
                let _ = release_rx.recv();
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let started = Instant::now();
        let outcome = writer.stop_with_timeout(Duration::ZERO);

        assert_eq!(
            outcome,
            WriterShutdownOutcome::Unresponsive { timeout_ms: 0 }
        );
        assert!(
            started.elapsed() < Duration::from_millis(25),
            "the bounded shutdown deadline must include queue wakeup retries"
        );

        release_tx.send(()).unwrap();
        writer.join_handle.take().unwrap().join().unwrap();
    }

    #[test]
    fn repeated_keyboard_shutdown_cannot_mask_unresponsive_after_late_writer_exit() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_receiver_alive = command_rx;
                let _ = release_rx.recv();
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let first = writer.stop_with_timeout(Duration::from_millis(10));
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        while writer
            .join_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        let repeated = writer.stop_with_timeout(Duration::from_millis(100));

        assert_eq!(
            first,
            WriterShutdownOutcome::Unresponsive { timeout_ms: 10 }
        );
        assert_eq!(repeated, first, "late exit must not authorize recovery");

        if let Some(handle) = writer.join_handle.take() {
            handle.join().unwrap();
        }
    }

    #[test]
    fn writer_exit_notification_follows_owned_device_drop() {
        struct DropTrace(Arc<Mutex<Vec<&'static str>>>);

        impl Drop for DropTrace {
            fn drop(&mut self) {
                self.0.lock().unwrap().push("owned-device-drop");
            }
        }

        let trace = Arc::new(Mutex::new(Vec::new()));
        let worker_trace = Arc::clone(&trace);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            run_writer_thread_with_exit_notification(
                DropTrace(Arc::clone(&worker_trace)),
                exit_tx,
                |_owned_device| {
                    worker_trace.lock().unwrap().push("loop-return");
                },
            );
        });

        exit_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("writer must publish its exit notification");
        trace.lock().unwrap().push("exit-notification");
        worker.join().expect("writer thread should join");
        trace.lock().unwrap().push("join");

        assert_eq!(
            *trace.lock().unwrap(),
            vec![
                "loop-return",
                "owned-device-drop",
                "exit-notification",
                "join",
            ]
        );
    }

    #[test]
    fn writer_exit_notification_follows_owned_device_drop_during_unwind() {
        struct DropTrace(Arc<AtomicBool>);

        impl Drop for DropTrace {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let device_dropped = Arc::new(AtomicBool::new(false));
        let worker_device_dropped = Arc::clone(&device_dropped);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            std::panic::catch_unwind(|| {
                run_writer_thread_with_exit_notification(
                    DropTrace(worker_device_dropped),
                    exit_tx,
                    |_owned_device| panic!("simulated writer panic"),
                );
            })
        });

        exit_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("unwinding writer must publish its exit notification");
        assert!(
            device_dropped.load(Ordering::SeqCst),
            "exit notification must follow owned device destruction"
        );
        assert!(worker.join().unwrap().is_err());
    }

    #[test]
    fn writer_startup_error_is_preserved_after_confirmed_stop() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(0);
        drop(ready_tx);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(false)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                drop(command_rx);
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let error = writer
            .finish_startup(
                ready_rx,
                Duration::from_millis(10),
                Duration::from_millis(250),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterDisconnected
        ));
        assert_eq!(
            writer.shutdown_outcome,
            Some(WriterShutdownOutcome::Stopped)
        );
        assert!(writer.join_handle.is_none());
    }

    #[test]
    fn writer_startup_error_becomes_fail_stop_when_writer_is_unresponsive() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_ready_tx, ready_rx) = mpsc::sync_channel::<()>(0);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(false)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_channels_alive = command_rx;
                let _ = release_rx.recv();
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let error = writer
            .finish_startup(
                ready_rx,
                Duration::from_millis(5),
                Duration::from_millis(10),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 10,
                phase: "writer-startup",
                ref trigger,
            } if trigger.contains("5 ms")
        ));
        assert!(writer.join_handle.is_some());

        release_tx.send(()).unwrap();
        writer.join_handle.take().unwrap().join().unwrap();
    }

    #[test]
    fn writer_startup_timeout_releases_ready_sender_before_shutdown_wait() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(0);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(false)),
                stop_requested,
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _keep_command_receiver_alive = command_rx;
                while !worker_stop_requested.load(Ordering::SeqCst) {
                    thread::yield_now();
                }
                let _ = ready_tx.send(());
                let _ = exit_tx.try_send(());
            })),
            exit_rx,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx: mpsc::channel().1,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let error = writer
            .finish_startup(ready_rx, Duration::ZERO, Duration::from_millis(20))
            .unwrap_err();
        if let Some(handle) = writer.join_handle.take() {
            handle.join().unwrap();
        }

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterStartupTimedOut { timeout_ms: 0 }
        ));
    }

    #[test]
    fn transaction_returns_disconnected_when_receiver_is_dropped() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
        drop(command_rx);

        let error = handle
            .run_transaction(copy_shortcut_transaction())
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterDisconnected
        ));
    }

    #[test]
    fn transaction_returns_disconnected_when_reply_sender_is_dropped() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
        let worker = thread::spawn(move || {
            let command = command_rx
                .recv()
                .expect("transaction command should be sent");
            match command {
                WriterCommand::Transaction(WriterTransaction::Execute { reply, .. }) => {
                    drop(reply);
                }
                _ => panic!("expected transaction command"),
            }
        });

        let error = handle
            .run_transaction(copy_shortcut_transaction())
            .unwrap_err();
        worker.join().expect("worker should finish");

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterDisconnected
        ));
    }

    #[test]
    fn writer_transaction_timeout_returns_and_cancels_an_accepted_command() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
        let caller_handle = handle.clone();
        let started = Instant::now();
        let caller = thread::spawn(move || {
            caller_handle.run_transaction_with_timeout(
                copy_shortcut_transaction(),
                Duration::from_millis(30),
            )
        });

        let command = command_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("transaction should be accepted before its deadline");
        let (request_id, control, _held_reply_sender) = match command {
            WriterCommand::Transaction(WriterTransaction::Execute { control, reply, .. }) => {
                (control.request_id(), control, reply)
            }
            _ => panic!("expected transaction command"),
        };

        let error = caller.join().expect("caller should return").unwrap_err();

        assert!(
            started.elapsed() < Duration::from_millis(250),
            "accepted transaction must never leave the caller waiting forever"
        );
        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut {
                request_id: timed_out
            } if timed_out == request_id
        ));
        assert!(control.is_cancelled());
        assert_eq!(handle.transaction_failure_request_id(), Some(request_id));
        assert!(!handle.is_alive());
        assert!(matches!(
            handle.type_separator(Key::KEY_SPACE),
            Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut {
                request_id: blocked
            }) if blocked == request_id
        ));
    }

    #[test]
    fn writer_transaction_late_reply_cannot_replace_timeout_outcome() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
        let caller_handle = handle.clone();
        let caller = thread::spawn(move || {
            caller_handle.run_transaction_with_timeout(
                copy_shortcut_transaction(),
                Duration::from_millis(25),
            )
        });

        let command = command_rx.recv().expect("transaction should be accepted");
        let (request_id, control, reply) = match command {
            WriterCommand::Transaction(WriterTransaction::Execute { control, reply, .. }) => {
                (control.request_id(), control, reply)
            }
            _ => panic!("expected transaction command"),
        };
        thread::sleep(Duration::from_millis(50));
        let late_publish = publish_writer_transaction_result(
            &control,
            reply,
            Ok(CorrectionExecutionOutcome {
                layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
            }),
        );

        let error = caller.join().expect("caller should return").unwrap_err();

        assert!(
            matches!(
                late_publish,
                Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut {
                    request_id: late_request_id
                }) if late_request_id == request_id
            ),
            "late worker publication must observe the timeout winner"
        );
        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut {
                request_id: timed_out
            } if timed_out == request_id
        ));
    }

    #[test]
    fn writer_timeout_wakes_another_waiting_clone_within_one_quantum() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
        let first_handle = handle.clone();
        let first = thread::spawn(move || {
            first_handle
                .run_transaction_with_timeout(copy_shortcut_transaction(), Duration::from_secs(1))
        });
        let first_command = command_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("first transaction should queue");
        let (first_request_id, first_control, _first_reply) = match first_command {
            WriterCommand::Transaction(WriterTransaction::Execute { control, reply, .. }) => {
                (control.request_id(), control, reply)
            }
            _ => panic!("expected first transaction"),
        };

        let second_handle = handle.clone();
        let second = thread::spawn(move || {
            second_handle
                .run_transaction_with_timeout(copy_shortcut_transaction(), Duration::from_secs(1))
        });
        let second_command = command_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("second transaction should queue");
        let (second_control, _second_reply) = match second_command {
            WriterCommand::Transaction(WriterTransaction::Execute { control, reply, .. }) => {
                (control, reply)
            }
            _ => panic!("expected second transaction"),
        };

        let second_started = Instant::now();
        let _ = first_control.mark_timed_out();
        let first_error = first
            .join()
            .expect("first caller should return")
            .unwrap_err();
        let second_error = second
            .join()
            .expect("second caller should wake on shared failure")
            .unwrap_err();

        assert!(second_started.elapsed() < Duration::from_millis(200));
        assert!(matches!(
            first_error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id }
                if request_id == first_request_id
        ));
        assert!(matches!(
            second_error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id }
                if request_id == first_request_id
        ));
        assert_eq!(first_control.state(), WriterTransactionState::TimedOut);
        assert_eq!(second_control.state(), WriterTransactionState::TimedOut);
    }

    #[test]
    fn writer_transaction_rejects_oversized_plan_before_enqueue() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);

        let error = handle
            .run_transaction(oversized_correction_transaction())
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkValidation(
                crate::error::ValidationError::InputCorrectionPlanTooLarge { .. }
            )
        ));
        assert!(matches!(
            command_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn writer_transaction_control_interrupts_long_sleep_at_deadline() {
        let control =
            WriterTransactionControl::with_timeout_for_test(77, Duration::from_millis(25));
        let started = Instant::now();

        let error = control
            .sleep_interruptibly(Duration::from_secs(1))
            .unwrap_err();

        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 77 }
        ));
        assert!(control.is_cancelled());
    }

    #[test]
    fn writer_transaction_timeout_cancels_other_controls_sharing_the_writer() {
        let failure = Arc::new(AtomicU64::new(0));
        let terminal_gate = Arc::new(Mutex::new(()));
        let first = WriterTransactionControl::new_with_terminal_gate(
            101,
            Duration::from_secs(1),
            Arc::clone(&failure),
            Arc::clone(&terminal_gate),
        );
        let second = WriterTransactionControl::new_with_terminal_gate(
            102,
            Duration::from_secs(1),
            failure,
            terminal_gate,
        );

        let timeout = second.mark_timed_out();
        let first_error = first.ensure_active().unwrap_err();

        assert!(matches!(
            timeout,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 102 }
        ));
        assert!(matches!(
            first_error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 102 }
        ));
        assert!(first.is_cancelled());
    }

    #[test]
    fn writer_transaction_completion_published_before_timeout_cas_wins() {
        let control = WriterTransactionControl::with_timeout_for_test(103, Duration::from_secs(1));
        let (reply_tx, reply_rx) = mpsc::channel();
        let expected = CorrectionExecutionOutcome {
            layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
        };

        reply_tx
            .send(Ok(expected))
            .expect("reply must be published before completion state");
        assert!(control.publish_completed());

        let outcome = control.wait_for_reply(reply_rx).unwrap();

        assert_eq!(outcome, expected);
        assert_eq!(control.state(), WriterTransactionState::Completed);
        assert_eq!(control.failure_request_id.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn completed_transaction_cannot_authorize_new_mutation() {
        let failure = Arc::new(AtomicU64::new(0));
        let control =
            WriterTransactionControl::new(106, Duration::from_secs(1), Arc::clone(&failure));
        assert!(control.publish_completed());
        let mutation_started = Cell::new(false);

        let error = control
            .authorize_mutation_start()
            .and_then(|_| {
                mutation_started.set(true);
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 106,
                ..
            }
        ));
        assert!(!mutation_started.get());
        assert_eq!(control.state(), WriterTransactionState::Completed);
        assert_eq!(failure.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn writer_transaction_completion_after_wall_deadline_publishes_timeout() {
        let control = WriterTransactionControl::with_timeout_for_test(104, Duration::ZERO);
        let (reply_tx, reply_rx) = mpsc::channel();

        reply_tx
            .send(Ok(CorrectionExecutionOutcome {
                layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
            }))
            .expect("reply publication should remain ordered before the terminal state");

        assert!(!control.publish_completed());
        let error = control.wait_for_reply(reply_rx).unwrap_err();

        assert_eq!(control.state(), WriterTransactionState::TimedOut);
        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 104 }
        ));
        assert_eq!(control.failure_request_id.load(Ordering::SeqCst), 104);
    }

    #[test]
    fn writer_stop_interrupts_reply_wait_before_transaction_deadline() {
        let failure = Arc::new(AtomicU64::new(0));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let terminal_gate = Arc::new(Mutex::new(()));
        let control = WriterTransactionControl::new_with_writer_state(
            107,
            Duration::from_millis(200),
            failure,
            Arc::clone(&stop_requested),
            Arc::clone(&terminal_gate),
        );
        let (_reply_tx, reply_rx) = mpsc::channel();
        {
            let _terminal_guard = terminal_gate.lock().unwrap();
            stop_requested.store(true, Ordering::SeqCst);
        }
        let started = Instant::now();

        let error = control.wait_for_reply(reply_rx).unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterDisconnected
        ));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "stop must interrupt the waiter instead of consuming its full deadline"
        );
    }

    #[test]
    fn required_input_worker_loss_interrupts_pending_transaction_before_deadline() {
        let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
        let input_worker_alive = Arc::new(AtomicBool::new(true));
        let caller_input_worker_alive = Arc::clone(&input_worker_alive);
        let caller_handle = handle.clone();
        let caller = thread::spawn(move || {
            caller_handle.run_transaction_with_timeout_and_input_health(
                copy_shortcut_transaction(),
                Duration::from_secs(1),
                || {
                    (!caller_input_worker_alive.load(Ordering::SeqCst)).then_some(
                        SwitcherError::InputWorkerDisconnected {
                            worker: "input-target-watcher",
                        },
                    )
                },
            )
        });

        let command = command_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("transaction should be accepted before watcher loss");
        let (control, _held_reply_sender) = match command {
            WriterCommand::Transaction(WriterTransaction::Execute { control, reply, .. }) => {
                (control, reply)
            }
            _ => panic!("expected transaction command"),
        };

        let failure_started = Instant::now();
        input_worker_alive.store(false, Ordering::SeqCst);
        let error = caller
            .join()
            .expect("caller should wake after required watcher loss")
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkerDisconnected {
                worker: "input-target-watcher"
            }
        ));
        assert!(
            failure_started.elapsed() < Duration::from_millis(200),
            "watcher loss must interrupt the transaction instead of consuming its deadline"
        );
        assert!(handle.stop_requested.load(Ordering::SeqCst));
        assert!(matches!(
            control.ensure_active(),
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        ));
    }

    #[test]
    fn queued_writer_error_wins_over_concurrent_input_worker_loss() {
        let control = WriterTransactionControl::with_timeout_for_test(109, Duration::from_secs(1));
        let (reply_tx, reply_rx) = mpsc::channel();
        reply_tx
            .send(Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 109,
                reason: "uinput write failed".to_string(),
            }))
            .unwrap();

        let error = control
            .wait_for_reply_with_input_health(reply_rx, || {
                Some(SwitcherError::InputWorkerDisconnected {
                    worker: "input-target-watcher",
                })
            })
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 109,
                ref reason
            } if reason == "uinput write failed"
        ));
        assert!(!control.stop_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn writer_reply_is_not_visible_before_terminal_completion_is_committed() {
        let terminal_gate = Arc::new(Mutex::new(()));
        let control = WriterTransactionControl::new_with_terminal_gate(
            110,
            Duration::from_secs(1),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&terminal_gate),
        );
        let publisher_control = control.clone();
        let (reply_tx, reply_rx) = mpsc::channel();
        let held_gate = terminal_gate.lock().unwrap();
        let publisher = thread::spawn(move || {
            publish_writer_transaction_result(
                &publisher_control,
                reply_tx,
                Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                    request_id: 110,
                    reason: "uinput write failed".to_string(),
                }),
            )
        });

        let early_reply = reply_rx.recv_timeout(Duration::from_millis(50));
        drop(held_gate);
        let publisher_result = publisher.join().expect("publisher should not panic");

        assert!(
            matches!(early_reply, Err(mpsc::RecvTimeoutError::Timeout)),
            "reply must not become visible before terminal state can be committed"
        );
        assert!(matches!(
            publisher_result,
            Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 110,
                ..
            })
        ));
        assert!(matches!(
            reply_rx.recv_timeout(Duration::from_millis(100)),
            Ok(Err(SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 110,
                ..
            }))
        ));
        assert_eq!(control.state(), WriterTransactionState::Completed);
    }

    #[test]
    fn writer_stop_wins_terminal_race_against_late_completion() {
        let failure = Arc::new(AtomicU64::new(0));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let terminal_gate = Arc::new(Mutex::new(()));
        let control = WriterTransactionControl::new_with_writer_state(
            108,
            Duration::from_secs(1),
            failure,
            Arc::clone(&stop_requested),
            Arc::clone(&terminal_gate),
        );
        {
            let _terminal_guard = terminal_gate.lock().unwrap();
            stop_requested.store(true, Ordering::SeqCst);
        }

        assert!(!control.publish_completed());
        assert_eq!(control.state(), WriterTransactionState::Pending);
    }

    #[test]
    fn writer_stop_published_before_timeout_attempt_wins_without_poisoning_failure() {
        let failure = Arc::new(AtomicU64::new(0));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let terminal_gate = Arc::new(Mutex::new(()));
        let control = WriterTransactionControl::new_with_writer_state(
            110,
            Duration::from_secs(1),
            Arc::clone(&failure),
            Arc::clone(&stop_requested),
            Arc::clone(&terminal_gate),
        );
        {
            let _terminal_guard = terminal_gate.lock().unwrap();
            stop_requested.store(true, Ordering::SeqCst);
        }

        let error = control.mark_timed_out();

        assert_eq!(
            (
                control.state(),
                failure.load(Ordering::SeqCst),
                matches!(error, SwitcherError::VirtualKeyboardWriterDisconnected),
            ),
            (WriterTransactionState::Pending, 0, true),
            "stop that linearized first must remain the terminal outcome"
        );
    }

    #[test]
    fn writer_timeout_published_before_stop_remains_the_terminal_outcome() {
        let failure = Arc::new(AtomicU64::new(0));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let terminal_gate = Arc::new(Mutex::new(()));
        let control = WriterTransactionControl::new_with_writer_state(
            111,
            Duration::from_secs(1),
            Arc::clone(&failure),
            Arc::clone(&stop_requested),
            Arc::clone(&terminal_gate),
        );

        let timeout = control.mark_timed_out();
        {
            let _terminal_guard = terminal_gate.lock().unwrap();
            stop_requested.store(true, Ordering::SeqCst);
        }
        let terminal_outcome = control.cancellation_error();

        assert!(matches!(
            timeout,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 111 }
        ));
        assert_eq!(control.state(), WriterTransactionState::TimedOut);
        assert_eq!(failure.load(Ordering::SeqCst), 111);
        assert!(matches!(
            terminal_outcome,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 111 }
        ));
    }

    #[test]
    fn writer_failure_circuit_breaker_stops_next_mutation_after_timeout() {
        let failure = Arc::new(AtomicU64::new(0));
        let timed_out =
            WriterTransactionControl::new(104, Duration::from_secs(1), Arc::clone(&failure));
        let _ = timed_out.mark_timed_out();
        let mutation_started = Cell::new(false);

        let error = ensure_writer_not_failed(&failure)
            .and_then(|_| {
                mutation_started.set(true);
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 104 }
        ));
        assert!(!mutation_started.get());
    }

    #[test]
    fn writer_transaction_timeout_before_mutation_permit_prevents_emitter() {
        let control = WriterTransactionControl::with_timeout_for_test(105, Duration::from_secs(1));
        let timeout_control = control.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let timeout_barrier = Arc::clone(&barrier);
        let (timeout_done_tx, timeout_done_rx) = mpsc::channel();
        let timeout = thread::spawn(move || {
            timeout_barrier.wait();
            let error = timeout_control.mark_timed_out();
            timeout_done_tx.send(error).unwrap();
        });
        barrier.wait();
        let timeout_error = timeout_done_rx.recv().unwrap();
        timeout.join().unwrap();
        let mutation_started = Cell::new(false);

        let permit_error = control
            .authorize_mutation_start()
            .and_then(|_| {
                mutation_started.set(true);
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(
            timeout_error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 105 }
        ));
        assert!(matches!(
            permit_error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 105 }
        ));
        assert!(!mutation_started.get());
    }

    #[test]
    fn transaction_returns_worker_reply_result() {
        fn not_needed_outcome() -> CorrectionExecutionOutcome {
            CorrectionExecutionOutcome {
                layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
            }
        }

        fn run_with_reply(
            reply_result: Result<CorrectionExecutionOutcome, SwitcherError>,
        ) -> Result<CorrectionExecutionOutcome, SwitcherError> {
            let (handle, command_rx) = test_writer_handle(WRITER_QUEUE_CAPACITY, true);
            let should_fail_worker = reply_result.is_err();
            let worker = thread::spawn(move || {
                let command = command_rx
                    .recv()
                    .expect("transaction command should be sent");
                match command {
                    WriterCommand::Transaction(WriterTransaction::Execute {
                        control,
                        reply,
                        ..
                    }) => {
                        let publication =
                            publish_writer_transaction_result(&control, reply, reply_result);
                        assert_eq!(publication.is_err(), should_fail_worker);
                    }
                    _ => panic!("expected transaction command"),
                }
            });

            let result = handle.run_transaction(copy_shortcut_transaction());
            worker.join().expect("worker should finish");
            result
        }

        assert_eq!(
            run_with_reply(Ok(not_needed_outcome())).unwrap(),
            not_needed_outcome()
        );

        let error = run_with_reply(Err(SwitcherError::VirtualKeyboardWriterSaturated)).unwrap_err();
        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterSaturated
        ));
    }

    #[test]
    fn transaction_error_is_published_then_loop_stops_before_next_command() {
        let failure = Arc::new(AtomicU64::new(0));
        let terminal_gate = Arc::new(Mutex::new(()));
        let first_control = WriterTransactionControl::new_with_terminal_gate(
            501,
            Duration::from_secs(1),
            Arc::clone(&failure),
            Arc::clone(&terminal_gate),
        );
        let second_control = WriterTransactionControl::new_with_terminal_gate(
            502,
            Duration::from_secs(1),
            Arc::clone(&failure),
            terminal_gate,
        );
        let (first_reply_tx, first_reply_rx) = mpsc::channel();
        let (second_reply_tx, second_reply_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        command_tx
            .send(WriterCommand::Transaction(WriterTransaction::Execute {
                control: first_control.clone(),
                kind: copy_shortcut_transaction(),
                reply: first_reply_tx,
            }))
            .unwrap();
        command_tx
            .send(WriterCommand::Transaction(WriterTransaction::Execute {
                control: second_control.clone(),
                kind: copy_shortcut_transaction(),
                reply: second_reply_tx,
            }))
            .unwrap();
        drop(command_tx);
        let dispatched = Cell::new(0usize);

        let worker_error = run_writer_command_loop_with(command_rx, &failure, |command| {
            dispatched.set(dispatched.get() + 1);
            match command {
                WriterCommand::Transaction(WriterTransaction::Execute {
                    control, reply, ..
                }) => publish_writer_transaction_result(
                    &control,
                    reply,
                    Err(SwitcherError::Io(io::Error::other("post-mutation failure"))),
                ),
                _ => panic!("expected transaction command"),
            }
        })
        .unwrap_err();

        assert!(matches!(
            worker_error,
            SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 501,
                ..
            }
        ));
        assert_eq!(dispatched.get(), 1);
        assert_eq!(first_control.state(), WriterTransactionState::Completed);
        assert!(matches!(
            first_reply_rx.recv().unwrap(),
            Err(SwitcherError::Io(error)) if error.to_string() == "post-mutation failure"
        ));
        assert!(matches!(second_reply_rx.recv(), Err(mpsc::RecvError)));
        assert_eq!(second_control.state(), WriterTransactionState::Pending);
    }

    #[test]
    fn available_xtest_separator_runtime_error_does_not_fallback_or_dispatch_next_command() {
        let failure = AtomicU64::new(0);
        let (command_tx, command_rx) = mpsc::channel();
        for key in [Key::KEY_SPACE, Key::KEY_ENTER] {
            command_tx
                .send(WriterCommand::Fast(WriterFastCommand::TypeSeparator {
                    key,
                }))
                .unwrap();
        }
        drop(command_tx);
        let dispatched = Cell::new(0usize);
        let uinput_fallback_called = Cell::new(false);

        let error = run_writer_command_loop_with(command_rx, &failure, |_command| {
            dispatched.set(dispatched.get() + 1);
            finish_fast_separator_replay(
                Some(Err(SwitcherError::Io(io::Error::other(
                    "available xtest runtime failure",
                )))),
                || {
                    uinput_fallback_called.set(true);
                    Ok(())
                },
            )
        })
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::Io(error) if error.to_string() == "available xtest runtime failure"
        ));
        assert_eq!(dispatched.get(), 1);
        assert!(!uinput_fallback_called.get());
    }

    #[test]
    fn queued_writer_backlog_after_stop_executes_no_dispatch() {
        let failure = AtomicU64::new(0);
        let stop_requested = AtomicBool::new(true);
        let terminal_gate = Mutex::new(());
        let (command_tx, command_rx) = mpsc::channel();
        for key in [Key::KEY_SPACE, Key::KEY_ENTER] {
            command_tx
                .send(WriterCommand::Fast(WriterFastCommand::TypeSeparator {
                    key,
                }))
                .unwrap();
        }
        drop(command_tx);
        let dispatched = Cell::new(0usize);
        let ready_published = Cell::new(false);

        let error = run_writer_command_loop_with_stop(
            command_rx,
            &failure,
            &stop_requested,
            &terminal_gate,
            || {
                ready_published.set(true);
                Ok(())
            },
            |_command| {
                dispatched.set(dispatched.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterDisconnected
        ));
        assert!(!ready_published.get());
        assert_eq!(dispatched.get(), 0);
    }

    #[test]
    fn transaction_send_times_out_when_queue_stays_full() {
        let (handle, command_rx) = test_writer_handle(1, true);
        handle
            .command_tx
            .send(WriterCommand::Shutdown)
            .expect("pre-fill should succeed");

        let error = handle
            .run_transaction(copy_shortcut_transaction())
            .unwrap_err();
        drop(command_rx);

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterSaturated
        ));
    }

    #[test]
    fn transaction_enqueue_uses_own_deadline_before_saturation_window() {
        let (handle, command_rx) = test_writer_handle(1, true);
        handle
            .command_tx
            .send(WriterCommand::Shutdown)
            .expect("pre-fill should succeed");
        let started = Instant::now();

        let error = handle
            .run_transaction_with_timeout(copy_shortcut_transaction(), Duration::from_millis(5))
            .unwrap_err();
        drop(command_rx);

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id }
                if handle.transaction_failure_request_id() == Some(request_id)
        ));
        assert!(
            started.elapsed() < TRANSACTION_SEND_RETRY_WINDOW,
            "the request deadline must win before the fixed saturation window"
        );
    }

    #[test]
    fn transaction_enqueue_retry_observes_another_clone_failure() {
        let (handle, command_rx) = test_writer_handle(1, true);
        handle
            .command_tx
            .send(WriterCommand::Shutdown)
            .expect("pre-fill should succeed");
        let failure = Arc::clone(&handle.transaction_failure_request_id);
        let caller_handle = handle.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let caller = thread::spawn(move || {
            started_tx.send(()).unwrap();
            caller_handle.run_transaction(copy_shortcut_transaction())
        });
        started_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(5));
        failure.store(401, Ordering::SeqCst);

        let error = caller.join().unwrap().unwrap_err();
        drop(command_rx);

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 401 }
        ));
    }

    #[test]
    fn fast_command_returns_saturated_when_queue_stays_full() {
        let (handle, command_rx) = test_writer_handle(1, true);
        handle
            .command_tx
            .send(WriterCommand::Shutdown)
            .expect("pre-fill should succeed");

        let error = handle
            .send_fast_command(WriterFastCommand::TypeSeparator {
                key: Key::KEY_SPACE,
            })
            .unwrap_err();
        drop(command_rx);

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterSaturated
        ));
    }

    // Watcher readiness

    #[test]
    fn pointer_watcher_polling_interval_stays_within_idle_cpu_budget() {
        assert!(POINTER_POLL_INTERVAL >= Duration::from_millis(10));
        assert!(POINTER_POLL_INTERVAL <= Duration::from_millis(20));
    }

    #[test]
    fn pointer_click_classifier_accepts_only_physical_pointer_buttons() {
        for key in [
            Key::BTN_LEFT,
            Key::BTN_RIGHT,
            Key::BTN_MIDDLE,
            Key::BTN_SIDE,
            Key::BTN_EXTRA,
            Key::BTN_FORWARD,
            Key::BTN_BACK,
            Key::BTN_TASK,
        ] {
            assert!(is_pointer_click(key), "expected physical button: {key:?}");
        }
    }

    #[test]
    fn pointer_click_classifier_rejects_touch_tool_and_non_pointer_codes() {
        for key in [
            Key::BTN_TOUCH,
            Key::BTN_TOOL_FINGER,
            Key::BTN_TOOL_DOUBLETAP,
            Key::BTN_TOOL_PEN,
            Key::BTN_STYLUS,
            Key::BTN_0,
            Key::BTN_SOUTH,
        ] {
            assert!(
                !is_pointer_click(key),
                "must not be a pointer click: {key:?}"
            );
        }
    }

    #[test]
    fn x11_pointer_click_classifier_accepts_primary_middle_secondary_and_navigation() {
        for detail in [1, 2, 3, 8, 9] {
            assert!(is_x11_pointer_click(detail), "detail={detail}");
        }
    }

    #[test]
    fn x11_pointer_click_classifier_rejects_scroll_and_unknown_buttons() {
        for detail in [0, 4, 5, 6, 7, 10] {
            assert!(!is_x11_pointer_click(detail), "detail={detail}");
        }
    }

    #[test]
    fn pointer_click_invalidation_drains_both_sources() {
        let physical = AtomicBool::new(true);
        let logical = AtomicBool::new(true);

        assert!(take_pointer_click_flags(&physical, &logical));
        assert!(!physical.load(Ordering::SeqCst));
        assert!(!logical.load(Ordering::SeqCst));
        assert!(!take_pointer_click_flags(&physical, &logical));
    }

    #[test]
    fn xinput_pointer_click_mask_selects_raw_press_for_all_master_devices() {
        use x11rb::protocol::xinput::{Device, XIEventMask};

        let mask = xinput_pointer_click_event_mask();

        assert_eq!(mask.deviceid, u16::from(Device::ALL_MASTER));
        assert_eq!(mask.mask, vec![XIEventMask::RAW_BUTTON_PRESS]);
    }

    #[test]
    fn x11_pointer_click_event_accepts_emulated_primary_button() {
        use x11rb::protocol::xinput::{PointerEventFlags, RawButtonPressEvent};
        use x11rb::protocol::Event;

        let event = Event::XinputRawButtonPress(RawButtonPressEvent {
            detail: 1,
            flags: PointerEventFlags::POINTER_EMULATED,
            ..RawButtonPressEvent::default()
        });

        assert_eq!(
            x11_pointer_click_event(&event),
            Some(X11ContextEvent::PointerClick { detail: 1 })
        );
    }

    #[test]
    fn x11_pointer_click_event_rejects_scroll_and_non_pointer_events() {
        use x11rb::protocol::xinput::RawButtonPressEvent;
        use x11rb::protocol::xproto::KeyPressEvent;
        use x11rb::protocol::Event;

        let scroll = Event::XinputRawButtonPress(RawButtonPressEvent {
            detail: 4,
            ..RawButtonPressEvent::default()
        });
        let keyboard = Event::KeyPress(KeyPressEvent::default());

        assert_eq!(x11_pointer_click_event(&scroll), None);
        assert_eq!(x11_pointer_click_event(&keyboard), None);
    }

    #[test]
    fn x11_context_events_set_only_the_matching_invalidation_flag() {
        let changed = AtomicBool::new(false);
        let pointer_click = AtomicBool::new(false);

        publish_x11_context_event(
            X11ContextEvent::ActiveWindowChanged {
                previous: Some(1),
                current: Some(2),
            },
            &changed,
            &pointer_click,
        );
        assert!(changed.swap(false, Ordering::SeqCst));
        assert!(!pointer_click.load(Ordering::SeqCst));

        publish_x11_context_event(
            X11ContextEvent::PointerClick { detail: 1 },
            &changed,
            &pointer_click,
        );
        assert!(!changed.load(Ordering::SeqCst));
        assert!(pointer_click.swap(false, Ordering::SeqCst));
    }

    #[test]
    fn input_target_logical_click_participates_in_combined_drain() {
        let pointer_watcher = PointerWatcher {
            click_flag: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
            required: false,
            handle: None,
        };
        let logical_click = Arc::new(AtomicBool::new(true));
        let input_target_watcher = InputTargetWatcher {
            changed_flag: Arc::new(AtomicBool::new(false)),
            pointer_click_flag: Arc::clone(&logical_click),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
            required: false,
            handle: None,
            stop_wakeup: None,
        };

        assert!(take_pointer_click_flags(
            &pointer_watcher.click_flag,
            &input_target_watcher.pointer_click_flag,
        ));
        assert!(!logical_click.load(Ordering::SeqCst));
    }

    #[test]
    fn pointer_watcher_with_no_discovered_devices_is_disabled_and_ready() {
        let watcher = PointerWatcher::spawn(Vec::new()).unwrap();

        assert!(watcher.is_ready());
    }

    #[test]
    fn pointer_watcher_with_no_readable_devices_is_disabled_without_false_worker_ready() {
        let watcher = PointerWatcher::spawn(vec![PathBuf::from(
            "/openswitcher-test/nonexistent-pointer-device",
        )])
        .unwrap();

        assert!(!watcher.required);
        assert!(!watcher.alive.load(Ordering::SeqCst));
        assert!(watcher.is_ready());
    }

    #[test]
    fn pointer_poll_cycle_stops_after_last_open_device_is_lost() {
        let mut devices = vec!["touchpad", "mouse"];
        let polled = Cell::new(0usize);

        let keep_running = retain_available_pointer_devices(&mut devices, |_| {
            polled.set(polled.get() + 1);
            false
        });

        assert_eq!(polled.get(), 2, "removal must not skip the next device");
        assert!(!keep_running);
        assert!(devices.is_empty());
    }

    #[test]
    fn pointer_watcher_readiness_is_false_when_required_thread_is_dead() {
        let watcher = PointerWatcher {
            click_flag: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
            required: true,
            handle: None,
        };

        assert!(!watcher.is_ready());
    }

    #[test]
    fn pointer_watcher_readiness_is_true_immediately_after_successful_spawn() {
        let watcher = PointerWatcher {
            click_flag: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(true)),
            required: true,
            handle: None,
        };

        assert!(watcher.is_ready());
    }

    #[test]
    fn input_target_stop_signal_wakes_idle_waiter() {
        use crate::daemon::x11_wait::{wait_for_x11_or_stop, X11WaitOutcome};
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        let (stop_wakeup, stop_reader) = input_target_stop_wakeup_pair().unwrap();
        let (x11_reader, _x11_writer) = UnixStream::pair().unwrap();

        signal_input_target_stop(Some(&stop_wakeup));

        assert_eq!(
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap(),
            X11WaitOutcome::StopRequested
        );
    }

    #[test]
    fn repeated_input_target_stop_is_idempotent() {
        use crate::daemon::x11_wait::{wait_for_x11_or_stop, X11WaitOutcome};
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        let (stop_wakeup, stop_reader) = input_target_stop_wakeup_pair().unwrap();
        let (x11_reader, _x11_writer) = UnixStream::pair().unwrap();

        signal_input_target_stop(Some(&stop_wakeup));
        signal_input_target_stop(Some(&stop_wakeup));

        assert_eq!(
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap(),
            X11WaitOutcome::StopRequested
        );
    }

    #[test]
    fn buffered_x11_events_are_drained_before_fd_wait() {
        use crate::daemon::x11_wait::X11WaitOutcome;
        use std::cell::RefCell;

        let stop_requested = AtomicBool::new(false);
        let next_calls = Cell::new(0);
        let order = RefCell::new(Vec::new());

        let keep_running = run_x11_event_cycle(
            &stop_requested,
            || {
                order.borrow_mut().push("next");
                let call = next_calls.get();
                next_calls.set(call + 1);
                Ok(if call == 0 { Some(7) } else { None })
            },
            |event| {
                assert_eq!(event, 7);
                order.borrow_mut().push("handle");
            },
            || {
                order.borrow_mut().push("wait");
                Ok(X11WaitOutcome::X11Ready)
            },
        )
        .unwrap();

        assert!(keep_running);
        assert_eq!(*order.borrow(), ["next", "handle", "next", "wait"]);
    }

    #[test]
    fn stop_observed_after_drain_skips_fd_wait() {
        use crate::daemon::x11_wait::X11WaitOutcome;
        use std::cell::RefCell;

        let stop_requested = AtomicBool::new(false);
        let next_calls = Cell::new(0);
        let wait_called = Cell::new(false);
        let order = RefCell::new(Vec::new());

        let keep_running = run_x11_event_cycle(
            &stop_requested,
            || {
                order.borrow_mut().push("next");
                let call = next_calls.get();
                next_calls.set(call + 1);
                Ok(if call == 0 { Some(7) } else { None })
            },
            |event| {
                assert_eq!(event, 7);
                order.borrow_mut().push("handle");
                stop_requested.store(true, Ordering::SeqCst);
            },
            || {
                wait_called.set(true);
                Ok(X11WaitOutcome::X11Ready)
            },
        )
        .unwrap();

        assert!(!keep_running);
        assert!(!wait_called.get());
        assert_eq!(*order.borrow(), ["next", "handle", "next"]);
    }

    #[test]
    fn input_target_watcher_readiness_is_true_when_disabled_by_policy() {
        let watcher = InputTargetWatcher {
            changed_flag: Arc::new(AtomicBool::new(false)),
            pointer_click_flag: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
            required: false,
            handle: None,
            stop_wakeup: None,
        };

        assert!(watcher.is_ready());
    }

    #[test]
    fn x11_input_target_connection_failure_is_recoverable_worker_failure() {
        let error = prepare_input_target_monitor(SessionType::X11, || {
            Err::<u8, _>(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "test X11 endpoint unavailable",
            ))
        })
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkerDisconnected {
                worker: "input-target-watcher"
            }
        ));
    }

    #[test]
    fn non_x11_input_target_does_not_attempt_x11_connection() {
        let connect_called = Cell::new(false);
        let monitor = prepare_input_target_monitor(SessionType::Wayland, || {
            connect_called.set(true);
            Ok::<_, io::Error>(7u8)
        })
        .unwrap();

        assert_eq!(monitor, None);
        assert!(!connect_called.get());
    }

    #[test]
    fn disabled_input_target_watcher_object_is_ready() {
        let watcher = InputTargetWatcher::disabled(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );

        assert!(watcher.is_ready());
    }

    #[test]
    fn input_target_watcher_readiness_is_false_when_required_thread_is_dead() {
        let watcher = InputTargetWatcher {
            changed_flag: Arc::new(AtomicBool::new(false)),
            pointer_click_flag: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
            required: true,
            handle: None,
            stop_wakeup: None,
        };

        assert!(!watcher.is_ready());
    }

    #[test]
    fn input_target_watcher_readiness_is_true_immediately_after_successful_spawn() {
        let watcher = InputTargetWatcher {
            changed_flag: Arc::new(AtomicBool::new(false)),
            pointer_click_flag: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(true)),
            required: true,
            handle: None,
            stop_wakeup: None,
        };

        assert!(watcher.is_ready());
    }

    // Deferred manual current-word writer

    #[test]
    fn deferred_manual_correction_without_completion_times_out_and_fails_writer() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (_completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let started = Instant::now();

        let outcome = writer
            .begin_manual_current_word_correction_with_timeout(
                CorrectionPlan {
                    buffer: Vec::new(),
                    extra_backspaces: 0,
                },
                test_runtime_config_snapshot(),
                ModifierState::default(),
                Duration::from_millis(30),
            )
            .expect("accepted deferred command should return immediately");
        let request_id = match outcome {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            other => panic!("expected accepted deferred command, got {other:?}"),
        };
        let control = match command_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("deferred command should be enqueued")
        {
            WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => control,
            _ => panic!("expected deferred correction command"),
        };

        let error = loop {
            match writer.poll_manual_current_word_completion() {
                Ok(None) if started.elapsed() < Duration::from_millis(250) => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => break error,
                other => panic!("deferred timeout should fail writer, got {other:?}"),
            }
        };

        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut {
                request_id: failed_request_id
            } if failed_request_id == request_id
        ));
        assert_eq!(control.state(), WriterTransactionState::TimedOut);
        assert_eq!(
            writer.handle.transaction_failure_request_id(),
            Some(request_id)
        );
    }

    #[test]
    fn deferred_manual_correction_rejects_oversized_plan_before_enqueue() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (_completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let plan = match oversized_correction_transaction() {
            WriterTransactionKind::ApplyCorrection { plan, .. } => plan,
            _ => unreachable!(),
        };

        let error = writer
            .begin_manual_current_word_correction(
                plan,
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkValidation(
                crate::error::ValidationError::InputCorrectionPlanTooLarge { .. }
            )
        ));
        assert!(matches!(
            command_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(writer.pending_manual_current_word.is_none());
    }

    #[test]
    fn completed_deferred_success_is_consumed_without_false_health_error() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let plan = CorrectionPlan {
            buffer: Vec::new(),
            extra_backspaces: 0,
        };
        let request_id = match writer
            .begin_manual_current_word_correction(
                plan.clone(),
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .unwrap()
        {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            other => panic!("expected deferred command, got {other:?}"),
        };
        let control = match command_rx.recv().unwrap() {
            WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => control,
            _ => panic!("expected deferred correction command"),
        };

        completion_tx
            .send(ManualCurrentWordCompletion {
                request_id,
                outcome: ManualCurrentWordOutcome::Succeeded(plan),
            })
            .unwrap();
        assert!(control.publish_completed());
        assert!(
            writer.health_error().is_none(),
            "published success must remain pollable instead of becoming a generic health error"
        );

        let completion = writer
            .poll_manual_current_word_completion()
            .unwrap()
            .expect("published completion should be visible");
        assert_eq!(completion.request_id, request_id);
        assert!(writer.pending_manual_current_word.is_none());
    }

    #[test]
    fn deferred_poll_accepts_completion_published_after_pending_state_snapshot() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let plan = CorrectionPlan {
            buffer: Vec::new(),
            extra_backspaces: 0,
        };
        let request_id = match writer
            .begin_manual_current_word_correction(
                plan.clone(),
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .unwrap()
        {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            other => panic!("expected deferred command, got {other:?}"),
        };
        let control = match command_rx.recv().unwrap() {
            WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => control,
            _ => panic!("expected deferred correction command"),
        };
        let expected = ManualCurrentWordCompletion {
            request_id,
            outcome: ManualCurrentWordOutcome::Succeeded(plan),
        };
        completion_tx.send(expected.clone()).unwrap();

        let (pending_observed_tx, pending_observed_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let poller = thread::spawn(move || {
            install_deferred_poll_before_pending_active_check_hook(move || {
                pending_observed_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            });
            writer.poll_manual_current_word_completion()
        });

        pending_observed_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("poll must pause after observing Pending");
        assert!(control.publish_completed());
        resume_tx.send(()).unwrap();

        let completion = poller
            .join()
            .expect("poll thread should finish")
            .expect("published completion must not become an active-state error");
        assert_eq!(completion, Some(expected));
    }

    #[test]
    fn deferred_completed_result_wins_over_concurrent_writer_death_health() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::clone(&alive),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let plan = CorrectionPlan {
            buffer: Vec::new(),
            extra_backspaces: 0,
        };
        let request_id = match writer
            .begin_manual_current_word_correction(
                plan.clone(),
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .unwrap()
        {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            other => panic!("expected deferred command, got {other:?}"),
        };
        let control = match command_rx.recv().unwrap() {
            WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => control,
            _ => panic!("expected deferred correction command"),
        };

        let (pending_checked_tx, pending_checked_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let health_checker = thread::spawn(move || {
            install_deferred_health_before_handle_check_hook(move || {
                pending_checked_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            });
            writer.health_error()
        });

        pending_checked_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("health check must pause after validating Pending");
        completion_tx
            .send(ManualCurrentWordCompletion {
                request_id,
                outcome: ManualCurrentWordOutcome::Succeeded(plan),
            })
            .unwrap();
        assert!(control.publish_completed());
        alive.store(false, Ordering::SeqCst);
        resume_tx.send(()).unwrap();

        let health_error = health_checker.join().expect("health thread should finish");
        assert!(
            health_error.is_none(),
            "valid Completed must win over the writer-dead health observed afterward"
        );
    }

    #[test]
    fn deferred_failure_completion_precedes_worker_dead_health() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::clone(&alive),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };
        let request_id = match writer
            .begin_manual_current_word_correction(
                CorrectionPlan {
                    buffer: Vec::new(),
                    extra_backspaces: 0,
                },
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .unwrap()
        {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            other => panic!("expected deferred command, got {other:?}"),
        };
        let control = match command_rx.recv().unwrap() {
            WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => control,
            _ => panic!("expected deferred correction command"),
        };
        completion_tx
            .send(ManualCurrentWordCompletion {
                request_id,
                outcome: ManualCurrentWordOutcome::FailedAfterMutation(
                    "post-mutation failure".to_string(),
                ),
            })
            .unwrap();
        assert!(control.publish_completed());
        alive.store(false, Ordering::SeqCst);

        let completion = writer
            .poll_manual_current_word_completion()
            .unwrap()
            .expect("terminal deferred completion must win over dead health once");

        assert_eq!(completion.request_id, request_id);
        assert_eq!(
            completion.outcome,
            ManualCurrentWordOutcome::FailedAfterMutation("post-mutation failure".to_string())
        );
        assert!(writer.pending_manual_current_word.is_none());
        assert!(matches!(
            writer.health_error(),
            Some(SwitcherError::VirtualKeyboardWriterDisconnected)
        ));
    }

    #[test]
    fn deferred_error_is_published_then_loop_stops_before_next_command() {
        let failure = Arc::new(AtomicU64::new(0));
        let terminal_gate = Arc::new(Mutex::new(()));
        let first_control = WriterTransactionControl::new_with_terminal_gate(
            601,
            Duration::from_secs(1),
            Arc::clone(&failure),
            Arc::clone(&terminal_gate),
        );
        let second_control = WriterTransactionControl::new_with_terminal_gate(
            602,
            Duration::from_secs(1),
            Arc::clone(&failure),
            terminal_gate,
        );
        let (command_tx, command_rx) = mpsc::channel();
        for control in [first_control.clone(), second_control.clone()] {
            command_tx
                .send(WriterCommand::DeferredManualCurrentWordCorrection {
                    control,
                    plan: CorrectionPlan {
                        buffer: Vec::new(),
                        extra_backspaces: 0,
                    },
                    config: test_runtime_config_snapshot(),
                    modifiers: ModifierState::default(),
                })
                .unwrap();
        }
        drop(command_tx);
        let (completion_tx, completion_rx) = mpsc::channel();
        let dispatched = Cell::new(0usize);

        let worker_error = run_writer_command_loop_with(command_rx, &failure, |command| {
            dispatched.set(dispatched.get() + 1);
            match command {
                WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => {
                    publish_deferred_manual_completion(
                        &control,
                        &completion_tx,
                        ManualCurrentWordCompletion {
                            request_id: control.request_id(),
                            outcome: ManualCurrentWordOutcome::FailedAfterMutation(
                                "post-mutation failure".to_string(),
                            ),
                        },
                        Some("post-mutation failure".to_string()),
                    )
                }
                _ => panic!("expected deferred command"),
            }
        })
        .unwrap_err();

        assert!(matches!(
            worker_error,
            SwitcherError::VirtualKeyboardWriterTransactionFailed {
                request_id: 601,
                ..
            }
        ));
        assert_eq!(dispatched.get(), 1);
        assert_eq!(first_control.state(), WriterTransactionState::Completed);
        assert_eq!(second_control.state(), WriterTransactionState::Pending);
        assert_eq!(
            completion_rx.recv().unwrap(),
            ManualCurrentWordCompletion {
                request_id: 601,
                outcome: ManualCurrentWordOutcome::FailedAfterMutation(
                    "post-mutation failure".to_string()
                ),
            }
        );
    }

    #[test]
    fn begin_manual_current_word_correction_returns_request_id_immediately() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (_completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                let _ = command_rx.recv();
            })),
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let outcome = writer
            .begin_manual_current_word_correction(
                CorrectionPlan {
                    buffer: Vec::new(),
                    extra_backspaces: 0,
                },
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .expect("begin should return immediately");

        assert_eq!(outcome, ManualCurrentWordStartOutcome::Started(1));
    }

    #[test]
    fn begin_manual_current_word_correction_rejects_before_mutation_when_queue_is_full() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_completion_tx, completion_rx) = mpsc::channel();
        command_tx
            .send(WriterCommand::Shutdown)
            .expect("pre-fill should succeed");
        let keep_receiver_alive = Arc::new(AtomicBool::new(true));
        let worker_alive = Arc::clone(&keep_receiver_alive);
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: Some(thread::spawn(move || {
                while worker_alive.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
                drop(command_rx);
            })),
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let outcome = writer
            .begin_manual_current_word_correction(
                CorrectionPlan {
                    buffer: Vec::new(),
                    extra_backspaces: 0,
                },
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .expect("begin should reject, not fail fatally");

        assert_eq!(
            outcome,
            ManualCurrentWordStartOutcome::RejectedBeforeMutation(
                "virtual-keyboard-writer-saturated".to_string()
            )
        );

        keep_receiver_alive.store(false, Ordering::SeqCst);
        writer.stop();
    }

    #[test]
    fn poll_manual_current_word_completion_returns_none_before_worker_finishes() {
        let (_command_tx, _command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (_completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx: _command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 1,
            pending_manual_current_word: None,
        };

        let completion = writer
            .poll_manual_current_word_completion()
            .expect("poll should not fail");

        assert!(completion.is_none());
    }

    #[test]
    fn poll_manual_current_word_completion_waits_for_completed_publication() {
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let mut writer = VirtualKeyboardWriter {
            handle: VirtualKeyboardHandle {
                command_tx,
                alive: Arc::new(AtomicBool::new(true)),
                stop_requested: Arc::new(AtomicBool::new(false)),
                transaction_failure_request_id: Arc::new(AtomicU64::new(0)),
                transaction_terminal_gate: Arc::new(Mutex::new(())),
            },
            join_handle: None,
            exit_rx: mpsc::channel().1,
            shutdown_started_at: None,
            shutdown_outcome: None,
            completion_rx,
            next_request_id: 7,
            pending_manual_current_word: None,
        };
        let plan = CorrectionPlan {
            buffer: Vec::new(),
            extra_backspaces: 0,
        };
        let request_id = match writer
            .begin_manual_current_word_correction(
                plan.clone(),
                test_runtime_config_snapshot(),
                ModifierState::default(),
            )
            .unwrap()
        {
            ManualCurrentWordStartOutcome::Started(request_id) => request_id,
            other => panic!("expected deferred command, got {other:?}"),
        };
        let control = match command_rx.recv().unwrap() {
            WriterCommand::DeferredManualCurrentWordCorrection { control, .. } => control,
            _ => panic!("expected deferred correction command"),
        };

        completion_tx
            .send(ManualCurrentWordCompletion {
                request_id,
                outcome: ManualCurrentWordOutcome::Succeeded(plan),
            })
            .expect("completion send should succeed");

        assert!(writer
            .poll_manual_current_word_completion()
            .expect("poll should succeed")
            .is_none());
        assert!(control.publish_completed());
        let completion = writer
            .poll_manual_current_word_completion()
            .expect("poll should succeed")
            .expect("completed reply should be ready");

        assert_eq!(completion.request_id, request_id);
        assert!(matches!(
            completion.outcome,
            ManualCurrentWordOutcome::Succeeded(_)
        ));
        assert!(writer.pending_manual_current_word.is_none());
        assert_eq!(writer.handle.transaction_failure_request_id(), None);
    }
}
