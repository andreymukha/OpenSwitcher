use open_switcher::config::AppConfig;
use open_switcher::daemon::runtime::{ConfigService, RuntimeState};
use open_switcher::dbus::{
    OpenSwitcherDbusApi, OpenSwitcherProxyBlocking, INTERFACE_NAME, OBJECT_PATH,
};
use open_switcher::model::{
    AutoDetectedLayoutSwitch, LayoutSwitchCapturePhase, LayoutSwitchCaptureState,
    LayoutSwitchCombo, LayoutSwitchSetting, LayoutSwitchSource, SelectedTextHotkey, SettingsDto,
    UndoKey, UpdateSettingsResult,
};
use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;
use zbus::blocking::{Connection, ConnectionBuilder, Proxy};

#[test]
fn dbus_roundtrip_updates_runtime_and_config() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("roundtrip");
    let _service = spawn_service(&config_path, &service_name)?;
    let initial_config = AppConfig::load_or_create(&config_path)?;

    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;

    let initial: SettingsDto = proxy.call("GetSettings", &())?;
    assert!(initial.auto_switch_enabled);
    assert_eq!(
        proxy.get_property::<bool>("IsEnabled")?,
        initial.auto_switch_enabled
    );
    assert_eq!(initial.layout_delay_ms, 30);
    assert_eq!(initial.undo_key, UndoKey::Pause);
    assert_eq!(initial.selected_text_hotkey, SelectedTextHotkey::ShiftPause);
    assert_eq!(initial.layout_switch, initial_config.settings().layout_switch);

    let result: UpdateSettingsResult = proxy.call(
        "UpdateSettings",
        &(SettingsDto {
            auto_switch_enabled: false,
            fix_two_capitals: false,
            fix_accidental_caps_lock: false,
            layout_delay_ms: 77,
            undo_key: UndoKey::F12,
            selected_text_hotkey: SelectedTextHotkey::AltF12,
            layout_switch: LayoutSwitchSetting {
                combo: LayoutSwitchCombo::alt_shift(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            },
        }),
    )?;
    assert!(!result.restart_required);

    let updated: SettingsDto = proxy.call("GetSettings", &())?;
    assert!(!updated.auto_switch_enabled);
    assert_eq!(updated.layout_delay_ms, 77);
    assert_eq!(updated.undo_key, UndoKey::F12);
    assert_eq!(updated.selected_text_hotkey, SelectedTextHotkey::AltF12);
    assert_eq!(updated.layout_switch.combo, LayoutSwitchCombo::alt_shift());
    assert_eq!(updated.layout_switch.source, LayoutSwitchSource::Manual);
    let reloaded_client = Connection::session()?;
    let reloaded_proxy = settings_proxy(&reloaded_client, &service_name)?;
    assert_eq!(
        reloaded_proxy.get_property::<bool>("IsEnabled")?,
        updated.auto_switch_enabled
    );

    let config = AppConfig::load_or_create(&config_path)?;
    assert!(!config.features.auto_switch_enabled);
    assert_eq!(config.layout.delay_ms, 77);
    assert_eq!(config.features.undo_key, UndoKey::F12);
    assert_eq!(
        config.features.selected_text_switch_hotkey,
        SelectedTextHotkey::AltF12
    );
    assert_eq!(config.layout.switch_combo, LayoutSwitchCombo::alt_shift());

    Ok(())
}

#[test]
fn dbus_rejects_invalid_settings() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("validation");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;

    let error = proxy
        .call_method(
            "UpdateSettings",
            &(SettingsDto {
                auto_switch_enabled: true,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                layout_delay_ms: 999,
                undo_key: UndoKey::Pause,
                selected_text_hotkey: SelectedTextHotkey::ShiftPause,
                layout_switch: LayoutSwitchSetting {
                    combo: LayoutSwitchCombo::ctrl_shift(),
                    source: LayoutSwitchSource::Manual,
                    auto_detected: AutoDetectedLayoutSwitch::default(),
                },
            }),
        )
        .expect_err("invalid settings must be rejected");

    let error_text = error.to_string();
    assert!(error_text.contains("Задержка переключения"));

    let config = AppConfig::load_or_create(&config_path)?;
    assert_eq!(config.layout.delay_ms, 30);
    assert_eq!(config.features.undo_key, UndoKey::Pause);
    assert_eq!(
        config.features.selected_text_switch_hotkey,
        SelectedTextHotkey::ShiftPause
    );

    Ok(())
}

