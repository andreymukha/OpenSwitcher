use super::{log_selected_text_debug, ClipboardDisposition};
use crate::error::SelectedTextError;

pub(super) trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String, SelectedTextError>;
    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError>;
    fn clear(&mut self) -> Result<(), SelectedTextError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClipboardSnapshot {
    RestorableText(String),
    Unrestorable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OwnedTextKind {
    Sentinel,
    Selected,
    Converted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupOutcome {
    Restored,
    SentinelCleared,
    MeaningfulTextKept,
    NothingToClean,
    Failed,
}

pub(super) struct ClipboardTransaction<'a, C: ClipboardAccess> {
    clipboard: &'a mut C,
    original: ClipboardSnapshot,
    expected_text: Option<String>,
    expected_kind: Option<OwnedTextKind>,
    last_meaningful_text: Option<String>,
    finalized: bool,
}

impl<'a, C: ClipboardAccess> ClipboardTransaction<'a, C> {
    pub(super) fn begin(clipboard: &'a mut C) -> Self {
        let original = match clipboard.get_text() {
            Ok(text) => ClipboardSnapshot::RestorableText(text),
            Err(_) => ClipboardSnapshot::Unrestorable,
        };

        Self {
            clipboard,
            original,
            expected_text: None,
            expected_kind: None,
            last_meaningful_text: None,
            finalized: false,
        }
    }

    pub(super) fn original(&self) -> &ClipboardSnapshot {
        &self.original
    }

    pub(super) fn write_operation_text(
        &mut self,
        kind: OwnedTextKind,
        value: &str,
    ) -> Result<(), SelectedTextError> {
        self.expected_text = Some(value.to_owned());
        self.expected_kind = Some(kind);
        if kind != OwnedTextKind::Sentinel {
            self.last_meaningful_text = Some(value.to_owned());
        }
        self.clipboard.set_text(value)
    }

    pub(super) fn adopt_selected_text(&mut self, text: &str) {
        self.expected_text = Some(text.to_owned());
        self.expected_kind = Some(OwnedTextKind::Selected);
        self.last_meaningful_text = Some(text.to_owned());
    }

    pub(super) fn finish_success(mut self) -> ClipboardDisposition {
        let disposition = match self.original.clone() {
            ClipboardSnapshot::RestorableText(_) => match self.rollback() {
                CleanupOutcome::Restored => ClipboardDisposition::Restored,
                CleanupOutcome::Failed => ClipboardDisposition::RestoreFailed,
                CleanupOutcome::SentinelCleared
                | CleanupOutcome::MeaningfulTextKept
                | CleanupOutcome::NothingToClean => ClipboardDisposition::RestoreFailed,
            },
            ClipboardSnapshot::Unrestorable => ClipboardDisposition::ConvertedTextKept,
        };
        self.finalized = true;
        disposition
    }

    pub(super) fn finish_no_selected_text(mut self) {
        let outcome = self.rollback();
        self.log_cleanup_outcome(outcome);
        self.finalized = true;
    }

    fn rollback(&mut self) -> CleanupOutcome {
        match self.original.clone() {
            ClipboardSnapshot::RestorableText(previous) => {
                if self.expected_text.is_none() {
                    return CleanupOutcome::NothingToClean;
                }
                if self.clipboard.set_text(&previous).is_ok() {
                    CleanupOutcome::Restored
                } else {
                    CleanupOutcome::Failed
                }
            }
            ClipboardSnapshot::Unrestorable => {
                if self.expected_kind != Some(OwnedTextKind::Sentinel) {
                    return if self.last_meaningful_text.is_some() {
                        CleanupOutcome::MeaningfulTextKept
                    } else {
                        CleanupOutcome::NothingToClean
                    };
                }

                let Some(expected) = self.expected_text.as_deref() else {
                    return CleanupOutcome::NothingToClean;
                };
                if self.clipboard.get_text().ok().as_deref() != Some(expected) {
                    return CleanupOutcome::NothingToClean;
                }
                if self.clipboard.clear().is_ok() {
                    CleanupOutcome::SentinelCleared
                } else {
                    CleanupOutcome::Failed
                }
            }
        }
    }

    fn log_cleanup_outcome(&self, outcome: CleanupOutcome) {
        log_selected_text_debug("clipboard-cleanup", &format!("outcome={outcome:?}"));
    }
}

impl<C: ClipboardAccess> Drop for ClipboardTransaction<'_, C> {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let outcome = self.rollback();
            self.log_cleanup_outcome(outcome);
        }));
    }
}

impl<C: ClipboardAccess> ClipboardAccess for ClipboardTransaction<'_, C> {
    fn get_text(&mut self) -> Result<String, SelectedTextError> {
        self.clipboard.get_text()
    }

    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
        self.clipboard.set_text(value)
    }

    fn clear(&mut self) -> Result<(), SelectedTextError> {
        self.clipboard.clear()
    }
}
