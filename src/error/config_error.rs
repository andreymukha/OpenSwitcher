use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read or write config file")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse config.toml")]
    Parse(#[from] toml::de::Error),
    #[error("Failed to serialize config.toml")]
    Serialize(#[from] toml::ser::Error),
    #[error("Configuration lock is poisoned")]
    LockPoisoned,
}
