use crate::daemon::runtime::RuntimeConfigSnapshot;
use crate::daemon::switch_logic::CorrectionPlan;
use crate::error::SwitcherError;
use crate::model::{LayoutSwitchCombo, UndoKey};
use evdev::{enumerate, Device, InputEvent, Key};
use std::env;
use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const INPUT_EVENT_KEYBOARD: i32 = 0x01;
const MODIFIER_SYNC_DELAY_MS: u64 = 20;
const LAYOUT_SWITCH_DELAY_MS: u64 = 20;
const KEYBOARD_PATH_ENV: &str = "OPEN_SWITCHER_KEYBOARD_PATH";
const INPUT_DEBUG_ENV: &str = "OPEN_SWITCHER_INPUT_DEBUG";
const INPUT_DEBUG_FILE_ENV: &str = "OPEN_SWITCHER_INPUT_DEBUG_FILE";
const POINTER_POLL_INTERVAL: Duration = Duration::from_millis(5);
// Fast-path writer queue is bounded to avoid unbounded memory growth under load.
// Transactional commands use the same total-order queue, but are represented as
// single indivisible commands and are sent with blocking semantics because they
// are rare and correctness matters more than shaving a few microseconds there.
const WRITER_QUEUE_CAPACITY: usize = 1024;
const FAST_PATH_SATURATION_RETRY_WINDOW: Duration = Duration::from_millis(2);

pub struct KeyboardController {
    real_device: GrabbedKeyboardDevice,
    pointer_watcher: PointerWatcher,
    virtual_device: VirtualKeyboardWriter,
}

pub struct SelectionKeyboardTransport {
    virtual_device: VirtualKeyboardHandle,
    modifiers: SharedModifierState,
}

#[derive(Clone)]
struct VirtualKeyboardHandle {
    command_tx: mpsc::SyncSender<WriterCommand>,
    alive: Arc<AtomicBool>,
}

struct VirtualKeyboardWriter {
    handle: VirtualKeyboardHandle,
    join_handle: Option<JoinHandle<()>>,
}

enum WriterCommand {
    Shutdown,
    Fast(WriterFastCommand),
    Transaction(WriterTransaction),
}

#[derive(Clone)]
enum WriterFastCommand {
    ForwardEvent {
        key: Key,
        value: i32,
    },
    TypeSeparator {
        key: Key,
    },
}

enum WriterTransactionKind {
    ApplyCorrection {
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    },
    CopyShortcut {
        modifiers: ModifierState,
    },
    PasteShortcut {
        modifiers: ModifierState,
    },
}

enum WriterTransaction {
    Execute {
        kind: WriterTransactionKind,
        reply: mpsc::Sender<Result<(), SwitcherError>>,
    },
}

struct GrabbedKeyboardDevice {
    device: Device,
    grabbed: bool,
}

struct PointerWatcher {
    click_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
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
        let mut real_device = GrabbedKeyboardDevice::open(keyboard_path)?;
        println!(
            "[INFO] Клавиатура: {}",
            real_device.name().unwrap_or("Unknown")
        );
        thread::sleep(Duration::from_secs(1));
        real_device.grab()?;

        let virtual_device = VirtualKeyboardWriter::new("Open-Switcher Virtual Device")?;
        let pointer_watcher = PointerWatcher::spawn(pointer_paths);

        println!("[OK] Open-Switcher запущен.");
        log_input_debug("grab-acquired", "keyboard grab established at controller startup");

        Ok(Self {
            real_device,
            pointer_watcher,
            virtual_device,
        })
    }

    pub fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        self.real_device.fetch_events()
    }

    pub fn take_pointer_click_invalidation(&self) -> bool {
        self.pointer_watcher.take_click_invalidation()
    }

    pub fn shutdown(&mut self) {
        self.virtual_device.stop();
        self.pointer_watcher.stop();
        if let Err(error) = self.real_device.release_grab() {
            log_input_debug("grab-release-error", &format!("error={error}"));
            eprintln!("[input] Не удалось освободить grab клавиатуры: {error}");
        } else {
            log_input_debug("grab-released", "keyboard grab released during shutdown");
        }
    }

    pub fn with_temporarily_released_grab<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, SwitcherError>,
    ) -> Result<T, SwitcherError> {
        self.real_device.release_grab()?;
        log_input_debug(
            "grab-released",
            "keyboard grab temporarily released for critical operation",
        );

        let operation_result = f(self);
        let regrab_result = self.real_device.grab();
        if regrab_result.is_ok() {
            log_input_debug(
                "grab-acquired",
                "keyboard grab reacquired after critical operation",
            );
        }

        match (operation_result, regrab_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(regrab_error)) => Err(regrab_error),
            (Err(error), Err(_regrab_error)) => Err(error),
        }
    }

    pub fn forward_event(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.virtual_device.handle().forward_event(key, value)
    }

    pub fn type_separator(&mut self, key: Key) -> Result<(), SwitcherError> {
        self.virtual_device.handle().type_separator(key)
    }

    pub fn apply_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        self.virtual_device
            .handle()
            .apply_correction(plan.clone(), config.clone(), modifiers)
    }

    pub fn selection_transport(&self, modifiers: SharedModifierState) -> SelectionKeyboardTransport {
        SelectionKeyboardTransport {
            virtual_device: self.virtual_device.handle(),
            modifiers,
        }
    }

    pub fn is_writer_alive(&self) -> bool {
        self.virtual_device.handle().is_alive()
    }
}

