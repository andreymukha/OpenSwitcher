use crate::daemon::keyboard::is_russian_layout;
use crate::daemon::layout_switcher::{LayoutSwitcher, X11LayoutSwitcher};
use crate::layout_backend::{
    detect_layout_setup, BackendCapabilities, CurrentLayoutState, LayoutBackend,
    LayoutBackendError, LayoutBackendOperation, LayoutSetup, LayoutSetupDetection, LayoutStateSink,
    SystemLayout,
};
use crate::layout_switch::{CommandDesktopSettingsReader, DesktopSettingsReader};
use crate::model::{LayoutSwitchCombo, SystemContext};
use std::sync::RwLock;

pub fn legacy_backend_factory() -> Result<Box<dyn LayoutBackend>, LayoutBackendError> {
    Ok(Box::new(LegacyLayoutBackend::new()))
}

struct LegacyLayoutBackend {
    setup: RwLock<LayoutSetup>,
}

impl LegacyLayoutBackend {
    fn new() -> Self {
        Self {
            setup: RwLock::new(LayoutSetup::Unsupported {
                reason: "layout-setup-not-detected".to_string(),
            }),
        }
    }

    fn detect_setup_with_reader<R: DesktopSettingsReader + ?Sized>(
        &self,
        context: SystemContext,
        reader: &R,
    ) -> LayoutSetupDetection {
        let detection = detect_layout_setup(context, reader);
        *self
            .setup
            .write()
            .unwrap_or_else(|error| error.into_inner()) = detection.effective_setup();
        detection
    }

    fn cached_setup(&self) -> LayoutSetup {
        self.setup
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn cached_pair(&self) -> Option<(SystemLayout, SystemLayout)> {
        match self.cached_setup() {
            LayoutSetup::StrictPair { en, ru } | LayoutSetup::PairPlusOther { en, ru, .. } => {
                Some((en, ru))
            }
            LayoutSetup::Unsupported { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LayoutAutoDetectError;
    use crate::layout_backend::LayoutSetupDetection;
    use crate::layout_switch::DesktopSettingsReader;
    use crate::model::{DesktopEnvironment, SessionType, SystemContext};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn legacy_backend_never_installs_default_pair_after_detection_failure() {
        let backend = LegacyLayoutBackend::new();
        let detection =
            backend.detect_setup_with_reader(x11_context(), &SetupReaderStub::failing());

        assert!(matches!(
            detection,
            LayoutSetupDetection::TemporarilyUnavailable { .. }
        ));
        assert!(matches!(
            backend.cached_setup(),
            LayoutSetup::Unsupported { .. }
        ));
    }

    #[test]
    fn x11_backend_uses_setxkbmap_and_caches_the_confirmed_pair() {
        let reader = SetupReaderStub::x11_us_ru();
        let backend = LegacyLayoutBackend::new();

        assert!(matches!(
            backend.detect_setup_with_reader(x11_context(), &reader),
            LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
        ));
        assert!(matches!(
            backend.cached_setup(),
            LayoutSetup::StrictPair { .. }
        ));
        assert_eq!(reader.gsettings_calls(), 0);
        assert_eq!(reader.setxkbmap_calls(), 1);
    }

    #[test]
    fn gnome_wayland_backend_uses_gsettings_not_setxkbmap() {
        let reader = SetupReaderStub::gnome_us_ru();
        let backend = LegacyLayoutBackend::new();

        assert!(matches!(
            backend.detect_setup_with_reader(gnome_wayland_context(), &reader),
            LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
        ));
        assert_eq!(reader.gsettings_calls(), 1);
        assert_eq!(reader.setxkbmap_calls(), 0);
    }

    fn x11_context() -> SystemContext {
        SystemContext {
            session_type: SessionType::X11,
            desktop_environment: DesktopEnvironment::Cinnamon,
            ..SystemContext::default()
        }
    }

    fn gnome_wayland_context() -> SystemContext {
        SystemContext {
            session_type: SessionType::Wayland,
            desktop_environment: DesktopEnvironment::Gnome,
            ..SystemContext::default()
        }
    }

    struct SetupReaderStub {
        gnome_sources: Option<Vec<String>>,
        x11_query: Option<String>,
        gsettings_calls: AtomicUsize,
        setxkbmap_calls: AtomicUsize,
    }

    impl SetupReaderStub {
        fn failing() -> Self {
            Self {
                gnome_sources: None,
                x11_query: None,
                gsettings_calls: AtomicUsize::new(0),
                setxkbmap_calls: AtomicUsize::new(0),
            }
        }

        fn x11_us_ru() -> Self {
            Self {
                x11_query: Some("layout: us,ru\nvariant: ,\n".to_string()),
                ..Self::failing()
            }
        }

        fn gnome_us_ru() -> Self {
            Self {
                gnome_sources: Some(vec!["xkb".into(), "us".into(), "xkb".into(), "ru".into()]),
                ..Self::failing()
            }
        }

        fn gsettings_calls(&self) -> usize {
            self.gsettings_calls.load(Ordering::SeqCst)
        }

        fn setxkbmap_calls(&self) -> usize {
            self.setxkbmap_calls.load(Ordering::SeqCst)
        }
    }

    impl DesktopSettingsReader for SetupReaderStub {
        fn gsettings_string_list(
            &self,
            _schema: &str,
            _key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            self.gsettings_calls.fetch_add(1, Ordering::SeqCst);
            self.gnome_sources
                .clone()
                .ok_or_else(|| LayoutAutoDetectError::GSettingsFailed {
                    stderr: "injected unavailable".to_string(),
                })
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
            self.setxkbmap_calls.fetch_add(1, Ordering::SeqCst);
            self.x11_query
                .clone()
                .ok_or_else(|| LayoutAutoDetectError::SetXkbMapFailed {
                    stderr: "injected unavailable".to_string(),
                })
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

    fn detect_setup(&self, context: SystemContext) -> LayoutSetupDetection {
        self.detect_setup_with_reader(context, &CommandDesktopSettingsReader)
    }

    fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError> {
        let Some((en, ru)) = self.cached_pair() else {
            return Ok(CurrentLayoutState::Unknown {
                reason: "legacy-layout-setup-unconfirmed".to_string(),
            });
        };
        match is_russian_layout() {
            Ok(true) => Ok(CurrentLayoutState::Known {
                layout: ru,
                trustworthy: false,
            }),
            Ok(false) => Ok(CurrentLayoutState::Known {
                layout: en,
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
