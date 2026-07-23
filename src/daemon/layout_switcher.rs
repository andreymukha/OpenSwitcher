use crate::error::SwitcherError;
use crate::model::{LayoutSwitchCombo, SessionType};
use std::cell::Cell;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSwitchStrategy {
    X11,
    UinputFallback,
}

impl LayoutSwitchStrategy {
    pub fn for_session_type(session_type: SessionType) -> Self {
        match session_type {
            SessionType::X11 => Self::X11,
            SessionType::Wayland | SessionType::Unknown => Self::UinputFallback,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::UinputFallback => "uinput",
        }
    }
}

pub trait LayoutSwitcher {
    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError>;

    fn switch_layout_with_hooks(
        &mut self,
        combo: LayoutSwitchCombo,
        hooks: &LayoutSwitchHooks<'_>,
    ) -> Result<(), SwitcherError> {
        hooks.checkpoint()?;
        let result = self.switch_layout(combo);
        let checkpoint_result = hooks.checkpoint();
        result?;
        checkpoint_result
    }
}

pub struct LayoutSwitchHooks<'a> {
    checkpoint: &'a dyn Fn() -> Result<(), SwitcherError>,
    authorize_mutation: &'a dyn Fn() -> Result<(), SwitcherError>,
    mutation_authorized: Cell<bool>,
}

impl<'a> LayoutSwitchHooks<'a> {
    pub fn new(
        checkpoint: &'a dyn Fn() -> Result<(), SwitcherError>,
        authorize_mutation: &'a dyn Fn() -> Result<(), SwitcherError>,
    ) -> Self {
        Self {
            checkpoint,
            authorize_mutation,
            mutation_authorized: Cell::new(false),
        }
    }

    fn checkpoint(&self) -> Result<(), SwitcherError> {
        (self.checkpoint)()
    }

    pub(crate) fn authorize_mutation(&self) -> Result<(), SwitcherError> {
        (self.authorize_mutation)()?;
        self.mutation_authorized.set(true);
        Ok(())
    }

    pub(crate) fn mutation_was_authorized(&self) -> bool {
        self.mutation_authorized.get()
    }
}

trait UinputLayoutSink {
    fn press_key(&mut self, key: &uinput::event::keyboard::Key) -> Result<(), SwitcherError>;
    fn release_key(&mut self, key: &uinput::event::keyboard::Key) -> Result<(), SwitcherError>;
    fn synchronize_keys(&mut self) -> Result<(), SwitcherError>;
}

impl UinputLayoutSink for uinput::Device {
    fn press_key(&mut self, key: &uinput::event::keyboard::Key) -> Result<(), SwitcherError> {
        self.press(key).map_err(Into::into)
    }

    fn release_key(&mut self, key: &uinput::event::keyboard::Key) -> Result<(), SwitcherError> {
        self.release(key).map_err(Into::into)
    }

    fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
        self.synchronize().map_err(Into::into)
    }
}

pub struct UinputLayoutSwitcher<'a> {
    device: &'a mut uinput::Device,
    delay_ms: u64,
    delay_waiter: Option<&'a dyn Fn(Duration) -> Result<(), SwitcherError>>,
}

impl<'a> UinputLayoutSwitcher<'a> {
    pub fn new(device: &'a mut uinput::Device, delay_ms: u64) -> Self {
        Self {
            device,
            delay_ms,
            delay_waiter: None,
        }
    }

