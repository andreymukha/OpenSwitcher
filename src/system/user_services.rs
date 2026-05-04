use crate::error::ServiceManagerError;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DAEMON_UNIT: &str = "open-switcher-daemon.service";
pub const TRAY_UNIT: &str = "open-switcher-tray.service";

const XDG_AUTOSTART_FILE: &str = "open-switcher.desktop";
const XDG_AUTOSTART_CONTENT: &str = "[Desktop Entry]\nType=Application\nName=OpenSwitcher\nComment=Start OpenSwitcher tray service\nExec=systemctl --user start open-switcher-tray.service\nX-GNOME-Autostart-enabled=true\n";

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
        let output = Command::new(program).args(args).output().map_err(|error| {
            ServiceManagerError::SpawnFailed {
                command: owned_command,
                message: error.to_string(),
            }
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
    autostart_file: PathBuf,
}

impl UserServiceController<ProcessCommandRunner> {
    pub fn from_system() -> Self {
        Self::new(ProcessCommandRunner)
    }
}

impl<R: CommandRunner> UserServiceController<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            autostart_file: default_xdg_autostart_file(),
        }
    }

    pub fn enable_autostart(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "enable", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "enable", TRAY_UNIT])?;
        self.install_xdg_autostart_fallback()?;
        Ok(())
    }

    pub fn disable_autostart(&self) -> Result<(), ServiceManagerError> {
        self.run(["systemctl", "--user", "disable", DAEMON_UNIT])?;
        self.run(["systemctl", "--user", "disable", TRAY_UNIT])?;
        self.remove_xdg_autostart_fallback()?;
        Ok(())
    }

    pub fn is_autostart_enabled(&self) -> Result<bool, ServiceManagerError> {
        match self.run(["systemctl", "--user", "is-enabled", DAEMON_UNIT]) {
            Ok(output) => Ok(output.trim() == "enabled" && self.xdg_autostart_fallback_installed()),
            Err(ServiceManagerError::CommandFailed {
                code: Some(1 | 4), ..
            }) => Ok(false),
            Err(error) => Err(error),
        }
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

    fn install_xdg_autostart_fallback(&self) -> Result<(), ServiceManagerError> {
        if let Some(parent) = self.autostart_file.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                file_service_manager_error("create XDG autostart directory", parent, error)
            })?;
        }

        fs::write(&self.autostart_file, XDG_AUTOSTART_CONTENT).map_err(|error| {
            file_service_manager_error("write XDG autostart fallback", &self.autostart_file, error)
        })
    }

    fn remove_xdg_autostart_fallback(&self) -> Result<(), ServiceManagerError> {
        match fs::remove_file(&self.autostart_file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(file_service_manager_error(
                "remove XDG autostart fallback",
                &self.autostart_file,
                error,
            )),
        }
    }

    fn xdg_autostart_fallback_installed(&self) -> bool {
        match fs::read_to_string(&self.autostart_file) {
            Ok(content) => content == XDG_AUTOSTART_CONTENT,
            Err(_) => false,
        }
    }
}

fn default_xdg_autostart_file() -> PathBuf {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));

    config_home.join("autostart").join(XDG_AUTOSTART_FILE)
}

