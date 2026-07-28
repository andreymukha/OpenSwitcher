use crate::error::SwitcherError;
use evdev::Key;
use std::fmt::Debug;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OperationId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PressId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalProof {
    Reconciled,
    OwnerGenerationDestroyed { generation: u64 },
    Unreconciled { remaining: usize },
}

pub(crate) trait SyntheticKeySink {
    type Token: Clone + Debug + Eq;

    fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError>;
    fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError>;
    fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError>;
    fn synchronize(&mut self) -> Result<(), SwitcherError>;
    fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof;
}

pub(crate) struct PendingTransfer<T> {
    token: Option<T>,
    release_only_fallback: Arc<Mutex<Vec<T>>>,
}

impl<T> PendingTransfer<T> {
    fn new(token: T, release_only_fallback: Arc<Mutex<Vec<T>>>) -> Self {
        Self {
            token: Some(token),
            release_only_fallback,
        }
    }

    pub(crate) fn commit(
        mut self,
        adopt: impl FnOnce(&T) -> Result<(), SwitcherError>,
    ) -> Result<(), SwitcherError> {
        let token = self
            .token
            .as_ref()
            .ok_or(crate::error::InputSafetyError::Invariant {
                context: "pending transfer token is missing",
            })?;
        let result = adopt(token);
        if result.is_ok() {
            self.token.take();
        }
        result
    }
}

impl<T> Drop for PendingTransfer<T> {
    fn drop(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        self.release_only_fallback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(token);
    }
}

struct ReleaseOnlyFallbackDrain<T> {
    fallback: Arc<Mutex<Vec<T>>>,
    pending: Vec<T>,
    retained: Vec<T>,
}

impl<T> ReleaseOnlyFallbackDrain<T> {
    fn new(fallback: Arc<Mutex<Vec<T>>>) -> Self {
        let pending = {
            let mut slot = fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *slot)
        };
        Self {
            fallback,
            pending,
            retained: Vec::new(),
        }
    }
}

impl<T> Drop for ReleaseOnlyFallbackDrain<T> {
    fn drop(&mut self) {
        let mut fallback = self
            .fallback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fallback.append(&mut self.pending);
        fallback.append(&mut self.retained);
    }
}

pub(crate) trait RestoredModifierTarget<T> {
    fn adopt_restored(
        &mut self,
        key: Key,
        transfer: PendingTransfer<T>,
    ) -> Result<(), SwitcherError>;
}

#[derive(Debug)]
pub(crate) struct DropCleanupReport {
    pub(crate) cleanup: Option<SwitcherError>,
    pub(crate) proof: TerminalProof,
}

#[derive(Clone, Default)]
pub(crate) struct SyntheticFailureLatch(Arc<Mutex<Option<DropCleanupReport>>>);

impl SyntheticFailureLatch {
    pub(crate) fn fail(&self, report: DropCleanupReport) {
        let mut slot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(report);
        }
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub(crate) fn take_report(&self) -> Option<DropCleanupReport> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperationOutcome {
    Success,
    SoftCancelled,
    HardFailed,
}

#[derive(Debug)]
pub(crate) struct OperationTerminalReport {
    pub(crate) outcome: OperationOutcome,
    pub(crate) primary: Option<SwitcherError>,
    pub(crate) cleanup: Option<SwitcherError>,
    pub(crate) proof: TerminalProof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownState {
    AttemptingDown,
    PossiblyDown,
}

#[derive(Clone, Debug)]
struct SyntheticDebt<T> {
    press_id: PressId,
    token: T,
    state: DownState,
}

#[derive(Clone, Debug)]
pub(crate) struct SyntheticKeyLedger<T> {
    next_press_id: u64,
    debts: Vec<SyntheticDebt<T>>,
    terminal: bool,
}

impl<T: Clone + Eq> SyntheticKeyLedger<T> {
    pub(crate) fn new() -> Self {
        Self {
            next_press_id: 1,
            debts: Vec::new(),
            terminal: false,
        }
    }

    pub(crate) fn begin_down(&mut self, token: T) -> Result<PressId, SwitcherError> {
        if self.terminal {
            return Err(crate::error::InputSafetyError::Invariant {
                context: "mutation after terminal state",
            }
            .into());
        }

        let press_id = PressId(self.next_press_id);
        self.next_press_id =
            self.next_press_id
                .checked_add(1)
                .ok_or(crate::error::InputSafetyError::Invariant {
                    context: "press id exhausted",
                })?;
        self.debts.push(SyntheticDebt {
            press_id,
            token,
            state: DownState::AttemptingDown,
        });
        Ok(press_id)
    }

    pub(crate) fn mark_possibly_down(&mut self, press_id: PressId) -> Result<(), SwitcherError> {
        let debt = self.debts.iter_mut().find(|debt| debt.press_id == press_id);
        match debt {
            Some(debt) => {
                debt.state = DownState::PossiblyDown;
                Ok(())
            }
            None => Err(crate::error::InputSafetyError::Invariant {
                context: "press id does not belong to ledger",
            }
            .into()),
        }
    }

    pub(crate) fn acknowledge_up(&mut self, press_id: PressId) -> Result<(), SwitcherError> {
        let index = self
            .debts
            .iter()
            .position(|debt| debt.press_id == press_id)
            .ok_or(crate::error::InputSafetyError::Invariant {
                context: "release id does not belong to ledger",
            })?;
        self.debts.remove(index);
        Ok(())
    }

    pub(crate) fn transfer(&mut self, press_id: PressId) -> Result<T, SwitcherError> {
        if self.terminal {
            return Err(crate::error::InputSafetyError::Invariant {
                context: "transfer after terminal state",
            }
            .into());
        }
        let index = self
            .debts
            .iter()
            .position(|debt| debt.press_id == press_id)
            .ok_or(crate::error::InputSafetyError::Invariant {
                context: "transfer id does not belong to ledger",
            })?;
        if self.debts[index].state != DownState::PossiblyDown {
            return Err(crate::error::InputSafetyError::Invariant {
                context: "only a possibly-down token can be transferred",
            }
            .into());
        }
        Ok(self.debts.remove(index).token)
    }

    pub(crate) fn begin_terminal(&mut self) {
        self.terminal = true;
    }

    fn token(&self, press_id: PressId) -> Result<T, SwitcherError> {
        self.debts
            .iter()
            .find(|debt| debt.press_id == press_id)
            .map(|debt| debt.token.clone())
            .ok_or_else(|| {
                crate::error::InputSafetyError::Invariant {
                    context: "release id does not belong to ledger",
                }
                .into()
            })
    }

    fn reverse_snapshot(&self) -> Vec<SyntheticDebt<T>> {
        self.debts.iter().rev().cloned().collect()
    }

