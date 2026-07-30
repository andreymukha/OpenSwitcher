use super::{AppLayoutKind, CurrentLayoutState, LayoutCode, LayoutSetup, SystemLayout};
use crate::layout_switch::DesktopSettingsReader;
use crate::model::{DesktopEnvironment, SessionType, SystemContext};
use std::collections::HashSet;

pub(crate) const GNOME_INPUT_SOURCES_SCHEMA: &str = "org.gnome.desktop.input-sources";
pub(crate) const GNOME_SOURCES_KEY: &str = "sources";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutSetupDetection {
    Confirmed(LayoutSetup),
    TemporarilyUnavailable { reason: String },
    Unsupported { reason: String },
}

impl LayoutSetupDetection {
    pub fn effective_setup(&self) -> LayoutSetup {
        match self {
            Self::Confirmed(setup) => setup.clone(),
            Self::TemporarilyUnavailable { reason } => LayoutSetup::Unsupported {
                reason: format!("temporarily-unavailable:{reason}"),
            },
            Self::Unsupported { reason } => LayoutSetup::Unsupported {
                reason: format!("unsupported:{reason}"),
            },
        }
    }

    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed(_))
    }
}

pub fn detect_layout_setup<R: DesktopSettingsReader + ?Sized>(
    context: SystemContext,
    reader: &R,
) -> LayoutSetupDetection {
    match (context.session_type, context.desktop_environment) {
        (SessionType::X11, _) => match reader.setxkbmap_query() {
            Ok(query) => detect_x11_setup_from_query(&query),
            Err(error) => LayoutSetupDetection::TemporarilyUnavailable {
                reason: format!("setxkbmap-query:{error}"),
            },
        },
        (SessionType::Wayland, DesktopEnvironment::Gnome) => {
            match reader.gsettings_string_list(GNOME_INPUT_SOURCES_SCHEMA, GNOME_SOURCES_KEY) {
                Ok(sources) => detect_gnome_setup_from_sources(&sources),
                Err(error) => LayoutSetupDetection::TemporarilyUnavailable {
                    reason: format!("gnome-sources:{error}"),
                },
            }
        }
        _ => LayoutSetupDetection::Unsupported {
            reason: "unsupported-session-context".to_string(),
        },
    }
}

pub(crate) fn detect_x11_setup_from_query(query: &str) -> LayoutSetupDetection {
    let layout_value = match single_query_field(query, "layout:") {
        Ok(Some(value)) if !value.is_empty() => value,
        Ok(_) => return unsupported("x11-layout-missing"),
        Err(reason) => return unsupported(reason),
    };

    let layout_ids = layout_value.split(',').map(str::trim).collect::<Vec<_>>();
    if layout_ids.iter().any(|value| value.is_empty()) {
        return unsupported("x11-layout-malformed");
    }

    match single_query_field(query, "variant:") {
        Ok(Some(value)) => {
            let variants = value.split(',').map(str::trim).collect::<Vec<_>>();
            if variants.len() != layout_ids.len() {
                return unsupported("x11-variant-count-mismatch");
            }
            if variants.iter().any(|variant| !variant.is_empty()) {
                return unsupported("x11-variant-unsupported");
            }
        }
        Ok(None) => {}
        Err(reason) => return unsupported(reason),
    }

    classify_plain_layouts("x11", &layout_ids)
}

pub(crate) fn detect_gnome_setup_from_sources(sources: &[String]) -> LayoutSetupDetection {
    let sources = match parse_gnome_sources(sources) {
        Ok(sources) if !sources.is_empty() => sources,
        Ok(_) => return unsupported("gnome-sources-empty"),
        Err(reason) => return unsupported(reason),
    };

    if sources.iter().any(|source| source.source_type != "xkb") {
        return unsupported("gnome-source-type-unsupported");
    }

    let layout_ids = sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<Vec<_>>();
    classify_plain_layouts("gnome", &layout_ids)
}

pub fn current_layout_from_group(
    setup: &LayoutSetup,
    current_group: u8,
    actual_num_groups: u8,
) -> CurrentLayoutState {
    let Some(layouts) = layouts_from_confirmed_setup(setup) else {
        return unknown_current_layout("layout-setup-unconfirmed");
    };

    if !indices_match_group_count(&layouts, actual_num_groups) {
        return unknown_current_layout("layout-group-count-mismatch");
    }

    layouts
        .into_iter()
        .find(|layout| layout.index == Some(u32::from(current_group)))
        .cloned()
        .map(known_current_layout)
        .unwrap_or_else(|| unknown_current_layout("layout-group-index-unknown"))
}

