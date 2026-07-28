use super::protocol::{
    decode_frame, encode_frame, response_matches, CleanupDeadlineNs, FatalCode, Message,
    MutationDeadlineNs, PreparedToken, ReleaseDeadline, Request, Response, Sequence, ServerEpoch,
    SessionId, WireTerminalProof, MAX_ACTIVE_DEBTS, MAX_PREPARED_TOKENS, MAX_RELEASE_CLEANUP_NS,
};
use super::seqpacket::Seqpacket;
use super::service::{monotonic_now_ns, X11ServerIdentity};
use super::x11::EmergencyX11Releaser;
use crate::daemon::debug_log::{format_input, try_debug_line, DebugLogKind};
use crate::daemon::synthetic_input::{
    InputGeneration, OperationId, PhysicalSequence, SyntheticKeySink, TerminalProof,
};
use crate::error::{InputSafetyError, SwitcherError};
use evdev::Key;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const GUARDIAN_COMMAND_CAPACITY: usize = 16;
const LATENCY_SAMPLE_CAPACITY: usize = 512;
pub(crate) const GUARDIAN_EMERGENCY_DEADLINE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
pub(crate) struct GuardianMutationDeadline {
    local: Instant,
    wire: MutationDeadlineNs,
}

impl GuardianMutationDeadline {
    pub(crate) fn from_instant(local: Instant) -> Result<Self, SwitcherError> {
        let now_ns = monotonic_now_ns()?;
        // Sampling `Instant` after CLOCK_MONOTONIC keeps the wire deadline
        // conservative by the tiny interval between the two reads.
        let sampled_at = Instant::now();
        let remaining = local
            .checked_duration_since(sampled_at)
            .ok_or(InputSafetyError::GuardianRequestTimedOut { operation_id: 0 })?;
        let remaining_ns = u64::try_from(remaining.as_nanos()).map_err(|_| {
            InputSafetyError::GuardianProtocol {
                context: "mutation deadline cannot fit CLOCK_MONOTONIC nanoseconds",
            }
        })?;
        let wire = now_ns
            .checked_add(remaining_ns)
            .ok_or(InputSafetyError::GuardianProtocol {
                context: "mutation deadline overflowed CLOCK_MONOTONIC",
            })?;
        Ok(Self {
            local,
            wire: MutationDeadlineNs(wire),
        })
    }

    #[cfg(test)]
    fn for_test(after: Duration) -> Self {
        Self::from_instant(Instant::now() + after).unwrap()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GuardianReady {
    pub(crate) session: SessionId,
    pub(crate) epoch: ServerEpoch,
    pub(crate) epoch_window: u32,
    pub(crate) epoch_nonce: [u8; 16],
}

impl GuardianReady {
    pub(crate) fn server_identity(&self, root: u32) -> Result<X11ServerIdentity, SwitcherError> {
        if root == 0
            || self.session.0.iter().all(|byte| *byte == 0)
            || self.epoch.0.iter().all(|byte| *byte == 0)
            || self.epoch_window == 0
            || self.epoch_nonce.iter().all(|byte| *byte == 0)
        {
            return Err(InputSafetyError::GuardianProtocol {
                context: "ready identity contains a zero field",
            }
            .into());
        }
        Ok(X11ServerIdentity {
            epoch: self.epoch,
            root,
            epoch_window: self.epoch_window,
            epoch_nonce: self.epoch_nonce,
        })
    }
}

#[derive(Clone)]
pub(crate) struct GuardianHealth {
    failed: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<InputSafetyError>>>,
}

impl GuardianHealth {
    fn new() -> Self {
        Self {
            failed: Arc::new(AtomicBool::new(false)),
            reason: Arc::new(Mutex::new(None)),
        }
    }

    fn fail(&self, error: InputSafetyError) {
        let mut reason = self
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if reason.is_none() {
            *reason = Some(error);
            self.failed.store(true, Ordering::Release);
        }
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(crate) fn error(&self) -> Option<InputSafetyError> {
        self.reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn failed_for_test(error: InputSafetyError) -> Self {
        let health = Self::new();
        health.fail(error);
        health
    }
}

pub(crate) trait EmergencyRelease: Send + 'static {
    fn server_epoch(&self) -> ServerEpoch;
    fn release_token(&mut self, token: PreparedToken) -> Result<(), SwitcherError>;
    fn synchronize(&mut self) -> Result<(), SwitcherError>;
}

impl EmergencyRelease for EmergencyX11Releaser {
    fn server_epoch(&self) -> ServerEpoch {
        self.server_identity().epoch
    }

    fn release_token(&mut self, token: PreparedToken) -> Result<(), SwitcherError> {
        EmergencyX11Releaser::release_token(self, token)
    }

    fn synchronize(&mut self) -> Result<(), SwitcherError> {
        EmergencyX11Releaser::synchronize(self)
    }
}

enum EmergencyState {
    Unarmed,
    Armed {
        connection: Box<dyn EmergencyRelease>,
        session: SessionId,
        epoch: ServerEpoch,
        mirrored: Vec<PreparedToken>,
    },
    Pending {
        connection: Box<dyn EmergencyRelease>,
        mirrored: Vec<PreparedToken>,
    },
    Running {
        remaining: usize,
    },
    Finished(TerminalProof),
}

#[derive(Clone)]
pub(crate) struct EmergencyCoordinator {
    state: Arc<Mutex<EmergencyState>>,
}

impl EmergencyCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(EmergencyState::Unarmed)),
        }
    }

    pub(crate) fn arm<R: EmergencyRelease>(
        &self,
        connection: R,
        expected_session: SessionId,
        expected_epoch: ServerEpoch,
    ) -> Result<(), SwitcherError> {
        if expected_session.0.iter().all(|byte| *byte == 0)
            || expected_epoch.0.iter().all(|byte| *byte == 0)
            || connection.server_epoch() != expected_epoch
        {
            return Err(InputSafetyError::GuardianProtocol {
                context: "emergency connection epoch does not match guardian ready epoch",
            }
            .into());
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, EmergencyState::Unarmed) {
            return Err(InputSafetyError::GuardianProtocol {
                context: "emergency connection can be armed only once",
            }
            .into());
        }
        *state = EmergencyState::Armed {
            connection: Box::new(connection),
            session: expected_session,
            epoch: expected_epoch,
            mirrored: Vec::with_capacity(MAX_ACTIVE_DEBTS),
        };
        Ok(())
    }

    fn insert_possible(&self, token: PreparedToken) -> Result<(), SwitcherError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let EmergencyState::Armed {
            session,
            epoch,
            mirrored,
            ..
        } = &mut *state
        else {
            return Err(InputSafetyError::GuardianUnavailable {
                context: "emergency release path is not armed",
            }
            .into());
        };
        if token.session != *session || token.epoch != *epoch {
            return Err(InputSafetyError::GuardianProtocol {
                context: "prepared token session or epoch does not match emergency connection",
            }
            .into());
        }
        if mirrored.contains(&token) {
            return Err(InputSafetyError::GuardianProtocol {
                context: "ambiguous XTEST down cannot be attempted twice",
            }
            .into());
        }
        if mirrored.len() >= MAX_ACTIVE_DEBTS {
            return Err(InputSafetyError::GuardianProtocol {
                context: "daemon XTEST mirror capacity exceeded",
            }
            .into());
        }
        mirrored.push(token);
        Ok(())
    }

    fn remove_reconciled(&self, token: PreparedToken) -> Result<(), SwitcherError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let EmergencyState::Armed { mirrored, .. } = &mut *state else {
            return Err(InputSafetyError::GuardianUnavailable {
                context: "guardian failed before mirror reconciliation",
            }
            .into());
        };
        let index = mirrored
            .iter()
            .position(|candidate| *candidate == token)
            .ok_or(InputSafetyError::GuardianProtocol {
                context: "synchronized XTEST release has no mirrored token",
            })?;
        mirrored.remove(index);
        Ok(())
    }

    fn contains_possible(&self, token: PreparedToken) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(
            &*state,
            EmergencyState::Armed { mirrored, .. } if mirrored.contains(&token)
        )
    }

    fn mark_guardian_failed(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::mem::replace(
            &mut *state,
            EmergencyState::Finished(TerminalProof::Unreconciled { remaining: 0 }),
        );
        *state = match previous {
            EmergencyState::Armed {
                connection,
                mirrored,
                ..
            } => EmergencyState::Pending {
                connection,
                mirrored,
            },
            EmergencyState::Unarmed => EmergencyState::Finished(TerminalProof::Reconciled),
            other => other,
        };
    }

    pub(crate) fn possible_tokens(&self) -> Vec<PreparedToken> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            EmergencyState::Armed { mirrored, .. } | EmergencyState::Pending { mirrored, .. } => {
                mirrored.clone()
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn start_pending_release(&self) -> Result<EmergencyJob, SwitcherError> {
        self.start_pending_release_if_needed()?.ok_or_else(|| {
            InputSafetyError::GuardianProtocol {
                context: "emergency release was started outside pending state",
            }
            .into()
        })
    }

    pub(crate) fn start_pending_release_if_needed(
        &self,
    ) -> Result<Option<EmergencyJob>, SwitcherError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::mem::replace(
            &mut *state,
            EmergencyState::Finished(TerminalProof::Unreconciled { remaining: 0 }),
        );
        let EmergencyState::Pending {
            mut connection,
            mirrored,
        } = previous
        else {
            *state = previous;
            return Ok(None);
        };
        let remaining = mirrored.len();
        if remaining == 0 {
            *state = EmergencyState::Finished(TerminalProof::Reconciled);
            return Ok(Some(EmergencyJob::completed(
                self.clone(),
                TerminalProof::Reconciled,
            )));
        }
        *state = EmergencyState::Running { remaining };
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let spawn = thread::Builder::new()
            .name("openswitcher-xtest-emergency".to_string())
            .spawn(move || {
                let mut failed = 0usize;
                for token in mirrored.iter().rev().copied() {
                    if connection.release_token(token).is_err() {
                        failed += 1;
                    }
                }
                let proof = if connection.synchronize().is_ok() {
                    if failed == 0 {
                        TerminalProof::Reconciled
                    } else {
                        TerminalProof::Unreconciled { remaining: failed }
                    }
                } else {
                    TerminalProof::Unreconciled { remaining }
                };
                let _ = result_tx.send(proof);
            });
        if let Err(error) = spawn {
            *state = EmergencyState::Finished(TerminalProof::Unreconciled { remaining });
            return Err(SwitcherError::Io(error));
        }
        Ok(Some(EmergencyJob {
            coordinator: self.clone(),
            result: EmergencyJobResult::Pending(result_rx),
            initial_remaining: remaining,
        }))
    }

    fn finish_job(&self, proof: TerminalProof) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, EmergencyState::Running { .. }) {
            *state = EmergencyState::Finished(proof);
        }
    }

    pub(crate) fn terminal_proof(&self) -> TerminalProof {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            EmergencyState::Unarmed => TerminalProof::Unreconciled { remaining: 0 },
            EmergencyState::Armed { mirrored, .. } | EmergencyState::Pending { mirrored, .. } => {
                if mirrored.is_empty() {
                    TerminalProof::Reconciled
                } else {
                    TerminalProof::Unreconciled {
                        remaining: mirrored.len(),
                    }
                }
            }
            EmergencyState::Running { remaining } => TerminalProof::Unreconciled {
                remaining: *remaining,
            },
            EmergencyState::Finished(proof) => proof.clone(),
        }
    }

    fn mark_guardian_terminal(&self, proof: TerminalProof) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = EmergencyState::Finished(proof);
    }
}

