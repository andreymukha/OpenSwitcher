use crate::daemon::runtime::RuntimeConfigSnapshot;
use crate::daemon::switch_logic::CorrectionPlan;
use crate::error::SwitcherError;
use crate::model::{LayoutSwitchCombo, UndoKey};
use evdev::{enumerate, Device, InputEvent, Key};
use std::env;
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const INPUT_EVENT_KEYBOARD: i32 = 0x01;
const MODIFIER_SYNC_DELAY_MS: u64 = 20;
const LAYOUT_SWITCH_DELAY_MS: u64 = 20;
const KEYBOARD_PATH_ENV: &str = "OPEN_SWITCHER_KEYBOARD_PATH";

pub struct KeyboardController {
    real_device: Device,
    pointer_devices: Vec<Device>,
    virtual_device: SharedVirtualKeyboard,
}

pub struct SelectionKeyboardTransport {
    virtual_device: SharedVirtualKeyboard,
    modifiers: SharedModifierState,
}

#[derive(Clone)]
struct SharedVirtualKeyboard {
    inner: Arc<Mutex<uinput::Device>>,
}

#[derive(Clone, Default)]
pub struct SharedModifierState {
    bits: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierState {
    left_ctrl: bool,
    right_ctrl: bool,
    left_shift: bool,
    right_shift: bool,
    left_alt: bool,
    right_alt: bool,
    left_meta: bool,
    right_meta: bool,
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
            Key::KEY_RIGHTALT => self.right_alt = pressed,
            Key::KEY_LEFTMETA => self.left_meta = pressed,
            Key::KEY_RIGHTMETA => self.right_meta = pressed,
            _ => {}
        }
    }

    pub fn is_shift_pressed(&self) -> bool {
        self.left_shift || self.right_shift
    }

    pub fn is_ctrl_pressed(&self) -> bool {
        self.left_ctrl || self.right_ctrl
    }

    pub fn is_alt_pressed(&self) -> bool {
        self.left_alt || self.right_alt
    }

    pub fn is_meta_pressed(&self) -> bool {
        self.left_meta || self.right_meta
    }

    pub fn matches_layout_switch_combo(
        &self,
        combo: LayoutSwitchCombo,
        key: Key,
        value: i32,
    ) -> bool {
        if value != 1 {
            return false;
        }

        match combo {
            LayoutSwitchCombo::CtrlShift => {
                matches!(
                    key,
                    Key::KEY_LEFTCTRL
                        | Key::KEY_RIGHTCTRL
                        | Key::KEY_LEFTSHIFT
                        | Key::KEY_RIGHTSHIFT
                ) && self.is_ctrl_pressed()
                    && self.is_shift_pressed()
            }
            LayoutSwitchCombo::AltShift => {
                matches!(
                    key,
                    Key::KEY_LEFTALT
                        | Key::KEY_RIGHTALT
                        | Key::KEY_LEFTSHIFT
                        | Key::KEY_RIGHTSHIFT
                ) && self.is_alt_pressed()
                    && self.is_shift_pressed()
            }
            LayoutSwitchCombo::CapsLock => key == Key::KEY_CAPSLOCK,
            LayoutSwitchCombo::CtrlSpace => {
                key == Key::KEY_SPACE && self.is_ctrl_pressed()
            }
            LayoutSwitchCombo::SuperSpace => {
                key == Key::KEY_SPACE && self.is_meta_pressed()
            }
            LayoutSwitchCombo::LeftCtrlLeftShift => {
                matches!(key, Key::KEY_LEFTCTRL | Key::KEY_LEFTSHIFT)
                    && self.left_ctrl
                    && self.left_shift
            }
            LayoutSwitchCombo::RightCtrlRightShift => {
                matches!(key, Key::KEY_RIGHTCTRL | Key::KEY_RIGHTSHIFT)
                    && self.right_ctrl
                    && self.right_shift
            }
            LayoutSwitchCombo::LeftAltLeftShift => {
                matches!(key, Key::KEY_LEFTALT | Key::KEY_LEFTSHIFT)
                    && self.left_alt
                    && self.left_shift
            }
            LayoutSwitchCombo::RightAltRightShift => {
                matches!(key, Key::KEY_RIGHTALT | Key::KEY_RIGHTSHIFT)
                    && self.right_alt
                    && self.right_shift
            }
        }
    }

    fn to_bits(self) -> u8 {
        (self.left_ctrl as u8)
            | ((self.right_ctrl as u8) << 1)
            | ((self.left_shift as u8) << 2)
            | ((self.right_shift as u8) << 3)
            | ((self.left_alt as u8) << 4)
            | ((self.right_alt as u8) << 5)
            | ((self.left_meta as u8) << 6)
            | ((self.right_meta as u8) << 7)
    }

    fn from_bits(bits: u8) -> Self {
        Self {
            left_ctrl: bits & 0b000001 != 0,
            right_ctrl: bits & 0b000010 != 0,
            left_shift: bits & 0b000100 != 0,
            right_shift: bits & 0b001000 != 0,
            left_alt: bits & 0b010000 != 0,
            right_alt: bits & 0b100000 != 0,
            left_meta: bits & 0b1000000 != 0,
            right_meta: bits & 0b10000000 != 0,
        }
    }
}

