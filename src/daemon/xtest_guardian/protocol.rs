use crate::daemon::synthetic_input::{InputGeneration, OperationId, PhysicalSequence};
use crate::error::SwitcherError;

const MAGIC: [u8; 4] = *b"OSXG";
const HEADER_BYTES: usize = 17;
pub(crate) const PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_FRAME_BYTES: usize = 128;
pub(crate) const MAX_PREPARED_TOKENS: usize = 512;
pub(crate) const MAX_ACTIVE_DEBTS: usize = 32;
pub(crate) const MAX_TRANSACTION_TIMEOUT_NS: u64 = 5_000_000_000;
pub(crate) const MAX_RELEASE_CLEANUP_NS: u64 = 1_000_000_000;

const KIND_HELLO: u8 = 1;
const KIND_PREPARE_KEY: u8 = 2;
const KIND_EXECUTE_DOWN: u8 = 3;
const KIND_KEY_UP: u8 = 4;
const KIND_SYNCHRONIZE: u8 = 5;
const KIND_TRANSFER_TO_PHYSICAL_DEBT: u8 = 6;
const KIND_PHYSICAL_RELEASE_COMMITTED: u8 = 7;
const KIND_CANCEL_AND_DRAIN: u8 = 8;
const KIND_RELEASE_ALL_AND_EXIT: u8 = 9;
const KIND_READY: u8 = 101;
const KIND_PREPARED: u8 = 102;
const KIND_DOWN_ACK: u8 = 103;
const KIND_UP_ACK: u8 = 104;
const KIND_SYNC_ACK: u8 = 105;
const KIND_TRANSFER_ACK: u8 = 106;
const KIND_RELEASE_COMMIT_ACK: u8 = 107;
const KIND_DRAINED: u8 = 108;
const KIND_STOPPED: u8 = 109;
const KIND_FATAL: u8 = 110;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Sequence(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionId(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ServerEpoch(pub(crate) [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProtocolSession {
    pub(crate) session: SessionId,
    pub(crate) epoch: ServerEpoch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MutationDeadlineNs(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CleanupDeadlineNs(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReleaseDeadline {
    Mutation(MutationDeadlineNs),
    Cleanup(CleanupDeadlineNs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PreparedToken {
    pub(crate) session: SessionId,
    pub(crate) epoch: ServerEpoch,
    pub(crate) token_id: u64,
    pub(crate) evdev_code: u16,
    pub(crate) x11_keycode: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireTerminalProof {
    Reconciled,
    OwnerGenerationDestroyed { generation: u64 },
    Unreconciled { remaining: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FatalCode {
    ProtocolViolation,
    DeadlineExpired,
    DeadlineTooFar,
    CapacityExceeded,
    BackendUnavailable,
    BackendFailure,
    Unreconciled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Hello {
        daemon_nonce: [u8; 16],
        deadline: MutationDeadlineNs,
    },
    PrepareKey {
        operation: OperationId,
        evdev_code: u16,
        deadline: MutationDeadlineNs,
    },
    ExecuteDown {
        operation: OperationId,
        token: PreparedToken,
        deadline: MutationDeadlineNs,
    },
    KeyUp {
        operation: OperationId,
        token: PreparedToken,
        deadline: ReleaseDeadline,
    },
    Synchronize {
        operation: OperationId,
        token_id: u64,
        deadline: ReleaseDeadline,
    },
    TransferToPhysicalDebt {
        operation: OperationId,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: MutationDeadlineNs,
    },
    PhysicalReleaseCommitted {
        sequence: PhysicalSequence,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: MutationDeadlineNs,
    },
    CancelAndDrain {
        operation: OperationId,
        deadline: CleanupDeadlineNs,
    },
    ReleaseAllAndExit {
        deadline: CleanupDeadlineNs,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Response {
    Ready {
        session: SessionId,
        epoch: ServerEpoch,
        epoch_window: u32,
        epoch_nonce: [u8; 16],
    },
    Prepared {
        operation: OperationId,
        token: PreparedToken,
    },
    DownAck {
        operation: OperationId,
        token_id: u64,
    },
    UpAck {
        operation: OperationId,
        token_id: u64,
    },
    SyncAck {
        operation: OperationId,
        token_id: u64,
    },
    TransferAck {
        operation: OperationId,
        token_id: u64,
    },
    ReleaseCommitAck {
        sequence: PhysicalSequence,
        token_id: u64,
    },
    Drained {
        operation: OperationId,
        proof: WireTerminalProof,
    },
    Stopped {
        proof: WireTerminalProof,
    },
    Fatal {
        code: FatalCode,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Message {
    Request(Request),
    Response(Response),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DecodedFrame {
    pub(crate) sequence: Sequence,
    pub(crate) message: Message,
}

fn protocol_error(context: &'static str) -> SwitcherError {
    SwitcherError::input_safety(context)
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(MAX_FRAME_BYTES - HEADER_BYTES),
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), SwitcherError> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > MAX_FRAME_BYTES - HEADER_BYTES)
        {
            return Err(protocol_error("XTEST guardian frame payload is too large"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn push_u8(&mut self, value: u8) -> Result<(), SwitcherError> {
        self.push_bytes(&[value])
    }

    fn push_u16(&mut self, value: u16) -> Result<(), SwitcherError> {
        self.push_bytes(&value.to_be_bytes())
    }

    fn push_u32(&mut self, value: u32) -> Result<(), SwitcherError> {
        self.push_bytes(&value.to_be_bytes())
    }

    fn push_u64(&mut self, value: u64) -> Result<(), SwitcherError> {
        self.push_bytes(&value.to_be_bytes())
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SwitcherError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or_else(|| protocol_error("XTEST guardian frame offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| protocol_error("XTEST guardian frame is truncated"))?;
        self.offset = end;
        let mut result = [0; N];
        result.copy_from_slice(bytes);
        Ok(result)
    }

    fn read_u8(&mut self) -> Result<u8, SwitcherError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_u16(&mut self) -> Result<u16, SwitcherError> {
        Ok(u16::from_be_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SwitcherError> {
        Ok(u32::from_be_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SwitcherError> {
        Ok(u64::from_be_bytes(self.read_array()?))
    }

    fn finish(self) -> Result<(), SwitcherError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(protocol_error(
                "XTEST guardian frame contains trailing payload bytes",
            ))
        }
    }
}

fn encode_operation(encoder: &mut Encoder, operation: OperationId) -> Result<(), SwitcherError> {
    encoder.push_u64(operation.0)
}

fn decode_operation(decoder: &mut Decoder<'_>) -> Result<OperationId, SwitcherError> {
    Ok(OperationId(decoder.read_u64()?))
}

fn encode_token(encoder: &mut Encoder, token: PreparedToken) -> Result<(), SwitcherError> {
    encoder.push_bytes(&token.session.0)?;
    encoder.push_bytes(&token.epoch.0)?;
    encoder.push_u64(token.token_id)?;
    encoder.push_u16(token.evdev_code)?;
    encoder.push_u8(token.x11_keycode)
}

fn decode_token(decoder: &mut Decoder<'_>) -> Result<PreparedToken, SwitcherError> {
    Ok(PreparedToken {
        session: SessionId(decoder.read_array()?),
        epoch: ServerEpoch(decoder.read_array()?),
        token_id: decoder.read_u64()?,
        evdev_code: decoder.read_u16()?,
        x11_keycode: decoder.read_u8()?,
    })
}

fn encode_release_deadline(
    encoder: &mut Encoder,
    deadline: ReleaseDeadline,
) -> Result<(), SwitcherError> {
    match deadline {
        ReleaseDeadline::Mutation(deadline) => {
            encoder.push_u8(1)?;
            encoder.push_u64(deadline.0)
        }
        ReleaseDeadline::Cleanup(deadline) => {
            encoder.push_u8(2)?;
            encoder.push_u64(deadline.0)
        }
    }
}

fn decode_release_deadline(decoder: &mut Decoder<'_>) -> Result<ReleaseDeadline, SwitcherError> {
    match decoder.read_u8()? {
        1 => Ok(ReleaseDeadline::Mutation(MutationDeadlineNs(
            decoder.read_u64()?,
        ))),
        2 => Ok(ReleaseDeadline::Cleanup(CleanupDeadlineNs(
            decoder.read_u64()?,
        ))),
        _ => Err(protocol_error(
            "XTEST guardian release deadline kind is unknown",
        )),
    }
}

fn encode_terminal_proof(
    encoder: &mut Encoder,
    proof: WireTerminalProof,
) -> Result<(), SwitcherError> {
    match proof {
        WireTerminalProof::Reconciled => encoder.push_u8(1),
        WireTerminalProof::OwnerGenerationDestroyed { generation } => {
            encoder.push_u8(2)?;
            encoder.push_u64(generation)
        }
        WireTerminalProof::Unreconciled { remaining } => {
            encoder.push_u8(3)?;
            encoder.push_u16(remaining)
        }
    }
}

fn decode_terminal_proof(decoder: &mut Decoder<'_>) -> Result<WireTerminalProof, SwitcherError> {
    match decoder.read_u8()? {
        1 => Ok(WireTerminalProof::Reconciled),
        2 => Ok(WireTerminalProof::OwnerGenerationDestroyed {
            generation: decoder.read_u64()?,
        }),
        3 => Ok(WireTerminalProof::Unreconciled {
            remaining: decoder.read_u16()?,
        }),
        _ => Err(protocol_error(
            "XTEST guardian terminal proof kind is unknown",
        )),
    }
}

fn encode_fatal_code(encoder: &mut Encoder, code: FatalCode) -> Result<(), SwitcherError> {
    let value = match code {
        FatalCode::ProtocolViolation => 1,
        FatalCode::DeadlineExpired => 2,
        FatalCode::DeadlineTooFar => 3,
        FatalCode::CapacityExceeded => 4,
        FatalCode::BackendUnavailable => 5,
        FatalCode::BackendFailure => 6,
        FatalCode::Unreconciled => 7,
    };
    encoder.push_u8(value)
}

fn decode_fatal_code(decoder: &mut Decoder<'_>) -> Result<FatalCode, SwitcherError> {
    match decoder.read_u8()? {
        1 => Ok(FatalCode::ProtocolViolation),
        2 => Ok(FatalCode::DeadlineExpired),
        3 => Ok(FatalCode::DeadlineTooFar),
        4 => Ok(FatalCode::CapacityExceeded),
        5 => Ok(FatalCode::BackendUnavailable),
        6 => Ok(FatalCode::BackendFailure),
        7 => Ok(FatalCode::Unreconciled),
        _ => Err(protocol_error("XTEST guardian fatal code is unknown")),
    }
}

fn encode_message(message: &Message) -> Result<(u8, Vec<u8>), SwitcherError> {
    let mut encoder = Encoder::new();
    let kind = match *message {
        Message::Request(Request::Hello {
            daemon_nonce,
            deadline,
        }) => {
            encoder.push_bytes(&daemon_nonce)?;
            encoder.push_u64(deadline.0)?;
            KIND_HELLO
        }
        Message::Request(Request::PrepareKey {
            operation,
            evdev_code,
            deadline,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u16(evdev_code)?;
            encoder.push_u64(deadline.0)?;
            KIND_PREPARE_KEY
        }
        Message::Request(Request::ExecuteDown {
            operation,
            token,
            deadline,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encode_token(&mut encoder, token)?;
            encoder.push_u64(deadline.0)?;
            KIND_EXECUTE_DOWN
        }
        Message::Request(Request::KeyUp {
            operation,
            token,
            deadline,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encode_token(&mut encoder, token)?;
            encode_release_deadline(&mut encoder, deadline)?;
            KIND_KEY_UP
        }
        Message::Request(Request::Synchronize {
            operation,
            token_id,
            deadline,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u64(token_id)?;
            encode_release_deadline(&mut encoder, deadline)?;
            KIND_SYNCHRONIZE
        }
        Message::Request(Request::TransferToPhysicalDebt {
            operation,
            token,
            input_generation,
            deadline,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encode_token(&mut encoder, token)?;
            encoder.push_u64(input_generation.0)?;
            encoder.push_u64(deadline.0)?;
            KIND_TRANSFER_TO_PHYSICAL_DEBT
        }
        Message::Request(Request::PhysicalReleaseCommitted {
            sequence,
            token,
            input_generation,
            deadline,
        }) => {
            encoder.push_u64(sequence.0)?;
            encode_token(&mut encoder, token)?;
            encoder.push_u64(input_generation.0)?;
            encoder.push_u64(deadline.0)?;
            KIND_PHYSICAL_RELEASE_COMMITTED
        }
        Message::Request(Request::CancelAndDrain {
            operation,
            deadline,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u64(deadline.0)?;
            KIND_CANCEL_AND_DRAIN
        }
        Message::Request(Request::ReleaseAllAndExit { deadline }) => {
            encoder.push_u64(deadline.0)?;
            KIND_RELEASE_ALL_AND_EXIT
        }
        Message::Response(Response::Ready {
            session,
            epoch,
            epoch_window,
            epoch_nonce,
        }) => {
            encoder.push_bytes(&session.0)?;
            encoder.push_bytes(&epoch.0)?;
            encoder.push_u32(epoch_window)?;
            encoder.push_bytes(&epoch_nonce)?;
            KIND_READY
        }
        Message::Response(Response::Prepared { operation, token }) => {
            encode_operation(&mut encoder, operation)?;
            encode_token(&mut encoder, token)?;
            KIND_PREPARED
        }
        Message::Response(Response::DownAck {
            operation,
            token_id,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u64(token_id)?;
            KIND_DOWN_ACK
        }
        Message::Response(Response::UpAck {
            operation,
            token_id,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u64(token_id)?;
            KIND_UP_ACK
        }
        Message::Response(Response::SyncAck {
            operation,
            token_id,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u64(token_id)?;
            KIND_SYNC_ACK
        }
        Message::Response(Response::TransferAck {
            operation,
            token_id,
        }) => {
            encode_operation(&mut encoder, operation)?;
            encoder.push_u64(token_id)?;
            KIND_TRANSFER_ACK
        }
        Message::Response(Response::ReleaseCommitAck { sequence, token_id }) => {
            encoder.push_u64(sequence.0)?;
            encoder.push_u64(token_id)?;
            KIND_RELEASE_COMMIT_ACK
        }
        Message::Response(Response::Drained { operation, proof }) => {
            encode_operation(&mut encoder, operation)?;
            encode_terminal_proof(&mut encoder, proof)?;
            KIND_DRAINED
        }
        Message::Response(Response::Stopped { proof }) => {
            encode_terminal_proof(&mut encoder, proof)?;
            KIND_STOPPED
        }
        Message::Response(Response::Fatal { code }) => {
            encode_fatal_code(&mut encoder, code)?;
            KIND_FATAL
        }
    };
    Ok((kind, encoder.bytes))
}

pub(crate) fn encode_frame(
    sequence: Sequence,
    message: &Message,
) -> Result<Vec<u8>, SwitcherError> {
    if sequence.0 == 0 {
        return Err(protocol_error("XTEST guardian sequence must be nonzero"));
    }
    let (kind, payload) = encode_message(message)?;
    let payload_len = u16::try_from(payload.len())
        .map_err(|_| protocol_error("XTEST guardian payload length overflow"))?;
    let frame_len = HEADER_BYTES
        .checked_add(payload.len())
        .ok_or_else(|| protocol_error("XTEST guardian frame length overflow"))?;
    if frame_len > MAX_FRAME_BYTES {
        return Err(protocol_error("XTEST guardian frame is too large"));
    }

    let mut frame = Vec::with_capacity(frame_len);
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    frame.push(kind);
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&sequence.0.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn decode_message(kind: u8, decoder: &mut Decoder<'_>) -> Result<Message, SwitcherError> {
    let message = match kind {
        KIND_HELLO => Message::Request(Request::Hello {
            daemon_nonce: decoder.read_array()?,
            deadline: MutationDeadlineNs(decoder.read_u64()?),
        }),
        KIND_PREPARE_KEY => Message::Request(Request::PrepareKey {
            operation: decode_operation(decoder)?,
            evdev_code: decoder.read_u16()?,
            deadline: MutationDeadlineNs(decoder.read_u64()?),
        }),
        KIND_EXECUTE_DOWN => Message::Request(Request::ExecuteDown {
            operation: decode_operation(decoder)?,
            token: decode_token(decoder)?,
            deadline: MutationDeadlineNs(decoder.read_u64()?),
        }),
        KIND_KEY_UP => Message::Request(Request::KeyUp {
            operation: decode_operation(decoder)?,
            token: decode_token(decoder)?,
            deadline: decode_release_deadline(decoder)?,
        }),
        KIND_SYNCHRONIZE => Message::Request(Request::Synchronize {
            operation: decode_operation(decoder)?,
            token_id: decoder.read_u64()?,
            deadline: decode_release_deadline(decoder)?,
        }),
        KIND_TRANSFER_TO_PHYSICAL_DEBT => Message::Request(Request::TransferToPhysicalDebt {
            operation: decode_operation(decoder)?,
            token: decode_token(decoder)?,
            input_generation: InputGeneration(decoder.read_u64()?),
            deadline: MutationDeadlineNs(decoder.read_u64()?),
        }),
        KIND_PHYSICAL_RELEASE_COMMITTED => Message::Request(Request::PhysicalReleaseCommitted {
            sequence: PhysicalSequence(decoder.read_u64()?),
            token: decode_token(decoder)?,
            input_generation: InputGeneration(decoder.read_u64()?),
            deadline: MutationDeadlineNs(decoder.read_u64()?),
        }),
        KIND_CANCEL_AND_DRAIN => Message::Request(Request::CancelAndDrain {
            operation: decode_operation(decoder)?,
            deadline: CleanupDeadlineNs(decoder.read_u64()?),
        }),
        KIND_RELEASE_ALL_AND_EXIT => Message::Request(Request::ReleaseAllAndExit {
            deadline: CleanupDeadlineNs(decoder.read_u64()?),
        }),
        KIND_READY => Message::Response(Response::Ready {
            session: SessionId(decoder.read_array()?),
            epoch: ServerEpoch(decoder.read_array()?),
            epoch_window: decoder.read_u32()?,
            epoch_nonce: decoder.read_array()?,
        }),
        KIND_PREPARED => Message::Response(Response::Prepared {
            operation: decode_operation(decoder)?,
            token: decode_token(decoder)?,
        }),
        KIND_DOWN_ACK => Message::Response(Response::DownAck {
            operation: decode_operation(decoder)?,
            token_id: decoder.read_u64()?,
        }),
        KIND_UP_ACK => Message::Response(Response::UpAck {
            operation: decode_operation(decoder)?,
            token_id: decoder.read_u64()?,
        }),
        KIND_SYNC_ACK => Message::Response(Response::SyncAck {
            operation: decode_operation(decoder)?,
            token_id: decoder.read_u64()?,
        }),
        KIND_TRANSFER_ACK => Message::Response(Response::TransferAck {
            operation: decode_operation(decoder)?,
            token_id: decoder.read_u64()?,
        }),
        KIND_RELEASE_COMMIT_ACK => Message::Response(Response::ReleaseCommitAck {
            sequence: PhysicalSequence(decoder.read_u64()?),
            token_id: decoder.read_u64()?,
        }),
        KIND_DRAINED => Message::Response(Response::Drained {
            operation: decode_operation(decoder)?,
            proof: decode_terminal_proof(decoder)?,
        }),
        KIND_STOPPED => Message::Response(Response::Stopped {
            proof: decode_terminal_proof(decoder)?,
        }),
        KIND_FATAL => Message::Response(Response::Fatal {
            code: decode_fatal_code(decoder)?,
        }),
        _ => return Err(protocol_error("XTEST guardian message kind is unknown")),
    };
    Ok(message)
}

pub(crate) fn decode_frame(frame: &[u8]) -> Result<DecodedFrame, SwitcherError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(protocol_error("XTEST guardian frame is too large"));
    }
    if frame.len() < HEADER_BYTES {
        return Err(protocol_error("XTEST guardian frame header is truncated"));
    }

    let mut header = Decoder::new(&frame[..HEADER_BYTES]);
    if header.read_array::<4>()? != MAGIC {
        return Err(protocol_error("XTEST guardian frame magic is invalid"));
    }
    if header.read_u16()? != PROTOCOL_VERSION {
        return Err(protocol_error(
            "XTEST guardian protocol version is unsupported",
        ));
    }
    let kind = header.read_u8()?;
    let payload_len = usize::from(header.read_u16()?);
    let sequence = Sequence(header.read_u64()?);
    header.finish()?;
    if sequence.0 == 0 {
        return Err(protocol_error("XTEST guardian sequence must be nonzero"));
    }
    let expected_len = HEADER_BYTES
        .checked_add(payload_len)
        .ok_or_else(|| protocol_error("XTEST guardian frame length overflow"))?;
    if expected_len != frame.len() {
        return Err(protocol_error(
            "XTEST guardian payload length does not match frame",
        ));
    }

    let mut payload = Decoder::new(&frame[HEADER_BYTES..]);
    let message = decode_message(kind, &mut payload)?;
    payload.finish()?;
    Ok(DecodedFrame { sequence, message })
}

pub(crate) fn response_matches(
    request_sequence: Sequence,
    request: &Request,
    response: &DecodedFrame,
) -> Result<(), SwitcherError> {
    if response.sequence != request_sequence {
        return Err(protocol_error(
            "XTEST guardian response sequence does not match request",
        ));
    }
    let Message::Response(response) = response.message else {
        return Err(protocol_error(
            "XTEST guardian peer returned a request as a response",
        ));
    };
    if matches!(response, Response::Fatal { .. }) {
        return Ok(());
    }

    let matches = match (*request, response) {
        (Request::Hello { .. }, Response::Ready { .. }) => true,
        (
            Request::PrepareKey {
                operation,
                evdev_code,
                ..
            },
            Response::Prepared {
                operation: response_operation,
                token,
            },
        ) => operation == response_operation && evdev_code == token.evdev_code,
        (
            Request::ExecuteDown {
                operation, token, ..
            },
            Response::DownAck {
                operation: response_operation,
                token_id,
            },
        ) => operation == response_operation && token.token_id == token_id,
        (
            Request::KeyUp {
                operation, token, ..
            },
            Response::UpAck {
                operation: response_operation,
                token_id,
            },
        ) => operation == response_operation && token.token_id == token_id,
        (
            Request::Synchronize {
                operation,
                token_id,
                ..
            },
            Response::SyncAck {
                operation: response_operation,
                token_id: response_token_id,
            },
        ) => operation == response_operation && token_id == response_token_id,
        (
            Request::TransferToPhysicalDebt {
                operation, token, ..
            },
            Response::TransferAck {
                operation: response_operation,
                token_id,
            },
        ) => operation == response_operation && token.token_id == token_id,
        (
            Request::PhysicalReleaseCommitted {
                sequence, token, ..
            },
            Response::ReleaseCommitAck {
                sequence: response_sequence,
                token_id,
            },
        ) => sequence == response_sequence && token.token_id == token_id,
        (
            Request::CancelAndDrain { operation, .. },
            Response::Drained {
                operation: response_operation,
                ..
            },
        ) => operation == response_operation,
        (Request::ReleaseAllAndExit { .. }, Response::Stopped { .. }) => true,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(protocol_error(
            "XTEST guardian response does not match request",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackedTokenState {
    Prepared,
    PossiblyDown,
    PhysicalDebt(InputGeneration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackedToken {
    operation: OperationId,
    token: PreparedToken,
    state: TrackedTokenState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingPrepare {
    operation: OperationId,
    evdev_code: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingSyncKind {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingSync {
    operation: OperationId,
    token_id: u64,
    kind: PendingSyncKind,
    request_accepted: bool,
}

#[derive(Debug)]
pub(crate) struct ProtocolState {
    session: ProtocolSession,
    last_sequence: u64,
    last_operation: u64,
    active_operation: Option<OperationId>,
    operation_deadline: Option<MutationDeadlineNs>,
    last_prepared_token_id: u64,
    last_physical_release_sequence: u64,
    pending_prepare: Option<PendingPrepare>,
    pending_sync: Option<PendingSync>,
    tokens: Vec<TrackedToken>,
    terminal: bool,
    cleanup_deadline: Option<CleanupDeadlineNs>,
}

impl ProtocolState {
    pub(crate) fn ready(session: ProtocolSession) -> Result<Self, SwitcherError> {
        if session.session.0.iter().all(|byte| *byte == 0) {
            return Err(protocol_error(
                "XTEST guardian session identifier must be nonzero",
            ));
        }
        if session.epoch.0.iter().all(|byte| *byte == 0) {
            return Err(protocol_error(
                "XTEST guardian server epoch must be nonzero",
            ));
        }
        Ok(Self {
            session,
            last_sequence: 0,
            last_operation: 0,
            active_operation: None,
            operation_deadline: None,
            last_prepared_token_id: 0,
            last_physical_release_sequence: 0,
            pending_prepare: None,
            pending_sync: None,
            tokens: Vec::with_capacity(MAX_ACTIVE_DEBTS),
            terminal: false,
            cleanup_deadline: None,
        })
    }

    pub(crate) fn accept(
        &mut self,
        sequence: Sequence,
        request: &Request,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        if sequence.0 == 0 || sequence.0 <= self.last_sequence {
            return Err(protocol_error(
                "XTEST guardian request sequence is stale or zero",
            ));
        }

        match *request {
            Request::Hello { .. } => {
                return Err(protocol_error(
                    "XTEST guardian ready session cannot accept another hello",
                ));
            }
            Request::PrepareKey {
                operation,
                evdev_code,
                deadline,
            } => self.accept_prepare(operation, evdev_code, deadline, now_ns)?,
            Request::ExecuteDown {
                operation,
                token,
                deadline,
            } => self.accept_execute_down(operation, token, deadline, now_ns)?,
            Request::KeyUp {
                operation,
                token,
                deadline,
            } => self.accept_key_up(operation, token, deadline, now_ns)?,
            Request::Synchronize {
                operation,
                token_id,
                deadline,
            } => self.accept_synchronize(operation, token_id, deadline, now_ns)?,
            Request::TransferToPhysicalDebt {
                operation,
                token,
                input_generation,
                deadline,
            } => self.accept_transfer(operation, token, input_generation, deadline, now_ns)?,
            Request::PhysicalReleaseCommitted {
                sequence,
                token,
                input_generation,
                deadline,
            } => {
                self.accept_physical_release(sequence, token, input_generation, deadline, now_ns)?
            }
            Request::CancelAndDrain {
                operation,
                deadline,
            } => self.accept_cancel_and_drain(operation, deadline, now_ns)?,
            Request::ReleaseAllAndExit { deadline } => {
                let starts_terminal = self.validate_cleanup_deadline(deadline, now_ns)?;
                self.apply_cleanup_deadline(deadline, starts_terminal);
                self.pending_prepare = None;
                self.pending_sync = None;
            }
        }

        self.last_sequence = sequence.0;
        Ok(())
    }

    pub(crate) fn record_prepared(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
    ) -> Result<(), SwitcherError> {
        let Some(pending) = self.pending_prepare else {
            return Err(protocol_error(
                "XTEST guardian prepared token has no matching request",
            ));
        };
        if pending.operation != operation || pending.evdev_code != token.evdev_code {
            return Err(protocol_error(
                "XTEST guardian prepared token does not match request",
            ));
        }
        self.validate_token_identity(token)?;
        if token.token_id <= self.last_prepared_token_id {
            return Err(protocol_error(
                "XTEST guardian prepared token identifier is stale",
            ));
        }
        if self.tokens.len() >= MAX_PREPARED_TOKENS {
            return Err(protocol_error(
                "XTEST guardian prepared token capacity exceeded",
            ));
        }
        if self
            .tokens
            .iter()
            .any(|tracked| tracked.token.token_id == token.token_id)
        {
            return Err(protocol_error(
                "XTEST guardian prepared token identifier is duplicated",
            ));
        }

        self.tokens.push(TrackedToken {
            operation,
            token,
            state: TrackedTokenState::Prepared,
        });
        self.last_prepared_token_id = token.token_id;
        self.pending_prepare = None;
        Ok(())
    }

    pub(crate) fn complete_synchronize(&mut self, token_id: u64) -> Result<(), SwitcherError> {
        let Some(pending) = self.pending_sync else {
            return Err(protocol_error(
                "XTEST guardian synchronize completion has no request",
            ));
        };
        if pending.token_id != token_id || !pending.request_accepted {
            return Err(protocol_error(
                "XTEST guardian synchronize completion does not match request",
            ));
        }

        if pending.kind == PendingSyncKind::Up {
            let index = self
                .tokens
                .iter()
                .position(|tracked| tracked.token.token_id == token_id)
                .ok_or_else(|| {
                    protocol_error("XTEST guardian release completion lost its token")
                })?;
            self.tokens.remove(index);
        }
        self.pending_sync = None;
        Ok(())
    }

    pub(crate) fn debt_count(&self) -> usize {
        self.tokens
            .iter()
            .filter(|tracked| tracked.state != TrackedTokenState::Prepared)
            .count()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn accept_prepare(
        &mut self,
        operation: OperationId,
        evdev_code: u16,
        deadline: MutationDeadlineNs,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_normal_deadline(deadline, now_ns)?;
        if evdev_code == 0 {
            return Err(protocol_error(
                "XTEST guardian evdev key code must be nonzero",
            ));
        }
        if self.pending_prepare.is_some() || self.pending_sync.is_some() {
            return Err(protocol_error(
                "XTEST guardian accepts only one request in flight",
            ));
        }
        let starts_operation = self.validate_prepare_operation(operation)?;
        if !starts_operation {
            self.validate_operation_deadline(operation, deadline)?;
        }

        if starts_operation {
            self.last_operation = operation.0;
            self.active_operation = Some(operation);
            self.operation_deadline = Some(deadline);
        }
        self.pending_prepare = Some(PendingPrepare {
            operation,
            evdev_code,
        });
        Ok(())
    }

    fn accept_execute_down(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
        deadline: MutationDeadlineNs,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_normal_deadline(deadline, now_ns)?;
        self.validate_active_operation(operation)?;
        self.validate_operation_deadline(operation, deadline)?;
        self.validate_token_identity(token)?;
        if self.pending_prepare.is_some() || self.pending_sync.is_some() {
            return Err(protocol_error(
                "XTEST guardian accepts only one request in flight",
            ));
        }
        let index = self.find_token(token)?;
        if self.tokens[index].operation != operation
            || self.tokens[index].state != TrackedTokenState::Prepared
        {
            return Err(protocol_error(
                "XTEST guardian down token is not prepared for this operation",
            ));
        }
        if self.debt_count() >= MAX_ACTIVE_DEBTS {
            return Err(protocol_error(
                "XTEST guardian active debt capacity exceeded",
            ));
        }

        self.tokens[index].state = TrackedTokenState::PossiblyDown;
        self.pending_sync = Some(PendingSync {
            operation,
            token_id: token.token_id,
            kind: PendingSyncKind::Down,
            request_accepted: false,
        });
        Ok(())
    }

    fn accept_key_up(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
        deadline: ReleaseDeadline,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_active_operation(operation)?;
        self.validate_token_identity(token)?;
        if self.pending_prepare.is_some() || self.pending_sync.is_some() {
            return Err(protocol_error(
                "XTEST guardian accepts only one request in flight",
            ));
        }
        let index = self.find_token(token)?;
        match self.tokens[index].state {
            TrackedTokenState::Prepared => {
                return Err(protocol_error(
                    "XTEST guardian cannot release a token that was never down",
                ));
            }
            TrackedTokenState::PossiblyDown if self.tokens[index].operation != operation => {
                return Err(protocol_error(
                    "XTEST guardian release operation does not own token",
                ));
            }
            TrackedTokenState::PossiblyDown | TrackedTokenState::PhysicalDebt(_) => {}
        }
        let starts_terminal = self.validate_release_deadline(operation, deadline, now_ns)?;

        if let ReleaseDeadline::Cleanup(deadline) = deadline {
            self.apply_cleanup_deadline(deadline, starts_terminal);
        }
        self.pending_sync = Some(PendingSync {
            operation,
            token_id: token.token_id,
            kind: PendingSyncKind::Up,
            request_accepted: false,
        });
        Ok(())
    }

    fn accept_synchronize(
        &mut self,
        operation: OperationId,
        token_id: u64,
        deadline: ReleaseDeadline,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_active_operation(operation)?;
        if token_id == 0 {
            return Err(protocol_error(
                "XTEST guardian synchronize token must be nonzero",
            ));
        }
        let Some(pending) = self.pending_sync else {
            return Err(protocol_error(
                "XTEST guardian synchronize has no preceding mutation",
            ));
        };
        if pending.operation != operation
            || pending.token_id != token_id
            || pending.request_accepted
        {
            return Err(protocol_error(
                "XTEST guardian synchronize does not match mutation",
            ));
        }
        let starts_terminal = self.validate_release_deadline(operation, deadline, now_ns)?;

        if let ReleaseDeadline::Cleanup(deadline) = deadline {
            self.apply_cleanup_deadline(deadline, starts_terminal);
        }
        self.pending_sync = Some(PendingSync {
            request_accepted: true,
            ..pending
        });
        Ok(())
    }

    fn accept_transfer(
        &mut self,
        operation: OperationId,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: MutationDeadlineNs,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_normal_deadline(deadline, now_ns)?;
        self.validate_active_operation(operation)?;
        self.validate_operation_deadline(operation, deadline)?;
        self.validate_token_identity(token)?;
        if input_generation.0 == 0 {
            return Err(protocol_error(
                "XTEST guardian input generation must be nonzero",
            ));
        }
        if self.pending_prepare.is_some() || self.pending_sync.is_some() {
            return Err(protocol_error(
                "XTEST guardian cannot transfer an in-flight mutation",
            ));
        }
        let index = self.find_token(token)?;
        if self.tokens[index].operation != operation
            || self.tokens[index].state != TrackedTokenState::PossiblyDown
        {
            return Err(protocol_error(
                "XTEST guardian transfer token is not owned by operation",
            ));
        }

        self.tokens[index].state = TrackedTokenState::PhysicalDebt(input_generation);
        Ok(())
    }

    fn accept_physical_release(
        &mut self,
        sequence: PhysicalSequence,
        token: PreparedToken,
        input_generation: InputGeneration,
        deadline: MutationDeadlineNs,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_normal_deadline(deadline, now_ns)?;
        self.validate_token_identity(token)?;
        if input_generation.0 == 0 {
            return Err(protocol_error(
                "XTEST guardian input generation must be nonzero",
            ));
        }
        if sequence.0 == 0 || sequence.0 <= self.last_physical_release_sequence {
            return Err(protocol_error(
                "XTEST guardian physical release sequence is stale or zero",
            ));
        }
        if self.pending_sync.is_some() {
            return Err(protocol_error(
                "XTEST guardian cannot commit release during synchronization",
            ));
        }
        let index = self.find_token(token)?;
        if self.tokens[index].state != TrackedTokenState::PhysicalDebt(input_generation) {
            return Err(protocol_error(
                "XTEST guardian physical release generation does not match debt",
            ));
        }

        self.tokens.remove(index);
        self.last_physical_release_sequence = sequence.0;
        Ok(())
    }

    fn accept_cancel_and_drain(
        &mut self,
        operation: OperationId,
        deadline: CleanupDeadlineNs,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        self.validate_active_operation(operation)?;
        let starts_terminal = self.validate_cleanup_deadline(deadline, now_ns)?;
        self.apply_cleanup_deadline(deadline, starts_terminal);
        self.pending_prepare = None;
        self.pending_sync = None;
        Ok(())
    }

    fn validate_prepare_operation(&self, operation: OperationId) -> Result<bool, SwitcherError> {
        if operation.0 == 0 {
            return Err(protocol_error(
                "XTEST guardian operation identifier must be nonzero",
            ));
        }
        if operation.0 < self.last_operation {
            return Err(protocol_error(
                "XTEST guardian operation identifier is stale",
            ));
        }
        if operation.0 == self.last_operation {
            if self.active_operation == Some(operation) {
                return Ok(false);
            }
            return Err(protocol_error(
                "XTEST guardian completed operation cannot be reused",
            ));
        }
        if self
            .tokens
            .iter()
            .any(|tracked| !matches!(tracked.state, TrackedTokenState::PhysicalDebt(_)))
        {
            return Err(protocol_error(
                "XTEST guardian previous operation has transient tokens",
            ));
        }
        Ok(true)
    }

    fn validate_active_operation(&self, operation: OperationId) -> Result<(), SwitcherError> {
        if operation.0 == 0 || self.active_operation != Some(operation) {
            return Err(protocol_error(
                "XTEST guardian request operation is stale or inactive",
            ));
        }
        Ok(())
    }

    fn validate_operation_deadline(
        &self,
        operation: OperationId,
        deadline: MutationDeadlineNs,
    ) -> Result<(), SwitcherError> {
        if self.active_operation != Some(operation) || self.operation_deadline != Some(deadline) {
            return Err(protocol_error(
                "XTEST guardian operation mutation deadline cannot be changed",
            ));
        }
        Ok(())
    }

    fn validate_token_identity(&self, token: PreparedToken) -> Result<(), SwitcherError> {
        if token.session != self.session.session || token.epoch != self.session.epoch {
            return Err(protocol_error(
                "XTEST guardian token belongs to another session or epoch",
            ));
        }
        if token.token_id == 0 || token.evdev_code == 0 || token.x11_keycode == 0 {
            return Err(protocol_error(
                "XTEST guardian token contains a zero identifier",
            ));
        }
        Ok(())
    }

    fn find_token(&self, token: PreparedToken) -> Result<usize, SwitcherError> {
        self.tokens
            .iter()
            .position(|tracked| tracked.token == token)
            .ok_or_else(|| protocol_error("XTEST guardian token is unknown or stale"))
    }

    fn validate_normal_deadline(
        &self,
        deadline: MutationDeadlineNs,
        now_ns: u64,
    ) -> Result<(), SwitcherError> {
        if self.terminal {
            return Err(protocol_error(
                "XTEST guardian normal mutation attempted after terminal transition",
            ));
        }
        validate_future_deadline(
            deadline.0,
            now_ns,
            MAX_TRANSACTION_TIMEOUT_NS,
            "XTEST guardian mutation deadline expired",
            "XTEST guardian mutation deadline is too far in the future",
        )
    }

    fn validate_release_deadline(
        &self,
        operation: OperationId,
        deadline: ReleaseDeadline,
        now_ns: u64,
    ) -> Result<bool, SwitcherError> {
        match deadline {
            ReleaseDeadline::Mutation(deadline) => {
                self.validate_normal_deadline(deadline, now_ns)?;
                self.validate_operation_deadline(operation, deadline)?;
                Ok(false)
            }
            ReleaseDeadline::Cleanup(deadline) => self.validate_cleanup_deadline(deadline, now_ns),
        }
    }

    fn validate_cleanup_deadline(
        &self,
        deadline: CleanupDeadlineNs,
        now_ns: u64,
    ) -> Result<bool, SwitcherError> {
        validate_future_deadline(
            deadline.0,
            now_ns,
            MAX_RELEASE_CLEANUP_NS,
            "XTEST guardian cleanup deadline expired",
            "XTEST guardian cleanup deadline is too far in the future",
        )?;
        match self.cleanup_deadline {
            Some(existing) if existing != deadline => Err(protocol_error(
                "XTEST guardian cleanup deadline cannot be changed",
            )),
            Some(_) => Ok(false),
            None => Ok(true),
        }
    }

    fn apply_cleanup_deadline(&mut self, deadline: CleanupDeadlineNs, starts_terminal: bool) {
        if starts_terminal {
            self.cleanup_deadline = Some(deadline);
        }
        self.terminal = true;
    }
}

fn validate_future_deadline(
    deadline_ns: u64,
    now_ns: u64,
    maximum_delta_ns: u64,
    expired_context: &'static str,
    too_far_context: &'static str,
) -> Result<(), SwitcherError> {
    let Some(delta) = deadline_ns.checked_sub(now_ns) else {
        return Err(protocol_error(expired_context));
    };
    if delta == 0 {
        return Err(protocol_error(expired_context));
    }
    if delta > maximum_delta_ns {
        return Err(protocol_error(too_far_context));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::synthetic_input::{InputGeneration, OperationId, PhysicalSequence};

    const NOW_NS: u64 = 10_000_000_000;

    fn test_session() -> ProtocolSession {
        ProtocolSession {
            session: SessionId([0x11; 16]),
            epoch: ServerEpoch([0x22; 16]),
        }
    }

    fn mutation_deadline() -> MutationDeadlineNs {
        MutationDeadlineNs(NOW_NS + 1_000_000)
    }

    fn cleanup_deadline() -> CleanupDeadlineNs {
        CleanupDeadlineNs(NOW_NS + 2_000_000)
    }

    fn token(token_id: u64, evdev_code: u16) -> PreparedToken {
        PreparedToken {
            session: test_session().session,
            epoch: test_session().epoch,
            token_id,
            evdev_code,
            x11_keycode: 38,
        }
    }

    fn all_v1_test_messages() -> Vec<Message> {
        let prepared = token(17, 30);
        vec![
            Message::Request(Request::Hello {
                daemon_nonce: [0x33; 16],
                deadline: mutation_deadline(),
            }),
            Message::Request(Request::PrepareKey {
                operation: OperationId(9),
                evdev_code: 30,
                deadline: mutation_deadline(),
            }),
            Message::Request(Request::ExecuteDown {
                operation: OperationId(9),
                token: prepared,
                deadline: mutation_deadline(),
            }),
            Message::Request(Request::KeyUp {
                operation: OperationId(9),
                token: prepared,
                deadline: ReleaseDeadline::Mutation(mutation_deadline()),
            }),
            Message::Request(Request::KeyUp {
                operation: OperationId(9),
                token: prepared,
                deadline: ReleaseDeadline::Cleanup(cleanup_deadline()),
            }),
            Message::Request(Request::Synchronize {
                operation: OperationId(9),
                token_id: prepared.token_id,
                deadline: ReleaseDeadline::Mutation(mutation_deadline()),
            }),
            Message::Request(Request::Synchronize {
                operation: OperationId(9),
                token_id: prepared.token_id,
                deadline: ReleaseDeadline::Cleanup(cleanup_deadline()),
            }),
            Message::Request(Request::TransferToPhysicalDebt {
                operation: OperationId(9),
                token: prepared,
                input_generation: InputGeneration(5),
                deadline: mutation_deadline(),
            }),
            Message::Request(Request::PhysicalReleaseCommitted {
                sequence: PhysicalSequence(12),
                token: prepared,
                input_generation: InputGeneration(5),
                deadline: mutation_deadline(),
            }),
            Message::Request(Request::CancelAndDrain {
                operation: OperationId(9),
                deadline: cleanup_deadline(),
            }),
            Message::Request(Request::ReleaseAllAndExit {
                deadline: cleanup_deadline(),
            }),
            Message::Response(Response::Ready {
                session: test_session().session,
                epoch: test_session().epoch,
                epoch_window: 44,
                epoch_nonce: [0x55; 16],
            }),
            Message::Response(Response::Prepared {
                operation: OperationId(9),
                token: prepared,
            }),
            Message::Response(Response::DownAck {
                operation: OperationId(9),
                token_id: prepared.token_id,
            }),
            Message::Response(Response::UpAck {
                operation: OperationId(9),
                token_id: prepared.token_id,
            }),
            Message::Response(Response::SyncAck {
                operation: OperationId(9),
                token_id: prepared.token_id,
            }),
            Message::Response(Response::TransferAck {
                operation: OperationId(9),
                token_id: prepared.token_id,
            }),
            Message::Response(Response::ReleaseCommitAck {
                sequence: PhysicalSequence(12),
                token_id: prepared.token_id,
            }),
            Message::Response(Response::Drained {
                operation: OperationId(9),
                proof: WireTerminalProof::Unreconciled { remaining: 2 },
            }),
            Message::Response(Response::Stopped {
                proof: WireTerminalProof::Reconciled,
            }),
            Message::Response(Response::Stopped {
                proof: WireTerminalProof::OwnerGenerationDestroyed { generation: 5 },
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::ProtocolViolation,
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::DeadlineExpired,
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::DeadlineTooFar,
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::CapacityExceeded,
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::BackendUnavailable,
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::BackendFailure,
            }),
            Message::Response(Response::Fatal {
                code: FatalCode::Unreconciled,
            }),
        ]
    }

    #[test]
    fn codec_round_trip_preserves_every_v1_message() {
        for message in all_v1_test_messages() {
            let frame = encode_frame(Sequence(7), &message).unwrap();
            assert!(frame.len() <= MAX_FRAME_BYTES);
            let decoded = decode_frame(&frame).unwrap();
            assert_eq!(decoded.sequence, Sequence(7));
            assert_eq!(decoded.message, message);
        }
    }

    #[test]
    fn parser_rejects_oversize_unknown_version_kind_and_trailing_bytes() {
        assert!(decode_frame(&vec![0; MAX_FRAME_BYTES + 1]).is_err());

        let mut unknown_version = encode_frame(Sequence(1), &all_v1_test_messages()[0]).unwrap();
        unknown_version[4..6].copy_from_slice(&(PROTOCOL_VERSION + 1).to_be_bytes());
        assert!(decode_frame(&unknown_version).is_err());

        let mut unknown_kind = encode_frame(Sequence(1), &all_v1_test_messages()[0]).unwrap();
        unknown_kind[6] = u8::MAX;
        assert!(decode_frame(&unknown_kind).is_err());

        let mut trailing = encode_frame(Sequence(1), &all_v1_test_messages()[0]).unwrap();
        trailing.push(0);
        let payload_len = u16::from_be_bytes([trailing[7], trailing[8]]) + 1;
        trailing[7..9].copy_from_slice(&payload_len.to_be_bytes());
        assert!(decode_frame(&trailing).is_err());

        let mut unknown_release_deadline = encode_frame(
            Sequence(1),
            &Message::Request(Request::KeyUp {
                operation: OperationId(1),
                token: token(1, 30),
                deadline: ReleaseDeadline::Mutation(mutation_deadline()),
            }),
        )
        .unwrap();
        unknown_release_deadline[HEADER_BYTES + 8 + 43] = u8::MAX;
        assert!(decode_frame(&unknown_release_deadline).is_err());
    }

    #[test]
    fn response_matching_rejects_wrong_sequence_and_identifier() {
        let request = Request::PrepareKey {
            operation: OperationId(9),
            evdev_code: 30,
            deadline: mutation_deadline(),
        };
        let response = DecodedFrame {
            sequence: Sequence(7),
            message: Message::Response(Response::Prepared {
                operation: OperationId(9),
                token: token(17, 30),
            }),
        };
        assert!(response_matches(Sequence(7), &request, &response).is_ok());
        assert!(response_matches(Sequence(8), &request, &response).is_err());

        let wrong_operation = DecodedFrame {
            sequence: Sequence(7),
            message: Message::Response(Response::Prepared {
                operation: OperationId(8),
                token: token(17, 30),
            }),
        };
        assert!(response_matches(Sequence(7), &request, &wrong_operation).is_err());
    }

    #[test]
    fn protocol_state_rejects_stale_sequence_operation_and_epoch() {
        let mut state = ProtocolState::ready(test_session()).unwrap();
        let prepare = Request::PrepareKey {
            operation: OperationId(9),
            evdev_code: 30,
            deadline: mutation_deadline(),
        };
        state.accept(Sequence(4), &prepare, NOW_NS).unwrap();
        state
            .record_prepared(OperationId(9), token(17, 30))
            .unwrap();

        let execute = Request::ExecuteDown {
            operation: OperationId(9),
            token: token(17, 30),
            deadline: mutation_deadline(),
        };
        assert!(state.accept(Sequence(4), &execute, NOW_NS).is_err());

        let stale_operation = Request::PrepareKey {
            operation: OperationId(8),
            evdev_code: 31,
            deadline: mutation_deadline(),
        };
        assert!(state.accept(Sequence(5), &stale_operation, NOW_NS).is_err());

        let mut stale_epoch = token(17, 30);
        stale_epoch.epoch = ServerEpoch([0x77; 16]);
        let stale_token = Request::ExecuteDown {
            operation: OperationId(9),
            token: stale_epoch,
            deadline: mutation_deadline(),
        };
        assert!(state.accept(Sequence(5), &stale_token, NOW_NS).is_err());
    }

    #[test]
    fn operation_mutation_deadline_cannot_be_extended() {
        let mut state = ProtocolState::ready(test_session()).unwrap();
        let original_deadline = mutation_deadline();
        state
            .accept(
                Sequence(1),
                &Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: original_deadline,
                },
                NOW_NS,
            )
            .unwrap();
        state.record_prepared(OperationId(1), token(1, 30)).unwrap();

        assert!(state
            .accept(
                Sequence(2),
                &Request::ExecuteDown {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: MutationDeadlineNs(original_deadline.0 + 1),
                },
                NOW_NS,
            )
            .is_err());
        state
            .accept(
                Sequence(2),
                &Request::ExecuteDown {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: original_deadline,
                },
                NOW_NS,
            )
            .unwrap();
    }

    #[test]
    fn mutation_deadlines_are_bounded_and_cleanup_closes_normal_gate() {
        let mut state = ProtocolState::ready(test_session()).unwrap();
        let expired = Request::PrepareKey {
            operation: OperationId(1),
            evdev_code: 30,
            deadline: MutationDeadlineNs(NOW_NS),
        };
        assert!(state.accept(Sequence(1), &expired, NOW_NS).is_err());

        let too_far = Request::PrepareKey {
            operation: OperationId(1),
            evdev_code: 30,
            deadline: MutationDeadlineNs(NOW_NS + MAX_TRANSACTION_TIMEOUT_NS + 1),
        };
        assert!(state.accept(Sequence(1), &too_far, NOW_NS).is_err());

        let prepare = Request::PrepareKey {
            operation: OperationId(1),
            evdev_code: 30,
            deadline: mutation_deadline(),
        };
        state.accept(Sequence(1), &prepare, NOW_NS).unwrap();
        state.record_prepared(OperationId(1), token(1, 30)).unwrap();
        state
            .accept(
                Sequence(2),
                &Request::ExecuteDown {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        state
            .accept(
                Sequence(3),
                &Request::Synchronize {
                    operation: OperationId(1),
                    token_id: 1,
                    deadline: ReleaseDeadline::Mutation(mutation_deadline()),
                },
                NOW_NS,
            )
            .unwrap();
        state.complete_synchronize(1).unwrap();

        let after_transaction_expiry = NOW_NS + 1_000_001;
        let cleanup = CleanupDeadlineNs(after_transaction_expiry + 500_000);
        state
            .accept(
                Sequence(4),
                &Request::KeyUp {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: ReleaseDeadline::Cleanup(cleanup),
                },
                after_transaction_expiry,
            )
            .unwrap();
        assert!(state.is_terminal());

        let normal_after_terminal = Request::PrepareKey {
            operation: OperationId(2),
            evdev_code: 31,
            deadline: MutationDeadlineNs(after_transaction_expiry + 100_000),
        };
        assert!(state
            .accept(
                Sequence(5),
                &normal_after_terminal,
                after_transaction_expiry
            )
            .is_err());

        let extended_cleanup = Request::Synchronize {
            operation: OperationId(1),
            token_id: 1,
            deadline: ReleaseDeadline::Cleanup(CleanupDeadlineNs(cleanup.0 + 1)),
        };
        assert!(state
            .accept(Sequence(5), &extended_cleanup, after_transaction_expiry)
            .is_err());

        state
            .accept(
                Sequence(5),
                &Request::Synchronize {
                    operation: OperationId(1),
                    token_id: 1,
                    deadline: ReleaseDeadline::Cleanup(cleanup),
                },
                after_transaction_expiry,
            )
            .unwrap();
        state.complete_synchronize(1).unwrap();
        assert_eq!(state.debt_count(), 0);
    }

    #[test]
    fn cleanup_expiry_between_steps_keeps_debt_unreconciled() {
        let mut state = ProtocolState::ready(test_session()).unwrap();
        state
            .accept(
                Sequence(1),
                &Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        state.record_prepared(OperationId(1), token(1, 30)).unwrap();
        state
            .accept(
                Sequence(2),
                &Request::ExecuteDown {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        state
            .accept(
                Sequence(3),
                &Request::Synchronize {
                    operation: OperationId(1),
                    token_id: 1,
                    deadline: ReleaseDeadline::Mutation(mutation_deadline()),
                },
                NOW_NS,
            )
            .unwrap();
        state.complete_synchronize(1).unwrap();

        let cleanup = CleanupDeadlineNs(NOW_NS + 10);
        state
            .accept(
                Sequence(4),
                &Request::KeyUp {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: ReleaseDeadline::Cleanup(cleanup),
                },
                NOW_NS,
            )
            .unwrap();
        assert!(state
            .accept(
                Sequence(5),
                &Request::Synchronize {
                    operation: OperationId(1),
                    token_id: 1,
                    deadline: ReleaseDeadline::Cleanup(cleanup),
                },
                cleanup.0,
            )
            .is_err());
        assert_eq!(state.debt_count(), 1);
    }

    #[test]
    fn protocol_state_enforces_prepared_and_debt_capacity() {
        let mut prepared_state = ProtocolState::ready(test_session()).unwrap();
        for index in 1..=MAX_PREPARED_TOKENS {
            let sequence = Sequence(index as u64);
            prepared_state
                .accept(
                    sequence,
                    &Request::PrepareKey {
                        operation: OperationId(1),
                        evdev_code: 30,
                        deadline: mutation_deadline(),
                    },
                    NOW_NS,
                )
                .unwrap();
            prepared_state
                .record_prepared(OperationId(1), token(index as u64, 30))
                .unwrap();
        }
        prepared_state
            .accept(
                Sequence(MAX_PREPARED_TOKENS as u64 + 1),
                &Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        assert!(prepared_state
            .record_prepared(OperationId(1), token(MAX_PREPARED_TOKENS as u64 + 1, 30))
            .is_err());

        let mut debt_state = ProtocolState::ready(test_session()).unwrap();
        let mut sequence = 1u64;
        for token_id in 1..=MAX_ACTIVE_DEBTS as u64 {
            debt_state
                .accept(
                    Sequence(sequence),
                    &Request::PrepareKey {
                        operation: OperationId(1),
                        evdev_code: 30,
                        deadline: mutation_deadline(),
                    },
                    NOW_NS,
                )
                .unwrap();
            sequence += 1;
            debt_state
                .record_prepared(OperationId(1), token(token_id, 30))
                .unwrap();
            debt_state
                .accept(
                    Sequence(sequence),
                    &Request::ExecuteDown {
                        operation: OperationId(1),
                        token: token(token_id, 30),
                        deadline: mutation_deadline(),
                    },
                    NOW_NS,
                )
                .unwrap();
            sequence += 1;
            debt_state
                .accept(
                    Sequence(sequence),
                    &Request::Synchronize {
                        operation: OperationId(1),
                        token_id,
                        deadline: ReleaseDeadline::Mutation(mutation_deadline()),
                    },
                    NOW_NS,
                )
                .unwrap();
            sequence += 1;
            debt_state.complete_synchronize(token_id).unwrap();
        }
        debt_state
            .accept(
                Sequence(sequence),
                &Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 31,
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        sequence += 1;
        debt_state
            .record_prepared(OperationId(1), token(MAX_ACTIVE_DEBTS as u64 + 1, 31))
            .unwrap();
        assert!(debt_state
            .accept(
                Sequence(sequence),
                &Request::ExecuteDown {
                    operation: OperationId(1),
                    token: token(MAX_ACTIVE_DEBTS as u64 + 1, 31),
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .is_err());
        assert_eq!(debt_state.debt_count(), MAX_ACTIVE_DEBTS);
    }

    #[test]
    fn physical_release_requires_matching_generation_and_fresh_sequence() {
        let mut state = ProtocolState::ready(test_session()).unwrap();
        state
            .accept(
                Sequence(1),
                &Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        state.record_prepared(OperationId(1), token(1, 30)).unwrap();
        state
            .accept(
                Sequence(2),
                &Request::ExecuteDown {
                    operation: OperationId(1),
                    token: token(1, 30),
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();
        state
            .accept(
                Sequence(3),
                &Request::Synchronize {
                    operation: OperationId(1),
                    token_id: 1,
                    deadline: ReleaseDeadline::Mutation(mutation_deadline()),
                },
                NOW_NS,
            )
            .unwrap();
        state.complete_synchronize(1).unwrap();
        state
            .accept(
                Sequence(4),
                &Request::TransferToPhysicalDebt {
                    operation: OperationId(1),
                    token: token(1, 30),
                    input_generation: InputGeneration(5),
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .unwrap();

        let wrong_generation = Request::PhysicalReleaseCommitted {
            sequence: PhysicalSequence(1),
            token: token(1, 30),
            input_generation: InputGeneration(6),
            deadline: mutation_deadline(),
        };
        assert!(state
            .accept(Sequence(5), &wrong_generation, NOW_NS)
            .is_err());

        let committed = Request::PhysicalReleaseCommitted {
            sequence: PhysicalSequence(1),
            token: token(1, 30),
            input_generation: InputGeneration(5),
            deadline: mutation_deadline(),
        };
        state.accept(Sequence(5), &committed, NOW_NS).unwrap();
        assert_eq!(state.debt_count(), 0);

        assert!(state
            .accept(
                Sequence(6),
                &Request::PhysicalReleaseCommitted {
                    sequence: PhysicalSequence(1),
                    token: token(1, 30),
                    input_generation: InputGeneration(5),
                    deadline: mutation_deadline(),
                },
                NOW_NS,
            )
            .is_err());
    }
}
