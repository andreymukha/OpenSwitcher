use crate::daemon::keyboard::{
    log_input_debug, resolve_error_after_writer_shutdown, InputBackendReadiness,
    KeyboardController, SharedModifierState, WriterShutdownOutcome,
};
use crate::daemon::selected_text::SelectedTextJobRunner;
use crate::error::SwitcherError;
use std::time::{Duration, Instant};

const INITIAL_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
const STEADY_RETRY_DELAY: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputBackendState {
    WaitingForInputAccess,
    Recovering,
    Ready,
}

fn runtime_failure_recovery_state(error: &SwitcherError) -> Option<InputBackendState> {
    match error {
        SwitcherError::KeyboardNotFound
        | SwitcherError::KeyboardAccessDenied { .. }
        | SwitcherError::UinputAccessDenied { .. } => {
            Some(InputBackendState::WaitingForInputAccess)
        }
        SwitcherError::InputWorkerDisconnected { .. } => Some(InputBackendState::Recovering),
        SwitcherError::Io(io_error)
            if matches!(io_error.raw_os_error(), Some(19))
                || io_error.to_string().contains("No such device") =>
        {
            Some(InputBackendState::Recovering)
        }
        _ => None,
    }
}

pub trait InputBackendHandle {
    fn shutdown(&mut self) -> WriterShutdownOutcome;
}

pub struct OpenedInputBackend<B: InputBackendHandle> {
    pub backend: B,
    pub readiness: InputBackendReadiness,
}

pub trait InputBackendOpener {
    type Backend: InputBackendHandle;

    fn reopen_backend(
        &self,
        shared_modifiers: SharedModifierState,
    ) -> Result<OpenedInputBackend<Self::Backend>, SwitcherError>;
}

pub struct ActiveInputBackend {
    pub keyboard: KeyboardController,
    pub selected_text_runner: SelectedTextJobRunner,
    pub initial_caps_lock_active: bool,
}

impl InputBackendHandle for ActiveInputBackend {
    fn shutdown(&mut self) -> WriterShutdownOutcome {
        self.keyboard.shutdown()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyboardInputBackendOpener;

#[cfg(test)]
fn finish_prepared_input_backend<Prepared, Active, Dependent, Error>(
    prepared: Prepared,
    prepare_dependent: impl FnOnce(&Prepared) -> Result<Dependent, Error>,
    activate: impl FnOnce(Prepared, &Dependent) -> Result<Active, Error>,
) -> Result<(Active, Dependent), Error> {
    let dependent = prepare_dependent(&prepared)?;
    let active = activate(prepared, &dependent)?;
    Ok((active, dependent))
}

fn shutdown_backend_after_error(
    backend: &mut impl InputBackendHandle,
    error: SwitcherError,
    phase: &'static str,
) -> SwitcherError {
    let outcome = backend.shutdown();
    resolve_error_after_writer_shutdown(error, phase, outcome)
}

impl InputBackendOpener for KeyboardInputBackendOpener {
    type Backend = ActiveInputBackend;

    fn reopen_backend(
        &self,
        shared_modifiers: SharedModifierState,
    ) -> Result<OpenedInputBackend<Self::Backend>, SwitcherError> {
        let mut prepared_keyboard = KeyboardController::prepare()?;
        let selected_text_runner = match SelectedTextJobRunner::new(
            prepared_keyboard.selection_transport(shared_modifiers),
        ) {
            Ok(runner) => runner,
            Err(error) => {
                let outcome = prepared_keyboard.shutdown();
                return Err(resolve_error_after_writer_shutdown(
                    error,
                    "backend-open",
                    outcome,
                ));
            }
        };
        if let Err(error) = selected_text_runner.ensure_ready() {
            drop(selected_text_runner);
            let outcome = prepared_keyboard.shutdown();
            return Err(resolve_error_after_writer_shutdown(
                error,
                "backend-open",
                outcome,
            ));
        }
        let (keyboard, initial_caps_lock_active) = match prepared_keyboard.activate() {
            Ok(active) => active,
            Err(error) => {
                drop(selected_text_runner);
                return Err(error);
            }
        };
        let mut backend = ActiveInputBackend {
            keyboard,
            selected_text_runner,
            initial_caps_lock_active,
        };
        if let Err(error) = backend.selected_text_runner.ensure_ready() {
            return Err(shutdown_backend_after_error(
                &mut backend,
                error,
                "backend-open",
            ));
        }
        let readiness = backend.keyboard.readiness();

        Ok(OpenedInputBackend { backend, readiness })
    }
}

#[derive(Clone, Debug)]
struct LatchedWriterShutdownFailure {
    timeout_ms: u64,
    phase: &'static str,
    trigger: String,
}

impl LatchedWriterShutdownFailure {
    fn to_error(&self) -> SwitcherError {
        SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
            timeout_ms: self.timeout_ms,
            phase: self.phase,
            trigger: self.trigger.clone(),
        }
    }
}

#[derive(Debug)]
pub struct InputBackendLifecycle<O: InputBackendOpener> {
    opener: O,
    state: InputBackendState,
    retry_attempt: usize,
    retry_deadline: Option<Instant>,
    last_error: Option<String>,
    writer_fail_stop: Option<LatchedWriterShutdownFailure>,
}

impl<O: InputBackendOpener> InputBackendLifecycle<O> {
    pub fn new(opener: O) -> Self {
        Self {
            opener,
            state: InputBackendState::WaitingForInputAccess,
            retry_attempt: 0,
            retry_deadline: Some(Instant::now()),
            last_error: None,
            writer_fail_stop: None,
        }
    }