impl KeyboardController {
    pub fn open() -> Result<Self, SwitcherError> {
        let keyboard_path = configured_keyboard_path()
            .or_else(find_keyboard)
            .ok_or(SwitcherError::KeyboardNotFound)?;
        let pointer_paths = find_pointer_devices(&keyboard_path);
        let mut real_device = Device::open(keyboard_path)?;
        println!(
            "[INFO] Клавиатура: {}",
            real_device.name().unwrap_or("Unknown")
        );
        thread::sleep(Duration::from_secs(1));
        real_device.grab()?;

        let virtual_device = create_virtual_keyboard("Open-Switcher Virtual Device")?;
        let pointer_devices = open_pointer_devices(pointer_paths);

        println!("[OK] Open-Switcher запущен.");

        Ok(Self {
            real_device,
            pointer_devices,
            virtual_device,
        })
    }

    pub fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        Ok(self.real_device.fetch_events()?.collect())
    }

    pub fn drain_pointer_clicks(&mut self) -> Result<bool, SwitcherError> {
        let mut saw_click = false;

        for device in &mut self.pointer_devices {
            loop {
                match device.fetch_events() {
                    Ok(events) => {
                        let mut had_events = false;
                        for event in events {
                            had_events = true;
                            if let evdev::InputEventKind::Key(key) = event.kind() {
                                if is_pointer_click(key) && event.value() == 1 {
                                    saw_click = true;
                                }
                            }
                        }

                        if !had_events {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }

        Ok(saw_click)
    }

    pub fn with_temporarily_released_grab<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, SwitcherError>,
    ) -> Result<T, SwitcherError> {
        self.real_device.ungrab()?;

        let operation_result = f(self);
        let regrab_result = self.real_device.grab().map_err(SwitcherError::from);

        match (operation_result, regrab_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(regrab_error)) => Err(regrab_error),
            (Err(error), Err(_regrab_error)) => Err(error),
        }
    }

    pub fn forward_event(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.virtual_device.with_device(|device| {
            device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)?;
            device.synchronize()?;
            Ok(())
        })
    }

    pub fn type_separator(&mut self, key: Key) -> Result<(), SwitcherError> {
        self.virtual_device.with_device(|device| {
            device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, 1)?;
            device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, 0)?;
            device.synchronize()?;
            Ok(())
        })
    }

    pub fn apply_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        self.release_modifiers(modifiers)?;
        for _ in 0..(plan.buffer.len() + plan.extra_backspaces) {
            self.virtual_device.with_device(|device| {
                device.click(&uinput::event::keyboard::Key::BackSpace)?;
                device.synchronize()?;
                Ok(())
            })?;
            thread::sleep(Duration::from_millis(config.backspace_ms));
        }

        self.switch_layout(config.layout_switch_combo)?;
        thread::sleep(Duration::from_millis(config.layout_delay_ms));

        for stroke in &plan.buffer {
            if stroke.shift {
                self.virtual_device.with_device(|device| {
                    device.press(&uinput::event::keyboard::Key::LeftShift)?;
                    Ok(())
                })?;
            }
            self.virtual_device.with_device(|device| {
                device.write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 1)?;
                device.write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 0)?;
                Ok(())
            })?;
            if stroke.shift {
                self.virtual_device.with_device(|device| {
                    device.release(&uinput::event::keyboard::Key::LeftShift)?;
                    Ok(())
                })?;
            }
            self.virtual_device.with_device(|device| {
                device.synchronize()?;
                Ok(())
            })?;
            thread::sleep(Duration::from_millis(config.typing_ms));
        }

        if plan.extra_backspaces > 0 {
            self.virtual_device.with_device(|device| {
                device.click(&uinput::event::keyboard::Key::Space)?;
                device.synchronize()?;
                Ok(())
            })?;
        }

        self.restore_modifiers(modifiers)?;
        Ok(())
    }

    fn release_modifiers(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        release_modifiers(&mut self.virtual_device, modifiers)
    }

    fn restore_modifiers(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        restore_modifiers(&mut self.virtual_device, modifiers)
    }

    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
        let (modifiers, trigger_key) = layout_switch_combo_sequence(combo);

        self.virtual_device.with_device(|device| {
            for modifier in modifiers {
                device.press(modifier)?;
            }

            if let Some(key) = trigger_key {
                device.press(key)?;
            }

            device.synchronize()?;
            thread::sleep(Duration::from_millis(LAYOUT_SWITCH_DELAY_MS));

            if let Some(key) = trigger_key {
                device.release(key)?;
            }

            for modifier in modifiers.iter().rev() {
                device.release(modifier)?;
            }

            device.synchronize()?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn selection_transport(&self, modifiers: SharedModifierState) -> SelectionKeyboardTransport {
        SelectionKeyboardTransport {
            virtual_device: self.virtual_device.clone(),
            modifiers,
        }
    }
}

fn configured_keyboard_path() -> Option<PathBuf> {
    let raw = env::var_os(KEYBOARD_PATH_ENV)?;
    if raw.is_empty() {
        return None;
    }

    Some(PathBuf::from(raw))
}

fn open_pointer_devices(paths: Vec<PathBuf>) -> Vec<Device> {
    let mut devices = Vec::new();

    for path in paths {
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        if set_nonblocking(&device).is_ok() {
            devices.push(device);
        }
    }

    devices
}

fn create_virtual_keyboard(name: &str) -> Result<SharedVirtualKeyboard, SwitcherError> {
    let virtual_device = uinput::default()?
        .name(name)?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(500));
    Ok(SharedVirtualKeyboard {
        inner: Arc::new(Mutex::new(virtual_device)),
    })
}