enum EmergencyJobResult {
    Pending(mpsc::Receiver<TerminalProof>),
    Completed(TerminalProof),
}

pub(crate) struct EmergencyJob {
    coordinator: EmergencyCoordinator,
    result: EmergencyJobResult,
    initial_remaining: usize,
}

impl EmergencyJob {
    fn completed(coordinator: EmergencyCoordinator, proof: TerminalProof) -> Self {
        Self {
            coordinator,
            result: EmergencyJobResult::Completed(proof),
            initial_remaining: 0,
        }
    }

    pub(crate) fn wait(self, timeout: Duration) -> TerminalProof {
        let proof = match self.result {
            EmergencyJobResult::Completed(proof) => proof,
            EmergencyJobResult::Pending(receiver) => {
                receiver
                    .recv_timeout(timeout)
                    .unwrap_or(TerminalProof::Unreconciled {
                        remaining: self.initial_remaining,
                    })
            }
        };
        self.coordinator.finish_job(proof.clone());
        proof
    }
}

#[derive(Clone)]
struct BrokerFailure {
    accepting: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    health: GuardianHealth,
    emergency: EmergencyCoordinator,
    serialized: Arc<Mutex<()>>,
}

impl BrokerFailure {
    fn fail(&self, error: InputSafetyError) {
        let _guard = self
            .serialized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.health.is_failed() {
            self.stopping.store(true, Ordering::Release);
            return;
        }
        self.accepting.store(false, Ordering::Release);
        self.emergency.mark_guardian_failed();
        self.health.fail(error);
        self.stopping.store(true, Ordering::Release);
    }
}

struct BrokerCommand {
    request: Request,
    local_deadline: Instant,
    reply: mpsc::SyncSender<Result<Response, SwitcherError>>,
}

struct BrokerHandle {
    commands: mpsc::SyncSender<BrokerCommand>,
    wake: Arc<UnixStream>,
    accepting: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    health: GuardianHealth,
    failure: BrokerFailure,
}

impl BrokerHandle {
    fn submit(
        &self,
        request: Request,
        local_deadline: Instant,
        allow_before_arm: bool,
    ) -> Result<Response, SwitcherError> {
        if !allow_before_arm && !self.accepting.load(Ordering::Acquire) {
            return Err(self.health_error_or_unavailable());
        }
        if self.stopping.load(Ordering::Acquire) {
            return Err(self.health_error_or_unavailable());
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let command = BrokerCommand {
            request,
            local_deadline,
            reply: reply_tx,
        };
        match self.commands.try_send(command) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                let error = InputSafetyError::GuardianUnavailable {
                    context: "guardian broker command queue is full",
                };
                self.failure.fail(error.clone());
                self.wake();
                return Err(error.into());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                let error = InputSafetyError::GuardianUnavailable {
                    context: "guardian broker command queue is disconnected",
                };
                self.failure.fail(error.clone());
                self.wake();
                return Err(error.into());
            }
        }
        self.wake();

        let remaining = match local_deadline.checked_duration_since(Instant::now()) {
            Some(remaining) if !remaining.is_zero() => remaining,
            _ => {
                return Err(self.fail_timeout(&request));
            }
        };
        match reply_rx.recv_timeout(remaining) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(self.fail_timeout(&request)),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let error = InputSafetyError::GuardianUnavailable {
                    context: "guardian broker reply channel is disconnected",
                };
                self.failure.fail(error.clone());
                self.wake();
                Err(error.into())
            }
        }
    }

    fn fail_timeout(&self, request: &Request) -> SwitcherError {
        let error = InputSafetyError::GuardianRequestTimedOut {
            operation_id: request_operation_id(request),
        };
        self.failure.fail(error.clone());
        self.wake();
        error.into()
    }

    fn health_error_or_unavailable(&self) -> SwitcherError {
        self.health
            .error()
            .unwrap_or(InputSafetyError::GuardianUnavailable {
                context: "guardian broker is terminal",
            })
            .into()
    }

    fn wake(&self) {
        let mut stream = &*self.wake;
        match stream.write(&[1]) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => {}
        }
    }
}

fn request_operation_id(request: &Request) -> u64 {
    match *request {
        Request::PrepareKey { operation, .. }
        | Request::ExecuteDown { operation, .. }
        | Request::KeyUp { operation, .. }
        | Request::Synchronize { operation, .. }
        | Request::TransferToPhysicalDebt { operation, .. }
        | Request::CancelAndDrain { operation, .. } => operation.0,
        Request::Hello { .. }
        | Request::PhysicalReleaseCommitted { .. }
        | Request::ReleaseAllAndExit { .. } => 0,
    }
}

#[derive(Clone, Copy)]
enum PendingMutation {
    Down(PreparedToken),
    Up {
        token: PreparedToken,
        clears_mirror: bool,
    },
}

pub(crate) struct GuardianClient {
    broker: BrokerHandle,
    ready: GuardianReady,
    emergency: EmergencyCoordinator,
    prepared_tokens: Mutex<Vec<(OperationId, PreparedToken)>>,
    pending_mutation: Option<PendingMutation>,
    broker_thread: Option<JoinHandle<()>>,
}

impl GuardianClient {
    pub(crate) fn connect(path: &Path, handshake_deadline: Instant) -> Result<Self, SwitcherError> {
        Self::from_connection(Seqpacket::connect(path)?, handshake_deadline)
    }

    #[cfg(test)]
    pub(crate) fn from_test_connection(
        connection: Seqpacket,
        handshake_deadline: Instant,
    ) -> Result<Self, SwitcherError> {
        Self::from_connection(connection, handshake_deadline)
    }