    fn len(&self) -> usize {
        self.debts.len()
    }
}

pub(crate) struct OperationControl {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl OperationControl {
    pub(crate) fn new(deadline: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            deadline,
            cancelled,
        }
    }

    fn ensure_active(&self, operation_id: OperationId) -> Result<(), SwitcherError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(crate::error::InputSafetyError::OperationCancelled {
                operation_id: operation_id.0,
            }
            .into());
        }
        if Instant::now() >= self.deadline {
            return Err(crate::error::InputSafetyError::OperationTimedOut {
                operation_id: operation_id.0,
            }
            .into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrozenPhysicalSnapshot {
    modifier_bits: u16,
    caps_lock_active: bool,
}

impl FrozenPhysicalSnapshot {
    pub(crate) fn new(modifier_bits: u16, caps_lock_active: bool) -> Self {
        Self {
            modifier_bits,
            caps_lock_active,
        }
    }

    fn holds_modifier(self, key: Key) -> bool {
        modifier_bit(key)
            .map(|bit| self.modifier_bits & bit != 0)
            .unwrap_or(false)
    }

    pub(crate) fn caps_lock_active(self) -> bool {
        self.caps_lock_active
    }
}

fn modifier_bit(key: Key) -> Option<u16> {
    match key {
        Key::KEY_LEFTCTRL => Some(1 << 0),
        Key::KEY_RIGHTCTRL => Some(1 << 1),
        Key::KEY_LEFTSHIFT => Some(1 << 2),
        Key::KEY_RIGHTSHIFT => Some(1 << 3),
        Key::KEY_LEFTALT => Some(1 << 4),
        Key::KEY_RIGHTALT => Some(1 << 5),
        Key::KEY_LEFTMETA => Some(1 << 6),
        Key::KEY_RIGHTMETA => Some(1 << 7),
        _ => None,
    }
}

#[derive(Default)]
pub(crate) struct PhysicalRestorePlan {
    temporarily_released: Vec<Key>,
}

impl PhysicalRestorePlan {
    fn record(
        &mut self,
        operation_id: OperationId,
        snapshot: FrozenPhysicalSnapshot,
        key: Key,
    ) -> Result<(), SwitcherError> {
        if !snapshot.holds_modifier(key) {
            return Err(crate::error::InputSafetyError::ProtocolViolation {
                operation_id: operation_id.0,
                context: "temporary release requires a frozen held modifier",
            }
            .into());
        }
        if self.temporarily_released.contains(&key) {
            return Err(crate::error::InputSafetyError::ProtocolViolation {
                operation_id: operation_id.0,
                context: "modifier was already temporarily released",
            }
            .into());
        }
        self.temporarily_released.push(key);
        Ok(())
    }

    fn snapshot(&self) -> Vec<Key> {
        self.temporarily_released.clone()
    }
}

pub(crate) struct SyntheticOperation<'a, S: SyntheticKeySink> {
    id: OperationId,
    sink: &'a mut S,
    ledger: SyntheticKeyLedger<S::Token>,
    control: OperationControl,
    frozen_physical: FrozenPhysicalSnapshot,
    restore_plan: PhysicalRestorePlan,
    release_only_fallback: Arc<Mutex<Vec<S::Token>>>,
    failure_latch: SyntheticFailureLatch,
    finalized: bool,
}

impl<'a, S: SyntheticKeySink> SyntheticOperation<'a, S> {
    pub(crate) fn new(
        id: OperationId,
        sink: &'a mut S,
        control: OperationControl,
        frozen_physical: FrozenPhysicalSnapshot,
        failure_latch: SyntheticFailureLatch,
    ) -> Self {
        Self {
            id,
            sink,
            ledger: SyntheticKeyLedger::new(),
            control,
            frozen_physical,
            restore_plan: PhysicalRestorePlan::default(),
            release_only_fallback: Arc::new(Mutex::new(Vec::new())),
            failure_latch,
            finalized: false,
        }
    }

    pub(crate) fn press(&mut self, key: Key) -> Result<PressId, SwitcherError> {
        self.control.ensure_active(self.id)?;
        let token = self.sink.prepare_down(key)?;
        self.control.ensure_active(self.id)?;
        let press_id = self.ledger.begin_down(token.clone())?;

        let down_result = self.sink.attempt_down(&token);
        self.ledger.mark_possibly_down(press_id)?;
        down_result?;
        self.control.ensure_active(self.id)?;
        self.sink.synchronize()?;
        self.control.ensure_active(self.id)?;
        Ok(press_id)
    }

    pub(crate) fn temporarily_release_physical_modifier(
        &mut self,
        key: Key,
    ) -> Result<(), SwitcherError> {
        self.control.ensure_active(self.id)?;
        let token = self.sink.prepare_down(key)?;
        self.control.ensure_active(self.id)?;
        self.restore_plan
            .record(self.id, self.frozen_physical, key)?;

        let release_result = self.sink.attempt_up(&token);
        release_result?;
        self.control.ensure_active(self.id)?;
        self.sink.synchronize()?;
        self.control.ensure_active(self.id)
    }

    pub(crate) fn release(&mut self, press_id: PressId) -> Result<(), SwitcherError> {
        let token = self.ledger.token(press_id)?;
        let mut control_error = self.control.ensure_active(self.id).err();
        let release_result = self.sink.attempt_up(&token);
        if control_error.is_none() {
            control_error = self.control.ensure_active(self.id).err();
        }
        let synchronize_result = self.sink.synchronize();
        if control_error.is_none() {
            control_error = self.control.ensure_active(self.id).err();
        }
        if release_result.is_ok() && synchronize_result.is_ok() {
            self.ledger.acknowledge_up(press_id)?;
        }
        release_result?;
        synchronize_result?;
        match control_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) fn finish_hard_failure(mut self, primary: SwitcherError) -> OperationTerminalReport {
        self.ledger.begin_terminal();
        let mut cleanup = self.release_all().err();
        Self::keep_first_error(&mut cleanup, self.drain_release_only_fallback());
        Self::keep_first_error(&mut cleanup, self.sink.synchronize());
        let proof = self.sink.terminal_proof(self.remaining_debt());
        if cleanup.is_some() || matches!(proof, TerminalProof::Unreconciled { .. }) {
            self.record_reconciliation_failure(proof.clone());
        }
        self.finalized = true;
        OperationTerminalReport {
            outcome: OperationOutcome::HardFailed,
            primary: Some(primary),
            cleanup,
            proof,
        }
    }

    pub(crate) fn finish_success<R>(mut self, _restored: &mut R) -> OperationTerminalReport
    where
        R: RestoredModifierTarget<S::Token>,
    {
        self.finish_with_restore(OperationOutcome::Success, _restored)
    }

    pub(crate) fn finish_soft_cancel<R>(mut self, _restored: &mut R) -> OperationTerminalReport
    where
        R: RestoredModifierTarget<S::Token>,
    {
        self.finish_with_restore(OperationOutcome::SoftCancelled, _restored)
    }

    fn finish_with_restore<R>(
        &mut self,
        requested_outcome: OperationOutcome,
        restored: &mut R,
    ) -> OperationTerminalReport
    where
        R: RestoredModifierTarget<S::Token>,
    {
        let mut cleanup = self.restore_planned_modifiers(restored);
        self.ledger.begin_terminal();
        Self::keep_first_error(&mut cleanup, self.release_all());
        Self::keep_first_error(&mut cleanup, self.drain_release_only_fallback());
        Self::keep_first_error(&mut cleanup, self.sink.synchronize());
        let proof = self.sink.terminal_proof(self.remaining_debt());
        let clean = cleanup.is_none() && proof == TerminalProof::Reconciled;
        if !clean {
            self.record_reconciliation_failure(proof.clone());
        }
        self.finalized = true;
        OperationTerminalReport {
            outcome: if clean {
                requested_outcome
            } else {
                OperationOutcome::HardFailed
            },
            primary: None,
            cleanup,
            proof,
        }
    }

    fn restore_planned_modifiers<R>(&mut self, restored: &mut R) -> Option<SwitcherError>
    where
        R: RestoredModifierTarget<S::Token>,
    {
        for key in self.restore_plan.snapshot() {
            if let Err(error) = self.restore_one_modifier(key, restored) {
                return Some(error);
            }
        }
        None
    }

    fn restore_one_modifier<R>(&mut self, key: Key, restored: &mut R) -> Result<(), SwitcherError>
    where
        R: RestoredModifierTarget<S::Token>,
    {
        self.control.ensure_active(self.id)?;
        let token = self.sink.prepare_down(key)?;
        self.control.ensure_active(self.id)?;
        let press_id = self.ledger.begin_down(token.clone())?;

        let down_result = self.sink.attempt_down(&token);
        self.ledger.mark_possibly_down(press_id)?;
        down_result?;
        self.control.ensure_active(self.id)?;
        self.sink.synchronize()?;
        self.control.ensure_active(self.id)?;

        let token = self.ledger.transfer(press_id)?;
        restored.adopt_restored(
            key,
            PendingTransfer::new(token, self.release_only_fallback.clone()),
        )
    }

    fn record_reconciliation_failure(&self, proof: TerminalProof) {
        self.failure_latch.fail(DropCleanupReport {
            cleanup: Some(
                crate::error::InputSafetyError::Reconciliation {
                    operation_id: self.id.0,
                    remaining: self.remaining_debt(),
                }
                .into(),
            ),
            proof,
        });
    }

    fn release_all(&mut self) -> Result<(), SwitcherError> {
        let mut first_error = None;
        for debt in self.ledger.reverse_snapshot() {
            let released = match self.sink.attempt_up(&debt.token) {
                Ok(()) => true,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    false
                }
            };
            let synchronized = match self.sink.synchronize() {
                Ok(()) => true,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    false
                }
            };
            if released && synchronized {
                let _ = self.ledger.acknowledge_up(debt.press_id);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn drain_release_only_fallback(&mut self) -> Result<(), SwitcherError> {
        let mut drain = ReleaseOnlyFallbackDrain::new(self.release_only_fallback.clone());
        let mut first_error = None;
        while let Some(token) = drain.pending.last() {
            let released = match self.sink.attempt_up(token) {
                Ok(()) => true,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    false
                }
            };
            let synchronized = match self.sink.synchronize() {
                Ok(()) => true,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    false
                }
            };
            if !(released && synchronized) {
                let Some(token) = drain.pending.pop() else {
                    return Err(SwitcherError::input_safety(
                        "fallback token disappeared during cleanup",
                    ));
                };
                drain.retained.push(token);
            } else {
                if drain.pending.pop().is_none() {
                    return Err(SwitcherError::input_safety(
                        "fallback token disappeared during cleanup",
                    ));
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn remaining_debt(&self) -> usize {
        self.ledger.len()
            + self
                .release_only_fallback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
    }

    fn keep_first_error(
        first_error: &mut Option<SwitcherError>,
        result: Result<(), SwitcherError>,
    ) {
        if let Err(error) = result {
            if first_error.is_none() {
                *first_error = Some(error);
            }
        }
    }
}

impl<S: SyntheticKeySink> Drop for SyntheticOperation<'_, S> {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }

        self.ledger.begin_terminal();
        let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut cleanup = self.release_all().err();
            Self::keep_first_error(&mut cleanup, self.drain_release_only_fallback());
            Self::keep_first_error(&mut cleanup, self.sink.synchronize());
            let proof = self.sink.terminal_proof(self.remaining_debt());
            (cleanup, proof)
        }));
        let report = match cleanup {
            Ok((cleanup, proof)) => DropCleanupReport { cleanup, proof },
            Err(_) => {
                let remaining = self.remaining_debt();
                DropCleanupReport {
                    cleanup: Some(
                        crate::error::InputSafetyError::Reconciliation {
                            operation_id: self.id.0,
                            remaining,
                        }
                        .into(),
                    ),
                    proof: TerminalProof::Unreconciled { remaining },
                }
            }
        };
        self.failure_latch.fail(report);
        self.finalized = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{InputSafetyError, SwitcherError};
    use evdev::Key;
    use std::cell::{Cell, RefCell};
    use std::io;
    use std::sync::{atomic::AtomicBool, Arc};
    use std::time::{Duration, Instant};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeToken {
        key: Key,
    }

    #[derive(Default)]
    struct FakeRestoredModifierTarget {
        adopted: Vec<(Key, FakeToken)>,
        reject: bool,
    }

    impl RestoredModifierTarget<FakeToken> for FakeRestoredModifierTarget {
        fn adopt_restored(
            &mut self,
            key: Key,
            transfer: PendingTransfer<FakeToken>,
        ) -> Result<(), SwitcherError> {
            transfer.commit(|token| {
                if self.reject {
                    return Err(SwitcherError::Io(io::Error::other(
                        "restored modifier target rejected transfer",
                    )));
                }
                self.adopted.push((key, token.clone()));
                Ok(())
            })
        }
    }

    struct PanickingRestoredModifierTarget;

    impl RestoredModifierTarget<FakeToken> for PanickingRestoredModifierTarget {
        fn adopt_restored(
            &mut self,
            _key: Key,
            transfer: PendingTransfer<FakeToken>,
        ) -> Result<(), SwitcherError> {
            transfer.commit(|_| panic!("fake restored target panic"))
        }
    }

    #[derive(Default)]
    struct FakeSink {
        down_count: usize,
        up_count: usize,
        fail_down_after_apply: bool,
        fail_up_for: Option<Key>,
        fail_sync_on_call: Option<usize>,
        sync_calls: usize,
        release_order: Vec<Key>,
        cancel_after_prepare: Option<Arc<AtomicBool>>,
        cancel_after_down: Option<Arc<AtomicBool>>,
        cancel_after_up: Option<Arc<AtomicBool>>,
        cancel_after_sync_on_call: Option<(usize, Arc<AtomicBool>)>,
        delay_after_prepare: Option<Duration>,
        delay_after_down: Option<Duration>,
        delay_after_up: Option<Duration>,
        delay_after_sync_on_call: Option<(usize, Duration)>,
        panic_on_up: bool,
        panic_on_up_call: Option<usize>,
    }

    impl FakeSink {
        fn fail_down_after_apply() -> Self {
            Self {
                fail_down_after_apply: true,
                ..Self::default()
            }
        }

        fn fail_cleanup_for(key: Key) -> Self {
            Self {
                fail_up_for: Some(key),
                ..Self::default()
            }
        }
    }

    fn test_operation<'a>(
        id: u64,
        sink: &'a mut FakeSink,
        latch: SyntheticFailureLatch,
    ) -> SyntheticOperation<'a, FakeSink> {
        SyntheticOperation::new(
            OperationId(id),
            sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(60),
                Arc::new(AtomicBool::new(false)),
            ),
            FrozenPhysicalSnapshot::default(),
            latch,
        )
    }

    impl SyntheticKeySink for FakeSink {
        type Token = FakeToken;

        fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
            if let Some(cancelled) = &self.cancel_after_prepare {
                cancelled.store(true, Ordering::Release);
            }
            if let Some(delay) = self.delay_after_prepare {
                std::thread::sleep(delay);
            }
            Ok(FakeToken { key })
        }

        fn attempt_down(&mut self, _token: &Self::Token) -> Result<(), SwitcherError> {
            self.down_count += 1;
            if let Some(cancelled) = &self.cancel_after_down {
                cancelled.store(true, Ordering::Release);
            }
            if let Some(delay) = self.delay_after_down {
                std::thread::sleep(delay);
            }
            if self.fail_down_after_apply {
                Err(SwitcherError::Io(io::Error::other(
                    "down failed after mutation",
                )))
            } else {
                Ok(())
            }
        }

        fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
            if self.panic_on_up {
                panic!("fake backend panic during cleanup");
            }
            self.up_count += 1;
            if self.panic_on_up_call == Some(self.up_count) {
                self.panic_on_up_call = None;
                panic!("fake one-shot backend panic during cleanup");
            }
            self.release_order.push(token.key);
            if let Some(cancelled) = &self.cancel_after_up {
                cancelled.store(true, Ordering::Release);
            }
            if let Some(delay) = self.delay_after_up {
                std::thread::sleep(delay);
            }
            if self.fail_up_for == Some(token.key) {
                Err(SwitcherError::Io(io::Error::other("cleanup up failed")))
            } else {
                Ok(())
            }
        }

        fn synchronize(&mut self) -> Result<(), SwitcherError> {
            self.sync_calls += 1;
            if let Some((call, cancelled)) = &self.cancel_after_sync_on_call {
                if *call == self.sync_calls {
                    cancelled.store(true, Ordering::Release);
                }
            }
            if let Some((call, delay)) = self.delay_after_sync_on_call {
                if call == self.sync_calls {
                    std::thread::sleep(delay);
                }
            }
            if self.fail_sync_on_call == Some(self.sync_calls) {
                Err(SwitcherError::Io(io::Error::other("synchronize failed")))
            } else {
                Ok(())
            }
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

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FailAt {
        Prepare,
        DownBeforeApply,
        DownAfterApply,
        DownSync,
        UpBeforeApply,
        UpAfterApply,
        UpSync,
        CleanupUp,
        CleanupSync,
    }

    #[derive(Clone, Copy)]
    enum MatrixMutation {
        Down,
        Up,
    }

    struct MatrixSink {
        fail_at: FailAt,
        failed_once: bool,
        persistent_cleanup_up_failures: usize,
        down_attempts: Vec<Key>,
        up_attempts: Vec<Key>,
        possibly_down: Vec<Key>,
        pending_up: Option<Key>,
        last_mutation: Option<MatrixMutation>,
        terminal: Cell<bool>,
        down_after_terminal: bool,
    }

    impl MatrixSink {
        fn new(fail_at: FailAt) -> Self {
            Self {
                fail_at,
                failed_once: false,
                persistent_cleanup_up_failures: 0,
                down_attempts: Vec::new(),
                up_attempts: Vec::new(),
                possibly_down: Vec::new(),
                pending_up: None,
                last_mutation: None,
                terminal: Cell::new(false),
                down_after_terminal: false,
            }
        }

        fn with_cleanup_up_failures(failures: usize) -> Self {
            let mut sink = Self::new(FailAt::CleanupUp);
            sink.persistent_cleanup_up_failures = failures;
            sink
        }

        fn fail_once(&mut self, at: FailAt) -> bool {
            if !self.failed_once && self.fail_at == at {
                self.failed_once = true;
                true
            } else {
                false
            }
        }

        fn remember_down(&mut self, key: Key) {
            if !self.possibly_down.contains(&key) {
                self.possibly_down.push(key);
            }
        }

        fn down_count(&self, key: Key) -> usize {
            self.down_attempts
                .iter()
                .filter(|candidate| **candidate == key)
                .count()
        }

        fn no_down_after_terminal(&self) -> bool {
            !self.down_after_terminal
        }
    }

    impl SyntheticKeySink for MatrixSink {
        type Token = FakeToken;

        fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
            if self.fail_once(FailAt::Prepare) {
                Err(SwitcherError::Io(io::Error::other("prepare failed")))
            } else {
                Ok(FakeToken { key })
            }
        }

        fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
            if self.terminal.get() {
                self.down_after_terminal = true;
            }
            self.down_attempts.push(token.key);
            self.last_mutation = Some(MatrixMutation::Down);
            if self.fail_once(FailAt::DownBeforeApply) {
                return Err(SwitcherError::Io(io::Error::other(
                    "down failed before apply",
                )));
            }
            self.remember_down(token.key);
            if self.fail_once(FailAt::DownAfterApply) {
                Err(SwitcherError::Io(io::Error::other(
                    "down failed after apply",
                )))
            } else {
                Ok(())
            }
        }

        fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
            self.up_attempts.push(token.key);
            self.last_mutation = Some(MatrixMutation::Up);
            if self.persistent_cleanup_up_failures > 0 {
                self.persistent_cleanup_up_failures -= 1;
                return Err(SwitcherError::Io(io::Error::other("cleanup up failed")));
            }
            if self.fail_once(FailAt::UpBeforeApply) || self.fail_once(FailAt::CleanupUp) {
                return Err(SwitcherError::Io(io::Error::other(
                    "up failed before apply",
                )));
            }
            self.pending_up = Some(token.key);
            if self.fail_once(FailAt::UpAfterApply) {
                Err(SwitcherError::Io(io::Error::other("up failed after apply")))
            } else {
                Ok(())
            }
        }

        fn synchronize(&mut self) -> Result<(), SwitcherError> {
            let fail = match self.last_mutation {
                Some(MatrixMutation::Down) => self.fail_once(FailAt::DownSync),
                Some(MatrixMutation::Up) => {
                    self.fail_once(FailAt::UpSync) || self.fail_once(FailAt::CleanupSync)
                }
                None => false,
            };
            if fail {
                self.pending_up = None;
                self.last_mutation = None;
                return Err(SwitcherError::Io(io::Error::other("synchronize failed")));
            }
            if let Some(key) = self.pending_up.take() {
                self.possibly_down.retain(|candidate| *candidate != key);
            }
            self.last_mutation = None;
            Ok(())
        }

        fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof {
            self.terminal.set(true);
            if remaining_debt == 0 && self.possibly_down.is_empty() {
                TerminalProof::Reconciled
            } else {
                TerminalProof::Unreconciled {
                    remaining: remaining_debt.max(self.possibly_down.len()),
                }
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ThirdBackendToken {
        generation: u64,
        ordinal: u32,
        logical_key: Key,
    }

    struct FakeThirdBackend {
        generation: u64,
        next_ordinal: u32,
        down_attempts: usize,
        owner_state: RefCell<Vec<Key>>,
    }

    impl FakeThirdBackend {
        fn new(generation: u64) -> Self {
            Self {
                generation,
                next_ordinal: 1,
                down_attempts: 0,
                owner_state: RefCell::new(Vec::new()),
            }
        }
    }

    impl SyntheticKeySink for FakeThirdBackend {
        type Token = ThirdBackendToken;

        fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
            let token = ThirdBackendToken {
                generation: self.generation,
                ordinal: self.next_ordinal,
                logical_key: key,
            };
            self.next_ordinal += 1;
            Ok(token)
        }

        fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
            self.down_attempts += 1;
            self.owner_state.borrow_mut().push(token.logical_key);
            Ok(())
        }

        fn attempt_up(&mut self, _token: &Self::Token) -> Result<(), SwitcherError> {
            Err(SwitcherError::Io(io::Error::other(
                "owner-scoped backend lost release acknowledgement",
            )))
        }

        fn synchronize(&mut self) -> Result<(), SwitcherError> {
            Ok(())
        }

        fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof {
            if remaining_debt == 0 {
                TerminalProof::Reconciled
            } else {
                self.owner_state.borrow_mut().clear();
                TerminalProof::OwnerGenerationDestroyed {
                    generation: self.generation,
                }
            }
        }
    }

    fn run_synthetic_sink_conformance<S: SyntheticKeySink>(
        sink: &mut S,
    ) -> OperationTerminalReport {
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(900),
            sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            ),
            FrozenPhysicalSnapshot::default(),
            latch,
        );
        operation.press(Key::KEY_A).unwrap();
        operation.finish_hard_failure(SwitcherError::Io(io::Error::other("conformance primary")))
    }

    #[test]
    fn ambiguous_down_is_recorded_before_backend_call_and_never_repeated() {
        let mut sink = FakeSink::fail_down_after_apply();
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(41, &mut sink, latch.clone());

        let primary = operation.press(Key::KEY_A).unwrap_err();
        let proof = operation.finish_hard_failure(primary).proof;

        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 1);
        assert_eq!(proof, TerminalProof::Reconciled);
        assert!(!latch.is_failed());
    }

    #[test]
    fn cleanup_continues_after_first_release_error() {
        let mut sink = FakeSink::fail_cleanup_for(Key::KEY_B);
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(42, &mut sink, latch.clone());

        operation.press(Key::KEY_A).unwrap();
        operation.press(Key::KEY_B).unwrap();
        let result =
            operation.finish_hard_failure(SwitcherError::Io(io::Error::other("primary failure")));

        assert_eq!(sink.release_order, [Key::KEY_B, Key::KEY_A]);
        assert_eq!(result.proof, TerminalProof::Unreconciled { remaining: 1 });
        assert!(latch.is_failed());
    }

    #[test]
    fn terminal_ledger_rejects_mutation_with_typed_error() {
        let mut ledger = SyntheticKeyLedger::new();
        ledger.begin_terminal();

        let error = ledger
            .begin_down(FakeToken { key: Key::KEY_A })
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputSafety(InputSafetyError::Invariant {
                context: "mutation after terminal state",
            })
        ));
    }

    #[test]
    fn release_debt_is_removed_only_after_up_and_sync_succeed() {
        let mut sink = FakeSink {
            fail_sync_on_call: Some(2),
            ..FakeSink::default()
        };
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(43, &mut sink, latch.clone());

        let press_id = operation.press(Key::KEY_A).unwrap();
        let primary = operation.release(press_id).unwrap_err();
        let report = operation.finish_hard_failure(primary);

        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 2);
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(!latch.is_failed());
    }

    #[test]
    fn cancellation_before_prepare_forbids_backend_mutation() {
        let mut sink = FakeSink::default();
        let cancelled = Arc::new(AtomicBool::new(true));
        let control = OperationControl::new(Instant::now() + Duration::from_secs(1), cancelled);
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(44),
            &mut sink,
            control,
            FrozenPhysicalSnapshot::default(),
            latch,
        );

        let error = operation.press(Key::KEY_A).unwrap_err();

        assert!(matches!(
            &error,
            SwitcherError::InputSafety(InputSafetyError::OperationCancelled { operation_id: 44 })
        ));
        let _ = operation.finish_hard_failure(error);
        assert_eq!(sink.down_count, 0);
    }

    #[test]
    fn expired_deadline_before_prepare_forbids_backend_mutation() {
        let mut sink = FakeSink::default();
        let control = OperationControl::new(
            Instant::now() - Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        );
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(45),
            &mut sink,
            control,
            FrozenPhysicalSnapshot::default(),
            latch,
        );

        let error = operation.press(Key::KEY_A).unwrap_err();

        assert!(matches!(
            &error,
            SwitcherError::InputSafety(InputSafetyError::OperationTimedOut { operation_id: 45 })
        ));
        let _ = operation.finish_hard_failure(error);
        assert_eq!(sink.down_count, 0);
    }

    #[test]
    fn cancellation_after_down_forbids_the_next_backend_call() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut sink = FakeSink {
            cancel_after_down: Some(cancelled.clone()),
            ..FakeSink::default()
        };
        let control = OperationControl::new(Instant::now() + Duration::from_secs(1), cancelled);
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(46),
            &mut sink,
            control,
            FrozenPhysicalSnapshot::default(),
            latch,
        );

        let primary = operation.press(Key::KEY_A).unwrap_err();
        assert!(matches!(
            primary,
            SwitcherError::InputSafety(InputSafetyError::OperationCancelled { operation_id: 46 })
        ));
        let report = operation.finish_hard_failure(primary);

        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 1);
        assert_eq!(sink.sync_calls, 2);
        assert_eq!(report.proof, TerminalProof::Reconciled);
    }

    #[test]
    fn hard_failure_report_keeps_primary_and_cleanup_separate() {
        let mut sink = FakeSink::fail_cleanup_for(Key::KEY_A);
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(47, &mut sink, latch.clone());
        operation.press(Key::KEY_A).unwrap();

        let report =
            operation.finish_hard_failure(SwitcherError::Io(io::Error::other("primary failure")));

        assert_eq!(report.outcome, OperationOutcome::HardFailed);
        assert!(matches!(report.primary, Some(SwitcherError::Io(ref error))
            if error.to_string() == "primary failure"));
        assert!(matches!(report.cleanup, Some(SwitcherError::Io(ref error))
            if error.to_string() == "cleanup up failed"));
        assert_eq!(report.proof, TerminalProof::Unreconciled { remaining: 1 });
        assert!(latch.is_failed());
    }

    #[test]
    fn successful_finalizer_reconciles_before_publishing_success() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(48, &mut sink, latch.clone());
        operation.press(Key::KEY_A).unwrap();
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_success(&mut restored);

        assert_eq!(sink.up_count, 1);
        assert_eq!(report.outcome, OperationOutcome::Success);
        assert!(report.primary.is_none());
        assert!(report.cleanup.is_none());
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(!latch.is_failed());
    }

    #[test]
    fn soft_cancel_is_published_only_after_reconciliation() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(49, &mut sink, latch.clone());
        operation.press(Key::KEY_A).unwrap();
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_soft_cancel(&mut restored);

        assert_eq!(report.outcome, OperationOutcome::SoftCancelled);
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(!latch.is_failed());
    }

    #[test]
    fn cleanup_failure_downgrades_success_to_hard_failure() {
        let mut sink = FakeSink::fail_cleanup_for(Key::KEY_A);
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(50, &mut sink, latch.clone());
        operation.press(Key::KEY_A).unwrap();
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_success(&mut restored);

        assert_eq!(report.outcome, OperationOutcome::HardFailed);
        assert!(report.primary.is_none());
        assert!(report.cleanup.is_some());
        assert_eq!(report.proof, TerminalProof::Unreconciled { remaining: 1 });
        assert!(latch.is_failed());
    }

    #[test]
    fn dropping_unfinished_operation_releases_debt_and_records_report() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        {
            let mut operation = test_operation(51, &mut sink, latch.clone());
            operation.press(Key::KEY_A).unwrap();
        }

        let report = latch
            .take_report()
            .expect("unfinished operation must be reported");
        assert_eq!(sink.up_count, 1);
        assert!(report.cleanup.is_none());
        assert_eq!(report.proof, TerminalProof::Reconciled);
    }

    #[test]
    fn drop_cleanup_never_propagates_backend_panic() {
        let mut sink = FakeSink {
            panic_on_up: true,
            ..FakeSink::default()
        };
        let latch = SyntheticFailureLatch::default();

        let drop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut operation = test_operation(52, &mut sink, latch.clone());
            operation.press(Key::KEY_A).unwrap();
        }));

        assert!(drop_result.is_ok());
        let report = latch
            .take_report()
            .expect("panicking cleanup must be reported");
        assert!(matches!(
            report.cleanup,
            Some(SwitcherError::InputSafety(
                InputSafetyError::Reconciliation {
                    operation_id: 52,
                    remaining: 1,
                }
            ))
        ));
        assert_eq!(report.proof, TerminalProof::Unreconciled { remaining: 1 });
    }

    #[test]
    fn soft_cancel_restores_frozen_temporarily_released_modifier() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let snapshot = FrozenPhysicalSnapshot::new(0b0000_0100, false);
        let mut operation = SyntheticOperation::new(
            OperationId(53),
            &mut sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            ),
            snapshot,
            latch.clone(),
        );
        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_soft_cancel(&mut restored);

        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 1);
        assert_eq!(
            restored.adopted,
            [(
                Key::KEY_LEFTSHIFT,
                FakeToken {
                    key: Key::KEY_LEFTSHIFT
                }
            )]
        );
        assert_eq!(report.outcome, OperationOutcome::SoftCancelled);
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(!latch.is_failed());
    }

    #[test]
    fn rejected_restore_transfer_is_drained_release_only() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let snapshot = FrozenPhysicalSnapshot::new(0b0000_0100, false);
        let mut operation = SyntheticOperation::new(
            OperationId(54),
            &mut sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            ),
            snapshot,
            latch.clone(),
        );
        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        let mut restored = FakeRestoredModifierTarget {
            reject: true,
            ..FakeRestoredModifierTarget::default()
        };

        let report = operation.finish_soft_cancel(&mut restored);

        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 2);
        assert!(restored.adopted.is_empty());
        assert_eq!(report.outcome, OperationOutcome::HardFailed);
        assert!(matches!(report.cleanup, Some(SwitcherError::Io(ref error))
            if error.to_string() == "restored modifier target rejected transfer"));
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(latch.is_failed());
    }

    #[test]
    fn failure_at_n_matrix_never_repeats_down_and_reports_honest_proof() {
        let phases = [
            FailAt::Prepare,
            FailAt::DownBeforeApply,
            FailAt::DownAfterApply,
            FailAt::DownSync,
            FailAt::UpBeforeApply,
            FailAt::UpAfterApply,
            FailAt::UpSync,
            FailAt::CleanupUp,
            FailAt::CleanupSync,
        ];

        for phase in phases {
            let mut sink = MatrixSink::new(phase);
            let latch = SyntheticFailureLatch::default();
            let mut operation = SyntheticOperation::new(
                OperationId(100 + phase as u64),
                &mut sink,
                OperationControl::new(
                    Instant::now() + Duration::from_secs(1),
                    Arc::new(AtomicBool::new(false)),
                ),
                FrozenPhysicalSnapshot::default(),
                latch.clone(),
            );

            let primary = match phase {
                FailAt::Prepare
                | FailAt::DownBeforeApply
                | FailAt::DownAfterApply
                | FailAt::DownSync => operation.press(Key::KEY_A).unwrap_err(),
                FailAt::UpBeforeApply | FailAt::UpAfterApply | FailAt::UpSync => {
                    let press_id = operation.press(Key::KEY_A).unwrap();
                    operation.release(press_id).unwrap_err()
                }
                FailAt::CleanupUp | FailAt::CleanupSync => {
                    operation.press(Key::KEY_A).unwrap();
                    SwitcherError::Io(io::Error::other("primary failure"))
                }
            };
            let report = operation.finish_hard_failure(primary);

            assert!(
                sink.no_down_after_terminal(),
                "down after terminal for {phase:?}"
            );
            assert!(
                sink.down_count(Key::KEY_A) <= 1,
                "down repeated for {phase:?}"
            );
            assert_eq!(
                report.proof == TerminalProof::Reconciled,
                sink.possibly_down.is_empty(),
                "dishonest proof for {phase:?}"
            );
            assert_eq!(
                latch.is_failed(),
                report.proof != TerminalProof::Reconciled,
                "unexpected latch state for {phase:?}"
            );
        }
    }

    #[test]
    fn cleanup_continues_through_two_consecutive_release_errors() {
        let mut sink = MatrixSink::with_cleanup_up_failures(2);
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(200),
            &mut sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            ),
            FrozenPhysicalSnapshot::default(),
            latch.clone(),
        );
        operation.press(Key::KEY_A).unwrap();
        operation.press(Key::KEY_B).unwrap();

        let report =
            operation.finish_hard_failure(SwitcherError::Io(io::Error::other("primary failure")));

        assert_eq!(sink.up_attempts, [Key::KEY_B, Key::KEY_A]);
        assert_eq!(report.proof, TerminalProof::Unreconciled { remaining: 2 });
        assert!(latch.is_failed());
    }

    #[test]
    fn fake_third_backend_passes_unmodified_sink_contract() {
        let mut sink = FakeThirdBackend::new(73);

        let report = run_synthetic_sink_conformance(&mut sink);

        assert_eq!(sink.down_attempts, 1);
        assert!(sink.owner_state.borrow().is_empty());
        assert_eq!(
            report.proof,
            TerminalProof::OwnerGenerationDestroyed { generation: 73 }
        );
    }

    #[test]
    fn frozen_snapshot_is_not_changed_with_live_modifier_state() {
        let mut live_modifier_bits = 0b0000_0100;
        let snapshot = FrozenPhysicalSnapshot::new(live_modifier_bits, true);
        live_modifier_bits = 0;

        assert_eq!(live_modifier_bits, 0);
        assert!(snapshot.holds_modifier(Key::KEY_LEFTSHIFT));
        assert!(snapshot.caps_lock_active());
    }

    #[test]
    fn temporary_release_rejects_modifier_absent_from_frozen_snapshot() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let mut operation = test_operation(55, &mut sink, latch);

        let error = operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputSafety(InputSafetyError::ProtocolViolation {
                operation_id: 55,
                context: "temporary release requires a frozen held modifier",
            })
        ));
        let _ = operation.finish_hard_failure(error);
        assert_eq!(sink.down_count, 0);
        assert_eq!(sink.up_count, 0);
    }

    #[test]
    fn hard_failure_never_creates_restore_down() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(56),
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

        let report =
            operation.finish_hard_failure(SwitcherError::Io(io::Error::other("primary failure")));

        assert_eq!(sink.down_count, 0);
        assert_eq!(sink.up_count, 1);
        assert_eq!(report.proof, TerminalProof::Reconciled);
    }

    #[test]
    fn cancellation_before_restore_escalates_without_new_down() {
        let mut sink = FakeSink::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(57),
            &mut sink,
            OperationControl::new(Instant::now() + Duration::from_secs(1), cancelled.clone()),
            FrozenPhysicalSnapshot::new(0b0000_0100, false),
            latch.clone(),
        );
        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        cancelled.store(true, Ordering::Release);
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_soft_cancel(&mut restored);

        assert_eq!(sink.down_count, 0);
        assert_eq!(sink.up_count, 1);
        assert_eq!(report.outcome, OperationOutcome::HardFailed);
        assert!(matches!(
            report.cleanup,
            Some(SwitcherError::InputSafety(
                InputSafetyError::OperationCancelled { operation_id: 57 }
            ))
        ));
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(latch.is_failed());
    }

    #[test]
    fn expired_deadline_before_restore_escalates_without_new_down() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(58),
            &mut sink,
            OperationControl::new(
                Instant::now() + Duration::from_millis(100),
                Arc::new(AtomicBool::new(false)),
            ),
            FrozenPhysicalSnapshot::new(0b0000_0100, false),
            latch.clone(),
        );
        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        std::thread::sleep(Duration::from_millis(120));
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_soft_cancel(&mut restored);

        assert_eq!(sink.down_count, 0);
        assert_eq!(sink.up_count, 1);
        assert_eq!(report.outcome, OperationOutcome::HardFailed);
        assert!(matches!(
            report.cleanup,
            Some(SwitcherError::InputSafety(
                InputSafetyError::OperationTimedOut { operation_id: 58 }
            ))
        ));
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(latch.is_failed());
    }

    #[test]
    fn ambiguous_restore_down_is_released_without_retrying_down() {
        let mut sink = FakeSink::fail_down_after_apply();
        let latch = SyntheticFailureLatch::default();
        let mut operation = SyntheticOperation::new(
            OperationId(59),
            &mut sink,
            OperationControl::new(
                Instant::now() + Duration::from_secs(1),
                Arc::new(AtomicBool::new(false)),
            ),
            FrozenPhysicalSnapshot::new(0b0000_0100, false),
            latch.clone(),
        );
        operation
            .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
            .unwrap();
        let mut restored = FakeRestoredModifierTarget::default();

        let report = operation.finish_success(&mut restored);

        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 2);
        assert!(restored.adopted.is_empty());
        assert_eq!(report.outcome, OperationOutcome::HardFailed);
        assert!(matches!(report.cleanup, Some(SwitcherError::Io(ref error))
            if error.to_string() == "down failed after mutation"));
        assert_eq!(report.proof, TerminalProof::Reconciled);
        assert!(latch.is_failed());
    }

    #[test]
    fn panic_during_restore_transfer_returns_token_to_drop_cleanup() {
        let mut sink = FakeSink::default();
        let latch = SyntheticFailureLatch::default();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut operation = SyntheticOperation::new(
                OperationId(60),
                &mut sink,
                OperationControl::new(
                    Instant::now() + Duration::from_secs(1),
                    Arc::new(AtomicBool::new(false)),
                ),
                FrozenPhysicalSnapshot::new(0b0000_0100, false),
                latch.clone(),
            );
            operation
                .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
                .unwrap();
            let mut restored = PanickingRestoredModifierTarget;
            let _ = operation.finish_soft_cancel(&mut restored);
        }));

        assert!(unwind.is_err());
        assert_eq!(sink.down_count, 1);
        assert_eq!(sink.up_count, 2);
        let report = latch
            .take_report()
            .expect("unwinding finalizer must run drop cleanup");
        assert_eq!(report.proof, TerminalProof::Reconciled);
    }

    #[test]
    fn panic_while_draining_fallback_does_not_lose_transfer_token() {
        let mut sink = FakeSink {
            panic_on_up_call: Some(2),
            ..FakeSink::default()
        };
        let latch = SyntheticFailureLatch::default();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut operation = SyntheticOperation::new(
                OperationId(61),
                &mut sink,
                OperationControl::new(
                    Instant::now() + Duration::from_secs(1),
                    Arc::new(AtomicBool::new(false)),
                ),
                FrozenPhysicalSnapshot::new(0b0000_0100, false),
                latch.clone(),
            );
            operation
                .temporarily_release_physical_modifier(Key::KEY_LEFTSHIFT)
                .unwrap();
            let mut restored = FakeRestoredModifierTarget {
                reject: true,
                ..FakeRestoredModifierTarget::default()
            };
            let _ = operation.finish_soft_cancel(&mut restored);
        }));

        assert!(unwind.is_err());
        assert_eq!(sink.down_count, 1);
        assert_eq!(
            sink.up_count, 3,
            "Drop must retry the token whose first cleanup panicked"
        );
        let report = latch
            .take_report()
            .expect("unwinding finalizer must run drop cleanup");
        assert_eq!(report.proof, TerminalProof::Reconciled);
    }

    #[test]
    fn cancellation_between_backend_calls_never_skips_release_or_repeats_down() {
        #[derive(Clone, Copy, Debug)]
        enum CancelAt {
            Prepare,
            Down,
            DownSync,
            Up,
            UpSync,
        }

        for cancel_at in [
            CancelAt::Prepare,
            CancelAt::Down,
            CancelAt::DownSync,
            CancelAt::Up,
            CancelAt::UpSync,
        ] {
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut sink = FakeSink::default();
            match cancel_at {
                CancelAt::Prepare => sink.cancel_after_prepare = Some(cancelled.clone()),
                CancelAt::Down => sink.cancel_after_down = Some(cancelled.clone()),
                CancelAt::DownSync => sink.cancel_after_sync_on_call = Some((1, cancelled.clone())),
                CancelAt::Up => sink.cancel_after_up = Some(cancelled.clone()),
                CancelAt::UpSync => sink.cancel_after_sync_on_call = Some((2, cancelled.clone())),
            }
            let latch = SyntheticFailureLatch::default();
            let mut operation = SyntheticOperation::new(
                OperationId(300 + cancel_at as u64),
                &mut sink,
                OperationControl::new(Instant::now() + Duration::from_secs(1), cancelled),
                FrozenPhysicalSnapshot::default(),
                latch.clone(),
            );

            let primary = match cancel_at {
                CancelAt::Prepare | CancelAt::Down | CancelAt::DownSync => {
                    operation.press(Key::KEY_A).unwrap_err()
                }
                CancelAt::Up | CancelAt::UpSync => {
                    let press_id = operation.press(Key::KEY_A).unwrap();
                    operation.release(press_id).unwrap_err()
                }
            };
            assert!(matches!(
                primary,
                SwitcherError::InputSafety(InputSafetyError::OperationCancelled { .. })
            ));
            let report = operation.finish_hard_failure(primary);

            let expected_down = usize::from(!matches!(cancel_at, CancelAt::Prepare));
            assert_eq!(sink.down_count, expected_down, "{cancel_at:?}");
            assert_eq!(
                sink.up_count, expected_down,
                "release mismatch at {cancel_at:?}"
            );
            assert_eq!(report.proof, TerminalProof::Reconciled, "{cancel_at:?}");
            assert!(!latch.is_failed(), "{cancel_at:?}");
        }
    }

    #[test]
    fn timeout_between_backend_calls_never_skips_release_or_repeats_down() {
        #[derive(Clone, Copy, Debug)]
        enum ExpireAt {
            Prepare,
            Down,
            DownSync,
            Up,
            UpSync,
        }

        const DEADLINE: Duration = Duration::from_millis(100);
        const DELAY: Duration = Duration::from_millis(120);

        for expire_at in [
            ExpireAt::Prepare,
            ExpireAt::Down,
            ExpireAt::DownSync,
            ExpireAt::Up,
            ExpireAt::UpSync,
        ] {
            let mut sink = FakeSink::default();
            match expire_at {
                ExpireAt::Prepare => sink.delay_after_prepare = Some(DELAY),
                ExpireAt::Down => sink.delay_after_down = Some(DELAY),
                ExpireAt::DownSync => sink.delay_after_sync_on_call = Some((1, DELAY)),
                ExpireAt::Up => sink.delay_after_up = Some(DELAY),
                ExpireAt::UpSync => sink.delay_after_sync_on_call = Some((2, DELAY)),
            }
            let latch = SyntheticFailureLatch::default();
            let mut operation = SyntheticOperation::new(
                OperationId(400 + expire_at as u64),
                &mut sink,
                OperationControl::new(Instant::now() + DEADLINE, Arc::new(AtomicBool::new(false))),
                FrozenPhysicalSnapshot::default(),
                latch.clone(),
            );

            let primary = match expire_at {
                ExpireAt::Prepare | ExpireAt::Down | ExpireAt::DownSync => {
                    operation.press(Key::KEY_A).unwrap_err()
                }
                ExpireAt::Up | ExpireAt::UpSync => {
                    let press_id = operation.press(Key::KEY_A).unwrap();
                    operation.release(press_id).unwrap_err()
                }
            };
            assert!(matches!(
                primary,
                SwitcherError::InputSafety(InputSafetyError::OperationTimedOut { .. })
            ));
            let report = operation.finish_hard_failure(primary);

            let expected_down = usize::from(!matches!(expire_at, ExpireAt::Prepare));
            assert_eq!(sink.down_count, expected_down, "{expire_at:?}");
            assert_eq!(
                sink.up_count, expected_down,
                "release mismatch at {expire_at:?}"
            );
            assert_eq!(report.proof, TerminalProof::Reconciled, "{expire_at:?}");
            assert!(!latch.is_failed(), "{expire_at:?}");
        }
    }
}
