use crate::error::{
    LayoutSwitchComboParseError, SelectedTextHotkeyParseError, UndoKeyParseError, ValidationError,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use zvariant::Type;

pub const LAYOUT_DELAY_MIN_MS: u32 = 0;
pub const LAYOUT_DELAY_MAX_MS: u32 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s")]
#[derive(Default)]
pub enum UndoKey {
    #[serde(rename = "Pause")]
    #[default]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zvariant(signature = "s")]
#[derive(Default)]
pub enum SelectedTextHotkey {
    #[serde(rename = "ShiftPause")]
    #[default]
    ShiftPause,
    #[serde(rename = "CtrlPause")]
    CtrlPause,
    #[serde(rename = "AltPause")]
    AltPause,
    #[serde(rename = "ShiftF12")]
    ShiftF12,
    #[serde(rename = "CtrlF12")]
    CtrlF12,
    #[serde(rename = "AltF12")]
    AltF12,
    #[serde(rename = "ShiftScrollLock")]
    ShiftScrollLock,
    #[serde(rename = "CtrlScrollLock")]
    CtrlScrollLock,
    #[serde(rename = "AltScrollLock")]
    AltScrollLock,
}

impl SelectedTextHotkey {
    pub const ALL: [SelectedTextHotkey; 9] = [
        SelectedTextHotkey::ShiftPause,
        SelectedTextHotkey::CtrlPause,
        SelectedTextHotkey::AltPause,
        SelectedTextHotkey::ShiftF12,
        SelectedTextHotkey::CtrlF12,
        SelectedTextHotkey::AltF12,
        SelectedTextHotkey::ShiftScrollLock,
        SelectedTextHotkey::CtrlScrollLock,
        SelectedTextHotkey::AltScrollLock,
    ];

    pub const fn trigger_key(self) -> UndoKey {
        match self {
            Self::ShiftPause | Self::CtrlPause | Self::AltPause => UndoKey::Pause,
            Self::ShiftF12 | Self::CtrlF12 | Self::AltF12 => UndoKey::F12,
            Self::ShiftScrollLock | Self::CtrlScrollLock | Self::AltScrollLock => {
                UndoKey::ScrollLock
            }
        }
    }

    pub const fn uses_shift(self) -> bool {
        matches!(
            self,
            Self::ShiftPause | Self::ShiftF12 | Self::ShiftScrollLock
        )
    }

    pub const fn uses_ctrl(self) -> bool {
        matches!(self, Self::CtrlPause | Self::CtrlF12 | Self::CtrlScrollLock)
    }

    pub const fn uses_alt(self) -> bool {
        matches!(self, Self::AltPause | Self::AltF12 | Self::AltScrollLock)
    }

    pub const fn config_value(self) -> &'static str {
        match self {
            Self::ShiftPause => "ShiftPause",
            Self::CtrlPause => "CtrlPause",
            Self::AltPause => "AltPause",
            Self::ShiftF12 => "ShiftF12",
            Self::CtrlF12 => "CtrlF12",
            Self::AltF12 => "AltF12",
            Self::ShiftScrollLock => "ShiftScrollLock",
            Self::CtrlScrollLock => "CtrlScrollLock",
            Self::AltScrollLock => "AltScrollLock",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::ShiftPause => "Shift+Pause",
            Self::CtrlPause => "Ctrl+Pause",
            Self::AltPause => "Alt+Pause",
            Self::ShiftF12 => "Shift+F12",
            Self::CtrlF12 => "Ctrl+F12",
            Self::AltF12 => "Alt+F12",
            Self::ShiftScrollLock => "Shift+ScrollLock",
            Self::CtrlScrollLock => "Ctrl+ScrollLock",
            Self::AltScrollLock => "Alt+ScrollLock",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[zvariant(signature = "s")]
pub enum LayoutSwitchCombo {
    #[serde(rename = "CtrlShift")]
    CtrlShift,
    #[serde(rename = "AltShift")]
    AltShift,
    #[serde(rename = "CapsLock")]
    CapsLock,
    #[serde(rename = "CtrlSpace")]
    CtrlSpace,
    #[serde(rename = "SuperSpace")]
    SuperSpace,
    #[serde(rename = "LeftCtrlLeftShift")]
    LeftCtrlLeftShift,
    #[serde(rename = "RightCtrlRightShift")]
    RightCtrlRightShift,
    #[serde(rename = "LeftAltLeftShift")]
    LeftAltLeftShift,
    #[serde(rename = "RightAltRightShift")]
    RightAltRightShift,
}

impl LayoutSwitchCombo {
    pub const WHITELIST: [LayoutSwitchCombo; 9] = [
        LayoutSwitchCombo::ctrl_shift(),
        LayoutSwitchCombo::alt_shift(),
        LayoutSwitchCombo::caps_lock(),
        LayoutSwitchCombo::ctrl_space(),
        LayoutSwitchCombo::super_space(),
        LayoutSwitchCombo::left_ctrl_left_shift(),
        LayoutSwitchCombo::right_ctrl_right_shift(),
        LayoutSwitchCombo::left_alt_left_shift(),
        LayoutSwitchCombo::right_alt_right_shift(),
    ];

    pub const fn ctrl_shift() -> Self {
        Self::CtrlShift
    }

    pub const fn alt_shift() -> Self {
        Self::AltShift
    }

    pub const fn caps_lock() -> Self {
        Self::CapsLock
    }

    pub const fn ctrl_space() -> Self {
        Self::CtrlSpace
    }

    pub const fn super_space() -> Self {
        Self::SuperSpace
    }

    pub const fn left_ctrl_left_shift() -> Self {
        Self::LeftCtrlLeftShift
    }

    pub const fn right_ctrl_right_shift() -> Self {
        Self::RightCtrlRightShift
    }

    pub const fn left_alt_left_shift() -> Self {
        Self::LeftAltLeftShift
    }

    pub const fn right_alt_right_shift() -> Self {
        Self::RightAltRightShift
    }

    pub const fn config_value(self) -> &'static str {
        match self {
            Self::CtrlShift => "CtrlShift",
            Self::AltShift => "AltShift",
            Self::CapsLock => "CapsLock",
            Self::CtrlSpace => "CtrlSpace",
            Self::SuperSpace => "SuperSpace",
            Self::LeftCtrlLeftShift => "LeftCtrlLeftShift",
            Self::RightCtrlRightShift => "RightCtrlRightShift",
            Self::LeftAltLeftShift => "LeftAltLeftShift",
            Self::RightAltRightShift => "RightAltRightShift",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::CtrlShift => "Ctrl+Shift",
            Self::AltShift => "Alt+Shift",
            Self::CapsLock => "CapsLock",
            Self::CtrlSpace => "Ctrl+Space",
            Self::SuperSpace => "Super+Space",
            Self::LeftCtrlLeftShift => "Left Ctrl+Left Shift",
            Self::RightCtrlRightShift => "Right Ctrl+Right Shift",
            Self::LeftAltLeftShift => "Left Alt+Left Shift",
            Self::RightAltRightShift => "Right Alt+Right Shift",
        }
    }

    pub const fn xkb_option(self) -> &'static str {
        match self {
            Self::CtrlShift => "grp:ctrl_shift_toggle",
            Self::AltShift => "grp:alt_shift_toggle",
            Self::CapsLock => "grp:caps_toggle",
            Self::CtrlSpace => "grp:ctrl_space_toggle",
            Self::SuperSpace => "grp:win_space_toggle",
            Self::LeftCtrlLeftShift => "grp:lctrl_lshift_toggle",
            Self::RightCtrlRightShift => "grp:rctrl_rshift_toggle",
            Self::LeftAltLeftShift => "grp:lalt_lshift_toggle",
            Self::RightAltRightShift => "grp:ralt_rshift_toggle",
        }
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

impl std::fmt::Display for SelectedTextHotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.short_label())
    }
}

impl std::fmt::Display for LayoutSwitchCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.short_label())
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