impl GrabbedKeyboardDevice {
    fn open(path: PathBuf) -> Result<Self, SwitcherError> {
        Ok(Self {
            device: Device::open(path)?,
            grabbed: false,
        })
    }

    fn name(&self) -> Option<&str> {
        self.device.name()
    }

    fn grab(&mut self) -> Result<(), SwitcherError> {
        if self.grabbed {
            return Ok(());
        }

        self.device.grab()?;
        self.grabbed = true;
        Ok(())
    }

    fn release_grab(&mut self) -> Result<(), SwitcherError> {
        if !self.grabbed {
            return Ok(());
        }

        self.device.ungrab()?;
        self.grabbed = false;
        Ok(())
    }

    fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        Ok(self.device.fetch_events()?.collect())
    }
}

impl Drop for GrabbedKeyboardDevice {
    fn drop(&mut self) {
        if !self.grabbed {
            return;
        }

        match self.device.ungrab() {
            Ok(()) => log_input_debug("grab-released", "keyboard grab released in Drop"),
            Err(error) => {
                log_input_debug("grab-release-error", &format!("during_drop=true error={error}"));
                eprintln!("[input] Не удалось освободить grab клавиатуры в Drop: {error}");
            }
        }
        self.grabbed = false;
    }
}

impl PointerWatcher {
    fn spawn(paths: Vec<PathBuf>) -> Self {
        let click_flag = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::new(AtomicBool::new(false));

        if paths.is_empty() {
            log_input_debug("pointer-watcher-start", "devices=0 mode=disabled");
            return Self {
                click_flag,
                stop_flag,
                handle: None,
            };
        }

        let worker_click_flag = Arc::clone(&click_flag);
        let worker_stop_flag = Arc::clone(&stop_flag);
        let handle = thread::spawn(move || {
            let mut devices = open_pointer_devices(paths);
            log_input_debug(
                "pointer-watcher-start",
                &format!("devices={}", devices.len()),
            );

            while !worker_stop_flag.load(Ordering::SeqCst) {
                let mut index = 0usize;
                while index < devices.len() {
                    let mut remove_device = false;
                    let device = &mut devices[index];
                    let device_name = device.name().unwrap_or("unknown").to_string();
                    loop {
                        match device.fetch_events() {
                            Ok(events) => {
                                let mut had_events = false;
                                for event in events {
                                    had_events = true;
                                    if let evdev::InputEventKind::Key(key) = event.kind() {
                                        if is_pointer_click(key) && event.value() == 1 {
                                            worker_click_flag.store(true, Ordering::SeqCst);
                                            log_input_debug(
                                                "pointer-click",
                                                &format!("device={device_name} key={key:?}"),
                                            );
                                        }
                                    }
                                }

                                if !had_events {
                                    break;
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) => {
                                log_input_debug(
                                    "pointer-read-error",
                                    &format!("device={device_name} error={error}"),
                                );
                                remove_device = true;
                                break;
                            }
                        }
                    }

                    if remove_device {
                        devices.remove(index);
                    } else {
                        index += 1;
                    }
                }

                thread::sleep(POINTER_POLL_INTERVAL);
            }

            log_input_debug("pointer-watcher-stop", "reason=shutdown");
        });

        Self {
            click_flag,
            stop_flag,
            handle: Some(handle),
        }
    }

    fn take_click_invalidation(&self) -> bool {
        self.click_flag.swap(false, Ordering::SeqCst)
    }

    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl VirtualKeyboardWriter {
    fn new(name: &str) -> Result<Self, SwitcherError> {
        let device = create_virtual_keyboard(name)?;
        let (command_tx, command_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = Arc::clone(&alive);

        let join_handle = thread::spawn(move || {
            log_input_debug("writer-start", "virtual keyboard writer thread started");
            let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_virtual_keyboard_writer_loop(device, command_rx)
            }));

            match loop_result {
                Ok(Ok(())) => {
                    log_input_debug("writer-stop", "virtual keyboard writer thread stopped");
                }
                Ok(Err(error)) => {
                    log_input_debug("writer-error", &format!("error={error}"));
                    eprintln!("[input] Ошибка writer path виртуальной клавиатуры: {error}");
                }
                Err(payload) => {
                    let reason = if let Some(text) = payload.downcast_ref::<&str>() {
                        *text
                    } else if let Some(text) = payload.downcast_ref::<String>() {
                        text.as_str()
                    } else {
                        "unknown panic payload"
                    };
                    log_input_debug("writer-panic", &format!("reason={reason}"));
                    eprintln!("[input] Writer path виртуальной клавиатуры аварийно завершился: {reason}");
                }
            }

            worker_alive.store(false, Ordering::SeqCst);
        });

        Ok(Self {
            handle: VirtualKeyboardHandle { command_tx, alive },
            join_handle: Some(join_handle),
        })
    }

