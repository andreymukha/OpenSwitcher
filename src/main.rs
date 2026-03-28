use evdev::{enumerate, Device, InputEventKind, Key};
use open_switcher::SwitcherApi;
use serde::Deserialize;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use zbus::blocking::ConnectionBuilder;
use zbus::SignalContext;

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

#[derive(Clone, Debug)]
struct Keystroke {
    key: Key,
    shift: bool,
}

const EXCLUDED_WORDS: &[&str] = &[
    "sudo", "git", "cargo", "rustc", "python", "node", "grep", "echo", "ls", "cd", "rm", "mkdir", "apt"
];

fn find_keyboard() -> Option<PathBuf> {
    for (path, device) in enumerate() {
        let name = device.name().unwrap_or("");
        if name.contains("Virtual") || name.contains("Button") || name.contains("Camera") {
            continue;
        }
        if let Some(keys) = device.supported_keys() {
            if keys.contains(Key::KEY_ENTER) && keys.contains(Key::KEY_SPACE) && keys.contains(Key::KEY_A) {
                return Some(path);
            }
        }
    }
    None
}

fn is_russian_layout() -> bool {
    let output = Command::new("xset")
        .env("DISPLAY", ":0.0")
        .env("XAUTHORITY", "/home/fly/.Xauthority")
        .arg("-q")
        .output();
        
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("LED mask:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(mask_str) = parts.last() {
                    if let Ok(mask) = u32::from_str_radix(mask_str, 16) {
                        return (mask & 0x1000) != 0;
                    }
                }
            }
        }
    }
    false
}

fn keys_to_string(keys: &[Keystroke]) -> String {
    keys.iter().filter_map(|k| {
        let name = format!("{:?}", k.key);
        name.strip_prefix("KEY_").map(|s| s.to_lowercase())
    }).collect()
}

fn count_rus_vowels(keys: &[Keystroke]) -> usize {
    keys.iter().filter(|k| {
        matches!(k.key, Key::KEY_F | Key::KEY_T | Key::KEY_GRAVE | Key::KEY_B | Key::KEY_J | Key::KEY_E | Key::KEY_S | Key::KEY_APOSTROPHE | Key::KEY_DOT | Key::KEY_Z)
    }).count()
}

fn is_likely_english(word: &str) -> bool {
    let clean_word = word.trim_end_matches(&['.', ',', ';', '\'', '`', '[', ']'][..]);
    if clean_word.len() < 3 { return true; }
    if EXCLUDED_WORDS.contains(&clean_word) { return true; }
    
    let eng_vowels = clean_word.chars().filter(|c| "aeiouy".contains(*c)).count();
    if eng_vowels >= 2 && clean_word.len() <= 5 { return true; }
    
    false
}

fn should_switch(buffer: &[Keystroke]) -> bool {
    if buffer.len() < 3 { return false; }
    
    let word = keys_to_string(buffer);
    println!("[DEBUG] Анализ слова: {}", word);
    
    if is_likely_english(&word) { 
        println!("[DEBUG] Слово '{}' похоже на английское, пропускаем.", word);
        return false; 
    }

    let mut score = 0;
    
    for (i, k) in buffer.iter().enumerate() {
        if matches!(k.key, Key::KEY_LEFTBRACE | Key::KEY_RIGHTBRACE | Key::KEY_SEMICOLON | Key::KEY_APOSTROPHE | Key::KEY_GRAVE) {
            score += 15;
        }
        if matches!(k.key, Key::KEY_COMMA | Key::KEY_DOT) {
            // Если это внутри слова (например dstk.xb) - это точно русская буква
            if i < buffer.len() - 1 {
                score += 15;
            } else {
                score += 5;
            }
        }
    }
    
    let rus_vowels = count_rus_vowels(buffer);
    let eng_vowels = word.chars().filter(|c| "aeiouy".contains(*c)).count();

    if rus_vowels > eng_vowels { score += 10; }
    if eng_vowels == 0 { score += 12; }

    score >= 10
}

