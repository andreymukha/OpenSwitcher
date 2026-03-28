use evdev::{Device, InputEventKind, Key, enumerate};
use std::error::Error;
use std::fs;
use serde::Deserialize;
use std::thread;
use std::time::Duration;
use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use zbus::blocking::ConnectionBuilder;
use open_switcher::SwitcherApi;

#[derive(Deserialize)]
struct Config {
    layout: LayoutConfig,
    delays: DelaysConfig,
    features: FeaturesConfig,
}

#[derive(Deserialize)]
struct LayoutConfig {
    keys: Vec<String>,
    delay_ms: u64,
}

#[derive(Deserialize)]
struct DelaysConfig {
    backspace_ms: u64,
    typing_ms: u64,
}

#[derive(Deserialize)]
struct FeaturesConfig {
    undo_key: String,
}

fn find_keyboard() -> Option<PathBuf> {
    for (path, device) in enumerate() {
        if let Some(keys) = device.supported_keys() {
            if keys.contains(Key::KEY_ENTER) && keys.contains(Key::KEY_SPACE) && keys.contains(Key::KEY_A) {
                return Some(path);
            }
        }
    }
    None
}

fn main() -> Result<(), Box<dyn Error>> {
    let config_str = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config_str)?;

    let enabled = Arc::new(AtomicBool::new(true));
    let api = SwitcherApi { enabled: enabled.clone() };
    
    // Исправлено: всё в нижнем регистре
    let _conn = ConnectionBuilder::session()?
        .name("org.openswitcher.daemon")?
        .serve_at("/org/openswitcher/daemon", api)?
        .build()?;
    
    println!("[OK] D-Bus интерфейс готов.");

    let kb_path = find_keyboard().ok_or("Keyboard not found")?;
    let mut real_dev = Device::open(kb_path)?;
    real_dev.grab()?;

    let mut virtual_dev = uinput::default()?
        .name("Open-Switcher Virtual Device")?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(500));
    println!("[OK] Open-Switcher v1.2.2 запущен.");

    let mut buffer: Vec<Key> = Vec::new();
    let switch_key_code = parse_evdev_key(&config.features.undo_key)?;

    loop {
        for event in real_dev.fetch_events()? {
            if let InputEventKind::Key(key) = event.kind() {
                let value = event.value();
                if value == 1 {
                    if key == switch_key_code {
                        if !buffer.is_empty() {
                            println!("[ACTION] Ручное исправление...");
                            for _ in 0..buffer.len() {
                                virtual_dev.click(&uinput::event::keyboard::Key::BackSpace)?;
                                thread::sleep(Duration::from_millis(config.delays.backspace_ms));
                            }
                            switch_layout(&mut virtual_dev, &config.layout.keys)?;
                            thread::sleep(Duration::from_millis(config.layout.delay_ms));
                            for k in &buffer {
                                virtual_dev.write(0x01, k.code() as i32, 1)?;
                                virtual_dev.write(0x01, k.code() as i32, 0)?;
                                virtual_dev.synchronize()?;
                                thread::sleep(Duration::from_millis(config.delays.typing_ms));
                            }
                        }
                        continue;
                    }

                    match key {
                        Key::KEY_SPACE | Key::KEY_ENTER | Key::KEY_DOT | Key::KEY_COMMA | Key::KEY_SEMICOLON => {
                            buffer.clear();
                            forward_event(&mut virtual_dev, key, 1)?;
                        }
                        Key::KEY_BACKSPACE => {
                            buffer.pop();
                            forward_event(&mut virtual_dev, key, 1)?;
                        }
                        _ => {
                            if is_character(key) { buffer.push(key); }
                            forward_event(&mut virtual_dev, key, 1)?;
                        }
                    }
                } else {
                    forward_event(&mut virtual_dev, key, value)?;
                }
            }
        }
    }
}

fn parse_evdev_key(name: &str) -> Result<Key, Box<dyn Error>> {
    match name {
        "Pause" => Ok(Key::KEY_PAUSE),
        "F12" => Ok(Key::KEY_F12),
        "ScrollLock" => Ok(Key::KEY_SCROLLLOCK),
        _ => Err(format!("Unknown key: {}", name).into()),
    }
}

fn switch_layout(vdev: &mut uinput::Device, keys: &[String]) -> Result<(), Box<dyn Error>> {
    for key_name in keys {
        let u_key = parse_uinput_key(key_name)?;
        vdev.press(&u_key)?;
    }
    for key_name in keys.iter().rev() {
        let u_key = parse_uinput_key(key_name)?;
        vdev.release(&u_key)?;
    }
    vdev.synchronize()?;
    Ok(())
}

fn parse_uinput_key(name: &str) -> Result<uinput::event::keyboard::Key, Box<dyn Error>> {
    match name {
        "LeftControl" => Ok(uinput::event::keyboard::Key::LeftControl),
        "LeftShift" => Ok(uinput::event::keyboard::Key::LeftShift),
        "LeftAlt" => Ok(uinput::event::keyboard::Key::LeftAlt),
        "CapsLock" => Ok(uinput::event::keyboard::Key::CapsLock),
        _ => Err(format!("Unknown uinput key: {}", name).into()),
    }
}

fn forward_event(vdev: &mut uinput::Device, key: Key, value: i32) -> Result<(), Box<dyn Error>> {
    vdev.write(0x01, key.code() as i32, value)?;
    vdev.synchronize()?;
    Ok(())
}

fn is_character(k: Key) -> bool {
    let code = k.code();
    (code >= Key::KEY_Q.code() && code <= Key::KEY_P.code()) ||
    (code >= Key::KEY_A.code() && code <= Key::KEY_L.code()) ||
    (code >= Key::KEY_Z.code() && code <= Key::KEY_M.code()) ||
    matches!(k, Key::KEY_LEFTBRACE | Key::KEY_RIGHTBRACE | Key::KEY_SEMICOLON | Key::KEY_APOSTROPHE | Key::KEY_COMMA | Key::KEY_DOT | Key::KEY_GRAVE)
}
