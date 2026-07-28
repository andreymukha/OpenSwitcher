use crate::daemon::synthetic_input::{
    InputGeneration, PendingTransfer, PhysicalReleaseCommit, PhysicalSequence,
    RestoredModifierTarget, SessionModifierLedger, SessionModifierState, SyntheticFailureLatch,
    SyntheticKeySink, TerminalProof,
};
use crate::error::SwitcherError;
use evdev::Key;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

const INPUT_EVENT_KEYBOARD: i32 = 0x01;
static NEXT_INPUT_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_input_generation() -> Result<InputGeneration, SwitcherError> {
    NEXT_INPUT_GENERATION
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            current.checked_add(1)
        })
        .map(InputGeneration)
        .map_err(|_| SwitcherError::input_safety("uinput generation exhausted"))
}

pub(crate) trait UinputRawSink {
    fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError>;
    fn synchronize_keys(&mut self) -> Result<(), SwitcherError>;
}

impl UinputRawSink for uinput::Device {
    fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
        self.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)
            .map_err(Into::into)
    }

    fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
        self.synchronize().map_err(Into::into)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UinputKeyToken {
    pub(crate) generation: InputGeneration,
    pub(crate) token_id: u64,
    pub(crate) key: Key,
}

pub(crate) struct UinputSyntheticSink<'a> {
    raw: &'a mut dyn UinputRawSink,
    generation: InputGeneration,
    next_token_id: &'a mut u64,
    session_modifiers: &'a RefCell<SessionModifierLedger<UinputKeyToken>>,
}

impl<'a> UinputSyntheticSink<'a> {
    pub(crate) fn new(
        raw: &'a mut dyn UinputRawSink,
        generation: InputGeneration,
        next_token_id: &'a mut u64,
        session_modifiers: &'a RefCell<SessionModifierLedger<UinputKeyToken>>,
    ) -> Self {
        Self {
            raw,
            generation,
            next_token_id,
            session_modifiers,
        }
    }
}

impl SyntheticKeySink for UinputSyntheticSink<'_> {
    type Token = UinputKeyToken;

    fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
        let token_id = *self.next_token_id;
        if token_id == 0 {
            return Err(SwitcherError::input_safety(
                "uinput token id must be nonzero",
            ));
        }
        let next_token_id = token_id
            .checked_add(1)
            .ok_or_else(|| SwitcherError::input_safety("uinput token id exhausted"))?;
        *self.next_token_id = next_token_id;
        Ok(UinputKeyToken {
            generation: self.generation,
            token_id,
            key,
        })
    }

    fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.session_modifiers
            .borrow()
            .authorize_synthetic_down(token.key)?;
        self.raw.write_key(token.key, 1)
    }

    fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.raw.write_key(token.key, 0)
    }

    fn synchronize(&mut self) -> Result<(), SwitcherError> {
        self.raw.synchronize_keys()
    }

    fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof {
        if remaining_debt == 0 {
            TerminalProof::Reconciled
        } else {
            TerminalProof::Unreconciled {
                remaining: remaining_debt,
            }
        }
    }
}

pub(crate) struct UinputSessionModifierTarget<'a> {
    ledger: &'a RefCell<SessionModifierLedger<UinputKeyToken>>,
}

impl UinputSessionModifierTarget<'_> {
    pub(crate) fn contains(&self, key: Key) -> bool {
        self.ledger.borrow().contains(key)
    }

    pub(crate) fn mark_temporarily_released(&mut self, key: Key) -> Result<(), SwitcherError> {
        self.ledger.borrow_mut().mark_temporarily_released(key)
    }

    pub(crate) fn release_only_snapshot(&self) -> Vec<UinputKeyToken> {
        self.ledger.borrow().release_only_snapshot()
    }

    pub(crate) fn state(&self, key: Key) -> Option<SessionModifierState> {
        self.ledger.borrow().state(key)
    }
}

