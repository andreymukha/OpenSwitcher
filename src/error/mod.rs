pub mod config_error;
pub mod dbus_error;
pub mod input_safety_error;
pub mod ui_error;

pub use config_error::ConfigError;
pub use dbus_error::DbusError;
pub use input_safety_error::InputSafetyError;
pub use ui_error::{SettingsClientError, UiError};

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UndoKeyParseError {
    #[error("Неподдерживаемая клавиша ручного исправления: {value}")]
    UnsupportedValue { value: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SelectedTextHotkeyParseError {
    #[error("Неподдерживаемая горячая клавиша для выделенного текста: {value}")]
    UnsupportedValue { value: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LayoutSwitchComboParseError {
    #[error("Неподдерживаемая комбинация переключения раскладки: {value}")]
    UnsupportedValue { value: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Задержка переключения должна быть в диапазоне от {min} до {max} мс.")]
    LayoutDelayOutOfRange { min: u32, max: u32, found: u32 },
    #[error("Задержка {field} должна быть не больше {max} мс (получено {found} мс).")]
    InputDelayOutOfRange {
        field: &'static str,
        max: u32,
        found: u32,
    },
    #[error(
        "Максимальная расчётная длительность коррекции должна быть не больше {max_ms} мс (получено {found_ms} мс)."
    )]
    InputCorrectionScheduleTooLong { max_ms: u64, found_ms: u64 },
    #[error(
        "План коррекции превышает предел: максимум {max_strokes} клавиш и {max_extra_backspaces} дополнительное удаление (получено {strokes} и {extra_backspaces})."
    )]
    InputCorrectionPlanTooLarge {
        max_strokes: usize,
        max_extra_backspaces: usize,
        strokes: usize,
        extra_backspaces: usize,
    },
    #[error("Горячие клавиши ручного исправления и выделенного текста совпадают: {hotkey}")]
    DuplicateHotkey { hotkey: String },
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("Failed to load configuration")]
    LoadFailed(#[source] ConfigError),
    #[error("Failed to save configuration")]
    SaveFailed(#[source] ConfigError),
    #[error("Configuration lock is poisoned")]
    LockPoisoned,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CaptureError {
    #[error("Capture session lock is poisoned")]
    LockPoisoned,
    #[error("A layout switch capture session is already owned by another caller")]
    Busy,
    #[error("The caller does not own the active layout switch capture session")]
    NotOwner,
    #[error("No owned layout switch capture session is active")]
    NotActive,
}

#[derive(Debug, Error)]
pub enum SelectedTextError {
    #[error("Не удалось получить доступ к буферу обмена")]
    ClipboardUnavailable(#[source] arboard::Error),
    #[error("Не удалось прочитать текст из буфера обмена")]
    ClipboardRead(#[source] arboard::Error),
    #[error("Не удалось записать текст в буфер обмена")]
    ClipboardWrite(#[source] arboard::Error),
    #[error("Не удалось очистить буфер обмена")]
    ClipboardClear(#[source] arboard::Error),
}

#[derive(Debug, Error)]
pub enum SwitcherError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Dbus(#[from] zbus::Error),
    #[error(transparent)]
    DbusApi(#[from] DbusError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SelectedText(#[from] SelectedTextError),
    #[error("Selected text worker thread is unavailable")]
    SelectedTextWorkerDisconnected,
    #[error("Input worker {worker} is unavailable")]
    InputWorkerDisconnected { worker: &'static str },
    #[error("Input worker {worker} did not become ready within {timeout_ms} ms")]
    InputWorkerStartupTimedOut {
        worker: &'static str,
        timeout_ms: u64,
    },
    #[error("Cannot install a newly grabbed input backend while another backend is active")]
    InputBackendAlreadyActive,
    #[error("Daemon input loop panicked")]
    DaemonPanicked,
    #[error("Virtual keyboard writer is unavailable")]
    VirtualKeyboardWriterDisconnected,
    #[error("Virtual keyboard writer did not become ready within {timeout_ms} ms")]
    VirtualKeyboardWriterStartupTimedOut { timeout_ms: u64 },
    #[error(
        "Virtual keyboard writer did not stop within {timeout_ms} ms during {phase}; trigger: {trigger}"
    )]
    VirtualKeyboardWriterShutdownUnresponsive {
        timeout_ms: u64,
        phase: &'static str,
        trigger: String,
    },
    #[error("Virtual keyboard writer queue is saturated")]
    VirtualKeyboardWriterSaturated,
    #[error("Virtual keyboard writer transaction {request_id} exceeded its deadline")]
    VirtualKeyboardWriterTransactionTimedOut { request_id: u64 },
    #[error("Virtual keyboard writer transaction {request_id} was cancelled")]
    VirtualKeyboardWriterTransactionCancelled { request_id: u64 },
    #[error("Virtual keyboard writer transaction {request_id} failed after mutation: {reason}")]
    VirtualKeyboardWriterTransactionFailed { request_id: u64, reason: String },
    #[error("Deferred physical input reached emergency capacity {limit}")]
    DeferredInputCapacityExceeded { limit: usize },
    #[error(transparent)]
    InputWorkValidation(#[from] ValidationError),
    #[error(transparent)]
    InputSafety(#[from] InputSafetyError),
    #[error(transparent)]
    Ui(#[from] UiError),
    #[error("Input session is not currently authorized")]
    InputSessionInactive,
    #[error("Required logind session monitor stopped")]
    SessionMonitorStopped,
    #[error("Keyboard device was not found")]
    KeyboardNotFound,
    #[error("Keyboard device is present but access was denied: {path}")]
    KeyboardAccessDenied {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("uinput device is present but access was denied: {path}")]
    UinputAccessDenied {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to execute xset command")]
    Xset(#[source] std::io::Error),
    #[error("Failed to detect system context")]
    SystemContext(#[from] SystemContextError),
    #[error("Failed to auto-detect layout switch combo")]
    LayoutAutoDetect(#[from] LayoutAutoDetectError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("UInput error")]
    UInput(#[from] uinput::Error),
}

#[derive(Debug, Error)]
pub enum SystemContextError {
    #[error("Failed to read /etc/os-release")]
    OsReleaseIo(#[source] std::io::Error),
}

#[derive(Debug, Error)]
pub enum LayoutAutoDetectError {
    #[error("Failed to execute gsettings")]
    GSettingsIo(#[source] std::io::Error),
    #[error("gsettings returned a non-zero exit status")]
    GSettingsFailed { stderr: String },
    #[error("Failed to execute xfconf-query")]
    XfconfIo(#[source] std::io::Error),
    #[error("xfconf-query returned a non-zero exit status")]
    XfconfFailed { stderr: String },
    #[error("Failed to execute setxkbmap")]
    SetXkbMapIo(#[source] std::io::Error),
    #[error("setxkbmap returned a non-zero exit status")]
    SetXkbMapFailed { stderr: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ServiceManagerError {
    #[error("Failed to execute command: {command:?}")]
    SpawnFailed {
        command: Vec<String>,
        message: String,
    },
    #[error("Command failed: {command:?} (code={code:?})")]
    CommandFailed {
        command: Vec<String>,
        code: Option<i32>,
        stderr: String,
    },
}

impl SwitcherError {
    pub(crate) fn input_safety(context: &'static str) -> Self {
        InputSafetyError::Invariant { context }.into()
    }

    pub fn is_recoverable_input_error(&self) -> bool {
        match self {
            SwitcherError::KeyboardNotFound
            | SwitcherError::KeyboardAccessDenied { .. }
            | SwitcherError::UinputAccessDenied { .. }
            | SwitcherError::InputSessionInactive
            | SwitcherError::InputWorkerDisconnected { .. } => true,
            SwitcherError::Io(error) => {
                matches!(error.raw_os_error(), Some(19))
                    || error.to_string().contains("No such device")
            }
            _ => false,
        }
    }

    pub fn linux_input_setup_hint(&self) -> Option<String> {
        match self {
            SwitcherError::KeyboardAccessDenied { path, .. } => Some(format!(
                concat!(
                    "Linux input setup is not ready.\n",
                    "Keyboard access is denied for: {}\n",
                    "Install a trusted OpenSwitcher .deb with your package manager.\n",
                    "If the same package version is already installed, use the package manager's ",
                    "reinstall option.\n",
                    "Sign out and sign in again."
                ),
                path.display()
            )),
            SwitcherError::UinputAccessDenied { path, .. } => Some(format!(
                concat!(
                    "Linux input setup is not ready.\n",
                    "uinput access is denied for: {}\n",
                    "Install a trusted OpenSwitcher .deb with your package manager.\n",
                    "If the same package version is already installed, use the package manager's ",
                    "reinstall option.\n",
                    "Sign out and sign in again."
                ),
                path.display()
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_safety_helper_creates_typed_invariant_without_payload_data() {
        let error = SwitcherError::input_safety("test invariant");

        assert!(matches!(
            error,
            SwitcherError::InputSafety(InputSafetyError::Invariant {
                context: "test invariant",
            })
        ));
    }

    #[test]
    fn keyboard_access_denied_is_recoverable_input_error() {
        let error = SwitcherError::KeyboardAccessDenied {
            path: PathBuf::from("/dev/input/event3"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        assert!(error.is_recoverable_input_error());
    }

    #[test]
    fn plain_io_error_is_not_recoverable_input_error() {
        let error = SwitcherError::Io(std::io::Error::other("boom"));

        assert!(!error.is_recoverable_input_error());
    }

    #[test]
    fn disconnected_worker_is_recoverable_but_startup_timeout_requires_process_restart() {
        let disconnected = SwitcherError::InputWorkerDisconnected {
            worker: "pointer-watcher",
        };
        let timed_out = SwitcherError::InputWorkerStartupTimedOut {
            worker: "input-target-watcher",
            timeout_ms: 5_000,
        };

        assert!(disconnected.is_recoverable_input_error());
        assert!(!timed_out.is_recoverable_input_error());
    }

    #[test]
    fn inactive_input_session_is_recoverable_but_monitor_loss_is_fatal() {
        assert!(SwitcherError::InputSessionInactive.is_recoverable_input_error());
        assert!(!SwitcherError::SessionMonitorStopped.is_recoverable_input_error());
    }

    #[test]
    fn unresponsive_writer_shutdown_requires_process_restart() {
        let error = SwitcherError::VirtualKeyboardWriterShutdownUnresponsive {
            timeout_ms: 1_000,
            phase: "keyboard-shutdown",
            trigger: "test trigger".to_string(),
        };

        assert!(!error.is_recoverable_input_error());
    }

    #[test]
    fn guardian_failure_requires_process_restart() {
        let unavailable = SwitcherError::InputSafety(InputSafetyError::GuardianUnavailable {
            context: "test guardian loss",
        });
        let unreconciled =
            SwitcherError::InputSafety(InputSafetyError::GuardianEmergencyTimedOut {
                remaining: 1,
            });

        assert!(!unavailable.is_recoverable_input_error());
        assert!(!unreconciled.is_recoverable_input_error());
    }

    #[test]
    fn keyboard_access_denied_has_linux_input_setup_hint() {
        let error = SwitcherError::KeyboardAccessDenied {
            path: PathBuf::from("/dev/input/event4"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let hint = error
            .linux_input_setup_hint()
            .expect("setup hint must be present");

        assert!(hint.contains("trusted OpenSwitcher .deb"));
        assert!(hint.contains("package manager"));
        assert!(hint.contains("same package version"));
        assert!(hint.contains("reinstall option"));
        assert!(hint.contains("Sign out and sign in again"));
        assert!(!hint.contains("./manage.sh bootstrap linux-input"));
        assert!(!hint.contains("./manage.sh"));
        assert!(!hint.contains("./dist"));
        assert!(!hint.contains("Build"));
        assert!(!hint.contains("<artifact>"));
    }

    #[test]
    fn uinput_access_denied_has_linux_input_setup_hint() {
        let error = SwitcherError::UinputAccessDenied {
            path: PathBuf::from("/dev/uinput"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let hint = error
            .linux_input_setup_hint()
            .expect("setup hint must be present");

        assert!(hint.contains("trusted OpenSwitcher .deb"));
        assert!(hint.contains("package manager"));
        assert!(hint.contains("same package version"));
        assert!(hint.contains("reinstall option"));
        assert!(hint.contains("Sign out and sign in again"));
        assert!(!hint.contains("./manage.sh bootstrap linux-input"));
        assert!(!hint.contains("./manage.sh"));
        assert!(!hint.contains("./dist"));
        assert!(!hint.contains("Build"));
        assert!(!hint.contains("<artifact>"));
    }
}
