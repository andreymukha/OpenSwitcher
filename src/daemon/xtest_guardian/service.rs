use crate::daemon::synthetic_input::TerminalProof;
use crate::daemon::xtest_guardian::protocol::{
    decode_frame, encode_frame, FatalCode, Message, MutationDeadlineNs, PreparedToken,
    ProtocolSession, ProtocolState, Request, Response, Sequence, ServerEpoch, WireTerminalProof,
    MAX_RELEASE_CLEANUP_NS, MAX_TRANSACTION_TIMEOUT_NS,
};
use crate::daemon::xtest_guardian::seqpacket::Seqpacket;
use crate::error::{InputSafetyError, SwitcherError};
use nix::sys::time::TimeValLike;
use nix::time::{clock_gettime, ClockId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X11ServerIdentity {
    pub(crate) epoch: ServerEpoch,
    pub(crate) root: u32,
    pub(crate) epoch_window: u32,
    pub(crate) epoch_nonce: [u8; 16],
}

pub(crate) trait XtestExecutor {
    fn server_identity(&self) -> &X11ServerIdentity;
    fn prepare_key(&mut self, evdev_code: u16) -> Result<(u8, ServerEpoch), InputSafetyError>;
    fn key_down(&mut self, keycode: u8) -> Result<(), InputSafetyError>;
    fn key_up(&mut self, keycode: u8) -> Result<(), InputSafetyError>;
    fn synchronize(&mut self) -> Result<(), InputSafetyError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopReason {
    PeerEof,
    Sigterm,
    ProtocolViolation,
    BackendFailure,
    Requested,
    Drop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalRecord {
    pub(crate) reason: StopReason,
    pub(crate) proof: TerminalProof,
}

struct RequestFailure {
    code: FatalCode,
    reason: StopReason,
    error: SwitcherError,
}

impl RequestFailure {
    fn protocol(error: SwitcherError) -> Self {
        Self {
            code: protocol_fatal_code(&error),
            reason: StopReason::ProtocolViolation,
            error,
        }
    }

    fn backend(error: InputSafetyError) -> Self {
        Self {
            code: FatalCode::BackendFailure,
            reason: StopReason::BackendFailure,
            error: error.into(),
        }
    }
}

pub(crate) struct GuardianSession<'a, E: XtestExecutor> {
    executor: &'a mut E,
    protocol: ProtocolState,
    next_token_id: u64,
    terminal: Option<TerminalRecord>,
}

impl<'a, E: XtestExecutor> GuardianSession<'a, E> {
    pub(crate) fn ready(
        session: ProtocolSession,
        executor: &'a mut E,
    ) -> Result<Self, SwitcherError> {
        Self::ready_with_protocol(session, executor, ProtocolState::ready(session)?)
    }

    fn ready_after_handshake(
        session: ProtocolSession,
        handshake_sequence: Sequence,
        executor: &'a mut E,
    ) -> Result<Self, SwitcherError> {
        Self::ready_with_protocol(
            session,
            executor,
            ProtocolState::ready_after_handshake(session, handshake_sequence)?,
        )
    }

    fn ready_with_protocol(
        session: ProtocolSession,
        executor: &'a mut E,
        protocol: ProtocolState,
    ) -> Result<Self, SwitcherError> {
        if executor.server_identity().epoch != session.epoch {
            return Err(SwitcherError::input_safety(
                "XTEST executor epoch does not match guardian session",
            ));
        }
        Ok(Self {
            executor,
            protocol,
            next_token_id: 1,
            terminal: None,
        })
    }

    pub(crate) fn ready_response(&self) -> Response {
        let identity = self.executor.server_identity();
        Response::Ready {
            session: self.protocol.session().session,
            epoch: identity.epoch,
            epoch_window: identity.epoch_window,
            epoch_nonce: identity.epoch_nonce,
        }
    }

    pub(crate) fn handle_request(
        &mut self,
        sequence: Sequence,
        request: Request,
        now_ns: u64,
    ) -> Result<Response, SwitcherError> {
        self.handle_request_detailed(sequence, request, now_ns)
            .map_err(|failure| failure.error)
    }

    fn handle_request_detailed(
        &mut self,
        sequence: Sequence,
        request: Request,
        now_ns: u64,
    ) -> Result<Response, RequestFailure> {
        if self.terminal.is_some() {
            return Err(RequestFailure::protocol(SwitcherError::input_safety(
                "XTEST guardian request arrived after session termination",
            )));
        }
        self.protocol
            .accept(sequence, &request, now_ns)
            .map_err(RequestFailure::protocol)?;

        match request {
            Request::Hello { .. } => Err(RequestFailure::protocol(SwitcherError::input_safety(
                "XTEST guardian ready session cannot accept another hello",
            ))),
            Request::PrepareKey {
                operation,
                evdev_code,
                ..
            } => {
                let (x11_keycode, epoch) = self
                    .executor
                    .prepare_key(evdev_code)
                    .map_err(RequestFailure::backend)?;
                if epoch != self.executor.server_identity().epoch {
                    return Err(RequestFailure::backend(InputSafetyError::Invariant {
                        context: "XTEST mapping epoch changed during key preparation",
                    }));
                }
                let token = PreparedToken {
                    session: self.protocol.session().session,
                    epoch,
                    token_id: self.next_token_id,
                    evdev_code,
                    x11_keycode,
                };
                self.next_token_id = self
                    .next_token_id
                    .checked_add(1)
                    .ok_or(InputSafetyError::Invariant {
                        context: "XTEST guardian token identifier exhausted",
                    })
                    .map_err(RequestFailure::backend)?;
                self.protocol
                    .record_prepared(operation, token)
                    .map_err(RequestFailure::protocol)?;
                Ok(Response::Prepared { operation, token })
            }
            Request::ExecuteDown {
                operation, token, ..
            } => {
                let down_result = self.executor.key_down(token.x11_keycode);
                self.protocol
                    .record_down_attempt(token.token_id)
                    .map_err(RequestFailure::protocol)?;
                down_result.map_err(RequestFailure::backend)?;
                Ok(Response::DownAck {
                    operation,
                    token_id: token.token_id,
                })
            }
            Request::KeyUp {
                operation, token, ..
            } => {
                self.executor
                    .key_up(token.x11_keycode)
                    .map_err(RequestFailure::backend)?;
                Ok(Response::UpAck {
                    operation,
                    token_id: token.token_id,
                })
            }
            Request::Synchronize {
                operation,
                token_id,
                ..
            } => {
                self.executor
                    .synchronize()
                    .map_err(RequestFailure::backend)?;
                self.protocol
                    .complete_synchronize(token_id)
                    .map_err(RequestFailure::protocol)?;
                Ok(Response::SyncAck {
                    operation,
                    token_id,
                })
            }
            Request::TransferToPhysicalDebt {
                operation, token, ..
            } => Ok(Response::TransferAck {
                operation,
                token_id: token.token_id,
            }),
            Request::PhysicalReleaseCommitted {
                sequence, token, ..
            } => Ok(Response::ReleaseCommitAck {
                sequence,
                token_id: token.token_id,
            }),
            Request::CancelAndDrain {
                operation,
                deadline,
            } => {
                let proof = self.finish_until_with_clock(
                    StopReason::Requested,
                    deadline.0,
                    monotonic_now_ns_or_zero,
                );
                Ok(Response::Drained {
                    operation,
                    proof: wire_terminal_proof(&proof),
                })
            }
            Request::ReleaseAllAndExit { deadline } => {
                let proof = self.finish_until_with_clock(
                    StopReason::Requested,
                    deadline.0,
                    monotonic_now_ns_or_zero,
                );
                Ok(Response::Stopped {
                    proof: wire_terminal_proof(&proof),
                })
            }
        }
    }

    pub(crate) fn finish_with_clock(
        &mut self,
        reason: StopReason,
        mut clock: impl FnMut() -> u64,
    ) -> TerminalProof {
        if let Some(record) = &self.terminal {
            return record.proof.clone();
        }
        let deadline = clock().saturating_add(MAX_RELEASE_CLEANUP_NS);
        self.finish_until_with_clock(reason, deadline, clock)
    }

    fn finish_until_with_clock(
        &mut self,
        reason: StopReason,
        deadline_ns: u64,
        mut clock: impl FnMut() -> u64,
    ) -> TerminalProof {
        if let Some(record) = &self.terminal {
            return record.proof.clone();
        }

        self.protocol.begin_terminal();
        for token in self.protocol.cleanup_tokens_reverse() {
            if clock() >= deadline_ns {
                break;
            }
            let released = self.executor.key_up(token.x11_keycode).is_ok();
            if clock() >= deadline_ns {
                break;
            }
            let synchronized = self.executor.synchronize().is_ok();
            if released && synchronized {
                let _ = self.protocol.acknowledge_cleanup_release(token.token_id);
            }
        }

        let remaining = self.protocol.debt_count();
        let proof = if remaining == 0 {
            TerminalProof::Reconciled
        } else {
            TerminalProof::Unreconciled { remaining }
        };
        self.terminal = Some(TerminalRecord {
            reason,
            proof: proof.clone(),
        });
        proof
    }

    pub(crate) fn debt_count(&self) -> usize {
        self.protocol.debt_count()
    }

    pub(crate) fn executor_ref(&self) -> &E {
        self.executor
    }

    pub(crate) fn terminal_record(&self) -> Option<&TerminalRecord> {
        self.terminal.as_ref()
    }
}

impl<E: XtestExecutor> Drop for GuardianSession<'_, E> {
    fn drop(&mut self) {
        if self.terminal.is_none() {
            let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.finish_with_clock(StopReason::Drop, monotonic_now_ns_or_zero)
            }));
            if cleanup.is_err() && self.terminal.is_none() {
                self.protocol.begin_terminal();
                self.terminal = Some(TerminalRecord {
                    reason: StopReason::Drop,
                    proof: TerminalProof::Unreconciled {
                        remaining: self.protocol.debt_count(),
                    },
                });
            }
        }
    }
}

