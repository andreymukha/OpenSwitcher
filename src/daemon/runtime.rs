use crate::config::AppConfig;
use crate::daemon::capture::LayoutSwitchCaptureSession;
use crate::error::{CaptureError, ConfigError, SettingsError};
use crate::layout_backend::{
    compatibility_from_setup, feature_availability_for, legacy_backend_factory,
    legacy_current_layout_bool, legacy_layout_state_from_bool, BackendCapabilities,
    CurrentLayoutState, FeatureAvailability, LayoutBackend, LayoutBackendRegistry,
    LayoutBackendRegistryResult, LayoutCompatibility, LayoutSetup,
};
use crate::layout_switch::{
    failed_detection_fallback, DesktopSettingsReader, LayoutSwitchAutoDetector,
};
use crate::model::SystemContext;
use crate::model::{
    LayoutSwitchCaptureState, LayoutSwitchCombo, SelectedTextHotkey, Settings, UndoKey,
    UpdateSettingsResult,
};
use crate::system::SystemContextDetector;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

const LAYOUT_DEBUG_ENV: &str = "OPEN_SWITCHER_LAYOUT_DEBUG";
const LAYOUT_DEBUG_FILE_ENV: &str = "OPEN_SWITCHER_LAYOUT_DEBUG_FILE";
const BACKGROUND_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Clone, Debug)]
pub struct RuntimeConfigSnapshot {
    pub layout_switch_combo: LayoutSwitchCombo,
    pub layout_delay_ms: u64,
    pub backspace_ms: u64,
    pub typing_ms: u64,
    pub undo_key: UndoKey,
    pub selected_text_hotkey: SelectedTextHotkey,
}

impl From<&AppConfig> for RuntimeConfigSnapshot {
    fn from(value: &AppConfig) -> Self {
        Self {
            layout_switch_combo: value.layout.switch_combo,
            layout_delay_ms: value.layout.delay_ms as u64,
            backspace_ms: value.delays.backspace_ms as u64,
            typing_ms: value.delays.typing_ms as u64,
            undo_key: value.features.undo_key,
            selected_text_hotkey: value.features.selected_text_switch_hotkey,
        }
    }
}

pub struct ConfigService {
    config_path: PathBuf,
    inner: RwLock<AppConfig>,
}

impl ConfigService {
    pub fn load(config_path: PathBuf) -> Result<Self, ConfigError> {
        let context = SystemContextDetector::detect_current()?;
        let detector = LayoutSwitchAutoDetector::new();
        Self::load_with_context_and_detector(config_path, context, &detector)
    }

    fn load_with_context_and_detector<R: DesktopSettingsReader>(
        config_path: PathBuf,
        context: SystemContext,
        detector: &LayoutSwitchAutoDetector<R>,
    ) -> Result<Self, ConfigError> {
        let mut config = AppConfig::load_or_create(&config_path)?;

        let should_detect = !matches!(
            config.layout.switch_source,
            crate::model::LayoutSwitchSource::Manual
        );

        if should_detect {
            let detected = match detector.detect(context) {
                Ok(detected) => detected,
                Err(error) => {
                    eprintln!(
                        "[config] Failed to auto-detect layout switch combo, using fallback: {error}"
                    );
                    failed_detection_fallback(context)
                }
            };
            if config.settings().layout_switch != detected {
                config.layout.switch_combo = detected.combo;
                config.layout.switch_source = detected.source;
                config.layout.auto_detected = detected.auto_detected;
                config.save_to_path(&config_path)?;
            }
        }

        Ok(Self {
            config_path,
            inner: RwLock::new(config),
        })
    }

    pub fn load_current(&self) -> Result<AppConfig, SettingsError> {
        self.inner
            .read()
            .map(|config| config.clone())
            .map_err(|_| SettingsError::LockPoisoned)
    }

    pub fn get_settings(&self) -> Result<Settings, SettingsError> {
        self.inner
            .read()
            .map(|config| config.settings())
            .map_err(|_| SettingsError::LockPoisoned)
    }

