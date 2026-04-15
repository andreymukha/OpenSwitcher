use crate::daemon::runtime::RuntimeConfigSnapshot;
use crate::daemon::switch_logic::CorrectionPlan;
use crate::error::SwitcherError;
use crate::model::{LayoutSwitchCombo, SessionType, UndoKey};
use crate::system::SystemContextDetector;
use evdev::{enumerate, Device, InputEvent, Key, LedType};
use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const INPUT_EVENT_KEYBOARD: i32 = 0x01;
const MODIFIER_SYNC_DELAY_MS: u64 = 20;
const LAYOUT_SWITCH_DELAY_MS: u64 = 20;
const KEYBOARD_PATH_ENV: &str = "OPEN_SWITCHER_KEYBOARD_PATH";
const KEYBOARD_SYMLINK_SUFFIX: &str = "-event-kbd";
const KEYBOARD_SYMLINK_DIRS: [&str; 2] = ["/dev/input/by-path", "/dev/input/by-id"];
const UINPUT_PATHS: [&str; 2] = ["/dev/uinput", "/dev/input/uinput"];
const INPUT_DEBUG_ENV: &str = "OPEN_SWITCHER_INPUT_DEBUG";
const INPUT_DEBUG_FILE_ENV: &str = "OPEN_SWITCHER_INPUT_DEBUG_FILE";
pub const INPUT_EVENT_WAIT_TIMEOUT: Duration = Duration::from_millis(100);
const POINTER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const INPUT_TARGET_POLL_INTERVAL: Duration = Duration::from_millis(5);
// Fast-path writer queue is bounded to avoid unbounded memory growth under load.
// Transactional commands use the same total-order queue, but are represented as
// single indivisible commands and are sent with blocking semantics because they
// are rare and correctness matters more than shaving a few microseconds there.
const WRITER_QUEUE_CAPACITY: usize = 1024;
const FAST_PATH_SATURATION_RETRY_WINDOW: Duration = Duration::from_millis(2);

pub struct KeyboardController {
    real_device: GrabbedKeyboardDevice,
    pointer_watcher: PointerWatcher,
    input_target_watcher: InputTargetWatcher,
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
    ForwardEvent { key: Key, value: i32 },
    TypeSeparator { key: Key },
}

enum WriterTransactionKind {
    ApplyCorrection {
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    },
    ApplySameLayoutCorrection {
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
    path: PathBuf,
    device: Device,
    grabbed: bool,
}

struct PointerWatcher {
    click_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

struct InputTargetWatcher {
    changed_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

struct PointerDeviceState {
    device: Device,
    pressed_buttons: HashSet<Key>,
}

struct ActiveWindowMonitor {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
    active_window_atom: u32,
    current_window: Option<u32>,
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
    caps_lock_active: bool,
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
            Key::KEY_CAPSLOCK if value == 1 => self.caps_lock_active = !self.caps_lock_active,
            _ => {}
        }
    }

    pub fn set_caps_lock_active(&mut self, active: bool) {
        self.caps_lock_active = active;
    }

    pub fn is_caps_lock_active(&self) -> bool {
        self.caps_lock_active
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
                    Key::KEY_LEFTALT | Key::KEY_RIGHTALT | Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT
                ) && self.is_alt_pressed()
                    && self.is_shift_pressed()
            }
            LayoutSwitchCombo::CapsLock => key == Key::KEY_CAPSLOCK,
            LayoutSwitchCombo::CtrlSpace => key == Key::KEY_SPACE && self.is_ctrl_pressed(),
            LayoutSwitchCombo::SuperSpace => key == Key::KEY_SPACE && self.is_meta_pressed(),
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

    pub fn keeps_layout_switch_combo_active(&self, combo: LayoutSwitchCombo) -> bool {
        match combo {
            LayoutSwitchCombo::CtrlShift => self.is_ctrl_pressed() && self.is_shift_pressed(),
            LayoutSwitchCombo::AltShift => self.is_alt_pressed() && self.is_shift_pressed(),
            LayoutSwitchCombo::CapsLock => false,
            LayoutSwitchCombo::CtrlSpace => self.is_ctrl_pressed(),
            LayoutSwitchCombo::SuperSpace => self.is_meta_pressed(),
            LayoutSwitchCombo::LeftCtrlLeftShift => self.left_ctrl && self.left_shift,
            LayoutSwitchCombo::RightCtrlRightShift => self.right_ctrl && self.right_shift,
            LayoutSwitchCombo::LeftAltLeftShift => self.left_alt && self.left_shift,
            LayoutSwitchCombo::RightAltRightShift => self.right_alt && self.right_shift,
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
            caps_lock_active: false,
        }
    }
}

