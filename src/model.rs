use crate::error::{LayoutSwitchComboParseError, UndoKeyParseError, ValidationError};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use zvariant::{Signature, Type};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayoutModifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl LayoutModifier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
            Self::Super => "Super",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayoutTriggerKey {
    Space,
    CapsLock,
}

impl LayoutTriggerKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Space => "Space",
            Self::CapsLock => "CapsLock",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayoutSwitchCombo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    pub key: Option<LayoutTriggerKey>,
}

impl LayoutSwitchCombo {
    pub const COMMON_CHOICES: [LayoutSwitchCombo; 5] = [
        LayoutSwitchCombo::ctrl_shift(),
        LayoutSwitchCombo::alt_shift(),
        LayoutSwitchCombo::caps_lock(),
        LayoutSwitchCombo::ctrl_space(),
        LayoutSwitchCombo::super_space(),
    ];

    pub const fn ctrl_shift() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: true,
            super_key: false,
            key: None,
        }
    }

    pub const fn alt_shift() -> Self {
        Self {
            ctrl: false,
            alt: true,
            shift: true,
            super_key: false,
            key: None,
        }
    }

    pub const fn caps_lock() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: false,
            key: Some(LayoutTriggerKey::CapsLock),
        }
    }

    pub const fn ctrl_space() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: false,
            super_key: false,
            key: Some(LayoutTriggerKey::Space),
        }
    }

    pub const fn super_space() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: true,
            key: Some(LayoutTriggerKey::Space),
        }
    }

    pub const fn modifiers_count(self) -> usize {
        self.ctrl as usize + self.alt as usize + self.shift as usize + self.super_key as usize
    }

    pub fn modifiers(self) -> impl Iterator<Item = LayoutModifier> {
        [
            self.ctrl.then_some(LayoutModifier::Ctrl),
            self.alt.then_some(LayoutModifier::Alt),
            self.shift.then_some(LayoutModifier::Shift),
            self.super_key.then_some(LayoutModifier::Super),
        ]
        .into_iter()
        .flatten()
    }

    pub fn from_parts(
        ctrl: bool,
        alt: bool,
        shift: bool,
        super_key: bool,
        key: Option<LayoutTriggerKey>,
    ) -> Result<Self, LayoutSwitchComboParseError> {
        let combo = Self {
            ctrl,
            alt,
            shift,
            super_key,
            key,
        };

        if combo.is_valid() {
            Ok(combo)
        } else {
            Err(LayoutSwitchComboParseError::UnsupportedValue {
                value: combo.to_string(),
            })
        }
    }

    pub fn is_valid(self) -> bool {
        if self.key.is_some() {
            return self.ctrl
                || self.alt
                || self.shift
                || self.super_key
                || self.key == Some(LayoutTriggerKey::CapsLock);
        }

        self.modifiers_count() >= 2
    }

    pub fn config_value(self) -> String {
        self.to_string()
    }

    pub fn short_label(self) -> String {
        self.to_string()
    }
}

impl Default for UndoKey {
    fn default() -> Self {
        Self::Pause
    }
}

impl Default for LayoutSwitchCombo {
    fn default() -> Self {
        Self::ctrl_shift()
    }
}

impl std::fmt::Display for UndoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for LayoutSwitchCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<&'static str> =
            self.modifiers().map(|modifier| modifier.as_str()).collect();
        if let Some(key) = self.key {
            parts.push(key.as_str());
        }
        f.write_str(&parts.join("+"))
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

impl FromStr for LayoutSwitchCombo {
    type Err = LayoutSwitchComboParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "CtrlShift" => return Ok(Self::ctrl_shift()),
            "AltShift" => return Ok(Self::alt_shift()),
            "CapsLock" => return Ok(Self::caps_lock()),
            "CtrlSpace" => return Ok(Self::ctrl_space()),
            "SuperSpace" => return Ok(Self::super_space()),
            _ => {}
        }

        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut super_key = false;
        let mut key = None;

        for part in value
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            match part {
                "Ctrl" => ctrl = true,
                "Alt" => alt = true,
                "Shift" => shift = true,
                "Super" => super_key = true,
                "Space" => key = Some(LayoutTriggerKey::Space),
                "CapsLock" => key = Some(LayoutTriggerKey::CapsLock),
                _ => {
                    return Err(LayoutSwitchComboParseError::UnsupportedValue {
                        value: value.to_string(),
                    })
                }
            }
        }

        Self::from_parts(ctrl, alt, shift, super_key, key).map_err(|_| {
            LayoutSwitchComboParseError::UnsupportedValue {
                value: value.to_string(),
            }
        })
    }
}

impl Serialize for LayoutSwitchCombo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.config_value())
    }
}

impl<'de> Deserialize<'de> for LayoutSwitchCombo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(D::Error::custom)
    }
}

