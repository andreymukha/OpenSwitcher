use crate::error::{ServiceManagerError, ValidationError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsClientError {
    #[error("Не удалось подключиться к session D-Bus")]
    Connection(#[source] zbus::Error),
    #[error("Не удалось создать D-Bus proxy")]
    Proxy(#[source] zbus::Error),
    #[error("Демон вернул ошибку")]
    Daemon(#[source] zbus::Error),
    #[error("Не удалось управлять user-сервисами OpenSwitcher")]
    ServiceManager(#[source] ServiceManagerError),
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

#[derive(Debug, Error)]
pub enum UiError {
    #[error("Не удалось отправить событие в UI")]
    Dispatch,
}