impl KeyboardController {
    pub fn open() -> Result<Self, SwitcherError> {
        let keyboard_path = resolve_keyboard_path()?;
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
        let input_target_watcher = InputTargetWatcher::spawn();

        println!("[OK] Open-Switcher запущен.");
        log_input_debug(
            "grab-acquired",
            "keyboard grab established at controller startup",
        );

        Ok(Self {
            real_device,
            pointer_watcher,
            input_target_watcher,
            virtual_device,
        })
    }

    pub fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        self.real_device.fetch_events()
    }

    pub fn fetch_events_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<InputEvent>, SwitcherError> {
        self.real_device.fetch_events_timeout(timeout)
    }

    pub fn take_pointer_click_invalidation(&self) -> bool {
        self.pointer_watcher.take_click_invalidation()
    }

    pub fn take_input_target_invalidation(&self) -> bool {
        self.input_target_watcher.take_change_invalidation()
    }

    pub fn shutdown(&mut self) {
        self.virtual_device.stop();
        self.pointer_watcher.stop();
        self.input_target_watcher.stop();
        if let Err(error) = self.real_device.release_grab() {
            log_input_debug("grab-release-error", &format!("error={error}"));
            eprintln!("[input] Не удалось освободить grab клавиатуры: {error}");
        } else {
            log_input_debug("grab-released", "keyboard grab released during shutdown");
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

    pub fn apply_same_layout_correction(
        &mut self,
        plan: &CorrectionPlan,
        config: &RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        self.virtual_device.handle().apply_same_layout_correction(
            plan.clone(),
            config.clone(),
            modifiers,
        )
    }

    pub fn selection_transport(
        &self,
        modifiers: SharedModifierState,
    ) -> SelectionKeyboardTransport {
        SelectionKeyboardTransport {
            virtual_device: self.virtual_device.handle(),
            modifiers,
        }
    }

    pub fn is_writer_alive(&self) -> bool {
        self.virtual_device.handle().is_alive()
    }

    pub fn caps_lock_active(&self) -> bool {
        self.real_device.caps_lock_active().unwrap_or(false)
    }
}

impl GrabbedKeyboardDevice {
    fn open(path: PathBuf) -> Result<Self, SwitcherError> {
        let device = Device::open(&path).map_err(|error| map_keyboard_open_error(&path, error))?;
        Ok(Self {
            path,
            device,
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

        self.device
            .grab()
            .map_err(|error| map_keyboard_open_error(&self.path, error))?;
        self.grabbed = true;
        Ok(())
    }

    fn release_grab(&mut self) -> Result<(), SwitcherError> {
        if !self.grabbed {
            return Ok(());
        }

        self.device
            .ungrab()
            .map_err(|error| map_keyboard_open_error(&self.path, error))?;
        self.grabbed = false;
        Ok(())
    }

    fn fetch_events(&mut self) -> Result<Vec<InputEvent>, SwitcherError> {
        self.device
            .fetch_events()
            .map(|events| events.collect())
            .map_err(|error| map_keyboard_open_error(&self.path, error))
    }

    fn fetch_events_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<InputEvent>, SwitcherError> {
        if !wait_for_device_input(&self.device, timeout)? {
            return Ok(Vec::new());
        }

        self.fetch_events()
    }

    fn caps_lock_active(&self) -> Result<bool, SwitcherError> {
        self.device
            .get_led_state()
            .map(|state| state.contains(LedType::LED_CAPSL))
            .map_err(|error| map_keyboard_open_error(&self.path, error))
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
                log_input_debug(
                    "grab-release-error",
                    &format!("during_drop=true error={error}"),
                );
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
                    let device_name = device.device.name().unwrap_or("unknown").to_string();
                    loop {
                        match device.device.fetch_events() {
                            Ok(events) => {
                                let mut had_events = false;
                                for event in events {
                                    had_events = true;
                                    if let evdev::InputEventKind::Key(key) = event.kind() {
                                        if is_pointer_click(key)
                                            && event.value() == 1
                                            && device.pressed_buttons.insert(key)
                                        {
                                            worker_click_flag.store(true, Ordering::SeqCst);
                                            log_input_debug(
                                                "pointer-click",
                                                &format!("device={device_name} key={key:?}"),
                                            );
                                        } else if is_pointer_click(key) && event.value() == 0 {
                                            device.pressed_buttons.remove(&key);
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

impl InputTargetWatcher {
    fn spawn() -> Self {
        let changed_flag = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let session_type = detect_current_session_type();
        if !should_enable_x11_input_target_watcher(session_type) {
            log_input_debug(
                "input-target-watcher-disabled",
                &format!(
                    "reason=non-x11-session session_type={}",
                    format_session_type(session_type)
                ),
            );
            return Self {
                changed_flag,
                stop_flag,
                handle: None,
            };
        }

        let Ok(mut monitor) = ActiveWindowMonitor::connect() else {
            log_input_debug(
                "input-target-watcher-disabled",
                "reason=x11-active-window-unavailable",
            );
            return Self {
                changed_flag,
                stop_flag,
                handle: None,
            };
        };

        let worker_changed_flag = Arc::clone(&changed_flag);
        let worker_stop_flag = Arc::clone(&stop_flag);
        let handle = thread::spawn(move || {
            log_input_debug(
                "input-target-watcher-start",
                &format!(
                    "source=_NET_ACTIVE_WINDOW initial_window={}",
                    format_x11_window(monitor.current_window)
                ),
            );

            while !worker_stop_flag.load(Ordering::SeqCst) {
                let mut had_events = false;

                loop {
                    match monitor.poll_change() {
                        Ok(Some((previous_window, current_window))) => {
                            had_events = true;
                            worker_changed_flag.store(true, Ordering::SeqCst);
                            log_input_debug(
                                "input-target-changed",
                                &format!(
                                    "source=_NET_ACTIVE_WINDOW previous={} current={}",
                                    format_x11_window(previous_window),
                                    format_x11_window(current_window)
                                ),
                            );
                        }
                        Ok(None) => break,
                        Err(error) => {
                            log_input_debug(
                                "input-target-read-error",
                                &format!("source=_NET_ACTIVE_WINDOW error={error}"),
                            );
                            log_input_debug("input-target-watcher-stop", "reason=watcher-error");
                            return;
                        }
                    }
                }

                if !had_events {
                    thread::sleep(INPUT_TARGET_POLL_INTERVAL);
                }
            }

            log_input_debug("input-target-watcher-stop", "reason=shutdown");
        });

        Self {
            changed_flag,
            stop_flag,
            handle: Some(handle),
        }
    }

    fn take_change_invalidation(&self) -> bool {
        self.changed_flag.swap(false, Ordering::SeqCst)
    }

    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn detect_current_session_type() -> SessionType {
    SystemContextDetector::detect_current()
        .map(|context| context.session_type)
        .unwrap_or(SessionType::Unknown)
}

fn should_enable_x11_input_target_watcher(session_type: SessionType) -> bool {
    session_type == SessionType::X11
}

fn format_session_type(session_type: SessionType) -> &'static str {
    match session_type {
        SessionType::X11 => "x11",
        SessionType::Wayland => "wayland",
        SessionType::Unknown => "unknown",
    }
}

fn initialize_x11_switcher_for_session<T, F>(session_type: SessionType, init_x11: F) -> Option<T>
where
    F: FnOnce() -> Result<T, SwitcherError>,
{
    let strategy = LayoutSwitchStrategy::for_session_type(session_type);

    match strategy {
        LayoutSwitchStrategy::X11 => match init_x11() {
            Ok(switcher) => {
                log_input_debug(
                    "layout-switch-strategy",
                    &format!(
                        "session_type={} strategy={}",
                        format_session_type(session_type),
                        strategy.as_str()
                    ),
                );
                Some(switcher)
            }
            Err(error) => {
                log_input_debug(
                    "layout-switch-strategy",
                    &format!(
                        "session_type={} strategy=uinput reason=x11-init-failed error={error}",
                        format_session_type(session_type)
                    ),
                );
                None
            }
        },
        LayoutSwitchStrategy::UinputFallback => {
            log_input_debug(
                "layout-switch-strategy",
                &format!(
                    "session_type={} strategy={}",
                    format_session_type(session_type),
                    strategy.as_str()
                ),
            );
            None
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
                    eprintln!(
                        "[input] Writer path виртуальной клавиатуры аварийно завершился: {reason}"
                    );
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

    fn apply_same_layout_correction(
        &self,
        plan: CorrectionPlan,
        config: RuntimeConfigSnapshot,
        modifiers: ModifierState,
    ) -> Result<(), SwitcherError> {
        self.run_transaction(WriterTransactionKind::ApplySameLayoutCorrection {
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
            match self
                .command_tx
                .try_send(WriterCommand::Fast(command.clone()))
            {
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

impl Drop for InputTargetWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ActiveWindowMonitor {
    fn connect() -> io::Result<Self> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _, EventMask};

        let (conn, screen_num) =
            x11rb::connect(None).map_err(|error| io::Error::other(error.to_string()))?;
        let root = conn.setup().roots[screen_num].root;
        let active_window_atom = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?
            .atom;

        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::default().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|error| io::Error::other(error.to_string()))?
        .check()
        .map_err(|error| io::Error::other(error.to_string()))?;
        conn.flush()
            .map_err(|error| io::Error::other(error.to_string()))?;

        let current_window = Self::query_active_window(&conn, root, active_window_atom)?;
        Ok(Self {
            conn,
            root,
            active_window_atom,
            current_window,
        })
    }

    fn poll_change(&mut self) -> io::Result<Option<(Option<u32>, Option<u32>)>> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::Event;

        loop {
            let Some(event) = self
                .conn
                .poll_for_event()
                .map_err(|error| io::Error::other(error.to_string()))?
            else {
                return Ok(None);
            };

            match event {
                Event::PropertyNotify(property)
                    if property.window == self.root && property.atom == self.active_window_atom =>
                {
                    let previous_window = self.current_window;
                    let current_window =
                        Self::query_active_window(&self.conn, self.root, self.active_window_atom)?;
                    if current_window != previous_window {
                        self.current_window = current_window;
                        return Ok(Some((previous_window, current_window)));
                    }
                }
                _ => {}
            }
        }
    }

    fn query_active_window(
        conn: &x11rb::rust_connection::RustConnection,
        root: u32,
        active_window_atom: u32,
    ) -> io::Result<Option<u32>> {
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

        let reply = conn
            .get_property(false, root, active_window_atom, AtomEnum::WINDOW, 0, 1)
            .map_err(|error| io::Error::other(error.to_string()))?
            .reply()
            .map_err(|error| io::Error::other(error.to_string()))?;

        Ok(reply.value32().and_then(|mut values| values.next()))
    }
}

fn configured_keyboard_path() -> Option<PathBuf> {
    let raw = env::var_os(KEYBOARD_PATH_ENV)?;
    if raw.is_empty() {
        return None;
    }

    Some(PathBuf::from(raw))
}

fn resolve_keyboard_path() -> Result<PathBuf, SwitcherError> {
    if let Some(path) = configured_keyboard_path() {
        log_input_debug(
            "keyboard-detect",
            &format!("source=env path={}", path.display()),
        );
        return Ok(path);
    }

    if let Some(path) = find_keyboard_via_symlinks()? {
        return Ok(path);
    }

    match find_keyboard() {
        Some(path) => {
            log_input_debug(
                "keyboard-detect",
                &format!("source=enumerate path={}", path.display()),
            );
            Ok(path)
        }
        None => {
            log_input_debug("keyboard-detect", "source=enumerate result=not-found");
            Err(SwitcherError::KeyboardNotFound)
        }
    }
}

fn find_keyboard_via_symlinks() -> Result<Option<PathBuf>, SwitcherError> {
    let search_roots: Vec<&Path> = KEYBOARD_SYMLINK_DIRS.iter().map(Path::new).collect();
    let candidates = collect_keyboard_symlink_candidates_from_dirs(&search_roots);
    log_input_debug(
        "keyboard-detect-symlinks",
        &format!("candidates={}", candidates.len()),
    );

    let mut first_access_denied: Option<(PathBuf, io::Error)> = None;

    for path in candidates {
        match Device::open(&path) {
            Ok(_) => {
                log_input_debug(
                    "keyboard-detect-symlinks",
                    &format!("result=selected path={}", path.display()),
                );
                return Ok(Some(path));
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                log_input_debug(
                    "keyboard-detect-symlinks",
                    &format!("result=access-denied path={} error={error}", path.display()),
                );
                if first_access_denied.is_none() {
                    first_access_denied = Some((path, error));
                }
            }
            Err(error) => {
                log_input_debug(
                    "keyboard-detect-symlinks",
                    &format!("result=skip path={} error={error}", path.display()),
                );
            }
        }
    }

    if let Some((path, error)) = first_access_denied {
        return Err(map_keyboard_open_error(&path, error));
    }

    Ok(None)
}

fn collect_keyboard_symlink_candidates_from_dirs(dirs: &[&Path]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };

        let mut dir_candidates: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(KEYBOARD_SYMLINK_SUFFIX))
            })
            .collect();
        dir_candidates.sort();
        candidates.extend(dir_candidates);
    }

    candidates
}

fn map_keyboard_open_error(path: &Path, error: io::Error) -> SwitcherError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        SwitcherError::KeyboardAccessDenied {
            path: path.to_path_buf(),
            source: error,
        }
    } else {
        SwitcherError::Io(error)
    }
}

fn open_pointer_devices(paths: Vec<PathBuf>) -> Vec<PointerDeviceState> {
    let mut devices = Vec::new();

    for path in paths {
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        if set_nonblocking(&device).is_ok() {
            devices.push(PointerDeviceState {
                device,
                pressed_buttons: HashSet::new(),
            });
        }
    }

    devices
}

fn create_virtual_keyboard(name: &str) -> Result<uinput::Device, SwitcherError> {
    ensure_uinput_writable()?;

    let virtual_device = uinput::default()?
        .name(name)?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(500));
    Ok(virtual_device)
}

fn ensure_uinput_writable() -> Result<PathBuf, SwitcherError> {
    let mut first_existing_path: Option<PathBuf> = None;
    let mut last_error: Option<io::Error> = None;

    for raw_path in UINPUT_PATHS {
        let path = PathBuf::from(raw_path);
        match OpenOptions::new().write(true).open(&path) {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                last_error = Some(error);
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Err(SwitcherError::UinputAccessDenied {
                    path,
                    source: error,
                });
            }
            Err(error) => {
                if first_existing_path.is_none() && path.exists() {
                    first_existing_path = Some(path);
                }
                last_error = Some(error);
            }
        }
    }

    if let Some(path) = first_existing_path {
        if let Some(error) = last_error {
            return Err(SwitcherError::Io(io::Error::new(
                error.kind(),
                format!("failed to open {}: {}", path.display(), error),
            )));
        }
    }

    Err(last_error.map(SwitcherError::Io).unwrap_or_else(|| {
        SwitcherError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "uinput device not found",
        ))
    }))
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

fn wait_for_device_input(device: &Device, timeout: Duration) -> io::Result<bool> {
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    let mut poll_fd = libc::pollfd {
        fd: device.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result > 0 {
            return Ok((poll_fd.revents & libc::POLLIN) != 0);
        }
        if result == 0 {
            return Ok(false);
        }

        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
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

fn format_x11_window(window: Option<u32>) -> String {
    match window {
        Some(window) => format!("0x{window:x}"),
        None => "none".to_string(),
    }
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

use crate::daemon::layout_switcher::{
    LayoutSwitchStrategy, LayoutSwitcher, UinputLayoutSwitcher, X11LayoutSwitcher,
};

fn replay_shift_for_stroke(
    stroke: &crate::daemon::switch_logic::Keystroke,
    caps_lock: bool,
) -> bool {
    if is_case_sensitive_letter_key(stroke.key) {
        stroke.shift ^ caps_lock
    } else {
        stroke.shift
    }
}

fn is_case_sensitive_letter_key(key: Key) -> bool {
    matches!(
        key,
        Key::KEY_A
            | Key::KEY_B
            | Key::KEY_C
            | Key::KEY_D
            | Key::KEY_E
            | Key::KEY_F
            | Key::KEY_G
            | Key::KEY_H
            | Key::KEY_I
            | Key::KEY_J
            | Key::KEY_K
            | Key::KEY_L
            | Key::KEY_M
            | Key::KEY_N
            | Key::KEY_O
            | Key::KEY_P
            | Key::KEY_Q
            | Key::KEY_R
            | Key::KEY_S
            | Key::KEY_T
            | Key::KEY_U
            | Key::KEY_V
            | Key::KEY_W
            | Key::KEY_X
            | Key::KEY_Y
            | Key::KEY_Z
            | Key::KEY_GRAVE
            | Key::KEY_LEFTBRACE
            | Key::KEY_RIGHTBRACE
            | Key::KEY_SEMICOLON
            | Key::KEY_APOSTROPHE
            | Key::KEY_COMMA
            | Key::KEY_DOT
    )
}

fn run_correction(
    device: &mut uinput::Device,
    plan: &CorrectionPlan,
    config: &RuntimeConfigSnapshot,
    modifiers: ModifierState,
    x11_switcher: &mut Option<X11LayoutSwitcher>,
    switch_layout: bool,
) -> Result<(), SwitcherError> {
    log_input_debug(
        "correction-transaction",
        &format!(
            "switch_layout={} combo={:?} buffer_len={} extra_backspaces={} layout_delay_ms={} typing_ms={} backspace_ms={}",
            switch_layout,
            config.layout_switch_combo,
            plan.buffer.len(),
            plan.extra_backspaces,
            config.layout_delay_ms,
            config.typing_ms,
            config.backspace_ms,
        ),
    );
    release_modifiers(device, modifiers)?;
    for _ in 0..(plan.buffer.len() + plan.extra_backspaces) {
        device.press(&uinput::event::keyboard::Key::BackSpace)?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(2));
        device.release(&uinput::event::keyboard::Key::BackSpace)?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(config.backspace_ms));
    }

    if switch_layout {
        if let Some(switcher) = x11_switcher {
            log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=x11 hold_ms={}",
                    config.layout_switch_combo, config.layout_delay_ms
                ),
            );
            if let Err(e) = switcher.switch_layout(config.layout_switch_combo) {
                log_input_debug("x11-layout-switcher", &format!("failed: {}", e));
                log_input_debug(
                    "correction-layout-switch",
                    &format!(
                        "combo={:?} strategy=x11 result=error fallback=uinput",
                        config.layout_switch_combo
                    ),
                );
                let mut uinput_switcher = UinputLayoutSwitcher::new(device, config.layout_delay_ms);
                uinput_switcher.switch_layout(config.layout_switch_combo)?;
                log_input_debug(
                    "correction-layout-switch",
                    &format!(
                        "combo={:?} strategy=uinput result=ok hold_ms={}",
                        config.layout_switch_combo, config.layout_delay_ms
                    ),
                );
            } else {
                log_input_debug(
                    "correction-layout-switch",
                    &format!(
                        "combo={:?} strategy=x11 result=ok",
                        config.layout_switch_combo
                    ),
                );
            }
        } else {
            log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=uinput hold_ms={}",
                    config.layout_switch_combo, config.layout_delay_ms
                ),
            );
            let mut uinput_switcher = UinputLayoutSwitcher::new(device, config.layout_delay_ms);
            uinput_switcher.switch_layout(config.layout_switch_combo)?;
            log_input_debug(
                "correction-layout-switch",
                &format!(
                    "combo={:?} strategy=uinput result=ok",
                    config.layout_switch_combo
                ),
            );
        }
        thread::sleep(Duration::from_millis(config.layout_delay_ms));
    }

    for stroke in &plan.buffer {
        let effective_shift = replay_shift_for_stroke(stroke, modifiers.is_caps_lock_active());
        if effective_shift {
            device.press(&uinput::event::keyboard::Key::LeftShift)?;
            device.synchronize()?;
            thread::sleep(Duration::from_millis(1));
        }
        device.write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 1)?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(2));
        device.write(INPUT_EVENT_KEYBOARD, stroke.key.code() as i32, 0)?;
        if effective_shift {
            device.release(&uinput::event::keyboard::Key::LeftShift)?;
        }
        device.synchronize()?;
        thread::sleep(Duration::from_millis(config.typing_ms));
    }

    if plan.extra_backspaces > 0 {
        device.press(&uinput::event::keyboard::Key::Space)?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(2));
        device.release(&uinput::event::keyboard::Key::Space)?;
        device.synchronize()?;
    }

    restore_modifiers(device, modifiers)?;
    Ok(())
}

