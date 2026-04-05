use crate::error::ServiceManagerError;
use crate::system::user_services::{CommandRunner, UserServiceController};
use std::thread;
use std::time::Duration;
use zbus::blocking::Connection;

pub const TRAY_SERVICE_NAME: &str = "org.oswitch.tray";
pub const MAX_DAEMON_RECOVERY_ATTEMPTS: usize = 3;
pub const DAEMON_RECOVERY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayInstanceError {
    AlreadyRunning,
    Dbus(String),
}

pub trait TrayNameRequester {
    fn request_tray_name(&self) -> Result<(), TrayInstanceError>;
}

pub trait DaemonStarter {
    fn start_daemon_service(&self) -> Result<(), ServiceManagerError>;
}

impl TrayNameRequester for Connection {
    fn request_tray_name(&self) -> Result<(), TrayInstanceError> {
        match self.request_name(TRAY_SERVICE_NAME) {
            Ok(()) => Ok(()),
            Err(zbus::Error::NameTaken) => Err(TrayInstanceError::AlreadyRunning),
            Err(error) => Err(TrayInstanceError::Dbus(error.to_string())),
        }
    }
}

impl<R: CommandRunner> DaemonStarter for UserServiceController<R> {
    fn start_daemon_service(&self) -> Result<(), ServiceManagerError> {
        UserServiceController::start_daemon_service(self)
    }
}

pub fn acquire_tray_instance(requester: &impl TrayNameRequester) -> Result<(), TrayInstanceError> {
    requester.request_tray_name()
}

pub fn start_daemon_with_retry(
    starter: &impl DaemonStarter,
    attempts: usize,
    delay: Duration,
) -> Result<(), ServiceManagerError> {
    let mut last_error = None;

    for attempt in 0..attempts {
        match starter.start_daemon_service() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < attempts && !delay.is_zero() {
                    thread::sleep(delay);
                }
            }
        }
    }

    Err(last_error.expect("daemon retry requires at least one attempt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::time::Duration;

    #[test]
    fn second_tray_instance_is_rejected_when_name_is_taken() {
        let bus = FakeNameRequester {
            result: RefCell::new(Some(Err(TrayInstanceError::AlreadyRunning))),
        };

        let err = acquire_tray_instance(&bus).unwrap_err();
        assert_eq!(err, TrayInstanceError::AlreadyRunning);
    }

    #[test]
    fn daemon_restart_is_attempted_three_times_before_tray_gives_up() {
        let starter = FakeDaemonStarter {
            results: RefCell::new(VecDeque::from(vec![
                Err(ServiceManagerError::CommandFailed {
                    command: vec!["systemctl".into()],
                    code: Some(1),
                    stderr: "fail-1".into(),
                }),
                Err(ServiceManagerError::CommandFailed {
                    command: vec!["systemctl".into()],
                    code: Some(1),
                    stderr: "fail-2".into(),
                }),
                Err(ServiceManagerError::CommandFailed {
                    command: vec!["systemctl".into()],
                    code: Some(1),
                    stderr: "fail-3".into(),
                }),
            ])),
            calls: RefCell::new(0),
        };

        let result = start_daemon_with_retry(&starter, 3, Duration::ZERO);
        assert!(result.is_err());
        assert_eq!(*starter.calls.borrow(), 3);
    }

    struct FakeNameRequester {
        result: RefCell<Option<Result<(), TrayInstanceError>>>,
    }

    impl TrayNameRequester for FakeNameRequester {
        fn request_tray_name(&self) -> Result<(), TrayInstanceError> {
            self.result
                .borrow_mut()
                .take()
                .expect("fake request result must be queued")
        }
    }

    struct FakeDaemonStarter {
        results: RefCell<VecDeque<Result<(), ServiceManagerError>>>,
        calls: RefCell<usize>,
    }

    impl DaemonStarter for FakeDaemonStarter {
        fn start_daemon_service(&self) -> Result<(), ServiceManagerError> {
            *self.calls.borrow_mut() += 1;
            self.results
                .borrow_mut()
                .pop_front()
                .expect("fake daemon start result must be queued")
        }
    }
}