    fn from_connection(
        connection: Seqpacket,
        handshake_deadline: Instant,
    ) -> Result<Self, SwitcherError> {
        Self::from_connection_with_emergency(
            connection,
            handshake_deadline,
            EmergencyCoordinator::new(),
        )
    }

    fn from_connection_with_emergency(
        connection: Seqpacket,
        handshake_deadline: Instant,
        emergency: EmergencyCoordinator,
    ) -> Result<Self, SwitcherError> {
        let deadline = GuardianMutationDeadline::from_instant(handshake_deadline)?;
        let daemon_nonce = read_nonzero_nonce()?;
        let (broker, broker_thread) = spawn_broker(connection, emergency.clone())?;
        let response = match broker.submit(
            Request::Hello {
                daemon_nonce,
                deadline: deadline.wire,
            },
            deadline.local,
            true,
        ) {
            Ok(response) => response,
            Err(error) => {
                broker.wake();
                let _ = broker_thread.join();
                return Err(error);
            }
        };
        let Response::Ready {
            session,
            epoch,
            epoch_window,
            epoch_nonce,
        } = response
        else {
            broker.failure.fail(InputSafetyError::GuardianProtocol {
                context: "guardian handshake did not return Ready",
            });
            broker.wake();
            let _ = broker_thread.join();
            return Err(InputSafetyError::GuardianProtocol {
                context: "guardian handshake did not return Ready",
            }
            .into());
        };
        let ready = GuardianReady {
            session,
            epoch,
            epoch_window,
            epoch_nonce,
        };
        if let Err(error) = ready.server_identity(1) {
            broker.failure.fail(health_error_for(&error));
            broker.wake();
            let _ = broker_thread.join();
            return Err(error);
        }
        Ok(Self {
            broker,
            ready,
            emergency,
            prepared_tokens: Mutex::new(Vec::with_capacity(MAX_PREPARED_TOKENS)),
            pending_mutation: None,
            broker_thread: Some(broker_thread),
        })
    }

    pub(crate) fn ready(&self) -> &GuardianReady {
        &self.ready
    }

    pub(crate) fn health(&self) -> GuardianHealth {
        self.broker.health.clone()
    }

    pub(crate) fn emergency_coordinator(&self) -> EmergencyCoordinator {
        self.emergency.clone()
    }

    pub(crate) fn arm_emergency<R: EmergencyRelease>(
        &self,
        connection: R,
    ) -> Result<(), SwitcherError> {
        // Serialize arming with fail-stop publication so a concurrent guardian
        // loss cannot close the gate and then have this path reopen it.
        let _guard = self
            .broker
            .failure
            .serialized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.broker.health.is_failed() {
            return Err(self.broker.health_error_or_unavailable());
        }
        self.emergency
            .arm(connection, self.ready.session, self.ready.epoch)?;
        self.broker.accepting.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn prepare_key(
        &mut self,
        operation: OperationId,
        key: Key,
        deadline: GuardianMutationDeadline,
    ) -> Result<PreparedToken, SwitcherError> {
        self.ensure_no_pending_mutation()?;
        if self
            .prepared_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            >= MAX_PREPARED_TOKENS
        {
            return Err(self.fail_protocol("daemon prepared token capacity exceeded"));
        }
        let response = self.broker.submit(
            Request::PrepareKey {
                operation,
                evdev_code: key.code(),
                deadline: deadline.wire,
            },
            deadline.local,
            false,
        )?;
        let Response::Prepared {
            operation: response_operation,
            token,
        } = response
        else {
            return Err(self.fail_protocol("PrepareKey response is not Prepared"));
        };
        if response_operation != operation
            || token.session != self.ready.session
            || token.epoch != self.ready.epoch
            || token.evdev_code != key.code()
            || token.token_id == 0
            || token.x11_keycode == 0
        {
            return Err(self.fail_protocol("Prepared token identity is invalid"));
        }
        self.prepared_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((operation, token));
        Ok(token)
    }

    pub(crate) fn execute_down(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
        deadline: GuardianMutationDeadline,
    ) -> Result<(), SwitcherError> {
        self.ensure_no_pending_mutation()?;
        self.validate_token_identity(token)?;
        self.consume_prepared(operation, token)?;
        if let Err(error) = self.emergency.insert_possible(token) {
            self.broker.failure.fail(health_error_for(&error));
            self.broker.wake();
            return Err(error);
        }
        let response = self.broker.submit(
            Request::ExecuteDown {
                operation,
                token,
                deadline: deadline.wire,
            },
            deadline.local,
            false,
        )?;
        match response {
            Response::DownAck {
                operation: response_operation,
                token_id,
            } if response_operation == operation && token_id == token.token_id => {
                self.pending_mutation = Some(PendingMutation::Down(token));
                Ok(())
            }
            _ => Err(self.fail_protocol("ExecuteDown response is not matching DownAck")),
        }
    }

    pub(crate) fn key_up(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
        deadline: GuardianMutationDeadline,
    ) -> Result<(), SwitcherError> {
        self.ensure_no_pending_mutation()?;
        self.validate_token_identity(token)?;
        let clears_mirror = self.emergency.contains_possible(token);
        if !clears_mirror {
            self.consume_prepared(operation, token)?;
        }
        let response = self.broker.submit(
            Request::KeyUp {
                operation,
                token,
                deadline: ReleaseDeadline::Mutation(deadline.wire),
            },
            deadline.local,
            false,
        )?;
        match response {
            Response::UpAck {
                operation: response_operation,
                token_id,
            } if response_operation == operation && token_id == token.token_id => {
                self.pending_mutation = Some(PendingMutation::Up {
                    token,
                    clears_mirror,
                });
                Ok(())
            }
            _ => Err(self.fail_protocol("KeyUp response is not matching UpAck")),
        }
    }

    pub(crate) fn synchronize(
        &mut self,
        operation: OperationId,
        deadline: GuardianMutationDeadline,
    ) -> Result<(), SwitcherError> {
        let Some(pending) = self.pending_mutation else {
            return Err(self.fail_protocol("Synchronize has no pending XTEST mutation"));
        };
        let token = match pending {
            PendingMutation::Down(token) | PendingMutation::Up { token, .. } => token,
        };
        let response = self.broker.submit(
            Request::Synchronize {
                operation,
                token_id: token.token_id,
                deadline: ReleaseDeadline::Mutation(deadline.wire),
            },
            deadline.local,
            false,
        )?;
        match response {
            Response::SyncAck {
                operation: response_operation,
                token_id,
            } if response_operation == operation && token_id == token.token_id => {
                if matches!(
                    pending,
                    PendingMutation::Up {
                        clears_mirror: true,
                        ..
                    }
                ) {
                    if let Err(error) = self.emergency.remove_reconciled(token) {
                        self.broker.failure.fail(health_error_for(&error));
                        self.broker.wake();
                        return Err(error);
                    }
                }
                self.pending_mutation = None;
                Ok(())
            }
            _ => Err(self.fail_protocol("Synchronize response is not matching SyncAck")),
        }
    }

    pub(crate) fn synchronize_if_pending(
        &mut self,
        operation: OperationId,
        deadline: GuardianMutationDeadline,
    ) -> Result<(), SwitcherError> {
        if self.pending_mutation.is_none() {
            return Ok(());
        }
        self.synchronize(operation, deadline)
    }

    pub(crate) fn transfer_to_physical_debt(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: GuardianMutationDeadline,
    ) -> Result<(), SwitcherError> {
        self.ensure_no_pending_mutation()?;
        self.validate_token_identity(token)?;
        if input_generation.0 == 0 {
            return Err(self.fail_protocol("XTEST physical debt generation must be nonzero"));
        }
        if !self.emergency.contains_possible(token) {
            return Err(self.fail_protocol(
                "XTEST physical debt transfer has no matching possible-down mirror",
            ));
        }
        let response = self.broker.submit(
            Request::TransferToPhysicalDebt {
                operation,
                token,
                input_generation,
                deadline: deadline.wire,
            },
            deadline.local,
            false,
        )?;
        match response {
            Response::TransferAck {
                operation: response_operation,
                token_id,
            } if response_operation == operation && token_id == token.token_id => Ok(()),
            _ => {
                Err(self
                    .fail_protocol("TransferToPhysicalDebt response is not matching TransferAck"))
            }
        }
    }

    pub(crate) fn commit_physical_release(
        &mut self,
        sequence: PhysicalSequence,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: GuardianMutationDeadline,
    ) -> Result<(), SwitcherError> {
        self.ensure_no_pending_mutation()?;
        self.validate_token_identity(token)?;
        if sequence.0 == 0 {
            return Err(self.fail_protocol("XTEST physical release sequence must be nonzero"));
        }
        if input_generation.0 == 0 {
            return Err(self.fail_protocol("XTEST physical release generation must be nonzero"));
        }
        if !self.emergency.contains_possible(token) {
            return Err(self.fail_protocol(
                "XTEST physical release commit has no matching possible-down mirror",
            ));
        }
        let response = self.broker.submit(
            Request::PhysicalReleaseCommitted {
                sequence,
                token,
                input_generation,
                deadline: deadline.wire,
            },
            deadline.local,
            false,
        )?;
        match response {
            Response::ReleaseCommitAck {
                sequence: response_sequence,
                token_id,
            } if response_sequence == sequence && token_id == token.token_id => {
                if let Err(error) = self.emergency.remove_reconciled(token) {
                    self.broker.failure.fail(health_error_for(&error));
                    self.broker.wake();
                    return Err(error);
                }
                Ok(())
            }
            _ => Err(self.fail_protocol(
                "PhysicalReleaseCommitted response is not matching ReleaseCommitAck",
            )),
        }
    }

    fn ensure_no_pending_mutation(&self) -> Result<(), SwitcherError> {
        if self.pending_mutation.is_some() {
            return Err(self.fail_protocol("previous XTEST mutation must be synchronized first"));
        }
        Ok(())
    }

    fn validate_token_identity(&self, token: PreparedToken) -> Result<(), SwitcherError> {
        if token.session != self.ready.session
            || token.epoch != self.ready.epoch
            || token.token_id == 0
            || token.evdev_code == 0
            || token.x11_keycode == 0
        {
            return Err(
                self.fail_protocol("XTEST token does not belong to the active guardian session")
            );
        }
        Ok(())
    }

    fn consume_prepared(
        &self,
        operation: OperationId,
        token: PreparedToken,
    ) -> Result<(), SwitcherError> {
        let mut prepared = self
            .prepared_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = prepared
            .iter()
            .position(|candidate| *candidate == (operation, token))
        else {
            drop(prepared);
            return Err(
                self.fail_protocol("XTEST mutation token was not prepared for this operation")
            );
        };
        prepared.remove(index);
        Ok(())
    }

    fn has_any_prepared(&self) -> bool {
        !self
            .prepared_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    fn clear_prepared(&self) {
        self.prepared_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub(crate) fn fail_protocol(&self, context: &'static str) -> SwitcherError {
        let error = InputSafetyError::GuardianProtocol { context };
        self.broker.failure.fail(error.clone());
        self.broker.wake();
        error.into()
    }

    pub(crate) fn operation_terminal_proof(
        &self,
        operation: OperationId,
        remaining_debt: usize,
    ) -> TerminalProof {
        if !self.health().is_failed()
            && remaining_debt == 0
            && !self.has_any_prepared()
            && self.pending_mutation.is_none()
        {
            TerminalProof::Reconciled
        } else {
            self.cancel_and_drain(operation)
        }
    }

    pub(crate) fn cancel_and_drain(&self, operation: OperationId) -> TerminalProof {
        if self.broker.health.is_failed() {
            return self.emergency.terminal_proof();
        }
        let deadline = match cleanup_deadline() {
            Ok(deadline) => deadline,
            Err(_) => {
                return self.emergency.terminal_proof();
            }
        };
        let response = self.broker.submit(
            Request::CancelAndDrain {
                operation,
                deadline: deadline.wire,
            },
            deadline.local,
            false,
        );
        match response {
            Ok(Response::Drained {
                operation: response_operation,
                proof,
            }) if response_operation == operation => {
                let proof = wire_proof(proof);
                self.broker.accepting.store(false, Ordering::Release);
                if matches!(proof, TerminalProof::Unreconciled { .. }) {
                    self.broker
                        .failure
                        .fail(InputSafetyError::GuardianProtocol {
                            context: "guardian reported unreconciled terminal debt",
                        });
                    self.broker.wake();
                    self.emergency.terminal_proof()
                } else {
                    self.clear_prepared();
                    self.emergency.mark_guardian_terminal(proof.clone());
                    proof
                }
            }
            Ok(_) => {
                let _ = self.fail_protocol("CancelAndDrain response is not matching Drained");
                self.emergency.terminal_proof()
            }
            Err(_) => self.emergency.terminal_proof(),
        }
    }
}

impl Drop for GuardianClient {
    fn drop(&mut self) {
        self.broker.accepting.store(false, Ordering::Release);
        self.broker.stopping.store(true, Ordering::Release);
        self.broker.wake();
        if let Some(thread) = self.broker_thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) struct GuardianSyntheticSink<'a> {
    client: &'a mut GuardianClient,
    operation: OperationId,
    deadline: GuardianMutationDeadline,
}

impl<'a> GuardianSyntheticSink<'a> {
    pub(crate) fn new(
        client: &'a mut GuardianClient,
        operation: OperationId,
        deadline: GuardianMutationDeadline,
    ) -> Self {
        Self {
            client,
            operation,
            deadline,
        }
    }
}

impl SyntheticKeySink for GuardianSyntheticSink<'_> {
    type Token = PreparedToken;

    fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
        self.client.prepare_key(self.operation, key, self.deadline)
    }

    fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.client
            .execute_down(self.operation, *token, self.deadline)
    }

    fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.client.key_up(self.operation, *token, self.deadline)
    }

    fn synchronize(&mut self) -> Result<(), SwitcherError> {
        self.client
            .synchronize_if_pending(self.operation, self.deadline)
    }

    fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof {
        self.client
            .operation_terminal_proof(self.operation, remaining_debt)
    }
}

#[derive(Clone, Copy)]
struct GuardianCleanupDeadline {
    local: Instant,
    wire: CleanupDeadlineNs,
}

fn cleanup_deadline() -> Result<GuardianCleanupDeadline, SwitcherError> {
    let now_ns = monotonic_now_ns()?;
    let local = Instant::now()
        .checked_add(GUARDIAN_EMERGENCY_DEADLINE)
        .ok_or(InputSafetyError::GuardianProtocol {
            context: "cleanup deadline overflowed local monotonic clock",
        })?;
    let wire =
        now_ns
            .checked_add(MAX_RELEASE_CLEANUP_NS)
            .ok_or(InputSafetyError::GuardianProtocol {
                context: "cleanup deadline overflowed CLOCK_MONOTONIC",
            })?;
    Ok(GuardianCleanupDeadline {
        local,
        wire: CleanupDeadlineNs(wire),
    })
}

fn wire_proof(proof: WireTerminalProof) -> TerminalProof {
    match proof {
        WireTerminalProof::Reconciled => TerminalProof::Reconciled,
        WireTerminalProof::OwnerGenerationDestroyed { generation } => {
            TerminalProof::OwnerGenerationDestroyed { generation }
        }
        WireTerminalProof::Unreconciled { remaining } => TerminalProof::Unreconciled {
            remaining: usize::from(remaining),
        },
    }
}

fn read_nonzero_nonce() -> Result<[u8; 16], SwitcherError> {
    let mut nonce = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    if nonce.iter().all(|byte| *byte == 0) {
        return Err(InputSafetyError::GuardianProtocol {
            context: "daemon nonce must be nonzero",
        }
        .into());
    }
    Ok(nonce)
}

struct LatencyRing {
    samples_us: VecDeque<u64>,
}

impl LatencyRing {
    fn new() -> Self {
        Self {
            samples_us: VecDeque::with_capacity(LATENCY_SAMPLE_CAPACITY),
        }
    }

    fn record(&mut self, elapsed: Duration) {
        if self.samples_us.len() == LATENCY_SAMPLE_CAPACITY {
            self.samples_us.pop_front();
        }
        self.samples_us
            .push_back(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX));
    }

    fn summary(&self) -> Option<(usize, u64, u64, u64)> {
        if self.samples_us.is_empty() {
            return None;
        }
        let mut sorted: Vec<_> = self.samples_us.iter().copied().collect();
        sorted.sort_unstable();
        let percentile = |numerator: usize| {
            let index = (sorted.len() - 1).saturating_mul(numerator) / 100;
            sorted[index]
        };
        Some((
            sorted.len(),
            percentile(50),
            percentile(95),
            *sorted.last().unwrap_or(&0),
        ))
    }
}