    fn handle(&self) -> VirtualKeyboardHandle {
        self.handle.clone()
    }

    fn stop(&mut self) {
        if self.handle.alive.load(Ordering::SeqCst) {
            let _ = self.handle.command_tx.send(WriterCommand::Shutdown);
        }

        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

impl Drop for VirtualKeyboardWriter {
    fn drop(&mut self) {
        self.stop();
    }
}

impl VirtualKeyboardHandle {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn ensure_alive(&self) -> Result<(), SwitcherError> {
        if self.is_alive() {
            Ok(())
        } else {
            Err(SwitcherError::VirtualKeyboardWriterDisconnected)
        }
    }

    fn forward_event(&self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.ensure_alive()?;
        self.send_fast_command(WriterFastCommand::ForwardEvent { key, value })
    }

    fn type_separator(&self, key: Key) -> Result<(), SwitcherError> {
        self.ensure_alive()?;
        self.send_fast_command(WriterFastCommand::TypeSeparator { key })
    }

    fn apply_correction(
        &self,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        self.run_transaction(WriterTransactionKind::ApplyCorrection {
            plan,
            config,
            modifiers,
        })
    }

    fn send_copy_shortcut(&self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.run_transaction(WriterTransactionKind::CopyShortcut { modifiers })
    }

    fn send_paste_shortcut(&self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.run_transaction(WriterTransactionKind::PasteShortcut { modifiers })
    }

    fn run_transaction(&self, kind: WriterTransactionKind) -> Result<(), SwitcherError> {
        self.ensure_alive()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.command_tx
            .send(WriterCommand::Transaction(WriterTransaction::Execute {
                kind,
                reply: reply_tx,
            }))
            .map_err(|_| SwitcherError::VirtualKeyboardWriterDisconnected)?;
        reply_rx
            .recv()
            .map_err(|_| SwitcherError::VirtualKeyboardWriterDisconnected)?
    }

    fn send_fast_command(&self, command: WriterFastCommand) -> Result<(), SwitcherError> {
        let started = Instant::now();
        let mut yielded = false;

        loop {
            match self.command_tx.try_send(WriterCommand::Fast(command.clone())) {
                Ok(()) => {
                    if yielded {
                        log_input_debug(
                            "writer-backpressure-recovered",
                            &format!("elapsed_us={}", started.elapsed().as_micros()),
                        );
                    }
                    return Ok(());
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(SwitcherError::VirtualKeyboardWriterDisconnected);
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    if started.elapsed() >= FAST_PATH_SATURATION_RETRY_WINDOW {
                        log_input_debug(
                            "writer-backpressure-failed",
                            &format!(
                                "elapsed_us={} retry_window_us={}",
                                started.elapsed().as_micros(),
                                FAST_PATH_SATURATION_RETRY_WINDOW.as_micros()
                            ),
                        );
                        return Err(SwitcherError::VirtualKeyboardWriterSaturated);
                    }

                    if !yielded {
                        log_input_debug(
                            "writer-backpressure",
                            &format!(
                                "retry_window_us={}",
                                FAST_PATH_SATURATION_RETRY_WINDOW.as_micros()
                            ),
                        );
                        yielded = true;
                    }
                    thread::yield_now();
                }
            }
        }
    }
}

impl Drop for PointerWatcher {
    fn drop(&mut self) {
        self.stop();
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

fn create_virtual_keyboard(name: &str) -> Result<uinput::Device, SwitcherError> {
    let virtual_device = uinput::default()?
        .name(name)?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(500));
    Ok(virtual_device)
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
        self.virtual_device.handle().send_copy_shortcut(modifiers)
    }

    pub fn send_paste_shortcut(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.virtual_device.handle().send_paste_shortcut(modifiers)
    }
}

impl SelectionKeyboardTransport {
    pub fn send_copy_shortcut(&mut self) -> Result<(), SwitcherError> {
        // Take a fresh modifier snapshot for each shortcut so copy/paste does not
        // replay stale modifier state across the whole selected-text operation.
        self.virtual_device
            .send_copy_shortcut(self.modifiers.snapshot())
    }

    pub fn send_paste_shortcut(&mut self) -> Result<(), SwitcherError> {
        self.virtual_device
            .send_paste_shortcut(self.modifiers.snapshot())
    }
}

fn release_modifiers(
    device: &mut uinput::Device,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    modifiers.for_each_pressed(|key| device.release(&key))?;
    device.synchronize()?;
    thread::sleep(Duration::from_millis(MODIFIER_SYNC_DELAY_MS));
    Ok(())
}

fn restore_modifiers(
    device: &mut uinput::Device,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    modifiers.for_each_pressed(|key| device.press(&key))?;
    device.synchronize()?;
    Ok(())
}

fn run_shortcut(
    device: &mut uinput::Device,
    modifiers: ModifierState,
    shortcut_modifiers: &[uinput::event::keyboard::Key],
    trigger_key: Option<&uinput::event::keyboard::Key>,
) -> Result<(), SwitcherError> {
    release_modifiers(device, modifiers)?;

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
    restore_modifiers(device, modifiers)?;
    Ok(())
}

fn run_layout_switch(
    device: &mut uinput::Device,
    combo: LayoutSwitchCombo,
) -> Result<(), SwitcherError> {
    let (modifiers, trigger_key) = layout_switch_combo_sequence(combo);

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
}

fn run_correction(
    device: &mut uinput::Device,
    plan: &CorrectionPlan,
    config: &RuntimeConfigSnapshot,
    modifiers: ModifierState,
) -> Result<(), SwitcherError> {
    release_modifiers(device, modifiers)?;
    for _ in 0..(plan.buffer.len() + plan.extra_backspaces) {
        device.click(&uinput::event::keyboard::Key::BackSpace)?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(config.backspace_ms));
    }

    run_layout_switch(device, config.layout_switch_combo)?;
    thread::sleep(Duration::from_millis(config.layout_delay_ms));

    for stroke in &plan.buffer {
        if stroke.shift {
            device.press(&uinput::event::keyboard::Key::LeftShift)?;
        }
        device.write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 1)?;
        device.write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 0)?;
        if stroke.shift {
            device.release(&uinput::event::keyboard::Key::LeftShift)?;
        }
        device.synchronize()?;
        thread::sleep(Duration::from_millis(config.typing_ms));
    }

    if plan.extra_backspaces > 0 {
        device.click(&uinput::event::keyboard::Key::Space)?;
        device.synchronize()?;
    }

    restore_modifiers(device, modifiers)?;
    Ok(())
}

