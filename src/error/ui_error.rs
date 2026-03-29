use crate::error::ValidationError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsClientError {
    #[error("Не удалось подключиться к session D-Bus")]
    Connection(#[source] zbus::Error),
    #[error("Не удалось создать D-Bus proxy")]
    Proxy(#[source] zbus::Error),
    #[error("Демон вернул ошибку")]
    Daemon(#[source] zbus::Error),
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

#[derive(Debug, Error)]
pub enum UiError {
    #[error("Не удалось отправить событие в UI")]
    Dispatch,
}