fn main() -> Result<(), Box<dyn Error>> {
    let config_str = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config_str)?;

    let enabled = Arc::new(AtomicBool::new(true));
    let layout = Arc::new(AtomicBool::new(true));

    let api = SwitcherApi { enabled: enabled.clone(), layout: layout.clone() };

    let conn = ConnectionBuilder::session()?
        .name("org.oswitch.core")?
        .serve_at("/org/oswitch/core", api)?
        .build()?;

    let kb_path = find_keyboard().ok_or("Keyboard not found")?;
    let mut real_dev = Device::open(kb_path)?;
    
    println!("[INFO] Клавиатура: {}", real_dev.name().unwrap_or("Unknown"));
    thread::sleep(Duration::from_secs(1));
    real_dev.grab()?;

    let mut virtual_dev = uinput::default()?
        .name("Open-Switcher Virtual Device")?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(500));
    println!("[OK] Open-Switcher v1.6.0 (Shift & Symbols Support) запущен.");

    let mut buffer: Vec<Keystroke> = Vec::new();
    let mut last_word_buffer: Vec<Keystroke> = Vec::new();
    let switch_key_code = parse_evdev_key(&config.features.undo_key)?;

    let mut left_ctrl_pressed = false;
    let mut right_ctrl_pressed = false;
    let mut left_shift_pressed = false;
    let mut right_shift_pressed = false;
    let mut left_alt_pressed = false;

    loop {
        for event in real_dev.fetch_events()? {
            if let InputEventKind::Key(key) = event.kind() {
                let value = event.value();
                
                if key == Key::KEY_LEFTCTRL { left_ctrl_pressed = value == 1 || value == 2; }
                if key == Key::KEY_RIGHTCTRL { right_ctrl_pressed = value == 1 || value == 2; }
                if key == Key::KEY_LEFTSHIFT { left_shift_pressed = value == 1 || value == 2; }
                if key == Key::KEY_RIGHTSHIFT { right_shift_pressed = value == 1 || value == 2; }
                if key == Key::KEY_LEFTALT { left_alt_pressed = value == 1 || value == 2; }
                
                let is_shift = left_shift_pressed || right_shift_pressed;
                let is_ctrl = left_ctrl_pressed || right_ctrl_pressed;
                
                if (key == Key::KEY_LEFTCTRL || key == Key::KEY_LEFTSHIFT) && value == 1 && is_ctrl && is_shift {
                    let new_layout = !layout.load(Ordering::SeqCst);
                    layout.store(new_layout, Ordering::SeqCst);
                    if let Ok(ctxt) = SignalContext::new(conn.inner(), "/org/oswitch/core") {
                        let _ = zbus::block_on(SwitcherApi::status_changed(&ctxt, enabled.load(Ordering::SeqCst), new_layout));
                    }
                }

                if value == 1 {
                    if key == switch_key_code {
                        let target_buffer = if !buffer.is_empty() {
                            Some((buffer.clone(), 0))
                        } else if !last_word_buffer.is_empty() {
                            Some((last_word_buffer.clone(), 1)) 
                        } else {
                            None
                        };

                        if let Some((buf, extra_bs)) = target_buffer {
                            println!("[ACTION] Ручное исправление / Отмена...");
                            
                            if left_shift_pressed { virtual_dev.release(&uinput::event::keyboard::Key::LeftShift)?; }
                            if right_shift_pressed { virtual_dev.release(&uinput::event::keyboard::Key::RightShift)?; }
                            if left_ctrl_pressed { virtual_dev.release(&uinput::event::keyboard::Key::LeftControl)?; }
                            if right_ctrl_pressed { virtual_dev.release(&uinput::event::keyboard::Key::RightControl)?; }
                            if left_alt_pressed { virtual_dev.release(&uinput::event::keyboard::Key::LeftAlt)?; }
                            virtual_dev.synchronize()?;
                            thread::sleep(Duration::from_millis(20));

                            for _ in 0..(buf.len() + extra_bs) {
                                virtual_dev.click(&uinput::event::keyboard::Key::BackSpace)?;
                                virtual_dev.synchronize()?;
                                thread::sleep(Duration::from_millis(config.delays.backspace_ms));
                            }
                            switch_layout(&mut virtual_dev, &config.layout.keys)?;
                            
                            let is_rus = is_russian_layout();
                            layout.store(!is_rus, Ordering::SeqCst);
                            
                            if let Ok(ctxt) = SignalContext::new(conn.inner(), "/org/oswitch/core") {
                                let _ = zbus::block_on(SwitcherApi::status_changed(&ctxt, enabled.load(Ordering::SeqCst), !is_rus));
                            }
                            
                            thread::sleep(Duration::from_millis(config.layout.delay_ms));
                            for k in &buf {
                                if k.shift { virtual_dev.press(&uinput::event::keyboard::Key::LeftShift)?; }
                                virtual_dev.write(0x01, k.key.code() as i32, 1)?;
                                virtual_dev.write(0x01, k.key.code() as i32, 0)?;
                                if k.shift { virtual_dev.release(&uinput::event::keyboard::Key::LeftShift)?; }
                                virtual_dev.synchronize()?;
                                thread::sleep(Duration::from_millis(config.delays.typing_ms));
                            }
                            if extra_bs > 0 {
                                virtual_dev.click(&uinput::event::keyboard::Key::Space)?;
                                virtual_dev.synchronize()?;
                            }

                            if left_shift_pressed { virtual_dev.press(&uinput::event::keyboard::Key::LeftShift)?; }
                            if right_shift_pressed { virtual_dev.press(&uinput::event::keyboard::Key::RightShift)?; }
                            if left_ctrl_pressed { virtual_dev.press(&uinput::event::keyboard::Key::LeftControl)?; }
                            if right_ctrl_pressed { virtual_dev.press(&uinput::event::keyboard::Key::RightControl)?; }
                            if left_alt_pressed { virtual_dev.press(&uinput::event::keyboard::Key::LeftAlt)?; }
                            virtual_dev.synchronize()?;
                            
                            if !buffer.is_empty() {
                                last_word_buffer = buffer.clone();
                                buffer.clear();
                            }
                        }
                        continue;
                    }

                    match key {
                        Key::KEY_SPACE | Key::KEY_ENTER | Key::KEY_TAB => {
                            let is_rus = is_russian_layout();
                            layout.store(!is_rus, Ordering::SeqCst);

                            if enabled.load(Ordering::SeqCst) && !is_rus && should_switch(&buffer) {
                                println!("[ACTION] Автоматическое исправление...");
                                let _ = apply_correction(&mut virtual_dev, &buffer, &config, &layout, &enabled, &conn, left_shift_pressed, right_shift_pressed, left_ctrl_pressed, right_ctrl_pressed, left_alt_pressed);
                            }
                            last_word_buffer = buffer.clone();
                            buffer.clear();
                            let _ = forward_event(&mut virtual_dev, key, 1);
                        }
                        Key::KEY_BACKSPACE => {
                            buffer.pop();
                            let _ = forward_event(&mut virtual_dev, key, 1);
                        }
                        _ => {
                            if is_character(key) { 
                                buffer.push(Keystroke { key, shift: is_shift }); 
                            } else if !is_modifier(key) {
                                buffer.clear();
                            }
                            let _ = forward_event(&mut virtual_dev, key, 1);
                        }
                    }
                } else {
                    let _ = forward_event(&mut virtual_dev, key, value);
                }
            }
        }
    }
}

