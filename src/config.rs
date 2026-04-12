use crate::error::ConfigError;
use crate::model::{
    AutoDetectedLayoutSwitch, LayoutSwitchCombo, LayoutSwitchSetting, LayoutSwitchSource,
    SelectedTextHotkey, Settings, UndoKey,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    pub layout: LayoutConfig,
    pub delays: DelaysConfig,
    pub features: FeaturesConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutConfig {
    pub switch_combo: LayoutSwitchCombo,
    pub switch_source: LayoutSwitchSource,
    pub auto_detected: AutoDetectedLayoutSwitch,
    pub delay_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelaysConfig {
    pub backspace_ms: u32,
    pub typing_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturesConfig {
    #[serde(default = "default_auto_switch_enabled")]
    pub auto_switch_enabled: bool,
    #[serde(default)]
    pub fix_two_capitals: bool,
    #[serde(default)]
    pub fix_accidental_caps_lock: bool,
    pub undo_key: UndoKey,
    #[serde(default)]
    pub selected_text_switch_hotkey: SelectedTextHotkey,
}

fn default_auto_switch_enabled() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            layout: LayoutConfig {
                switch_combo: LayoutSwitchCombo::default(),
                switch_source: LayoutSwitchSource::Unknown,
                auto_detected: AutoDetectedLayoutSwitch::default(),
                delay_ms: 30,
            },
            delays: DelaysConfig {
                backspace_ms: 0,
                typing_ms: 0,
            },
            features: FeaturesConfig {
                auto_switch_enabled: true,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                undo_key: UndoKey::Pause,
                selected_text_switch_hotkey: SelectedTextHotkey::default(),
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
        let file: AppConfigFile = toml::from_str(&config_str)?;
        AppConfig::try_from(file)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(&AppConfigFile::from(self))?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn settings(&self) -> Settings {
        Settings {
            auto_switch_enabled: self.features.auto_switch_enabled,
            fix_two_capitals: self.features.fix_two_capitals,
            fix_accidental_caps_lock: self.features.fix_accidental_caps_lock,
            layout_delay_ms: self.layout.delay_ms,
            undo_key: self.features.undo_key,
            selected_text_hotkey: self.features.selected_text_switch_hotkey,
            layout_switch: LayoutSwitchSetting {
                combo: self.layout.switch_combo,
                source: self.layout.switch_source,
                auto_detected: self.layout.auto_detected,
            },
        }
    }

    pub fn apply_settings(&mut self, settings: Settings) {
        self.features.auto_switch_enabled = settings.auto_switch_enabled;
        self.features.fix_two_capitals = settings.fix_two_capitals;
        self.features.fix_accidental_caps_lock = settings.fix_accidental_caps_lock;
        self.layout.delay_ms = settings.layout_delay_ms;
        self.layout.switch_combo = settings.layout_switch.combo;
        self.layout.switch_source = settings.layout_switch.source;
        self.layout.auto_detected = settings.layout_switch.auto_detected;
        self.features.undo_key = settings.undo_key;
        self.features.selected_text_switch_hotkey = settings.selected_text_hotkey;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppConfigFile {
    layout: LayoutConfigFile,
    delays: DelaysConfig,
    features: FeaturesConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LayoutConfigFile {
    #[serde(default)]
    switch_combo: Option<LayoutSwitchCombo>,
    #[serde(default)]
    switch_source: LayoutSwitchSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_detected: Option<AutoDetectedLayoutSwitch>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    delay_ms: u32,
}

impl From<&AppConfig> for AppConfigFile {
    fn from(value: &AppConfig) -> Self {
        Self {
            layout: LayoutConfigFile {
                switch_combo: Some(value.layout.switch_combo),
                switch_source: value.layout.switch_source,
                auto_detected: matches!(
                    value.layout.switch_source,
                    LayoutSwitchSource::AutoDetected | LayoutSwitchSource::AutoFallback
                )
                .then_some(value.layout.auto_detected),
                keys: None,
                delay_ms: value.layout.delay_ms,
            },
            delays: value.delays.clone(),
            features: value.features.clone(),
        }
    }
}

impl TryFrom<AppConfigFile> for AppConfig {
    type Error = ConfigError;

    fn try_from(value: AppConfigFile) -> Result<Self, Self::Error> {
        if value.layout.keys.is_some() && value.layout.switch_combo.is_none() {
            return Err(ConfigError::LegacyFormatUnsupported);
        }

        let switch_combo = value
            .layout
            .switch_combo
            .ok_or(ConfigError::MissingRequiredField {
                field: "layout.switch_combo",
            })?;

        Ok(Self {
            layout: LayoutConfig {
                switch_combo,
                switch_source: value.layout.switch_source,
                auto_detected: value.layout.auto_detected.unwrap_or_default(),
                delay_ms: value.layout.delay_ms,
            },
            delays: value.delays,
            features: value.features,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LayoutSwitchSource;

    #[test]
    fn rejects_legacy_layout_keys_format() {
        let legacy = AppConfigFile {
            layout: LayoutConfigFile {
                switch_combo: None,
                switch_source: LayoutSwitchSource::Unknown,
                auto_detected: None,
                keys: Some(vec![]),
                delay_ms: 30,
            },
            delays: DelaysConfig {
                backspace_ms: 0,
                typing_ms: 0,
            },
            features: FeaturesConfig {
                auto_switch_enabled: true,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                undo_key: UndoKey::Pause,
                selected_text_switch_hotkey: SelectedTextHotkey::default(),
            },
        };

        let error = AppConfig::try_from(legacy).unwrap_err();
        assert!(matches!(error, ConfigError::LegacyFormatUnsupported));
    }

    #[test]
    fn rejects_missing_switch_combo_in_current_format() {
        let file = AppConfigFile {
            layout: LayoutConfigFile {
                switch_combo: None,
                switch_source: LayoutSwitchSource::Unknown,
                auto_detected: None,
                keys: None,
                delay_ms: 30,
            },
            delays: DelaysConfig {
                backspace_ms: 0,
                typing_ms: 0,
            },
            features: FeaturesConfig {
                auto_switch_enabled: true,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                undo_key: UndoKey::Pause,
                selected_text_switch_hotkey: SelectedTextHotkey::default(),
            },
        };

        let error = AppConfig::try_from(file).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::MissingRequiredField {
                field: "layout.switch_combo"
            }
        ));
    }

    #[test]
    fn missing_new_feature_flags_in_config_default_to_supported_values() {
        let parsed: AppConfigFile = toml::from_str(
            r#"
[layout]
switch_combo = "CtrlShift"
switch_source = "Unknown"
delay_ms = 30

[delays]
backspace_ms = 0
typing_ms = 0

[features]
undo_key = "Pause"
selected_text_switch_hotkey = "ShiftPause"
"#,
        )
        .unwrap();

        let config = AppConfig::try_from(parsed).unwrap();
        assert!(config.features.auto_switch_enabled);
        assert!(!config.features.fix_two_capitals);
        assert!(!config.features.fix_accidental_caps_lock);
    }
}
