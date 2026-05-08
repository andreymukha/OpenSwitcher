use crate::config::AppConfig;
use crate::daemon::capture::LayoutSwitchCaptureSession;
use crate::error::{
    CaptureError, ConfigError, ServiceManagerError, SettingsError, SystemContextError,
};
use crate::layout_backend::{
    compatibility_from_setup, feature_availability_for, legacy_backend_factory,
    legacy_current_layout_bool, legacy_layout_state_from_bool, AppLayoutKind, BackendCapabilities,
    CurrentLayoutState, FeatureAvailability, LayoutBackend, LayoutBackendRegistry,
    LayoutBackendRegistryResult, LayoutCode, LayoutCompatibility, LayoutSetup, SystemLayout,
};
use crate::layout_switch::{
    failed_detection_fallback, CommandDesktopSettingsReader, DesktopSettingsReader,
    LayoutSwitchAutoDetector,
};
use crate::model::{
    DesktopEnvironment, DetectionConfidence, DistroKind, HotkeySpec, LayoutSwitchCaptureState,
    LayoutSwitchCombo, LayoutSwitchSetting, SessionType, Settings, SystemContext,
    UpdateSettingsResult,
};
use crate::system::{SystemContextDetector, UserServiceController};
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const LAYOUT_DEBUG_ENV: &str = "OPEN_SWITCHER_LAYOUT_DEBUG";
const LAYOUT_DEBUG_FILE_ENV: &str = "OPEN_SWITCHER_LAYOUT_DEBUG_FILE";
const BACKGROUND_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(300);
const GNOME_INPUT_SOURCES_SCHEMA: &str = "org.gnome.desktop.input-sources";
const GNOME_SOURCES_KEY: &str = "sources";
const GNOME_MRU_SOURCES_KEY: &str = "mru-sources";
pub const TRAY_WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);
pub const TRAY_RECOVERY_DELAY: Duration = Duration::from_millis(500);
pub const MAX_TRAY_RECOVERY_ATTEMPTS: usize = 3;
const SETTINGS_HOTKEY_CAPTURE_INHIBITION_LEASE: Duration = Duration::from_secs(120);

// Runtime config snapshot

#[derive(Clone, Debug)]
pub struct RuntimeConfigSnapshot {
    pub auto_switch_enabled: bool,
    pub fix_two_capitals: bool,
    pub fix_accidental_caps_lock: bool,
    pub layout_switch_combo: LayoutSwitchCombo,
    pub layout_delay_ms: u64,
    pub backspace_ms: u64,
    pub typing_ms: u64,
    pub manual_correction_hotkey: HotkeySpec,
    pub selected_text_hotkey: HotkeySpec,
}

impl From<&AppConfig> for RuntimeConfigSnapshot {
    fn from(value: &AppConfig) -> Self {
        Self {
            auto_switch_enabled: value.features.auto_switch_enabled,
            fix_two_capitals: value.features.fix_two_capitals,
            fix_accidental_caps_lock: value.features.fix_accidental_caps_lock,
            layout_switch_combo: value.layout.switch_combo,
            layout_delay_ms: value.layout.delay_ms as u64,
            backspace_ms: value.delays.backspace_ms as u64,
            typing_ms: value.delays.typing_ms as u64,
            manual_correction_hotkey: value.features.manual_correction_hotkey,
            selected_text_hotkey: value.features.selected_text_switch_hotkey,
        }
    }
}