    pub fn update_settings(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsError> {
        let settings = settings.validate()?;
        let mut config = self
            .inner
            .write()
            .map_err(|_| SettingsError::LockPoisoned)?;
        let mut updated = config.clone();
        updated.apply_settings(settings);
        updated
            .save_to_path(&self.config_path)
            .map_err(SettingsError::SaveFailed)?;
        *config = updated;

        Ok(UpdateSettingsResult {
            message: "Настройки сохранены и применены без перезапуска.".to_string(),
            restart_required: false,
        })
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let config = self.inner.read().map_err(|_| ConfigError::LockPoisoned)?;
        config.save_to_path(&self.config_path)
    }

    pub fn snapshot(&self) -> Result<RuntimeConfigSnapshot, SettingsError> {
        self.inner
            .read()
            .map(|config| RuntimeConfigSnapshot::from(&*config))
            .map_err(|_| SettingsError::LockPoisoned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LayoutAutoDetectError;
    use crate::layout_backend::{
        AppLayoutKind, BackendCapabilities, CurrentLayoutState, LayoutBackendError,
        LayoutBackendOperation, LayoutCode, LayoutStateSink, SystemLayout,
    };
    use crate::model::{
        AutoDetectedLayoutSwitch, DesktopEnvironment, DetectionConfidence, DetectionStrategy,
        DistroKind, LayoutSwitchSetting, LayoutSwitchSource, SessionType,
    };
    use std::io;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tempfile::TempDir;

    #[derive(Clone)]
    struct CountingReader {
        calls: Arc<AtomicUsize>,
        combo: LayoutSwitchCombo,
    }

    impl DesktopSettingsReader for CountingReader {
        fn gsettings_string_list(
            &self,
            _schema: &str,
            _key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![self.combo.xkb_option().to_string()])
        }

        fn xfconf_string(
            &self,
            _channel: &str,
            _property: &str,
        ) -> Result<String, LayoutAutoDetectError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.combo.xkb_option().to_string())
        }

        fn xfconf_bool(
            &self,
            _channel: &str,
            _property: &str,
        ) -> Result<bool, LayoutAutoDetectError> {
            Ok(false)
        }

        fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!(
                "rules: evdev\noptions:    {},grp_led:scroll\n",
                self.combo.xkb_option()
            ))
        }
    }

    fn cinnamon_x11_context() -> SystemContext {
        SystemContext {
            session_type: SessionType::X11,
            desktop_environment: DesktopEnvironment::Cinnamon,
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

    enum SnapshotOutcome {
        State(CurrentLayoutState),
        Error,
    }

    struct SnapshotBackend {
        snapshot: SnapshotOutcome,
    }

    impl LayoutBackend for SnapshotBackend {
        fn id(&self) -> &'static str {
            "test-backend"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        fn detect_setup(&self) -> Result<LayoutSetup, LayoutBackendError> {
            Err(LayoutBackendError::unsupported(
                self.id(),
                LayoutBackendOperation::DetectSetup,
            ))
        }

        fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError> {
            match &self.snapshot {
                SnapshotOutcome::State(snapshot) => Ok(snapshot.clone()),
                SnapshotOutcome::Error => Err(LayoutBackendError::runtime(
                    self.id(),
                    LayoutBackendOperation::CurrentLayoutSnapshot,
                    io::Error::other("backend down"),
                )),
            }
        }

        fn switch_to(
            &mut self,
            _target: &crate::layout_backend::SystemLayout,
        ) -> Result<(), LayoutBackendError> {
            Err(LayoutBackendError::unsupported(
                self.id(),
                LayoutBackendOperation::SwitchTo,
            ))
        }

        fn switch_next(&mut self) -> Result<(), LayoutBackendError> {
            Err(LayoutBackendError::unsupported(
                self.id(),
                LayoutBackendOperation::SwitchNext,
            ))
        }

        fn start_monitoring(&mut self, _sink: LayoutStateSink) -> Result<(), LayoutBackendError> {
            Err(LayoutBackendError::unsupported(
                self.id(),
                LayoutBackendOperation::StartMonitoring,
            ))
        }
    }

    fn english_layout() -> SystemLayout {
        SystemLayout {
            backend_key: "us".to_string(),
            normalized_code: LayoutCode::Us,
            display_name: "English".to_string(),
            kind: AppLayoutKind::English,
            index: Some(0),
        }
    }

    fn russian_layout() -> SystemLayout {
        SystemLayout {
            backend_key: "ru".to_string(),
            normalized_code: LayoutCode::Ru,
            display_name: "Russian".to_string(),
            kind: AppLayoutKind::Russian,
            index: Some(1),
        }
    }

    fn known_layout_state(layout: SystemLayout) -> CurrentLayoutState {
        CurrentLayoutState::Known {
            layout,
            trustworthy: true,
        }
    }

    fn test_runtime_with_backend(
        initial_layout_state: CurrentLayoutState,
        backend: Box<dyn LayoutBackend>,
    ) -> RuntimeState {
        RuntimeState {
            enabled: AtomicBool::new(true),
            layout_state: RwLock::new(initial_layout_state),
            backend: Mutex::new(Some(backend)),
            layout_setup: RwLock::new(LayoutSetup::Unsupported {
                reason: "test".to_string(),
            }),
            layout_compatibility: RwLock::new(LayoutCompatibility::Unsupported),
            feature_availability: RwLock::new(FeatureAvailability {
                auto_switch: false,
                manual_word_fix: false,
                selected_text_switch: true,
                reason: Some("test".to_string()),
            }),
            config_service: ConfigService {
                config_path: PathBuf::from("test-config.toml"),
                inner: RwLock::new(AppConfig::default()),
            },
            capture_session: Mutex::new(LayoutSwitchCaptureSession::default()),
            background_sync_started: AtomicBool::new(false),
            pending_status_change: AtomicBool::new(false),
        }
    }

    #[test]
    fn detects_combo_on_first_load_and_persists_it() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let calls = Arc::new(AtomicUsize::new(0));
        let detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&calls),
            combo: LayoutSwitchCombo::alt_shift(),
        });

        let service = ConfigService::load_with_context_and_detector(
            path.clone(),
            cinnamon_x11_context(),
            &detector,
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            service.get_settings().unwrap().layout_switch.combo,
            LayoutSwitchCombo::alt_shift()
        );

        let persisted = AppConfig::load_or_create(&path).unwrap();
        assert_eq!(
            persisted.layout.switch_source,
            LayoutSwitchSource::AutoDetected
        );
        assert_eq!(
            persisted.layout.switch_combo,
            LayoutSwitchCombo::alt_shift()
        );
    }

    #[test]
    fn rechecks_auto_detected_combo_when_context_did_not_change() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let calls = Arc::new(AtomicUsize::new(0));
        let detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&calls),
            combo: LayoutSwitchCombo::ctrl_shift(),
        });

        ConfigService::load_with_context_and_detector(
            path.clone(),
            cinnamon_x11_context(),
            &detector,
        )
        .unwrap();
        ConfigService::load_with_context_and_detector(path, cinnamon_x11_context(), &detector)
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn redetects_when_auto_detected_context_changes() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let calls = Arc::new(AtomicUsize::new(0));
        let detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&calls),
            combo: LayoutSwitchCombo::ctrl_shift(),
        });

        ConfigService::load_with_context_and_detector(
            path.clone(),
            cinnamon_x11_context(),
            &detector,
        )
        .unwrap();
        ConfigService::load_with_context_and_detector(
            path.clone(),
            gnome_wayland_context(),
            &detector,
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let persisted = AppConfig::load_or_create(&path).unwrap();
        assert_eq!(
            persisted.layout.switch_source,
            LayoutSwitchSource::AutoFallback
        );
        assert_eq!(
            persisted.layout.auto_detected,
            AutoDetectedLayoutSwitch {
                strategy: DetectionStrategy::NoSupportedStrategy,
                confidence: DetectionConfidence::Unsupported,
                context: gnome_wayland_context(),
            }
        );
    }

    #[test]
    fn preserves_manual_choice_even_when_context_changes() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.layout.switch_combo = LayoutSwitchCombo::caps_lock();
        config.layout.switch_source = LayoutSwitchSource::Manual;
        config.save_to_path(&path).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&calls),
            combo: LayoutSwitchCombo::alt_shift(),
        });

        let service =
            ConfigService::load_with_context_and_detector(path, cinnamon_x11_context(), &detector)
                .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            service.get_settings().unwrap().layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::caps_lock(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            }
        );
    }

    #[test]
    fn refreshes_auto_detected_combo_when_system_combo_changes_without_context_change() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        let first_calls = Arc::new(AtomicUsize::new(0));
        let first_detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&first_calls),
            combo: LayoutSwitchCombo::ctrl_shift(),
        });

        ConfigService::load_with_context_and_detector(
            path.clone(),
            cinnamon_x11_context(),
            &first_detector,
        )
        .unwrap();
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);

        let second_calls = Arc::new(AtomicUsize::new(0));
        let second_detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&second_calls),
            combo: LayoutSwitchCombo::alt_shift(),
        });

        let service = ConfigService::load_with_context_and_detector(
            path.clone(),
            cinnamon_x11_context(),
            &second_detector,
        )
        .unwrap();

        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            service.get_settings().unwrap().layout_switch.combo,
            LayoutSwitchCombo::alt_shift()
        );
        let persisted = AppConfig::load_or_create(&path).unwrap();
        assert_eq!(
            persisted.layout.switch_combo,
            LayoutSwitchCombo::alt_shift()
        );
    }

    #[test]
    fn rejects_legacy_config_in_runtime_path() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"[layout]
