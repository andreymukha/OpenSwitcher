use crate::config::AppConfig;
use crate::error::{ConfigError, SettingsError};
use crate::model::{LayoutSwitchKey, Settings, UndoKey, UpdateSettingsResult};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

#[derive(Clone, Debug)]
pub struct RuntimeConfigSnapshot {
    pub layout_keys: Vec<LayoutSwitchKey>,
    pub layout_delay_ms: u64,
    pub backspace_ms: u64,
    pub typing_ms: u64,
    pub undo_key: UndoKey,
}

impl From<&AppConfig> for RuntimeConfigSnapshot {
    fn from(value: &AppConfig) -> Self {
        Self {
            layout_keys: value.layout.keys.clone(),
            layout_delay_ms: value.layout.delay_ms as u64,
            backspace_ms: value.delays.backspace_ms as u64,
            typing_ms: value.delays.typing_ms as u64,
            undo_key: value.features.undo_key,
        }
    }
}

pub struct ConfigService {
    config_path: PathBuf,
    inner: RwLock<AppConfig>,
}

impl ConfigService {
    pub fn load(config_path: PathBuf) -> Result<Self, ConfigError> {
        let config = AppConfig::load_or_create(&config_path)?;
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

pub struct RuntimeState {
    enabled: AtomicBool,
    layout_is_english: AtomicBool,
    config_service: ConfigService,
}

impl RuntimeState {
    pub fn new(config_service: ConfigService) -> Self {
        Self {
            enabled: AtomicBool::new(true),
            layout_is_english: AtomicBool::new(true),
            config_service,
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
        self.layout_is_english.load(Ordering::SeqCst)
    }

    pub fn set_layout(&self, layout_is_english: bool) {
        self.layout_is_english
            .store(layout_is_english, Ordering::SeqCst);
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
}
