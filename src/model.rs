use crate::error::{UndoKeyParseError, ValidationError};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use zvariant::Type;

pub const LAYOUT_DELAY_MIN_MS: u32 = 0;
pub const LAYOUT_DELAY_MAX_MS: u32 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s")]
pub enum UndoKey {
    #[serde(rename = "Pause")]
    Pause,
    #[serde(rename = "F12")]
    F12,
    #[serde(rename = "ScrollLock")]
    ScrollLock,
}

impl UndoKey {
    pub const ALL: [UndoKey; 3] = [UndoKey::Pause, UndoKey::F12, UndoKey::ScrollLock];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "Pause",
            Self::F12 => "F12",
            Self::ScrollLock => "ScrollLock",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutSwitchKey {
    #[serde(rename = "LeftControl")]
    LeftControl,
    #[serde(rename = "LeftShift")]
    LeftShift,
    #[serde(rename = "LeftAlt")]
    LeftAlt,
    #[serde(rename = "CapsLock")]
    CapsLock,
}

impl Default for UndoKey {
    fn default() -> Self {
        Self::Pause
    }
}

impl std::fmt::Display for UndoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for UndoKey {
    type Err = UndoKeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Pause" => Ok(Self::Pause),
            "F12" => Ok(Self::F12),
            "ScrollLock" => Ok(Self::ScrollLock),
            _ => Err(UndoKeyParseError::UnsupportedValue {
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub layout_delay_ms: u32,
    pub undo_key: UndoKey,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout_delay_ms: 30,
            undo_key: UndoKey::default(),
        }
    }
}

impl Settings {
    pub fn validate(self) -> Result<Self, ValidationError> {
        if !(LAYOUT_DELAY_MIN_MS..=LAYOUT_DELAY_MAX_MS).contains(&self.layout_delay_ms) {
            return Err(ValidationError::LayoutDelayOutOfRange {
                min: LAYOUT_DELAY_MIN_MS,
                max: LAYOUT_DELAY_MAX_MS,
                found: self.layout_delay_ms,
            });
        }

        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SettingsDto {
    pub layout_delay_ms: u32,
    pub undo_key: UndoKey,
}

impl Default for SettingsDto {
    fn default() -> Self {
        Self::from(Settings::default())
    }
}

impl From<Settings> for SettingsDto {
    fn from(value: Settings) -> Self {
        Self {
            layout_delay_ms: value.layout_delay_ms,
            undo_key: value.undo_key,
        }
    }
}

impl TryFrom<SettingsDto> for Settings {
    type Error = ValidationError;

    fn try_from(value: SettingsDto) -> Result<Self, Self::Error> {
        Settings {
            layout_delay_ms: value.layout_delay_ms,
            undo_key: value.undo_key,
        }
        .validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UpdateSettingsResult {
    pub message: String,
    pub restart_required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_settings_range() {
        let result = Settings {
            layout_delay_ms: 700,
            undo_key: UndoKey::Pause,
        }
        .validate();

        assert_eq!(
            result.unwrap_err(),
            ValidationError::LayoutDelayOutOfRange {
                min: LAYOUT_DELAY_MIN_MS,
                max: LAYOUT_DELAY_MAX_MS,
                found: 700,
            }
        );
    }

    #[test]
    fn converts_dto_into_domain_settings() {
        let dto = SettingsDto {
            layout_delay_ms: 30,
            undo_key: UndoKey::F12,
        };

        let settings = Settings::try_from(dto).unwrap();
        assert_eq!(settings.undo_key, UndoKey::F12);
    }

    #[test]
    fn rejects_unknown_undo_key() {
        let result = UndoKey::from_str("Unknown");

        assert_eq!(
            result.unwrap_err(),
            UndoKeyParseError::UnsupportedValue {
                value: "Unknown".to_string(),
            }
        );
    }
}
