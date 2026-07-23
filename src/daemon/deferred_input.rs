use evdev::Key;
use std::collections::VecDeque;
use std::time::SystemTime;

pub(crate) const DEFERRED_INPUT_SOFT_LIMIT: usize = 256;
pub(crate) const DEFERRED_INPUT_HARD_LIMIT: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeferredInputEvent {
    pub(crate) sequence_id: u64,
    pub(crate) key: Key,
    pub(crate) value: i32,
    pub(crate) timestamp: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeferredAdmission {
    Queued,
    RequestCancellation,
    CapacityExceeded { limit: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeferredAckError {
    pub(crate) expected: u64,
    pub(crate) received: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeferredReconciliationReport {
    pub(crate) accepted: u64,
    pub(crate) acknowledged: u64,
    pub(crate) reconciled: u64,
    pub(crate) queued: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredInputLedger {
    queue: VecDeque<DeferredInputEvent>,
    accepted: u64,
    acknowledged: u64,
    soft_limit_reported: bool,
    soft_limit: usize,
    hard_limit: usize,
}

impl Default for DeferredInputLedger {
    fn default() -> Self {
        Self::with_limits(DEFERRED_INPUT_SOFT_LIMIT, DEFERRED_INPUT_HARD_LIMIT)
    }
}

impl DeferredInputLedger {
    pub(crate) fn with_limits(soft_limit: usize, hard_limit: usize) -> Self {
        debug_assert!(soft_limit < hard_limit);
        Self {
            queue: VecDeque::new(),
            accepted: 0,
            acknowledged: 0,
            soft_limit_reported: false,
            soft_limit,
            hard_limit,
        }
    }

    pub(crate) fn admit(&mut self, event: DeferredInputEvent) -> DeferredAdmission {
        if self.queue.len() >= self.hard_limit {
            return DeferredAdmission::CapacityExceeded {
                limit: self.hard_limit,
            };
        }

        self.queue.push_back(event);
        self.accepted = self.accepted.saturating_add(1);
        if !self.soft_limit_reported && self.queue.len() > self.soft_limit {
            self.soft_limit_reported = true;
            DeferredAdmission::RequestCancellation
        } else {
            DeferredAdmission::Queued
        }
    }

    pub(crate) fn peek(&self) -> Option<&DeferredInputEvent> {
        self.queue.front()
    }

    pub(crate) fn acknowledge(&mut self, sequence_id: u64) -> Result<(), DeferredAckError> {
        let expected = self
            .queue
            .front()
            .map(|event| event.sequence_id)
            .unwrap_or(0);
        if expected != sequence_id {
            return Err(DeferredAckError {
                expected,
                received: sequence_id,
            });
        }

        let _ = self.queue.pop_front();
        self.acknowledged = self.acknowledged.saturating_add(1);
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub(crate) fn reconcile_all(&mut self) -> DeferredReconciliationReport {
        self.reconciled_queued()
    }

    pub(crate) fn finish_drained(&mut self) -> DeferredReconciliationReport {
        debug_assert!(self.queue.is_empty());
        self.finish_report(0)
    }

    fn reconciled_queued(&mut self) -> DeferredReconciliationReport {
        let reconciled = self.queue.len() as u64;
        self.queue.clear();
        self.finish_report(reconciled)
    }

    fn finish_report(&mut self, reconciled: u64) -> DeferredReconciliationReport {
        let report = DeferredReconciliationReport {
            accepted: self.accepted,
            acknowledged: self.acknowledged,
            reconciled,
            queued: self.queue.len(),
        };
        debug_assert_eq!(
            report.accepted,
            report
                .acknowledged
                .saturating_add(report.reconciled)
                .saturating_add(report.queued as u64)
        );
        self.accepted = 0;
        self.acknowledged = 0;
        self.soft_limit_reported = false;
        report
    }

    #[cfg(test)]
    pub(crate) fn sequence_ids_for_test(&self) -> Vec<u64> {
        self.queue.iter().map(|event| event.sequence_id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::Key;
    use std::time::SystemTime;

    fn event(sequence_id: u64) -> DeferredInputEvent {
        DeferredInputEvent {
            sequence_id,
            key: Key::KEY_A,
            value: 1,
            timestamp: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn two_hundred_fifty_seventh_event_is_retained_and_requests_cancel_once() {
        let mut ledger = DeferredInputLedger::default();
        for sequence_id in 1..=256 {
            assert_eq!(ledger.admit(event(sequence_id)), DeferredAdmission::Queued);
        }
        assert_eq!(
            ledger.admit(event(257)),
            DeferredAdmission::RequestCancellation
        );
        assert_eq!(ledger.len(), 257);
        assert_eq!(ledger.admit(event(258)), DeferredAdmission::Queued);
    }

    #[test]
    fn head_is_removed_only_after_matching_ack() {
        let mut ledger = DeferredInputLedger::default();
        ledger.admit(event(10));
        ledger.admit(event(11));
        assert_eq!(ledger.peek().map(|event| event.sequence_id), Some(10));
        assert_eq!(
            ledger.acknowledge(11),
            Err(DeferredAckError {
                expected: 10,
                received: 11,
            })
        );
        assert_eq!(ledger.len(), 2);
        ledger.acknowledge(10).unwrap();
        assert_eq!(ledger.peek().map(|event| event.sequence_id), Some(11));
    }

    #[test]
    fn terminal_reconciliation_accounts_for_every_queued_event_once() {
        let mut ledger = DeferredInputLedger::default();
        ledger.admit(event(1));
        ledger.admit(event(2));
        ledger.acknowledge(1).unwrap();
        let report = ledger.reconcile_all();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.acknowledged, 1);
        assert_eq!(report.reconciled, 1);
        assert_eq!(report.queued, 0);
        assert!(ledger.is_empty());
    }

    #[test]
    fn hard_limit_rejects_ownership_transfer_without_dropping_existing_queue() {
        let mut ledger = DeferredInputLedger::with_limits(2, 3);
        assert_eq!(ledger.admit(event(1)), DeferredAdmission::Queued);
        assert_eq!(ledger.admit(event(2)), DeferredAdmission::Queued);
        assert_eq!(
            ledger.admit(event(3)),
            DeferredAdmission::RequestCancellation
        );
        assert_eq!(
            ledger.admit(event(4)),
            DeferredAdmission::CapacityExceeded { limit: 3 }
        );
        assert_eq!(ledger.len(), 3);
    }
}
