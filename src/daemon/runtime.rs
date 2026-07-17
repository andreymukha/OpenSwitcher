use crate::config::AppConfig;
use crate::daemon::capture::{CaptureEventOutcome, CaptureOwner, LayoutSwitchCaptureSession};
use crate::daemon::debug_log::{format_layout, try_debug_line, DebugLogKind};
use crate::daemon::input_snapshot::{
    InputRuntimeSnapshot, InputSnapshotPublication, LayoutRefreshRequests, RefreshRequestOutcome,
    SnapshotTryLoad, INPUT_LAYOUT_POLL_INTERVAL,
};
use crate::error::{
    CaptureError, ConfigError, ServiceManagerError, SettingsError, SystemContextError,
    ValidationError,
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
    estimated_correction_schedule_ms, DesktopEnvironment, DetectionConfidence, DistroKind,
    HotkeySpec, LayoutSwitchCaptureState, LayoutSwitchCombo, LayoutSwitchSetting, SessionType,
    Settings, SystemContext, UpdateSettingsResult, MAX_CORRECTION_EXTRA_BACKSPACES,
    MAX_CORRECTION_KEYSTROKES, MAX_CORRECTION_SCHEDULE_MS,
};
use crate::system::{SystemContextDetector, UserServiceController};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

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

impl RuntimeConfigSnapshot {
    pub(crate) fn estimated_correction_schedule(
        &self,
        strokes: usize,
        extra_backspaces: usize,
        switch_layout: bool,
    ) -> Result<Duration, ValidationError> {
        if strokes > MAX_CORRECTION_KEYSTROKES || extra_backspaces > MAX_CORRECTION_EXTRA_BACKSPACES
        {
            return Err(ValidationError::InputCorrectionPlanTooLarge {
                max_strokes: MAX_CORRECTION_KEYSTROKES,
                max_extra_backspaces: MAX_CORRECTION_EXTRA_BACKSPACES,
                strokes,
                extra_backspaces,
            });
        }

        let found_ms = estimated_correction_schedule_ms(
            strokes,
            extra_backspaces,
            self.layout_delay_ms,
            self.backspace_ms,
            self.typing_ms,
            switch_layout,
        )
        .unwrap_or(u64::MAX);
        if found_ms > MAX_CORRECTION_SCHEDULE_MS {
            return Err(ValidationError::InputCorrectionScheduleTooLong {
                max_ms: MAX_CORRECTION_SCHEDULE_MS,
                found_ms,
            });
        }

        Ok(Duration::from_millis(found_ms))
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
            log_layout_debug(
                "layout-switch-auto-detect",
                &format!("result=fallback error={error}"),
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
    use crate::daemon::capture::{CaptureEventDisposition, CaptureOwner, CAPTURE_SOFT_LEASE};
    use crate::daemon::input_snapshot::RefreshRequestOutcome;
    use crate::error::{LayoutAutoDetectError, ValidationError};
    use crate::layout_backend::{
        AppLayoutKind, BackendCapabilities, CurrentLayoutState, LayoutBackendError,
        LayoutBackendOperation, LayoutCode, LayoutStateSink, SystemLayout,
    };
    use crate::model::{
        default_manual_correction_hotkey, default_selected_text_hotkey, AutoDetectedLayoutSwitch,
        DesktopEnvironment, DetectionConfidence, DetectionStrategy, DistroKind,
        LayoutSwitchSetting, LayoutSwitchSource, SessionType,
    };
    use std::io;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };
    use tempfile::TempDir;

    #[test]
    fn runtime_snapshot_estimates_exact_bounded_correction_schedule() {
        let snapshot = RuntimeConfigSnapshot {
            auto_switch_enabled: true,
            fix_two_capitals: false,
            fix_accidental_caps_lock: false,
            layout_switch_combo: LayoutSwitchCombo::ctrl_shift(),
            layout_delay_ms: 500,
            backspace_ms: 4,
            typing_ms: 5,
            manual_correction_hotkey: default_manual_correction_hotkey(),
            selected_text_hotkey: default_selected_text_hotkey(),
        };

        assert_eq!(
            snapshot.estimated_correction_schedule(128, 1, true),
            Ok(Duration::from_millis(2_818))
        );
    }

    #[test]
    fn runtime_snapshot_rejects_correction_plan_above_work_limits() {
        let snapshot = RuntimeConfigSnapshot {
            auto_switch_enabled: true,
            fix_two_capitals: false,
            fix_accidental_caps_lock: false,
            layout_switch_combo: LayoutSwitchCombo::ctrl_shift(),
            layout_delay_ms: 500,
            backspace_ms: 4,
            typing_ms: 5,
            manual_correction_hotkey: default_manual_correction_hotkey(),
            selected_text_hotkey: default_selected_text_hotkey(),
        };

        for (strokes, extra_backspaces) in [(129, 1), (128, 2)] {
            assert!(matches!(
                snapshot.estimated_correction_schedule(strokes, extra_backspaces, true),
                Err(ValidationError::InputCorrectionPlanTooLarge { .. })
            ));
        }
    }

    #[test]
    fn runtime_snapshot_rejects_schedule_above_wall_budget() {
        let snapshot = RuntimeConfigSnapshot {
            auto_switch_enabled: true,
            fix_two_capitals: false,
            fix_accidental_caps_lock: false,
            layout_switch_combo: LayoutSwitchCombo::ctrl_shift(),
            layout_delay_ms: 500,
            backspace_ms: 10,
            typing_ms: 10,
            manual_correction_hotkey: default_manual_correction_hotkey(),
            selected_text_hotkey: default_selected_text_hotkey(),
        };

        assert!(matches!(
            snapshot.estimated_correction_schedule(128, 1, true),
            Err(ValidationError::InputCorrectionScheduleTooLong { .. })
        ));
    }

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

    struct BlockingSnapshotBackend {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl BlockingSnapshotBackend {
        fn new(entered: Arc<Barrier>, release: Arc<Barrier>) -> Self {
            Self { entered, release }
        }
    }

    impl LayoutBackend for BlockingSnapshotBackend {
        fn id(&self) -> &'static str {
            "blocking-test-backend"
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
            self.entered.wait();
            self.release.wait();
            Ok(known_layout_state(english_layout()))
        }

        fn switch_to(&mut self, _target: &SystemLayout) -> Result<(), LayoutBackendError> {
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

    struct PanickingSnapshotBackend;

    impl LayoutBackend for PanickingSnapshotBackend {
        fn id(&self) -> &'static str {
            "panicking-test-backend"
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
            panic!("injected layout refresh panic");
        }

        fn switch_to(&mut self, _target: &SystemLayout) -> Result<(), LayoutBackendError> {
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

    fn british_english_layout() -> SystemLayout {
        SystemLayout {
            backend_key: "gb".to_string(),
            normalized_code: LayoutCode::Gb,
            display_name: "English (UK)".to_string(),
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
        test_runtime_with_backend_and_context(
            initial_layout_state,
            backend,
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Xfce,
                distro: DistroKind::LinuxMint,
            },
        )
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
        let feature_availability = FeatureAvailability {
            auto_switch: false,
            manual_word_fix: false,
            selected_text_switch: true,
            reason: Some("test".to_string()),
        };
        let initial_input_snapshot = initial_input_snapshot(
            &config_service,
            enabled,
            feature_availability.clone(),
            system_context.session_type,
            initial_layout_state.clone(),
        );
        let (layout_refresh_requests, layout_refresh_receiver) = LayoutRefreshRequests::new();

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
            feature_availability: RwLock::new(feature_availability),
            system_context: RwLock::new(system_context),
            current_layout_observation: RwLock::new(None),
            config_service,
            settings_update_gate: Mutex::new(()),
            input_snapshot: InputSnapshotPublication::new(initial_input_snapshot),
            layout_invalidation_epoch: AtomicU64::new(0),
            layout_refresh_requests,
            layout_refresh_receiver: Mutex::new(Some(layout_refresh_receiver)),
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
    fn layout_refresh_backend_error_preserves_value_without_extending_freshness() {
        let confirmed_at = Instant::now();
        let initial = known_layout_state(english_layout());
        let runtime = test_runtime_with_backend(
            initial.clone(),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::Error,
            }),
        );
        runtime.force_input_confirmation_for_test(confirmed_at);

        assert_eq!(
            runtime.refresh_and_publish_layout(),
            BackendSyncResult::Skipped
        );
        let snapshot = runtime.input_snapshot_before_grab();
        assert_eq!(snapshot.confirmed_at, Some(confirmed_at));
        assert_eq!(snapshot.layout_state, initial);
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
    fn layout_refresh_spawn_failure_disconnects_without_blocking_snapshot() {
        let runtime = Arc::new(test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        ));
        let error = runtime
            .start_layout_refresh_coordinator_with(|job| {
                drop(job);
                Err(io::Error::other("injected spawn failure"))
            })
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(matches!(
            runtime.try_input_snapshot(),
            SnapshotTryLoad::Loaded(_)
        ));
        assert_eq!(
            runtime.request_layout_refresh(),
            RefreshRequestOutcome::Unavailable
        );
    }

    #[test]
    fn layout_refresh_invalidation_advances_epoch_before_queueing() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );
        let before = runtime.input_layout_epoch();

        runtime.invalidate_layout_and_request_refresh("test");

        assert_eq!(runtime.input_layout_epoch(), before + 1);
        assert_eq!(
            runtime
                .input_snapshot_before_grab()
                .layout_status_at(Instant::now(), runtime.input_layout_epoch()),
            crate::daemon::input_snapshot::InputLayoutStatus::AwaitingConfirmation
        );
    }

    #[test]
    fn layout_refresh_panicking_coordinator_degrades_to_snapshot_only() {
        let runtime = Arc::new(test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(PanickingSnapshotBackend),
        ));
        runtime
            .start_layout_refresh_coordinator_with(|job| {
                thread::Builder::new()
                    .name("test-layout-refresh-panic".to_string())
                    .spawn(job)
                    .map(|_| ())
            })
            .unwrap();
        assert_eq!(
            runtime.request_layout_refresh(),
            RefreshRequestOutcome::Queued
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.background_sync_started.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }

        assert!(!runtime.background_sync_started.load(Ordering::Acquire));
        assert!(matches!(
            runtime.try_input_snapshot(),
            SnapshotTryLoad::Loaded(_)
        ));
        assert_eq!(
            runtime.request_layout_refresh(),
            RefreshRequestOutcome::Unavailable
        );
    }