impl RestoredModifierTarget<UinputKeyToken> for UinputSessionModifierTarget<'_> {
    fn begin_restore(&mut self, key: Key) -> Result<(), SwitcherError> {
        RestoredModifierTarget::begin_restore(&mut *self.ledger.borrow_mut(), key)
    }

    fn adopt_restored(
        &mut self,
        key: Key,
        transfer: PendingTransfer<UinputKeyToken>,
    ) -> Result<(), SwitcherError> {
        RestoredModifierTarget::adopt_restored(&mut *self.ledger.borrow_mut(), key, transfer)
    }
}

pub(crate) struct UinputSyntheticRuntime<R: UinputRawSink> {
    raw: R,
    generation: InputGeneration,
    next_token_id: u64,
    session_modifiers: RefCell<SessionModifierLedger<UinputKeyToken>>,
    failure_latch: SyntheticFailureLatch,
}

impl<R: UinputRawSink> UinputSyntheticRuntime<R> {
    pub(crate) fn new(raw: R, generation: InputGeneration) -> Result<Self, SwitcherError> {
        if generation.0 == 0 {
            return Err(SwitcherError::input_safety(
                "uinput generation must be nonzero",
            ));
        }
        Ok(Self {
            raw,
            generation,
            next_token_id: 1,
            session_modifiers: RefCell::new(SessionModifierLedger::new(generation)),
            failure_latch: SyntheticFailureLatch::default(),
        })
    }

    pub(crate) fn forward_physical(
        &mut self,
        sequence: PhysicalSequence,
        key: Key,
        value: i32,
    ) -> Result<Option<UinputKeyToken>, SwitcherError> {
        // EV_KEY value 2 is an autorepeat of an already-held key, not a new press.
        if value == 1 && self.session_modifiers.borrow().contains(key) {
            return Err(SwitcherError::input_safety(
                "physical press raced an owned synthetic modifier",
            ));
        }
        self.raw.write_key(key, value)?;
        self.raw.synchronize_keys()?;
        if value != 0 {
            return Ok(None);
        }
        self.session_modifiers
            .borrow_mut()
            .commit_physical_release(PhysicalReleaseCommit {
                generation: self.generation,
                sequence,
                key,
            })
    }

    pub(crate) fn operation_parts(
        &mut self,
    ) -> (
        UinputSyntheticSink<'_>,
        UinputSessionModifierTarget<'_>,
        SyntheticFailureLatch,
    ) {
        let Self {
            raw,
            generation,
            next_token_id,
            session_modifiers,
            failure_latch,
        } = self;
        let session_modifiers = &*session_modifiers;
        (
            UinputSyntheticSink::new(raw, *generation, next_token_id, session_modifiers),
            UinputSessionModifierTarget {
                ledger: session_modifiers,
            },
            failure_latch.clone(),
        )
    }

    pub(crate) fn destroy_generation(self) -> TerminalProof {
        let generation = self.generation.0;
        drop(self);
        TerminalProof::OwnerGenerationDestroyed { generation }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::synthetic_input::{
        FrozenPhysicalSnapshot, InputGeneration, OperationControl, OperationId, OperationOutcome,
        PendingTransfer, PhysicalSequence, RestoredModifierTarget, SyntheticFailureLatch,
        SyntheticOperation, TerminalProof,
    };
    use crate::error::SwitcherError;
    use evdev::Key;
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum UinputTrace {
        Write(Key, i32),
        Synchronize,
    }

    struct FakeUinputRawSink {
        trace: Arc<Mutex<Vec<UinputTrace>>>,
    }

    impl UinputRawSink for FakeUinputRawSink {
        fn write_key(&mut self, key: Key, value: i32) -> Result<(), SwitcherError> {
            self.trace
                .lock()
                .unwrap()
                .push(UinputTrace::Write(key, value));
            Ok(())
        }

        fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
            self.trace.lock().unwrap().push(UinputTrace::Synchronize);
            Ok(())
        }
    }