pub(crate) fn run_connection<E: XtestExecutor>(
    connection: &Seqpacket,
    protocol_session: ProtocolSession,
    executor: &mut E,
) -> Result<TerminalRecord, SwitcherError> {
    let frame = connection.recv_frame()?;
    if frame.is_empty() {
        return Err(SwitcherError::input_safety(
            "XTEST guardian peer closed before handshake",
        ));
    }
    let decoded = decode_frame(&frame)?;
    let Message::Request(Request::Hello {
        daemon_nonce,
        deadline,
    }) = decoded.message
    else {
        let error =
            SwitcherError::input_safety("XTEST guardian first frame is not a hello request");
        send_fatal_best_effort(connection, decoded.sequence, protocol_fatal_code(&error));
        return Err(error);
    };
    if let Err(error) = validate_hello(daemon_nonce, deadline, monotonic_now_ns()?) {
        send_fatal_best_effort(connection, decoded.sequence, protocol_fatal_code(&error));
        return Err(error);
    }

    let mut session = match GuardianSession::ready_after_handshake(
        protocol_session,
        decoded.sequence,
        executor,
    ) {
        Ok(session) => session,
        Err(error) => {
            send_fatal_best_effort(connection, decoded.sequence, protocol_fatal_code(&error));
            return Err(error);
        }
    };
    send_response(connection, decoded.sequence, session.ready_response())?;

    loop {
        let frame = match connection.recv_frame() {
            Ok(frame) => frame,
            Err(_) => return Ok(finish_record(&mut session, StopReason::PeerEof)),
        };
        if frame.is_empty() {
            return Ok(finish_record(&mut session, StopReason::PeerEof));
        }

        let decoded = match decode_frame(&frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                send_fatal_best_effort(connection, Sequence(1), protocol_fatal_code(&error));
                return Ok(finish_record(&mut session, StopReason::ProtocolViolation));
            }
        };
        let Message::Request(request) = decoded.message else {
            send_fatal_best_effort(connection, decoded.sequence, FatalCode::ProtocolViolation);
            return Ok(finish_record(&mut session, StopReason::ProtocolViolation));
        };

        let response =
            match session.handle_request_detailed(decoded.sequence, request, monotonic_now_ns()?) {
                Ok(response) => response,
                Err(failure) => {
                    send_fatal_best_effort(connection, decoded.sequence, failure.code);
                    return Ok(finish_record(&mut session, failure.reason));
                }
            };
        let terminal_response = matches!(
            response,
            Response::Drained { .. } | Response::Stopped { .. }
        );
        if send_response(connection, decoded.sequence, response).is_err() {
            return Ok(finish_record(&mut session, StopReason::PeerEof));
        }
        if terminal_response {
            return Ok(session
                .terminal_record()
                .cloned()
                .unwrap_or_else(|| TerminalRecord {
                    reason: StopReason::Requested,
                    proof: TerminalProof::Unreconciled {
                        remaining: session.debt_count(),
                    },
                }));
        }
    }
}