// Config service

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

        if should_detect_layout_switch(&config) {
            let detected = detect_layout_switch_setting(context, detector);
            if apply_detected_layout_switch_if_changed(&mut config, detected) {
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

    pub fn auto_switch_enabled(&self) -> Result<bool, SettingsError> {
        self.inner
            .read()
            .map(|config| config.features.auto_switch_enabled)
            .map_err(|_| SettingsError::LockPoisoned)
    }

    fn apply_detected_layout_switch_runtime(
        &self,
        detected: LayoutSwitchSetting,
    ) -> Result<bool, SettingsError> {
        let mut config = self
            .inner
            .write()
            .map_err(|_| SettingsError::LockPoisoned)?;

        if !should_redetect_layout_switch_after_context_upgrade(&config) {
            return Ok(false);
        }

        if runtime_layout_switch_redetect_would_downgrade(config.settings().layout_switch, detected)
        {
            return Ok(false);
        }

        Ok(apply_detected_layout_switch_if_changed(
            &mut config,
            detected,
        ))
    }

    fn should_redetect_layout_switch_after_context_upgrade(&self) -> Result<bool, SettingsError> {
        self.inner
            .read()
            .map(|config| should_redetect_layout_switch_after_context_upgrade(&config))
            .map_err(|_| SettingsError::LockPoisoned)
    }
}

fn should_detect_layout_switch(config: &AppConfig) -> bool {
    !matches!(
        config.layout.switch_source,
        crate::model::LayoutSwitchSource::Manual
    )
}

fn should_redetect_layout_switch_after_context_upgrade(config: &AppConfig) -> bool {
    matches!(
        config.layout.switch_source,
        crate::model::LayoutSwitchSource::AutoDetected
            | crate::model::LayoutSwitchSource::AutoFallback
    )
}

fn runtime_layout_switch_redetect_would_downgrade(
    current: LayoutSwitchSetting,
    detected: LayoutSwitchSetting,
) -> bool {
    current.is_locked_by_auto_detection()
        && detected.source == crate::model::LayoutSwitchSource::AutoFallback
        && detected.auto_detected.confidence != DetectionConfidence::High
}

fn detect_layout_switch_setting<R: DesktopSettingsReader>(
    context: SystemContext,
    detector: &LayoutSwitchAutoDetector<R>,
) -> LayoutSwitchSetting {
    match detector.detect(context) {
        Ok(detected) => detected,
        Err(error) => {
            eprintln!(
                "[config] Failed to auto-detect layout switch combo, using fallback: {error}"
            );
            failed_detection_fallback(context)
        }
    }
}

fn apply_detected_layout_switch_if_changed(
    config: &mut AppConfig,
    detected: LayoutSwitchSetting,
) -> bool {
    if config.settings().layout_switch == detected {
        return false;
    }

    config.layout.switch_combo = detected.combo;
    config.layout.switch_source = detected.source;
    config.layout.auto_detected = detected.auto_detected;
    true
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

    // Test helpers

    #[derive(Clone)]
    struct CountingReader {
        calls: Arc<AtomicUsize>,
        combo: LayoutSwitchCombo,
    }

    impl DesktopSettingsReader for CountingReader {
        fn gsettings_string_list(
            &self,
            schema: &str,
            key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if schema == "org.gnome.desktop.wm.keybindings" && key == "switch-input-source" {
                return Ok(vec![gnome_binding_for_combo(self.combo).to_string()]);
            }

            if schema == "org.gnome.desktop.wm.keybindings" && key == "switch-input-source-backward"
            {
                return Ok(Vec::new());
            }

            if schema == GNOME_INPUT_SOURCES_SCHEMA && key == GNOME_SOURCES_KEY {
                return Ok(trusted_gnome_sources());
            }

            if schema == GNOME_INPUT_SOURCES_SCHEMA && key == GNOME_MRU_SOURCES_KEY {
                return Ok(gnome_sources(&[("xkb", "us"), ("xkb", "ru")]));
            }

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

    fn gnome_binding_for_combo(combo: LayoutSwitchCombo) -> &'static str {
        match combo {
            LayoutSwitchCombo::CtrlShift => "<Primary>Shift_L",
            LayoutSwitchCombo::AltShift => "<Shift>Alt_L",
            LayoutSwitchCombo::CapsLock => "Caps_Lock",
            LayoutSwitchCombo::CtrlSpace => "<Primary>space",
            LayoutSwitchCombo::SuperSpace => "<Super>space",
            LayoutSwitchCombo::LeftCtrlLeftShift => "<Control_L>Shift_L",
            LayoutSwitchCombo::RightCtrlRightShift => "<Control_R>Shift_R",
            LayoutSwitchCombo::LeftAltLeftShift => "<Alt_L>Shift_L",
            LayoutSwitchCombo::RightAltRightShift => "<Alt_R>Shift_R",
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
        test_runtime_with_backend_and_context(initial_layout_state, backend, cinnamon_x11_context())
    }

    fn test_runtime_with_backend_and_context(
        initial_layout_state: CurrentLayoutState,
        backend: Box<dyn LayoutBackend>,
        system_context: SystemContext,
    ) -> RuntimeState {
        let config_service = ConfigService {
            config_path: PathBuf::from("test-config.toml"),
            inner: RwLock::new(AppConfig::default()),
        };
        let enabled = config_service.auto_switch_enabled().unwrap_or(true);

        RuntimeState {
            enabled: AtomicBool::new(enabled),
            should_exit: AtomicBool::new(false),
            hotkey_capture_inhibition_started_at: Instant::now(),
            settings_hotkey_capture_inhibited_until_ms: AtomicU64::new(0),
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
            system_context: RwLock::new(system_context),
            current_layout_observation: RwLock::new(None),
            config_service,
            capture_session: Mutex::new(LayoutSwitchCaptureSession::default()),
            background_sync_started: AtomicBool::new(false),
            pending_status_change: AtomicBool::new(false),
        }
    }

    // Config auto-detection

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

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let persisted = AppConfig::load_or_create(&path).unwrap();
        assert_eq!(
            persisted.layout.switch_source,
            LayoutSwitchSource::AutoDetected
        );
        assert_eq!(
            persisted.layout.auto_detected,
            AutoDetectedLayoutSwitch {
                strategy: DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
                confidence: DetectionConfidence::High,
                context: gnome_wayland_context(),
            }
        );
    }

    // Manual config preservation

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

    // Backend sync

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

    // Background sync policy

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

    // Request exit / runtime flags

    #[test]
    fn request_exit_sets_exit_flag() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );

        assert!(!runtime.should_exit());
        runtime.request_exit();
        assert!(runtime.should_exit());
    }

    // Tray watchdog

    #[test]
    fn tray_watchdog_attempts_restart_three_times_then_requests_exit() {
        let runtime = Arc::new(test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        ));
        let bus = FakeTrayPresence { present: false };
        let starter = FakeTrayStarter {
            results: Arc::new(Mutex::new(vec![
                Err(crate::error::ServiceManagerError::CommandFailed {
                    command: vec!["systemctl".into()],
                    code: Some(1),
                    stderr: "fail-1".into(),
                }),
                Err(crate::error::ServiceManagerError::CommandFailed {
                    command: vec!["systemctl".into()],
                    code: Some(1),
                    stderr: "fail-2".into(),
                }),
                Err(crate::error::ServiceManagerError::CommandFailed {
                    command: vec!["systemctl".into()],
                    code: Some(1),
                    stderr: "fail-3".into(),
                }),
            ])),
            calls: Arc::new(AtomicUsize::new(0)),
        };

        run_tray_watchdog_iteration(runtime.clone(), &bus, &starter, 3, Duration::ZERO);

        assert!(runtime.should_exit());
        assert_eq!(starter.calls.load(Ordering::SeqCst), 3);
    }

    struct FakeTrayPresence {
        present: bool,
    }

    impl TrayPresenceProbe for FakeTrayPresence {
        fn tray_is_present(&self) -> Result<bool, std::io::Error> {
            Ok(self.present)
        }
    }

    struct FakeTrayStarter {
        results: Arc<Mutex<Vec<Result<(), crate::error::ServiceManagerError>>>>,
        calls: Arc<AtomicUsize>,
    }

    impl TrayServiceStarter for FakeTrayStarter {
        fn start_tray_service(&self) -> Result<(), crate::error::ServiceManagerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.results.lock().unwrap().remove(0)
        }
    }

    // GNOME Wayland observation

    struct LayoutObservationReaderStub {
        calls: Arc<AtomicUsize>,
        sources: Option<Vec<String>>,
        mru_sources: Option<Vec<String>>,
    }

    impl DesktopSettingsReader for LayoutObservationReaderStub {
        fn gsettings_string_list(
            &self,
            schema: &str,
            key: &str,
        ) -> Result<Vec<String>, LayoutAutoDetectError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if schema == GNOME_INPUT_SOURCES_SCHEMA && key == GNOME_SOURCES_KEY {
                return self
                    .sources
                    .clone()
                    .ok_or(LayoutAutoDetectError::GSettingsFailed {
                        stderr: "sources not configured".to_string(),
                    });
            }

            if schema == GNOME_INPUT_SOURCES_SCHEMA && key == GNOME_MRU_SOURCES_KEY {
                return self
                    .mru_sources
                    .clone()
                    .ok_or(LayoutAutoDetectError::GSettingsFailed {
                        stderr: "mru-sources not configured".to_string(),
                    });
            }

            Err(LayoutAutoDetectError::GSettingsFailed {
                stderr: format!("{schema}.{key} not configured"),
            })
        }

        fn xfconf_string(
            &self,
            _channel: &str,
            _property: &str,
        ) -> Result<String, LayoutAutoDetectError> {
            unimplemented!("not used in runtime tests")
        }

        fn xfconf_bool(
            &self,
            _channel: &str,
            _property: &str,
        ) -> Result<bool, LayoutAutoDetectError> {
            unimplemented!("not used in runtime tests")
        }

        fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError> {
            unimplemented!("not used in runtime tests")
        }
    }

    fn gnome_sources(pairs: &[(&str, &str)]) -> Vec<String> {
        pairs
            .iter()
            .flat_map(|(source_type, source_id)| {
                [(*source_type).to_string(), (*source_id).to_string()]
            })
            .collect()
    }

    fn trusted_gnome_sources() -> Vec<String> {
        gnome_sources(&[("xkb", "us"), ("xkb", "ru")])
    }

    fn layout_observation_reader(
        calls: Arc<AtomicUsize>,
        mru_sources: Vec<String>,
    ) -> LayoutObservationReaderStub {
        LayoutObservationReaderStub {
            calls,
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(mru_sources),
        }
    }

    fn assert_trusted_observed_kind(
        observation: Option<CurrentLayoutState>,
        expected_kind: AppLayoutKind,
    ) {
        assert!(matches!(
            observation,
            Some(CurrentLayoutState::Known {
                layout,
                trustworthy: true,
            }) if layout.kind == expected_kind
        ));
    }

    fn assert_untrusted_observation(observation: Option<CurrentLayoutState>) {
        assert!(matches!(
            observation,
            Some(CurrentLayoutState::Unknown { .. })
        ));
    }

    struct SystemContextDetectorStub {
        calls: Arc<AtomicUsize>,
        context: SystemContext,
    }

    impl SystemContextSource for SystemContextDetectorStub {
        fn detect_current(&self) -> Result<SystemContext, SystemContextError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.context)
        }
    }

    fn runtime_with_config_and_context(
        config: AppConfig,
        system_context: SystemContext,
    ) -> RuntimeState {
        let enabled = config.features.auto_switch_enabled;
        RuntimeState {
            enabled: AtomicBool::new(enabled),
            should_exit: AtomicBool::new(false),
            hotkey_capture_inhibition_started_at: Instant::now(),
            settings_hotkey_capture_inhibited_until_ms: AtomicU64::new(0),
            layout_state: RwLock::new(CurrentLayoutState::Unknown {
                reason: "test".to_string(),
            }),
            backend: Mutex::new(Some(Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Unknown {
                    reason: "test".to_string(),
                }),
            }))),
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
            system_context: RwLock::new(system_context),
            current_layout_observation: RwLock::new(None),
            config_service: ConfigService {
                config_path: PathBuf::from("test-config.toml"),
                inner: RwLock::new(config),
            },
            capture_session: Mutex::new(LayoutSwitchCaptureSession::default()),
            background_sync_started: AtomicBool::new(false),
            pending_status_change: AtomicBool::new(false),
        }
    }

    #[test]
    fn current_layout_state_prefers_gnome_wayland_observation() {
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: russian_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(russian_layout())),
            }),
            gnome_wayland_context(),
        );

        runtime.refresh_current_layout_observation_with_reader(&LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        });

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
        assert!(runtime.current_layout());
        assert_eq!(
            runtime.auto_correction_layout_kind(),
            AppLayoutKind::English
        );
    }

    #[test]
    fn sync_with_backend_keeps_gnome_wayland_observation_as_source_of_truth() {
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Known {
                    layout: russian_layout(),
                    trustworthy: false,
                }),
            }),
            gnome_wayland_context(),
        );

        runtime.refresh_current_layout_observation_with_reader(&LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        });
        runtime.clear_pending_status_change();

        assert_eq!(runtime.sync_with_backend(), BackendSyncResult::Unchanged);
        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
        assert!(!runtime.take_pending_status_change());
    }

    #[test]
    fn refresh_current_layout_observation_reads_gsettings_only_for_gnome_wayland() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reader = LayoutObservationReaderStub {
            calls: Arc::clone(&calls),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: russian_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(russian_layout())),
            }),
            cinnamon_x11_context(),
        );

        runtime.refresh_current_layout_observation_with_reader(&reader);

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime.current_layout_state(),
            CurrentLayoutState::Known {
                layout: russian_layout(),
                trustworthy: false,
            }
        );
    }

    #[test]
    fn refresh_current_layout_observation_updates_runtime_cache_and_pending_status() {
        let calls = Arc::new(AtomicUsize::new(0));
        let reader = LayoutObservationReaderStub {
            calls: Arc::clone(&calls),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: russian_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(russian_layout())),
            }),
            gnome_wayland_context(),
        );

        runtime.refresh_current_layout_observation_with_reader(&reader);

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
        assert!(runtime.take_pending_status_change());
    }

    #[test]
    fn gnome_wayland_observation_trusts_us_ru_sources_with_us_current_mru() {
        let reader = layout_observation_reader(
            Arc::new(AtomicUsize::new(0)),
            gnome_sources(&[("xkb", "us"), ("xkb", "ru")]),
        );

        assert_trusted_observed_kind(
            gnome_wayland_current_layout_state(&reader),
            AppLayoutKind::English,
        );
    }

    #[test]
    fn gnome_wayland_observation_trusts_us_ru_sources_with_ru_current_mru() {
        let reader = layout_observation_reader(
            Arc::new(AtomicUsize::new(0)),
            gnome_sources(&[("xkb", "ru"), ("xkb", "us")]),
        );

        assert_trusted_observed_kind(
            gnome_wayland_current_layout_state(&reader),
            AppLayoutKind::Russian,
        );
    }

    #[test]
    fn gnome_wayland_observation_rejects_ibus_current_without_falling_through_to_xkb() {
        let reader = layout_observation_reader(
            Arc::new(AtomicUsize::new(0)),
            gnome_sources(&[("ibus", "mozc-jp"), ("xkb", "us")]),
        );

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_rejects_xkb_variants() {
        let reader = layout_observation_reader(
            Arc::new(AtomicUsize::new(0)),
            gnome_sources(&[("xkb", "ru+phonetic"), ("xkb", "us")]),
        );

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_rejects_configured_xkb_variants() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(gnome_sources(&[("xkb", "us+intl"), ("xkb", "ru")])),
            mru_sources: Some(gnome_sources(&[("xkb", "ru"), ("xkb", "us+intl")])),
        };

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_rejects_more_than_two_configured_xkb_sources() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru"), ("xkb", "de")])),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_rejects_malformed_mru_sources() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(vec!["xkb".into(), "us".into(), "xkb".into()]),
        };

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_rejects_empty_mru_sources() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(Vec::new()),
        };

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_read_error_preserves_existing_observation() {
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: russian_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Known {
                    layout: russian_layout(),
                    trustworthy: false,
                }),
            }),
            gnome_wayland_context(),
        );

        runtime.refresh_current_layout_observation_with_reader(&layout_observation_reader(
            Arc::new(AtomicUsize::new(0)),
            gnome_sources(&[("xkb", "us"), ("xkb", "ru")]),
        ));
        runtime.clear_pending_status_change();

        runtime.refresh_current_layout_observation_with_reader(&LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: None,
            mru_sources: None,
        });

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
        assert!(!runtime.take_pending_status_change());
    }

    #[test]
    fn periodic_sync_tick_late_upgrades_unknown_context_and_uses_gnome_wayland_observation() {
        let reader_calls = Arc::new(AtomicUsize::new(0));
        let detector_calls = Arc::new(AtomicUsize::new(0));
        let reader = LayoutObservationReaderStub {
            calls: Arc::clone(&reader_calls),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "ru"), ("xkb", "us")])),
        };
        let detector = SystemContextDetectorStub {
            calls: Arc::clone(&detector_calls),
            context: gnome_wayland_context(),
        };
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Known {
                    layout: english_layout(),
                    trustworthy: false,
                }),
            }),
            SystemContext::default(),
        );

        runtime.clear_pending_status_change();

        assert_eq!(
            runtime.periodic_sync_tick_with(&reader, &detector),
            BackendSyncResult::Unchanged
        );
        assert_eq!(runtime.system_context(), gnome_wayland_context());
        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(russian_layout())
        );
        assert!(runtime.take_pending_status_change());
        assert_eq!(reader_calls.load(Ordering::SeqCst), 2);
        assert_eq!(detector_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn periodic_sync_tick_skips_context_refresh_when_candidate_is_not_a_late_upgrade() {
        let reader_calls = Arc::new(AtomicUsize::new(0));
        let detector_calls = Arc::new(AtomicUsize::new(0));
        let reader = LayoutObservationReaderStub {
            calls: Arc::clone(&reader_calls),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "ru"), ("xkb", "us")])),
        };
        let detector = SystemContextDetectorStub {
            calls: Arc::clone(&detector_calls),
            context: SystemContext::default(),
        };
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Known {
                    layout: english_layout(),
                    trustworthy: false,
                }),
            }),
            SystemContext::default(),
        );

        runtime.clear_pending_status_change();

        assert_eq!(
            runtime.periodic_sync_tick_with(&reader, &detector),
            BackendSyncResult::Unchanged
        );
        assert_eq!(runtime.system_context(), SystemContext::default());
        assert_eq!(
            runtime.current_layout_state(),
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            }
        );
        assert!(!runtime.take_pending_status_change());
        assert_eq!(reader_calls.load(Ordering::SeqCst), 0);
        assert_eq!(detector_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn late_context_upgrade_accepts_partial_context_that_becomes_more_complete() {
        let partial = SystemContext {
            session_type: SessionType::Unknown,
            desktop_environment: DesktopEnvironment::Gnome,
            distro: DistroKind::Ubuntu,
        };

        assert!(is_late_system_context_upgrade(
            partial,
            gnome_wayland_context()
        ));
        assert!(!is_late_system_context_upgrade(
            gnome_wayland_context(),
            partial
        ));
    }

    #[test]
    fn late_context_upgrade_accepts_stale_x11_to_gnome_wayland() {
        assert!(is_late_system_context_upgrade(
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
            gnome_wayland_context()
        ));
    }

    #[test]
    fn late_context_upgrade_rejects_wayland_to_unknown_downgrade() {
        assert!(!is_late_system_context_upgrade(
            gnome_wayland_context(),
            SystemContext::default()
        ));
    }

    #[test]
    fn periodic_sync_tick_preserves_manual_layout_switch_on_stale_x11_upgrade() {
        let detector_calls = Arc::new(AtomicUsize::new(0));
        let detector = SystemContextDetectorStub {
            calls: Arc::clone(&detector_calls),
            context: gnome_wayland_context(),
        };
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: None,
            mru_sources: None,
        };
        let mut config = AppConfig::default();
        config.layout.switch_combo = LayoutSwitchCombo::caps_lock();
        config.layout.switch_source = LayoutSwitchSource::Manual;
        config.layout.auto_detected = AutoDetectedLayoutSwitch::default();
        let runtime = runtime_with_config_and_context(
            config,
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        );

        let _ = runtime.periodic_sync_tick_with(&reader, &detector);

        assert_eq!(runtime.system_context(), gnome_wayland_context());
        assert_eq!(
            runtime.get_settings().unwrap().layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::caps_lock(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            }
        );
        assert_eq!(detector_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn periodic_sync_tick_redetects_auto_layout_switch_on_stale_x11_upgrade() {
        let detector = SystemContextDetectorStub {
            calls: Arc::new(AtomicUsize::new(0)),
            context: gnome_wayland_context(),
        };
        let reader_calls = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            calls: Arc::clone(&reader_calls),
            combo: LayoutSwitchCombo::super_space(),
        };
        let mut config = AppConfig::default();
        config.layout.switch_combo = LayoutSwitchCombo::alt_shift();
        config.layout.switch_source = LayoutSwitchSource::AutoDetected;
        config.layout.auto_detected = AutoDetectedLayoutSwitch {
            strategy: DetectionStrategy::CinnamonX11GSettingsXkbOptions,
            confidence: DetectionConfidence::High,
            context: SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        };
        let runtime = runtime_with_config_and_context(
            config,
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        );

        let _ = runtime.periodic_sync_tick_with(&reader, &detector);

        assert_eq!(runtime.system_context(), gnome_wayland_context());
        assert_eq!(
            runtime.get_settings().unwrap().layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::super_space(),
                source: LayoutSwitchSource::AutoDetected,
                auto_detected: AutoDetectedLayoutSwitch {
                    strategy: DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
                    confidence: DetectionConfidence::High,
                    context: gnome_wayland_context(),
                },
            }
        );
        assert_eq!(reader_calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn periodic_sync_tick_redetects_auto_fallback_layout_switch_on_stale_x11_upgrade() {
        let detector = SystemContextDetectorStub {
            calls: Arc::new(AtomicUsize::new(0)),
            context: gnome_wayland_context(),
        };
        let reader = CountingReader {
            calls: Arc::new(AtomicUsize::new(0)),
            combo: LayoutSwitchCombo::super_space(),
        };
        let mut config = AppConfig::default();
        config.layout.switch_combo = LayoutSwitchCombo::ctrl_shift();
        config.layout.switch_source = LayoutSwitchSource::AutoFallback;
        config.layout.auto_detected = AutoDetectedLayoutSwitch {
            strategy: DetectionStrategy::CinnamonX11GSettingsXkbOptions,
            confidence: DetectionConfidence::Low,
            context: SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        };
        let runtime = runtime_with_config_and_context(
            config,
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        );

        let _ = runtime.periodic_sync_tick_with(&reader, &detector);

        assert_eq!(
            runtime.get_settings().unwrap().layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::super_space(),
                source: LayoutSwitchSource::AutoDetected,
                auto_detected: AutoDetectedLayoutSwitch {
                    strategy: DetectionStrategy::GnomeWaylandGSettingsWmKeybindings,
                    confidence: DetectionConfidence::High,
                    context: gnome_wayland_context(),
                },
            }
        );
    }

    #[test]
    fn periodic_sync_tick_does_not_downgrade_high_confidence_auto_to_low_fallback() {
        let detector = SystemContextDetectorStub {
            calls: Arc::new(AtomicUsize::new(0)),
            context: gnome_wayland_context(),
        };
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: None,
            mru_sources: None,
        };
        let original = LayoutSwitchSetting {
            combo: LayoutSwitchCombo::alt_shift(),
            source: LayoutSwitchSource::AutoDetected,
            auto_detected: AutoDetectedLayoutSwitch {
                strategy: DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                confidence: DetectionConfidence::High,
                context: SystemContext {
                    session_type: SessionType::X11,
                    desktop_environment: DesktopEnvironment::Gnome,
                    distro: DistroKind::Ubuntu,
                },
            },
        };
        let mut config = AppConfig::default();
        config.layout.switch_combo = original.combo;
        config.layout.switch_source = original.source;
        config.layout.auto_detected = original.auto_detected;
        let runtime = runtime_with_config_and_context(
            config,
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        );

        let _ = runtime.periodic_sync_tick_with(&reader, &detector);

        assert_eq!(runtime.system_context(), gnome_wayland_context());
        assert_eq!(runtime.get_settings().unwrap().layout_switch, original);
    }

    #[test]
    fn set_layout_updates_gnome_wayland_observation_cache() {
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Known {
                    layout: russian_layout(),
                    trustworthy: false,
                }),
            }),
            gnome_wayland_context(),
        );

        runtime.refresh_current_layout_observation_with_reader(&LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        });
        runtime.clear_pending_status_change();

        runtime.set_layout_with_reason(false, "test-switch");

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(russian_layout())
        );
        assert!(!runtime.current_layout());
    }

    #[test]
    fn settings_hotkey_capture_inhibition_roundtrips() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );

        assert!(!runtime.settings_hotkey_capture_inhibited());
        runtime.set_settings_hotkey_capture_inhibited(true);
        assert!(runtime.settings_hotkey_capture_inhibited());
        runtime.set_settings_hotkey_capture_inhibited(false);
        assert!(!runtime.settings_hotkey_capture_inhibited());
    }

    #[test]
    fn settings_hotkey_capture_inhibition_expires_after_lease() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );

        runtime.set_settings_hotkey_capture_inhibited(true);
        runtime.force_settings_hotkey_capture_inhibited_until_ms_for_test(
            runtime.hotkey_capture_inhibition_now_ms().saturating_sub(1),
        );

        assert!(!runtime.settings_hotkey_capture_inhibited());
    }

    #[test]
    fn settings_hotkey_capture_inhibition_deadline_is_exclusive() {
        assert!(!settings_hotkey_capture_inhibited_at(10, 0));
        assert!(settings_hotkey_capture_inhibited_at(9, 10));
        assert!(!settings_hotkey_capture_inhibited_at(10, 10));
        assert!(!settings_hotkey_capture_inhibited_at(11, 10));
    }
}

