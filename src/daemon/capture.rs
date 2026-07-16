use crate::error::CaptureError;
use crate::model::{LayoutSwitchCaptureState, LayoutSwitchCombo};
use evdev::Key;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const UNSUPPORTED_MESSAGE: &str = "Эта комбинация сейчас не поддерживается OpenSwitcher.";
const CAPTURE_DEBUG_ENV: &str = "OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG";
const CAPTURE_DEBUG_FILE_ENV: &str = "OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG_FILE";
pub const CAPTURE_SOFT_LEASE: Duration = Duration::from_secs(10);
pub const CAPTURE_ABSOLUTE_LEASE: Duration = Duration::from_secs(65);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureOwner(String);

impl From<&str> for CaptureOwner {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for CaptureOwner {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug)]
struct CaptureLease {
    owner: CaptureOwner,
    soft_deadline: Instant,
    absolute_deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PhysicalCaptureKey {
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftShift,
    RightShift,
    Super,
    Space,
    CapsLock,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CaptureProgress {
    pressed_keys: BTreeSet<PhysicalCaptureKey>,
}

impl CaptureProgress {
    fn clear(&mut self) {
        self.pressed_keys.clear();
    }

    fn is_empty(&self) -> bool {
        self.pressed_keys.is_empty()
    }

    fn press_key(&mut self, key: PhysicalCaptureKey) {
        self.pressed_keys.insert(key);
    }

    fn release_key(&mut self, key: PhysicalCaptureKey) {
        self.pressed_keys.remove(&key);
    }

    fn evaluate(&self) -> EvaluatedCapture {
        use PhysicalCaptureKey as Key;

        let keys = &self.pressed_keys;
        let left_ctrl = keys.contains(&Key::LeftCtrl);
        let right_ctrl = keys.contains(&Key::RightCtrl);
        let left_alt = keys.contains(&Key::LeftAlt);
        let right_alt = keys.contains(&Key::RightAlt);
        let left_shift = keys.contains(&Key::LeftShift);
        let right_shift = keys.contains(&Key::RightShift);
        let super_key = keys.contains(&Key::Super);
        let space = keys.contains(&Key::Space);
        let caps_lock = keys.contains(&Key::CapsLock);

        if keys.is_empty() {
            return EvaluatedCapture::waiting("no-keys-pressed");
        }

        if keys.len() == 1 && caps_lock {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::caps_lock(),
                "matched-caps-lock",
            );
        }

        if keys.len() == 2 && left_ctrl && left_shift {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::left_ctrl_left_shift(),
                "matched-left-ctrl-left-shift",
            );
        }

        if keys.len() == 2 && right_ctrl && right_shift {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::right_ctrl_right_shift(),
                "matched-right-ctrl-right-shift",
            );
        }

