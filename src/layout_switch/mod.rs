use crate::error::LayoutAutoDetectError;
use crate::model::{
    AutoDetectedLayoutSwitch, DesktopEnvironment, DetectionConfidence, DetectionStrategy,
    LayoutSwitchCombo, LayoutSwitchSetting, LayoutSwitchSource, SessionType, SystemContext,
};
use std::process::Command;

const CINNAMON_INPUT_SOURCES_SCHEMA: &str = "org.cinnamon.desktop.input-sources";
const GNOME_WM_KEYBINDINGS_SCHEMA: &str = "org.gnome.desktop.wm.keybindings";
const GNOME_SWITCH_INPUT_SOURCE_KEY: &str = "switch-input-source";
const GNOME_SWITCH_INPUT_SOURCE_BACKWARD_KEY: &str = "switch-input-source-backward";
const XKB_OPTIONS_KEY: &str = "xkb-options";
const XFCE_KEYBOARD_LAYOUT_CHANNEL: &str = "keyboard-layout";
const XFCE_XKB_DISABLE_PROPERTY: &str = "/Default/XkbDisable";
const XFCE_XKB_GROUP_PROPERTY: &str = "/Default/XkbOptions/Group";

// Desktop settings reader

pub trait DesktopSettingsReader {
    fn gsettings_string_list(
        &self,
        schema: &str,
        key: &str,
    ) -> Result<Vec<String>, LayoutAutoDetectError>;

    fn xfconf_string(&self, channel: &str, property: &str)
        -> Result<String, LayoutAutoDetectError>;

    fn xfconf_bool(&self, channel: &str, property: &str) -> Result<bool, LayoutAutoDetectError>;

    fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CommandDesktopSettingsReader;

impl DesktopSettingsReader for CommandDesktopSettingsReader {
    fn gsettings_string_list(
        &self,
        schema: &str,
        key: &str,
    ) -> Result<Vec<String>, LayoutAutoDetectError> {
        let output = Command::new("gsettings")
            .arg("get")
            .arg(schema)
            .arg(key)
            .output()
            .map_err(LayoutAutoDetectError::GSettingsIo)?;

        if !output.status.success() {
            return Err(LayoutAutoDetectError::GSettingsFailed {
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(parse_gsettings_string_list(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn xfconf_string(
        &self,
        channel: &str,
        property: &str,
    ) -> Result<String, LayoutAutoDetectError> {
        let output = Command::new("xfconf-query")
            .arg("-c")
            .arg(channel)
            .arg("-p")
            .arg(property)
            .output()
            .map_err(LayoutAutoDetectError::XfconfIo)?;

        if !output.status.success() {
            return Err(LayoutAutoDetectError::XfconfFailed {
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn xfconf_bool(&self, channel: &str, property: &str) -> Result<bool, LayoutAutoDetectError> {
        let value = self.xfconf_string(channel, property)?;
        Ok(matches!(value.as_str(), "true" | "True" | "TRUE" | "1"))
    }

    fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError> {
        let output = Command::new("setxkbmap")
            .arg("-query")
            .output()
            .map_err(LayoutAutoDetectError::SetXkbMapIo)?;

        if !output.status.success() {
            return Err(LayoutAutoDetectError::SetXkbMapFailed {
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

// Layout switch auto-detector

#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutSwitchAutoDetector<R = CommandDesktopSettingsReader> {
    reader: R,
}

impl LayoutSwitchAutoDetector<CommandDesktopSettingsReader> {
    pub fn new() -> Self {
        Self {
            reader: CommandDesktopSettingsReader,
        }
    }
}

impl<R: DesktopSettingsReader> LayoutSwitchAutoDetector<R> {
    pub fn with_reader(reader: R) -> Self {
        Self { reader }
    }

    pub fn detect(
        &self,
        context: SystemContext,
    ) -> Result<LayoutSwitchSetting, LayoutAutoDetectError> {
        match select_strategy(context) {
            Strategy::XfceX11 => Ok(self.detect_xfce_x11(context)),
            Strategy::CinnamonX11 => Ok(self.detect_cinnamon_x11(context)),
            Strategy::GnomeWayland => Ok(self.detect_gnome_wayland(context)),
            Strategy::Unsupported => Ok(unsupported_context_fallback(context)),
        }
    }

    // Desktop-specific detection strategies

    fn detect_xfce_x11(&self, context: SystemContext) -> LayoutSwitchSetting {
        match self
            .reader
            .xfconf_bool(XFCE_KEYBOARD_LAYOUT_CHANNEL, XFCE_XKB_DISABLE_PROPERTY)
        {
            Ok(false) => self.detect_xfce_x11_managed(context),
            Ok(true) => self.detect_xfce_x11_system_defaults(context),
            Err(error) => {
                eprintln!(
                    "[layout-switch] Failed to read XFCE XkbDisable flag, using fallback: {error}"
                );
                fallback_setting(
                    LayoutSwitchCombo::default(),
                    DetectionStrategy::XfceX11XfconfKeyboardLayout,
                    DetectionConfidence::Low,
                    context,
                )
            }
        }
    }

    fn detect_xfce_x11_managed(&self, context: SystemContext) -> LayoutSwitchSetting {
        match self
            .reader
            .xfconf_string(XFCE_KEYBOARD_LAYOUT_CHANNEL, XFCE_XKB_GROUP_PROPERTY)
        {
            Ok(option) => combo_from_option_list(&option)
                .map(|combo| {
                    detected_setting(
                        combo,
                        DetectionStrategy::XfceX11XfconfKeyboardLayout,
                        context,
                    )
                })
                .unwrap_or_else(|| {
                    fallback_setting(
                        LayoutSwitchCombo::default(),
                        DetectionStrategy::XfceX11XfconfKeyboardLayout,
                        DetectionConfidence::Low,
                        context,
                    )
                }),
            Err(error) => {
                eprintln!(
                    "[layout-switch] Failed to read XFCE layout group option, using fallback: {error}"
                );
                fallback_setting(
                    LayoutSwitchCombo::default(),
                    DetectionStrategy::XfceX11XfconfKeyboardLayout,
                    DetectionConfidence::Low,
                    context,
                )
            }
        }
    }

    fn detect_xfce_x11_system_defaults(&self, context: SystemContext) -> LayoutSwitchSetting {
        match self.reader.setxkbmap_query() {
            Ok(query) => query
                .lines()
                .find_map(parse_setxkbmap_options_line)
                .and_then(|options| combo_from_option_list(&options))
                .map(|combo| {
                    detected_setting(combo, DetectionStrategy::XfceX11SetXkbmapQuery, context)
                })
                .unwrap_or_else(|| {
                    fallback_setting(
                        LayoutSwitchCombo::default(),
                        DetectionStrategy::XfceX11SetXkbmapQuery,
                        DetectionConfidence::Low,
                        context,
                    )
                }),
            Err(error) => {
                eprintln!(
                    "[layout-switch] Failed to read X11 runtime layout options, using fallback: {error}"
                );
                fallback_setting(
                    LayoutSwitchCombo::default(),
                    DetectionStrategy::XfceX11SetXkbmapQuery,
                    DetectionConfidence::Low,
                    context,
                )
            }
        }
    }

    fn detect_cinnamon_x11(&self, context: SystemContext) -> LayoutSwitchSetting {
        match self
            .reader
            .gsettings_string_list(CINNAMON_INPUT_SOURCES_SCHEMA, XKB_OPTIONS_KEY)
        {
            Ok(options) => options
                .iter()
                .find_map(|option| combo_from_xkb_option(option))
                .map(|combo| {
                    detected_setting(
                        combo,
                        DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                        context,
                    )
                })
                .unwrap_or_else(|| {
                    self.detect_cinnamon_x11_setxkbmap_fallback(context)
                }),
            Err(error) => {
                eprintln!(
                    "[layout-switch] Failed to read Cinnamon input source options, using fallback: {error}"
                );
                fallback_setting(
                    LayoutSwitchCombo::default(),
                    DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                    DetectionConfidence::Low,
                    context,
                )
            }
        }
    }

    fn detect_cinnamon_x11_setxkbmap_fallback(
        &self,
        context: SystemContext,
    ) -> LayoutSwitchSetting {
        match self.reader.setxkbmap_query() {
            Ok(query) => query
                .lines()
                .find_map(parse_setxkbmap_options_line)
                .and_then(|options| combo_from_option_list(&options))
                .map(|combo| {
                    detected_setting(
                        combo,
                        DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                        context,
                    )
                })
                .unwrap_or_else(|| {
                    fallback_setting(
                        LayoutSwitchCombo::default(),
                        DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                        DetectionConfidence::Low,
                        context,
                    )
                }),
            Err(error) => {
                eprintln!(
                    "[layout-switch] Failed to read X11 runtime layout options for Cinnamon fallback, using fallback: {error}"
                );
                fallback_setting(
                    LayoutSwitchCombo::default(),
                    DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                    DetectionConfidence::Low,
                    context,
                )
            }
        }
    }

    fn detect_gnome_wayland(&self, context: SystemContext) -> LayoutSwitchSetting {
        if let Some(combo) =
            self.detect_gnome_wayland_binding(GNOME_SWITCH_INPUT_SOURCE_KEY, "primary")
        {
            return detected_setting(
                combo,
                DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
                context,
            );
        }

        if let Some(combo) =
            self.detect_gnome_wayland_binding(GNOME_SWITCH_INPUT_SOURCE_BACKWARD_KEY, "backward")
        {
            return detected_setting(
                combo,
                DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
                context,
            );
        }

        fallback_setting(
            LayoutSwitchCombo::default(),
            DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
            DetectionConfidence::Low,
            context,
        )
    }

    fn detect_gnome_wayland_binding(&self, key: &str, source: &str) -> Option<LayoutSwitchCombo> {
        match self
            .reader
            .gsettings_string_list(GNOME_WM_KEYBINDINGS_SCHEMA, key)
        {
            Ok(bindings) => bindings
                .iter()
                .find_map(|binding| combo_from_gnome_binding(binding)),
            Err(error) => {
                eprintln!(
                    "[layout-switch] Failed to read GNOME {source} input source binding, using fallback: {error}"
                );
                None
            }
        }
    }
}

// Strategy selection

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    XfceX11,
    CinnamonX11,
    GnomeWayland,
    Unsupported,
}

fn select_strategy(context: SystemContext) -> Strategy {
    match (context.desktop_environment, context.session_type) {
        (DesktopEnvironment::Xfce, SessionType::X11) => Strategy::XfceX11,
        (DesktopEnvironment::Cinnamon, SessionType::X11) => Strategy::CinnamonX11,
        (DesktopEnvironment::Gnome, SessionType::Wayland) => Strategy::GnomeWayland,
        _ => Strategy::Unsupported,
    }
}

// Detection fallback builders

fn detected_setting(
    combo: LayoutSwitchCombo,
    strategy: DetectionStrategy,
    context: SystemContext,
) -> LayoutSwitchSetting {
    LayoutSwitchSetting {
        combo,
        source: LayoutSwitchSource::AutoDetected,
        auto_detected: AutoDetectedLayoutSwitch {
            strategy,
            confidence: DetectionConfidence::High,
            context,
        },
    }
}

fn fallback_setting(
    combo: LayoutSwitchCombo,
    strategy: DetectionStrategy,
    confidence: DetectionConfidence,
    context: SystemContext,
) -> LayoutSwitchSetting {
    LayoutSwitchSetting {
        combo,
        source: LayoutSwitchSource::AutoFallback,
        auto_detected: AutoDetectedLayoutSwitch {
            strategy,
            confidence,
            context,
        },
    }
}

pub fn unsupported_context_fallback(context: SystemContext) -> LayoutSwitchSetting {
    fallback_setting(
        LayoutSwitchCombo::default(),
        DetectionStrategy::NoSupportedStrategy,
        DetectionConfidence::Unsupported,
        context,
    )
}

pub fn failed_detection_fallback(context: SystemContext) -> LayoutSwitchSetting {
    let strategy = match select_strategy(context) {
        Strategy::XfceX11 => DetectionStrategy::XfceX11XfconfKeyboardLayout,
        Strategy::CinnamonX11 => DetectionStrategy::CinnamonX11GSettingsXkbOptions,
        Strategy::GnomeWayland => DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
        Strategy::Unsupported => DetectionStrategy::NoSupportedStrategy,
    };

    fallback_setting(
        LayoutSwitchCombo::default(),
        strategy,
        DetectionConfidence::Low,
        context,
    )
}

// XKB option parsing

fn combo_from_option_list(raw: &str) -> Option<LayoutSwitchCombo> {
    raw.split(',')
        .map(str::trim)
        .find_map(combo_from_xkb_option)
}

fn combo_from_xkb_option(option: &str) -> Option<LayoutSwitchCombo> {
    match option {
        "grp:ctrl_shift_toggle" => Some(LayoutSwitchCombo::ctrl_shift()),
        "grp:alt_shift_toggle" => Some(LayoutSwitchCombo::alt_shift()),
        "grp:caps_toggle" => Some(LayoutSwitchCombo::caps_lock()),
        "grp:ctrl_space_toggle" => Some(LayoutSwitchCombo::ctrl_space()),
        "grp:win_space_toggle" => Some(LayoutSwitchCombo::super_space()),
        "grp:lctrl_lshift_toggle" => Some(LayoutSwitchCombo::left_ctrl_left_shift()),
        "grp:rctrl_rshift_toggle" => Some(LayoutSwitchCombo::right_ctrl_right_shift()),
        "grp:lalt_lshift_toggle" => Some(LayoutSwitchCombo::left_alt_left_shift()),
        "grp:ralt_rshift_toggle" => Some(LayoutSwitchCombo::right_alt_right_shift()),
        _ => None,
    }
}

// GNOME accelerator parsing

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GnomeAcceleratorToken {
    Shift(GnomeAcceleratorSide),
    Ctrl(GnomeAcceleratorSide),
    Alt(GnomeAcceleratorSide),
    Super(GnomeAcceleratorSide),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GnomeAcceleratorSide {
    Generic,
    Left,
    Right,
}

fn combo_from_gnome_binding(binding: &str) -> Option<LayoutSwitchCombo> {
    let (modifiers, key) = parse_gnome_binding(binding)?;
    if modifiers.is_empty() {
        return matches!(key.as_str(), "caps_lock").then_some(LayoutSwitchCombo::caps_lock());
    }

    if modifiers.len() != 1 {
        return None;
    }

    match (modifiers[0], key.as_str()) {
        (GnomeAcceleratorToken::Super(GnomeAcceleratorSide::Generic), "space")
        | (GnomeAcceleratorToken::Super(GnomeAcceleratorSide::Left), "space") => {
            Some(LayoutSwitchCombo::super_space())
        }
        (GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Generic), "space")
        | (GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Left), "space") => {
            Some(LayoutSwitchCombo::ctrl_space())
        }
        _ => combo_from_gnome_shift_pair(modifiers[0], key.as_str()),
    }
}

fn combo_from_gnome_shift_pair(
    modifier: GnomeAcceleratorToken,
    key: &str,
) -> Option<LayoutSwitchCombo> {
    if let Some(combo) = gnome_shift_combo(
        modifier,
        key,
        GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Generic),
        GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Generic),
    ) {
        return Some(combo);
    }

    gnome_shift_combo(
        modifier,
        key,
        GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Generic),
        GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Generic),
    )
}

fn gnome_shift_combo(
    modifier: GnomeAcceleratorToken,
    key: &str,
    primary_generic: GnomeAcceleratorToken,
    secondary_generic: GnomeAcceleratorToken,
) -> Option<LayoutSwitchCombo> {
    let primary = primary_token_side(modifier, key, primary_generic, secondary_generic)?;
    let secondary = secondary_token_side(modifier, key, primary_generic, secondary_generic)?;

    match (primary_generic, primary, secondary) {
        (
            GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Generic),
            GnomeAcceleratorSide::Left,
            GnomeAcceleratorSide::Left,
        ) => Some(LayoutSwitchCombo::left_alt_left_shift()),
        (
            GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Generic),
            GnomeAcceleratorSide::Right,
            GnomeAcceleratorSide::Right,
        ) => Some(LayoutSwitchCombo::right_alt_right_shift()),
        (
            GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Generic),
            primary_side,
            secondary_side,
        ) if side_pair_matches_generic_left(primary_side, secondary_side) => {
            Some(LayoutSwitchCombo::alt_shift())
        }
        (
            GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Generic),
            GnomeAcceleratorSide::Left,
            GnomeAcceleratorSide::Left,
        ) => Some(LayoutSwitchCombo::left_ctrl_left_shift()),
        (
            GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Generic),
            GnomeAcceleratorSide::Right,
            GnomeAcceleratorSide::Right,
        ) => Some(LayoutSwitchCombo::right_ctrl_right_shift()),
        (
            GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Generic),
            primary_side,
            secondary_side,
        ) if side_pair_matches_generic_left(primary_side, secondary_side) => {
            Some(LayoutSwitchCombo::ctrl_shift())
        }
        _ => None,
    }
}

fn primary_token_side(
    modifier: GnomeAcceleratorToken,
    key: &str,
    primary_generic: GnomeAcceleratorToken,
    secondary_generic: GnomeAcceleratorToken,
) -> Option<GnomeAcceleratorSide> {
    match modifier {
        GnomeAcceleratorToken::Alt(side)
            if matches!(primary_generic, GnomeAcceleratorToken::Alt(_))
                && matches!(
                    normalize_gnome_key_token(key),
                    Some(GnomeAcceleratorToken::Shift(_))
                ) =>
        {
            Some(side)
        }
        GnomeAcceleratorToken::Ctrl(side)
            if matches!(primary_generic, GnomeAcceleratorToken::Ctrl(_))
                && matches!(
                    normalize_gnome_key_token(key),
                    Some(GnomeAcceleratorToken::Shift(_))
                ) =>
        {
            Some(side)
        }
        GnomeAcceleratorToken::Shift(_)
            if matches!(
                normalize_gnome_key_token(key),
                Some(GnomeAcceleratorToken::Alt(_))
            ) && matches!(primary_generic, GnomeAcceleratorToken::Alt(_)) =>
        {
            match normalize_gnome_key_token(key)? {
                GnomeAcceleratorToken::Alt(side) => Some(side),
                _ => None,
            }
        }
        GnomeAcceleratorToken::Shift(_)
            if matches!(
                normalize_gnome_key_token(key),
                Some(GnomeAcceleratorToken::Ctrl(_))
            ) && matches!(primary_generic, GnomeAcceleratorToken::Ctrl(_)) =>
        {
            match normalize_gnome_key_token(key)? {
                GnomeAcceleratorToken::Ctrl(side) => Some(side),
                _ => None,
            }
        }
        _ if matches!(
            secondary_generic,
            GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Generic)
        ) =>
        {
            None
        }
        _ => None,
    }
}

fn secondary_token_side(
    modifier: GnomeAcceleratorToken,
    key: &str,
    _primary_generic: GnomeAcceleratorToken,
    secondary_generic: GnomeAcceleratorToken,
) -> Option<GnomeAcceleratorSide> {
    if !matches!(
        secondary_generic,
        GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Generic)
    ) {
        return None;
    }

