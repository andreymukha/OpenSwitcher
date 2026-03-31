use crate::error::{CaptureError, SettingsError};
use thiserror::Error;
use zbus::fdo;

#[derive(Debug, Error)]
pub enum DbusError {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("Failed to emit D-Bus signal")]
    Signal(#[source] zbus::Error),
}

impl From<DbusError> for fdo::Error {
    fn from(value: DbusError) -> Self {
        fdo::Error::Failed(value.to_string())
    }
}
