use crate::error::{LayoutAutoDetectError, SystemContextError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read or write config file")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse config.toml")]
    Parse(#[from] toml::de::Error),
    #[error("Failed to serialize config.toml")]
    Serialize(#[from] toml::ser::Error),
    #[error("Failed to detect current system context")]
    SystemContext(#[from] SystemContextError),
    #[error("Failed to auto-detect layout switch combo")]
    LayoutAutoDetect(#[from] LayoutAutoDetectError),
    #[error(
        "Unsupported legacy config format detected; rewrite config.toml using the current format"
    )]
    LegacyFormatUnsupported,
    #[error("Missing required field in current config format: {field}")]
    MissingRequiredField { field: &'static str },
    #[error("Configuration lock is poisoned")]
    LockPoisoned,
}
