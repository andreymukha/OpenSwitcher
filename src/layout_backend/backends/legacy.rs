use crate::daemon::keyboard::is_russian_layout;
use crate::daemon::layout_switcher::{LayoutSwitcher, X11LayoutSwitcher};
use crate::layout_backend::{
    AppLayoutKind, BackendCapabilities, CurrentLayoutState, LayoutBackend, LayoutBackendError,
    LayoutBackendOperation, LayoutCode, LayoutSetup, LayoutStateSink, SystemLayout,
};
use crate::model::LayoutSwitchCombo;
use std::process::Command;

pub fn legacy_backend_factory() -> Result<Box<dyn LayoutBackend>, LayoutBackendError> {
    Ok(Box::new(LegacyLayoutBackend::new()))
}

struct LegacyLayoutBackend {
    en: SystemLayout,
    ru: SystemLayout,
}

impl LegacyLayoutBackend {
    fn new() -> Self {
        let (en, ru) = detect_legacy_layout_pair();
        Self { en, ru }
    }

    fn english_layout(code: LayoutCode, index: Option<u32>) -> SystemLayout {
        let (backend_key, display_name) = match code {
            LayoutCode::Gb => ("legacy:gb", "English (UK)"),
            _ => ("legacy:english", "English"),
        };

        SystemLayout {
            backend_key: backend_key.to_string(),
            normalized_code: code,
            display_name: display_name.to_string(),
            kind: AppLayoutKind::English,
            index,
        }
    }

    fn russian_layout(index: Option<u32>) -> SystemLayout {
        SystemLayout {
            backend_key: "legacy:russian".to_string(),
            normalized_code: LayoutCode::Ru,
            display_name: "Russian".to_string(),
            kind: AppLayoutKind::Russian,
            index,
        }
    }
}

fn detect_legacy_layout_pair() -> (SystemLayout, SystemLayout) {
    Command::new("setxkbmap")
        .arg("-query")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .map(|query| legacy_layout_pair_from_setxkbmap_query(&query))
        .unwrap_or_else(default_legacy_layout_pair)
}

fn default_legacy_layout_pair() -> (SystemLayout, SystemLayout) {
    (
        LegacyLayoutBackend::english_layout(LayoutCode::Us, None),
        LegacyLayoutBackend::russian_layout(None),
    )
}

fn legacy_layout_pair_from_setxkbmap_query(query: &str) -> (SystemLayout, SystemLayout) {
    let Some(layouts) = query.lines().find_map(parse_setxkbmap_layouts_line) else {
        return default_legacy_layout_pair();
    };

    let mut english = None;
    let mut russian = None;
    for (index, layout) in layouts.split(',').map(str::trim).enumerate() {
        match layout {
            "us" if english.is_none() => {
                english = Some(LegacyLayoutBackend::english_layout(
                    LayoutCode::Us,
                    Some(index as u32),
                ));
            }
            "gb" if english.is_none() => {
                english = Some(LegacyLayoutBackend::english_layout(
                    LayoutCode::Gb,
                    Some(index as u32),
                ));
            }
            "ru" if russian.is_none() => {
                russian = Some(LegacyLayoutBackend::russian_layout(Some(index as u32)));
            }
            _ => {}
        }
    }

    match (english, russian) {
        (Some(en), Some(ru)) => (en, ru),
        _ => default_legacy_layout_pair(),
    }
}

fn parse_setxkbmap_layouts_line(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix("layout:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_us_ru_legacy_pair_from_setxkbmap_query() {
        let (en, ru) = legacy_layout_pair_from_setxkbmap_query(
            "rules: evdev\nlayout: us,ru\noptions: grp:alt_shift_toggle\n",
        );

        assert_eq!(en.normalized_code, LayoutCode::Us);
        assert_eq!(en.kind, AppLayoutKind::English);
        assert_eq!(ru.normalized_code, LayoutCode::Ru);
        assert_eq!(ru.kind, AppLayoutKind::Russian);
    }

    #[test]
    fn detects_gb_ru_legacy_pair_from_setxkbmap_query() {
        let (en, ru) = legacy_layout_pair_from_setxkbmap_query(
            "rules: evdev\nlayout: gb,ru\noptions: grp:alt_shift_toggle\n",
        );

        assert_eq!(en.normalized_code, LayoutCode::Gb);
        assert_eq!(en.kind, AppLayoutKind::English);
        assert_eq!(ru.normalized_code, LayoutCode::Ru);
        assert_eq!(ru.kind, AppLayoutKind::Russian);
    }
}

impl LayoutBackend for LegacyLayoutBackend {
    fn id(&self) -> &'static str {
        "legacy"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            can_list_layouts: true,
            can_read_current_layout: true,
            can_switch_to_target: false,
            can_switch_next: true,
            can_observe_layout_changes: false,
            can_map_layouts_to_app_kinds: true,
        }
    }

    fn detect_setup(&self) -> Result<LayoutSetup, LayoutBackendError> {
        Ok(LayoutSetup::StrictPair {
            en: self.en.clone(),
            ru: self.ru.clone(),
        })
    }

    fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError> {
        match is_russian_layout() {
            Ok(true) => Ok(CurrentLayoutState::Known {
                layout: self.ru.clone(),
                trustworthy: false,
            }),
            Ok(false) => Ok(CurrentLayoutState::Known {
                layout: self.en.clone(),
                trustworthy: false,
            }),
            Err(error) => Err(LayoutBackendError::runtime(
                self.id(),
                LayoutBackendOperation::CurrentLayoutSnapshot,
                error,
            )),
        }
    }

    fn switch_to(&mut self, _target: &SystemLayout) -> Result<(), LayoutBackendError> {
        Err(LayoutBackendError::unsupported(
            self.id(),
            LayoutBackendOperation::SwitchTo,
        ))
    }

    fn switch_next(&mut self) -> Result<(), LayoutBackendError> {
        let mut switcher = X11LayoutSwitcher::new().map_err(|error| {
            LayoutBackendError::runtime(self.id(), LayoutBackendOperation::SwitchNext, error)
        })?;
        switcher
            .switch_layout(LayoutSwitchCombo::default())
            .map_err(|error| {
                LayoutBackendError::runtime(self.id(), LayoutBackendOperation::SwitchNext, error)
            })
    }

    fn start_monitoring(&mut self, _sink: LayoutStateSink) -> Result<(), LayoutBackendError> {
        Err(LayoutBackendError::unsupported(
            self.id(),
            LayoutBackendOperation::StartMonitoring,
        ))
    }
}
