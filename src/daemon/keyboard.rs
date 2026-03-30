use crate::daemon::runtime::RuntimeConfigSnapshot;
use crate::daemon::switch_logic::CorrectionPlan;
use crate::error::SwitcherError;
use crate::model::{LayoutModifier, LayoutSwitchCombo, LayoutTriggerKey, UndoKey};
use evdev::{enumerate, Device, InputEvent, Key};
use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

const INPUT_EVENT_KEYBOARD: i32 = 0x01;
const MODIFIER_SYNC_DELAY_MS: u64 = 20;
const LAYOUT_SWITCH_DELAY_MS: u64 = 20;

pub struct KeyboardController {
    real_device: Device,
    virtual_device: uinput::Device,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierState {
    left_ctrl: bool,
    right_ctrl: bool,
    left_shift: bool,
    right_shift: bool,
    left_alt: bool,
}

impl ModifierState {
    pub fn update(&mut self, key: Key, value: i32) {
        let pressed = value == 1 || value == 2;
        match key {
            Key::KEY_LEFTCTRL => self.left_ctrl = pressed,
            Key::KEY_RIGHTCTRL => self.right_ctrl = pressed,
            Key::KEY_LEFTSHIFT => self.left_shift = pressed,
            Key::KEY_RIGHTSHIFT => self.right_shift = pressed,
            Key::KEY_LEFTALT => self.left_alt = pressed,
            _ => {}
        }
    }

    pub fn is_shift_pressed(&self) -> bool {
        self.left_shift || self.right_shift
    }

    pub fn is_ctrl_pressed(&self) -> bool {
        self.left_ctrl || self.right_ctrl
    }

    pub fn should_toggle_layout_shortcut(&self, key: Key, value: i32) -> bool {
        (key == Key::KEY_LEFTCTRL || key == Key::KEY_LEFTSHIFT)
            && value == 1
            && self.is_ctrl_pressed()
            && self.is_shift_pressed()
    }
}

impl KeyboardController {
    pub fn open() -> Result<Self, SwitcherError> {
        let keyboard_path = find_keyboard().ok_or(SwitcherError::KeyboardNotFound)?;
        let mut real_device = Device::open(keyboard_path)?;
        println!(
            "[INFO] Клавиатура: {}",
            real_device.name().unwrap_or("Unknown")
        );
        thread::sleep(Duration::from_secs(1));
        real_device.grab()?;

        let virtual_device = uinput::default()?
            .name("Open-Switcher Virtual Device")?
            .event(uinput::event::Keyboard::All)?
            .create()?;

        thread::sleep(Duration::from_millis(500));
        println!("[OK] Open-Switcher запущен.");

        Ok(Self {
            real_device,
            virtual_device,
        })
    }

    pub fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        Ok(self.real_device.fetch_events()?.collect())
    }

    pub fn forward_event(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.virtual_device
            .write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)?;
        self.virtual_device.synchronize()?;
        Ok(())
    }

    pub fn apply_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        self.release_modifiers(modifiers)?;
        for _ in 0..(plan.buffer.len() + plan.extra_backspaces) {
            self.virtual_device
                .click(&uinput::event::keyboard::Key::BackSpace)?;
            self.virtual_device.synchronize()?;
            thread::sleep(Duration::from_millis(config.backspace_ms));
        }

        self.switch_layout(config.layout_switch_combo)?;
        thread::sleep(Duration::from_millis(config.layout_delay_ms));

        for stroke in &plan.buffer {
            if stroke.shift {
                self.virtual_device
                    .press(&uinput::event::keyboard::Key::LeftShift)?;
            }
            self.virtual_device
                .write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 1)?;
            self.virtual_device
                .write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 0)?;
            if stroke.shift {
                self.virtual_device
                    .release(&uinput::event::keyboard::Key::LeftShift)?;
            }
            self.virtual_device.synchronize()?;
            thread::sleep(Duration::from_millis(config.typing_ms));
        }

        if plan.extra_backspaces > 0 {
            self.virtual_device
                .click(&uinput::event::keyboard::Key::Space)?;
            self.virtual_device.synchronize()?;
        }

        self.restore_modifiers(modifiers)?;
        Ok(())
    }

    fn release_modifiers(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        modifiers.for_each_pressed(|key| self.virtual_device.release(&key))?;
        self.virtual_device.synchronize()?;
        thread::sleep(Duration::from_millis(MODIFIER_SYNC_DELAY_MS));
        Ok(())
    }

    fn restore_modifiers(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        modifiers.for_each_pressed(|key| self.virtual_device.press(&key))?;
        self.virtual_device.synchronize()?;
        Ok(())
    }

    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
        for modifier in combo.modifiers() {
            self.virtual_device
                .press(&layout_modifier_to_uinput_key(modifier))?;
        }

        if let Some(key) = combo.key {
            self.virtual_device
                .press(&layout_trigger_key_to_uinput_key(key))?;
        }

        self.virtual_device.synchronize()?;
        thread::sleep(Duration::from_millis(LAYOUT_SWITCH_DELAY_MS));

        if let Some(key) = combo.key {
            self.virtual_device
                .release(&layout_trigger_key_to_uinput_key(key))?;
        }

        for modifier in combo.modifiers().collect::<Vec<_>>().into_iter().rev() {
            self.virtual_device
                .release(&layout_modifier_to_uinput_key(modifier))?;
        }

        self.virtual_device.synchronize()?;
        Ok(())
    }
}

