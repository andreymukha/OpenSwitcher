pub mod config_error;
pub mod dbus_error;
pub mod ui_error;

pub use config_error::ConfigError;
pub use dbus_error::DbusError;
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

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("Capture session lock is poisoned")]
    LockPoisoned,
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
    #[error("Daemon input loop panicked")]
    DaemonPanicked,
    #[error("Virtual keyboard writer is unavailable")]
    VirtualKeyboardWriterDisconnected,
    #[error("Virtual keyboard writer queue is saturated")]
    VirtualKeyboardWriterSaturated,
    #[error(transparent)]
    Ui(#[from] UiError),
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
    pub fn is_recoverable_input_error(&self) -> bool {
        match self {
            SwitcherError::KeyboardNotFound
            | SwitcherError::KeyboardAccessDenied { .. }
            | SwitcherError::UinputAccessDenied { .. } => true,
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
                "Linux input setup is not ready.\nKeyboard access is denied for: {}\nInstall or reinstall the OpenSwitcher .deb package:\n  sudo apt install --reinstall ./dist/packages/open-switcher_*_amd64.deb\nSign out and sign in again, then run `./manage.sh doctor`.",
                path.display()
            )),
            SwitcherError::UinputAccessDenied { path, .. } => Some(format!(
                "Linux input setup is not ready.\nuinput access is denied for: {}\nInstall or reinstall the OpenSwitcher .deb package:\n  sudo apt install --reinstall ./dist/packages/open-switcher_*_amd64.deb\nSign out and sign in again, then run `./manage.sh doctor`.",
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
    fn keyboard_access_denied_has_linux_input_setup_hint() {
        let error = SwitcherError::KeyboardAccessDenied {
            path: PathBuf::from("/dev/input/event4"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let hint = error
            .linux_input_setup_hint()
            .expect("setup hint must be present");

        assert!(hint.contains("./manage.sh doctor"));
        assert!(hint.contains("OpenSwitcher .deb"));
        assert!(hint.contains("sudo apt install --reinstall"));
        assert!(hint.contains("Sign out and sign in again"));
        assert!(!hint.contains("./manage.sh bootstrap linux-input"));
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

        assert!(hint.contains("./manage.sh doctor"));
        assert!(hint.contains("OpenSwitcher .deb"));
        assert!(hint.contains("sudo apt install --reinstall"));
        assert!(hint.contains("Sign out and sign in again"));
        assert!(!hint.contains("./manage.sh bootstrap linux-input"));
    }
}