fn spawn_broker(
    connection: Seqpacket,
    emergency: EmergencyCoordinator,
) -> Result<(BrokerHandle, JoinHandle<()>), SwitcherError> {
    let (commands, receiver) = mpsc::sync_channel(GUARDIAN_COMMAND_CAPACITY);
    let (wake_reader, wake_writer) = UnixStream::pair()?;
    wake_reader.set_nonblocking(true)?;
    wake_writer.set_nonblocking(true)?;
    let wake = Arc::new(wake_writer);
    let accepting = Arc::new(AtomicBool::new(false));
    let stopping = Arc::new(AtomicBool::new(false));
    let health = GuardianHealth::new();
    let failure = BrokerFailure {
        accepting: accepting.clone(),
        stopping: stopping.clone(),
        health: health.clone(),
        emergency,
        serialized: Arc::new(Mutex::new(())),
    };
    let broker_failure = failure.clone();
    let broker_stopping = stopping.clone();
    let thread = thread::Builder::new()
        .name("openswitcher-xtest-broker".to_string())
        .spawn(move || {
            run_broker(
                connection,
                receiver,
                wake_reader,
                broker_stopping,
                broker_failure,
            );
        })
        .map_err(SwitcherError::Io)?;
    Ok((
        BrokerHandle {
            commands,
            wake,
            accepting,
            stopping,
            health,
            failure,
        },
        thread,
    ))
}

fn run_broker(
    connection: Seqpacket,
    commands: mpsc::Receiver<BrokerCommand>,
    mut wake: UnixStream,
    stopping: Arc<AtomicBool>,
    failure: BrokerFailure,
) {
    let mut next_sequence = 1u64;
    let mut latencies = LatencyRing::new();
    loop {
        if stopping.load(Ordering::Acquire) {
            break;
        }
        match commands.try_recv() {
            Ok(command) => {
                let started = Instant::now();
                let result = exchange(&connection, &mut wake, &stopping, next_sequence, &command);
                latencies.record(started.elapsed());
                next_sequence = match next_sequence.checked_add(1) {
                    Some(next) => next,
                    None => {
                        let error = InputSafetyError::GuardianProtocol {
                            context: "guardian broker sequence exhausted",
                        };
                        failure.fail(error.clone());
                        let _ = command.reply.send(Err(error.into()));
                        break;
                    }
                };
                if let Err(error) = &result {
                    failure.fail(health_error_for(error));
                }
                let terminal_response = matches!(
                    result,
                    Ok(Response::Drained { .. } | Response::Stopped { .. })
                );
                if command.reply.send(result).is_err() {
                    failure.fail(InputSafetyError::GuardianUnavailable {
                        context: "guardian request result had no live receiver",
                    });
                    break;
                }
                if terminal_response || stopping.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => match poll_idle(&connection, &mut wake) {
                Ok(PollIdle::Wake) => {}
                Ok(PollIdle::PeerEvent) => {
                    failure.fail(InputSafetyError::GuardianUnavailable {
                        context: "guardian channel closed while idle",
                    });
                    break;
                }
                Err(error) => {
                    failure.fail(health_error_for(&error));
                    break;
                }
            },
        }
    }
    if let Some((count, p50, p95, max)) = latencies.summary() {
        let _ = try_debug_line(DebugLogKind::Input, || {
            format_input(
                "xtest-guardian-ipc",
                &format!("count={count} p50_us={p50} p95_us={p95} max_us={max}"),
            )
        });
    }
}

fn exchange(
    connection: &Seqpacket,
    wake: &mut UnixStream,
    stopping: &AtomicBool,
    sequence: u64,
    command: &BrokerCommand,
) -> Result<Response, SwitcherError> {
    if Instant::now() >= command.local_deadline {
        return Err(InputSafetyError::GuardianRequestTimedOut {
            operation_id: request_operation_id(&command.request),
        }
        .into());
    }
    let sequence = Sequence(sequence);
    let frame = encode_frame(sequence, &Message::Request(command.request))?;
    connection.send_frame(&frame)?;
    loop {
        if stopping.load(Ordering::Acquire) {
            return Err(InputSafetyError::GuardianUnavailable {
                context: "guardian broker was stopped during a request",
            }
            .into());
        }
        let remaining = command
            .local_deadline
            .checked_duration_since(Instant::now())
            .ok_or(InputSafetyError::GuardianRequestTimedOut {
                operation_id: request_operation_id(&command.request),
            })?;
        match poll_pair(connection.as_raw_fd(), wake.as_raw_fd(), Some(remaining))? {
            PollPair::Wake => drain_wake(wake)?,
            PollPair::Peer => {
                if Instant::now() >= command.local_deadline {
                    return Err(InputSafetyError::GuardianRequestTimedOut {
                        operation_id: request_operation_id(&command.request),
                    }
                    .into());
                }
                let frame = connection.recv_frame()?;
                if frame.is_empty() {
                    return Err(InputSafetyError::GuardianUnavailable {
                        context: "guardian closed the channel before replying",
                    }
                    .into());
                }
                let decoded = decode_frame(&frame)?;
                response_matches(sequence, &command.request, &decoded)?;
                let Message::Response(response) = decoded.message else {
                    return Err(InputSafetyError::GuardianProtocol {
                        context: "guardian returned a request frame",
                    }
                    .into());
                };
                if let Response::Fatal { code } = response {
                    return Err(fatal_error(code).into());
                }
                return Ok(response);
            }
            PollPair::Timeout => {
                return Err(InputSafetyError::GuardianRequestTimedOut {
                    operation_id: request_operation_id(&command.request),
                }
                .into());
            }
            PollPair::Interrupted => {}
        }
    }
}

enum PollIdle {
    Wake,
    PeerEvent,
}

fn poll_idle(connection: &Seqpacket, wake: &mut UnixStream) -> Result<PollIdle, SwitcherError> {
    match poll_pair(connection.as_raw_fd(), wake.as_raw_fd(), None)? {
        PollPair::Wake => {
            drain_wake(wake)?;
            Ok(PollIdle::Wake)
        }
        PollPair::Peer | PollPair::Timeout => Ok(PollIdle::PeerEvent),
        PollPair::Interrupted => Ok(PollIdle::Wake),
    }
}

enum PollPair {
    Wake,
    Peer,
    Timeout,
    Interrupted,
}

fn poll_pair(
    peer_fd: i32,
    wake_fd: i32,
    timeout: Option<Duration>,
) -> Result<PollPair, SwitcherError> {
    let mut descriptors = [
        libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: peer_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let timeout_ms = timeout.map_or(-1, duration_to_poll_timeout);
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(PollPair::Interrupted);
        }
        return Err(error.into());
    }
    if result == 0 {
        return Ok(PollPair::Timeout);
    }
    let wake_events = descriptors[0].revents;
    if wake_events & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Err(InputSafetyError::GuardianUnavailable {
            context: "guardian broker wake channel failed",
        }
        .into());
    }
    if wake_events & libc::POLLIN != 0 {
        return Ok(PollPair::Wake);
    }
    let peer_events = descriptors[1].revents;
    if peer_events & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Ok(PollPair::Peer);
    }
    Ok(PollPair::Interrupted)
}

fn duration_to_poll_timeout(duration: Duration) -> i32 {
    let milliseconds = duration.as_millis();
    let rounded_up = if duration.subsec_nanos() % 1_000_000 == 0 {
        milliseconds
    } else {
        milliseconds.saturating_add(1)
    };
    i32::try_from(rounded_up).unwrap_or(i32::MAX).max(1)
}