    struct NoRestoredModifiers;

    impl RestoredModifierTarget<UinputKeyToken> for NoRestoredModifiers {
        fn adopt_restored(
            &mut self,
            _key: Key,
            _transfer: PendingTransfer<UinputKeyToken>,
        ) -> Result<(), SwitcherError> {
            Err(SwitcherError::input_safety(
                "unexpected restored modifier in adapter test",
            ))
        }
    }

    #[test]
    fn uinput_adapter_keeps_down_until_write_and_sync_release_are_acknowledged() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let mut raw = FakeUinputRawSink {
            trace: trace.clone(),
        };
        let generation = InputGeneration(11);
        let mut next_token_id = 1;
        let session_modifiers = RefCell::new(SessionModifierLedger::new(generation));
        let mut sink =
            UinputSyntheticSink::new(&mut raw, generation, &mut next_token_id, &session_modifiers);
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(5),
            &mut sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            ),
            FrozenPhysicalSnapshot::default(),
            latch,
        );

        let press = operation.press(Key::KEY_A).unwrap();
        operation.release(press).unwrap();
        let report = operation.finish_success(&mut NoRestoredModifiers);

        assert_eq!(report.outcome, OperationOutcome::Success);
        assert_eq!(
            *trace.lock().unwrap(),
            [
                UinputTrace::Write(Key::KEY_A, 1),
                UinputTrace::Synchronize,
                UinputTrace::Write(Key::KEY_A, 0),
                UinputTrace::Synchronize,
                UinputTrace::Synchronize,
            ]
        );
    }

    struct DropAwareRawSink {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for DropAwareRawSink {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    impl UinputRawSink for DropAwareRawSink {
        fn write_key(&mut self, _key: Key, _value: i32) -> Result<(), SwitcherError> {
            Ok(())
        }

        fn synchronize_keys(&mut self) -> Result<(), SwitcherError> {
            Ok(())
        }
    }

    #[test]
    fn uinput_owner_generation_proof_is_published_only_after_owner_drop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = UinputSyntheticRuntime::new(
            DropAwareRawSink {
                dropped: dropped.clone(),
            },
            InputGeneration(12),
        )
        .unwrap();
        assert!(!dropped.load(std::sync::atomic::Ordering::Acquire));

        let proof = runtime.destroy_generation();

        assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            proof,
            TerminalProof::OwnerGenerationDestroyed { generation: 12 }
        );
    }

    #[test]
    fn physical_forwarding_writes_and_syncs_before_session_commit() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let raw = FakeUinputRawSink {
            trace: trace.clone(),
        };
        let mut runtime = UinputSyntheticRuntime::new(raw, InputGeneration(13)).unwrap();

        assert_eq!(
            runtime
                .forward_physical(PhysicalSequence(1), Key::KEY_A, 1)
                .unwrap(),
            None
        );
        assert_eq!(
            runtime
                .forward_physical(PhysicalSequence(2), Key::KEY_A, 0)
                .unwrap(),
            None
        );

        assert_eq!(
            *trace.lock().unwrap(),
            [
                UinputTrace::Write(Key::KEY_A, 1),
                UinputTrace::Synchronize,
                UinputTrace::Write(Key::KEY_A, 0),
                UinputTrace::Synchronize,
            ]
        );
    }

    #[test]
    fn runtime_keeps_token_ids_monotonic_across_operations() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let raw = FakeUinputRawSink { trace };
        let mut runtime = UinputSyntheticRuntime::new(raw, InputGeneration(14)).unwrap();

        let first = {
            let (mut sink, _, _) = runtime.operation_parts();
            sink.prepare_down(Key::KEY_A).unwrap()
        };
        let second = {
            let (mut sink, _, _) = runtime.operation_parts();
            sink.prepare_down(Key::KEY_B).unwrap()
        };

        assert_eq!(first.token_id, 1);
        assert_eq!(second.token_id, 2);
        assert_eq!(first.generation, InputGeneration(14));
        assert_eq!(second.generation, InputGeneration(14));
    }

    #[test]
    fn physical_release_commits_restored_session_token_after_write_and_sync() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let raw = FakeUinputRawSink {
            trace: trace.clone(),
        };
        let mut runtime = UinputSyntheticRuntime::new(raw, InputGeneration(15)).unwrap();
        {
            let (mut sink, mut session_modifiers, latch) = runtime.operation_parts();
            let mut operation = SyntheticOperation::new(
                OperationId(6),
                &mut sink,
                OperationControl::new(
                    Instant::now() + Duration::from_secs(1),
                    Arc::new(AtomicBool::new(false)),
                ),
                FrozenPhysicalSnapshot::new(0b0000_0100, false),
                latch,
            );
            operation
                .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
                .unwrap();
            let report = operation.finish_success(&mut session_modifiers);
            assert_eq!(report.outcome, OperationOutcome::Success);
        }

        let restored = runtime
            .forward_physical(PhysicalSequence(1), Key::KEY_LEFTSHIFT, 0)
            .unwrap()
            .expect("physical release must commit restored session debt");

        assert_eq!(restored.generation, InputGeneration(15));
        assert_eq!(restored.token_id, 2);
        assert_eq!(
            *trace.lock().unwrap(),
            [
                UinputTrace::Write(Key::KEY_LEFTSHIFT, 0),
                UinputTrace::Synchronize,
                UinputTrace::Write(Key::KEY_LEFTSHIFT, 1),
                UinputTrace::Synchronize,
                UinputTrace::Synchronize,
                UinputTrace::Write(Key::KEY_LEFTSHIFT, 0),
                UinputTrace::Synchronize,
            ]
        );
    }

    #[test]
    fn physical_repeat_is_forwarded_while_session_modifier_awaits_release_commit() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let raw = FakeUinputRawSink {
            trace: trace.clone(),
        };
        let mut runtime = UinputSyntheticRuntime::new(raw, InputGeneration(16)).unwrap();
        {
            let (mut sink, mut session_modifiers, latch) = runtime.operation_parts();
            let mut operation = SyntheticOperation::new(
                OperationId(7),
                &mut sink,
                OperationControl::new(
                    Instant::now() + Duration::from_secs(1),
                    Arc::new(AtomicBool::new(false)),
                ),
                FrozenPhysicalSnapshot::new(0b0000_0100, false),
                latch,
            );
            operation
                .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
                .unwrap();
            let report = operation.finish_success(&mut session_modifiers);
            assert_eq!(report.outcome, OperationOutcome::Success);
        }
        let baseline_len = trace.lock().unwrap().len();

        assert_eq!(
            runtime
                .forward_physical(PhysicalSequence(1), Key::KEY_LEFTSHIFT, 2)
                .unwrap(),
            None
        );
        let restored = runtime
            .forward_physical(PhysicalSequence(2), Key::KEY_LEFTSHIFT, 0)
            .unwrap()
            .expect("physical release must still commit after a repeat");

        assert_eq!(restored.generation, InputGeneration(16));
        assert_eq!(
            trace.lock().unwrap()[baseline_len..],
            [
                UinputTrace::Write(Key::KEY_LEFTSHIFT, 2),
                UinputTrace::Synchronize,
                UinputTrace::Write(Key::KEY_LEFTSHIFT, 0),
                UinputTrace::Synchronize,
            ]
        );
    }

    #[test]
    fn input_generation_allocator_is_monotonic_and_never_returns_zero() {
        let first = allocate_input_generation().unwrap();
        let second = allocate_input_generation().unwrap();

        assert_ne!(first, InputGeneration(0));
        assert!(second.0 > first.0);
    }
}