    #[test]
    fn layout_refresh_blocked_backend_does_not_block_snapshot_reads() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let runtime = Arc::new(test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(BlockingSnapshotBackend::new(
                Arc::clone(&entered),
                Arc::clone(&release),
            )),
        ));
        let worker_runtime = Arc::clone(&runtime);
        let worker = thread::spawn(move || worker_runtime.refresh_and_publish_layout());
        entered.wait();

        assert!(matches!(
            runtime.try_input_snapshot(),
            SnapshotTryLoad::Loaded(_)
        ));

        release.wait();
        assert_eq!(worker.join().unwrap(), BackendSyncResult::Unchanged);
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

    fn trusted_gnome_gb_sources() -> Vec<String> {
        gnome_sources(&[("xkb", "gb"), ("xkb", "ru")])
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

    #[derive(Clone)]
    struct CinnamonInputSourceReaderStub {
        calls: Arc<AtomicUsize>,
        sources: Result<Vec<CinnamonInputSource>, String>,
    }

    impl CinnamonInputSourceReader for CinnamonInputSourceReaderStub {
        fn cinnamon_input_sources(&self) -> Result<Vec<CinnamonInputSource>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.sources.clone()
        }
    }

    fn cinnamon_input_source(source_id: &str, index: i32, active: bool) -> CinnamonInputSource {
        CinnamonInputSource {
            source_type: "xkb".to_string(),
            source_id: source_id.to_string(),
            index,
            layout_code: source_id.to_string(),
            active,
        }
    }

    fn cinnamon_sources(active_source_id: &str) -> Vec<CinnamonInputSource> {
        vec![
            cinnamon_input_source("us", 0, active_source_id == "us"),
            cinnamon_input_source("ru", 1, active_source_id == "ru"),
        ]
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
        let layout_state = CurrentLayoutState::Unknown {
            reason: "test".to_string(),
        };
        let feature_availability = FeatureAvailability {
            auto_switch: false,
            manual_word_fix: false,
            selected_text_switch: true,
            reason: Some("test".to_string()),
        };
        let config_service = ConfigService {
            config_path: PathBuf::from("test-config.toml"),
            inner: RwLock::new(config),
        };
        let initial_input_snapshot = initial_input_snapshot(
            &config_service,
            enabled,
            feature_availability.clone(),
            system_context.session_type,
            layout_state.clone(),
        );
        let (layout_refresh_requests, layout_refresh_receiver) = LayoutRefreshRequests::new();
        RuntimeState {
            enabled: AtomicBool::new(enabled),
            should_exit: AtomicBool::new(false),
            hotkey_capture_inhibition_started_at: Instant::now(),
            settings_hotkey_capture_inhibited_until_ms: AtomicU64::new(0),
            layout_state: RwLock::new(layout_state),
            backend: Mutex::new(Some(Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(CurrentLayoutState::Unknown {
                    reason: "test".to_string(),
                }),
            }))),
            layout_setup: RwLock::new(LayoutSetup::Unsupported {
                reason: "test".to_string(),
            }),
            layout_compatibility: RwLock::new(LayoutCompatibility::Unsupported),
            feature_availability: RwLock::new(feature_availability),
            system_context: RwLock::new(system_context),
            current_layout_observation: RwLock::new(None),
            config_service,
            settings_update_gate: Mutex::new(()),
            input_snapshot: InputSnapshotPublication::new(initial_input_snapshot),
            layout_invalidation_epoch: AtomicU64::new(0),
            layout_refresh_requests,
            layout_refresh_receiver: Mutex::new(Some(layout_refresh_receiver)),
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
    fn cinnamon_x11_current_layout_observation_reads_active_us_as_english() {
        assert_eq!(
            cinnamon_x11_current_layout_state_from_sources(&cinnamon_sources("us")),
            known_layout_state(english_layout())
        );
    }

    #[test]
    fn cinnamon_x11_current_layout_observation_reads_active_ru_as_russian() {
        assert_eq!(
            cinnamon_x11_current_layout_state_from_sources(&cinnamon_sources("ru")),
            known_layout_state(russian_layout())
        );
    }

    #[test]
    fn cinnamon_x11_current_layout_observation_rejects_unknown_active_source() {
        let sources = vec![
            cinnamon_input_source("us", 0, false),
            cinnamon_input_source("de", 1, true),
        ];

        assert_untrusted_observation(Some(cinnamon_x11_current_layout_state_from_sources(
            &sources,
        )));
    }

    #[test]
    fn current_layout_state_prefers_cinnamon_x11_observation() {
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
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "ru"), ("xkb", "us")])),
        };
        let cinnamon_reader = CinnamonInputSourceReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Ok(cinnamon_sources("us")),
        };

        runtime.refresh_current_layout_observation_with_readers(&reader, &cinnamon_reader);

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
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
    fn gnome_wayland_without_observation_rejects_untrusted_legacy_english_for_auto_correction() {
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
            gnome_wayland_context(),
        );

        assert!(matches!(
            runtime.current_layout_state(),
            CurrentLayoutState::Unknown { .. }
        ));
        assert_eq!(
            runtime.auto_correction_layout_kind(),
            AppLayoutKind::Unknown
        );
        assert!(runtime.feature_availability().selected_text_switch);
    }

    #[test]
    fn gnome_wayland_without_observation_rejects_untrusted_legacy_russian_for_auto_correction() {
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

        assert!(matches!(
            runtime.current_layout_state(),
            CurrentLayoutState::Unknown { .. }
        ));
        assert_eq!(
            runtime.auto_correction_layout_kind(),
            AppLayoutKind::Unknown
        );
        assert!(runtime.feature_availability().selected_text_switch);
    }

    #[test]
    fn non_cinnamon_x11_keeps_untrusted_legacy_state_behavior_unchanged() {
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
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Xfce,
                distro: DistroKind::LinuxMint,
            },
        );

        assert_eq!(
            runtime.current_layout_state(),
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            }
        );
        assert_eq!(
            runtime.auto_correction_layout_kind(),
            AppLayoutKind::English
        );
    }

    #[test]
    fn cinnamon_x11_without_observation_rejects_untrusted_legacy_state() {
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
            cinnamon_x11_context(),
        );

        assert!(matches!(
            runtime.current_layout_state(),
            CurrentLayoutState::Unknown { .. }
        ));
        assert_eq!(
            runtime.auto_correction_layout_kind(),
            AppLayoutKind::Unknown
        );
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
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Xfce,
                distro: DistroKind::LinuxMint,
            },
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
    fn refresh_current_layout_observation_uses_cinnamon_reader_for_cinnamon_x11() {
        let gsettings_calls = Arc::new(AtomicUsize::new(0));
        let cinnamon_calls = Arc::new(AtomicUsize::new(0));
        let reader = LayoutObservationReaderStub {
            calls: Arc::clone(&gsettings_calls),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };
        let cinnamon_reader = CinnamonInputSourceReaderStub {
            calls: Arc::clone(&cinnamon_calls),
            sources: Ok(cinnamon_sources("ru")),
        };
        let runtime = test_runtime_with_backend_and_context(
            CurrentLayoutState::Known {
                layout: english_layout(),
                trustworthy: false,
            },
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
            cinnamon_x11_context(),
        );

        runtime.refresh_current_layout_observation_with_readers(&reader, &cinnamon_reader);

        assert_eq!(gsettings_calls.load(Ordering::SeqCst), 0);
        assert_eq!(cinnamon_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(russian_layout())
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
    fn gnome_wayland_observation_trusts_gb_ru_sources_with_gb_current_mru() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_gb_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "gb"), ("xkb", "ru")])),
        };

        assert_eq!(
            gnome_wayland_current_layout_state(&reader),
            Some(known_layout_state(british_english_layout()))
        );
    }

    #[test]
    fn gnome_wayland_observation_trusts_gb_ru_sources_with_ru_current_mru() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_gb_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "ru"), ("xkb", "gb")])),
        };

        assert_eq!(
            gnome_wayland_current_layout_state(&reader),
            Some(known_layout_state(russian_layout()))
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
    fn gnome_wayland_observation_rejects_configured_gb_variant() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(gnome_sources(&[("xkb", "gb+intl"), ("xkb", "ru")])),
            mru_sources: Some(gnome_sources(&[("xkb", "gb+intl"), ("xkb", "ru")])),
        };

        assert_untrusted_observation(gnome_wayland_current_layout_state(&reader));
    }

    #[test]
    fn gnome_wayland_observation_rejects_more_than_two_configured_xkb_sources() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(gnome_sources(&[
                ("xkb", "us"),
                ("xkb", "ru"),
                ("xkb", "de"),
            ])),
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
    fn periodic_sync_tick_uses_cinnamon_observation_for_cinnamon_x11() {
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };
        let detector = SystemContextDetectorStub {
            calls: Arc::new(AtomicUsize::new(0)),
            context: cinnamon_x11_context(),
        };
        let cinnamon_reader = CinnamonInputSourceReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Ok(cinnamon_sources("ru")),
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
            cinnamon_x11_context(),
        );

        assert_eq!(
            runtime.periodic_sync_tick_with_readers(&reader, &detector, &cinnamon_reader),
            BackendSyncResult::Unchanged
        );

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(russian_layout())
        );
        assert!(runtime.take_pending_status_change());
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
    fn wayland_focus_switch_policy_uses_late_upgraded_runtime_context() {
        let detector = SystemContextDetectorStub {
            calls: Arc::new(AtomicUsize::new(0)),
            context: gnome_wayland_context(),
        };
        let runtime = runtime_with_config_and_context(
            AppConfig::default(),
            SystemContext {
                session_type: SessionType::X11,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            },
        );
        let mut modifiers = crate::daemon::keyboard::ModifierState::default();
        modifiers.update(evdev::Key::KEY_LEFTALT, 1);

        assert!(
            !crate::daemon::service::should_invalidate_for_wayland_focus_switch_shortcut(
                runtime.session_type(),
                modifiers,
                evdev::Key::KEY_TAB,
                1,
            )
        );

        assert!(runtime.refresh_system_context_with_detector(&detector));

        assert!(
            crate::daemon::service::should_invalidate_for_wayland_focus_switch_shortcut(
                runtime.session_type(),
                modifiers,
                evdev::Key::KEY_TAB,
                1,
            )
        );
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
    fn periodic_sync_tick_late_upgrades_cinnamon_x11_before_first_autocorrect() {
        let detector_calls = Arc::new(AtomicUsize::new(0));
        let detector = SystemContextDetectorStub {
            calls: Arc::clone(&detector_calls),
            context: cinnamon_x11_context(),
        };
        let reader_calls = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            calls: Arc::clone(&reader_calls),
            combo: LayoutSwitchCombo::super_space(),
        };
        let mut config = AppConfig::default();
        config.layout.switch_combo = LayoutSwitchCombo::ctrl_shift();
        config.layout.switch_source = LayoutSwitchSource::AutoFallback;
        config.layout.auto_detected = AutoDetectedLayoutSwitch {
            strategy: DetectionStrategy::NoSupportedStrategy,
            confidence: DetectionConfidence::Unsupported,
            context: SystemContext {
                session_type: SessionType::Unknown,
                desktop_environment: DesktopEnvironment::Unknown,
                distro: DistroKind::LinuxMint,
            },
        };
        let runtime = runtime_with_config_and_context(
            config,
            SystemContext {
                session_type: SessionType::Unknown,
                desktop_environment: DesktopEnvironment::Unknown,
                distro: DistroKind::LinuxMint,
            },
        );

        let _ = runtime.periodic_sync_tick_with(&reader, &detector);

        assert_eq!(runtime.system_context(), cinnamon_x11_context());
        assert_eq!(
            runtime.get_settings().unwrap().layout_switch,
            LayoutSwitchSetting {
                combo: LayoutSwitchCombo::super_space(),
                source: LayoutSwitchSource::AutoDetected,
                auto_detected: AutoDetectedLayoutSwitch {
                    strategy: DetectionStrategy::CinnamonX11GSettingsXkbOptions,
                    confidence: DetectionConfidence::High,
                    context: cinnamon_x11_context(),
                },
            }
        );
        assert_eq!(detector_calls.load(Ordering::SeqCst), 1);
        assert!(reader_calls.load(Ordering::SeqCst) >= 1);
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
    fn optimistic_gnome_wayland_uinput_update_switches_us_english_to_russian() {
        let runtime = test_runtime_with_backend_and_context(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
            gnome_wayland_context(),
        );
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };

        assert!(runtime.optimistic_gnome_wayland_uinput_layout_switch_with_reader(&reader));

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(russian_layout())
        );
    }

    #[test]
    fn optimistic_gnome_wayland_uinput_update_switches_russian_to_configured_gb() {
        let runtime = test_runtime_with_backend_and_context(
            known_layout_state(russian_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(russian_layout())),
            }),
            gnome_wayland_context(),
        );
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_gb_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "ru"), ("xkb", "gb")])),
        };

        assert!(runtime.optimistic_gnome_wayland_uinput_layout_switch_with_reader(&reader));

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(british_english_layout())
        );
    }

    #[test]
    fn optimistic_gnome_wayland_uinput_update_rejects_untrusted_sources() {
        let cases: &[(&[(&str, &str)], &[(&str, &str)])] = &[
            (
                &[("xkb", "us"), ("xkb", "ru")],
                &[("ibus", "mozc-jp"), ("xkb", "us")],
            ),
            (
                &[("xkb", "ru+phonetic"), ("xkb", "us")],
                &[("xkb", "ru+phonetic"), ("xkb", "us")],
            ),
            (
                &[("xkb", "us+dvorak"), ("xkb", "ru")],
                &[("xkb", "us+dvorak"), ("xkb", "ru")],
            ),
            (
                &[("xkb", "us+colemak"), ("xkb", "ru")],
                &[("xkb", "us+colemak"), ("xkb", "ru")],
            ),
            (
                &[("xkb", "us+intl"), ("xkb", "ru")],
                &[("xkb", "us+intl"), ("xkb", "ru")],
            ),
            (
                &[("xkb", "gb+intl"), ("xkb", "ru")],
                &[("xkb", "gb+intl"), ("xkb", "ru")],
            ),
            (
                &[("xkb", "us"), ("xkb", "ru"), ("xkb", "de")],
                &[("xkb", "us"), ("xkb", "ru")],
            ),
        ];

        for (sources, mru_sources) in cases {
            let runtime = test_runtime_with_backend_and_context(
                known_layout_state(english_layout()),
                Box::new(SnapshotBackend {
                    snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
                }),
                gnome_wayland_context(),
            );
            let reader = LayoutObservationReaderStub {
                calls: Arc::new(AtomicUsize::new(0)),
                sources: Some(gnome_sources(sources)),
                mru_sources: Some(gnome_sources(mru_sources)),
            };

            assert!(
                !runtime.optimistic_gnome_wayland_uinput_layout_switch_with_reader(&reader),
                "sources={sources:?} mru_sources={mru_sources:?}"
            );

            assert_eq!(
                runtime.current_layout_state(),
                known_layout_state(english_layout())
            );
        }
    }

    #[test]
    fn optimistic_gnome_wayland_uinput_update_rejects_non_gnome_wayland() {
        let runtime = test_runtime_with_backend_and_context(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
            cinnamon_x11_context(),
        );
        let reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };

        assert!(!runtime.optimistic_gnome_wayland_uinput_layout_switch_with_reader(&reader));

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
    }

    #[test]
    fn background_observation_can_reconcile_optimistic_gnome_wayland_update() {
        let runtime = test_runtime_with_backend_and_context(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
            gnome_wayland_context(),
        );
        let before_switch_reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };
        let reconciliation_reader = LayoutObservationReaderStub {
            calls: Arc::new(AtomicUsize::new(0)),
            sources: Some(trusted_gnome_sources()),
            mru_sources: Some(gnome_sources(&[("xkb", "us"), ("xkb", "ru")])),
        };

        assert!(runtime
            .optimistic_gnome_wayland_uinput_layout_switch_with_reader(&before_switch_reader));
        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(russian_layout())
        );

        runtime.refresh_current_layout_observation_with_reader(&reconciliation_reader);

        assert_eq!(
            runtime.current_layout_state(),
            known_layout_state(english_layout())
        );
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

    #[test]
    fn runtime_capture_routes_and_reports_terminal_state_atomically() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );
        let owner = CaptureOwner::from(":1.42");
        let now = Instant::now();
        runtime
            .start_layout_switch_capture_owned_at(owner, now)
            .unwrap();

        let outcome = runtime
            .route_layout_switch_capture_event_at(now, evdev::Key::KEY_A, 1)
            .unwrap();

        assert_eq!(outcome.disposition, CaptureEventDisposition::ForwardDirect);
        assert!(outcome
            .state_change
            .as_ref()
            .is_some_and(|state| !state.is_active()));
        assert!(!runtime.layout_switch_capture_state().unwrap().is_active());
    }

    #[test]
    fn runtime_capture_expiry_is_observed_without_input() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );
        let owner = CaptureOwner::from(":1.42");
        let now = Instant::now();
        runtime
            .start_layout_switch_capture_owned_at(owner, now)
            .unwrap();

        let state = runtime
            .expire_layout_switch_capture_at(now + CAPTURE_SOFT_LEASE)
            .unwrap()
            .expect("soft lease must expire without a key event");

        assert!(!state.is_active());
        assert!(!runtime.layout_switch_capture_state().unwrap().is_active());
    }

    #[test]
    fn runtime_capture_reset_clears_debt_for_backend_epoch_change() {
        let runtime = test_runtime_with_backend(
            known_layout_state(english_layout()),
            Box::new(SnapshotBackend {
                snapshot: SnapshotOutcome::State(known_layout_state(english_layout())),
            }),
        );
        let owner = CaptureOwner::from(":1.42");
        let now = Instant::now();
        runtime
            .start_layout_switch_capture_owned_at(owner.clone(), now)
            .unwrap();
        let press = runtime
            .route_layout_switch_capture_event_at(now, evdev::Key::KEY_LEFTCTRL, 1)
            .unwrap();
        assert_eq!(press.disposition, CaptureEventDisposition::Suppress);
        runtime
            .cancel_layout_switch_capture_owned_at(&owner, now)
            .unwrap();

        runtime.reset_layout_switch_capture_input_epoch().unwrap();
        let release = runtime
            .route_layout_switch_capture_event_at(now, evdev::Key::KEY_LEFTCTRL, 0)
            .unwrap();

        assert_eq!(release.disposition, CaptureEventDisposition::PassThrough);
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
    settings_update_gate: Mutex<()>,
    input_snapshot: InputSnapshotPublication,
    layout_invalidation_epoch: AtomicU64,
    layout_refresh_requests: LayoutRefreshRequests,
    layout_refresh_receiver: Mutex<Option<mpsc::Receiver<()>>>,
    capture_session: Mutex<LayoutSwitchCaptureSession>,
    background_sync_started: AtomicBool,
    pending_status_change: AtomicBool,
}

