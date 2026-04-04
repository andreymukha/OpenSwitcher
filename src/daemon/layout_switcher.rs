use crate::error::SwitcherError;
use crate::model::LayoutSwitchCombo;

#[derive(Debug, Clone, Copy)]
pub enum LayoutSwitchStrategy {
    X11,
    UinputFallback,
}

pub trait LayoutSwitcher {
    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError>;
}

pub struct UinputLayoutSwitcher<'a> {
    device: &'a mut uinput::Device,
    delay_ms: u64,
}

impl<'a> UinputLayoutSwitcher<'a> {
    pub fn new(device: &'a mut uinput::Device, delay_ms: u64) -> Self {
        Self { device, delay_ms }
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
        static CAPS_LOCK: [Key; 1] = [Key::CapsLock];

        match combo {
            LayoutSwitchCombo::CtrlShift => (&CTRL_SHIFT, None),
            LayoutSwitchCombo::AltShift => (&ALT_SHIFT, None),
            LayoutSwitchCombo::CtrlSpace => (&CTRL_SPACE, Some(&Key::Space)),
            LayoutSwitchCombo::SuperSpace => (&SUPER_SPACE, Some(&Key::Space)),
            LayoutSwitchCombo::LeftCtrlLeftShift => (&LEFT_CTRL_LEFT_SHIFT, None),
            LayoutSwitchCombo::RightCtrlRightShift => (&RIGHT_CTRL_RIGHT_SHIFT, None),
            LayoutSwitchCombo::LeftAltLeftShift => (&LEFT_ALT_LEFT_SHIFT, None),
            LayoutSwitchCombo::RightAltRightShift => (&RIGHT_ALT_RIGHT_SHIFT, None),
            LayoutSwitchCombo::CapsLock => (&CAPS_LOCK, None),
        }
    }
}

impl<'a> LayoutSwitcher for UinputLayoutSwitcher<'a> {
    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
        let (modifiers, trigger_key) = Self::layout_switch_combo_sequence(combo);

        for modifier in modifiers {
            self.device.press(modifier)?;
        }

        if let Some(key) = trigger_key {
            self.device.press(key)?;
        }

        self.device.synchronize()?;
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));

        if let Some(key) = trigger_key {
            self.device.release(key)?;
        }

        for modifier in modifiers.iter().rev() {
            self.device.release(modifier)?;
        }

        self.device.synchronize()?;
        Ok(())
    }
}

pub struct X11LayoutSwitcher {
    conn: x11rb::rust_connection::RustConnection,
}

impl X11LayoutSwitcher {
    pub fn new() -> Result<Self, SwitcherError> {
        let (conn, _) = x11rb::connect(None)
            .map_err(|e| SwitcherError::Io(std::io::Error::other(e.to_string())))?;

        use x11rb::protocol::xkb::ConnectionExt as _;
        conn.xkb_use_extension(1, 0)
            .map_err(|e| SwitcherError::Io(std::io::Error::other(e.to_string())))?
            .reply()
            .map_err(|e| SwitcherError::Io(std::io::Error::other(e.to_string())))?;

        Ok(Self { conn })
    }
}

impl LayoutSwitcher for X11LayoutSwitcher {
    fn switch_layout(&mut self, _combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
        use x11rb::protocol::xkb::{self, ConnectionExt as _};

        let controls = self
            .conn
            .xkb_get_controls(xkb::ID::USE_CORE_KBD.into())
            .map_err(|e| {
                SwitcherError::Io(std::io::Error::other(format!("XKB controls failed: {e}")))
            })?
            .reply()
            .map_err(|e| {
                SwitcherError::Io(std::io::Error::other(format!(
                    "XKB controls reply failed: {e}"
                )))
            })?;

        let state = self
            .conn
            .xkb_get_state(xkb::ID::USE_CORE_KBD.into())
            .map_err(|e| {
                SwitcherError::Io(std::io::Error::other(format!("XKB state failed: {e}")))
            })?
            .reply()
            .map_err(|e| {
                SwitcherError::Io(std::io::Error::other(format!(
                    "XKB state reply failed: {e}"
                )))
            })?;

        if controls.num_groups <= 1 {
            return Ok(());
        }

        let current_group = u8::from(state.group);
        let next_group = (current_group + 1) % controls.num_groups;

        self.conn
            .xkb_latch_lock_state(
                xkb::ID::USE_CORE_KBD.into(),
                0u8.into(),
                0u8.into(),
                true,
                next_group.into(),
                0u8.into(),
                false,
                0,
            )
            .map_err(|e| {
                SwitcherError::Io(std::io::Error::other(format!(
                    "XKB latch state failed: {e}"
                )))
            })?;

        use x11rb::connection::Connection as _;
        self.conn.flush().map_err(|e| {
            SwitcherError::Io(std::io::Error::other(format!("XKB flush failed: {e}")))
        })?;

        Ok(())
    }
}