fn finish_record<E: XtestExecutor>(
    session: &mut GuardianSession<'_, E>,
    reason: StopReason,
) -> TerminalRecord {
    session.finish_with_clock(reason, monotonic_now_ns_or_zero);
    session
        .terminal_record()
        .cloned()
        .unwrap_or_else(|| TerminalRecord {
            reason,
            proof: TerminalProof::Unreconciled {
                remaining: session.debt_count(),
            },
        })
}

fn send_response(
    connection: &Seqpacket,
    sequence: Sequence,
    response: Response,
) -> Result<(), SwitcherError> {
    let frame = encode_frame(sequence, &Message::Response(response))?;
    connection.send_frame(&frame)
}

fn send_fatal_best_effort(connection: &Seqpacket, sequence: Sequence, code: FatalCode) {
    let _ = send_response(connection, sequence, Response::Fatal { code });
}

fn validate_hello(
    daemon_nonce: [u8; 16],
    deadline: MutationDeadlineNs,
    now_ns: u64,
) -> Result<(), SwitcherError> {
    if daemon_nonce.iter().all(|byte| *byte == 0) {
        return Err(SwitcherError::input_safety(
            "XTEST guardian daemon nonce must be nonzero",
        ));
    }
    let Some(delta) = deadline.0.checked_sub(now_ns) else {
        return Err(SwitcherError::input_safety(
            "XTEST guardian hello deadline expired",
        ));
    };
    if delta == 0 {
        return Err(SwitcherError::input_safety(
            "XTEST guardian hello deadline expired",
        ));
    }
    if delta > MAX_TRANSACTION_TIMEOUT_NS {
        return Err(SwitcherError::input_safety(
            "XTEST guardian hello deadline is too far in the future",
        ));
    }
    Ok(())
}