keys = ["LeftControl", "LeftShift"]
delay_ms = 30

[delays]
backspace_ms = 0
typing_ms = 0

[features]
undo_key = "Pause"
"#,
        )
        .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let detector = LayoutSwitchAutoDetector::with_reader(CountingReader {
            calls: Arc::clone(&calls),
            combo: LayoutSwitchCombo::alt_shift(),
        });

        let context = SystemContext {
            session_type: SessionType::X11,
            desktop_environment: DesktopEnvironment::Xfce,
            distro: DistroKind::LinuxMint,
        };

        let error =
            match ConfigService::load_with_context_and_detector(path.clone(), context, &detector) {
                Ok(_) => panic!("legacy config must be rejected in dev runtime path"),
                Err(error) => error,
            };

        assert!(matches!(error, ConfigError::LegacyFormatUnsupported));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("keys = ["));
    }

    #[test]
    fn sync_with_backend_reports_update_and_replaces_cached_state() {
        let initial = known_layout_state(english_layout());
        let expected = known_layout_state(russian_layout());
        let runtime = test_runtime_with_backend(
            initial.clone(),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(expected.clone()),
            }),
        );

        let outcome = runtime.sync_with_backend();

        assert_eq!(
            outcome,
            BackendSyncResult::Updated {
                previous: initial,
                current: expected.clone(),
            }
        );
        assert_eq!(runtime.current_layout_state(), expected);
    }

    #[test]
    fn sync_with_backend_reports_unchanged_when_snapshot_matches_cache() {
        let initial = known_layout_state(english_layout());
        let runtime = test_runtime_with_backend(
            initial.clone(),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(initial.clone()),
            }),
        );

        let outcome = runtime.sync_with_backend();

        assert_eq!(outcome, BackendSyncResult::Unchanged);
        assert_eq!(runtime.current_layout_state(), initial);
    }

    #[test]
    fn sync_with_backend_reports_skipped_and_preserves_state_on_backend_error() {
        let initial = known_layout_state(english_layout());
        let runtime = test_runtime_with_backend(
            initial.clone(),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::Error,
            }),
        );

        let outcome = runtime.sync_with_backend();

        assert_eq!(outcome, BackendSyncResult::Skipped);
        assert_eq!(runtime.current_layout_state(), initial);
    }

    #[test]
    fn periodic_sync_tick_delegates_to_backend_sync() {
        let initial = known_layout_state(english_layout());
        let expected = known_layout_state(russian_layout());
        let runtime = test_runtime_with_backend(
            initial.clone(),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(expected.clone()),
            }),
        );

        let outcome = runtime.periodic_sync_tick();

        assert_eq!(
            outcome,
            BackendSyncResult::Updated {
                previous: initial,
                current: expected.clone(),
            }
        );
        assert_eq!(runtime.current_layout_state(), expected);
    }

    #[test]
    fn background_sync_polling_is_enabled_only_for_non_observing_backends() {
        assert!(background_sync_polling_enabled(BackendCapabilities {
            can_read_current_layout: true,
            can_observe_layout_changes: false,
            ..Default::default()
        }));

        assert!(!background_sync_polling_enabled(BackendCapabilities {
            can_read_current_layout: true,
            can_observe_layout_changes: true,
            ..Default::default()
        }));

        assert!(!background_sync_polling_enabled(BackendCapabilities {
            can_read_current_layout: false,
            can_observe_layout_changes: false,
            ..Default::default()
        }));
    }

    #[test]
    fn sync_with_backend_marks_status_change_pending_only_for_updates() {
        let initial = known_layout_state(english_layout());
        let expected = known_layout_state(russian_layout());
        let runtime = test_runtime_with_backend(
            initial,
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(expected),
            }),
        );

        assert!(!runtime.take_pending_status_change());
        assert!(matches!(
            runtime.sync_with_backend(),
            BackendSyncResult::Updated { .. }
        ));
        assert!(runtime.take_pending_status_change());
        assert!(!runtime.take_pending_status_change());
    }

    #[test]
    fn unchanged_sync_does_not_mark_status_change_pending() {
        let initial = known_layout_state(english_layout());
        let runtime = test_runtime_with_backend(
            initial.clone(),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(initial),
            }),
        );

        assert_eq!(runtime.sync_with_backend(), BackendSyncResult::Unchanged);
        assert!(!runtime.take_pending_status_change());
    }
}