        if keys.len() == 2 && left_alt && left_shift {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::left_alt_left_shift(),
                "matched-left-alt-left-shift",
            );
        }

        if keys.len() == 2 && right_alt && right_shift {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::right_alt_right_shift(),
                "matched-right-alt-right-shift",
            );
        }

        if keys.len() == 2 && (left_ctrl || right_ctrl) && (left_shift || right_shift) {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::ctrl_shift(),
                "matched-generic-ctrl-shift",
            );
        }

        if keys.len() == 2 && (left_alt || right_alt) && (left_shift || right_shift) {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::alt_shift(),
                "matched-generic-alt-shift",
            );
        }

        if keys.len() == 2 && space && (left_ctrl || right_ctrl) {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::ctrl_space(),
                "matched-ctrl-space",
            );
        }

        if keys.len() == 2 && space && super_key {
            return EvaluatedCapture::candidate(
                LayoutSwitchCombo::super_space(),
                "matched-super-space",
            );
        }

        if caps_lock || space {
            return EvaluatedCapture::unsupported("trigger-key-with-extra-keys");
        }

        if keys.len() == 1
            && (left_ctrl
                || right_ctrl
                || left_shift
                || right_shift
                || left_alt
                || right_alt
                || super_key)
        {
            return EvaluatedCapture::waiting("supported-prefix");
        }

        EvaluatedCapture::unsupported("no-whitelist-match")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureEvaluation {
    Waiting,
    Candidate(LayoutSwitchCombo),
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EvaluatedCapture {
    evaluation: CaptureEvaluation,
    reason: &'static str,
}

impl EvaluatedCapture {
    fn waiting(reason: &'static str) -> Self {
        Self {
            evaluation: CaptureEvaluation::Waiting,
            reason,
        }
    }

    fn candidate(combo: LayoutSwitchCombo, reason: &'static str) -> Self {
        Self {
            evaluation: CaptureEvaluation::Candidate(combo),
            reason,
        }
    }

    fn unsupported(reason: &'static str) -> Self {
        Self {
            evaluation: CaptureEvaluation::Unsupported,
            reason,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LayoutSwitchCaptureSession {
    progress: CaptureProgress,
    state: LayoutSwitchCaptureState,
    lease: Option<CaptureLease>,
}

impl LayoutSwitchCaptureSession {
    pub fn current_state(&self) -> LayoutSwitchCaptureState {
        self.state.clone()
    }

    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    pub fn start(&mut self) -> LayoutSwitchCaptureState {
        self.lease = None;
        self.progress.clear();
        self.state = LayoutSwitchCaptureState::waiting();
        log_capture_debug("start", None, None, &self.progress, None, "session-started");
        self.current_state()
    }

    pub fn cancel(&mut self) -> LayoutSwitchCaptureState {
        self.lease = None;
        self.progress.clear();
        self.state = LayoutSwitchCaptureState::cancelled();
        log_capture_debug(
            "cancel",
            None,
            None,
            &self.progress,
            None,
            "session-cancelled",
        );
        self.current_state()
    }

    pub fn finish(&mut self) -> LayoutSwitchCaptureState {
        self.lease = None;
        self.progress.clear();
        self.state = LayoutSwitchCaptureState::finished();
        log_capture_debug(
            "finish",
            None,
            None,
            &self.progress,
            None,
            "session-finished",
        );
        self.current_state()
    }

    pub fn start_owned(
        &mut self,
        owner: CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        self.expire_at(now);

        if let Some(lease) = self.lease.as_mut() {
            if lease.owner != owner {
                return Err(CaptureError::Busy);
            }

            lease.soft_deadline = (now + CAPTURE_SOFT_LEASE).min(lease.absolute_deadline);
            return Ok(self.current_state());
        }

        self.progress.clear();
        self.state = LayoutSwitchCaptureState::waiting();
        self.lease = Some(CaptureLease {
            owner,
            soft_deadline: now + CAPTURE_SOFT_LEASE,
            absolute_deadline: now + CAPTURE_ABSOLUTE_LEASE,
        });
        log_capture_debug(
            "start",
            None,
            None,
            &self.progress,
            None,
            "owned-session-started",
        );
        Ok(self.current_state())
    }

    pub fn renew_owned(
        &mut self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        self.expire_at(now);
        let lease = self.lease.as_mut().ok_or(CaptureError::NotActive)?;
        if lease.owner != *owner {
            return Err(CaptureError::NotOwner);
        }

        lease.soft_deadline = (now + CAPTURE_SOFT_LEASE).min(lease.absolute_deadline);
        Ok(self.current_state())
    }

    pub fn cancel_owned(
        &mut self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        self.expire_at(now);
        self.ensure_owner(owner)?;
        Ok(self.cancel())
    }

    pub fn finish_owned(
        &mut self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Result<LayoutSwitchCaptureState, CaptureError> {
        self.expire_at(now);
        self.ensure_owner(owner)?;
        Ok(self.finish())
    }

    pub fn owner_disappeared(
        &mut self,
        owner: &CaptureOwner,
        now: Instant,
    ) -> Option<LayoutSwitchCaptureState> {
        if let Some(expired) = self.expire_at(now) {
            return Some(expired);
        }

        let is_owner = self
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == *owner);
        is_owner.then(|| self.cancel())
    }

    pub fn expire_at(&mut self, now: Instant) -> Option<LayoutSwitchCaptureState> {
        let expired = self
            .lease
            .as_ref()
            .is_some_and(|lease| now >= lease.soft_deadline || now >= lease.absolute_deadline);
        expired.then(|| self.cancel())
    }

    fn ensure_owner(&self, owner: &CaptureOwner) -> Result<(), CaptureError> {
        let lease = self.lease.as_ref().ok_or(CaptureError::NotActive)?;
        if lease.owner != *owner {
            return Err(CaptureError::NotOwner);
        }
        Ok(())
    }

    pub fn handle_key_event(&mut self, key: Key, value: i32) -> Option<LayoutSwitchCaptureState> {
        if !self.is_active() {
            return None;
        }

        if value == 1 && key == Key::KEY_ESC {
            log_capture_debug(
                "event",
                Some(key),
                None,
                &self.progress,
                None,
                "escape-cancel",
            );
            return Some(self.cancel());
        }

        let Some(capture_key) = physical_capture_key_from_evdev(key) else {
            if value == 1 {
                self.progress.clear();
                log_capture_debug(
                    "event",
                    Some(key),
                    None,
                    &self.progress,
                    None,
                    "unresolved-evdev-key",
                );
                return self
                    .replace_state(LayoutSwitchCaptureState::unsupported(UNSUPPORTED_MESSAGE));
            }

            return None;
        };

        match value {
            1 | 2 => {
                if self.progress.is_empty() {
                    self.state = LayoutSwitchCaptureState::waiting();
                }
                self.progress.press_key(capture_key);
                let evaluated = self.progress.evaluate();
                log_capture_debug(
                    "event",
                    Some(key),
                    Some(capture_key),
                    &self.progress,
                    Some(evaluated),
                    if value == 2 {
                        "after-repeat"
                    } else {
                        "after-press"
                    },
                );
                match evaluated.evaluation {
                    CaptureEvaluation::Waiting => {
                        self.replace_state(LayoutSwitchCaptureState::waiting())
                    }
                    CaptureEvaluation::Candidate(combo) => {
                        self.replace_state(LayoutSwitchCaptureState::candidate(combo))
                    }
                    CaptureEvaluation::Unsupported => self
                        .replace_state(LayoutSwitchCaptureState::unsupported(UNSUPPORTED_MESSAGE)),
                }
            }
            0 => {
                self.progress.release_key(capture_key);
                let evaluated = self.progress.evaluate();
                log_capture_debug(
                    "event",
                    Some(key),
                    Some(capture_key),
                    &self.progress,
                    Some(evaluated),
                    "after-release",
                );
                None
            }
            _ => None,
        }
    }

    fn replace_state(
        &mut self,
        next: LayoutSwitchCaptureState,
    ) -> Option<LayoutSwitchCaptureState> {
        if self.state == next {
            return None;
        }

        if !next.is_active() {
            self.lease = None;
        }
        self.state = next;
        Some(self.current_state())
    }
}

fn capture_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(CAPTURE_DEBUG_ENV)
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false)
    })
}