fn apply_correction(vdev: &mut uinput::Device, buffer: &[Keystroke], config: &Config, layout: &Arc<AtomicBool>, enabled: &Arc<AtomicBool>, conn: &zbus::blocking::Connection, left_shift: bool, right_shift: bool, left_ctrl: bool, right_ctrl: bool, left_alt: bool) -> Result<(), Box<dyn Error>> {
    // 1. Отпускаем зажатые пользователем модификаторы, чтобы они не мешали хоткеям и набору текста
    if left_shift { vdev.release(&uinput::event::keyboard::Key::LeftShift)?; }
    if right_shift { vdev.release(&uinput::event::keyboard::Key::RightShift)?; }
    if left_ctrl { vdev.release(&uinput::event::keyboard::Key::LeftControl)?; }
    if right_ctrl { vdev.release(&uinput::event::keyboard::Key::RightControl)?; }
    if left_alt { vdev.release(&uinput::event::keyboard::Key::LeftAlt)?; }
    vdev.synchronize()?;
    thread::sleep(Duration::from_millis(20));

    for _ in 0..buffer.len() {
        vdev.click(&uinput::event::keyboard::Key::BackSpace)?;
        vdev.synchronize()?;
        thread::sleep(Duration::from_millis(config.delays.backspace_ms));
    }
    
    switch_layout(vdev, &config.layout.keys)?;
    
    let new_layout = !layout.load(Ordering::SeqCst);
    layout.store(new_layout, Ordering::SeqCst);

    if let Ok(ctxt) = SignalContext::new(conn.inner(), "/org/oswitch/core") {
        let _ = zbus::block_on(SwitcherApi::status_changed(&ctxt, enabled.load(Ordering::SeqCst), new_layout));
    }

    thread::sleep(Duration::from_millis(config.layout.delay_ms));

    for k in buffer {
        if k.shift { vdev.press(&uinput::event::keyboard::Key::LeftShift)?; }
        vdev.write(0x01, k.key.code() as i32, 1)?;
        vdev.write(0x01, k.key.code() as i32, 0)?;
        if k.shift { vdev.release(&uinput::event::keyboard::Key::LeftShift)?; }
        vdev.synchronize()?;
        thread::sleep(Duration::from_millis(config.delays.typing_ms));
    }

    // Возвращаем модификаторы, если они всё еще зажаты физически
    if left_shift { vdev.press(&uinput::event::keyboard::Key::LeftShift)?; }
    if right_shift { vdev.press(&uinput::event::keyboard::Key::RightShift)?; }
    if left_ctrl { vdev.press(&uinput::event::keyboard::Key::LeftControl)?; }
    if right_ctrl { vdev.press(&uinput::event::keyboard::Key::RightControl)?; }
    if left_alt { vdev.press(&uinput::event::keyboard::Key::LeftAlt)?; }
    vdev.synchronize()?;

    Ok(())
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
    
    vdev.synchronize()?;
    thread::sleep(Duration::from_millis(20));

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

fn is_modifier(k: Key) -> bool {
    matches!(k, Key::KEY_LEFTCTRL | Key::KEY_RIGHTCTRL | Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT | Key::KEY_LEFTALT | Key::KEY_RIGHTALT | Key::KEY_CAPSLOCK)
}

fn is_character(k: Key) -> bool {
    let code = k.code();
    (code >= Key::KEY_1.code() && code <= Key::KEY_EQUAL.code()) ||
    (code >= Key::KEY_Q.code() && code <= Key::KEY_RIGHTBRACE.code()) ||
    (code >= Key::KEY_A.code() && code <= Key::KEY_GRAVE.code()) ||
    (code >= Key::KEY_BACKSLASH.code() && code <= Key::KEY_SLASH.code())
}