pub struct RuntimeState {
    enabled: AtomicBool,
    layout_state: RwLock<CurrentLayoutState>,
    backend: Mutex<Option<Box<dyn LayoutBackend>>>,
    layout_setup: RwLock<LayoutSetup>,
    layout_compatibility: RwLock<LayoutCompatibility>,
    feature_availability: RwLock<FeatureAvailability>,
    config_service: ConfigService,
    capture_session: Mutex<LayoutSwitchCaptureSession>,
    background_sync_started: AtomicBool,
    pending_status_change: AtomicBool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendSyncResult {
    Updated {
        previous: CurrentLayoutState,
        current: CurrentLayoutState,
    },
    Unchanged,
    Skipped,
}

impl RuntimeState {
    pub fn new(config_service: ConfigService) -> Self {
        let (backend, layout_state, layout_setup, layout_compatibility, feature_availability) =
            Self::initialize_layout_backend();

        Self {
            enabled: AtomicBool::new(true),
            layout_state: RwLock::new(layout_state),
            backend: Mutex::new(backend),
            layout_setup: RwLock::new(layout_setup),
            layout_compatibility: RwLock::new(layout_compatibility),
            feature_availability: RwLock::new(feature_availability),
            config_service,
            capture_session: Mutex::new(LayoutSwitchCaptureSession::default()),
            background_sync_started: AtomicBool::new(false),
            pending_status_change: AtomicBool::new(false),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn toggle_enabled(&self) -> bool {
        let enabled = !self.enabled.load(Ordering::SeqCst);
        self.enabled.store(enabled, Ordering::SeqCst);
        enabled
    }

    pub fn current_layout(&self) -> bool {
        let state = self
            .layout_state
            .read()
            .unwrap_or_else(|error| error.into_inner());
        legacy_current_layout_bool(&state)
    }

    pub fn set_layout(&self, layout_is_english: bool) {
        self.set_layout_with_reason(layout_is_english, "unspecified");
    }

    pub fn set_layout_with_reason(&self, layout_is_english: bool, reason: &str) {
        let mut state = self
            .layout_state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let previous = legacy_current_layout_bool(&state);
        let next_state = legacy_layout_state_from_bool(layout_is_english);
        if *state != next_state {
            *state = next_state;
        }
        log_layout_debug(
            "set-layout",
            &format!(
                "reason={reason} previous={} next={}",
                layout_label(previous),
                layout_label(layout_is_english)
            ),
        );
    }

    pub fn current_layout_state(&self) -> CurrentLayoutState {
        self.layout_state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn layout_setup(&self) -> LayoutSetup {
        self.layout_setup
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn layout_compatibility(&self) -> LayoutCompatibility {
        *self
            .layout_compatibility
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn feature_availability(&self) -> FeatureAvailability {
        self.feature_availability
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn sync_with_backend(&self) -> BackendSyncResult {
        let snapshot = {
            let mut backend_guard = self
                .backend
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(backend) = backend_guard.as_mut() else {
                return BackendSyncResult::Skipped;
            };

            match backend.current_layout_snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    log_layout_debug("backend-sync-skip", &format!("error={error}"));
                    return BackendSyncResult::Skipped;
                }
            }
        };

        let mut state = self
            .layout_state
            .write()
            .unwrap_or_else(|error| error.into_inner());

        if *state == snapshot {
            return BackendSyncResult::Unchanged;
        }

        let previous = state.clone();
        *state = snapshot.clone();
        self.pending_status_change.store(true, Ordering::SeqCst);
        log_layout_debug(
            "backend-sync-update",
            &format!("previous={previous:?} next={snapshot:?}"),
        );
        BackendSyncResult::Updated {
            previous,
            current: snapshot,
        }
    }

    pub fn periodic_sync_tick(&self) -> BackendSyncResult {
        self.sync_with_backend()
    }

    pub fn take_pending_status_change(&self) -> bool {
        self.pending_status_change.swap(false, Ordering::SeqCst)
    }

    pub fn clear_pending_status_change(&self) {
        self.pending_status_change.store(false, Ordering::SeqCst);
    }

    pub fn start_background_sync_polling(self: &Arc<Self>) {
        let capabilities = {
            let backend_guard = self
                .backend
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(backend) = backend_guard.as_ref() else {
                return;
            };
            backend.capabilities()
        };

        if !background_sync_polling_enabled(capabilities) {
            return;
        }

        if self
            .background_sync_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let runtime = Arc::clone(self);
        if let Err(error) = thread::Builder::new()
            .name("layout-backend-poll".to_string())
            .spawn(move || loop {
                thread::sleep(BACKGROUND_SYNC_POLL_INTERVAL);
                let _ = runtime.periodic_sync_tick();
            })
        {
            self.background_sync_started.store(false, Ordering::SeqCst);
            log_layout_debug(
                "background-sync-start",
                &format!("failed=true error={error}"),
            );
            return;
        }

        log_layout_debug(
            "background-sync-start",
            &format!(
                "enabled=true interval_ms={}",
                BACKGROUND_SYNC_POLL_INTERVAL.as_millis()
            ),
        );
    }

    pub fn get_settings(&self) -> Result<Settings, SettingsError> {
        self.config_service.get_settings()
    }

    pub fn update_settings(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsError> {
        self.config_service.update_settings(settings)
    }

    pub fn config_snapshot(&self) -> Result<RuntimeConfigSnapshot, SettingsError> {
        self.config_service.snapshot()
    }

    pub fn start_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.start())
    }

    pub fn cancel_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.cancel())
    }

    pub fn finish_layout_switch_capture(&self) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.finish())
    }