fn drain_wake(wake: &mut UnixStream) -> Result<(), SwitcherError> {
    let mut buffer = [0u8; 64];
    loop {
        match wake.read(&mut buffer) {
            Ok(0) => {
                return Err(InputSafetyError::GuardianUnavailable {
                    context: "guardian broker wake channel closed",
                }
                .into());
            }
            Ok(read) if read == buffer.len() => {}
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

fn fatal_error(code: FatalCode) -> InputSafetyError {
    let context = match code {
        FatalCode::ProtocolViolation => "guardian reported a protocol violation",
        FatalCode::DeadlineExpired => "guardian reported an expired deadline",
        FatalCode::DeadlineTooFar => "guardian reported an excessive deadline",
        FatalCode::CapacityExceeded => "guardian reported capacity exhaustion",
        FatalCode::BackendUnavailable => "guardian XTEST backend is unavailable",
        FatalCode::BackendFailure => "guardian XTEST backend failed",
        FatalCode::Unreconciled => "guardian could not reconcile synthetic input",
    };
    InputSafetyError::GuardianProtocol { context }
}

fn health_error_for(error: &SwitcherError) -> InputSafetyError {
    match error {
        SwitcherError::InputSafety(error) => error.clone(),
        _ => InputSafetyError::GuardianUnavailable {
            context: "guardian transport failed",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::synthetic_input::{
        FrozenPhysicalSnapshot, InputGeneration, OperationControl, OperationId, OperationOutcome,
        SyntheticOperation, TerminalProof,
    };
    use crate::daemon::xtest_guardian::protocol::{
        decode_frame, encode_frame, Message, PreparedToken, Request, Response, ServerEpoch,
        SessionId,
    };
    use crate::daemon::xtest_guardian::runtime::{GuardianPlanStep, GuardianSyntheticRuntime};
    use evdev::Key;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    #[derive(Clone, Copy)]
    enum RecordingBehavior {
        Ready,
        FailAfterExecuteDown,
        StallAfterExecuteDown,
        CloseAfterReady,
        CloseAfterPhysicalReleaseCommit,
        UnreconciledOnCancel,
    }

    struct FakeEmergencyRelease {
        epoch: ServerEpoch,
        trace: Arc<Mutex<Vec<String>>>,
        delay: Duration,
    }

    impl EmergencyRelease for FakeEmergencyRelease {
        fn server_epoch(&self) -> ServerEpoch {
            self.epoch
        }

        fn release_token(&mut self, token: PreparedToken) -> Result<(), SwitcherError> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            self.trace
                .lock()
                .unwrap()
                .push(format!("up:{}", token.x11_keycode));
            Ok(())
        }

        fn synchronize(&mut self) -> Result<(), SwitcherError> {
            self.trace.lock().unwrap().push("sync".to_string());
            Ok(())
        }
    }

    struct RecordingGuardian {
        client: Mutex<Option<Seqpacket>>,
        identity: X11ServerIdentity,
        requests: Arc<Mutex<Vec<Request>>>,
        emergency_trace: Arc<Mutex<Vec<String>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl RecordingGuardian {
        fn ready() -> Self {
            Self::spawn(RecordingBehavior::Ready)
        }

        fn fail_after_receiving_execute_down() -> Self {
            Self::spawn(RecordingBehavior::FailAfterExecuteDown)
        }

        fn close_after_ready() -> Self {
            Self::spawn(RecordingBehavior::CloseAfterReady)
        }

        fn close_after_physical_release_commit() -> Self {
            Self::spawn(RecordingBehavior::CloseAfterPhysicalReleaseCommit)
        }

        fn stall_after_receiving_execute_down() -> Self {
            Self::spawn(RecordingBehavior::StallAfterExecuteDown)
        }

        fn unreconciled_on_cancel() -> Self {
            Self::spawn(RecordingBehavior::UnreconciledOnCancel)
        }

        fn spawn(behavior: RecordingBehavior) -> Self {
            let (client, guardian) = Seqpacket::pair().unwrap();
            let identity = X11ServerIdentity {
                epoch: ServerEpoch([0x52; 16]),
                root: 1,
                epoch_window: 2,
                epoch_nonce: [0x53; 16],
            };
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let thread_identity = identity.clone();
            let thread = std::thread::spawn(move || {
                let mut next_token_id = 1u64;
                loop {
                    let frame = guardian.recv_frame().unwrap();
                    if frame.is_empty() {
                        break;
                    }
                    let decoded = decode_frame(&frame).unwrap();
                    let Message::Request(request) = decoded.message else {
                        panic!("recording guardian received a response");
                    };
                    thread_requests.lock().unwrap().push(request);
                    let response = match request {
                        Request::Hello { .. } => Response::Ready {
                            session: SessionId([0x51; 16]),
                            epoch: thread_identity.epoch,
                            epoch_window: thread_identity.epoch_window,
                            epoch_nonce: thread_identity.epoch_nonce,
                        },
                        Request::PrepareKey {
                            operation,
                            evdev_code,
                            ..
                        } => {
                            let token = PreparedToken {
                                session: SessionId([0x51; 16]),
                                epoch: thread_identity.epoch,
                                token_id: next_token_id,
                                evdev_code,
                                x11_keycode: u8::try_from(evdev_code + 8).unwrap(),
                            };
                            next_token_id += 1;
                            Response::Prepared { operation, token }
                        }
                        Request::ExecuteDown {
                            operation, token, ..
                        } => {
                            if matches!(behavior, RecordingBehavior::FailAfterExecuteDown) {
                                break;
                            }
                            if matches!(behavior, RecordingBehavior::StallAfterExecuteDown) {
                                std::thread::sleep(Duration::from_millis(200));
                                break;
                            }
                            Response::DownAck {
                                operation,
                                token_id: token.token_id,
                            }
                        }
                        Request::KeyUp {
                            operation, token, ..
                        } => Response::UpAck {
                            operation,
                            token_id: token.token_id,
                        },
                        Request::Synchronize {
                            operation,
                            token_id,
                            ..
                        } => Response::SyncAck {
                            operation,
                            token_id,
                        },
                        Request::CancelAndDrain { operation, .. } => Response::Drained {
                            operation,
                            proof: if matches!(behavior, RecordingBehavior::UnreconciledOnCancel) {
                                WireTerminalProof::Unreconciled { remaining: 1 }
                            } else {
                                WireTerminalProof::Reconciled
                            },
                        },
                        Request::ReleaseAllAndExit { .. } => Response::Stopped {
                            proof: WireTerminalProof::Reconciled,
                        },
                        Request::TransferToPhysicalDebt {
                            operation, token, ..
                        } => Response::TransferAck {
                            operation,
                            token_id: token.token_id,
                        },
                        Request::PhysicalReleaseCommitted {
                            sequence, token, ..
                        } => {
                            if matches!(
                                behavior,
                                RecordingBehavior::CloseAfterPhysicalReleaseCommit
                            ) {
                                break;
                            }
                            Response::ReleaseCommitAck {
                                sequence,
                                token_id: token.token_id,
                            }
                        }
                    };
                    let frame =
                        encode_frame(decoded.sequence, &Message::Response(response)).unwrap();
                    guardian.send_frame(&frame).unwrap();
                    if matches!(behavior, RecordingBehavior::CloseAfterReady)
                        && matches!(response, Response::Ready { .. })
                    {
                        break;
                    }
                    if matches!(
                        response,
                        Response::Drained { .. } | Response::Stopped { .. }
                    ) {
                        break;
                    }
                }
            });
            Self {
                client: Mutex::new(Some(client)),
                identity,
                requests,
                emergency_trace: Arc::new(Mutex::new(Vec::new())),
                thread: Some(thread),
            }
        }

        fn identity(&self) -> X11ServerIdentity {
            self.identity.clone()
        }

        fn client_transport(&self) -> Seqpacket {
            self.client.lock().unwrap().take().unwrap()
        }

        fn emergency_trace(&self) -> Arc<Mutex<Vec<String>>> {
            self.emergency_trace.clone()
        }

        fn execute_down_count(&self, token: PreparedToken) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| {
                    matches!(
                        request,
                        Request::ExecuteDown {
                            token: candidate,
                            ..
                        } if *candidate == token
                    )
                })
                .count()
        }
    }

    impl Drop for RecordingGuardian {
        fn drop(&mut self) {
            self.client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl EmergencyCoordinator {
        fn for_test(
            identity: X11ServerIdentity,
            trace: Arc<Mutex<Vec<String>>>,
        ) -> EmergencyCoordinator {
            let coordinator = EmergencyCoordinator::new();
            coordinator
                .arm(
                    FakeEmergencyRelease {
                        epoch: identity.epoch,
                        trace,
                        delay: Duration::ZERO,
                    },
                    SessionId([0x51; 16]),
                    identity.epoch,
                )
                .unwrap();
            coordinator
        }

        fn for_blocking_test(epoch: ServerEpoch, delay: Duration) -> EmergencyCoordinator {
            let coordinator = EmergencyCoordinator::new();
            coordinator
                .arm(
                    FakeEmergencyRelease {
                        epoch,
                        trace: Arc::new(Mutex::new(Vec::new())),
                        delay,
                    },
                    SessionId([0x41; 16]),
                    epoch,
                )
                .unwrap();
            coordinator
        }

        fn insert_possible_for_test(&self, token: PreparedToken) -> Result<(), SwitcherError> {
            self.insert_possible(token)
        }

        fn guardian_failed_for_test(&self) {
            self.mark_guardian_failed();
        }
    }

    impl GuardianClient {
        fn from_test_transport(
            transport: Seqpacket,
            coordinator: EmergencyCoordinator,
        ) -> Result<Self, SwitcherError> {
            let client = GuardianClient::from_connection_with_emergency(
                transport,
                Instant::now() + Duration::from_secs(1),
                coordinator,
            )?;
            if !client.broker.health.is_failed() {
                client.broker.accepting.store(true, Ordering::Release);
            }
            Ok(client)
        }
    }

    #[test]
    fn mirror_is_written_before_execute_down_can_reach_transport() {
        let fixture = RecordingGuardian::fail_after_receiving_execute_down();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let token = client
            .prepare_key(OperationId(3), Key::KEY_A, deadline)
            .unwrap();

        assert!(client
            .execute_down(OperationId(3), token, deadline)
            .is_err());
        assert_eq!(coordinator.possible_tokens(), vec![token]);
        assert_eq!(fixture.execute_down_count(token), 1);

        assert!(client
            .execute_down(OperationId(3), token, deadline)
            .is_err());
        assert_eq!(fixture.execute_down_count(token), 1);
    }

    #[test]
    fn successful_up_and_sync_remove_only_matching_mirror() {
        let fixture = RecordingGuardian::ready();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let first = client
            .prepare_key(OperationId(3), Key::KEY_A, deadline)
            .unwrap();
        let second = client
            .prepare_key(OperationId(3), Key::KEY_B, deadline)
            .unwrap();

        client
            .execute_down(OperationId(3), first, deadline)
            .unwrap();
        client.synchronize(OperationId(3), deadline).unwrap();
        client
            .execute_down(OperationId(3), second, deadline)
            .unwrap();
        client.synchronize(OperationId(3), deadline).unwrap();
        client.key_up(OperationId(3), second, deadline).unwrap();

        assert_eq!(coordinator.possible_tokens(), vec![first, second]);

        client.synchronize(OperationId(3), deadline).unwrap();
        assert_eq!(coordinator.possible_tokens(), vec![first]);
    }

    #[test]
    fn physical_debt_transfer_keeps_exact_token_in_emergency_mirror() {
        let fixture = RecordingGuardian::ready();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let token = client
            .prepare_key(OperationId(11), Key::KEY_LEFTSHIFT, deadline)
            .unwrap();
        client
            .execute_down(OperationId(11), token, deadline)
            .unwrap();
        client.synchronize(OperationId(11), deadline).unwrap();

        client
            .transfer_to_physical_debt(OperationId(11), token, InputGeneration(7), deadline)
            .unwrap();

        assert_eq!(coordinator.possible_tokens(), vec![token]);
        assert!(fixture.requests.lock().unwrap().iter().any(|request| {
            matches!(
                request,
                Request::TransferToPhysicalDebt {
                    operation: OperationId(11),
                    token: candidate,
                    input_generation: InputGeneration(7),
                    ..
                } if *candidate == token
            )
        }));
    }

    #[test]
    fn lost_release_commit_ack_forbids_next_press_of_same_modifier() {
        let fixture = RecordingGuardian::close_after_physical_release_commit();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let token = client
            .prepare_key(OperationId(15), Key::KEY_LEFTSHIFT, deadline)
            .unwrap();
        client
            .execute_down(OperationId(15), token, deadline)
            .unwrap();
        client.synchronize(OperationId(15), deadline).unwrap();
        client
            .transfer_to_physical_debt(OperationId(15), token, InputGeneration(7), deadline)
            .unwrap();

        assert!(client
            .commit_physical_release(PhysicalSequence(8), token, InputGeneration(7), deadline,)
            .is_err());
        assert!(client.health().is_failed());
        assert_eq!(coordinator.possible_tokens(), vec![token]);

        let prepare_count = fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| matches!(request, Request::PrepareKey { .. }))
            .count();
        assert!(client
            .prepare_key(OperationId(16), Key::KEY_LEFTSHIFT, deadline)
            .is_err());
        assert_eq!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| matches!(request, Request::PrepareKey { .. }))
                .count(),
            prepare_count,
        );
    }

    #[test]
    fn guardian_runtime_success_accepts_final_idempotent_synchronize() {
        let fixture = RecordingGuardian::ready();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator).unwrap();
        let mut runtime = GuardianSyntheticRuntime::new(client, InputGeneration(7)).unwrap();
        let operation_id = OperationId(12);
        let local_deadline = Instant::now() + Duration::from_secs(1);
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let (mut sink, mut session_modifiers, failure_latch) = runtime
            .prepare_operation(
                operation_id,
                deadline,
                [GuardianPlanStep::Prepared(Key::KEY_A)],
            )
            .unwrap();
        let mut operation = SyntheticOperation::new(
            operation_id,
            &mut sink,
            OperationControl::new(local_deadline, Arc::new(AtomicBool::new(false))),
            FrozenPhysicalSnapshot::default(),
            failure_latch,
        );

        let press = operation.press(Key::KEY_A).unwrap();
        operation.release(press).unwrap();
        let report = operation.finish_success(&mut session_modifiers);

        assert_eq!(report.outcome, OperationOutcome::Success);
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(report.cleanup.is_none());
    }

    #[test]
    fn guardian_runtime_preflights_and_reconciles_physical_shift_trace() {
        let fixture = RecordingGuardian::ready();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let mut runtime = GuardianSyntheticRuntime::new(client, InputGeneration(7)).unwrap();
        let operation_id = OperationId(13);
        let local_deadline = Instant::now() + Duration::from_secs(1);
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let plan = [
            GuardianPlanStep::PhysicalRelease(Key::KEY_LEFTSHIFT),
            GuardianPlanStep::Prepared(Key::KEY_BACKSPACE),
            GuardianPlanStep::Prepared(Key::KEY_LEFTSHIFT),
            GuardianPlanStep::Prepared(Key::KEY_G),
            GuardianPlanStep::Prepared(Key::KEY_LEFTSHIFT),
        ];
        let (mut sink, mut session_modifiers, failure_latch) = runtime
            .prepare_operation(operation_id, deadline, plan)
            .unwrap();
        let mut operation = SyntheticOperation::new(
            operation_id,
            &mut sink,
            OperationControl::new(local_deadline, Arc::new(AtomicBool::new(false))),
            FrozenPhysicalSnapshot::from_pressed_modifiers([Key::KEY_LEFTSHIFT], false).unwrap(),
            failure_latch,
        );

        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        let backspace = operation.press(Key::KEY_BACKSPACE).unwrap();
        operation.release(backspace).unwrap();
        let shift = operation.press(Key::KEY_LEFTSHIFT).unwrap();
        let letter = operation.press(Key::KEY_G).unwrap();
        operation.release(letter).unwrap();
        operation.release(shift).unwrap();
        let report = operation.finish_success(&mut session_modifiers);

        assert_eq!(report.outcome, OperationOutcome::Success);
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(report.cleanup.is_none());

        let requests = fixture.requests.lock().unwrap();
        let prepared_codes: Vec<_> = requests
            .iter()
            .filter_map(|request| match request {
                Request::PrepareKey { evdev_code, .. } => Some(*evdev_code),
                _ => None,
            })
            .collect();
        assert_eq!(
            prepared_codes,
            [
                Key::KEY_LEFTSHIFT.code(),
                Key::KEY_BACKSPACE.code(),
                Key::KEY_LEFTSHIFT.code(),
                Key::KEY_G.code(),
                Key::KEY_LEFTSHIFT.code(),
            ]
        );
        let last_prepare = requests
            .iter()
            .rposition(|request| matches!(request, Request::PrepareKey { .. }))
            .unwrap();
        let first_mutation = requests
            .iter()
            .position(|request| {
                matches!(request, Request::ExecuteDown { .. } | Request::KeyUp { .. })
            })
            .unwrap();
        assert!(last_prepare < first_mutation);
        assert!(matches!(
            requests.get(first_mutation),
            Some(Request::KeyUp { token, .. })
                if token.evdev_code == Key::KEY_LEFTSHIFT.code()
        ));
        assert!(matches!(
            requests.last(),
            Some(Request::TransferToPhysicalDebt {
                input_generation: InputGeneration(7),
                token,
                ..
            }) if token.evdev_code == Key::KEY_LEFTSHIFT.code()
        ));
        drop(requests);
        let first_debt = coordinator.possible_tokens();
        assert_eq!(first_debt.len(), 1);
        drop(session_modifiers);
        drop(sink);
        assert!(runtime.has_session_modifier(Key::KEY_LEFTSHIFT));

        let operation_id = OperationId(14);
        let local_deadline = Instant::now() + Duration::from_secs(1);
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let (mut sink, mut session_modifiers, failure_latch) = runtime
            .prepare_operation(
                operation_id,
                deadline,
                [
                    GuardianPlanStep::PhysicalRelease(Key::KEY_LEFTSHIFT),
                    GuardianPlanStep::Prepared(Key::KEY_H),
                    GuardianPlanStep::Prepared(Key::KEY_LEFTSHIFT),
                ],
            )
            .unwrap();
        let mut operation = SyntheticOperation::new(
            operation_id,
            &mut sink,
            OperationControl::new(local_deadline, Arc::new(AtomicBool::new(false))),
            FrozenPhysicalSnapshot::from_pressed_modifiers([Key::KEY_LEFTSHIFT], false).unwrap(),
            failure_latch,
        );
        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        session_modifiers
            .mark_temporarily_released(Key::KEY_LEFTSHIFT)
            .unwrap();
        let letter = operation.press(Key::KEY_H).unwrap();
        operation.release(letter).unwrap();
        let report = operation.finish_success(&mut session_modifiers);

        assert_eq!(report.outcome, OperationOutcome::Success);
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(report.cleanup.is_none());
        drop(session_modifiers);
        drop(sink);
        let second_debt = coordinator.possible_tokens();
        assert_eq!(second_debt.len(), 1);
        assert_ne!(second_debt, first_debt);
        assert!(runtime.has_session_modifier(Key::KEY_LEFTSHIFT));

        runtime
            .commit_physical_release(
                PhysicalSequence(44),
                Key::KEY_LEFTSHIFT,
                GuardianMutationDeadline::for_test(Duration::from_secs(1)),
            )
            .unwrap();

        assert!(!runtime.has_session_modifier(Key::KEY_LEFTSHIFT));
        assert!(coordinator.possible_tokens().is_empty());
        assert!(matches!(
            fixture.requests.lock().unwrap().last(),
            Some(Request::PhysicalReleaseCommitted {
                sequence: PhysicalSequence(44),
                input_generation: InputGeneration(7),
                token,
                ..
            }) if *token == second_debt[0]
        ));
    }

    #[test]
    fn guardian_failure_only_arms_emergency_until_controller_starts_it() {
        let fixture = RecordingGuardian::fail_after_receiving_execute_down();
        let emergency_trace = fixture.emergency_trace();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let token = client
            .prepare_key(OperationId(4), Key::KEY_LEFTSHIFT, deadline)
            .unwrap();

        assert!(client
            .execute_down(OperationId(4), token, deadline)
            .is_err());
        assert!(emergency_trace.lock().unwrap().is_empty());

        let job = coordinator.start_pending_release().unwrap();
        assert_eq!(job.wait(Duration::from_secs(1)), TerminalProof::Reconciled);
        assert_eq!(
            *emergency_trace.lock().unwrap(),
            vec![format!("up:{}", token.x11_keycode), "sync".to_string()]
        );
    }

    #[test]
    fn emergency_wait_is_hard_bounded() {
        let token = PreparedToken {
            session: SessionId([0x41; 16]),
            epoch: ServerEpoch([0x42; 16]),
            token_id: 1,
            evdev_code: Key::KEY_A.code(),
            x11_keycode: 38,
        };
        let coordinator =
            EmergencyCoordinator::for_blocking_test(token.epoch, Duration::from_millis(200));
        coordinator.insert_possible_for_test(token).unwrap();
        coordinator.guardian_failed_for_test();
        let started = std::time::Instant::now();
        let proof = coordinator
            .start_pending_release()
            .unwrap()
            .wait(Duration::from_millis(20));

        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(proof, TerminalProof::Unreconciled { remaining: 1 });
    }

    #[test]
    fn idle_guardian_hup_closes_gate_without_starting_emergency() {
        let fixture = RecordingGuardian::close_after_ready();
        let trace = fixture.emergency_trace();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !client.health().is_failed() && Instant::now() < deadline {
            std::thread::yield_now();
        }

        assert!(client.health().is_failed());
        assert!(trace.lock().unwrap().is_empty());
        assert_eq!(coordinator.terminal_proof(), TerminalProof::Reconciled);
    }

    #[test]
    fn emergency_releases_mirror_in_reverse_and_synchronizes_once() {
        let epoch = ServerEpoch([0x62; 16]);
        let trace = Arc::new(Mutex::new(Vec::new()));
        let coordinator = EmergencyCoordinator::new();
        coordinator
            .arm(
                FakeEmergencyRelease {
                    epoch,
                    trace: trace.clone(),
                    delay: Duration::ZERO,
                },
                SessionId([0x61; 16]),
                epoch,
            )
            .unwrap();
        for (token_id, key) in [(1, Key::KEY_A), (2, Key::KEY_B)] {
            coordinator
                .insert_possible_for_test(PreparedToken {
                    session: SessionId([0x61; 16]),
                    epoch,
                    token_id,
                    evdev_code: key.code(),
                    x11_keycode: u8::try_from(key.code() + 8).unwrap(),
                })
                .unwrap();
        }
        coordinator.guardian_failed_for_test();

        assert_eq!(
            coordinator
                .start_pending_release()
                .unwrap()
                .wait(Duration::from_secs(1)),
            TerminalProof::Reconciled
        );
        assert_eq!(
            *trace.lock().unwrap(),
            vec!["up:56".to_string(), "up:38".to_string(), "sync".to_string()]
        );
    }

    #[test]
    fn latency_ring_is_bounded_and_reports_order_statistics() {
        let mut ring = LatencyRing::new();
        for micros in 1..=LATENCY_SAMPLE_CAPACITY as u64 + 3 {
            ring.record(Duration::from_micros(micros));
        }

        assert_eq!(
            ring.summary(),
            Some((LATENCY_SAMPLE_CAPACITY, 259, 489, 515))
        );
    }

    #[test]
    fn forged_session_token_is_rejected_before_execute_down_transport() {
        let fixture = RecordingGuardian::ready();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let prepared = client
            .prepare_key(OperationId(7), Key::KEY_A, deadline)
            .unwrap();
        let forged = PreparedToken {
            session: SessionId([0xEE; 16]),
            ..prepared
        };

        assert!(client
            .execute_down(OperationId(7), forged, deadline)
            .is_err());
        assert_eq!(fixture.execute_down_count(forged), 0);
        assert!(coordinator.possible_tokens().is_empty());
        assert!(client.health().is_failed());
    }

    #[test]
    fn execute_down_timeout_is_bounded_and_never_retried() {
        let fixture = RecordingGuardian::stall_after_receiving_execute_down();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_millis(30));
        let token = client
            .prepare_key(OperationId(8), Key::KEY_A, deadline)
            .unwrap();
        let started = Instant::now();

        assert!(client
            .execute_down(OperationId(8), token, deadline)
            .is_err());

        assert!(started.elapsed() < Duration::from_millis(150));
        assert!(client.health().is_failed());
        assert_eq!(coordinator.possible_tokens(), vec![token]);
        assert_eq!(fixture.execute_down_count(token), 1);
        assert!(client
            .execute_down(OperationId(8), token, deadline)
            .is_err());
        assert_eq!(fixture.execute_down_count(token), 1);
    }

    #[test]
    fn unreconciled_drain_preserves_emergency_connection_and_mirror() {
        let fixture = RecordingGuardian::unreconciled_on_cancel();
        let trace = fixture.emergency_trace();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator.clone())
                .unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let token = client
            .prepare_key(OperationId(9), Key::KEY_A, deadline)
            .unwrap();
        client
            .execute_down(OperationId(9), token, deadline)
            .unwrap();
        client.synchronize(OperationId(9), deadline).unwrap();

        assert_eq!(
            client.cancel_and_drain(OperationId(9)),
            TerminalProof::Unreconciled { remaining: 1 }
        );
        assert_eq!(coordinator.possible_tokens(), vec![token]);
        assert_eq!(
            coordinator
                .start_pending_release()
                .unwrap()
                .wait(Duration::from_secs(1)),
            TerminalProof::Reconciled
        );
        assert_eq!(
            *trace.lock().unwrap(),
            vec!["up:38".to_string(), "sync".to_string()]
        );
    }

    #[test]
    fn unused_prepared_token_forces_terminal_drain_instead_of_false_reconciled() {
        let fixture = RecordingGuardian::ready();
        let coordinator =
            EmergencyCoordinator::for_test(fixture.identity(), fixture.emergency_trace());
        let mut client =
            GuardianClient::from_test_transport(fixture.client_transport(), coordinator).unwrap();
        let deadline = GuardianMutationDeadline::for_test(Duration::from_secs(1));
        let operation = OperationId(10);
        let mut sink = GuardianSyntheticSink::new(&mut client, operation, deadline);
        let _unused = sink.prepare_down(Key::KEY_A).unwrap();

        assert_eq!(sink.terminal_proof(0), TerminalProof::Reconciled);
        assert!(!client.has_any_prepared());
    }
}