fn initial_input_snapshot(
    config_service: &ConfigService,
    enabled: bool,
    features: FeatureAvailability,
    session_type: SessionType,
    layout_state: CurrentLayoutState,
) -> InputRuntimeSnapshot {
    InputRuntimeSnapshot {
        config: config_service
            .snapshot()
            .unwrap_or_else(|_| RuntimeConfigSnapshot::from(&AppConfig::default())),
        enabled,
        features,
        session_type,
        layout_state,
        config_generation: 0,
        layout_generation: 0,
        confirmed_layout_epoch: 0,
        confirmed_at: None,
    }
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

trait CinnamonInputSourceReader {
    fn cinnamon_input_sources(&self) -> Result<Vec<CinnamonInputSource>, String>;
}

struct CommandCinnamonInputSourceReader;

type CinnamonInputSourceRow = (
    String,
    String,
    i32,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i32,
    bool,
);

impl CinnamonInputSourceReader for CommandCinnamonInputSourceReader {
    fn cinnamon_input_sources(&self) -> Result<Vec<CinnamonInputSource>, String> {
        let connection =
            zbus::blocking::Connection::session().map_err(|error| error.to_string())?;
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.Cinnamon",
            "/org/Cinnamon",
            "org.Cinnamon",
        )
        .map_err(|error| error.to_string())?;
        let rows: Vec<CinnamonInputSourceRow> = proxy
            .call("GetInputSources", &())
            .map_err(|error| error.to_string())?;

        Ok(rows
            .into_iter()
            .map(CinnamonInputSource::from_row)
            .collect())
    }
}