    pub fn new_with_waiter(
        device: &'a mut uinput::Device,
        delay_ms: u64,
        delay_waiter: &'a dyn Fn(Duration) -> Result<(), SwitcherError>,
    ) -> Self {
        Self {
            device,
            delay_ms,
            delay_waiter: Some(delay_waiter),
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

fn release_uinput_layout_keys(
    sink: &mut dyn UinputLayoutSink,
    pressed: &[&uinput::event::keyboard::Key],
) -> Result<(), SwitcherError> {
    let mut first_error = None;
    for key in pressed.iter().rev() {
        if let Err(error) = sink.release_key(key) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    if let Err(error) = sink.synchronize_keys() {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn finish_uinput_layout_failure_after_cleanup(
    error: SwitcherError,
    cleanup_result: Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    if matches!(
        error,
        SwitcherError::VirtualKeyboardWriterTransactionCancelled { .. }
    ) {
        cleanup_result?;
    }
    Err(error)
}

fn execute_uinput_layout_switch_sequence(
    sink: &mut dyn UinputLayoutSink,
    combo: LayoutSwitchCombo,
    delay: Duration,
    ensure_active: &dyn Fn() -> Result<(), SwitcherError>,
    wait: &dyn Fn(Duration) -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    let (modifiers, trigger_key) = UinputLayoutSwitcher::layout_switch_combo_sequence(combo);
    let mut pressed = Vec::new();

    for key in modifiers.iter().chain(trigger_key) {
        if let Err(error) = ensure_active() {
            let cleanup_result = release_uinput_layout_keys(sink, &pressed);
            return finish_uinput_layout_failure_after_cleanup(error, cleanup_result);
        }
        if let Err(error) = sink.press_key(key) {
            let cleanup_result = release_uinput_layout_keys(sink, &pressed);
            return finish_uinput_layout_failure_after_cleanup(error, cleanup_result);
        }
        pressed.push(key);
    }

    if let Err(error) = sink.synchronize_keys() {
        let cleanup_result = release_uinput_layout_keys(sink, &pressed);
        return finish_uinput_layout_failure_after_cleanup(error, cleanup_result);
    }
    let wait_result = wait(delay);
    let release_result = release_uinput_layout_keys(sink, &pressed);
    match wait_result {
        Ok(()) => release_result,
        Err(error) => finish_uinput_layout_failure_after_cleanup(error, release_result),
    }
}

impl<'a> LayoutSwitcher for UinputLayoutSwitcher<'a> {
    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
        let delay = Duration::from_millis(self.delay_ms);
        let delay_waiter = self.delay_waiter;
        let ensure_active = || match delay_waiter {
            Some(waiter) => waiter(Duration::ZERO),
            None => Ok(()),
        };
        let wait = |duration| match delay_waiter {
            Some(waiter) => waiter(duration),
            None => {
                std::thread::sleep(duration);
                Ok(())
            }
        };
        execute_uinput_layout_switch_sequence(self.device, combo, delay, &ensure_active, &wait)
    }
}

fn execute_x11_layout_switch(
    hooks: &LayoutSwitchHooks<'_>,
    mut read_num_groups: impl FnMut() -> Result<u8, SwitcherError>,
    mut read_current_group: impl FnMut() -> Result<u8, SwitcherError>,
    mut latch_group: impl FnMut(u8) -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    hooks.checkpoint()?;
    let num_groups = read_num_groups();
    let num_groups = num_groups?;
    hooks.checkpoint()?;

    hooks.checkpoint()?;
    let current_group = read_current_group();
    let current_group = current_group?;
    hooks.checkpoint()?;

    if num_groups <= 1 {
        return Ok(());
    }

    let next_group = (current_group + 1) % num_groups;
    hooks.authorize_mutation()?;
    let result = latch_group(next_group);
    let checkpoint_result = hooks.checkpoint();
    result?;
    checkpoint_result
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
    fn switch_layout(&mut self, combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
        let checkpoint = || Ok(());
        let hooks = LayoutSwitchHooks::new(&checkpoint, &checkpoint);
        self.switch_layout_with_hooks(combo, &hooks)
    }

    fn switch_layout_with_hooks(
        &mut self,
        _combo: LayoutSwitchCombo,
        hooks: &LayoutSwitchHooks<'_>,
    ) -> Result<(), SwitcherError> {
        use x11rb::protocol::xkb::{self, ConnectionExt as _};
        let conn = &self.conn;
        execute_x11_layout_switch(
            hooks,
            || {
                conn.xkb_get_controls(xkb::ID::USE_CORE_KBD.into())
                    .map_err(|e| {
                        SwitcherError::Io(std::io::Error::other(format!(
                            "XKB controls failed: {e}"
                        )))
                    })?
                    .reply()
                    .map(|controls| controls.num_groups)
                    .map_err(|e| {
                        SwitcherError::Io(std::io::Error::other(format!(
                            "XKB controls reply failed: {e}"
                        )))
                    })
            },
            || {
                conn.xkb_get_state(xkb::ID::USE_CORE_KBD.into())
                    .map_err(|e| {
                        SwitcherError::Io(std::io::Error::other(format!("XKB state failed: {e}")))
                    })?
                    .reply()
                    .map(|state| u8::from(state.group))
                    .map_err(|e| {
                        SwitcherError::Io(std::io::Error::other(format!(
                            "XKB state reply failed: {e}"
                        )))
                    })
            },
            |next_group| {
                conn.xkb_latch_lock_state(
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
                conn.flush().map_err(|e| {
                    SwitcherError::Io(std::io::Error::other(format!("XKB flush failed: {e}")))
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeUinputLayoutSink {
        events: Vec<String>,
        release_calls: usize,
        fail_release_on_call: Option<usize>,
    }

    impl UinputLayoutSink for FakeUinputLayoutSink {
        fn press_key(&mut self, key: &uinput::event::keyboard::Key) -> Result<(), SwitcherError> {
            self.events.push(format!("down:{key:?}"));
            Ok(())
        }

        fn release_key(&mut self, key: &uinput::event::keyboard::Key) -> Result<(), SwitcherError> {
            self.release_calls += 1;
            self.events.push(format!("up:{key:?}"));
            if self.fail_release_on_call == Some(self.release_calls) {
                Err(SwitcherError::Io(std::io::Error::other(
                    "layout key release failed",
                )))
            } else {
                Ok(())
            }
        }

        fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
            self.events.push("sync".to_string());
            Ok(())
        }
    }

    #[test]
    fn uinput_combo_cancellation_between_presses_starts_no_new_key_down() {
        let mut sink = FakeUinputLayoutSink::default();
        let checks = std::cell::Cell::new(0usize);
        let ensure_active = || {
            let next = checks.get() + 1;
            checks.set(next);
            if next == 2 {
                Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 201 })
            } else {
                Ok(())
            }
        };

        let error = execute_uinput_layout_switch_sequence(
            &mut sink,
            LayoutSwitchCombo::CtrlShift,
            Duration::ZERO,
            &ensure_active,
            &|_| Ok(()),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 201 }
        ));
        assert_eq!(
            sink.events,
            vec!["down:LeftControl", "up:LeftControl", "sync"]
        );
    }

    #[test]
    fn uinput_layout_soft_cancel_does_not_mask_trigger_release_failure() {
        let mut sink = FakeUinputLayoutSink {
            fail_release_on_call: Some(1),
            ..Default::default()
        };

        let error = execute_uinput_layout_switch_sequence(
            &mut sink,
            LayoutSwitchCombo::CtrlSpace,
            Duration::ZERO,
            &|| Ok(()),
            &|_| Err(SwitcherError::VirtualKeyboardWriterTransactionCancelled { request_id: 203 }),
        )
        .unwrap_err();

        assert!(error.to_string().contains("layout key release failed"));
        assert_eq!(
            sink.events,
            vec![
                "down:LeftControl",
                "down:Space",
                "sync",
                "up:Space",
                "up:LeftControl",
                "sync",
            ]
        );
    }

    #[test]
    fn uinput_layout_soft_cancel_between_presses_does_not_mask_cleanup_failure() {
        let mut sink = FakeUinputLayoutSink {
            fail_release_on_call: Some(1),
            ..Default::default()
        };
        let checkpoint_calls = Cell::new(0);

        let error = execute_uinput_layout_switch_sequence(
            &mut sink,
            LayoutSwitchCombo::CtrlSpace,
            Duration::ZERO,
            &|| {
                let call = checkpoint_calls.get();
                checkpoint_calls.set(call + 1);
                if call == 1 {
                    Err(SwitcherError::VirtualKeyboardWriterTransactionCancelled {
                        request_id: 204,
                    })
                } else {
                    Ok(())
                }
            },
            &|_| Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("layout key release failed"));
        assert_eq!(
            sink.events,
            vec!["down:LeftControl", "up:LeftControl", "sync"]
        );
    }

    struct FailingDefaultHookLayoutSwitcher;

    impl LayoutSwitcher for FailingDefaultHookLayoutSwitcher {
        fn switch_layout(&mut self, _combo: LayoutSwitchCombo) -> Result<(), SwitcherError> {
            Err(SwitcherError::Io(std::io::Error::other(
                "layout cleanup failed",
            )))
        }
    }

    #[test]
    fn default_layout_hooks_do_not_mask_operation_failure_with_post_checkpoint_cancel() {
        let mut switcher = FailingDefaultHookLayoutSwitcher;
        let checkpoint_calls = Cell::new(0);
        let checkpoint = || {
            let call = checkpoint_calls.get();
            checkpoint_calls.set(call + 1);
            if call == 1 {
                Err(SwitcherError::VirtualKeyboardWriterTransactionCancelled { request_id: 205 })
            } else {
                Ok(())
            }
        };
        let authorize = || Ok(());
        let hooks = LayoutSwitchHooks::new(&checkpoint, &authorize);

        let error = switcher
            .switch_layout_with_hooks(LayoutSwitchCombo::CapsLock, &hooks)
            .unwrap_err();

        assert!(error.to_string().contains("layout cleanup failed"));
    }

    #[test]
    fn x11_latch_error_wins_over_post_mutation_soft_cancel() {
        let cancelled = Cell::new(false);
        let checkpoint = || {
            if cancelled.get() {
                Err(SwitcherError::VirtualKeyboardWriterTransactionCancelled { request_id: 206 })
            } else {
                Ok(())
            }
        };
        let authorize = || Ok(());
        let hooks = LayoutSwitchHooks::new(&checkpoint, &authorize);

        let error = execute_x11_layout_switch(
            &hooks,
            || Ok(2),
            || Ok(0),
            |_| {
                cancelled.set(true);
                Err(SwitcherError::Io(std::io::Error::other("x11 latch failed")))
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("x11 latch failed"));
        assert!(hooks.mutation_was_authorized());
    }

    #[test]
    fn x11_cancellation_after_blocking_read_starts_no_latch_mutation() {
        let cancelled = std::cell::Cell::new(false);
        let controls_reads = std::cell::Cell::new(0usize);
        let state_reads = std::cell::Cell::new(0usize);
        let latch_mutations = std::cell::Cell::new(0usize);
        let checkpoint = || {
            if cancelled.get() {
                Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 202 })
            } else {
                Ok(())
            }
        };
        let hooks = LayoutSwitchHooks::new(&checkpoint, &checkpoint);

        let error = execute_x11_layout_switch(
            &hooks,
            || {
                controls_reads.set(controls_reads.get() + 1);
                cancelled.set(true);
                Ok(2)
            },
            || {
                state_reads.set(state_reads.get() + 1);
                Ok(0)
            },
            |_| {
                latch_mutations.set(latch_mutations.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 202 }
        ));
        assert_eq!(controls_reads.get(), 1);
        assert_eq!(state_reads.get(), 0);
        assert_eq!(latch_mutations.get(), 0);
    }

    #[test]
    fn prefers_x11_strategy_only_for_x11_sessions() {
        assert_eq!(
            LayoutSwitchStrategy::for_session_type(SessionType::X11),
            LayoutSwitchStrategy::X11
        );
        assert_eq!(
            LayoutSwitchStrategy::for_session_type(SessionType::Wayland),
            LayoutSwitchStrategy::UinputFallback
        );
        assert_eq!(
            LayoutSwitchStrategy::for_session_type(SessionType::Unknown),
            LayoutSwitchStrategy::UinputFallback
        );
    }

    #[test]
    fn formats_strategy_for_logs() {
        assert_eq!(LayoutSwitchStrategy::X11.as_str(), "x11");
        assert_eq!(LayoutSwitchStrategy::UinputFallback.as_str(), "uinput");
    }
}