fn set_nonblocking(device: &Device) -> io::Result<()> {
    let fd = device.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
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
            | Key::KEY_LEFTMETA
            | Key::KEY_RIGHTMETA
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

fn layout_switch_combo_sequence(
    combo: LayoutSwitchCombo,
) -> (
    &'static [uinput::event::keyboard::Key],
    Option<&'static uinput::event::keyboard::Key>,
) {
    use uinput::event::keyboard::Key;

    static CTRL_SHIFT: [Key; 2] = [Key::LeftControl, Key::LeftShift];
    static ALT_SHIFT: [Key; 2] = [Key::LeftAlt, Key::LeftShift];
    static CTRL_SPACE: [Key; 1] = [Key::LeftControl];
    static SUPER_SPACE: [Key; 1] = [Key::LeftMeta];
    static LEFT_CTRL_LEFT_SHIFT: [Key; 2] = [Key::LeftControl, Key::LeftShift];
    static RIGHT_CTRL_RIGHT_SHIFT: [Key; 2] = [Key::RightControl, Key::RightShift];
    static LEFT_ALT_LEFT_SHIFT: [Key; 2] = [Key::LeftAlt, Key::LeftShift];
    static RIGHT_ALT_RIGHT_SHIFT: [Key; 2] = [Key::RightAlt, Key::RightShift];
    static SPACE: Key = Key::Space;
    static CAPS_LOCK: Key = Key::CapsLock;

    match combo {
        LayoutSwitchCombo::CtrlShift => (&CTRL_SHIFT, None),
        LayoutSwitchCombo::AltShift => (&ALT_SHIFT, None),
        LayoutSwitchCombo::CapsLock => (&[], Some(&CAPS_LOCK)),
        LayoutSwitchCombo::CtrlSpace => (&CTRL_SPACE, Some(&SPACE)),
        LayoutSwitchCombo::SuperSpace => (&SUPER_SPACE, Some(&SPACE)),
        LayoutSwitchCombo::LeftCtrlLeftShift => (&LEFT_CTRL_LEFT_SHIFT, None),
        LayoutSwitchCombo::RightCtrlRightShift => (&RIGHT_CTRL_RIGHT_SHIFT, None),
        LayoutSwitchCombo::LeftAltLeftShift => (&LEFT_ALT_LEFT_SHIFT, None),
        LayoutSwitchCombo::RightAltRightShift => (&RIGHT_ALT_RIGHT_SHIFT, None),
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

fn find_pointer_devices(excluded_keyboard_path: &PathBuf) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for (path, device) in enumerate() {
        if &path == excluded_keyboard_path {
            continue;
        }

        let name = device.name().unwrap_or("");
        if name.contains("Camera") {
            continue;
        }

        let Some(keys) = device.supported_keys() else {
            continue;
        };

        if keys.contains(Key::BTN_LEFT)
            || keys.contains(Key::BTN_RIGHT)
            || keys.contains(Key::BTN_MIDDLE)
            || keys.contains(Key::BTN_SIDE)
            || keys.contains(Key::BTN_EXTRA)
            || keys.contains(Key::BTN_TOUCH)
            || keys.contains(Key::BTN_TOOL_FINGER)
            || keys.contains(Key::BTN_TOOL_DOUBLETAP)
        {
            paths.push(path);
        }
    }

    paths
}

fn is_pointer_click(key: Key) -> bool {
    let code = key.code();
    (Key::BTN_LEFT.code()..=Key::BTN_TOOL_DOUBLETAP.code()).contains(&code)
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
        if self.right_alt {
            f(uinput::event::keyboard::Key::RightAlt)?;
        }
        if self.left_meta {
            f(uinput::event::keyboard::Key::LeftMeta)?;
        }
        if self.right_meta {
            f(uinput::event::keyboard::Key::RightMeta)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(keys: &[(Key, i32)]) -> ModifierState {
        let mut state = ModifierState::default();
        for (key, value) in keys {
            state.update(*key, *value);
        }
        state
    }

    #[test]
    fn matches_alt_shift_combo_with_right_alt() {
        let state = pressed(&[(Key::KEY_RIGHTALT, 1), (Key::KEY_LEFTSHIFT, 1)]);
        assert!(state.matches_layout_switch_combo(
            LayoutSwitchCombo::AltShift,
            Key::KEY_LEFTSHIFT,
            1
        ));
    }

    #[test]
    fn matches_right_ctrl_right_shift_combo() {
        let state = pressed(&[(Key::KEY_RIGHTCTRL, 1), (Key::KEY_RIGHTSHIFT, 1)]);
        assert!(state.matches_layout_switch_combo(
            LayoutSwitchCombo::RightCtrlRightShift,
            Key::KEY_RIGHTSHIFT,
            1
        ));
    }

    #[test]
    fn matches_super_space_combo() {
        let state = pressed(&[(Key::KEY_LEFTMETA, 1)]);
        assert!(state.matches_layout_switch_combo(
            LayoutSwitchCombo::SuperSpace,
            Key::KEY_SPACE,
            1
        ));
    }
}

impl KeyboardController {
    pub fn send_copy_shortcut(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.send_shortcut(
            modifiers,
            &[uinput::event::keyboard::Key::LeftControl],
            Some(&uinput::event::keyboard::Key::C),
        )
    }

    pub fn send_paste_shortcut(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.send_shortcut(
            modifiers,
            &[uinput::event::keyboard::Key::LeftControl],
            Some(&uinput::event::keyboard::Key::V),
        )
    }

    fn send_shortcut(
        &mut self,
        modifiers: ModifierState,
        shortcut_modifiers: &[uinput::event::keyboard::Key],
        trigger_key: Option<&uinput::event::keyboard::Key>,
    ) -> Result<(), SwitcherError> {
        run_shortcut_on_shared_device(
            &self.virtual_device,
            modifiers,
            shortcut_modifiers,
            trigger_key,
        )
    }
}

impl SelectionKeyboardTransport {
    pub fn send_copy_shortcut(&mut self) -> Result<(), SwitcherError> {
        self.send_shortcut(
            self.modifiers.snapshot(),
            &[uinput::event::keyboard::Key::LeftControl],
            Some(&uinput::event::keyboard::Key::C),
        )
    }

    pub fn send_paste_shortcut(&mut self) -> Result<(), SwitcherError> {
        self.send_shortcut(
            self.modifiers.snapshot(),
            &[uinput::event::keyboard::Key::LeftControl],
            Some(&uinput::event::keyboard::Key::V),
        )
    }

    fn send_shortcut(
        &mut self,
        modifiers: ModifierState,
        shortcut_modifiers: &[uinput::event::keyboard::Key],
        trigger_key: Option<&uinput::event::keyboard::Key>,
    ) -> Result<(), SwitcherError> {
        run_shortcut_on_shared_device(
            &self.virtual_device,
            modifiers,
            shortcut_modifiers,
            trigger_key,
        )
    }
}

fn release_modifiers(
    virtual_device: &SharedVirtualKeyboard,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    virtual_device.with_device(|device| {
        modifiers.for_each_pressed(|key| device.release(&key))?;
        device.synchronize()?;
        Ok(())
    })?;
    thread::sleep(Duration::from_millis(MODIFIER_SYNC_DELAY_MS));
    Ok(())
}

fn restore_modifiers(
    virtual_device: &SharedVirtualKeyboard,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    virtual_device.with_device(|device| {
        modifiers.for_each_pressed(|key| device.press(&key))?;
        device.synchronize()?;
        Ok(())
    })?;
    Ok(())
}

fn run_shortcut_on_shared_device(
    virtual_device: &SharedVirtualKeyboard,
    modifiers: ModifierState,
    shortcut_modifiers: &[uinput::event::keyboard::Key],
    trigger_key: Option<&uinput::event::keyboard::Key>,
) -> Result<(), SwitcherError> {
    virtual_device.with_device(|device| {
        modifiers.for_each_pressed(|key| device.release(&key))?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(MODIFIER_SYNC_DELAY_MS));

        for modifier in shortcut_modifiers {
            device.press(modifier)?;
        }

        if let Some(key) = trigger_key {
            device.press(key)?;
        }

        device.synchronize()?;
        thread::sleep(Duration::from_millis(LAYOUT_SWITCH_DELAY_MS));

        if let Some(key) = trigger_key {
            device.release(key)?;
        }

        for modifier in shortcut_modifiers.iter().rev() {
            device.release(modifier)?;
        }

        device.synchronize()?;
        modifiers.for_each_pressed(|key| device.press(&key))?;
        device.synchronize()?;
        Ok(())
    })
}

impl SharedVirtualKeyboard {
    fn with_device<T>(
        &self,
        f: impl FnOnce(&mut uinput::Device) -> Result<T, SwitcherError>,
    ) -> Result<T, SwitcherError> {
        let mut device = self
            .inner
            .lock()
            .map_err(|_| SwitcherError::VirtualKeyboardLockPoisoned)?;
        f(&mut device)
    }
}

impl SharedModifierState {
    pub fn store(&self, modifiers: ModifierState) {
        self.bits.store(modifiers.to_bits(), Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> ModifierState {
        ModifierState::from_bits(self.bits.load(Ordering::SeqCst))
    }
}