    match modifier {
        GnomeAcceleratorToken::Shift(side)
            if matches!(
                normalize_gnome_key_token(key),
                Some(GnomeAcceleratorToken::Alt(_)) | Some(GnomeAcceleratorToken::Ctrl(_))
            ) =>
        {
            Some(side)
        }
        GnomeAcceleratorToken::Alt(_) | GnomeAcceleratorToken::Ctrl(_) => {
            match normalize_gnome_key_token(key)? {
                GnomeAcceleratorToken::Shift(side) => Some(side),
                _ => None,
            }
        }
        _ => None,
    }
}

fn side_pair_matches_generic_left(
    primary: GnomeAcceleratorSide,
    secondary: GnomeAcceleratorSide,
) -> bool {
    matches!(
        primary,
        GnomeAcceleratorSide::Generic | GnomeAcceleratorSide::Left
    ) && matches!(
        secondary,
        GnomeAcceleratorSide::Generic | GnomeAcceleratorSide::Left
    )
}

fn parse_gnome_binding(binding: &str) -> Option<(Vec<GnomeAcceleratorToken>, String)> {
    let mut modifiers = Vec::new();
    let mut rest = binding.trim();

    while let Some(stripped) = rest.strip_prefix('<') {
        let end = stripped.find('>')?;
        let token = &stripped[..end];
        modifiers.push(normalize_gnome_modifier_token(token)?);
        rest = &stripped[end + 1..];
    }

    let key = normalize_gnome_key_name(rest)?;
    Some((modifiers, key))
}

fn normalize_gnome_modifier_token(token: &str) -> Option<GnomeAcceleratorToken> {
    normalize_gnome_key_token(token)
}

fn normalize_gnome_key_token(token: &str) -> Option<GnomeAcceleratorToken> {
    match token.trim().to_ascii_lowercase().as_str() {
        "shift" => Some(GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Generic)),
        "shift_l" => Some(GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Left)),
        "shift_r" => Some(GnomeAcceleratorToken::Shift(GnomeAcceleratorSide::Right)),
        "control" | "ctrl" | "primary" => {
            Some(GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Generic))
        }
        "control_l" | "ctrl_l" | "primary_l" => {
            Some(GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Left))
        }
        "control_r" | "ctrl_r" | "primary_r" => {
            Some(GnomeAcceleratorToken::Ctrl(GnomeAcceleratorSide::Right))
        }
        "alt" => Some(GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Generic)),
        "alt_l" => Some(GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Left)),
        "alt_r" => Some(GnomeAcceleratorToken::Alt(GnomeAcceleratorSide::Right)),
        "super" | "meta" | "win" => {
            Some(GnomeAcceleratorToken::Super(GnomeAcceleratorSide::Generic))
        }
        "super_l" | "meta_l" | "win_l" => {
            Some(GnomeAcceleratorToken::Super(GnomeAcceleratorSide::Left))
        }
        "super_r" | "meta_r" | "win_r" => {
            Some(GnomeAcceleratorToken::Super(GnomeAcceleratorSide::Right))
        }
        _ => None,
    }
}