    pub fn state(&self) -> InputBackendState {
        self.state
    }

    pub fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn mark_backend_ready(&mut self, readiness: InputBackendReadiness) {
        if self.writer_fail_stop.is_none() && readiness.is_ready() {
            self.transition_to_ready();
        }
    }

    pub fn initialize(
        &mut self,
        shared_modifiers: SharedModifierState,
        now: Instant,
    ) -> Result<Option<OpenedInputBackend<O::Backend>>, SwitcherError> {
        self.try_reopen("startup", shared_modifiers, now)
    }

    pub fn try_recover(
        &mut self,
        shared_modifiers: SharedModifierState,
        now: Instant,
    ) -> Result<Option<OpenedInputBackend<O::Backend>>, SwitcherError> {
        if let Some(failure) = &self.writer_fail_stop {
            return Err(failure.to_error());
        }
        if self.state == InputBackendState::Ready || !self.retry_due(now) {
            return Ok(None);
        }

        self.try_reopen("background-retry", shared_modifiers, now)
    }

    pub fn record_startup_failure(&mut self, error: &SwitcherError, now: Instant) {
        if self.writer_fail_stop.is_none() && error.is_recoverable_input_error() {
            self.transition_with_retry(InputBackendState::WaitingForInputAccess, error, now);
        }
    }

    pub fn can_recover_runtime_failure(&self, error: &SwitcherError) -> bool {
        self.writer_fail_stop.is_none() && runtime_failure_recovery_state(error).is_some()
    }

    pub fn record_runtime_failure(&mut self, error: &SwitcherError, now: Instant) -> bool {
        if self.writer_fail_stop.is_some() {
            return false;
        }
        let next_state = runtime_failure_recovery_state(error);

        if let Some(next_state) = next_state {
            self.transition_with_retry(next_state, error, now);
            return true;
        }

        false
    }

    pub fn retry_due(&self, now: Instant) -> bool {
        self.writer_fail_stop.is_none()
            && self.retry_deadline.is_some_and(|deadline| now >= deadline)
    }

