use open_switcher::config::AppConfig;
use open_switcher::daemon::runtime::{ConfigService, RuntimeState};
use open_switcher::dbus::{
    OpenSwitcherDbusApi, OpenSwitcherProxyBlocking, INTERFACE_NAME, OBJECT_PATH, SERVICE_NAME,
};
use open_switcher::model::{
    AutoDetectedLayoutSwitch, HotkeyModifiers, HotkeySpec, HotkeyTrigger, LayoutSwitchCapturePhase,
    LayoutSwitchCaptureState, LayoutSwitchCombo, LayoutSwitchSetting, LayoutSwitchSource,
    SelectedTextHotkey, SettingsDto, SettingsFieldMask, SettingsPatchDto, UndoKey,
    UpdateSettingsResult,
};
use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;
use zbus::blocking::{Connection, ConnectionBuilder, Proxy};

// D-Bus contract constants

#[test]
fn dbus_public_constants_match_contract() {
    assert_eq!(SERVICE_NAME, "org.oswitch.core");
    assert_eq!(OBJECT_PATH, "/org/oswitch/core");
    assert_eq!(INTERFACE_NAME, "org.oswitch.core");
}

// Settings roundtrip

fn settings_patch(changed: SettingsFieldMask, values: SettingsDto) -> SettingsPatchDto {
    SettingsPatchDto { changed, values }
}

#[test]
fn stale_clients_merge_different_settings_fields() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("stale_different_fields");
    let _service = spawn_service(&config_path, &service_name)?;
    let client_a = Connection::session()?;
    let client_b = Connection::session()?;
    let proxy_a = settings_proxy(&client_a, &service_name)?;
    let proxy_b = settings_proxy(&client_b, &service_name)?;
    let initial_a: SettingsDto = proxy_a.call("GetSettings", &())?;
    let initial_b: SettingsDto = proxy_b.call("GetSettings", &())?;
    assert_eq!(initial_a, initial_b);

    let result_a: UpdateSettingsResult = proxy_a.call(
        "UpdateSettings",
        &settings_patch(
            SettingsFieldMask::AUTO_SWITCH_ENABLED,
            SettingsDto {
                auto_switch_enabled: false,
                ..initial_a
            },
        ),
    )?;
    assert!(!result_a.settings.auto_switch_enabled);

    let result_b: UpdateSettingsResult = proxy_b.call(
        "UpdateSettings",
        &settings_patch(
            SettingsFieldMask::FIX_TWO_CAPITALS,
            SettingsDto {
                fix_two_capitals: true,
                ..initial_b
            },
        ),
    )?;

    assert!(!result_b.settings.auto_switch_enabled);
    assert!(result_b.settings.fix_two_capitals);
    let current: SettingsDto = proxy_b.call("GetSettings", &())?;
    assert_eq!(current, result_b.settings);
    let persisted = AppConfig::load_or_create(&config_path)?.settings();
    assert!(!persisted.auto_switch_enabled);
    assert!(persisted.fix_two_capitals);
    Ok(())
}

#[test]
fn last_patch_wins_for_the_same_field() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("last_same_field");
    let _service = spawn_service(&config_path, &service_name)?;
    let client_a = Connection::session()?;
    let client_b = Connection::session()?;
    let proxy_a = settings_proxy(&client_a, &service_name)?;
    let proxy_b = settings_proxy(&client_b, &service_name)?;
    let initial: SettingsDto = proxy_a.call("GetSettings", &())?;

    let _: UpdateSettingsResult = proxy_a.call(
        "UpdateSettings",
        &settings_patch(
            SettingsFieldMask::LAYOUT_DELAY_MS,
            SettingsDto {
                layout_delay_ms: 50,
                ..initial
            },
        ),
    )?;
    let last: UpdateSettingsResult = proxy_b.call(
        "UpdateSettings",
        &settings_patch(
            SettingsFieldMask::LAYOUT_DELAY_MS,
            SettingsDto {
                layout_delay_ms: 70,
                ..initial
            },
        ),
    )?;

    assert_eq!(last.settings.layout_delay_ms, 70);
    let current: SettingsDto = proxy_b.call("GetSettings", &())?;
    assert_eq!(current.layout_delay_ms, 70);
    assert_eq!(AppConfig::load_or_create(&config_path)?.layout.delay_ms, 70);
    Ok(())
}

