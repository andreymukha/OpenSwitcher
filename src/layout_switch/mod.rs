use crate::error::LayoutAutoDetectError;
use crate::model::{
    AutoDetectedLayoutSwitch, DesktopEnvironment, DetectionConfidence, DetectionStrategy,
    LayoutSwitchCombo, LayoutSwitchSetting, LayoutSwitchSource, SessionType, SystemContext,
};
use std::process::Command;

const CINNAMON_INPUT_SOURCES_SCHEMA: &str = "org.cinnamon.desktop.input-sources";
const XKB_OPTIONS_KEY: &str = "xkb-options";
const XFCE_KEYBOARD_LAYOUT_CHANNEL: &str = "keyboard-layout";
const XFCE_XKB_DISABLE_PROPERTY: &str = "/Default/XkbDisable";
const XFCE_XKB_GROUP_PROPERTY: &str = "/Default/XkbOptions/Group";

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
            Strategy::Unsupported => Ok(unsupported_context_fallback(context)),
        }
    }

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
                    fallback_setting(
                        LayoutSwitchCombo::default(),
                        DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                        DetectionConfidence::Low,
                        context,
                    )
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    XfceX11,
    CinnamonX11,
    Unsupported,
}

fn select_strategy(context: SystemContext) -> Strategy {
    match (context.desktop_environment, context.session_type) {
        (DesktopEnvironment::Xfce, SessionType::X11) => Strategy::XfceX11,
        (DesktopEnvironment::Cinnamon, SessionType::X11) => Strategy::CinnamonX11,
        _ => Strategy::Unsupported,
    }
}

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
        Strategy::Unsupported => DetectionStrategy::NoSupportedStrategy,
    };

    fallback_setting(
        LayoutSwitchCombo::default(),
        strategy,
        DetectionConfidence::Low,
        context,
    )
}

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
        _ => None,
    }
}

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

    #[derive(Clone, Default)]
    struct StubReader {
        gsettings_values: Vec<String>,
        xfconf_group: String,
        xfconf_disabled: bool,
        setxkbmap_query_output: String,
    }

    impl DesktopSettingsReader for StubReader {
        fn gsettings_string_list(
            &self,
            _schema: &str,
            _key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            Ok(self.gsettings_values.clone())
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

    #[test]
    fn detects_supported_cinnamon_x11_combo() {
        let detector = LayoutSwitchAutoDetector::with_reader(StubReader {
            gsettings_values: vec!["grp:alt_shift_toggle".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::alt_shift());
        assert_eq!(setting.source, LayoutSwitchSource::AutoDetected);
    }

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
            gsettings_values: vec!["grp:toggle".to_string()],
            ..StubReader::default()
        });

        let setting = detector.detect(cinnamon_x11_context()).unwrap();
        assert_eq!(setting.combo, LayoutSwitchCombo::default());
        assert_eq!(setting.source, LayoutSwitchSource::AutoFallback);
        assert_eq!(setting.auto_detected.confidence, DetectionConfidence::Low);
    }
}