impl Type for LayoutSwitchCombo {
    fn signature() -> Signature<'static> {
        String::signature()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[zvariant(signature = "s")]
pub enum SessionType {
    #[serde(rename = "x11")]
    X11,
    #[serde(rename = "wayland")]
    Wayland,
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[zvariant(signature = "s")]
pub enum DesktopEnvironment {
    #[serde(rename = "cinnamon")]
    Cinnamon,
    #[serde(rename = "gnome")]
    Gnome,
    #[serde(rename = "xfce")]
    Xfce,
    #[serde(rename = "kde")]
    Kde,
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[zvariant(signature = "s")]
pub enum DistroKind {
    #[serde(rename = "linux-mint")]
    LinuxMint,
    #[serde(rename = "ubuntu")]
    Ubuntu,
    #[serde(rename = "debian")]
    Debian,
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
pub struct SystemContext {
    pub session_type: SessionType,
    pub desktop_environment: DesktopEnvironment,
    pub distro: DistroKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[zvariant(signature = "s")]
pub enum LayoutSwitchSource {
    #[default]
    #[serde(rename = "Unknown")]
    Unknown,
    #[serde(rename = "Manual")]
    Manual,
    #[serde(rename = "AutoDetected")]
    AutoDetected,
    #[serde(rename = "AutoFallback")]
    AutoFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s")]
pub enum DetectionStrategy {
    #[serde(rename = "NoSupportedStrategy")]
    NoSupportedStrategy,
    #[serde(rename = "XfceX11XfconfKeyboardLayout")]
    XfceX11XfconfKeyboardLayout,
    #[serde(rename = "XfceX11SetXkbmapQuery")]
    XfceX11SetXkbmapQuery,
    #[serde(rename = "CinnamonX11GSettingsXkbOptions")]
    CinnamonX11GSettingsXkbOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s")]
pub enum DetectionConfidence {
    #[serde(rename = "Unsupported")]
    Unsupported,
    #[serde(rename = "Low")]
    Low,
    #[serde(rename = "High")]
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AutoDetectedLayoutSwitch {
    pub strategy: DetectionStrategy,
    pub confidence: DetectionConfidence,
    pub context: SystemContext,
}

impl Default for AutoDetectedLayoutSwitch {
    fn default() -> Self {
        Self {
            strategy: DetectionStrategy::NoSupportedStrategy,
            confidence: DetectionConfidence::Unsupported,
            context: SystemContext::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LayoutSwitchSetting {
    pub combo: LayoutSwitchCombo,
    pub source: LayoutSwitchSource,
    pub auto_detected: AutoDetectedLayoutSwitch,
}

impl Default for LayoutSwitchSetting {
    fn default() -> Self {
        Self {
            combo: LayoutSwitchCombo::default(),
            source: LayoutSwitchSource::Unknown,
            auto_detected: AutoDetectedLayoutSwitch::default(),
        }
    }
}

impl LayoutSwitchSetting {
    pub fn is_locked_by_auto_detection(self) -> bool {
        self.source == LayoutSwitchSource::AutoDetected
            && self.auto_detected.confidence == DetectionConfidence::High
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub layout_delay_ms: u32,
    pub undo_key: UndoKey,
    pub layout_switch: LayoutSwitchSetting,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout_delay_ms: 30,
            undo_key: UndoKey::default(),
            layout_switch: LayoutSwitchSetting::default(),
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
    pub layout_switch: LayoutSwitchSetting,
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
            layout_switch: value.layout_switch,
        }
    }
}

impl TryFrom<SettingsDto> for Settings {
    type Error = ValidationError;

    fn try_from(value: SettingsDto) -> Result<Self, Self::Error> {
        Settings {
            layout_delay_ms: value.layout_delay_ms,
            undo_key: value.undo_key,
            layout_switch: value.layout_switch,
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
            layout_switch: LayoutSwitchSetting::default(),
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
            layout_switch: LayoutSwitchSetting {
                combo: LayoutSwitchCombo::alt_shift(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            },
        };

        let settings = Settings::try_from(dto).unwrap();
        assert_eq!(settings.undo_key, UndoKey::F12);
        assert_eq!(settings.layout_switch.combo, LayoutSwitchCombo::alt_shift());
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

    #[test]
    fn parses_and_formats_supported_layout_switch_combos() {
        let combos = [
            ("Ctrl+Shift", LayoutSwitchCombo::ctrl_shift()),
            ("Alt+Shift", LayoutSwitchCombo::alt_shift()),
            ("CapsLock", LayoutSwitchCombo::caps_lock()),
            ("Ctrl+Space", LayoutSwitchCombo::ctrl_space()),
            ("Super+Space", LayoutSwitchCombo::super_space()),
        ];

        for (raw, expected) in combos {
            assert_eq!(LayoutSwitchCombo::from_str(raw).unwrap(), expected);
            assert_eq!(expected.to_string(), raw);
        }
    }

    #[test]
    fn rejects_unsupported_layout_switch_combo() {
        let error = LayoutSwitchCombo::from_str("Space").unwrap_err();
        assert_eq!(
            error,
            LayoutSwitchComboParseError::UnsupportedValue {
                value: "Space".to_string(),
            }
        );
    }
}