#[test]
fn invalid_merged_patch_leaves_dbus_runtime_and_config_unchanged() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("invalid_merged_patch");
    let _service = spawn_service(&config_path, &service_name)?;
    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;
    let initial: SettingsDto = proxy.call("GetSettings", &())?;
    let enabled_before = proxy.get_property::<bool>("IsEnabled")?;
    let bytes_before = std::fs::read(&config_path)?;

    let error = proxy
        .call_method(
            "UpdateSettings",
            &settings_patch(
                SettingsFieldMask::SELECTED_TEXT_HOTKEY,
                SettingsDto {
                    selected_text_hotkey: initial.manual_correction_hotkey,
                    ..initial
                },
            ),
        )
        .expect_err("merged duplicate hotkeys must be rejected");

    assert!(error.to_string().contains("совпадают"));
    let current: SettingsDto = proxy.call("GetSettings", &())?;
    assert_eq!(current, initial);
    assert_eq!(proxy.get_property::<bool>("IsEnabled")?, enabled_before);
    assert_eq!(std::fs::read(&config_path)?, bytes_before);
    Ok(())
}

#[test]
fn stale_full_settings_signature_fails_without_writing() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("stale_full_signature");
    let _service = spawn_service(&config_path, &service_name)?;
    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;
    let initial: SettingsDto = proxy.call("GetSettings", &())?;
    let bytes_before = std::fs::read(&config_path)?;

    let error = proxy
        .call_method(
            "UpdateSettings",
            &(SettingsDto {
                fix_two_capitals: true,
                ..initial
            }),
        )
        .expect_err("the removed full-settings signature must fail closed");

    let error_text = error.to_string();
    let normalized_error = error_text.to_ascii_lowercase();
    assert!(
        normalized_error.contains("signature") || normalized_error.contains("invalidargs"),
        "unexpected stale-client error: {error_text}"
    );
    let current: SettingsDto = proxy.call("GetSettings", &())?;
    assert_eq!(current, initial);
    assert_eq!(std::fs::read(&config_path)?, bytes_before);
    Ok(())
}

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
    assert_eq!(
        initial.manual_correction_hotkey,
        HotkeySpec::from(UndoKey::Pause)
    );
    assert_eq!(
        initial.selected_text_hotkey,
        HotkeySpec::from(SelectedTextHotkey::ShiftPause)
    );
    assert_eq!(
        initial.layout_switch,
        initial_config.settings().layout_switch
    );

    let result: UpdateSettingsResult = proxy.call(
        "UpdateSettings",
        &settings_patch(
            SettingsFieldMask::all(),
            SettingsDto {
                auto_switch_enabled: false,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                layout_delay_ms: 77,
                manual_correction_hotkey: HotkeySpec::from(UndoKey::F12),
                selected_text_hotkey: HotkeySpec::from(SelectedTextHotkey::AltF12),
                layout_switch: LayoutSwitchSetting {
                    combo: LayoutSwitchCombo::alt_shift(),
                    source: LayoutSwitchSource::Manual,
                    auto_detected: AutoDetectedLayoutSwitch::default(),
                },
            },
        ),
    )?;
    assert!(!result.restart_required);

    let updated: SettingsDto = proxy.call("GetSettings", &())?;
    assert!(!updated.auto_switch_enabled);
    assert_eq!(updated.layout_delay_ms, 77);
    assert_eq!(
        updated.manual_correction_hotkey,
        HotkeySpec::from(UndoKey::F12)
    );
    assert_eq!(
        updated.selected_text_hotkey,
        HotkeySpec::from(SelectedTextHotkey::AltF12)
    );
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
    assert_eq!(
        config.features.manual_correction_hotkey,
        HotkeySpec::from(UndoKey::F12)
    );
    assert_eq!(
        config.features.selected_text_switch_hotkey,
        HotkeySpec::from(SelectedTextHotkey::AltF12)
    );
    assert_eq!(config.layout.switch_combo, LayoutSwitchCombo::alt_shift());

    Ok(())
}