// Runtime state

pub struct RuntimeState {
    enabled: AtomicBool,
    should_exit: AtomicBool,
    hotkey_capture_inhibition_started_at: Instant,
    settings_hotkey_capture_inhibited_until_ms: AtomicU64,
    layout_state: RwLock<CurrentLayoutState>,
    backend: Mutex<Option<Box<dyn LayoutBackend>>>,
    layout_setup: RwLock<LayoutSetup>,
    layout_compatibility: RwLock<LayoutCompatibility>,
    feature_availability: RwLock<FeatureAvailability>,
    system_context: RwLock<SystemContext>,
    current_layout_observation: RwLock<Option<CurrentLayoutState>>,
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

// Tray watchdog

pub trait TrayPresenceProbe {
    fn tray_is_present(&self) -> Result<bool, std::io::Error>;
}

trait SystemContextSource {
    fn detect_current(&self) -> Result<SystemContext, SystemContextError>;
}

impl SystemContextSource for SystemContextDetector {
    fn detect_current(&self) -> Result<SystemContext, SystemContextError> {
        Self::detect_current()
    }
}

pub trait TrayServiceStarter {
    fn start_tray_service(&self) -> Result<(), ServiceManagerError>;
}

impl<R: crate::system::user_services::CommandRunner> TrayServiceStarter
    for UserServiceController<R>
{
    fn start_tray_service(&self) -> Result<(), ServiceManagerError> {
        UserServiceController::start_tray_service(self)
    }
}

pub fn run_tray_watchdog_iteration(
    runtime: Arc<RuntimeState>,
    probe: &impl TrayPresenceProbe,
    starter: &impl TrayServiceStarter,
    attempts: usize,
    delay: Duration,
) {
    let tray_present = match probe.tray_is_present() {
        Ok(present) => present,
        Err(error) => {
            log_layout_debug("tray-watchdog-probe", &format!("error={error}"));
            false
        }
    };

    if tray_present {
        return;
    }

    for attempt in 0..attempts {
        match starter.start_tray_service() {
            Ok(()) => match probe.tray_is_present() {
                Ok(true) => {
                    log_layout_debug(
                        "tray-watchdog-recovery",
                        &format!("recovered=true attempts={}", attempt + 1),
                    );
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    log_layout_debug(
                        "tray-watchdog-probe",
                        &format!("attempt={} error={error}", attempt + 1),
                    );
                }
            },
            Err(error) => {
                log_layout_debug(
                    "tray-watchdog-start",
                    &format!("attempt={} error={error}", attempt + 1),
                );
            }
        }

        if attempt + 1 < attempts && !delay.is_zero() {
            thread::sleep(delay);
        }
    }

    log_layout_debug("tray-watchdog-exit", "recovery_failed=true");
    runtime.request_exit();
}

fn settings_hotkey_capture_inhibited_at(now_ms: u64, inhibited_until_ms: u64) -> bool {
    inhibited_until_ms != 0 && now_ms < inhibited_until_ms
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

impl RuntimeState {
    // Runtime state initialization and flags

    pub fn new(config_service: ConfigService) -> Self {
        let (backend, layout_state, layout_setup, layout_compatibility, feature_availability) =
            Self::initialize_layout_backend();
        let enabled = config_service.auto_switch_enabled().unwrap_or(true);
        let runtime = Self {
            enabled: AtomicBool::new(enabled),
            should_exit: AtomicBool::new(false),
            hotkey_capture_inhibition_started_at: Instant::now(),
            settings_hotkey_capture_inhibited_until_ms: AtomicU64::new(0),
            layout_state: RwLock::new(layout_state),
            backend: Mutex::new(backend),
            layout_setup: RwLock::new(layout_setup),
            layout_compatibility: RwLock::new(layout_compatibility),
            feature_availability: RwLock::new(feature_availability),
            system_context: RwLock::new(
                SystemContextDetector::detect_current().unwrap_or_default(),
            ),
            current_layout_observation: RwLock::new(None),
            config_service,
            capture_session: Mutex::new(LayoutSwitchCaptureSession::default()),
            background_sync_started: AtomicBool::new(false),
            pending_status_change: AtomicBool::new(false),
        };
        runtime.refresh_current_layout_observation();
        runtime
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn request_exit(&self) {
        self.should_exit.store(true, Ordering::SeqCst);
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit.load(Ordering::SeqCst)
    }

    pub fn set_settings_hotkey_capture_inhibited(&self, inhibited: bool) {
        let inhibited_until_ms = if inhibited {
            self.hotkey_capture_inhibition_now_ms()
                .saturating_add(duration_millis(SETTINGS_HOTKEY_CAPTURE_INHIBITION_LEASE))
                .max(1)
        } else {
            0
        };
        self.settings_hotkey_capture_inhibited_until_ms
            .store(inhibited_until_ms, Ordering::SeqCst);
    }

    pub fn settings_hotkey_capture_inhibited(&self) -> bool {
        settings_hotkey_capture_inhibited_at(
            self.hotkey_capture_inhibition_now_ms(),
            self.settings_hotkey_capture_inhibited_until_ms
                .load(Ordering::SeqCst),
        )
    }

    fn hotkey_capture_inhibition_now_ms(&self) -> u64 {
        duration_millis(self.hotkey_capture_inhibition_started_at.elapsed())
    }

    #[cfg(test)]
    fn force_settings_hotkey_capture_inhibited_until_ms_for_test(&self, inhibited_until_ms: u64) {
        self.settings_hotkey_capture_inhibited_until_ms
            .store(inhibited_until_ms, Ordering::SeqCst);
    }

    pub fn toggle_enabled(&self) -> bool {
        self.toggle_enabled_result().unwrap_or_else(|error| {
            log_layout_debug("toggle-enabled", &format!("error={error}"));
            self.is_enabled()
        })
    }

    pub fn toggle_enabled_result(&self) -> Result<bool, SettingsError> {
        let mut settings = self.get_settings()?;
        settings.auto_switch_enabled = !settings.auto_switch_enabled;
        self.update_settings(settings)?;
        Ok(settings.auto_switch_enabled)
    }

    pub fn current_layout(&self) -> bool {
        let state = self.current_layout_state();
        let state = &state;
        legacy_current_layout_bool(state)
    }

    fn raw_layout_state(&self) -> CurrentLayoutState {
        self.layout_state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn observed_layout_state(&self) -> Option<CurrentLayoutState> {
        self.current_layout_observation
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn effective_layout_state(&self, raw_state: &CurrentLayoutState) -> CurrentLayoutState {
        effective_current_layout_state(
            raw_state,
            self.observed_layout_state().as_ref(),
            self.system_context(),
        )
    }

    fn system_context(&self) -> SystemContext {
        *self
            .system_context
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn update_current_layout_observation(
        &self,
        next_observation: Option<CurrentLayoutState>,
        reason: &str,
    ) {
        let raw_state = self.raw_layout_state();
        let mut observation = self
            .current_layout_observation
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let previous_effective =
            effective_current_layout_state(&raw_state, observation.as_ref(), self.system_context());

        if *observation == next_observation {
            return;
        }

        let next_effective = effective_current_layout_state(
            &raw_state,
            next_observation.as_ref(),
            self.system_context(),
        );
        log_layout_debug(
            "current-layout-observation",
            &format!(
                "reason={reason} previous_observation={:?} next_observation={next_observation:?}",
                *observation
            ),
        );
        *observation = next_observation;

        if previous_effective != next_effective {
            self.pending_status_change.store(true, Ordering::SeqCst);
            log_layout_debug(
                "current-layout-effective-state",
                &format!("reason={reason} previous={previous_effective:?} next={next_effective:?}"),
            );
        }
    }

    fn update_current_layout_cache(
        &self,
        next_raw_state: CurrentLayoutState,
        reason: &str,
    ) -> BackendSyncResult {
        let observation = self.observed_layout_state();
        let mut state = self
            .layout_state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let previous_effective =
            effective_current_layout_state(&state, observation.as_ref(), self.system_context());
        let next_effective = effective_current_layout_state(
            &next_raw_state,
            observation.as_ref(),
            self.system_context(),
        );

        if *state != next_raw_state {
            *state = next_raw_state;
        }

        if previous_effective == next_effective {
            return BackendSyncResult::Unchanged;
        }

        self.pending_status_change.store(true, Ordering::SeqCst);
        log_layout_debug(
            reason,
            &format!("previous={previous_effective:?} next={next_effective:?}"),
        );
        BackendSyncResult::Updated {
            previous: previous_effective,
            current: next_effective,
        }
    }

    pub fn set_layout(&self, layout_is_english: bool) {
        self.set_layout_with_reason(layout_is_english, "unspecified");
    }

    pub fn set_layout_with_reason(&self, layout_is_english: bool, reason: &str) {
        let previous = self.current_layout();
        let next_state = legacy_layout_state_from_bool(layout_is_english);
        let _ = self.update_current_layout_cache(next_state, "set-layout-cache");
        if is_gnome_wayland_context(self.system_context()) {
            self.update_current_layout_observation(
                Some(gnome_wayland_layout_state_from_bool(layout_is_english)),
                reason,
            );
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
        let raw_state = self.raw_layout_state();
        self.effective_layout_state(&raw_state)
    }

    pub fn auto_correction_layout_kind(&self) -> AppLayoutKind {
        current_layout_kind_from_state(&self.current_layout_state())
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

    // Layout backend sync

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

        self.update_current_layout_cache(snapshot, "backend-sync-update")
    }

    pub fn periodic_sync_tick(&self) -> BackendSyncResult {
        self.periodic_sync_tick_with(&CommandDesktopSettingsReader, &SystemContextDetector)
    }

    fn periodic_sync_tick_with<R: DesktopSettingsReader, D: SystemContextSource>(
        &self,
        reader: &R,
        detector: &D,
    ) -> BackendSyncResult {
        if self.refresh_system_context_with_detector(detector) {
            self.redetect_layout_switch_after_context_upgrade(reader);
        }
        self.refresh_current_layout_observation_with_reader(reader);
        self.sync_with_backend()
    }

    pub fn take_pending_status_change(&self) -> bool {
        self.pending_status_change.swap(false, Ordering::SeqCst)
    }

    pub fn clear_pending_status_change(&self) {
        self.pending_status_change.store(false, Ordering::SeqCst);
    }

    // Background sync and tray watchdog

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

        self.refresh_current_layout_observation();

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
                if runtime.should_exit() {
                    break;
                }
                thread::sleep(BACKGROUND_SYNC_POLL_INTERVAL);
                if runtime.should_exit() {
                    break;
                }
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

    pub fn start_tray_watchdog(
        self: &Arc<Self>,
        probe: impl TrayPresenceProbe + Send + Sync + 'static,
    ) {
        let runtime = Arc::clone(self);
        let services = UserServiceController::from_system();
        if let Err(error) = thread::Builder::new()
            .name("tray-watchdog".to_string())
            .spawn(move || loop {
                if runtime.should_exit() {
                    break;
                }
                thread::sleep(TRAY_WATCHDOG_INTERVAL);
                if runtime.should_exit() {
                    break;
                }
                run_tray_watchdog_iteration(
                    Arc::clone(&runtime),
                    &probe,
                    &services,
                    MAX_TRAY_RECOVERY_ATTEMPTS,
                    TRAY_RECOVERY_DELAY,
                );
            })
        {
            log_layout_debug("tray-watchdog-start", &format!("failed=true error={error}"));
        }
    }

    // Config/settings bridge

    pub fn get_settings(&self) -> Result<Settings, SettingsError> {
        self.config_service.get_settings()
    }

    pub fn update_settings(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsError> {
        let result = self.config_service.update_settings(settings)?;
        self.enabled
            .store(settings.auto_switch_enabled, Ordering::SeqCst);
        Ok(result)
    }

    pub fn config_snapshot(&self) -> Result<RuntimeConfigSnapshot, SettingsError> {
        self.config_service.snapshot()
    }

    // Capture delegation

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

    // Layout backend initialization

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

    // GNOME Wayland observation

    fn refresh_current_layout_observation(&self) {
        self.refresh_current_layout_observation_with_reader(&CommandDesktopSettingsReader);
    }

    fn refresh_current_layout_observation_with_reader<R: DesktopSettingsReader>(&self, reader: &R) {
        if !is_gnome_wayland_context(self.system_context()) {
            self.clear_current_layout_observation();
            return;
        }

        let Some(next_observation) = gnome_wayland_current_layout_state(reader) else {
            return;
        };
        self.update_current_layout_observation(Some(next_observation), "runtime-sync");
    }

    fn clear_current_layout_observation(&self) {
        self.update_current_layout_observation(None, "clear-observation");
    }

    fn refresh_system_context_with_detector<D: SystemContextSource>(&self, detector: &D) -> bool {
        let previous = self.system_context();
        let next = match detector.detect_current() {
            Ok(next) => next,
            Err(error) => {
                log_layout_debug(
                    "system-context-redetect",
                    &format!(
                        "previous={previous:?} next=unavailable result=skipped reason=detect-error error={error}"
                    ),
                );
                return false;
            }
        };

        if !is_late_system_context_upgrade(previous, next) {
            log_layout_debug(
                "system-context-redetect",
                &format!(
                    "previous={previous:?} next={next:?} result=skipped reason=not-late-upgrade"
                ),
            );
            return false;
        }

        let mut context = self
            .system_context
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *context = next;
        log_layout_debug(
            "system-context-redetect",
            &format!("previous={previous:?} next={next:?} result=applied"),
        );
        true
    }

    fn redetect_layout_switch_after_context_upgrade<R: DesktopSettingsReader>(&self, reader: &R) {
        let context = self.system_context();
        match self
            .config_service
            .should_redetect_layout_switch_after_context_upgrade()
        {
            Ok(true) => {}
            Ok(false) => {
                log_layout_debug(
                    "layout-switch-redetect",
                    &format!("context={context:?} result=skipped reason=manual-or-unknown"),
                );
                return;
            }
            Err(error) => {
                log_layout_debug(
                    "layout-switch-redetect",
                    &format!("context={context:?} result=skipped error={error}"),
                );
                return;
            }
        }

        let detector = LayoutSwitchAutoDetector::with_reader(reader);
        let detected = detect_layout_switch_setting(context, &detector);

        match self
            .config_service
            .apply_detected_layout_switch_runtime(detected)
        {
            Ok(true) => log_layout_debug(
                "layout-switch-redetect",
                &format!("context={context:?} result=applied setting={detected:?}"),
            ),
            Ok(false) => log_layout_debug(
                "layout-switch-redetect",
                &format!("context={context:?} result=unchanged-or-manual"),
            ),
            Err(error) => log_layout_debug(
                "layout-switch-redetect",
                &format!("context={context:?} result=skipped error={error}"),
            ),
        }
    }
}

// Layout backend sync

fn background_sync_polling_enabled(capabilities: BackendCapabilities) -> bool {
    capabilities.can_read_current_layout && !capabilities.can_observe_layout_changes
}

fn current_layout_kind_from_state(state: &CurrentLayoutState) -> AppLayoutKind {
    match state {
        CurrentLayoutState::Known { layout, .. } => layout.kind,
        CurrentLayoutState::Unknown { .. } => AppLayoutKind::Unknown,
    }
}

fn is_gnome_wayland_context(context: SystemContext) -> bool {
    context.session_type == SessionType::Wayland
        && context.desktop_environment == DesktopEnvironment::Gnome
}

fn is_late_system_context_upgrade(current: SystemContext, candidate: SystemContext) -> bool {
    if candidate == current {
        return false;
    }

    if is_stale_x11_to_gnome_wayland_upgrade(current, candidate) {
        return true;
    }

    context_preserves_known_fields(current, candidate)
        && context_known_score(candidate) > context_known_score(current)
}

fn is_stale_x11_to_gnome_wayland_upgrade(current: SystemContext, candidate: SystemContext) -> bool {
    current.session_type == SessionType::X11
        && candidate.session_type == SessionType::Wayland
        && candidate.desktop_environment == DesktopEnvironment::Gnome
        && field_preserves_known(
            current.desktop_environment,
            candidate.desktop_environment,
            DesktopEnvironment::Unknown,
        )
        && field_preserves_known(current.distro, candidate.distro, DistroKind::Unknown)
}

fn context_preserves_known_fields(current: SystemContext, candidate: SystemContext) -> bool {
    field_preserves_known(
        current.session_type,
        candidate.session_type,
        SessionType::Unknown,
    ) && field_preserves_known(
        current.desktop_environment,
        candidate.desktop_environment,
        DesktopEnvironment::Unknown,
    ) && field_preserves_known(current.distro, candidate.distro, DistroKind::Unknown)
}

fn field_preserves_known<T: Copy + Eq>(current: T, candidate: T, unknown: T) -> bool {
    current == unknown || current == candidate
}

fn context_known_score(context: SystemContext) -> usize {
    usize::from(context.session_type != SessionType::Unknown)
        + usize::from(context.desktop_environment != DesktopEnvironment::Unknown)
        + usize::from(context.distro != DistroKind::Unknown)
}

fn effective_current_layout_state(
    raw_state: &CurrentLayoutState,
    observed_state: Option<&CurrentLayoutState>,
    context: SystemContext,
) -> CurrentLayoutState {
    if is_gnome_wayland_context(context) {
        if let Some(observed_state) = observed_state {
            return observed_state.clone();
        }
    }

    raw_state.clone()
}

// GNOME Wayland observation

#[derive(Clone, Debug, PartialEq, Eq)]
struct GnomeInputSource {
    source_type: String,
    source_id: String,
}

fn gnome_wayland_layout_state_from_bool(layout_is_english: bool) -> CurrentLayoutState {
    gnome_wayland_current_layout_state_from_code(if layout_is_english { "us" } else { "ru" })
}

fn gnome_wayland_current_layout_state_from_code(layout_code: &str) -> CurrentLayoutState {
    let (backend_key, normalized_code, display_name, kind, index) = match layout_code {
        "us" => (
            "us".to_string(),
            LayoutCode::Us,
            "English".to_string(),
            AppLayoutKind::English,
            Some(0),
        ),
        "ru" => (
            "ru".to_string(),
            LayoutCode::Ru,
            "Russian".to_string(),
            AppLayoutKind::Russian,
            Some(1),
        ),
        other => (
            format!("xkb:{other}"),
            LayoutCode::from_normalized(other).unwrap_or(LayoutCode::Unknown),
            other.to_string(),
            AppLayoutKind::Other,
            None,
        ),
    };

    CurrentLayoutState::Known {
        layout: SystemLayout {
            backend_key,
            normalized_code,
            display_name,
            kind,
            index,
        },
        trustworthy: true,
    }
}

fn gnome_wayland_current_layout_state<R: DesktopSettingsReader>(
    reader: &R,
) -> Option<CurrentLayoutState> {
    let configured_sources =
        match reader.gsettings_string_list(GNOME_INPUT_SOURCES_SCHEMA, GNOME_SOURCES_KEY) {
            Ok(values) => values,
            Err(error) => {
                log_layout_debug(
                    "gnome-wayland-observation",
                    &format!("result=preserve reason=sources-read-error error={error}"),
                );
                return None;
            }
        };
    let mru_sources =
        match reader.gsettings_string_list(GNOME_INPUT_SOURCES_SCHEMA, GNOME_MRU_SOURCES_KEY) {
            Ok(values) => values,
            Err(error) => {
                log_layout_debug(
                    "gnome-wayland-observation",
                    &format!("result=preserve reason=mru-read-error error={error}"),
                );
                return None;
            }
        };

    Some(gnome_wayland_current_layout_state_from_sources(
        &configured_sources,
        &mru_sources,
    ))
}

fn gnome_wayland_current_layout_state_from_sources(
    configured_sources: &[String],
    mru_sources: &[String],
) -> CurrentLayoutState {
    let configured_sources = match gnome_input_sources_from_flat_values(configured_sources) {
        Ok(sources) => sources,
        Err(reason) => return gnome_wayland_unknown_layout_state(reason),
    };
    if !gnome_configured_sources_are_trusted_us_ru_pair(&configured_sources) {
        return gnome_wayland_unknown_layout_state("unsupported-configured-sources");
    }

    let mru_sources = match gnome_input_sources_from_flat_values(mru_sources) {
        Ok(sources) if !sources.is_empty() => sources,
        Ok(_) => return gnome_wayland_unknown_layout_state("empty-mru-sources"),
        Err(reason) => return gnome_wayland_unknown_layout_state(reason),
    };

    let current = &mru_sources[0];
    match trusted_gnome_xkb_layout_code(current) {
        Some(layout_code) => gnome_wayland_current_layout_state_from_code(layout_code),
        None => gnome_wayland_unknown_layout_state("unsupported-current-source"),
    }
}

fn gnome_input_sources_from_flat_values(
    values: &[String],
) -> Result<Vec<GnomeInputSource>, &'static str> {
    if values.len() % 2 != 0 {
        return Err("malformed-input-sources");
    }

    Ok(values
        .chunks_exact(2)
        .map(|chunk| GnomeInputSource {
            source_type: chunk[0].clone(),
            source_id: chunk[1].clone(),
        })
        .collect())
}

fn gnome_configured_sources_are_trusted_us_ru_pair(sources: &[GnomeInputSource]) -> bool {
    sources.len() == 2
        && sources.iter().any(|source| trusted_gnome_xkb_layout_code(source) == Some("us"))
        && sources
            .iter()
            .any(|source| trusted_gnome_xkb_layout_code(source) == Some("ru"))
}

fn trusted_gnome_xkb_layout_code(source: &GnomeInputSource) -> Option<&'static str> {
    match (source.source_type.as_str(), source.source_id.as_str()) {
        ("xkb", "us") => Some("us"),
        ("xkb", "ru") => Some("ru"),
        _ => None,
    }
}

fn gnome_wayland_unknown_layout_state(reason: &'static str) -> CurrentLayoutState {
    log_layout_debug(
        "gnome-wayland-observation",
        &format!("result=unknown reason={reason}"),
    );
    CurrentLayoutState::Unknown {
        reason: format!("gnome-wayland-observation:{reason}"),
    }
}

// Logging helpers

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