fn file_service_manager_error(action: &str, path: &Path, error: std::io::Error) -> ServiceManagerError {
    ServiceManagerError::SpawnFailed {
        command: vec![action.to_string(), path.display().to_string()],
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    struct XdgConfigHomeGuard {
        previous: Option<OsString>,
    }

    impl XdgConfigHomeGuard {
        fn install(path: &Path) -> Self {
            let previous = env::var_os("XDG_CONFIG_HOME");
            env::set_var("XDG_CONFIG_HOME", path);
            Self { previous }
        }
    }

    impl Drop for XdgConfigHomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => env::set_var("XDG_CONFIG_HOME", value),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn with_temp_xdg_config_home(test: impl FnOnce(&Path)) {
        let _guard = env_lock();
        let temp_dir = tempfile::tempdir().unwrap();
        let _env = XdgConfigHomeGuard::install(temp_dir.path());
        test(temp_dir.path());
    }

    fn xdg_autostart_file(config_home: &Path) -> PathBuf {
        config_home.join("autostart").join("open-switcher.desktop")
    }

    fn expected_xdg_autostart_content() -> &'static str {
        "[Desktop Entry]\nType=Application\nName=OpenSwitcher\nComment=Start OpenSwitcher tray service\nExec=systemctl --user start open-switcher-tray.service\nX-GNOME-Autostart-enabled=true\n"
    }

    #[test]
    fn enable_autostart_only_enables_daemon_and_tray() {
        with_temp_xdg_config_home(|_| {
            let mut runner = FakeCommandRunner::default();
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
                ]
            );
        });
    }

    #[test]
    fn disable_autostart_only_disables_daemon_and_tray() {
        with_temp_xdg_config_home(|_| {
            let mut runner = FakeCommandRunner::default();
            runner.push_ok("");
            runner.push_ok("");

            let services = UserServiceController::new(runner.clone());
            services.disable_autostart().unwrap();

            assert_eq!(
                runner.commands(),
                vec![
                    vec![
                        "systemctl".to_string(),
                        "--user".to_string(),
                        "disable".to_string(),
                        "open-switcher-daemon.service".to_string(),
                    ],
                    vec![
                        "systemctl".to_string(),
                        "--user".to_string(),
                        "disable".to_string(),
                        "open-switcher-tray.service".to_string(),
                    ],
                ]
            );
        });
    }

    #[test]
    fn enable_autostart_installs_xdg_autostart_fallback() {
        with_temp_xdg_config_home(|config_home| {
            let mut runner = FakeCommandRunner::default();
            runner.push_ok("");
            runner.push_ok("");

            let services = UserServiceController::new(runner);
            services.enable_autostart().unwrap();

            let autostart_file = xdg_autostart_file(config_home);
            assert_eq!(
                fs::read_to_string(autostart_file).unwrap(),
                expected_xdg_autostart_content()
            );
        });
    }

    #[test]
    fn disable_autostart_removes_xdg_autostart_fallback() {
        with_temp_xdg_config_home(|config_home| {
            let autostart_file = xdg_autostart_file(config_home);
            fs::create_dir_all(autostart_file.parent().unwrap()).unwrap();
            fs::write(&autostart_file, expected_xdg_autostart_content()).unwrap();

            let mut runner = FakeCommandRunner::default();
            runner.push_ok("");
            runner.push_ok("");

            let services = UserServiceController::new(runner);
            services.disable_autostart().unwrap();

            assert!(!autostart_file.exists());
        });
    }

    #[test]
    fn autostart_checkbox_state_comes_from_daemon_unit_enabled_state() {
        with_temp_xdg_config_home(|config_home| {
            let autostart_file = xdg_autostart_file(config_home);
            fs::create_dir_all(autostart_file.parent().unwrap()).unwrap();
            fs::write(&autostart_file, expected_xdg_autostart_content()).unwrap();

            let mut runner = FakeCommandRunner::default();
            runner.push_ok("enabled\n");

            let services = UserServiceController::new(runner);
            assert!(services.is_autostart_enabled().unwrap());
        });
    }

    #[test]
    fn missing_xdg_autostart_fallback_is_reported_as_autostart_off() {
        with_temp_xdg_config_home(|_| {
            let mut runner = FakeCommandRunner::default();
            runner.push_ok("enabled\n");

            let services = UserServiceController::new(runner);
            assert!(!services.is_autostart_enabled().unwrap());
        });
    }

    #[test]
    fn disabled_daemon_unit_is_reported_as_autostart_off() {
        with_temp_xdg_config_home(|_| {
            let mut runner = FakeCommandRunner::default();
            runner.push_err(1, "");

            let services = UserServiceController::new(runner);
            assert!(!services.is_autostart_enabled().unwrap());
        });
    }

    #[test]
    fn missing_daemon_unit_is_reported_as_autostart_off() {
        with_temp_xdg_config_home(|_| {
            let mut runner = FakeCommandRunner::default();
            runner.push_err(4, "");

            let services = UserServiceController::new(runner);
            assert!(!services.is_autostart_enabled().unwrap());
        });
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