impl FromStr for SelectedTextHotkey {
    type Err = SelectedTextHotkeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ShiftPause" | "Shift+Pause" => Ok(Self::ShiftPause),
            "CtrlPause" | "Ctrl+Pause" => Ok(Self::CtrlPause),
            "AltPause" | "Alt+Pause" => Ok(Self::AltPause),
            "ShiftF12" | "Shift+F12" => Ok(Self::ShiftF12),
            "CtrlF12" | "Ctrl+F12" => Ok(Self::CtrlF12),
            "AltF12" | "Alt+F12" => Ok(Self::AltF12),
            "ShiftScrollLock" | "Shift+ScrollLock" => Ok(Self::ShiftScrollLock),
            "CtrlScrollLock" | "Ctrl+ScrollLock" => Ok(Self::CtrlScrollLock),
            "AltScrollLock" | "Alt+ScrollLock" => Ok(Self::AltScrollLock),
            _ => Err(SelectedTextHotkeyParseError::UnsupportedValue {
                value: value.to_string(),
            }),
        }
    }
}

impl FromStr for LayoutSwitchCombo {
    type Err = LayoutSwitchComboParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "CtrlShift" | "Ctrl+Shift" => Ok(Self::ctrl_shift()),
            "AltShift" | "Alt+Shift" => Ok(Self::alt_shift()),
            "CapsLock" => Ok(Self::caps_lock()),
            "CtrlSpace" | "Ctrl+Space" => Ok(Self::ctrl_space()),
            "SuperSpace" | "Super+Space" => Ok(Self::super_space()),
            "LeftCtrlLeftShift" | "Left Ctrl+Left Shift" => Ok(Self::left_ctrl_left_shift()),
            "RightCtrlRightShift" | "Right Ctrl+Right Shift" => Ok(Self::right_ctrl_right_shift()),
            "LeftAltLeftShift" | "Left Alt+Left Shift" => Ok(Self::left_alt_left_shift()),
            "RightAltRightShift" | "Right Alt+Right Shift" => Ok(Self::right_alt_right_shift()),
            _ => Err(LayoutSwitchComboParseError::UnsupportedValue {
                value: value.to_string(),
            }),
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[zvariant(signature = "s")]
pub enum LayoutSwitchCapturePhase {
    #[default]
    #[serde(rename = "Idle")]
    Idle,
    #[serde(rename = "Waiting")]
    Waiting,
    #[serde(rename = "Candidate")]
    Candidate,
    #[serde(rename = "Unsupported")]
    Unsupported,
    #[serde(rename = "Cancelled")]
    Cancelled,
    #[serde(rename = "Finished")]
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
pub struct LayoutSwitchCaptureState {
    pub phase: LayoutSwitchCapturePhase,
    pub candidate: LayoutSwitchCombo,
    pub has_candidate: bool,
    pub message: String,
}

impl LayoutSwitchCaptureState {
    pub fn idle() -> Self {
        Self::default()
    }

