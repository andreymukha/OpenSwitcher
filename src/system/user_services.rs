use crate::error::ServiceManagerError;
use std::process::Command;

pub const DAEMON_UNIT: &str = "open-switcher-daemon.service";
pub const TRAY_UNIT: &str = "open-switcher-tray.service";

pub trait CommandRunner: Clone {
    fn run(&self, command: &[&str]) -> Result<String, ServiceManagerError>;
}

#[derive(Clone, Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, command: &[&str]) -> Result<String, ServiceManagerError> {
        let (program, args) = command
            .split_first()
            .expect("command runner requires a non-empty command");
        let owned_command = command.iter().map(|part| (*part).to_string()).collect();
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| ServiceManagerError::SpawnFailed {
                command: owned_command,
                message: error.to_string(),
            })?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        Err(ServiceManagerError::CommandFailed {
            command: command.iter().map(|part| (*part).to_string()).collect(),
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[derive(Clone)]
pub struct UserServiceController<R = ProcessCommandRunner> {
    runner: R,
}

impl UserServiceController<ProcessCommandRunner> {
    pub fn from_system() -> Self {
        Self::new(ProcessCommandRunner)
    }
}

impl<R: CommandRunner> UserServiceController<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn enable_autostart(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "enable", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "enable", TRAY_UNIT])?;
        self.run(["systemctl", "--user", "start", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "start", TRAY_UNIT])?;
        Ok(())
    }

    pub fn disable_autostart(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "disable", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "disable", TRAY_UNIT])?;
        self.run(["systemctl", "--user", "stop", TRAY_UNIT])?;
        self.run(["systemctl", "--user", "stop", DAEMON_UNIT])?;
        Ok(())
    }

    pub fn is_autostart_enabled(&self) -> Result<bool, ServiceManagerError> {
        Ok(self.run(["systemctl", "--user", "is-enabled", DAEMON_UNIT])?.trim() == "enabled")
    }

    pub fn start_daemon_service(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "start", DAEMON_UNIT])?;
        Ok(())
    }

    pub fn start_tray_service(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "start", TRAY_UNIT])?;
        Ok(())
    }

    fn run(&self, command: [&str; 4]) -> Result<String, ServiceManagerError> {
        self.runner.run(&command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeCommandRunner {
        state: Arc<Mutex<FakeCommandRunnerState>>,
    }

    #[derive(Default)]
    struct FakeCommandRunnerState {
        commands: Vec<Vec<String>>,
        results: VecDeque<Result<String, ServiceManagerError>>,
    }

    impl FakeCommandRunner {
        fn push_ok(&mut self, stdout: &str) {
            self.state
                .lock()
                .unwrap()
                .results
                .push_back(Ok(stdout.to_string()));
        }

        fn push_err(&mut self, code: i32, stderr: &str) {
            self.state
                .lock()
                .unwrap()
                .results
                .push_back(Err(ServiceManagerError::CommandFailed {
                    command: Vec::new(),
                    code: Some(code),
                    stderr: stderr.to_string(),
                }));
        }

        fn commands(&self) -> Vec<Vec<String>> {
            self.state.lock().unwrap().commands.clone()
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, command: &[&str]) -> Result<String, ServiceManagerError> {
            let mut state = self.state.lock().unwrap();
            state
                .commands
                .push(command.iter().map(|part| (*part).to_string()).collect());

            match state.results.pop_front() {
                Some(Ok(stdout)) => Ok(stdout),
                Some(Err(ServiceManagerError::CommandFailed { code, stderr, .. })) => {
                    Err(ServiceManagerError::CommandFailed {
                        command: command.iter().map(|part| (*part).to_string()).collect(),
                        code,
                        stderr,
                    })
                }
                Some(Err(ServiceManagerError::SpawnFailed { message, .. })) => {
                    Err(ServiceManagerError::SpawnFailed {
                        command: command.iter().map(|part| (*part).to_string()).collect(),
                        message,
                    })
                }
                None => panic!("fake command runner has no queued result"),
            }
        }
    }

    #[test]
    fn enable_autostart_enables_and_starts_daemon_and_tray() {
        let mut runner = FakeCommandRunner::default();
        runner.push_ok("");
        runner.push_ok("");
        runner.push_ok("");
        runner.push_ok("");

        let services = UserServiceController::new(runner.clone());
        services.enable_autostart().unwrap();

        assert_eq!(
            runner.commands(),
            vec![
                vec![
                    "systemctl".to_string(),
                    "--user".to_string(),
                    "enable".to_string(),
                    "open-switcher-daemon.service".to_string(),
                ],
                vec![
                    "systemctl".to_string(),
                    "--user".to_string(),
                    "enable".to_string(),
                    "open-switcher-tray.service".to_string(),
                ],
                vec![
                    "systemctl".to_string(),
                    "--user".to_string(),
                    "start".to_string(),
                    "open-switcher-daemon.service".to_string(),
                ],
                vec![
                    "systemctl".to_string(),
                    "--user".to_string(),
                    "start".to_string(),
                    "open-switcher-tray.service".to_string(),
                ],
            ]
        );
    }

    #[test]
    fn autostart_checkbox_state_comes_from_daemon_unit_enabled_state() {
        let mut runner = FakeCommandRunner::default();
        runner.push_ok("enabled\n");

        let services = UserServiceController::new(runner);
        assert!(services.is_autostart_enabled().unwrap());
    }

    #[test]
    fn systemctl_failure_is_reported_as_runtime_failure() {
        let mut runner = FakeCommandRunner::default();
        runner.push_err(1, "permission denied");

        let services = UserServiceController::new(runner);
        let err = services.start_daemon_service().unwrap_err();

        assert!(matches!(err, ServiceManagerError::CommandFailed { .. }));
    }
}
