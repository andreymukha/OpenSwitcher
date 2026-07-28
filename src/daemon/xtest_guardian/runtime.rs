use super::client::{
    EmergencyCoordinator, GuardianClient, GuardianHealth, GuardianMutationDeadline,
};
use super::protocol::{PreparedToken, MAX_PREPARED_TOKENS};
use crate::daemon::synthetic_input::{
    InputGeneration, OperationId, PendingTransfer, RestoredModifierTarget, SessionModifierLedger,
    SessionModifierState, SyntheticFailureLatch, SyntheticKeySink, TerminalProof,
};
use crate::error::{InputSafetyError, SwitcherError};
use evdev::Key;
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuardianPlanStep {
    PhysicalRelease(Key),
    Prepared(Key),
}

pub(crate) struct GuardianSyntheticRuntime {
    client: RefCell<GuardianClient>,
    generation: InputGeneration,
    session_modifiers: RefCell<SessionModifierLedger<PreparedToken>>,
    failure_latch: SyntheticFailureLatch,
}

impl GuardianSyntheticRuntime {
    pub(crate) fn new(
        client: GuardianClient,
        generation: InputGeneration,
    ) -> Result<Self, SwitcherError> {
        if generation.0 == 0 {
            return Err(InputSafetyError::GuardianProtocol {
                context: "XTEST input generation must be nonzero",
            }
            .into());
        }
        Ok(Self {
            client: RefCell::new(client),
            generation,
            session_modifiers: RefCell::new(SessionModifierLedger::new(generation)),
            failure_latch: SyntheticFailureLatch::default(),
        })
    }

    pub(crate) fn health(&self) -> GuardianHealth {
        self.client.borrow().health()
    }

    pub(crate) fn emergency_coordinator(&self) -> EmergencyCoordinator {
        self.client.borrow().emergency_coordinator()
    }

    pub(crate) fn has_session_modifier(&self, key: Key) -> bool {
        self.session_modifiers.borrow().contains(key)
    }

    pub(crate) fn prepare_operation(
        &mut self,
        operation: OperationId,
        deadline: GuardianMutationDeadline,
        steps: impl IntoIterator<Item = GuardianPlanStep>,
    ) -> Result<
        (
            GuardianPreparedSink<'_>,
            GuardianSessionModifierTarget<'_>,
            SyntheticFailureLatch,
        ),
        SwitcherError,
    > {
        if self.failure_latch.is_failed() {
            return Err(InputSafetyError::Reconciliation {
                operation_id: operation.0,
                remaining: self
                    .session_modifiers
                    .borrow()
                    .release_only_snapshot()
                    .len(),
            }
            .into());
        }
        let steps: Vec<_> = steps.into_iter().collect();
        if steps.len() > MAX_PREPARED_TOKENS {
            return Err(self
                .client
                .borrow()
                .fail_protocol("XTEST operation plan exceeds prepared token capacity"));
        }

        let mut planned = VecDeque::with_capacity(steps.len());
        for step in steps {
            let key = match step {
                GuardianPlanStep::PhysicalRelease(key) | GuardianPlanStep::Prepared(key) => key,
            };
            let token = match step {
                GuardianPlanStep::PhysicalRelease(key) => {
                    let session = self.session_modifiers.borrow();
                    match session.state(key) {
                        Some(SessionModifierState::OwnedDown) => session.token_for_key(key).ok_or(
                            InputSafetyError::GuardianProtocol {
                                context: "XTEST session modifier lost its token",
                            },
                        )?,
                        Some(
                            SessionModifierState::TemporarilyReleased
                            | SessionModifierState::RestoringPossiblyDown,
                        ) => {
                            drop(session);
                            return Err(self.client.borrow().fail_protocol(
                                "XTEST session modifier has an in-flight transition",
                            ));
                        }
                        None => {
                            drop(session);
                            self.client
                                .borrow_mut()
                                .prepare_key(operation, key, deadline)?
                        }
                    }
                }
                GuardianPlanStep::Prepared(key) => self
                    .client
                    .borrow_mut()
                    .prepare_key(operation, key, deadline)?,
            };
            if token.evdev_code != key.code() {
                return Err(self
                    .client
                    .borrow()
                    .fail_protocol("XTEST operation plan token key does not match"));
            }
            planned.push_back((key, token));
        }

        Ok((
            GuardianPreparedSink {
                client: &self.client,
                operation,
                deadline,
                planned,
            },
            GuardianSessionModifierTarget {
                client: &self.client,
                ledger: &self.session_modifiers,
                operation,
                generation: self.generation,
                deadline,
            },
            self.failure_latch.clone(),
        ))
    }
}

pub(crate) struct GuardianPreparedSink<'a> {
    client: &'a RefCell<GuardianClient>,
    operation: OperationId,
    deadline: GuardianMutationDeadline,
    planned: VecDeque<(Key, PreparedToken)>,
}

impl SyntheticKeySink for GuardianPreparedSink<'_> {
    type Token = PreparedToken;

    fn prepare_down(&mut self, key: Key) -> Result<Self::Token, SwitcherError> {
        let Some((planned_key, token)) = self.planned.front().copied() else {
            return Err(self
                .client
                .borrow()
                .fail_protocol("XTEST operation consumed more keys than its prepared plan"));
        };
        if planned_key != key || token.evdev_code != key.code() {
            return Err(self
                .client
                .borrow()
                .fail_protocol("XTEST operation key order differs from its prepared plan"));
        }
        self.planned.pop_front();
        Ok(token)
    }

    fn attempt_down(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.client
            .borrow_mut()
            .execute_down(self.operation, *token, self.deadline)
    }

    fn attempt_up(&mut self, token: &Self::Token) -> Result<(), SwitcherError> {
        self.client
            .borrow_mut()
            .key_up(self.operation, *token, self.deadline)
    }

    fn synchronize(&mut self) -> Result<(), SwitcherError> {
        self.client
            .borrow_mut()
            .synchronize_if_pending(self.operation, self.deadline)
    }

    fn terminal_proof(&self, remaining_debt: usize) -> TerminalProof {
        self.client.borrow().operation_terminal_proof(
            self.operation,
            remaining_debt.saturating_add(self.planned.len()),
        )
    }
}

pub(crate) struct GuardianSessionModifierTarget<'a> {
    client: &'a RefCell<GuardianClient>,
    ledger: &'a RefCell<SessionModifierLedger<PreparedToken>>,
    operation: OperationId,
    generation: InputGeneration,
    deadline: GuardianMutationDeadline,
}

impl GuardianSessionModifierTarget<'_> {
    pub(crate) fn contains(&self, key: Key) -> bool {
        self.ledger.borrow().contains(key)
    }

    pub(crate) fn mark_temporarily_released(&mut self, key: Key) -> Result<(), SwitcherError> {
        self.ledger.borrow_mut().mark_temporarily_released(key)
    }
}

impl RestoredModifierTarget<PreparedToken> for GuardianSessionModifierTarget<'_> {
    fn begin_restore(&mut self, key: Key) -> Result<(), SwitcherError> {
        let mut ledger = self.ledger.borrow_mut();
        if ledger.contains(key) {
            ledger.mark_restoring(key)?;
        }
        Ok(())
    }

    fn adopt_restored(
        &mut self,
        key: Key,
        transfer: PendingTransfer<PreparedToken>,
    ) -> Result<(), SwitcherError> {
        transfer.commit(|token| {
            self.client.borrow_mut().transfer_to_physical_debt(
                self.operation,
                *token,
                self.generation,
                self.deadline,
            )?;
            self.ledger.borrow_mut().adopt_restored(key, *token)
        })
    }
}
