pub mod config_error;
pub mod dbus_error;
pub mod ui_error;

pub use config_error::ConfigError;
pub use dbus_error::DbusError;
pub use ui_error::{SettingsClientError, UiError};

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UndoKeyParseError {
    #[error("Неподдерживаемая клавиша ручного исправления: {value}")]
    UnsupportedValue { value: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Задержка переключения должна быть в диапазоне от {min} до {max} мс.")]
    LayoutDelayOutOfRange { min: u32, max: u32, found: u32 },
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
    Ui(#[from] UiError),
    #[error("Keyboard device was not found")]
    KeyboardNotFound,
    #[error("Failed to execute xset command")]
    Xset(#[source] std::io::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("UInput error")]
    UInput(#[from] uinput::Error),
}