#[test]
fn dbus_roundtrip_preserves_complex_hotkey_specs() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("complex_hotkeys");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = settings_proxy(&client, &service_name)?;
    let manual_hotkey = HotkeySpec::new(HotkeyModifiers::ctrl_alt(), HotkeyTrigger::F12);
    let selected_text_hotkey =
        HotkeySpec::new(HotkeyModifiers::shift_ctrl_alt(), HotkeyTrigger::Insert);

    let result: UpdateSettingsResult = proxy.call(
        "UpdateSettings",
        &settings_patch(
            SettingsFieldMask::all(),
            SettingsDto {
                auto_switch_enabled: true,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                layout_delay_ms: 30,
                manual_correction_hotkey: manual_hotkey,
                selected_text_hotkey,
                layout_switch: LayoutSwitchSetting {
                    combo: LayoutSwitchCombo::alt_shift(),
                    source: LayoutSwitchSource::Manual,
                    auto_detected: AutoDetectedLayoutSwitch::default(),
                },
            },
        ),
    )?;

    assert!(!result.restart_required);
    let updated: SettingsDto = proxy.call("GetSettings", &())?;
    assert_eq!(updated.manual_correction_hotkey, manual_hotkey);
    assert_eq!(updated.selected_text_hotkey, selected_text_hotkey);

    let config = AppConfig::load_or_create(&config_path)?;
    assert_eq!(config.features.manual_correction_hotkey, manual_hotkey);
    assert_eq!(
        config.features.selected_text_switch_hotkey,
        selected_text_hotkey
    );

    Ok(())
}

// Validation errors

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
            &settings_patch(
                SettingsFieldMask::all(),
                SettingsDto {
                    auto_switch_enabled: true,
                    fix_two_capitals: false,
                    fix_accidental_caps_lock: false,
                    layout_delay_ms: 999,
                    manual_correction_hotkey: HotkeySpec::from(UndoKey::Pause),
                    selected_text_hotkey: HotkeySpec::from(SelectedTextHotkey::ShiftPause),
                    layout_switch: LayoutSwitchSetting {
                        combo: LayoutSwitchCombo::ctrl_shift(),
                        source: LayoutSwitchSource::Manual,
                        auto_detected: AutoDetectedLayoutSwitch::default(),
                    },
                },
            ),
        )
        .expect_err("invalid settings must be rejected");

    let error_text = error.to_string();
    assert!(error_text.contains("Задержка переключения"));

    let config = AppConfig::load_or_create(&config_path)?;
    assert_eq!(config.layout.delay_ms, 30);
    assert_eq!(
        config.features.manual_correction_hotkey,
        HotkeySpec::from(UndoKey::Pause)
    );
    assert_eq!(
        config.features.selected_text_switch_hotkey,
        HotkeySpec::from(SelectedTextHotkey::ShiftPause)
    );

    Ok(())
}

// Enabled/toggle behavior

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
        &settings_patch(
            SettingsFieldMask::all(),
            SettingsDto {
                auto_switch_enabled: false,
                fix_two_capitals: false,
                fix_accidental_caps_lock: false,
                layout_delay_ms: 30,
                manual_correction_hotkey: HotkeySpec::from(UndoKey::Pause),
                selected_text_hotkey: HotkeySpec::from(SelectedTextHotkey::ShiftPause),
                layout_switch: LayoutSwitchSetting {
                    combo: LayoutSwitchCombo::ctrl_shift(),
                    source: LayoutSwitchSource::Manual,
                    auto_detected: AutoDetectedLayoutSwitch::default(),
                },
            },
        ),
    )?;

    assert!(!updated.restart_required);
    let reloaded_client = Connection::session()?;
    let reloaded_proxy = settings_proxy(&reloaded_client, &service_name)?;
    assert!(!reloaded_proxy.get_property::<bool>("IsEnabled")?);

    Ok(())
}

