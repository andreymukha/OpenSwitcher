use crate::layout_backend::LayoutSetupDetection;
use std::time::{Duration, Instant};

const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

pub(crate) struct LayoutSetupRetry {
    failures: usize,
    next_due: Option<Instant>,
}

impl LayoutSetupRetry {
    pub(crate) fn confirmed() -> Self {
        Self {
            failures: 0,
            next_due: None,
        }
    }

    pub(crate) fn pending_at(now: Instant) -> Self {
        Self {
            failures: 0,
            next_due: now.checked_add(RETRY_DELAYS[0]),
        }
    }

    pub(crate) fn from_detection(detection: &LayoutSetupDetection, now: Instant) -> Self {
        if detection.is_confirmed() {
            Self::confirmed()
        } else {
            Self::pending_at(now)
        }
    }

    pub(crate) fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let index = self.failures.min(RETRY_DELAYS.len() - 1);
        self.next_due = now.checked_add(RETRY_DELAYS[index]);
    }

    pub(crate) fn record_confirmed(&mut self) {
        self.failures = 0;
        self.next_due = None;
    }

    pub(crate) fn force_due(&mut self, now: Instant) {
        self.next_due = Some(now);
    }

    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.next_due
    }

    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.next_due.is_some_and(|due| now >= due)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn retry_uses_bounded_backoff_and_stops_after_confirmation() {
        let start = Instant::now();
        let mut retry = LayoutSetupRetry::pending_at(start);
        let expected = [1, 2, 5, 10, 30, 30];
        let mut previous = start;

        for seconds in expected {
            let due = retry.next_due().unwrap();
            assert_eq!(due.duration_since(previous), Duration::from_secs(seconds),);
            retry.record_failure(due);
            previous = due;
        }

        retry.record_confirmed();
        assert_eq!(retry.next_due(), None);
    }

    #[test]
    fn successful_mode_never_becomes_due_with_time() {
        let start = Instant::now();
        let retry = LayoutSetupRetry::confirmed();
        assert!(!retry.is_due(start + Duration::from_secs(86_400)));
    }
}