#[cfg(test)]
struct PreserveCinnamonInputSourceReader;

#[cfg(test)]
impl CinnamonInputSourceReader for PreserveCinnamonInputSourceReader {
    fn cinnamon_input_sources(&self) -> Result<Vec<CinnamonInputSource>, String> {
        Err("cinnamon reader unavailable in this path".to_string())
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

struct BackgroundSyncAliveGuard<'a> {
    started: &'a AtomicBool,
}

impl Drop for BackgroundSyncAliveGuard<'_> {
    fn drop(&mut self) {
        self.started.store(false, Ordering::Release);
    }
}

fn run_layout_refresh_loop(
    runtime: Arc<RuntimeState>,
    receiver: mpsc::Receiver<()>,
    periodic_refresh_enabled: bool,
) {
    let _alive = BackgroundSyncAliveGuard {
        started: &runtime.background_sync_started,
    };
    loop {
        let should_refresh = match receiver.recv_timeout(INPUT_LAYOUT_POLL_INTERVAL) {
            Ok(()) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => periodic_refresh_enabled,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if runtime.should_exit() {
            break;
        }
        if should_refresh {
            let _ = runtime.refresh_and_publish_layout();
        }
    }
}

impl RuntimeState {
    // Runtime state initialization and flags

    pub fn new(config_service: ConfigService) -> Self {
        let (backend, layout_state, layout_setup, layout_compatibility, feature_availability) =
            Self::initialize_layout_backend();
        let enabled = config_service.auto_switch_enabled().unwrap_or(true);
        let system_context = SystemContextDetector::detect_current().unwrap_or_default();
        let initial_input_snapshot = initial_input_snapshot(
            &config_service,
            enabled,
            feature_availability.clone(),
            system_context.session_type,
            layout_state.clone(),
        );
        let (layout_refresh_requests, layout_refresh_receiver) = LayoutRefreshRequests::new();
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
            system_context: RwLock::new(system_context),
            current_layout_observation: RwLock::new(None),
            config_service,
            settings_update_gate: Mutex::new(()),
            input_snapshot: InputSnapshotPublication::new(initial_input_snapshot),
            layout_invalidation_epoch: AtomicU64::new(0),
            layout_refresh_requests,
            layout_refresh_receiver: Mutex::new(Some(layout_refresh_receiver)),
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

    pub(crate) fn input_snapshot_before_grab(&self) -> InputRuntimeSnapshot {
        self.input_snapshot.load_before_grab()
    }

    pub(crate) fn try_input_snapshot(&self) -> SnapshotTryLoad {
        self.input_snapshot.try_load()
    }

    pub(crate) fn input_layout_epoch(&self) -> u64 {
        self.layout_invalidation_epoch.load(Ordering::Acquire)
    }

    pub(crate) fn request_layout_refresh(&self) -> RefreshRequestOutcome {
        self.layout_refresh_requests.request()
    }

    pub(crate) fn invalidate_layout_and_request_refresh(&self, reason: &str) {
        let epoch = self
            .layout_invalidation_epoch
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let outcome = self.request_layout_refresh();
        log_layout_debug(
            "input-layout-invalidated",
            &format!("reason={reason} epoch={epoch} request={outcome:?}"),
        );
    }

    #[cfg(test)]
    fn force_input_confirmation_for_test(&self, confirmed_at: Instant) {
        let epoch = self.input_layout_epoch();
        self.input_snapshot.update(|published| {
            published.confirmed_layout_epoch = epoch;
            published.confirmed_at = Some(confirmed_at);
        });
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
        let _gate = self
            .settings_update_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut settings = self.get_settings()?;
        settings.auto_switch_enabled = !settings.auto_switch_enabled;
        self.update_settings_under_gate(settings)?;
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

    pub(crate) fn session_type(&self) -> SessionType {
        self.system_context().session_type
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

    #[cfg(test)]
    fn optimistic_gnome_wayland_uinput_layout_switch_with_reader<R: DesktopSettingsReader>(
        &self,
        reader: &R,
    ) -> bool {
        if !is_gnome_wayland_context(self.system_context()) {
            log_layout_debug(
                "gnome-wayland-optimistic-layout-switch",
                "result=skipped reason=non-gnome-wayland-context",
            );
            return false;
        }

        let pair = match trusted_gnome_layout_pair_from_reader(reader) {
            Some(pair) => pair,
            None => return false,
        };

        let current_state = self.current_layout_state();
        let next_layout = match current_state {
            CurrentLayoutState::Known { layout, .. } => match layout.kind {
                AppLayoutKind::English => pair.russian_layout,
                AppLayoutKind::Russian => pair.english_layout,
                AppLayoutKind::Other | AppLayoutKind::Unknown => {
                    log_layout_debug(
                        "gnome-wayland-optimistic-layout-switch",
                        "result=skipped reason=current-layout-unsupported",
                    );
                    return false;
                }
            },
            CurrentLayoutState::Unknown { .. } => {
                log_layout_debug(
                    "gnome-wayland-optimistic-layout-switch",
                    "result=skipped reason=current-layout-unknown",
                );
                return false;
            }
        };

        let next_state = known_layout_state_from_layout(next_layout);
        let _ = self.update_current_layout_cache(
            next_state.clone(),
            "gnome-wayland-optimistic-layout-cache",
        );
        self.update_current_layout_observation(Some(next_state), "gnome-wayland-optimistic-uinput");
        true
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
        self.periodic_sync_tick_with_readers(
            &CommandDesktopSettingsReader,
            &SystemContextDetector,
            &CommandCinnamonInputSourceReader,
        )
    }

    #[cfg(test)]
    fn periodic_sync_tick_with<R: DesktopSettingsReader, D: SystemContextSource>(
        &self,
        reader: &R,
        detector: &D,
    ) -> BackendSyncResult {
        self.periodic_sync_tick_with_readers(reader, detector, &PreserveCinnamonInputSourceReader)
    }

    fn periodic_sync_tick_with_readers<
        R: DesktopSettingsReader,
        D: SystemContextSource,
        C: CinnamonInputSourceReader,
    >(
        &self,
        reader: &R,
        detector: &D,
        cinnamon_reader: &C,
    ) -> BackendSyncResult {
        if self.refresh_system_context_with_detector(detector) {
            self.redetect_layout_switch_after_context_upgrade(reader);
        }
        self.refresh_current_layout_observation_with_readers(reader, cinnamon_reader);
        self.sync_with_backend()
    }

    pub(crate) fn initial_input_refresh_before_grab(&self) -> BackendSyncResult {
        self.refresh_and_publish_layout()
    }

    fn refresh_and_publish_layout(&self) -> BackendSyncResult {
        let epoch_before = self.input_layout_epoch();
        let result = self.periodic_sync_tick();
        let epoch_after = self.input_layout_epoch();

        if !matches!(result, BackendSyncResult::Skipped) && epoch_before == epoch_after {
            self.publish_confirmed_layout(Instant::now(), epoch_after);
        }

        result
    }

    fn publish_confirmed_layout(&self, confirmed_at: Instant, confirmed_layout_epoch: u64) {
        let layout_state = self.current_layout_state();
        let features = self.feature_availability();
        let session_type = self.session_type();
        self.input_snapshot.update(|published| {
            if published.layout_state != layout_state {
                published.layout_generation = published.layout_generation.saturating_add(1);
            }
            published.layout_state = layout_state;
            published.features = features;
            published.session_type = session_type;
            published.confirmed_layout_epoch = confirmed_layout_epoch;
            published.confirmed_at = Some(confirmed_at);
        });
    }

    pub fn take_pending_status_change(&self) -> bool {
        self.pending_status_change.swap(false, Ordering::SeqCst)
    }

    pub fn clear_pending_status_change(&self) {
        self.pending_status_change.store(false, Ordering::SeqCst);
    }

    // Background sync and tray watchdog

    pub fn start_background_sync_polling(self: &Arc<Self>) {
        let result = self.start_layout_refresh_coordinator_with(|job| {
            thread::Builder::new()
                .name("layout-refresh-coordinator".to_string())
                .spawn(job)
                .map(|_| ())
        });
        if let Err(error) = result {
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
                INPUT_LAYOUT_POLL_INTERVAL.as_millis()
            ),
        );
    }

    fn start_layout_refresh_coordinator_with(
        self: &Arc<Self>,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let periodic_refresh_enabled = {
            let backend_guard = self
                .backend
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            backend_guard
                .as_ref()
                .is_some_and(|backend| background_sync_polling_enabled(backend.capabilities()))
        };
        let receiver = self
            .layout_refresh_receiver
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or_else(|| std::io::Error::other("layout refresh already started"))?;
        self.background_sync_started.store(true, Ordering::Release);
        let runtime = Arc::clone(self);
        let job =
            Box::new(move || run_layout_refresh_loop(runtime, receiver, periodic_refresh_enabled));
        if let Err(error) = spawn(job) {
            self.background_sync_started.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
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
        let _gate = self
            .settings_update_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.update_settings_under_gate(settings)
    }

    fn update_settings_under_gate(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsError> {
        let result = self.config_service.update_settings(settings)?;
        let snapshot = self.config_service.snapshot()?;
        self.enabled
            .store(settings.auto_switch_enabled, Ordering::SeqCst);
        self.input_snapshot.update(|published| {
            published.config = snapshot;
            published.enabled = settings.auto_switch_enabled;
            published.config_generation = published.config_generation.saturating_add(1);
        });
        Ok(result)
    }

    pub fn config_snapshot(&self) -> Result<RuntimeConfigSnapshot, SettingsError> {
        self.config_service.snapshot()
    }

    // Capture delegation

    pub fn start_layout_switch_capture_owned_at(
        &self,
        owner: CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        session.start_owned(owner, now)
    }

    pub fn renew_layout_switch_capture_owned_at(
        &self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        session.renew_owned(owner, now)
    }

    pub fn cancel_layout_switch_capture_owned_at(
        &self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        session.cancel_owned(owner, now)
    }

    pub fn finish_layout_switch_capture_owned_at(
        &self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        session.finish_owned(owner, now)
    }

    pub fn layout_switch_capture_owner_disappeared_at(
        &self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<Option<LayoutSwitchCaptureState>, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.owner_disappeared(owner, now))
    }

    pub fn expire_layout_switch_capture_at(
        &self,
        now: Instant,
    ) -> Result<Option<LayoutSwitchCaptureState>, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.expire_at(now))
    }

    pub fn route_layout_switch_capture_event_at(
        &self,
        now: Instant,
        key: evdev::Key,
        value: i32,
    ) -> Result<CaptureEventOutcome, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.route_event_at(now, key, value))
    }

    pub fn reset_layout_switch_capture_input_epoch(
        &self,
    ) -> Result<Option<LayoutSwitchCaptureState>, CaptureError> {
        let mut session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.reset_input_epoch())
    }

    pub fn layout_switch_capture_state(&self) -> Result<LayoutSwitchCaptureState, CaptureError> {
        let session = self
            .capture_session
            .lock()
            .map_err(|_| CaptureError::LockPoisoned)?;
        Ok(session.current_state())
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

    pub(crate) fn refresh_current_layout_observation(&self) {
        self.refresh_current_layout_observation_with_readers(
            &CommandDesktopSettingsReader,
            &CommandCinnamonInputSourceReader,
        );
    }

    #[cfg(test)]
    fn refresh_current_layout_observation_with_reader<R: DesktopSettingsReader>(&self, reader: &R) {
        self.refresh_current_layout_observation_with_readers(
            reader,
            &PreserveCinnamonInputSourceReader,
        );
    }

    fn refresh_current_layout_observation_with_readers<
        R: DesktopSettingsReader,
        C: CinnamonInputSourceReader,
    >(
        &self,
        reader: &R,
        cinnamon_reader: &C,
    ) {
        if !is_gnome_wayland_context(self.system_context()) {
            if is_cinnamon_x11_context(self.system_context()) {
                let Some(next_observation) = cinnamon_x11_current_layout_state(cinnamon_reader)
                else {
                    return;
                };
                self.update_current_layout_observation(Some(next_observation), "runtime-sync");
            } else {
                self.clear_current_layout_observation();
            }
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

fn is_cinnamon_x11_context(context: SystemContext) -> bool {
    context.session_type == SessionType::X11
        && context.desktop_environment == DesktopEnvironment::Cinnamon
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
    if is_gnome_wayland_context(context) || is_cinnamon_x11_context(context) {
        if let Some(observed_state) = observed_state {
            return observed_state.clone();
        }
        if matches!(
            raw_state,
            CurrentLayoutState::Known {
                trustworthy: false,
                ..
            }
        ) {
            return CurrentLayoutState::Unknown {
                reason: if is_gnome_wayland_context(context) {
                    "gnome-wayland-observation:missing-untrusted-legacy-fallback".to_string()
                } else {
                    "cinnamon-x11-observation:missing-untrusted-legacy-fallback".to_string()
                },
            };
        }
    }

    raw_state.clone()
}

// Cinnamon X11 observation

#[derive(Clone, Debug, PartialEq, Eq)]
struct CinnamonInputSource {
    source_type: String,
    source_id: String,
    index: i32,
    layout_code: String,
    active: bool,
}

impl CinnamonInputSource {
    fn from_row(row: CinnamonInputSourceRow) -> Self {
        Self {
            source_type: row.0,
            source_id: row.1,
            index: row.2,
            layout_code: row.4,
            active: row.11,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrustedCinnamonLayoutPair {
    english_layout: SystemLayout,
    russian_layout: SystemLayout,
    current_layout: SystemLayout,
}

fn cinnamon_x11_current_layout_state<R: CinnamonInputSourceReader>(
    reader: &R,
) -> Option<CurrentLayoutState> {
    let sources = match reader.cinnamon_input_sources() {
        Ok(sources) => sources,
        Err(error) => {
            log_layout_debug(
                "cinnamon-x11-observation",
                &format!("result=preserve reason=sources-read-error error={error}"),
            );
            return None;
        }
    };

    Some(cinnamon_x11_current_layout_state_from_sources(&sources))
}

fn cinnamon_x11_current_layout_state_from_sources(
    sources: &[CinnamonInputSource],
) -> CurrentLayoutState {
    match trusted_cinnamon_layout_pair_from_sources(sources) {
        Ok(pair) => known_layout_state_from_layout(pair.current_layout),
        Err(reason) => cinnamon_x11_unknown_layout_state(reason),
    }
}

fn trusted_cinnamon_layout_pair_from_sources(
    sources: &[CinnamonInputSource],
) -> Result<TrustedCinnamonLayoutPair, &'static str> {
    if sources.len() != 2 {
        return Err("unsupported-configured-sources");
    }

    let mut english_layout = None;
    let mut russian_layout = None;
    let mut current_layout = None;

    for source in sources {
        let Some(layout) = trusted_cinnamon_xkb_layout(source) else {
            return Err("unsupported-configured-sources");
        };

        if source.active {
            current_layout = Some(layout.clone());
        }

        match layout.kind {
            AppLayoutKind::English if english_layout.is_none() => english_layout = Some(layout),
            AppLayoutKind::Russian if russian_layout.is_none() => russian_layout = Some(layout),
            _ => return Err("unsupported-configured-sources"),
        }
    }

    Ok(TrustedCinnamonLayoutPair {
        english_layout: english_layout.ok_or("unsupported-configured-sources")?,
        russian_layout: russian_layout.ok_or("unsupported-configured-sources")?,
        current_layout: current_layout.ok_or("missing-active-source")?,
    })
}

fn trusted_cinnamon_xkb_layout(source: &CinnamonInputSource) -> Option<SystemLayout> {
    if source.source_type != "xkb" {
        return None;
    }

    let layout_code = if matches!(source.source_id.as_str(), "us" | "gb" | "ru") {
        source.source_id.as_str()
    } else if matches!(source.layout_code.as_str(), "us" | "gb" | "ru") {
        source.layout_code.as_str()
    } else {
        return None;
    };

    let (normalized_code, display_name, kind) = match layout_code {
        "us" => (
            LayoutCode::Us,
            "English".to_string(),
            AppLayoutKind::English,
        ),
        "gb" => (
            LayoutCode::Gb,
            "English (UK)".to_string(),
            AppLayoutKind::English,
        ),
        "ru" => (
            LayoutCode::Ru,
            "Russian".to_string(),
            AppLayoutKind::Russian,
        ),
        _ => return None,
    };

    Some(SystemLayout {
        backend_key: source.source_id.clone(),
        normalized_code,
        display_name,
        kind,
        index: u32::try_from(source.index).ok(),
    })
}

fn cinnamon_x11_unknown_layout_state(reason: &'static str) -> CurrentLayoutState {
    log_layout_debug(
        "cinnamon-x11-observation",
        &format!("result=unknown reason={reason}"),
    );
    CurrentLayoutState::Unknown {
        reason: format!("cinnamon-x11-observation:{reason}"),
    }
}

// GNOME Wayland observation

#[derive(Clone, Debug, PartialEq, Eq)]
struct GnomeInputSource {
    source_type: String,
    source_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrustedGnomeLayoutPair {
    pub english_layout: SystemLayout,
    pub russian_layout: SystemLayout,
    pub current_layout: SystemLayout,
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
        "gb" => (
            "gb".to_string(),
            LayoutCode::Gb,
            "English (UK)".to_string(),
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

#[cfg(test)]
fn trusted_gnome_layout_pair_from_reader<R: DesktopSettingsReader>(
    reader: &R,
) -> Option<TrustedGnomeLayoutPair> {
    let configured_sources =
        match reader.gsettings_string_list(GNOME_INPUT_SOURCES_SCHEMA, GNOME_SOURCES_KEY) {
            Ok(values) => values,
            Err(error) => {
                log_layout_debug(
                    "gnome-wayland-optimistic-layout-switch",
                    &format!("result=skipped reason=sources-read-error error={error}"),
                );
                return None;
            }
        };
    let mru_sources =
        match reader.gsettings_string_list(GNOME_INPUT_SOURCES_SCHEMA, GNOME_MRU_SOURCES_KEY) {
            Ok(values) => values,
            Err(error) => {
                log_layout_debug(
                    "gnome-wayland-optimistic-layout-switch",
                    &format!("result=skipped reason=mru-read-error error={error}"),
                );
                return None;
            }
        };

    match trusted_gnome_layout_pair_from_sources(&configured_sources, &mru_sources) {
        Ok(pair) => Some(pair),
        Err(reason) => {
            log_layout_debug(
                "gnome-wayland-optimistic-layout-switch",
                &format!("result=skipped reason={reason}"),
            );
            None
        }
    }
}

fn gnome_wayland_current_layout_state_from_sources(
    configured_sources: &[String],
    mru_sources: &[String],
) -> CurrentLayoutState {
    match trusted_gnome_layout_pair_from_sources(configured_sources, mru_sources) {
        Ok(pair) => known_layout_state_from_layout(pair.current_layout),
        Err(reason) => gnome_wayland_unknown_layout_state(reason),
    }
}

pub(crate) fn trusted_gnome_layout_pair_from_sources(
    configured_sources: &[String],
    mru_sources: &[String],
) -> Result<TrustedGnomeLayoutPair, &'static str> {
    let configured_sources = gnome_input_sources_from_flat_values(configured_sources)?;
    if configured_sources.len() != 2 {
        return Err("unsupported-configured-sources");
    }

    let mut english_layout = None;
    let mut russian_layout = None;
    for (index, source) in configured_sources.iter().enumerate() {
        match trusted_gnome_xkb_layout(source, index as u32) {
            Some(layout) if layout.kind == AppLayoutKind::English && english_layout.is_none() => {
                english_layout = Some(layout);
            }
            Some(layout) if layout.kind == AppLayoutKind::Russian && russian_layout.is_none() => {
                russian_layout = Some(layout);
            }
            _ => return Err("unsupported-configured-sources"),
        }
    }

    let english_layout = english_layout.ok_or("unsupported-configured-sources")?;
    let russian_layout = russian_layout.ok_or("unsupported-configured-sources")?;

    let mru_sources = match gnome_input_sources_from_flat_values(mru_sources) {
        Ok(sources) if !sources.is_empty() => sources,
        Ok(_) => return Err("empty-mru-sources"),
        Err(reason) => return Err(reason),
    };

    let current = &mru_sources[0];
    let current_layout = if gnome_input_source_matches_layout(current, &english_layout) {
        english_layout.clone()
    } else if gnome_input_source_matches_layout(current, &russian_layout) {
        russian_layout.clone()
    } else {
        return Err("unsupported-current-source");
    };

    Ok(TrustedGnomeLayoutPair {
        english_layout,
        russian_layout,
        current_layout,
    })
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

fn known_layout_state_from_layout(layout: SystemLayout) -> CurrentLayoutState {
    CurrentLayoutState::Known {
        layout,
        trustworthy: true,
    }
}

fn trusted_gnome_xkb_layout(source: &GnomeInputSource, index: u32) -> Option<SystemLayout> {
    let (normalized_code, display_name, kind) =
        match (source.source_type.as_str(), source.source_id.as_str()) {
            ("xkb", "us") => (
                LayoutCode::Us,
                "English".to_string(),
                AppLayoutKind::English,
            ),
            ("xkb", "gb") => (
                LayoutCode::Gb,
                "English (UK)".to_string(),
                AppLayoutKind::English,
            ),
            ("xkb", "ru") => (
                LayoutCode::Ru,
                "Russian".to_string(),
                AppLayoutKind::Russian,
            ),
            _ => return None,
        };

    Some(SystemLayout {
        backend_key: source.source_id.clone(),
        normalized_code,
        display_name,
        kind,
        index: Some(index),
    })
}

fn gnome_input_source_matches_layout(source: &GnomeInputSource, layout: &SystemLayout) -> bool {
    source.source_type == "xkb"
        && layout
            .normalized_code
            .normalized_str()
            .is_some_and(|code| source.source_id == code)
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
    let _ = try_debug_line(DebugLogKind::Layout, || format_layout(stage, details));
}

fn layout_label(is_english: bool) -> &'static str {
    if is_english {
        "EN"
    } else {
        "RU"
    }
}

#[cfg(test)]
mod input_snapshot_config_tests {
    use super::*;
    use crate::daemon::input_snapshot::SnapshotTryLoad;
    use tempfile::TempDir;

    fn test_runtime_with_config_path(config_path: PathBuf) -> RuntimeState {
        let config = AppConfig::default();
        let enabled = config.features.auto_switch_enabled;
        let layout_state = CurrentLayoutState::Unknown {
            reason: "test".to_string(),
        };
        let feature_availability = FeatureAvailability {
            auto_switch: false,
            manual_word_fix: false,
            selected_text_switch: true,
            reason: Some("test".to_string()),
        };
        let system_context = SystemContext::default();
        let config_service = ConfigService {
            config_path,
            inner: RwLock::new(config),
        };
        let initial_input_snapshot = initial_input_snapshot(
            &config_service,
            enabled,
            feature_availability.clone(),
            system_context.session_type,
            layout_state.clone(),
        );
        let (layout_refresh_requests, layout_refresh_receiver) = LayoutRefreshRequests::new();
        RuntimeState {
            enabled: AtomicBool::new(enabled),
            should_exit: AtomicBool::new(false),
            hotkey_capture_inhibition_started_at: Instant::now(),
            settings_hotkey_capture_inhibited_until_ms: AtomicU64::new(0),
            layout_state: RwLock::new(layout_state),
            backend: Mutex::new(None),
            layout_setup: RwLock::new(LayoutSetup::Unsupported {
                reason: "test".to_string(),
            }),
            layout_compatibility: RwLock::new(LayoutCompatibility::Unsupported),
            feature_availability: RwLock::new(feature_availability),
            system_context: RwLock::new(system_context),
            current_layout_observation: RwLock::new(None),
            config_service,
            settings_update_gate: Mutex::new(()),
            input_snapshot: InputSnapshotPublication::new(initial_input_snapshot),
            layout_invalidation_epoch: AtomicU64::new(0),
            layout_refresh_requests,
            layout_refresh_receiver: Mutex::new(Some(layout_refresh_receiver)),
            capture_session: Mutex::new(LayoutSwitchCaptureSession::default()),
            background_sync_started: AtomicBool::new(false),
            pending_status_change: AtomicBool::new(false),
        }
    }

    #[test]
    fn held_config_write_lock_does_not_block_input_snapshot_read() {
        let runtime = test_runtime_with_config_path(PathBuf::from("test-config.toml"));
        let _config_guard = runtime.config_service.inner.write().unwrap();

        assert!(matches!(
            runtime.try_input_snapshot(),
            SnapshotTryLoad::Loaded(_)
        ));
    }

    #[test]
    fn failed_settings_save_does_not_publish_new_config_generation() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join("config-as-directory");
        std::fs::create_dir(&config_path).unwrap();
        let runtime = test_runtime_with_config_path(config_path);
        let before = runtime.input_snapshot_before_grab();
        let mut settings = runtime.get_settings().unwrap();
        settings.fix_two_capitals = !settings.fix_two_capitals;

        assert!(runtime.update_settings(settings).is_err());
        let after = runtime.input_snapshot_before_grab();
        assert_eq!(after.config_generation, before.config_generation);
        assert_eq!(
            after.config.fix_two_capitals,
            before.config.fix_two_capitals
        );
    }

    #[test]
    fn successful_settings_save_publishes_one_complete_generation() {
        let temp = TempDir::new().unwrap();
        let runtime = test_runtime_with_config_path(temp.path().join("config.toml"));
        let before = runtime.input_snapshot_before_grab();
        let mut settings = runtime.get_settings().unwrap();
        settings.fix_two_capitals = true;

        runtime.update_settings(settings).unwrap();
        let after = runtime.input_snapshot_before_grab();
        assert_eq!(after.config_generation, before.config_generation + 1);
        assert!(after.config.fix_two_capitals);
    }

    #[test]
    fn toggles_publish_one_committed_generation_each() {
        let temp = TempDir::new().unwrap();
        let runtime = test_runtime_with_config_path(temp.path().join("config.toml"));
        let before = runtime.input_snapshot_before_grab();

        assert!(!runtime.toggle_enabled_result().unwrap());
        assert!(runtime.toggle_enabled_result().unwrap());

        let after = runtime.input_snapshot_before_grab();
        assert_eq!(after.config_generation, before.config_generation + 2);
        assert_eq!(after.enabled, runtime.is_enabled());
    }
}