fn append_capture_debug_line(line: &str) {
    let path = std::env::var(CAPTURE_DEBUG_FILE_ENV)
        .unwrap_or_else(|_| "/tmp/open-switcher-daemon-capture.log".to_string());

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn log_capture_debug(
    phase: &str,
    raw_key: Option<Key>,
    resolved_key: Option<PhysicalCaptureKey>,
    progress: &CaptureProgress,
    evaluated: Option<EvaluatedCapture>,
    note: &str,
) {
    if !capture_debug_enabled() {
        return;
    }

    let evaluation = evaluated.map(|value| value.evaluation);
    let reason = evaluated.map(|value| value.reason).unwrap_or("-");
    let line = format!(
        "[daemon-capture] phase={phase} raw_key={raw_key:?} resolved={resolved_key:?} pressed={:?} evaluation={evaluation:?} reason={reason} note={note}",
        progress.pressed_keys
    );
    eprintln!("{line}");
    append_capture_debug_line(&line);
}

fn physical_capture_key_from_evdev(key: Key) -> Option<PhysicalCaptureKey> {
    match key {
        Key::KEY_LEFTCTRL => Some(PhysicalCaptureKey::LeftCtrl),
        Key::KEY_RIGHTCTRL => Some(PhysicalCaptureKey::RightCtrl),
        Key::KEY_LEFTALT => Some(PhysicalCaptureKey::LeftAlt),
        Key::KEY_RIGHTALT => Some(PhysicalCaptureKey::RightAlt),
        Key::KEY_LEFTSHIFT => Some(PhysicalCaptureKey::LeftShift),
        Key::KEY_RIGHTSHIFT => Some(PhysicalCaptureKey::RightShift),
        Key::KEY_LEFTMETA | Key::KEY_RIGHTMETA => Some(PhysicalCaptureKey::Super),
        Key::KEY_SPACE => Some(PhysicalCaptureKey::Space),
        Key::KEY_CAPSLOCK => Some(PhysicalCaptureKey::CapsLock),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CaptureError;
    use std::time::{Duration, Instant};

    fn owner(name: &str) -> CaptureOwner {
        CaptureOwner::from(name)
    }

    #[test]
    fn different_owner_cannot_replace_live_capture() {
        let now = Instant::now();
        let mut session = LayoutSwitchCaptureSession::default();

        session.start_owned(owner(":1.10"), now).unwrap();

        assert!(matches!(
            session.start_owned(owner(":1.11"), now),
            Err(CaptureError::Busy)
        ));
        assert_eq!(session.current_state(), LayoutSwitchCaptureState::waiting());
    }

    #[test]
    fn same_owner_start_renews_without_clearing_candidate_or_progress() {
        let now = Instant::now();
        let capture_owner = owner(":1.10");
        let mut session = LayoutSwitchCaptureSession::default();
        session.start_owned(capture_owner.clone(), now).unwrap();
        session.handle_key_event(Key::KEY_LEFTCTRL, 1);
        session.handle_key_event(Key::KEY_LEFTSHIFT, 1);
        let candidate = session.current_state();
        let progress = session.progress.clone();

        let renewed = session
            .start_owned(capture_owner, now + Duration::from_secs(4))
            .unwrap();

        assert_eq!(renewed, candidate);
        assert_eq!(session.progress, progress);
        let lease = session
            .lease
            .as_ref()
            .expect("owner lease must remain active");
        assert_eq!(lease.soft_deadline, now + Duration::from_secs(14));
        assert_eq!(lease.absolute_deadline, now + CAPTURE_ABSOLUTE_LEASE);
    }

    #[test]
    fn non_owner_cannot_renew_cancel_or_finish() {
        let now = Instant::now();
        let capture_owner = owner(":1.10");
        let other_owner = owner(":1.11");
        let mut session = LayoutSwitchCaptureSession::default();
        session.start_owned(capture_owner, now).unwrap();
        session.handle_key_event(Key::KEY_LEFTCTRL, 1);
        let state = session.current_state();

        assert!(matches!(
            session.renew_owned(&other_owner, now),
            Err(CaptureError::NotOwner)
        ));
        assert!(matches!(
            session.cancel_owned(&other_owner, now),
            Err(CaptureError::NotOwner)
        ));
        assert!(matches!(
            session.finish_owned(&other_owner, now),
            Err(CaptureError::NotOwner)
        ));
        assert_eq!(session.current_state(), state);
    }

    #[test]
    fn owned_commands_reject_an_inactive_session() {
        let now = Instant::now();
        let capture_owner = owner(":1.10");
        let mut session = LayoutSwitchCaptureSession::default();

        assert!(matches!(
            session.renew_owned(&capture_owner, now),
            Err(CaptureError::NotActive)
        ));
        assert!(matches!(
            session.cancel_owned(&capture_owner, now),
            Err(CaptureError::NotActive)
        ));
        assert!(matches!(
            session.finish_owned(&capture_owner, now),
            Err(CaptureError::NotActive)
        ));
    }

    #[test]
    fn lease_expires_exactly_at_soft_deadline() {
        let now = Instant::now();
        let mut session = LayoutSwitchCaptureSession::default();
        session.start_owned(owner(":1.10"), now).unwrap();

        assert_eq!(
            session.expire_at(now + CAPTURE_SOFT_LEASE - Duration::from_nanos(1)),
            None
        );
        assert_eq!(
            session.expire_at(now + CAPTURE_SOFT_LEASE),
            Some(LayoutSwitchCaptureState::cancelled())
        );
        assert!(!session.is_active());
        assert!(session.lease.is_none());
    }

    #[test]
    fn absolute_deadline_is_not_extended_by_renew() {
        let now = Instant::now();
        let capture_owner = owner(":1.10");
        let mut session = LayoutSwitchCaptureSession::default();
        session.start_owned(capture_owner.clone(), now).unwrap();

        for seconds in [9, 18, 27, 36, 45, 54, 63, 64] {
            session
                .renew_owned(&capture_owner, now + Duration::from_secs(seconds))
                .unwrap();
        }

        let lease = session
            .lease
            .as_ref()
            .expect("owner lease must remain active");
        assert_eq!(lease.soft_deadline, now + CAPTURE_ABSOLUTE_LEASE);
        assert_eq!(lease.absolute_deadline, now + CAPTURE_ABSOLUTE_LEASE);
        assert_eq!(
            session.expire_at(now + CAPTURE_ABSOLUTE_LEASE - Duration::from_nanos(1)),
            None
        );
        assert_eq!(
            session.expire_at(now + CAPTURE_ABSOLUTE_LEASE),
            Some(LayoutSwitchCaptureState::cancelled())
        );
    }

    #[test]
    fn owner_loss_cancels_only_matching_owner() {
        let now = Instant::now();
        let capture_owner = owner(":1.10");
        let other_owner = owner(":1.11");
        let mut session = LayoutSwitchCaptureSession::default();
        session.start_owned(capture_owner.clone(), now).unwrap();

        assert_eq!(session.owner_disappeared(&other_owner, now), None);
        assert!(session.is_active());
        assert_eq!(
            session.owner_disappeared(&capture_owner, now),
            Some(LayoutSwitchCaptureState::cancelled())
        );
        assert!(!session.is_active());
        assert!(session.lease.is_none());
    }

    #[test]
    fn builds_side_specific_combos_on_press() {
        let mut session = LayoutSwitchCaptureSession::default();
        session.start();

        assert_eq!(session.handle_key_event(Key::KEY_LEFTCTRL, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_LEFTSHIFT, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::left_ctrl_left_shift()
            ))
        );

        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(session.handle_key_event(Key::KEY_RIGHTCTRL, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_RIGHTSHIFT, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::right_ctrl_right_shift()
            ))
        );

        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(session.handle_key_event(Key::KEY_LEFTALT, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_LEFTSHIFT, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::left_alt_left_shift()
            ))
        );

        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(session.handle_key_event(Key::KEY_RIGHTALT, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_RIGHTSHIFT, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::right_alt_right_shift()
            ))
        );
    }

    #[test]
    fn builds_generic_and_trigger_combos() {
        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(session.handle_key_event(Key::KEY_LEFTCTRL, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_RIGHTSHIFT, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::ctrl_shift()
            ))
        );

        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(session.handle_key_event(Key::KEY_RIGHTALT, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_LEFTSHIFT, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::alt_shift()
            ))
        );

        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(session.handle_key_event(Key::KEY_RIGHTCTRL, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_SPACE, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::ctrl_space()
            ))
        );
    }

    #[test]
    fn builds_super_space_trigger_combo() {
        let mut session = LayoutSwitchCaptureSession::default();
        session.start();

        assert_eq!(session.handle_key_event(Key::KEY_LEFTMETA, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_SPACE, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::super_space()
            ))
        );

        let mut session = LayoutSwitchCaptureSession::default();
        session.start();

        assert_eq!(session.handle_key_event(Key::KEY_RIGHTMETA, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_SPACE, 1),
            Some(LayoutSwitchCaptureState::candidate(
                LayoutSwitchCombo::super_space()
            ))
        );
    }

    #[test]
    fn rejects_unsupported_combos() {
        let mut session = LayoutSwitchCaptureSession::default();
        session.start();

        assert_eq!(session.handle_key_event(Key::KEY_RIGHTALT, 1), None);
        assert_eq!(
            session.handle_key_event(Key::KEY_LEFTCTRL, 1),
            Some(LayoutSwitchCaptureState::unsupported(UNSUPPORTED_MESSAGE))
        );
    }

    #[test]
    fn escape_cancels_active_capture() {
        let mut session = LayoutSwitchCaptureSession::default();
        session.start();
        assert_eq!(
            session.handle_key_event(Key::KEY_ESC, 1),
            Some(LayoutSwitchCaptureState::cancelled())
        );
        assert!(!session.is_active());
    }
}