pub fn current_layout_from_gnome_sources(
    configured_sources: &[String],
    mru_sources: &[String],
) -> CurrentLayoutState {
    let setup = match detect_gnome_setup_from_sources(configured_sources) {
        LayoutSetupDetection::Confirmed(setup) => setup,
        _ => return unknown_current_layout("gnome-layout-setup-unconfirmed"),
    };
    let mru_sources = match parse_gnome_sources(mru_sources) {
        Ok(sources) if !sources.is_empty() => sources,
        Ok(_) => return unknown_current_layout("gnome-mru-empty"),
        Err(reason) => return unknown_current_layout(reason),
    };
    let current = &mru_sources[0];
    if current.source_type != "xkb" {
        return unknown_current_layout("gnome-current-source-type-unsupported");
    }

    layouts_from_confirmed_setup(&setup)
        .and_then(|layouts| {
            layouts
                .into_iter()
                .find(|layout| {
                    layout
                        .normalized_code
                        .normalized_str()
                        .is_some_and(|code| code == current.source_id)
                })
                .cloned()
        })
        .map(known_current_layout)
        .unwrap_or_else(|| unknown_current_layout("gnome-current-source-unknown"))
}

fn single_query_field<'a>(query: &'a str, field: &str) -> Result<Option<&'a str>, &'static str> {
    let mut values = query
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(field).map(str::trim));
    let value = values.next();
    if values.next().is_some() {
        return Err(match field {
            "layout:" => "x11-layout-duplicated",
            "variant:" => "x11-variant-duplicated",
            _ => "x11-query-field-duplicated",
        });
    }
    Ok(value)
}

fn classify_plain_layouts(source: &str, layout_ids: &[&str]) -> LayoutSetupDetection {
    let mut seen = HashSet::with_capacity(layout_ids.len());
    let mut layouts = Vec::with_capacity(layout_ids.len());

    for (index, layout_id) in layout_ids.iter().enumerate() {
        if !seen.insert(*layout_id) {
            return unsupported("layout-code-duplicated");
        }
        let code = match LayoutCode::from_normalized(layout_id) {
            Ok(LayoutCode::Unknown) | Err(_) => {
                return unsupported("layout-code-malformed");
            }
            Ok(code) => code,
        };
        layouts.push(system_layout(source, layout_id, code, index as u32));
    }

    let mut english = layouts
        .iter()
        .filter(|layout| layout.kind == AppLayoutKind::English);
    let Some(en) = english.next().cloned() else {
        return unsupported("english-layout-missing");
    };
    if english.next().is_some() {
        return unsupported("english-layout-duplicated");
    }

    let mut russian = layouts
        .iter()
        .filter(|layout| layout.kind == AppLayoutKind::Russian);
    let Some(ru) = russian.next().cloned() else {
        return unsupported("russian-layout-missing");
    };
    if russian.next().is_some() {
        return unsupported("russian-layout-duplicated");
    }

    if layouts.len() == 2 {
        return LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { en, ru });
    }

    let others = layouts
        .into_iter()
        .filter(|layout| layout.kind == AppLayoutKind::Other)
        .collect();
    LayoutSetupDetection::Confirmed(LayoutSetup::PairPlusOther { en, ru, others })
}

fn system_layout(
    source: &str,
    layout_id: &str,
    normalized_code: LayoutCode,
    index: u32,
) -> SystemLayout {
    let (display_name, kind) = match normalized_code {
        LayoutCode::Us => ("English (US)".to_string(), AppLayoutKind::English),
        LayoutCode::Gb => ("English (UK)".to_string(), AppLayoutKind::English),
        LayoutCode::Ru => ("Russian".to_string(), AppLayoutKind::Russian),
        LayoutCode::Other(_) => (layout_id.to_string(), AppLayoutKind::Other),
        LayoutCode::Unknown => ("Unknown".to_string(), AppLayoutKind::Unknown),
    };
    SystemLayout {
        backend_key: format!("{source}:{layout_id}:{index}"),
        normalized_code,
        display_name,
        kind,
        index: Some(index),
    }
}

