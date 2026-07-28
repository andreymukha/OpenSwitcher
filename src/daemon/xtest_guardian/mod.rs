pub(crate) mod client;
#[cfg(test)]
mod process_tests;
pub(crate) mod protocol;
pub(crate) mod runtime;
pub(crate) mod seqpacket;
pub(crate) mod service;
pub(crate) mod x11;

use crate::daemon::synthetic_input::TerminalProof;
use crate::daemon::xtest_guardian::protocol::{ProtocolSession, SessionId};
use crate::daemon::xtest_guardian::seqpacket::ActivatedListener;
use crate::daemon::xtest_guardian::service::{
    GuardianWaitOutcome, StopReason, TerminalRecord, XtestExecutor,
};
use crate::daemon::xtest_guardian::x11::GuardianX11Executor;
use crate::error::{InputSafetyError, SwitcherError};
use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags};
use nix::sys::signal::{pthread_sigmask, SigSet, SigmaskHow, Signal};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::thread::{self, JoinHandle};

enum ActivationOutcome<C> {
    Accepted(C),
    StopRequested,
}

struct SignalWakeup {
    reader: UnixStream,
    _waiter: JoinHandle<()>,
}

impl SignalWakeup {
    fn install() -> Result<Self, SwitcherError> {
        let (reader, mut writer) = UnixStream::pair()?;
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGTERM);
        signals.add(Signal::SIGINT);
        let mut previous = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), Some(&mut previous))
            .map_err(nix_signal_error)?;

        let waiter = match thread::Builder::new()
            .name("openswitcher-xtest-signal".to_owned())
            .spawn(move || {
                if signals.wait().is_ok() {
                    let _ = writer.write_all(&[1]);
                }
            }) {
            Ok(waiter) => waiter,
            Err(error) => {
                let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
                return Err(error.into());
            }
        };

        Ok(Self {
            reader,
            _waiter: waiter,
        })
    }

    fn as_raw_fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }

    fn is_requested(&self) -> Result<bool, SwitcherError> {
        let mut poll_fd = [PollFd::new(self.reader.as_raw_fd(), PollFlags::POLLIN)];
        loop {
            match poll(&mut poll_fd, 0) {
                Ok(_) => {
                    let events = poll_fd[0].revents().unwrap_or_else(PollFlags::empty);
                    return Ok(events.intersects(
                        PollFlags::POLLIN
                            | PollFlags::POLLHUP
                            | PollFlags::POLLERR
                            | PollFlags::POLLNVAL,
                    ));
                }
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(nix_signal_error(error)),
            }
        }
    }
}

fn nix_signal_error(error: Errno) -> SwitcherError {
    std::io::Error::from_raw_os_error(error as i32).into()
}

pub(crate) fn run_internal_v1() -> Result<(), SwitcherError> {
    let signal = SignalWakeup::install()?;
    run_internal_guardian_with(
        || {
            let listener = ActivatedListener::from_process_environment()?;
            match service::wait_for_guardian_io_or_stop(listener.as_raw_fd(), signal.as_raw_fd())? {
                GuardianWaitOutcome::DataReady => {
                    let connection = listener.accept_authenticated()?;
                    if signal.is_requested()? {
                        Ok(ActivationOutcome::StopRequested)
                    } else {
                        Ok(ActivationOutcome::Accepted(connection))
                    }
                }
                GuardianWaitOutcome::StopRequested => Ok(ActivationOutcome::StopRequested),
                GuardianWaitOutcome::DataClosed => Err(InputSafetyError::GuardianUnavailable {
                    context: "activated guardian listener closed before accept",
                }
                .into()),
            }
        },
        GuardianX11Executor::connect_and_establish,
        |connection, mut executor| {
            let session = ProtocolSession {
                session: read_nonzero_session_id()?,
                epoch: executor.server_identity().epoch,
            };
            service::run_connection_until_signal(
                &connection,
                session,
                &mut executor,
                signal.as_raw_fd(),
            )
        },
    )
}

fn run_internal_guardian_with<C, E>(
    activate: impl FnOnce() -> Result<ActivationOutcome<C>, SwitcherError>,
    connect_x11: impl FnOnce() -> Result<E, SwitcherError>,
    serve: impl FnOnce(C, E) -> Result<TerminalRecord, SwitcherError>,
) -> Result<(), SwitcherError> {
    let connection = match activate()? {
        ActivationOutcome::Accepted(connection) => connection,
        ActivationOutcome::StopRequested => return Ok(()),
    };
    let executor = connect_x11()?;
    terminal_record_result(serve(connection, executor)?)
}

fn terminal_record_result(record: TerminalRecord) -> Result<(), SwitcherError> {
    match record.proof {
        TerminalProof::Unreconciled { remaining } => Err(InputSafetyError::Reconciliation {
            operation_id: 0,
            remaining,
        }
        .into()),
        TerminalProof::OwnerGenerationDestroyed { .. } => Err(InputSafetyError::Invariant {
            context: "XTEST guardian returned a uinput owner-generation proof",
        }
        .into()),
        TerminalProof::Reconciled => match record.reason {
            StopReason::ProtocolViolation => Err(InputSafetyError::GuardianProtocol {
                context: "XTEST guardian stopped after a protocol violation",
            }
            .into()),
            StopReason::BackendFailure => Err(InputSafetyError::GuardianUnavailable {
                context: "XTEST guardian stopped after an X11 backend failure",
            }
            .into()),
            StopReason::PeerEof | StopReason::Signal | StopReason::Requested | StopReason::Drop => {
                Ok(())
            }
        },
    }
}

fn read_nonzero_session_id() -> Result<SessionId, SwitcherError> {
    let mut session = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut session)?;
    if session.iter().all(|byte| *byte == 0) {
        return Err(InputSafetyError::GuardianProtocol {
            context: "XTEST guardian session identifier must be nonzero",
        }
        .into());
    }
    Ok(SessionId(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InputSafetyError;
    use std::cell::RefCell;

    #[test]
    fn activation_failure_is_rejected_before_x11_connection() {
        let trace = RefCell::new(Vec::new());

        let result = run_internal_guardian_with(
            || {
                trace.borrow_mut().push("activation-rejected");
                Err::<ActivationOutcome<()>, SwitcherError>(
                    InputSafetyError::GuardianUnavailable {
                        context: "test activation rejected",
                    }
                    .into(),
                )
            },
            || {
                trace.borrow_mut().push("x11-connect");
                Ok(())
            },
            |_, _| unreachable!("service cannot run after activation rejection"),
        );

        assert!(result.is_err());
        assert_eq!(*trace.borrow(), ["activation-rejected"]);
    }
}