    pub fn waiting() -> Self {
        Self {
            phase: LayoutSwitchCapturePhase::Waiting,
            candidate: LayoutSwitchCombo::default(),
            has_candidate: false,
            message: String::new(),
        }
    }

    pub fn candidate(combo: LayoutSwitchCombo) -> Self {
        Self {
            phase: LayoutSwitchCapturePhase::Candidate,
            candidate: combo,
            has_candidate: true,
            message: String::new(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            phase: LayoutSwitchCapturePhase::Unsupported,
            candidate: LayoutSwitchCombo::default(),
            has_candidate: false,
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            phase: LayoutSwitchCapturePhase::Cancelled,
            candidate: LayoutSwitchCombo::default(),
            has_candidate: false,
            message: String::new(),
        }
    }

    pub fn finished() -> Self {
        Self {
            phase: LayoutSwitchCapturePhase::Finished,
            candidate: LayoutSwitchCombo::default(),
            has_candidate: false,
            message: String::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            LayoutSwitchCapturePhase::Waiting
                | LayoutSwitchCapturePhase::Candidate
                | LayoutSwitchCapturePhase::Unsupported
        )
    }
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
    pub selected_text_hotkey: SelectedTextHotkey,
    pub layout_switch: LayoutSwitchSetting,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout_delay_ms: 30,
            undo_key: UndoKey::default(),
            selected_text_hotkey: SelectedTextHotkey::default(),
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
    pub selected_text_hotkey: SelectedTextHotkey,
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
            selected_text_hotkey: value.selected_text_hotkey,
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
            selected_text_hotkey: value.selected_text_hotkey,
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
            selected_text_hotkey: SelectedTextHotkey::default(),
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
            selected_text_hotkey: SelectedTextHotkey::AltPause,
            layout_switch: LayoutSwitchSetting {
                combo: LayoutSwitchCombo::alt_shift(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            },
        };

        let settings = Settings::try_from(dto).unwrap();
        assert_eq!(settings.undo_key, UndoKey::F12);
        assert_eq!(settings.selected_text_hotkey, SelectedTextHotkey::AltPause);
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
    fn parses_and_formats_supported_selected_text_hotkeys() {
        let hotkeys = [
            ("Shift+Pause", SelectedTextHotkey::ShiftPause),
            ("Ctrl+Pause", SelectedTextHotkey::CtrlPause),
            ("Alt+Pause", SelectedTextHotkey::AltPause),
            ("Shift+F12", SelectedTextHotkey::ShiftF12),
            ("Ctrl+F12", SelectedTextHotkey::CtrlF12),
            ("Alt+F12", SelectedTextHotkey::AltF12),
            ("Shift+ScrollLock", SelectedTextHotkey::ShiftScrollLock),
            ("Ctrl+ScrollLock", SelectedTextHotkey::CtrlScrollLock),
            ("Alt+ScrollLock", SelectedTextHotkey::AltScrollLock),
        ];

        for (raw, expected) in hotkeys {
            assert_eq!(SelectedTextHotkey::from_str(raw).unwrap(), expected);
            assert_eq!(expected.to_string(), raw);
        }
    }

    #[test]
    fn parses_and_formats_supported_layout_switch_combos() {
        let combos = [
            ("Ctrl+Shift", LayoutSwitchCombo::ctrl_shift()),
            ("Alt+Shift", LayoutSwitchCombo::alt_shift()),
            ("CapsLock", LayoutSwitchCombo::caps_lock()),
            ("Ctrl+Space", LayoutSwitchCombo::ctrl_space()),
            ("Super+Space", LayoutSwitchCombo::super_space()),
            (
                "Left Ctrl+Left Shift",
                LayoutSwitchCombo::left_ctrl_left_shift(),
            ),
            (
                "Right Ctrl+Right Shift",
                LayoutSwitchCombo::right_ctrl_right_shift(),
            ),
            (
                "Left Alt+Left Shift",
                LayoutSwitchCombo::left_alt_left_shift(),
            ),
            (
                "Right Alt+Right Shift",
                LayoutSwitchCombo::right_alt_right_shift(),
            ),
        ];

        for (raw, expected) in combos {
            assert_eq!(LayoutSwitchCombo::from_str(raw).unwrap(), expected);
            assert_eq!(expected.to_string(), raw);
        }
    }

    #[test]
    fn maps_supported_layout_switch_combos_to_xkb_options() {
        assert_eq!(
            LayoutSwitchCombo::ctrl_shift().xkb_option(),
            "grp:ctrl_shift_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::alt_shift().xkb_option(),
            "grp:alt_shift_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::caps_lock().xkb_option(),
            "grp:caps_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::ctrl_space().xkb_option(),
            "grp:ctrl_space_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::super_space().xkb_option(),
            "grp:win_space_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::left_ctrl_left_shift().xkb_option(),
            "grp:lctrl_lshift_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::right_ctrl_right_shift().xkb_option(),
            "grp:rctrl_rshift_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::left_alt_left_shift().xkb_option(),
            "grp:lalt_lshift_toggle"
        );
        assert_eq!(
            LayoutSwitchCombo::right_alt_right_shift().xkb_option(),
            "grp:ralt_rshift_toggle"
        );
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