pub fn is_russian_layout() -> Result<bool, SwitcherError> {
    let display = env::var("DISPLAY").unwrap_or_else(|_| ":0.0".to_string());
    let xauthority = env::var_os("XAUTHORITY").unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".Xauthority")
            .into_os_string()
    });

    let output = Command::new("xset")
        .env("DISPLAY", display)
        .env("XAUTHORITY", xauthority)
        .arg("-q")
        .output()
        .map_err(SwitcherError::Xset)?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("LED mask:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(mask_str) = parts.last() {
                if let Ok(mask) = u32::from_str_radix(mask_str, 16) {
                    return Ok((mask & 0x1000) != 0);
                }
            }
        }
    }

    Ok(false)
}

pub fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::KEY_LEFTCTRL
            | Key::KEY_RIGHTCTRL
            | Key::KEY_LEFTSHIFT
            | Key::KEY_RIGHTSHIFT
            | Key::KEY_LEFTALT
            | Key::KEY_RIGHTALT
            | Key::KEY_CAPSLOCK
    )
}

pub fn is_character(key: Key) -> bool {
    let code = key.code();
    (code >= Key::KEY_1.code() && code <= Key::KEY_EQUAL.code())
        || (code >= Key::KEY_Q.code() && code <= Key::KEY_RIGHTBRACE.code())
        || (code >= Key::KEY_A.code() && code <= Key::KEY_GRAVE.code())
        || (code >= Key::KEY_BACKSLASH.code() && code <= Key::KEY_SLASH.code())
}

pub fn undo_key_to_evdev_key(key: UndoKey) -> Key {
    match key {
        UndoKey::Pause => Key::KEY_PAUSE,
        UndoKey::F12 => Key::KEY_F12,
        UndoKey::ScrollLock => Key::KEY_SCROLLLOCK,
    }
}

fn layout_modifier_to_uinput_key(modifier: LayoutModifier) -> uinput::event::keyboard::Key {
    match modifier {
        LayoutModifier::Ctrl => uinput::event::keyboard::Key::LeftControl,
        LayoutModifier::Alt => uinput::event::keyboard::Key::LeftAlt,
        LayoutModifier::Shift => uinput::event::keyboard::Key::LeftShift,
        LayoutModifier::Super => uinput::event::keyboard::Key::LeftMeta,
    }
}

fn layout_trigger_key_to_uinput_key(key: LayoutTriggerKey) -> uinput::event::keyboard::Key {
    match key {
        LayoutTriggerKey::Space => uinput::event::keyboard::Key::Space,
        LayoutTriggerKey::CapsLock => uinput::event::keyboard::Key::CapsLock,
    }
}

fn find_keyboard() -> Option<PathBuf> {
    for (path, device) in enumerate() {
        let name = device.name().unwrap_or("");
        if name.contains("Virtual") || name.contains("Button") || name.contains("Camera") {
            continue;
        }

        if let Some(keys) = device.supported_keys() {
            if keys.contains(Key::KEY_ENTER)
                && keys.contains(Key::KEY_SPACE)
                && keys.contains(Key::KEY_A)
            {
                return Some(path);
            }
        }
    }

    None
}

impl ModifierState {
    fn for_each_pressed(
        self,
        mut f: impl FnMut(uinput::event::keyboard::Key) -> Result<(), uinput::Error>,
    ) -> Result<(), SwitcherError> {
        if self.left_shift {
            f(uinput::event::keyboard::Key::LeftShift)?;
        }
        if self.right_shift {
            f(uinput::event::keyboard::Key::RightShift)?;
        }
        if self.left_ctrl {
            f(uinput::event::keyboard::Key::LeftControl)?;
        }
        if self.right_ctrl {
            f(uinput::event::keyboard::Key::RightControl)?;
        }
        if self.left_alt {
            f(uinput::event::keyboard::Key::LeftAlt)?;
        }

        Ok(())
    }
}
