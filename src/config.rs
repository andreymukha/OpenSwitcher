use crate::error::ConfigError;
use crate::error::ValidationError;
use crate::model::{
    default_manual_correction_hotkey, default_selected_text_hotkey,
    estimated_correction_schedule_ms, AutoDetectedLayoutSwitch, HotkeySpec, LayoutSwitchCombo,
    LayoutSwitchSetting, LayoutSwitchSource, Settings, INPUT_KEY_DELAY_MAX_MS,
    MAX_CORRECTION_EXTRA_BACKSPACES, MAX_CORRECTION_KEYSTROKES, MAX_CORRECTION_SCHEDULE_MS,
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
    #[serde(rename = "undo_key", default = "default_manual_correction_hotkey")]
    pub manual_correction_hotkey: HotkeySpec,
    #[serde(default = "default_selected_text_hotkey")]
    pub selected_text_switch_hotkey: HotkeySpec,
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
                manual_correction_hotkey: default_manual_correction_hotkey(),
                selected_text_switch_hotkey: default_selected_text_hotkey(),
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
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(&AppConfigFile::from(self))?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.settings().validate()?;

        for (field, found) in [
            ("backspace_ms", self.delays.backspace_ms),
            ("typing_ms", self.delays.typing_ms),
        ] {
            if found > INPUT_KEY_DELAY_MAX_MS {
                return Err(ValidationError::InputDelayOutOfRange {
                    field,
                    max: INPUT_KEY_DELAY_MAX_MS,
                    found,
                });
            }
        }

        let found_ms = estimated_correction_schedule_ms(
            MAX_CORRECTION_KEYSTROKES,
            MAX_CORRECTION_EXTRA_BACKSPACES,
            u64::from(self.layout.delay_ms),
            u64::from(self.delays.backspace_ms),
            u64::from(self.delays.typing_ms),
            true,
        )
        .unwrap_or(u64::MAX);
        if found_ms > MAX_CORRECTION_SCHEDULE_MS {
            return Err(ValidationError::InputCorrectionScheduleTooLong {
                max_ms: MAX_CORRECTION_SCHEDULE_MS,
                found_ms,
            });
        }

        Ok(())
    }

    pub fn settings(&self) -> Settings {
        Settings {
            auto_switch_enabled: self.features.auto_switch_enabled,
            fix_two_capitals: self.features.fix_two_capitals,
            fix_accidental_caps_lock: self.features.fix_accidental_caps_lock,
            layout_delay_ms: self.layout.delay_ms,
            manual_correction_hotkey: self.features.manual_correction_hotkey,
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
        self.features.manual_correction_hotkey = settings.manual_correction_hotkey;
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

        let config = Self {
            layout: LayoutConfig {
                switch_combo,
                switch_source: value.layout.switch_source,
                auto_detected: value.layout.auto_detected.unwrap_or_default(),
                delay_ms: value.layout.delay_ms,
            },
            delays: value.delays,
            features: value.features,
        };
        config.validate()?;
        Ok(config)
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
    use crate::error::ValidationError;
    use crate::model::{
        DesktopEnvironment, DetectionConfidence, DetectionStrategy, DistroKind, HotkeyModifiers,
        HotkeySpec, HotkeyTrigger, SelectedTextHotkey, SessionType, SystemContext, UndoKey,
        INPUT_KEY_DELAY_MAX_MS, MAX_CORRECTION_SCHEDULE_MS,
    };
    use tempfile::TempDir;

    // Test helpers

    fn cinnamon_x11_detection() -> AutoDetectedLayoutSwitch {
        AutoDetectedLayoutSwitch {
            strategy: DetectionStrategy::CinnamonX11GSettingsXkbOptions,
            confidence: DetectionConfidence::High,
            context: SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Cinnamon,
                distro: DistroKind::LinuxMint,
            },
        }
    }

    fn xfce_x11_detection() -> AutoDetectedLayoutSwitch {
        AutoDetectedLayoutSwitch {
            strategy: DetectionStrategy::XfceX11XfconfKeyboardLayout,
            confidence: DetectionConfidence::Low,
            context: SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Xfce,
                distro: DistroKind::Debian,
            },
        }
    }

    fn non_default_config(source: LayoutSwitchSource) -> AppConfig {
        AppConfig {
            layout: LayoutConfig {
                switch_combo: LayoutSwitchCombo::right_ctrl_right_shift(),
                switch_source: source,
                auto_detected: cinnamon_x11_detection(),
                delay_ms: 123,
            },
            delays: DelaysConfig {
                backspace_ms: 4,
                typing_ms: 5,
            },
            features: FeaturesConfig {
                auto_switch_enabled: false,
                fix_two_capitals: true,
                fix_accidental_caps_lock: true,
                manual_correction_hotkey: HotkeySpec::from(UndoKey::ScrollLock),
                selected_text_switch_hotkey: HotkeySpec::from(SelectedTextHotkey::CtrlF12),
            },
        }
    }

    // Legacy config rejection

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
                manual_correction_hotkey: default_manual_correction_hotkey(),
                selected_text_switch_hotkey: default_selected_text_hotkey(),
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
                manual_correction_hotkey: default_manual_correction_hotkey(),
                selected_text_switch_hotkey: default_selected_text_hotkey(),
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

    // Defaults / missing fields

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

    #[test]
    fn old_hotkey_values_load_as_hotkey_specs() {
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
undo_key = "ScrollLock"
selected_text_switch_hotkey = "CtrlF12"
"#,
        )
        .unwrap();

        let config = AppConfig::try_from(parsed).unwrap();
        assert_eq!(
            config.features.manual_correction_hotkey,
            HotkeySpec::from(UndoKey::ScrollLock)
        );
        assert_eq!(
            config.features.selected_text_switch_hotkey,
            HotkeySpec::from(SelectedTextHotkey::CtrlF12)
        );
    }

    #[test]
    fn new_hotkey_values_save_load_roundtrip_as_canonical_strings() {
        let mut config = non_default_config(LayoutSwitchSource::Manual);
        config.layout.auto_detected = AutoDetectedLayoutSwitch::default();
        config.features.manual_correction_hotkey =
            HotkeySpec::new(HotkeyModifiers::ctrl_alt(), HotkeyTrigger::F12);
        config.features.selected_text_switch_hotkey =
            HotkeySpec::new(HotkeyModifiers::shift_ctrl_alt(), HotkeyTrigger::Insert);
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");

        config.save_to_path(&path).unwrap();
        let serialized = std::fs::read_to_string(&path).unwrap();
        assert!(serialized.contains("undo_key = \"ctrl+alt+f12\""));
        assert!(serialized.contains("selected_text_switch_hotkey = \"shift+ctrl+alt+insert\""));

        let loaded = AppConfig::load_or_create(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn duplicate_hotkeys_in_config_are_rejected() {
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
undo_key = "ctrl+alt+f12"
selected_text_switch_hotkey = "ctrl+alt+f12"
"#,
        )
        .unwrap();

        let error = AppConfig::try_from(parsed).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Validation(ValidationError::DuplicateHotkey { .. })
        ));
    }

    #[test]
    fn maximum_individual_input_delays_within_total_budget_are_accepted() {
        for (backspace_ms, typing_ms) in [(INPUT_KEY_DELAY_MAX_MS, 0), (0, INPUT_KEY_DELAY_MAX_MS)]
        {
            let mut config = non_default_config(LayoutSwitchSource::Manual);
            config.layout.delay_ms = crate::model::LAYOUT_DELAY_MAX_MS;
            config.delays = DelaysConfig {
                backspace_ms,
                typing_ms,
            };

            config.validate().expect(
                "a maximum individual delay should remain valid when total work is bounded",
            );
        }
    }

    #[test]
    fn input_delay_above_per_key_limit_is_rejected_before_runtime_snapshot() {
        let mut config = non_default_config(LayoutSwitchSource::Manual);
        config.delays.backspace_ms = INPUT_KEY_DELAY_MAX_MS + 1;

        let error = config.validate().unwrap_err();

        assert!(matches!(
            error,
            ValidationError::InputDelayOutOfRange {
                field: "backspace_ms",
                max: INPUT_KEY_DELAY_MAX_MS,
                found,
            } if found == INPUT_KEY_DELAY_MAX_MS + 1
        ));
    }

    #[test]
    fn excessive_worst_case_correction_schedule_is_rejected() {
        let mut config = non_default_config(LayoutSwitchSource::Manual);
        config.layout.delay_ms = crate::model::LAYOUT_DELAY_MAX_MS;
        config.delays = DelaysConfig {
            backspace_ms: INPUT_KEY_DELAY_MAX_MS,
            typing_ms: INPUT_KEY_DELAY_MAX_MS,
        };

        let error = config.validate().unwrap_err();

        assert!(matches!(
            error,
            ValidationError::InputCorrectionScheduleTooLong {
                max_ms: MAX_CORRECTION_SCHEDULE_MS,
                found_ms,
            } if found_ms > MAX_CORRECTION_SCHEDULE_MS
        ));
    }

    #[test]
    fn loading_toml_rejects_unbounded_input_delay_before_backend_startup() {
        let parsed: AppConfigFile = toml::from_str(&format!(
            r#"
[layout]
switch_combo = "CtrlShift"
switch_source = "Manual"
delay_ms = 30

[delays]
backspace_ms = {}
typing_ms = 0

[features]
undo_key = "Pause"
selected_text_switch_hotkey = "ShiftPause"
"#,
            INPUT_KEY_DELAY_MAX_MS + 1
        ))
        .unwrap();

        let error = AppConfig::try_from(parsed).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Validation(ValidationError::InputDelayOutOfRange { .. })
        ));
    }

    #[test]
    fn saving_public_config_fields_cannot_bypass_input_delay_validation() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let mut config = non_default_config(LayoutSwitchSource::Manual);
        config.delays.typing_ms = INPUT_KEY_DELAY_MAX_MS + 1;

        let error = config.save_to_path(&path).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Validation(ValidationError::InputDelayOutOfRange {
                field: "typing_ms",
                ..
            })
        ));
        assert!(!path.exists());
    }

    #[test]
    fn loading_toml_rejects_aggregate_correction_schedule_above_limit() {
        let parsed: AppConfigFile = toml::from_str(
            r#"
[layout]
switch_combo = "CtrlShift"
switch_source = "Manual"
delay_ms = 500

[delays]
backspace_ms = 10
typing_ms = 10

[features]
undo_key = "Pause"
selected_text_switch_hotkey = "ShiftPause"
"#,
        )
        .unwrap();

        let error = AppConfig::try_from(parsed).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Validation(ValidationError::InputCorrectionScheduleTooLong { .. })
        ));
    }

    #[test]
    fn saving_config_rejects_aggregate_correction_schedule_before_writing() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nested").join("config.toml");
        let mut config = non_default_config(LayoutSwitchSource::Manual);
        config.layout.delay_ms = crate::model::LAYOUT_DELAY_MAX_MS;
        config.delays = DelaysConfig {
            backspace_ms: INPUT_KEY_DELAY_MAX_MS,
            typing_ms: INPUT_KEY_DELAY_MAX_MS,
        };

        let error = config.save_to_path(&path).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Validation(ValidationError::InputCorrectionScheduleTooLong { .. })
        ));
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    // Settings mapping

    #[test]
    fn app_config_settings_and_apply_settings_map_all_boundary_fields() {
        let mut config = non_default_config(LayoutSwitchSource::AutoDetected);
        let settings = config.settings();

        assert!(!settings.auto_switch_enabled);
        assert!(settings.fix_two_capitals);
        assert!(settings.fix_accidental_caps_lock);
        assert_eq!(settings.layout_delay_ms, 123);
        assert_eq!(
            settings.manual_correction_hotkey,
            HotkeySpec::from(UndoKey::ScrollLock)
        );
        assert_eq!(
            settings.selected_text_hotkey,
            HotkeySpec::from(SelectedTextHotkey::CtrlF12)
        );
        assert_eq!(
            settings.layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::right_ctrl_right_shift(),
                source: LayoutSwitchSource::AutoDetected,
                auto_detected: cinnamon_x11_detection(),
            }
        );

        let updated = Settings {
            auto_switch_enabled: true,
            fix_two_capitals: false,
            fix_accidental_caps_lock: false,
            layout_delay_ms: 77,
            manual_correction_hotkey: HotkeySpec::from(UndoKey::F12),
            selected_text_hotkey: HotkeySpec::from(SelectedTextHotkey::AltScrollLock),
            layout_switch: LayoutSwitchSetting {
                combo: LayoutSwitchCombo::left_alt_left_shift(),
                source: LayoutSwitchSource::AutoFallback,
                auto_detected: xfce_x11_detection(),
            },
        };

        config.apply_settings(updated);

        assert!(config.features.auto_switch_enabled);
        assert!(!config.features.fix_two_capitals);
        assert!(!config.features.fix_accidental_caps_lock);
        assert_eq!(config.layout.delay_ms, 77);
        assert_eq!(
            config.features.manual_correction_hotkey,
            HotkeySpec::from(UndoKey::F12)
        );
        assert_eq!(
            config.features.selected_text_switch_hotkey,
            HotkeySpec::from(SelectedTextHotkey::AltScrollLock)
        );
        assert_eq!(
            config.layout.switch_combo,
            LayoutSwitchCombo::left_alt_left_shift()
        );
        assert_eq!(
            config.layout.switch_source,
            LayoutSwitchSource::AutoFallback
        );
        assert_eq!(config.layout.auto_detected, xfce_x11_detection());
        assert_eq!(config.settings(), updated);
    }

    // TOML roundtrip / serialization shape

    #[test]
    fn save_to_path_load_or_create_toml_roundtrip_preserves_boundary_fields() {
        let config = non_default_config(LayoutSwitchSource::AutoDetected);
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");

        config.save_to_path(&path).unwrap();
        let loaded = AppConfig::load_or_create(&path).unwrap();

        assert_eq!(loaded, config);
        assert_eq!(loaded.settings(), config.settings());
        assert_eq!(
            loaded.features.selected_text_switch_hotkey,
            HotkeySpec::from(SelectedTextHotkey::CtrlF12)
        );
        assert_eq!(
            loaded.layout.switch_combo,
            LayoutSwitchCombo::right_ctrl_right_shift()
        );
        assert_eq!(
            loaded.layout.switch_source,
            LayoutSwitchSource::AutoDetected
        );
        assert_eq!(loaded.layout.auto_detected, cinnamon_x11_detection());
        assert_eq!(loaded.layout.delay_ms, 123);
        assert_eq!(loaded.delays.backspace_ms, 4);
        assert_eq!(loaded.delays.typing_ms, 5);
    }

    #[test]
    fn auto_detected_serialization_depends_on_layout_switch_source() {
        let manual = non_default_config(LayoutSwitchSource::Manual);
        let manual_toml = toml::to_string_pretty(&AppConfigFile::from(&manual)).unwrap();
        assert!(!manual_toml.contains("auto_detected"));
        let parsed_manual: AppConfigFile = toml::from_str(&manual_toml).unwrap();
        assert!(parsed_manual.layout.auto_detected.is_none());
        assert_eq!(
            AppConfig::try_from(parsed_manual)
                .unwrap()
                .layout
                .auto_detected,
            AutoDetectedLayoutSwitch::default()
        );

        for source in [
            LayoutSwitchSource::AutoDetected,
            LayoutSwitchSource::AutoFallback,
        ] {
            let config = non_default_config(source);
            let serialized = toml::to_string_pretty(&AppConfigFile::from(&config)).unwrap();
            assert!(serialized.contains("auto_detected"));
            let parsed: AppConfigFile = toml::from_str(&serialized).unwrap();
            assert_eq!(parsed.layout.auto_detected, Some(cinnamon_x11_detection()));
            assert_eq!(
                AppConfig::try_from(parsed).unwrap().layout.auto_detected,
                cinnamon_x11_detection()
            );
        }
    }
}
