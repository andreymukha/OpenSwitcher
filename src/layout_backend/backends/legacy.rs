use crate::daemon::keyboard::is_russian_layout;
use crate::daemon::layout_switcher::{LayoutSwitcher, X11LayoutSwitcher};
use crate::layout_backend::{
    AppLayoutKind, BackendCapabilities, CurrentLayoutState, LayoutBackend, LayoutBackendError,
    LayoutBackendOperation, LayoutCode, LayoutSetup, LayoutStateSink, SystemLayout,
};
use crate::model::LayoutSwitchCombo;

pub fn legacy_backend_factory() -> Result<Box<dyn LayoutBackend>, LayoutBackendError> {
    Ok(Box::new(LegacyLayoutBackend))
}

struct LegacyLayoutBackend;

impl LegacyLayoutBackend {
    fn english_layout() -> SystemLayout {
        SystemLayout {
            backend_key: "legacy:english".to_string(),
            normalized_code: LayoutCode::Us,
            display_name: "English".to_string(),
            kind: AppLayoutKind::English,
            index: None,
        }
    }

    fn russian_layout() -> SystemLayout {
        SystemLayout {
            backend_key: "legacy:russian".to_string(),
            normalized_code: LayoutCode::Ru,
            display_name: "Russian".to_string(),
            kind: AppLayoutKind::Russian,
            index: None,
        }
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
            en: Self::english_layout(),
            ru: Self::russian_layout(),
        })
    }

    fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError> {
        match is_russian_layout() {
            Ok(true) => Ok(CurrentLayoutState::Known {
                layout: Self::russian_layout(),
                trustworthy: false,
            }),
            Ok(false) => Ok(CurrentLayoutState::Known {
                layout: Self::english_layout(),
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