fn layouts_from_confirmed_setup(setup: &LayoutSetup) -> Option<Vec<&SystemLayout>> {
    match setup {
        LayoutSetup::StrictPair { en, ru } => Some(vec![en, ru]),
        LayoutSetup::PairPlusOther { en, ru, others } => {
            let mut layouts = Vec::with_capacity(others.len() + 2);
            layouts.push(en);
            layouts.push(ru);
            layouts.extend(others);
            Some(layouts)
        }
        LayoutSetup::Unsupported { .. } => None,
    }
}

fn indices_match_group_count(layouts: &[&SystemLayout], actual_num_groups: u8) -> bool {
    if usize::from(actual_num_groups) != layouts.len() {
        return false;
    }
    let mut indices = HashSet::with_capacity(layouts.len());
    layouts.iter().all(|layout| {
        layout
            .index
            .is_some_and(|index| index < u32::from(actual_num_groups) && indices.insert(index))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GnomeInputSource {
    source_type: String,
    source_id: String,
}

fn parse_gnome_sources(values: &[String]) -> Result<Vec<GnomeInputSource>, &'static str> {
    if values.len() % 2 != 0 {
        return Err("gnome-sources-malformed");
    }
    Ok(values
        .chunks_exact(2)
        .map(|chunk| GnomeInputSource {
            source_type: chunk[0].clone(),
            source_id: chunk[1].clone(),
        })
        .collect())
}

fn known_current_layout(layout: SystemLayout) -> CurrentLayoutState {
    CurrentLayoutState::Known {
        layout,
        trustworthy: true,
    }
}

fn unknown_current_layout(reason: impl Into<String>) -> CurrentLayoutState {
    CurrentLayoutState::Unknown {
        reason: reason.into(),
    }
}

fn unsupported(reason: impl Into<String>) -> LayoutSetupDetection {
    LayoutSetupDetection::Unsupported {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LayoutAutoDetectError;
    use crate::layout_backend::{
        AppLayoutKind, CurrentLayoutState, LayoutCode, LayoutSetup, SystemLayout,
    };
    use crate::layout_switch::DesktopSettingsReader;
    use crate::model::{DesktopEnvironment, SessionType, SystemContext};
    use std::cell::Cell;

    #[test]
    fn x11_exact_us_ru_and_gb_ru_are_strict_pairs() {
        for (query, english) in [
            ("layout: us,ru\nvariant: ,\n", LayoutCode::Us),
            ("layout: ru,gb\nvariant: ,\n", LayoutCode::Gb),
        ] {
            let LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { en, ru }) =
                detect_x11_setup_from_query(query)
            else {
                panic!("expected confirmed strict pair for {query:?}");
            };
            assert_eq!(en.normalized_code, english);
            assert_eq!(ru.normalized_code, LayoutCode::Ru);
            assert_ne!(en.index, ru.index);
        }
    }

    #[test]
    fn x11_missing_pair_malformed_and_variant_fail_closed() {
        for query in [
            "",
            "rules: evdev\n",
            "layout: us\n",
            "layout: ru\n",
            "layout: us,us,ru\n",
            "layout: us,ru\nvariant: dvorak,\n",
            "layout: us,ru\nvariant: ,phonetic\n",
            "layout: us,ru\nvariant: \n",
        ] {
            assert!(
                matches!(
                    detect_x11_setup_from_query(query),
                    LayoutSetupDetection::Unsupported { .. }
                ),
                "query must fail closed: {query:?}"
            );
        }
    }

    #[test]
    fn x11_extra_plain_layout_is_pair_plus_other() {
        assert!(matches!(
            detect_x11_setup_from_query("layout: us,de,ru\nvariant: ,,"),
            LayoutSetupDetection::Confirmed(LayoutSetup::PairPlusOther {
                ref others,
                ..
            }) if others.len() == 1
        ));
    }

    #[test]
    fn x11_absent_variant_is_allowed_but_duplicate_or_empty_groups_are_not() {
        assert!(matches!(
            detect_x11_setup_from_query("layout: us,ru\n"),
            LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
        ));

        for query in [
            "layout: us,ru\nlayout: us,ru\n",
            "layout: us,,ru\n",
            "layout: us,ru\nvariant: ,\nvariant: ,\n",
            "layout: US,ru\n",
        ] {
            assert!(
                matches!(
                    detect_x11_setup_from_query(query),
                    LayoutSetupDetection::Unsupported { .. }
                ),
                "query must fail closed: {query:?}"
            );
        }
    }

    #[test]
    fn gnome_pair_does_not_require_mru_to_confirm_setup() {
        let sources = flat_sources(&[("xkb", "us"), ("xkb", "ru")]);
        assert!(matches!(
            detect_gnome_setup_from_sources(&sources),
            LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
        ));
    }

    #[test]
    fn gnome_ibus_variant_and_missing_pair_are_unsupported() {
        for sources in [
            flat_sources(&[("ibus", "typing-booster"), ("xkb", "ru")]),
            flat_sources(&[("xkb", "us+dvorak"), ("xkb", "ru")]),
            flat_sources(&[("xkb", "us")]),
        ] {
            assert!(matches!(
                detect_gnome_setup_from_sources(&sources),
                LayoutSetupDetection::Unsupported { .. }
            ));
        }
    }

    #[test]
    fn gnome_extra_plain_source_is_pair_plus_other_but_malformed_is_not() {
        let extra = flat_sources(&[("xkb", "us"), ("xkb", "de"), ("xkb", "ru")]);
        assert!(matches!(
            detect_gnome_setup_from_sources(&extra),
            LayoutSetupDetection::Confirmed(LayoutSetup::PairPlusOther { .. })
        ));

        for sources in [
            vec!["xkb".to_string()],
            flat_sources(&[("xkb", "us"), ("xkb", "us"), ("xkb", "ru")]),
            flat_sources(&[("xkb", "US"), ("xkb", "ru")]),
        ] {
            assert!(matches!(
                detect_gnome_setup_from_sources(&sources),
                LayoutSetupDetection::Unsupported { .. }
            ));
        }
    }

    #[test]
    fn group_mapping_rejects_a_different_group_count() {
        let setup = test_strict_pair(LayoutCode::Us);
        assert!(matches!(
            current_layout_from_group(&setup, 0, 3),
            CurrentLayoutState::Unknown { .. }
        ));
    }

    #[test]
    fn pair_plus_other_maps_only_the_confirmed_index() {
        let setup = test_pair_plus_german();
        assert_eq!(
            current_layout_kind(&current_layout_from_group(&setup, 0, 3)),
            AppLayoutKind::English
        );
        assert_eq!(
            current_layout_kind(&current_layout_from_group(&setup, 1, 3)),
            AppLayoutKind::Other
        );
    }

    #[test]
    fn gnome_current_source_maps_only_the_confirmed_first_mru_source() {
        let configured = flat_sources(&[("xkb", "us"), ("xkb", "de"), ("xkb", "ru")]);
        let german_first = flat_sources(&[("xkb", "de"), ("xkb", "us"), ("xkb", "ru")]);
        assert_eq!(
            current_layout_kind(&current_layout_from_gnome_sources(
                &configured,
                &german_first,
            )),
            AppLayoutKind::Other
        );

        for mru in [Vec::new(), flat_sources(&[("ibus", "typing-booster")])] {
            assert!(matches!(
                current_layout_from_gnome_sources(&configured, &mru),
                CurrentLayoutState::Unknown { .. }
            ));
        }
    }

    #[test]
    fn unavailable_and_unsupported_outcomes_are_effectively_unsupported() {
        for detection in [
            LayoutSetupDetection::TemporarilyUnavailable {
                reason: "not-ready".to_string(),
            },
            LayoutSetupDetection::Unsupported {
                reason: "wrong-pair".to_string(),
            },
        ] {
            assert!(!detection.is_confirmed());
            assert!(matches!(
                detection.effective_setup(),
                LayoutSetup::Unsupported { .. }
            ));
        }
    }

    #[test]
    fn detector_uses_only_the_source_for_the_current_session() {
        let reader = SourceReaderStub::new();

        assert!(matches!(
            detect_layout_setup(
                SystemContext {
                    session_type: SessionType::X11,
                    desktop_environment: DesktopEnvironment::Cinnamon,
                    ..SystemContext::default()
                },
                &reader,
            ),
            LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
        ));
        assert_eq!(reader.setxkbmap_calls.get(), 1);
        assert_eq!(reader.gsettings_calls.get(), 0);

        assert!(matches!(
            detect_layout_setup(
                SystemContext {
                    session_type: SessionType::Wayland,
                    desktop_environment: DesktopEnvironment::Gnome,
                    ..SystemContext::default()
                },
                &reader,
            ),
            LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
        ));
        assert_eq!(reader.setxkbmap_calls.get(), 1);
        assert_eq!(reader.gsettings_calls.get(), 1);
    }

    #[test]
    fn detector_distinguishes_unavailable_source_from_unsupported_context() {
        let failing = SourceReaderStub {
            fail: true,
            ..SourceReaderStub::new()
        };
        assert!(matches!(
            detect_layout_setup(
                SystemContext {
                    session_type: SessionType::X11,
                    ..SystemContext::default()
                },
                &failing,
            ),
            LayoutSetupDetection::TemporarilyUnavailable { .. }
        ));
        assert!(matches!(
            detect_layout_setup(SystemContext::default(), &failing),
            LayoutSetupDetection::Unsupported { .. }
        ));
    }

    fn test_layout(code: LayoutCode, kind: AppLayoutKind, index: u32) -> SystemLayout {
        SystemLayout {
            backend_key: format!("test:{index}"),
            normalized_code: code,
            display_name: format!("{kind:?}"),
            kind,
            index: Some(index),
        }
    }

    fn test_strict_pair(english: LayoutCode) -> LayoutSetup {
        LayoutSetup::StrictPair {
            en: test_layout(english, AppLayoutKind::English, 0),
            ru: test_layout(LayoutCode::Ru, AppLayoutKind::Russian, 1),
        }
    }

    fn test_pair_plus_german() -> LayoutSetup {
        LayoutSetup::PairPlusOther {
            en: test_layout(LayoutCode::Us, AppLayoutKind::English, 0),
            ru: test_layout(LayoutCode::Ru, AppLayoutKind::Russian, 2),
            others: vec![test_layout(
                LayoutCode::from_normalized("de").unwrap(),
                AppLayoutKind::Other,
                1,
            )],
        }
    }

    fn flat_sources(values: &[(&str, &str)]) -> Vec<String> {
        values
            .iter()
            .flat_map(|(kind, id)| [(*kind).to_string(), (*id).to_string()])
            .collect()
    }

    fn current_layout_kind(state: &CurrentLayoutState) -> AppLayoutKind {
        match state {
            CurrentLayoutState::Known { layout, .. } => layout.kind,
            CurrentLayoutState::Unknown { .. } => AppLayoutKind::Unknown,
        }
    }

    struct SourceReaderStub {
        fail: bool,
        gsettings_calls: Cell<usize>,
        setxkbmap_calls: Cell<usize>,
    }

    impl SourceReaderStub {
        fn new() -> Self {
            Self {
                fail: false,
                gsettings_calls: Cell::new(0),
                setxkbmap_calls: Cell::new(0),
            }
        }
    }

    impl DesktopSettingsReader for SourceReaderStub {
        fn gsettings_string_list(
            &self,
            _schema: &str,
            _key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            self.gsettings_calls
                .set(self.gsettings_calls.get().saturating_add(1));
            if self.fail {
                return Err(LayoutAutoDetectError::GSettingsFailed {
                    stderr: "injected".to_string(),
                });
            }
            Ok(flat_sources(&[("xkb", "us"), ("xkb", "ru")]))
        }

        fn xfconf_string(
            &self,
            _channel: &str,
            _property: &str,
        ) -> Result<String, LayoutAutoDetectError> {
            Err(LayoutAutoDetectError::XfconfFailed {
                stderr: "unused".to_string(),
            })
        }

        fn xfconf_bool(
            &self,
            _channel: &str,
            _property: &str,
        ) -> Result<bool, LayoutAutoDetectError> {
            Err(LayoutAutoDetectError::XfconfFailed {
                stderr: "unused".to_string(),
            })
        }

        fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError> {
            self.setxkbmap_calls
                .set(self.setxkbmap_calls.get().saturating_add(1));
            if self.fail {
                return Err(LayoutAutoDetectError::SetXkbMapFailed {
                    stderr: "injected".to_string(),
                });
            }
            Ok("layout: us,ru\nvariant: ,\n".to_string())
        }
    }
}