#[test]
fn dbus_update_settings_changes_daemon_visible_is_enabled() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("enabled_property");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;

    assert!(proxy.get_property::<bool>("IsEnabled")?);

    let updated: UpdateSettingsResult = proxy.call(
        "UpdateSettings",
        &(SettingsDto {
            auto_switch_enabled: false,
            fix_two_capitals: false,
            fix_accidental_caps_lock: false,
            layout_delay_ms: 30,
            undo_key: UndoKey::Pause,
            selected_text_hotkey: SelectedTextHotkey::ShiftPause,
            layout_switch: LayoutSwitchSetting {
                combo: LayoutSwitchCombo::ctrl_shift(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            },
        }),
    )?;

    assert!(!updated.restart_required);
    let reloaded_client = Connection::session()?;
    let reloaded_proxy = settings_proxy(&reloaded_client, &service_name)?;
    assert!(!reloaded_proxy.get_property::<bool>("IsEnabled")?);

    Ok(())
}

#[test]
fn dbus_toggle_updates_persisted_auto_switch_setting() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("toggle_persisted");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = OpenSwitcherProxyBlocking::builder(&client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;

    assert!(proxy.is_enabled()?);
    proxy.toggle()?;

    let reloaded_client = Connection::session()?;
    let reloaded_proxy = OpenSwitcherProxyBlocking::builder(&reloaded_client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;

    let updated = reloaded_proxy.get_settings()?;
    assert!(!updated.auto_switch_enabled);
    assert_eq!(reloaded_proxy.is_enabled()?, updated.auto_switch_enabled);

    let config = AppConfig::load_or_create(&config_path)?;
    assert!(!config.features.auto_switch_enabled);

    Ok(())
}

#[test]
fn tray_and_settings_reload_observe_same_auto_switch_value() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("reload_consistency");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = OpenSwitcherProxyBlocking::builder(&client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;

    let before_settings = proxy.get_settings()?;
    let before_tray = proxy.is_enabled()?;
    assert_eq!(before_settings.auto_switch_enabled, before_tray);

    proxy.toggle()?;

    let reloaded_settings_client = Connection::session()?;
    let settings_proxy = OpenSwitcherProxyBlocking::builder(&reloaded_settings_client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;
    let reloaded_tray_client = Connection::session()?;
    let tray_proxy = OpenSwitcherProxyBlocking::builder(&reloaded_tray_client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;

    let after_settings = settings_proxy.get_settings()?;
    let after_tray = tray_proxy.is_enabled()?;
    assert_eq!(after_settings.auto_switch_enabled, after_tray);

    Ok(())
}

#[test]
fn dbus_exposes_layout_switch_capture_session_controls() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("capture");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;

    let initial: LayoutSwitchCaptureState = proxy.call("GetLayoutSwitchCaptureState", &())?;
    assert_eq!(initial.phase, LayoutSwitchCapturePhase::Idle);

    let started: LayoutSwitchCaptureState = proxy.call("StartLayoutSwitchCapture", &())?;
    assert_eq!(started.phase, LayoutSwitchCapturePhase::Waiting);

    let cancelled: LayoutSwitchCaptureState = proxy.call("CancelLayoutSwitchCapture", &())?;
    assert_eq!(cancelled.phase, LayoutSwitchCapturePhase::Cancelled);

    let finished: LayoutSwitchCaptureState = proxy.call("FinishLayoutSwitchCapture", &())?;
    assert_eq!(finished.phase, LayoutSwitchCapturePhase::Finished);

    Ok(())
}

fn spawn_service(config_path: &Path, service_name: &str) -> Result<Connection, Box<dyn Error>> {
    let runtime = Arc::new(RuntimeState::new(ConfigService::load(config_path.to_path_buf())?));
    let connection = ConnectionBuilder::session()?
        .name(service_name)?
        .serve_at(OBJECT_PATH, OpenSwitcherDbusApi::new(runtime))?
        .build()?;

    Ok(connection)
}

fn settings_proxy<'a>(
    connection: &'a Connection,
    service_name: &'a str,
) -> Result<Proxy<'a>, zbus::Error> {
    Proxy::new(connection, service_name, OBJECT_PATH, INTERFACE_NAME)
}

fn unique_service_name(suffix: &str) -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    format!("org.oswitch.core.test.{suffix}.p{pid}.n{nanos}")
}
