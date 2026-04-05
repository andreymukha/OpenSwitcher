use super::{AppLayoutKind, CurrentLayoutState};

pub const LEGACY_LAYOUT_FALLBACK_IS_ENGLISH: bool = true;

pub fn legacy_layout_state_from_bool(layout_is_english: bool) -> CurrentLayoutState {
    CurrentLayoutState::Known {
        layout: crate::layout_backend::SystemLayout {
            backend_key: if layout_is_english {
                "legacy-compat:english".to_string()
            } else {
                "legacy-compat:russian".to_string()
            },
            normalized_code: if layout_is_english {
                crate::layout_backend::LayoutCode::Us
            } else {
                crate::layout_backend::LayoutCode::Ru
            },
            display_name: if layout_is_english {
                "English".to_string()
            } else {
                "Russian".to_string()
            },
            kind: if layout_is_english {
                AppLayoutKind::English
            } else {
                AppLayoutKind::Russian
            },
            index: None,
        },
        trustworthy: true,
    }
}

pub fn legacy_current_layout_bool(state: &CurrentLayoutState) -> bool {
    match state {
        CurrentLayoutState::Known { layout, .. } => match layout.kind {
            AppLayoutKind::English => true,
            AppLayoutKind::Russian => false,
            AppLayoutKind::Other | AppLayoutKind::Unknown => LEGACY_LAYOUT_FALLBACK_IS_ENGLISH,
        },
        CurrentLayoutState::Unknown { .. } => LEGACY_LAYOUT_FALLBACK_IS_ENGLISH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_backend::{LayoutCode, SystemLayout};

    fn state(kind: AppLayoutKind) -> CurrentLayoutState {
        CurrentLayoutState::Known {
            layout: SystemLayout {
                backend_key: "backend".to_string(),
                normalized_code: LayoutCode::Unknown,
                display_name: "layout".to_string(),
                kind,
                index: None,
            },
            trustworthy: true,
        }
    }

    #[test]
    fn compatibility_bridge_maps_english_and_russian_to_legacy_bool() {
        assert!(legacy_current_layout_bool(&state(AppLayoutKind::English)));
        assert!(!legacy_current_layout_bool(&state(AppLayoutKind::Russian)));
    }

    #[test]
    fn compatibility_bridge_can_build_legacy_layout_state_from_bool() {
        let english = legacy_layout_state_from_bool(true);
        let russian = legacy_layout_state_from_bool(false);

        assert!(legacy_current_layout_bool(&english));
        assert!(!legacy_current_layout_bool(&russian));
    }

    #[test]
    fn compatibility_bridge_uses_fallback_for_other_and_unknown() {
        assert_eq!(
            legacy_current_layout_bool(&state(AppLayoutKind::Other)),
            LEGACY_LAYOUT_FALLBACK_IS_ENGLISH
        );
        assert_eq!(
            legacy_current_layout_bool(&CurrentLayoutState::Unknown {
                reason: "no data".to_string(),
            }),
            LEGACY_LAYOUT_FALLBACK_IS_ENGLISH
        );
    }
}