fn protocol_fatal_code(error: &SwitcherError) -> FatalCode {
    match error {
        SwitcherError::InputSafety(InputSafetyError::OversizedFrame { .. }) => {
            FatalCode::CapacityExceeded
        }
        SwitcherError::InputSafety(InputSafetyError::Invariant { context })
            if context.contains("expired") =>
        {
            FatalCode::DeadlineExpired
        }
        SwitcherError::InputSafety(InputSafetyError::Invariant { context })
            if context.contains("too far") =>
        {
            FatalCode::DeadlineTooFar
        }
        SwitcherError::InputSafety(InputSafetyError::Invariant { context })
            if context.contains("capacity") || context.contains("too large") =>
        {
            FatalCode::CapacityExceeded
        }
        _ => FatalCode::ProtocolViolation,
    }
}

fn wire_terminal_proof(proof: &TerminalProof) -> WireTerminalProof {
    match *proof {
        TerminalProof::Reconciled => WireTerminalProof::Reconciled,
        TerminalProof::OwnerGenerationDestroyed { generation } => {
            WireTerminalProof::OwnerGenerationDestroyed { generation }
        }
        TerminalProof::Unreconciled { remaining } => WireTerminalProof::Unreconciled {
            remaining: u16::try_from(remaining).unwrap_or(u16::MAX),
        },
    }
}