    fn try_reopen(
        &mut self,
        phase: &'static str,
        shared_modifiers: SharedModifierState,
        now: Instant,
    ) -> Result<Option<OpenedInputBackend<O::Backend>>, SwitcherError> {
        if let Some(failure) = &self.writer_fail_stop {
            return Err(failure.to_error());
        }

        match self.opener.reopen_backend(shared_modifiers) {
            Ok(mut opened) => {
                if opened.readiness.is_ready() {
                    self.transition_to_ready();
                    return Ok(Some(opened));
                }

                let shutdown_outcome = opened.backend.shutdown();
                log_input_debug(
                    "input-backend-transition",
                    &format!(
                        "phase={phase} previous_state={:?} next_state={:?} result=skipped reason=incomplete-readiness readiness={:?}",
                        self.state,
                        self.state,
                        opened.readiness,
                    ),
                );
                let trigger = "input backend readiness incomplete".to_string();
                match shutdown_outcome {
                    WriterShutdownOutcome::Stopped => {
                        self.schedule_retry_with_reason(trigger, now);
                        Ok(None)
                    }
                    WriterShutdownOutcome::Unresponsive { timeout_ms } => {
                        Err(self.latch_writer_fail_stop(timeout_ms, "backend-readiness", trigger))
                    }
                }
            }
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms,
                phase,
                trigger,
            }) => Err(self.latch_writer_fail_stop(timeout_ms, phase, trigger)),
            Err(error) if error.is_recoverable_input_error() => {
                log_input_debug(
                    "input-backend-transition",
                    &format!(
                        "phase={phase} previous_state={:?} next_state={:?} result=skipped reason={error}",
                        self.state,
                        self.state,
                    ),
                );
                self.schedule_retry_with_reason(error.to_string(), now);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn transition_to_ready(&mut self) {
        debug_assert!(self.writer_fail_stop.is_none());
        self.state = InputBackendState::Ready;
        self.retry_attempt = 0;
        self.retry_deadline = None;
        self.last_error = None;
    }

    fn transition_with_retry(
        &mut self,
        next_state: InputBackendState,
        error: &SwitcherError,
        now: Instant,
    ) {
        log_input_debug(
            "input-backend-transition",
            &format!(
                "previous_state={:?} next_state={next_state:?} result=applied reason={error}",
                self.state
            ),
        );
        self.state = next_state;
        self.schedule_retry_with_reason(error.to_string(), now);
    }

    fn schedule_retry_with_reason(&mut self, reason: String, now: Instant) {
        self.last_error = Some(reason);
        self.retry_deadline = Some(now + retry_delay_for_attempt(self.retry_attempt));
        self.retry_attempt += 1;
    }

    fn latch_writer_fail_stop(
        &mut self,
        timeout_ms: u64,
        phase: &'static str,
        trigger: String,
    ) -> SwitcherError {
        let failure = LatchedWriterShutdownFailure {
            timeout_ms,
            phase,
            trigger,
        };
        let error = failure.to_error();
        log_input_debug(
            "input-backend-transition",
            &format!(
                "phase={phase} previous_state={:?} next_state={:?} result=process-fail-stop error={error}",
                self.state, self.state,
            ),
        );
        self.retry_deadline = None;
        self.last_error = Some(error.to_string());
        self.writer_fail_stop = Some(failure);
        error
    }
}

fn retry_delay_for_attempt(attempt: usize) -> Duration {
    INITIAL_RETRY_DELAYS
        .get(attempt)
        .copied()
        .unwrap_or(STEADY_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::keyboard::WriterShutdownOutcome;
    use crate::error::SwitcherError;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    #[derive(Clone, Debug)]
    struct FakeBackend {
        active_backends: Option<Rc<RefCell<usize>>>,
        shutdowns: Rc<RefCell<usize>>,
        shutdown_outcome: WriterShutdownOutcome,
    }

    impl InputBackendHandle for FakeBackend {
        fn shutdown(&mut self) -> WriterShutdownOutcome {
            *self.shutdowns.borrow_mut() += 1;
            self.shutdown_outcome
        }
    }

    impl Drop for FakeBackend {
        fn drop(&mut self) {
            if let Some(active_backends) = &self.active_backends {
                let mut active_backends = active_backends.borrow_mut();
                *active_backends = active_backends.saturating_sub(1);
            }
        }
    }

    enum FakeOutcome {
        Ok {
            shutdowns: Rc<RefCell<usize>>,
            readiness: InputBackendReadiness,
            shutdown_outcome: WriterShutdownOutcome,
        },
        OkTracked {
            active_backends: Rc<RefCell<usize>>,
            shutdowns: Rc<RefCell<usize>>,
            readiness: InputBackendReadiness,
            shutdown_outcome: WriterShutdownOutcome,
        },
        OkCounted {
            opens: Rc<RefCell<usize>>,
            shutdowns: Rc<RefCell<usize>>,
            readiness: InputBackendReadiness,
            shutdown_outcome: WriterShutdownOutcome,
        },
        KeyboardAccessDenied,
    }

    struct FakeOpener {
        outcome: FakeOutcome,
    }

    // Test helpers

    impl InputBackendOpener for FakeOpener {
        type Backend = FakeBackend;

        fn reopen_backend(
            &self,
            _shared_modifiers: SharedModifierState,
        ) -> Result<OpenedInputBackend<Self::Backend>, SwitcherError> {
            match &self.outcome {
                FakeOutcome::Ok {
                    shutdowns,
                    readiness,
                    shutdown_outcome,
                } => Ok(OpenedInputBackend {
                    backend: FakeBackend {
                        active_backends: None,
                        shutdowns: shutdowns.clone(),
                        shutdown_outcome: *shutdown_outcome,
                    },
                    readiness: *readiness,
                }),
                FakeOutcome::OkTracked {
                    active_backends,
                    shutdowns,
                    readiness,
                    shutdown_outcome,
                } => {
                    *active_backends.borrow_mut() += 1;
                    Ok(OpenedInputBackend {
                        backend: FakeBackend {
                            active_backends: Some(active_backends.clone()),
                            shutdowns: shutdowns.clone(),
                            shutdown_outcome: *shutdown_outcome,
                        },
                        readiness: *readiness,
                    })
                }
                FakeOutcome::OkCounted {
                    opens,
                    shutdowns,
                    readiness,
                    shutdown_outcome,
                } => {
                    *opens.borrow_mut() += 1;
                    Ok(OpenedInputBackend {
                        backend: FakeBackend {
                            active_backends: None,
                            shutdowns: shutdowns.clone(),
                            shutdown_outcome: *shutdown_outcome,
                        },
                        readiness: *readiness,
                    })
                }
                FakeOutcome::KeyboardAccessDenied => Err(SwitcherError::KeyboardAccessDenied {
                    path: PathBuf::from("/dev/input/event3"),
                    source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                }),
            }
        }
    }

    fn ready_readiness() -> InputBackendReadiness {
        InputBackendReadiness {
            keyboard_open: true,
            writer_ready: true,
            watchers_ready: true,
            event_processing_ready: true,
        }
    }

    fn incomplete_readiness() -> InputBackendReadiness {
        InputBackendReadiness {
            keyboard_open: true,
            writer_ready: true,
            watchers_ready: false,
            event_processing_ready: false,
        }
    }

    #[test]
    fn dependent_workers_are_prepared_before_physical_grab() {
        let phases = Rc::new(RefCell::new(vec!["keyboard-prepared"]));
        let dependent_phases = Rc::clone(&phases);
        let grab_phases = Rc::clone(&phases);

        let (keyboard, dependent) = finish_prepared_input_backend(
            "prepared-keyboard",
            move |_| {
                dependent_phases.borrow_mut().push("dependent-prepared");
                Ok::<_, SwitcherError>("selected-text-worker")
            },
            move |keyboard, _| {
                grab_phases.borrow_mut().push("grab");
                Ok::<_, SwitcherError>(format!("{keyboard}-active"))
            },
        )
        .unwrap();

        assert_eq!(keyboard, "prepared-keyboard-active");
        assert_eq!(dependent, "selected-text-worker");
        assert_eq!(
            *phases.borrow(),
            vec!["keyboard-prepared", "dependent-prepared", "grab"]
        );
    }

    #[test]
    fn dependent_worker_failure_never_attempts_physical_grab() {
        let grab_attempted = Rc::new(RefCell::new(false));
        let observed_grab_attempt = Rc::clone(&grab_attempted);

        let result: Result<((), ()), SwitcherError> = finish_prepared_input_backend(
            "prepared-keyboard",
            |_| Err(SwitcherError::SelectedTextWorkerDisconnected),
            move |_, _| {
                *observed_grab_attempt.borrow_mut() = true;
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(SwitcherError::SelectedTextWorkerDisconnected)
        ));
        assert!(!*grab_attempted.borrow());
    }

    #[test]
    fn dependent_worker_death_before_activation_never_attempts_physical_grab() {
        let grab_attempted = Rc::new(RefCell::new(false));
        let observed_grab_attempt = Rc::clone(&grab_attempted);

        let result: Result<((), bool), SwitcherError> = finish_prepared_input_backend(
            "prepared-keyboard",
            |_| Ok(false),
            move |_, dependent_alive| {
                if !*dependent_alive {
                    return Err(SwitcherError::InputWorkerDisconnected {
                        worker: "selected-text-worker",
                    });
                }
                *observed_grab_attempt.borrow_mut() = true;
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(SwitcherError::InputWorkerDisconnected {
                worker: "selected-text-worker"
            })
        ));
        assert!(!*grab_attempted.borrow());
    }

    // Retry scheduling helpers

    #[test]
    fn retry_delay_uses_initial_backoff_sequence() {
        assert_eq!(retry_delay_for_attempt(0), Duration::from_millis(500));
        assert_eq!(retry_delay_for_attempt(1), Duration::from_secs(1));
        assert_eq!(retry_delay_for_attempt(2), Duration::from_secs(2));
    }

    #[test]
    fn retry_delay_uses_steady_delay_after_initial_sequence() {
        assert_eq!(retry_delay_for_attempt(3), STEADY_RETRY_DELAY);
        assert_eq!(retry_delay_for_attempt(20), STEADY_RETRY_DELAY);
    }

    // Lifecycle transitions

    #[test]
    fn startup_access_denied_enters_waiting_for_input_access() {
        let opener = FakeOpener {
            outcome: FakeOutcome::KeyboardAccessDenied,
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);

        lifecycle.record_startup_failure(
            &SwitcherError::KeyboardAccessDenied {
                path: PathBuf::from("/dev/input/event3"),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            },
            Instant::now(),
        );

        assert_eq!(lifecycle.state(), InputBackendState::WaitingForInputAccess);
        assert!(lifecycle.retry_deadline().is_some());
    }

    #[test]
    fn runtime_device_loss_enters_recovering() {
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: Rc::new(RefCell::new(0)),
                readiness: ready_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);
        lifecycle.mark_backend_ready(ready_readiness());
        let error = SwitcherError::Io(std::io::Error::from_raw_os_error(19));

        lifecycle.record_runtime_failure(&error, Instant::now());

        assert_eq!(lifecycle.state(), InputBackendState::Recovering);
        assert!(lifecycle.retry_deadline().is_some());
    }

    #[test]
    fn runtime_input_worker_disconnect_enters_recovering() {
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: Rc::new(RefCell::new(0)),
                readiness: ready_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);
        lifecycle.mark_backend_ready(ready_readiness());
        let now = Instant::now();
        let error = SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        };