fn run_virtual_keyboard_writer_loop(
    mut device: uinput::Device,
    command_rx: mpsc::Receiver<WriterCommand>,
) -> Result<(), SwitcherError> {
    for command in command_rx {
        match command {
            WriterCommand::Shutdown => break,
            WriterCommand::Fast(command) => match command {
                WriterFastCommand::ForwardEvent { key, value } => {
                    device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)?;
                    device.synchronize()?;
                }
                WriterFastCommand::TypeSeparator { key } => {
                    device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, 1)?;
                    device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, 0)?;
                    device.synchronize()?;
                }
            },
            WriterCommand::Transaction(transaction) => match transaction {
                WriterTransaction::Execute { kind, reply } => {
                    let result = match kind {
                        WriterTransactionKind::ApplyCorrection {
                            plan,
                            config,
                            modifiers,
                        } => run_correction(&mut device, &plan, &config, modifiers),
                        WriterTransactionKind::CopyShortcut { modifiers } => run_shortcut(
                            &mut device,
                            modifiers,
                            &[uinput::event::keyboard::Key::LeftControl],
                            Some(&uinput::event::keyboard::Key::C),
                        ),
                        WriterTransactionKind::PasteShortcut { modifiers } => run_shortcut(
                            &mut device,
                            modifiers,
                            &[uinput::event::keyboard::Key::LeftControl],
                            Some(&uinput::event::keyboard::Key::V),
                        ),
                    };
                    let _ = reply.send(result);
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn log_input_debug(stage: &str, details: &str) {
    if !input_debug_enabled() {
        return;
    }

    let line = format!("[input-debug] stage={stage} {details}");
    eprintln!("{line}");

    let path = env::var(INPUT_DEBUG_FILE_ENV)
        .unwrap_or_else(|_| "/tmp/open-switcher-input-debug.log".to_string());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

fn input_debug_enabled() -> bool {
    matches!(
        env::var(INPUT_DEBUG_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

impl SharedModifierState {
    pub fn store(&self, modifiers: ModifierState) {
        self.bits.store(modifiers.to_bits(), Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> ModifierState {
        ModifierState::from_bits(self.bits.load(Ordering::SeqCst))
    }
}
