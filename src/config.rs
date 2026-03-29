use crate::error::ConfigError;
use crate::model::{LayoutSwitchKey, Settings, UndoKey};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub layout: LayoutConfig,
    pub delays: DelaysConfig,
    pub features: FeaturesConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutConfig {
    pub keys: Vec<LayoutSwitchKey>,
    pub delay_ms: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelaysConfig {
    pub backspace_ms: u32,
    pub typing_ms: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeaturesConfig {
    pub undo_key: UndoKey,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            layout: LayoutConfig {
                keys: vec![LayoutSwitchKey::LeftControl, LayoutSwitchKey::LeftShift],
                delay_ms: 30,
            },
            delays: DelaysConfig {
                backspace_ms: 0,
                typing_ms: 0,
            },
            features: FeaturesConfig {
                undo_key: UndoKey::Pause,
            },
        }
    }
}

impl AppConfig {
    pub fn load_or_create(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            let default_config = Self::default();
            default_config.save_to_path(path)?;
            return Ok(default_config);
        }

        let config_str = fs::read_to_string(path)?;
        Ok(toml::from_str(&config_str)?)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn settings(&self) -> Settings {
        Settings {
            layout_delay_ms: self.layout.delay_ms,
            undo_key: self.features.undo_key,
        }
    }

    pub fn apply_settings(&mut self, settings: Settings) {
        self.layout.delay_ms = settings.layout_delay_ms;
        self.features.undo_key = settings.undo_key;
    }
}

pub fn default_config_dir() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| {
        let mut fallback = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        fallback.push(".config");
        fallback
    });
    path.push("open-switcher");
    path
}

pub fn default_config_path() -> PathBuf {
    let mut path = default_config_dir();
    path.push("config.toml");
    path
}