        assert!(lifecycle.record_runtime_failure(&error, now));
        assert_eq!(lifecycle.state(), InputBackendState::Recovering);
        assert!(lifecycle
            .retry_deadline()
            .is_some_and(|deadline| deadline > now));
    }

    #[test]
    fn non_recoverable_runtime_error_does_not_leave_ready() {
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: Rc::new(RefCell::new(0)),
                readiness: ready_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);
        lifecycle.mark_backend_ready(ready_readiness());
        let error = SwitcherError::DaemonPanicked;

        lifecycle.record_runtime_failure(&error, Instant::now());

        assert_eq!(lifecycle.state(), InputBackendState::Ready);
    }

    #[test]
    fn mark_ready_requires_full_backend_initialization() {
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: Rc::new(RefCell::new(0)),
                readiness: ready_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);

        lifecycle.mark_backend_ready(InputBackendReadiness {
            keyboard_open: true,
            writer_ready: true,
            watchers_ready: true,
            event_processing_ready: true,
        });

        assert_eq!(lifecycle.state(), InputBackendState::Ready);
    }

    #[test]
    fn incomplete_backend_with_clean_shutdown_schedules_retry() {
        let shutdowns = Rc::new(RefCell::new(0));
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: shutdowns.clone(),
                readiness: incomplete_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);

        let reopened = lifecycle
            .try_recover(SharedModifierState::default(), Instant::now())
            .expect("recovery should stay recoverable");

        assert!(reopened.is_none());
        assert_eq!(lifecycle.state(), InputBackendState::WaitingForInputAccess);
        assert_eq!(*shutdowns.borrow(), 1);
        assert!(lifecycle.retry_deadline().is_some());
    }

    #[test]
    fn incomplete_backend_with_unresponsive_writer_returns_fatal_error() {
        let shutdowns = Rc::new(RefCell::new(0));
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: shutdowns.clone(),
                readiness: incomplete_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 },
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);

        let error = match lifecycle.try_recover(SharedModifierState::default(), Instant::now()) {
            Err(error) => error,
            Ok(_) => panic!("unresponsive partial backend must be fatal"),
        };

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 1_000,
                phase: "backend-readiness",
                ref trigger,
            } if trigger == "input backend readiness incomplete"
        ));
        assert_eq!(*shutdowns.borrow(), 1);
        assert!(lifecycle.retry_deadline().is_none());
    }

    #[test]
    fn unresponsive_writer_forbids_second_backend_install() {
        let opens = Rc::new(RefCell::new(0));
        let opener = FakeOpener {
            outcome: FakeOutcome::OkCounted {
                opens: Rc::clone(&opens),
                shutdowns: Rc::new(RefCell::new(0)),
                readiness: incomplete_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 },
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);
        let now = Instant::now();

        let first = lifecycle.try_recover(SharedModifierState::default(), now);
        let second =
            lifecycle.try_recover(SharedModifierState::default(), now + Duration::from_secs(5));

        assert!(matches!(
            first,
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive { .. })
        ));
        assert!(matches!(
            second,
            Err(SwitcherError::VirtualKeyboardWriterShutdownUnresponsive { .. })
        ));
        assert_eq!(*opens.borrow(), 1);
    }

    #[test]
    fn post_activation_failure_preserves_original_error_after_clean_shutdown() {
        let shutdowns = Rc::new(RefCell::new(0));
        let mut backend = FakeBackend {
            active_backends: None,
            shutdowns: Rc::clone(&shutdowns),
            shutdown_outcome: WriterShutdownOutcome::Stopped,
        };

        let error = shutdown_backend_after_error(
            &mut backend,
            SwitcherError::InputWorkerDisconnected {
                worker: "selected-text-worker",
            },
            "backend-open",
        );

        assert!(matches!(
            error,
            SwitcherError::InputWorkerDisconnected {
                worker: "selected-text-worker"
            }
        ));
        assert_eq!(*shutdowns.borrow(), 1);
    }

    #[test]
    fn post_activation_failure_is_overridden_by_unresponsive_fail_stop() {
        let mut backend = FakeBackend {
            active_backends: None,
            shutdowns: Rc::new(RefCell::new(0)),
            shutdown_outcome: WriterShutdownOutcome::Unresponsive { timeout_ms: 1_000 },
        };

        let error = shutdown_backend_after_error(
            &mut backend,
            SwitcherError::InputWorkerDisconnected {
                worker: "selected-text-worker",
            },
            "backend-open",
        );

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
                timeout_ms: 1_000,
                phase: "backend-open",
                ref trigger,
            } if trigger == "Input worker selected-text-worker is unavailable"
        ));
    }

    #[test]
    fn repeated_incomplete_recoveries_shutdown_and_drop_each_partial_backend() {
        let active_backends = Rc::new(RefCell::new(0));
        let shutdowns = Rc::new(RefCell::new(0));
        let opener = FakeOpener {
            outcome: FakeOutcome::OkTracked {
                active_backends: active_backends.clone(),
                shutdowns: shutdowns.clone(),
                readiness: InputBackendReadiness {
                    keyboard_open: true,
                    writer_ready: true,
                    watchers_ready: false,
                    event_processing_ready: false,
                },
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);
        let mut now = Instant::now();

        for expected_shutdowns in 1..=3 {
            let reopened = lifecycle
                .try_recover(SharedModifierState::default(), now)
                .expect("recovery should stay recoverable");

            assert!(reopened.is_none());
            assert_eq!(*active_backends.borrow(), 0);
            assert_eq!(*shutdowns.borrow(), expected_shutdowns);
            now += Duration::from_secs(5);
        }
    }

    #[test]
    fn try_recover_returns_backend_when_reopen_is_fully_ready() {
        let opener = FakeOpener {
            outcome: FakeOutcome::Ok {
                shutdowns: Rc::new(RefCell::new(0)),
                readiness: ready_readiness(),
                shutdown_outcome: WriterShutdownOutcome::Stopped,
            },
        };
        let mut lifecycle = InputBackendLifecycle::new(opener);

        let reopened = lifecycle
            .try_recover(SharedModifierState::default(), Instant::now())
            .expect("recovery should succeed");

        assert!(reopened.is_some());
        assert_eq!(lifecycle.state(), InputBackendState::Ready);
        assert!(lifecycle.retry_deadline().is_none());
    }
}