    pub fn layout_switch_capture_state(&self) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.current_state())
    }

    pub fn is_capture_active(&self) -> Result<bool, CaptureError> {
        let session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.is_active())
    }

    pub fn handle_capture_key_event(
        &self,
        key: evdev::Key,
        value: i32,
    ) -> Result<Option<LayoutSwitchCaptureState>, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.handle_key_event(key, value))
    }

    fn initialize_layout_backend() -> (
        Option<Box<dyn LayoutBackend>>,
        CurrentLayoutState,
        LayoutSetup,
        LayoutCompatibility,
        FeatureAvailability,
    ) {
        let mut registry = LayoutBackendRegistry::new();
        registry.register_factory(legacy_backend_factory);

        match registry.pick_backend() {
            LayoutBackendRegistryResult::Backend(backend) => {
                let capabilities = backend.capabilities();
                let layout_setup = match backend.detect_setup() {
                    Ok(setup) => setup,
                    Err(error) => LayoutSetup::Unsupported {
                        reason: error.to_string(),
                    },
                };
                let layout_compatibility = compatibility_from_setup(&layout_setup);
                let feature_availability =
                    feature_availability_for(layout_compatibility, capabilities);
                let layout_state = match backend.current_layout_snapshot() {
                    Ok(state) => state,
                    Err(error) => CurrentLayoutState::Unknown {
                        reason: error.to_string(),
                    },
                };

                (
                    Some(backend),
                    layout_state,
                    layout_setup,
                    layout_compatibility,
                    feature_availability,
                )
            }
            LayoutBackendRegistryResult::Unsupported { reason } => {
                let layout_state = CurrentLayoutState::Unknown {
                    reason: reason.clone(),
                };
                let layout_setup = LayoutSetup::Unsupported {
                    reason: reason.clone(),
                };
                let layout_compatibility = compatibility_from_setup(&layout_setup);
                let feature_availability =
                    feature_availability_for(layout_compatibility, Default::default());

                (
                    None,
                    layout_state,
                    layout_setup,
                    layout_compatibility,
                    feature_availability,
                )
            }
        }
    }
}

fn background_sync_polling_enabled(capabilities: BackendCapabilities) -> bool {
    capabilities.can_read_current_layout && !capabilities.can_observe_layout_changes
}

pub(crate) fn log_layout_debug(stage: &str, details: &str) {
    if !layout_debug_enabled() {
        return;
    }

    let line = format!("[layout-debug] stage={stage} {details}");
    eprintln!("{line}");

    let path = env::var(LAYOUT_DEBUG_FILE_ENV)
        .unwrap_or_else(|_| "/tmp/open-switcher-layout-debug.log".to_string());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn layout_debug_enabled() -> bool {
    matches!(
        env::var(LAYOUT_DEBUG_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

fn layout_label(is_english: bool) -> &'static str {
    if is_english {
        "EN"
    } else {
        "RU"
    }
}