pub(crate) fn monotonic_now_ns() -> Result<u64, SwitcherError> {
    let nanoseconds = clock_gettime(ClockId::CLOCK_MONOTONIC)
        .map_err(|error| std::io::Error::from_raw_os_error(error as i32))?
        .num_nanoseconds();
    u64::try_from(nanoseconds)
        .map_err(|_| SwitcherError::input_safety("CLOCK_MONOTONIC returned a negative timestamp"))
}

fn monotonic_now_ns_or_zero() -> u64 {
    monotonic_now_ns().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::synthetic_input::{InputGeneration, OperationId, TerminalProof};
    use crate::daemon::xtest_guardian::protocol::{
        MutationDeadlineNs, PreparedToken, ProtocolSession, ReleaseDeadline, Request, Response,
        Sequence, ServerEpoch, SessionId, MAX_RELEASE_CLEANUP_NS,
    };
    use crate::error::InputSafetyError;

    const NOW_NS: u64 = 10_000_000_000;
    const DEADLINE: MutationDeadlineNs = MutationDeadlineNs(NOW_NS + 1_000_000);

    struct FakeXtestExecutor {
        identity: X11ServerIdentity,
        downs: Vec<u8>,
        release_attempts: Vec<u8>,
        successful_releases: Vec<u8>,
        synchronize_attempts: usize,
        fail_release_number: Option<usize>,
        fail_down_after_apply: bool,
    }

    impl Default for FakeXtestExecutor {
        fn default() -> Self {
            Self {
                identity: test_identity(),
                downs: Vec::new(),
                release_attempts: Vec::new(),
                successful_releases: Vec::new(),
                synchronize_attempts: 0,
                fail_release_number: None,
                fail_down_after_apply: false,
            }
        }
    }

    impl FakeXtestExecutor {
        fn fail_release_number(number: usize) -> Self {
            Self {
                identity: test_identity(),
                fail_release_number: Some(number),
                ..Self::default()
            }
        }

        fn fail_down_after_apply() -> Self {
            Self {
                identity: test_identity(),
                fail_down_after_apply: true,
                ..Self::default()
            }
        }
    }

    impl XtestExecutor for FakeXtestExecutor {
        fn server_identity(&self) -> &X11ServerIdentity {
            &self.identity
        }

        fn prepare_key(&mut self, evdev_code: u16) -> Result<(u8, ServerEpoch), InputSafetyError> {
            Ok(((evdev_code + 8) as u8, self.identity.epoch))
        }

        fn key_down(&mut self, keycode: u8) -> Result<(), InputSafetyError> {
            self.downs.push(keycode);
            if self.fail_down_after_apply {
                return Err(InputSafetyError::Invariant {
                    context: "fake down failed after apply",
                });
            }
            Ok(())
        }

        fn key_up(&mut self, keycode: u8) -> Result<(), InputSafetyError> {
            self.release_attempts.push(keycode);
            if self.fail_release_number == Some(self.release_attempts.len()) {
                return Err(InputSafetyError::Invariant {
                    context: "fake release failure",
                });
            }
            self.successful_releases.push(keycode);
            Ok(())
        }

        fn synchronize(&mut self) -> Result<(), InputSafetyError> {
            self.synchronize_attempts += 1;
            Ok(())
        }
    }

    fn test_identity() -> X11ServerIdentity {
        X11ServerIdentity {
            epoch: ServerEpoch([0x22; 16]),
            root: 1,
            epoch_window: 2,
            epoch_nonce: [0x33; 16],
        }
    }

    fn test_session() -> ProtocolSession {
        ProtocolSession {
            session: SessionId([0x11; 16]),
            epoch: test_identity().epoch,
        }
    }

    fn apply_down(
        session: &mut GuardianSession<'_, FakeXtestExecutor>,
        sequence: &mut u64,
        operation: OperationId,
        evdev_code: u16,
    ) -> PreparedToken {
        let prepared = session
            .handle_request(
                Sequence(*sequence),
                Request::PrepareKey {
                    operation,
                    evdev_code,
                    deadline: DEADLINE,
                },
                NOW_NS,
            )
            .unwrap();
        *sequence += 1;
        let Response::Prepared { token, .. } = prepared else {
            panic!("expected prepared response");
        };
        assert!(matches!(
            session
                .handle_request(
                    Sequence(*sequence),
                    Request::ExecuteDown {
                        operation,
                        token,
                        deadline: DEADLINE,
                    },
                    NOW_NS,
                )
                .unwrap(),
            Response::DownAck { .. }
        ));
        *sequence += 1;
        assert!(matches!(
            session
                .handle_request(
                    Sequence(*sequence),
                    Request::Synchronize {
                        operation,
                        token_id: token.token_id,
                        deadline: ReleaseDeadline::Mutation(DEADLINE),
                    },
                    NOW_NS,
                )
                .unwrap(),
            Response::SyncAck { .. }
        ));
        *sequence += 1;
        token
    }

    fn session_with_two_debts(
        executor: &mut FakeXtestExecutor,
    ) -> GuardianSession<'_, FakeXtestExecutor> {
        let mut session = GuardianSession::ready(test_session(), executor).unwrap();
        let mut sequence = 1;
        apply_down(&mut session, &mut sequence, OperationId(1), 30);
        apply_down(&mut session, &mut sequence, OperationId(1), 42);
        session
    }

    #[test]
    fn eof_drains_authoritative_temporary_and_session_debt_in_reverse_order() {
        let mut executor = FakeXtestExecutor {
            identity: test_identity(),
            ..FakeXtestExecutor::default()
        };
        let mut session = GuardianSession::ready(test_session(), &mut executor).unwrap();
        let mut sequence = 1;
        let temporary = apply_down(&mut session, &mut sequence, OperationId(1), 30);
        let modifier = apply_down(&mut session, &mut sequence, OperationId(1), 42);
        assert!(matches!(
            session
                .handle_request(
                    Sequence(sequence),
                    Request::TransferToPhysicalDebt {
                        operation: OperationId(1),
                        token: modifier,
                        input_generation: InputGeneration(7),
                        deadline: DEADLINE,
                    },
                    NOW_NS,
                )
                .unwrap(),
            Response::TransferAck { .. }
        ));

        let proof = session.finish_with_clock(StopReason::PeerEof, || NOW_NS);

        assert_eq!(
            session.executor_ref().release_attempts,
            [modifier.x11_keycode, temporary.x11_keycode]
        );
        assert_eq!(proof, TerminalProof::Reconciled);
        assert_eq!(session.debt_count(), 0);
    }

    #[test]
    fn cleanup_continues_after_first_executor_error_and_reports_unreconciled() {
        let mut executor = FakeXtestExecutor::fail_release_number(1);
        let mut session = session_with_two_debts(&mut executor);

        let proof = session.finish_with_clock(StopReason::PeerEof, || NOW_NS);

        assert_eq!(session.executor_ref().release_attempts.len(), 2);
        assert_eq!(proof, TerminalProof::Unreconciled { remaining: 1 });
        assert_eq!(session.debt_count(), 1);
    }

    #[test]
    fn applied_down_that_returns_error_remains_debt_until_terminal_release() {
        let mut executor = FakeXtestExecutor::fail_down_after_apply();
        let mut session = GuardianSession::ready(test_session(), &mut executor).unwrap();
        let prepared = session
            .handle_request(
                Sequence(1),
                Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: DEADLINE,
                },
                NOW_NS,
            )
            .unwrap();
        let Response::Prepared { token, .. } = prepared else {
            panic!("expected prepared token");
        };

        assert!(session
            .handle_request(
                Sequence(2),
                Request::ExecuteDown {
                    operation: OperationId(1),
                    token,
                    deadline: DEADLINE,
                },
                NOW_NS,
            )
            .is_err());
        assert_eq!(session.debt_count(), 1);

        let proof = session.finish_with_clock(StopReason::BackendFailure, || NOW_NS);
        assert_eq!(proof, TerminalProof::Reconciled);
        assert_eq!(session.executor_ref().downs, [token.x11_keycode]);
        assert_eq!(session.executor_ref().release_attempts, [token.x11_keycode]);
    }

    #[test]
    fn successful_key_up_is_not_forgotten_until_synchronize_succeeds() {
        let mut executor = FakeXtestExecutor {
            identity: test_identity(),
            ..FakeXtestExecutor::default()
        };
        let mut session = GuardianSession::ready(test_session(), &mut executor).unwrap();
        let mut sequence = 1;
        let token = apply_down(&mut session, &mut sequence, OperationId(1), 30);
        session
            .handle_request(
                Sequence(sequence),
                Request::KeyUp {
                    operation: OperationId(1),
                    token,
                    deadline: ReleaseDeadline::Mutation(DEADLINE),
                },
                NOW_NS,
            )
            .unwrap();

        assert_eq!(session.debt_count(), 1);
        let proof = session.finish_with_clock(StopReason::PeerEof, || NOW_NS);
        assert_eq!(proof, TerminalProof::Reconciled);
        assert_eq!(
            session.executor_ref().release_attempts,
            [token.x11_keycode, token.x11_keycode]
        );
    }

    #[test]
    fn cleanup_deadline_expiry_between_up_and_sync_keeps_debt() {
        let mut executor = FakeXtestExecutor {
            identity: test_identity(),
            ..FakeXtestExecutor::default()
        };
        let mut session = GuardianSession::ready(test_session(), &mut executor).unwrap();
        let mut sequence = 1;
        apply_down(&mut session, &mut sequence, OperationId(1), 30);
        let cleanup_deadline = NOW_NS + MAX_RELEASE_CLEANUP_NS;
        let mut clock_values = [NOW_NS, NOW_NS, cleanup_deadline].into_iter();

        let proof = session.finish_with_clock(StopReason::PeerEof, || {
            clock_values.next().unwrap_or(cleanup_deadline)
        });

        assert_eq!(session.executor_ref().release_attempts.len(), 1);
        assert_eq!(proof, TerminalProof::Unreconciled { remaining: 1 });
        assert_eq!(session.debt_count(), 1);
    }

    #[test]
    fn terminal_session_rejects_every_new_mutation() {
        let mut executor = FakeXtestExecutor {
            identity: test_identity(),
            ..FakeXtestExecutor::default()
        };
        let mut session = GuardianSession::ready(test_session(), &mut executor).unwrap();
        assert_eq!(
            session.finish_with_clock(StopReason::PeerEof, || NOW_NS),
            TerminalProof::Reconciled
        );

        assert!(session
            .handle_request(
                Sequence(1),
                Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: DEADLINE,
                },
                NOW_NS,
            )
            .is_err());
    }

    fn send_request(connection: &Seqpacket, sequence: u64, request: Request) {
        let frame = encode_frame(Sequence(sequence), &Message::Request(request)).unwrap();
        connection.send_frame(&frame).unwrap();
    }

    fn receive_response(connection: &Seqpacket) -> Response {
        let frame = connection.recv_frame().unwrap();
        let decoded = decode_frame(&frame).unwrap();
        let Message::Response(response) = decoded.message else {
            panic!("expected response");
        };
        response
    }

    #[test]
    fn request_loop_releases_acknowledged_down_after_peer_eof() {
        let (client, server) = Seqpacket::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut executor = FakeXtestExecutor::default();
            let record = run_connection(&server, test_session(), &mut executor).unwrap();
            (record, executor)
        });
        let now = monotonic_now_ns().unwrap();
        let deadline = MutationDeadlineNs(now + 1_000_000_000);

        send_request(
            &client,
            1,
            Request::Hello {
                daemon_nonce: [0x44; 16],
                deadline,
            },
        );
        assert!(matches!(receive_response(&client), Response::Ready { .. }));
        send_request(
            &client,
            2,
            Request::PrepareKey {
                operation: OperationId(1),
                evdev_code: 30,
                deadline,
            },
        );
        let Response::Prepared { token, .. } = receive_response(&client) else {
            panic!("expected prepared response");
        };
        send_request(
            &client,
            3,
            Request::ExecuteDown {
                operation: OperationId(1),
                token,
                deadline,
            },
        );
        assert!(matches!(
            receive_response(&client),
            Response::DownAck { .. }
        ));
        send_request(
            &client,
            4,
            Request::Synchronize {
                operation: OperationId(1),
                token_id: token.token_id,
                deadline: ReleaseDeadline::Mutation(deadline),
            },
        );
        assert!(matches!(
            receive_response(&client),
            Response::SyncAck { .. }
        ));
        drop(client);

        let (record, executor) = server_thread.join().unwrap();
        assert_eq!(record.reason, StopReason::PeerEof);
        assert_eq!(record.proof, TerminalProof::Reconciled);
        assert_eq!(executor.downs, [token.x11_keycode]);
        assert_eq!(executor.release_attempts, [token.x11_keycode]);
    }

    #[test]
    fn lost_down_ack_never_repeats_down_and_still_releases_on_eof() {
        let (client, server) = Seqpacket::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut executor = FakeXtestExecutor::default();
            let record = run_connection(&server, test_session(), &mut executor).unwrap();
            (record, executor)
        });
        let now = monotonic_now_ns().unwrap();
        let deadline = MutationDeadlineNs(now + 1_000_000_000);

        send_request(
            &client,
            1,
            Request::Hello {
                daemon_nonce: [0x44; 16],
                deadline,
            },
        );
        let _ = receive_response(&client);
        send_request(
            &client,
            2,
            Request::PrepareKey {
                operation: OperationId(1),
                evdev_code: 30,
                deadline,
            },
        );
        let Response::Prepared { token, .. } = receive_response(&client) else {
            panic!("expected prepared response");
        };
        send_request(
            &client,
            3,
            Request::ExecuteDown {
                operation: OperationId(1),
                token,
                deadline,
            },
        );
        drop(client);

        let (record, executor) = server_thread.join().unwrap();
        assert_eq!(record.proof, TerminalProof::Reconciled);
        assert_eq!(executor.downs, [token.x11_keycode]);
        assert_eq!(executor.release_attempts, [token.x11_keycode]);
    }

    #[test]
    fn invalid_hello_sends_bounded_fatal_before_closing() {
        let (client, server) = Seqpacket::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut executor = FakeXtestExecutor::default();
            run_connection(&server, test_session(), &mut executor)
        });
        let now = monotonic_now_ns().unwrap();

        send_request(
            &client,
            1,
            Request::Hello {
                daemon_nonce: [0; 16],
                deadline: MutationDeadlineNs(now + 1_000_000_000),
            },
        );

        assert_eq!(
            receive_response(&client),
            Response::Fatal {
                code: FatalCode::ProtocolViolation,
            }
        );
        assert!(server_thread.join().unwrap().is_err());
    }
}
