use super::{BackendCapabilities, LayoutCompatibility, LayoutSetup};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureAvailability {
    pub auto_switch: bool,
    pub manual_word_fix: bool,
    pub selected_text_switch: bool,
    pub reason: Option<String>,
}

pub fn compatibility_from_setup(setup: &LayoutSetup) -> LayoutCompatibility {
    match setup {
        LayoutSetup::StrictPair { .. } => LayoutCompatibility::FullStrictPair,
        LayoutSetup::PairPlusOther { .. } => LayoutCompatibility::PairPlusOther,
        LayoutSetup::Unsupported { .. } => LayoutCompatibility::Unsupported,
    }
}

pub fn feature_availability_for(
    compatibility: LayoutCompatibility,
    capabilities: BackendCapabilities,
) -> FeatureAvailability {
    match compatibility {
        LayoutCompatibility::FullStrictPair => {
            let auto_switch = capabilities.can_read_current_layout
                && (capabilities.can_switch_to_target || capabilities.can_switch_next)
                && capabilities.can_map_layouts_to_app_kinds;
            let manual_word_fix = (capabilities.can_switch_to_target
                || capabilities.can_switch_next)
                && capabilities.can_map_layouts_to_app_kinds;
            let selected_text_switch = true;
            let reason = (!auto_switch || !manual_word_fix).then(|| {
                "Backend does not provide enough capabilities for all EN/RU features.".to_string()
            });

            FeatureAvailability {
                auto_switch,
                manual_word_fix,
                selected_text_switch,
                reason,
            }
        }
        LayoutCompatibility::PairPlusOther => {
            let auto_switch = capabilities.can_read_current_layout
                && capabilities.can_switch_to_target
                && capabilities.can_map_layouts_to_app_kinds;
            let manual_word_fix = capabilities.can_read_current_layout
                && capabilities.can_switch_to_target
                && capabilities.can_map_layouts_to_app_kinds;
            let selected_text_switch = true;
            let reason = Some(
                "Only EN/RU-specific features are available, extra layouts are treated as Other."
                    .to_string(),
            );

            FeatureAvailability {
                auto_switch,
                manual_word_fix,
                selected_text_switch,
                reason,
            }
        }
        LayoutCompatibility::Limited => FeatureAvailability {
            auto_switch: false,
            manual_word_fix: false,
            selected_text_switch: true,
            reason: Some(
                "Backend is limited: automatic and manual layout-dependent features are disabled."
                    .to_string(),
            ),
        },
        LayoutCompatibility::Unsupported => FeatureAvailability {
            auto_switch: false,
            manual_word_fix: false,
            selected_text_switch: true,
            reason: Some(
                "Layout setup is unsupported for EN/RU automation on this backend.".to_string(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_backend::{AppLayoutKind, LayoutCode, SystemLayout};

    fn layout(kind: AppLayoutKind, code: LayoutCode) -> SystemLayout {
        SystemLayout {
            backend_key: "test".to_string(),
            normalized_code: code,
            display_name: "test".to_string(),
            kind,
            index: Some(0),
        }
    }

    #[test]
    fn strict_pair_setup_maps_to_full_strict_pair_compatibility() {
        let setup = LayoutSetup::StrictPair {
            en: layout(AppLayoutKind::English, LayoutCode::Us),
            ru: layout(AppLayoutKind::Russian, LayoutCode::Ru),
        };

        assert_eq!(
            compatibility_from_setup(&setup),
            LayoutCompatibility::FullStrictPair
        );
    }

    #[test]
    fn pair_plus_other_setup_maps_to_pair_plus_other_compatibility() {
        let setup = LayoutSetup::PairPlusOther {
            en: layout(AppLayoutKind::English, LayoutCode::Us),
            ru: layout(AppLayoutKind::Russian, LayoutCode::Ru),
            others: vec![layout(
                AppLayoutKind::Other,
                LayoutCode::from_normalized("de").unwrap(),
            )],
        };

        assert_eq!(
            compatibility_from_setup(&setup),
            LayoutCompatibility::PairPlusOther
        );
    }

    #[test]
    fn strict_pair_allows_next_switch_for_auto_switch() {
        let availability = feature_availability_for(
            LayoutCompatibility::FullStrictPair,
            BackendCapabilities {
                can_read_current_layout: true,
                can_switch_to_target: false,
                can_switch_next: true,
                can_map_layouts_to_app_kinds: true,
                ..Default::default()
            },
        );

        assert!(availability.auto_switch);
        assert!(availability.manual_word_fix);
        assert!(availability.selected_text_switch);
    }

    #[test]
    fn pair_plus_other_disables_manual_fix_without_target_switch() {
        let availability = feature_availability_for(
            LayoutCompatibility::PairPlusOther,
            BackendCapabilities {
                can_read_current_layout: true,
                can_switch_next: true,
                can_map_layouts_to_app_kinds: true,
                ..Default::default()
            },
        );

        assert!(!availability.auto_switch);
        assert!(!availability.manual_word_fix);
        assert!(availability.selected_text_switch);
    }

    #[test]
    fn unsupported_setup_disables_layout_dependent_features() {
        let availability = feature_availability_for(
            LayoutCompatibility::Unsupported,
            BackendCapabilities::default(),
        );

        assert!(!availability.auto_switch);
        assert!(!availability.manual_word_fix);
        assert!(availability.selected_text_switch);
    }
}