fn normalize_gnome_key_name(key: &str) -> Option<String> {
    let normalized = key.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    Some(normalized)
}

// Command output parsing

fn parse_setxkbmap_options_line(line: &str) -> Option<String> {
    let line = line.trim();
    let (_, options) = line.split_once("options:")?;
    Some(options.trim().to_string())
}

fn parse_gsettings_string_list(output: &str) -> Vec<String> {
    let trimmed = output.trim();
    if trimmed == "@as []" || trimmed == "[]" {
        return Vec::new();
    }

    let mut values = Vec::new();
    let mut in_string = false;
    let mut current = String::new();

    for ch in trimmed.chars() {
        match ch {
            '\'' if in_string => {
                values.push(current.clone());
                current.clear();
                in_string = false;
            }
            '\'' => in_string = true,
            _ if in_string => current.push(ch),
            _ => {}
        }
    }

    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DesktopEnvironment, DistroKind, SessionType};

    // Test helpers

    #[derive(Clone, Default)]
    struct StubReader {
        cinnamon_gsettings_values: Vec<String>,
        gnome_primary_gsettings_values: Vec<String>,
        gnome_backward_gsettings_values: Vec<String>,
        xfconf_group: String,
        xfconf_disabled: bool,
        setxkbmap_query_output: String,
        setxkbmap_query_should_fail: bool,
    }

    impl DesktopSettingsReader for StubReader {
        fn gsettings_string_list(
            &self,
            schema: &str,
            key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            if schema == CINNAMON_INPUT_SOURCES_SCHEMA && key == XKB_OPTIONS_KEY {
                Ok(self.cinnamon_gsettings_values.clone())
            } else if schema == GNOME_WM_KEYBINDINGS_SCHEMA && key == GNOME_SWITCH_INPUT_SOURCE_KEY
            {
                Ok(self.gnome_primary_gsettings_values.clone())
            } else if schema == GNOME_WM_KEYBINDINGS_SCHEMA
                && key == GNOME_SWITCH_INPUT_SOURCE_BACKWARD_KEY
            {
                Ok(self.gnome_backward_gsettings_values.clone())
            } else {
                Ok(Vec::new())
            }
        }

        fn xfconf_string(
            &self,
            _channel: &str,
            property: &str,
        ) -> Result<String, LayoutAutoDetectError> {
            match property {
                XFCE_XKB_GROUP_PROPERTY => Ok(self.xfconf_group.clone()),
                XFCE_XKB_DISABLE_PROPERTY => Ok(if self.xfconf_disabled {
                    "true".to_string()
                } else {
                    "false".to_string()
                }),
                _ => Ok(String::new()),
            }
        }

        fn xfconf_bool(
            &self,
            _channel: &str,
            property: &str,
        ) -> Result<bool, LayoutAutoDetectError> {
            match property {
                XFCE_XKB_DISABLE_PROPERTY => Ok(self.xfconf_disabled),
                _ => Ok(false),
            }
        }

        fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError> {
            if self.setxkbmap_query_should_fail {
                return Err(LayoutAutoDetectError::SetXkbMapFailed {
                    stderr: "unexpected setxkbmap query".to_string(),
                });
            }

            Ok(self.setxkbmap_query_output.clone())
        }
    }

    fn cinnamon_x11_context() -> SystemContext {
        SystemContext {
            session_type: SessionType::X11,
            desktop_environment: DesktopEnvironment::Cinnamon,
            distro: DistroKind::LinuxMint,
        }
    }

    fn xfce_x11_context() -> SystemContext {
        SystemContext {
            session_type: SessionType::X11,
            desktop_environment: DesktopEnvironment::Xfce,
            distro: DistroKind::LinuxMint,
        }
    }

    fn gnome_wayland_context() -> SystemContext {
        SystemContext {
            session_type: SessionType::Wayland,
            desktop_environment: DesktopEnvironment::Gnome,
            distro: DistroKind::Ubuntu,
        }
    }

    fn setxkbmap_query_with_options(options: &str) -> String {
        format!("rules: evdev\noptions:    {options}\n")
    }

    // Parser helpers

    #[test]
    fn parses_gsettings_string_list() {
        let parsed = parse_gsettings_string_list("['grp:ctrl_shift_toggle', 'foo']");
        assert_eq!(parsed, vec!["grp:ctrl_shift_toggle", "foo"]);
    }

    #[test]
    fn parses_setxkbmap_options_line() {
        let parsed =
            parse_setxkbmap_options_line("options:    grp:ctrl_shift_toggle,grp_led:scroll");
        assert_eq!(
            parsed.as_deref(),
            Some("grp:ctrl_shift_toggle,grp_led:scroll")
        );
    }

    // Strategy selection

    #[test]
    fn selects_gnome_wayland_strategy() {
        assert_eq!(
            select_strategy(gnome_wayland_context()),
            Strategy::GnomeWayland
        );
    }

    // GNOME binding parsing

    #[test]
    fn maps_supported_gnome_bindings_to_layout_switch_combos() {
        let cases = [
            ("<Super>space", LayoutSwitchCombo::super_space()),
            ("<Super_L>space", LayoutSwitchCombo::super_space()),
            ("<Primary>space", LayoutSwitchCombo::ctrl_space()),
            ("<Control>space", LayoutSwitchCombo::ctrl_space()),
            ("<Control_L>space", LayoutSwitchCombo::ctrl_space()),
            ("Caps_Lock", LayoutSwitchCombo::caps_lock()),
            ("<Shift>Alt_L", LayoutSwitchCombo::alt_shift()),
            ("<Alt>Shift_L", LayoutSwitchCombo::alt_shift()),
            ("<Alt_L>Shift_L", LayoutSwitchCombo::left_alt_left_shift()),
            ("<Alt_R>Shift_R", LayoutSwitchCombo::right_alt_right_shift()),
            ("<Shift>Control_L", LayoutSwitchCombo::ctrl_shift()),
            ("<Primary>Shift_L", LayoutSwitchCombo::ctrl_shift()),
            (
                "<Control_L>Shift_L",
                LayoutSwitchCombo::left_ctrl_left_shift(),
            ),
            (
                "<Control_R>Shift_R",
                LayoutSwitchCombo::right_ctrl_right_shift(),
            ),
        ];

        for (binding, expected) in cases {
            assert_eq!(
                combo_from_gnome_binding(binding),
                Some(expected),
                "{binding}"
            );
        }
    }

    // XFCE detection

    #[test]
    fn detects_supported_xfce_x11_combo_from_xfconf() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            xfconf_group: "grp:alt_shift_toggle".to_string(),
            ..StubReader::default()
        });

        let setting = detector.detect(xfce_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::alt_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::XfceX11XfconfKeyboardLayout
        );
    }

    #[test]
    fn detects_side_specific_xfce_x11_combo_from_xfconf() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            xfconf_group: "grp:rctrl_rshift_toggle".to_string(),
            ..StubReader::default()
        });

        let setting = detector.detect(xfce_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::right_ctrl_right_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
    }

    #[test]
    fn xfce_xfconf_falls_back_for_unsupported_combo() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            xfconf_group: "grp:toggle".to_string(),
            ..StubReader::default()
        });

        let setting = detector.detect(xfce_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::XfceX11XfconfKeyboardLayout
        );
    }

    #[test]
    fn xfce_system_defaults_use_setxkbmap_query() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            xfconf_disabled: true,
            setxkbmap_query_output: "rules: evdev\noptions:    grp:caps_toggle,grp_led:scroll\n"
                .to_string(),
            ..StubReader::default()
        });

        let setting = detector.detect(xfce_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::caps_lock());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::XfceX11SetXkbmapQuery
        );
    }

    #[test]
    fn xfce_system_defaults_fall_back_for_missing_supported_combo() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            xfconf_disabled: true,
            setxkbmap_query_output: "rules: evdev\noptions:    grp:toggle,grp_led:scroll\n"
                .to_string(),
            ..StubReader::default()
        });

        let setting = detector.detect(xfce_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::XfceX11SetXkbmapQuery
        );
    }

    // Cinnamon detection

    #[test]
    fn detects_supported_cinnamon_x11_combo() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            cinnamon_gsettings_values: vec!["grp:alt_shift_toggle".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::alt_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
    }

    #[test]
    fn cinnamon_x11_gsettings_combo_wins_over_setxkbmap_fallback() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            cinnamon_gsettings_values: vec!["grp:alt_shift_toggle".to_string()],
            setxkbmap_query_should_fail: true,
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::alt_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::CinnamonX11GSettingsXkbOptions
        );
    }

    #[test]
    fn detects_side_specific_cinnamon_x11_combo() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            cinnamon_gsettings_values: vec!["grp:lalt_lshift_toggle".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::left_alt_left_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
    }

    #[test]
    fn cinnamon_x11_uses_setxkbmap_when_gsettings_is_empty() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            setxkbmap_query_output: setxkbmap_query_with_options(
                "grp:alt_shift_toggle,grp_led:scroll",
            ),
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::alt_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::CinnamonX11GSettingsXkbOptions
        );
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::High);
    }

    #[test]
    fn cinnamon_x11_uses_setxkbmap_when_gsettings_has_no_supported_combo() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            cinnamon_gsettings_values: vec!["grp:toggle".to_string()],
            setxkbmap_query_output: setxkbmap_query_with_options(
                "grp:rctrl_rshift_toggle,grp_led:scroll",
            ),
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::right_ctrl_right_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::CinnamonX11GSettingsXkbOptions
        );
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::High);
    }

    #[test]
    fn cinnamon_x11_falls_back_when_gsettings_and_setxkbmap_are_not_useful() {
        let cases = [
            (
                vec!["grp:toggle".to_string()],
                setxkbmap_query_with_options("grp:toggle,grp_led:scroll"),
            ),
            (Vec::new(), "rules: evdev\nlayout: us,ru\n".to_string()),
        ];

        for (cinnamon_gsettings_values, setxkbmap_query_output) in cases {
            let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
                cinnamon_gsettings_values,
                setxkbmap_query_output,
                ..StubReader::default()
            });

            let setting = detector.detect(cinnamon_x11_context()).unwrap();
            assert_eq!(setting.combo, LayoutSwitchCombo::default());
            assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
            assert_eq!(
                setting.auto_detected.strategy,
                DetectionStrategy::CinnamonX11GSettingsXkbOptions
            );
            assert_eq!(setting.auto_detected.confidence, DetectionConfidence::Low);
        }
    }

    // Fallback behavior

    #[test]
    fn falls_back_for_unsupported_context() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader::default());
        let context = SystemContext {
            session_type: SessionType::X11,
            desktop_environment: DesktopEnvironment::Kde,
            distro: DistroKind::LinuxMint,
        };

        let setting = detector.detect(context).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::NoSupportedStrategy
        );
    }

    #[test]
    fn falls_back_when_cinnamon_x11_option_is_not_supported() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            cinnamon_gsettings_values: vec!["grp:toggle".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::Low);
    }

    #[test]
    fn detects_right_alt_shift_option() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            cinnamon_gsettings_values: vec!["grp:ralt_rshift_toggle".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::right_alt_right_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
    }

    #[test]
    fn failed_detection_fallback_keeps_supported_strategy_context() {
        let context = cinnamon_x11_context();
        let setting = failed_detection_fallback(context);

        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::CinnamonX11GSettingsXkbOptions
        );
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::Low);
        assert_eq!(setting.auto_detected.context, context);
    }

    #[test]
    fn failed_detection_fallback_uses_no_strategy_for_unknown_context() {
        let context = SystemContext::default();
        let setting = failed_detection_fallback(context);

        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::NoSupportedStrategy
        );
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::Low);
        assert_eq!(setting.auto_detected.context, context);
    }

    // GNOME Wayland detection

    #[test]
    fn detects_supported_gnome_wayland_combo_from_primary_keybindings() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            gnome_primary_gsettings_values: vec![
                "<Super>space".to_string(),
                "XF86Keyboard".to_string(),
            ],
            gnome_backward_gsettings_values: vec![
                "<Shift><Super>space".to_string(),
                "<Shift>XF86Keyboard".to_string(),
            ],
            ..StubReader::default()
        });

        let setting = detector.detect(gnome_wayland_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::super_space());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::GnomeWaylandGSettingsWmKeybindings
        );
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::High);
    }

    #[test]
    fn uses_backward_gnome_wayland_binding_when_primary_is_not_recognized() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            gnome_primary_gsettings_values: vec!["XF86Keyboard".to_string()],
            gnome_backward_gsettings_values: vec!["<Primary>space".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(gnome_wayland_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::ctrl_space());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::GnomeWaylandGSettingsWmKeybindings
        );
    }

    #[test]
    fn gnome_wayland_falls_back_with_supported_strategy_when_binding_is_not_recognized() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            gnome_primary_gsettings_values: vec!["XF86Keyboard".to_string()],
            gnome_backward_gsettings_values: vec!["<Shift><Super>space".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(gnome_wayland_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(
            setting.auto_detected.strategy,
            DetectionStrategy::GnomeWaylandGSettingsWmKeybindings
        );
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::Low);
    }
}