fn run_virtual_keyboard_writer_loop(
    mut device: uinput::Device,
    command_rx: mpsc::Receiver<WriterCommand>,
) -> Result<(), SwitcherError> {
    let session_type = detect_current_session_type();
    let mut x11_switcher =
        initialize_x11_switcher_for_session(session_type, X11LayoutSwitcher::new);

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
                    device.synchronize()?;
                    thread::sleep(Duration::from_millis(2));
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
                        } => run_correction(
                            &mut device,
                            &plan,
                            &config,
                            modifiers,
                            &mut x11_switcher,
                            true,
                        ),
                        WriterTransactionKind::ApplySameLayoutCorrection {
                            plan,
                            config,
                            modifiers,
                        } => run_correction(
                            &mut device,
                            &plan,
                            &config,
                            modifiers,
                            &mut x11_switcher,
                            false,
                        ),
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
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::io;
    use std::path::Path;

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

    #[test]
    fn replay_shift_uses_caps_lock_to_preserve_visible_case() {
        let lowercase_target = crate::daemon::switch_logic::Keystroke {
            key: Key::KEY_H,
            shift: false,
            caps_lock: false,
        };
        let uppercase_target = crate::daemon::switch_logic::Keystroke {
            key: Key::KEY_H,
            shift: true,
            caps_lock: false,
        };

        assert!(replay_shift_for_stroke(&lowercase_target, true));
        assert!(!replay_shift_for_stroke(&uppercase_target, true));
        assert!(!replay_shift_for_stroke(&lowercase_target, false));
        assert!(replay_shift_for_stroke(&uppercase_target, false));
    }

    #[test]
    fn keyboard_symlink_candidates_prioritize_by_path_before_by_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let by_path = temp_dir.path().join("by-path");
        let by_id = temp_dir.path().join("by-id");
        fs::create_dir_all(&by_path).unwrap();
        fs::create_dir_all(&by_id).unwrap();

        let by_path_keyboard = by_path.join("platform-i8042-serio-0-event-kbd");
        let by_id_keyboard = by_id.join("usb-Logitech_USB_Keyboard-event-kbd");
        fs::write(&by_path_keyboard, "").unwrap();
        fs::write(&by_id_keyboard, "").unwrap();

        let candidates =
            collect_keyboard_symlink_candidates_from_dirs(&[by_path.as_path(), by_id.as_path()]);

        assert_eq!(
            candidates,
            vec![by_path_keyboard, by_id_keyboard],
            "stable symlink search must prefer /dev/input/by-path before /dev/input/by-id",
        );
    }

    #[test]
    fn keyboard_open_permission_denied_maps_to_keyboard_access_denied() {
        let error = map_keyboard_open_error(
            Path::new("/dev/input/event4"),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );

        assert!(matches!(error, SwitcherError::KeyboardAccessDenied { .. }));
    }

    #[test]
    fn x11_input_target_watcher_is_enabled_only_for_x11_sessions() {
        assert!(should_enable_x11_input_target_watcher(SessionType::X11));
        assert!(!should_enable_x11_input_target_watcher(
            SessionType::Wayland
        ));
        assert!(!should_enable_x11_input_target_watcher(
            SessionType::Unknown
        ));
    }

    #[test]
    fn layout_switcher_initializer_skips_x11_outside_x11_sessions() {
        let init_called = Cell::new(false);

        let switcher = initialize_x11_switcher_for_session(SessionType::Wayland, || {
            init_called.set(true);
            Ok::<_, SwitcherError>("x11")
        });

        assert_eq!(switcher, None);
        assert!(
            !init_called.get(),
            "Wayland strategy selection must not initialize the X11 switcher"
        );
    }

    #[test]
    fn layout_switcher_initializer_uses_x11_in_x11_sessions() {
        let init_called = Cell::new(false);

        let switcher = initialize_x11_switcher_for_session(SessionType::X11, || {
            init_called.set(true);
            Ok::<_, SwitcherError>("x11")
        });

        assert_eq!(switcher, Some("x11"));
        assert!(
            init_called.get(),
            "X11 strategy selection must keep using the X11 switcher"
        );
    }

    #[test]
    fn layout_switcher_initializer_falls_back_to_uinput_when_x11_init_fails() {
        let init_called = Cell::new(false);

        let switcher = initialize_x11_switcher_for_session(SessionType::X11, || {
            init_called.set(true);
            Err::<&str, _>(SwitcherError::Io(io::Error::other("x11 unavailable")))
        });

        assert_eq!(switcher, None);
        assert!(
            init_called.get(),
            "X11 session should attempt X11 init before falling back to uinput"
        );
    }
}
