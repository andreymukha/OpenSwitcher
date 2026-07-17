use std::array;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

pub(crate) const MAX_DEBUG_RECORD_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugLogKind {
    Input = 0,
    Layout = 1,
    Capture = 2,
    SelectedText = 3,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DebugLogRecord {
    pub kind: DebugLogKind,
    pub line: Box<str>,
}

enum DebugLogCommand {
    Record(DebugLogRecord),
    #[allow(dead_code)]
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugEnqueueOutcome {
    Disabled,
    Queued,
    DroppedFull,
    DroppedClosed,
}

struct DebugDropCounters {
    values: [AtomicU64; 4],
}

impl Default for DebugDropCounters {
    fn default() -> Self {
        Self {
            values: array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

#[derive(Clone)]
pub(crate) struct DebugLogProducer {
    enabled_mask: u8,
    sender: Option<mpsc::SyncSender<DebugLogCommand>>,
    dropped: Arc<DebugDropCounters>,
}

impl DebugLogProducer {
    pub(crate) fn enabled(&self, kind: DebugLogKind) -> bool {
        self.enabled_mask & (1 << kind as u8) != 0
    }

    pub(crate) fn try_enqueue_with(
        &self,
        kind: DebugLogKind,
        build: impl FnOnce() -> String,
    ) -> DebugEnqueueOutcome {
        if !self.enabled(kind) {
            return DebugEnqueueOutcome::Disabled;
        }
        self.try_enqueue(kind, build())
    }

    pub(crate) fn try_enqueue(&self, kind: DebugLogKind, mut line: String) -> DebugEnqueueOutcome {
        truncate_utf8(&mut line, MAX_DEBUG_RECORD_BYTES);
        let record = DebugLogRecord {
            kind,
            line: line.into_boxed_str(),
        };
        let Some(sender) = self.sender.as_ref() else {
            self.record_drop(kind);
            return DebugEnqueueOutcome::DroppedClosed;
        };
        match sender.try_send(DebugLogCommand::Record(record)) {
            Ok(()) => DebugEnqueueOutcome::Queued,
            Err(mpsc::TrySendError::Full(_)) => {
                self.record_drop(kind);
                DebugEnqueueOutcome::DroppedFull
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.record_drop(kind);
                DebugEnqueueOutcome::DroppedClosed
            }
        }
    }

    fn record_drop(&self, kind: DebugLogKind) {
        self.dropped.values[kind as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn dropped(&self, kind: DebugLogKind) -> u64 {
        self.dropped.values[kind as usize].load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn for_test(
        capacity: usize,
        enabled: &[DebugLogKind],
    ) -> (Self, mpsc::Receiver<DebugLogCommand>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let enabled_mask = enabled
            .iter()
            .fold(0u8, |mask, kind| mask | (1 << *kind as u8));
        (
            Self {
                enabled_mask,
                sender: Some(sender),
                dropped: Arc::new(DebugDropCounters::default()),
            },
            receiver,
        )
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::TryRecvError;

    #[test]
    fn full_queue_drops_newest_without_replacing_first_record() {
        let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Input]);

        assert_eq!(
            producer.try_enqueue(DebugLogKind::Input, "first".to_string()),
            DebugEnqueueOutcome::Queued
        );
        assert_eq!(
            producer.try_enqueue(DebugLogKind::Input, "second".to_string()),
            DebugEnqueueOutcome::DroppedFull
        );
        let DebugLogCommand::Record(record) = receiver.try_recv().unwrap() else {
            panic!("expected queued record");
        };
        assert_eq!(record.line.as_ref(), "first");
        assert_eq!(producer.dropped(DebugLogKind::Input), 1);
    }

    #[test]
    fn disconnected_queue_drops_without_panicking() {
        let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Layout]);
        drop(receiver);

        assert_eq!(
            producer.try_enqueue(DebugLogKind::Layout, "line".to_string()),
            DebugEnqueueOutcome::DroppedClosed
        );
        assert_eq!(producer.dropped(DebugLogKind::Layout), 1);
    }

    #[test]
    fn disabled_category_does_not_build_or_enqueue_a_record() {
        let (producer, receiver) =
            DebugLogProducer::for_test(1, &[DebugLogKind::Capture, DebugLogKind::SelectedText]);
        let built = AtomicBool::new(false);

        assert_eq!(
            producer.try_enqueue_with(DebugLogKind::Input, || {
                built.store(true, Ordering::SeqCst);
                "must-not-be-built".to_string()
            }),
            DebugEnqueueOutcome::Disabled
        );
        assert!(!built.load(Ordering::SeqCst));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn oversized_record_is_truncated_on_utf8_boundary() {
        let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Input]);
        producer.try_enqueue(DebugLogKind::Input, "я".repeat(MAX_DEBUG_RECORD_BYTES));

        let DebugLogCommand::Record(record) = receiver.try_recv().unwrap() else {
            panic!("expected queued record");
        };
        assert!(record.line.len() <= MAX_DEBUG_RECORD_BYTES);
        assert!(record.line.is_char_boundary(record.line.len()));
    }
}
