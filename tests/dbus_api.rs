use open_switcher::config::AppConfig;
use open_switcher::daemon::runtime::{ConfigService, RuntimeState};
use open_switcher::dbus::{OpenSwitcherDbusApi, INTERFACE_NAME, OBJECT_PATH};
use open_switcher::model::{
    AutoDetectedLayoutSwitch, LayoutSwitchCombo, LayoutSwitchSetting, LayoutSwitchSource,
    SettingsDto, UndoKey, UpdateSettingsResult,
};
use std::error::Error;
use std::path::PathBuf;
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

    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;

    let initial: SettingsDto = proxy.call("GetSettings", &())?;
    assert_eq!(initial.layout_delay_ms, 30);
    assert_eq!(initial.undo_key, UndoKey::Pause);
    assert_eq!(initial.layout_switch.combo, LayoutSwitchCombo::ctrl_shift());

    let result: UpdateSettingsResult = proxy.call(
        "UpdateSettings",
        &(SettingsDto {
            layout_delay_ms: 77,
            undo_key: UndoKey::F12,
            layout_switch: LayoutSwitchSetting {
                combo: LayoutSwitchCombo::alt_shift(),
                source: LayoutSwitchSource::Manual,
                auto_detected: AutoDetectedLayoutSwitch::default(),
            },
        }),
    )?;
    assert!(!result.restart_required);

    let updated: SettingsDto = proxy.call("GetSettings", &())?;
    assert_eq!(updated.layout_delay_ms, 77);
    assert_eq!(updated.undo_key, UndoKey::F12);
    assert_eq!(updated.layout_switch.combo, LayoutSwitchCombo::alt_shift());
    assert_eq!(updated.layout_switch.source, LayoutSwitchSource::Manual);

    let config = AppConfig::load_or_create(&config_path)?;
    assert_eq!(config.layout.delay_ms, 77);
    assert_eq!(config.features.undo_key, UndoKey::F12);
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
                layout_delay_ms: 999,
                undo_key: UndoKey::Pause,
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

    Ok(())
}

fn spawn_service(config_path: &PathBuf, service_name: &str) -> Result<Connection, Box<dyn Error>> {
    let runtime = Arc::new(RuntimeState::new(ConfigService::load(config_path.clone())?));
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