#[test]
fn dbus_current_layout_property_is_available() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("current_layout_property");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let generic_proxy = settings_proxy(&client, &service_name)?;
    let generated_proxy = OpenSwitcherProxyBlocking::builder(&client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;

    let generic_value = generic_proxy.get_property::<bool>("CurrentLayout")?;
    let generated_value = generated_proxy.current_layout()?;

    assert_eq!(generic_value, generated_value);

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

// Tray/settings reload consistency

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

// Capture controls

#[test]
fn dbus_binds_layout_switch_capture_commands_to_the_starting_connection(
) -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("capture_owner");
    let _service = spawn_service(&config_path, &service_name)?;

    let owner_client = Connection::session()?;
    let owner_proxy = settings_proxy(&owner_client, &service_name)?;
    let other_client = Connection::session()?;
    let other_proxy = settings_proxy(&other_client, &service_name)?;

    assert_ne!(owner_client.unique_name(), other_client.unique_name());

    let initial: LayoutSwitchCaptureState = other_proxy.call("GetLayoutSwitchCaptureState", &())?;
    assert_eq!(initial.phase, LayoutSwitchCapturePhase::Idle);

    let started: LayoutSwitchCaptureState = owner_proxy.call("StartLayoutSwitchCapture", &())?;
    assert_eq!(started.phase, LayoutSwitchCapturePhase::Waiting);

    let busy = other_proxy
        .call_method("StartLayoutSwitchCapture", &())
        .expect_err("a different D-Bus connection must not replace the capture session");
    assert!(
        busy.to_string().contains("already owned"),
        "unexpected StartLayoutSwitchCapture error: {busy}"
    );

    for member in [
        "RenewLayoutSwitchCapture",
        "CancelLayoutSwitchCapture",
        "FinishLayoutSwitchCapture",
    ] {
        let error = other_proxy
            .call_method(member, &())
            .expect_err("a different D-Bus connection must not control the capture session");
        assert!(
            error.to_string().contains("does not own"),
            "unexpected {member} error: {error}"
        );
    }

    let renewed: LayoutSwitchCaptureState = owner_proxy.call("RenewLayoutSwitchCapture", &())?;
    assert_eq!(renewed.phase, LayoutSwitchCapturePhase::Waiting);

    let public_state: LayoutSwitchCaptureState =
        other_proxy.call("GetLayoutSwitchCaptureState", &())?;
    assert_eq!(public_state.phase, LayoutSwitchCapturePhase::Waiting);

    let cancelled: LayoutSwitchCaptureState = owner_proxy.call("CancelLayoutSwitchCapture", &())?;
    assert_eq!(cancelled.phase, LayoutSwitchCapturePhase::Cancelled);

    Ok(())
}

#[test]
fn dbus_owner_can_finish_an_active_layout_switch_capture() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("capture_finish");
    let _service = spawn_service(&config_path, &service_name)?;

    let owner_client = Connection::session()?;
    let owner_proxy = settings_proxy(&owner_client, &service_name)?;

    let started: LayoutSwitchCaptureState = owner_proxy.call("StartLayoutSwitchCapture", &())?;
    assert_eq!(started.phase, LayoutSwitchCapturePhase::Waiting);

    let finished: LayoutSwitchCaptureState = owner_proxy.call("FinishLayoutSwitchCapture", &())?;
    assert_eq!(finished.phase, LayoutSwitchCapturePhase::Finished);

    Ok(())
}

#[test]
fn dbus_exposes_settings_hotkey_capture_inhibition() -> Result<(), Box<dyn Error>> {
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("config.toml");
    let service_name = unique_service_name("hotkey_capture_inhibit");
    let _service = spawn_service(&config_path, &service_name)?;

    let client = Connection::session()?;
    let proxy = OpenSwitcherProxyBlocking::builder(&client)
        .destination(service_name.clone())?
        .path(OBJECT_PATH)?
        .build()?;

    proxy.set_hotkey_capture_inhibited(true)?;
    proxy.set_hotkey_capture_inhibited(false)?;

    Ok(())
}

// Test harness

fn spawn_service(config_path: &Path, service_name: &str) -> Result<Connection, Box<dyn Error>> {
    let runtime = Arc::new(RuntimeState::new(ConfigService::load(
        config_path.to_path_buf(),
    )?));
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
