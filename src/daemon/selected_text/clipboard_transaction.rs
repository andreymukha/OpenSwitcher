use super::{log_selected_text_debug, ClipboardDisposition};
use crate::error::SelectedTextError;

pub(super) trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String, SelectedTextError>;
    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError>;
    fn clear(&mut self) -> Result<(), SelectedTextError>;
    fn owner_token(&mut self) -> Option<ClipboardOwnerToken>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ClipboardOwnerToken(pub(super) u32);

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
    ExternalChangePreserved,
    OwnershipUncertain,
    NothingToClean,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnedClipboardState {
    kind: OwnedTextKind,
    text: String,
    owner: Option<ClipboardOwnerToken>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingWrite {
    kind: OwnedTextKind,
    text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnershipStatus {
    Owned,
    Foreign,
    Uncertain,
    NothingToClean,
}

pub(super) struct ClipboardReadObservation {
    pub(super) text: Result<String, SelectedTextError>,
    pub(super) owner: Option<ClipboardOwnerToken>,
}

pub(super) fn read_with_stable_owner(
    clipboard: &mut impl ClipboardAccess,
) -> ClipboardReadObservation {
    let owner_before = clipboard.owner_token();
    let text = clipboard.get_text();
    let owner_after = clipboard.owner_token();
    let owner = match (owner_before, owner_after) {
        (Some(before), Some(after)) if before == after => Some(before),
        _ => None,
    };

    ClipboardReadObservation { text, owner }
}

pub(super) struct ClipboardTransaction<'a, C: ClipboardAccess> {
    clipboard: &'a mut C,
    original: ClipboardSnapshot,
    original_owner: Option<ClipboardOwnerToken>,
    current: Option<OwnedClipboardState>,
    pending_write: Option<PendingWrite>,
    last_meaningful_text: Option<String>,
    finalized: bool,
}

impl<'a, C: ClipboardAccess> ClipboardTransaction<'a, C> {
    pub(super) fn begin(clipboard: &'a mut C) -> Self {
        let observation = read_with_stable_owner(clipboard);
        let original = match observation.text {
            Ok(text) => ClipboardSnapshot::RestorableText(text),
            Err(_) => ClipboardSnapshot::Unrestorable,
        };

        Self {
            clipboard,
            original,
            original_owner: observation.owner,
            current: None,
            pending_write: None,
            last_meaningful_text: None,
            finalized: false,
        }
    }

    pub(super) fn original(&self) -> &ClipboardSnapshot {
        &self.original
    }

    pub(super) fn original_owner_is_known(&self) -> bool {
        self.original_owner.is_some()
    }

    pub(super) fn write_operation_text(
        &mut self,
        kind: OwnedTextKind,
        value: &str,
    ) -> Result<(), SelectedTextError> {
        self.pending_write = Some(PendingWrite {
            kind,
            text: value.to_owned(),
        });
        if kind != OwnedTextKind::Sentinel {
            self.last_meaningful_text = Some(value.to_owned());
        }
        self.clipboard.set_text(value)?;
        self.current = Some(OwnedClipboardState {
            kind,
            text: value.to_owned(),
            owner: self.clipboard.owner_token(),
        });
        self.pending_write = None;
        Ok(())
    }

    pub(super) fn adopt_selected_text(&mut self, text: &str, owner: Option<ClipboardOwnerToken>) {
        self.current = Some(OwnedClipboardState {
            kind: OwnedTextKind::Selected,
            text: text.to_owned(),
            owner,
        });
        self.pending_write = None;
        self.last_meaningful_text = Some(text.to_owned());
    }

    pub(super) fn finish_success(mut self) -> ClipboardDisposition {
        let disposition = match self.original.clone() {
            ClipboardSnapshot::RestorableText(_) => match self.rollback() {
                CleanupOutcome::Restored => ClipboardDisposition::Restored,
                CleanupOutcome::Failed => ClipboardDisposition::RestoreFailed,
                CleanupOutcome::ExternalChangePreserved | CleanupOutcome::OwnershipUncertain => {
                    ClipboardDisposition::ExternalChangePreserved
                }
                CleanupOutcome::SentinelCleared
                | CleanupOutcome::MeaningfulTextKept
                | CleanupOutcome::NothingToClean => ClipboardDisposition::RestoreFailed,
            },
            ClipboardSnapshot::Unrestorable => match self.rollback() {
                CleanupOutcome::ExternalChangePreserved => {
                    ClipboardDisposition::ExternalChangePreserved
                }
                CleanupOutcome::Failed => ClipboardDisposition::RestoreFailed,
                CleanupOutcome::MeaningfulTextKept
                | CleanupOutcome::OwnershipUncertain
                | CleanupOutcome::NothingToClean
                | CleanupOutcome::SentinelCleared
                | CleanupOutcome::Restored => ClipboardDisposition::ConvertedTextKept,
            },
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
                if self.original_owner.is_none() {
                    return CleanupOutcome::OwnershipUncertain;
                }
                match self.ownership_status() {
                    OwnershipStatus::Owned => {
                        if self.clipboard.set_text(&previous).is_ok() {
                            CleanupOutcome::Restored
                        } else {
                            CleanupOutcome::Failed
                        }
                    }
                    OwnershipStatus::Foreign => CleanupOutcome::ExternalChangePreserved,
                    OwnershipStatus::Uncertain => CleanupOutcome::OwnershipUncertain,
                    OwnershipStatus::NothingToClean => CleanupOutcome::NothingToClean,
                }
            }
            ClipboardSnapshot::Unrestorable => {
                let only_sentinel = self.last_meaningful_text.is_none()
                    && self
                        .current
                        .as_ref()
                        .map(|state| state.kind == OwnedTextKind::Sentinel)
                        .unwrap_or_else(|| {
                            self.pending_write
                                .as_ref()
                                .is_some_and(|pending| pending.kind == OwnedTextKind::Sentinel)
                        });

                match (only_sentinel, self.ownership_status()) {
                    (true, OwnershipStatus::Owned) => {
                        if self.clipboard.clear().is_ok() {
                            CleanupOutcome::SentinelCleared
                        } else {
                            CleanupOutcome::Failed
                        }
                    }
                    (_, OwnershipStatus::Foreign) => CleanupOutcome::ExternalChangePreserved,
                    (true, OwnershipStatus::Uncertain) => CleanupOutcome::OwnershipUncertain,
                    (false, OwnershipStatus::Owned | OwnershipStatus::Uncertain) => {
                        CleanupOutcome::MeaningfulTextKept
                    }
                    (_, OwnershipStatus::NothingToClean) => CleanupOutcome::NothingToClean,
                }
            }
        }
    }

    fn ownership_status(&mut self) -> OwnershipStatus {
        if self.current.is_none() && self.pending_write.is_none() {
            return OwnershipStatus::NothingToClean;
        }

        let observation = read_with_stable_owner(self.clipboard);
        let observed_text = observation.text.ok();

        if let Some(current) = &self.current {
            if current.owner.is_some()
                && current.owner == observation.owner
                && observed_text.as_deref() == Some(current.text.as_str())
            {
                return OwnershipStatus::Owned;
            }
        }

        if self.pending_write.as_ref().is_some_and(|pending| {
            pending.kind == OwnedTextKind::Sentinel
                && observed_text.as_deref() == Some(pending.text.as_str())
        }) {
            return OwnershipStatus::Owned;
        }

        if observation.owner.is_some() {
            OwnershipStatus::Foreign
        } else {
            OwnershipStatus::Uncertain
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

    fn owner_token(&mut self) -> Option<ClipboardOwnerToken> {
        self.clipboard.owner_token()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Clone)]
    struct SharedClipboardState {
        text: Rc<RefCell<Option<String>>>,
        owner: Rc<RefCell<Option<u32>>>,
        owner_reads: Rc<RefCell<VecDeque<Option<u32>>>>,
        owner_probe_available: Rc<RefCell<bool>>,
    }

    impl SharedClipboardState {
        fn with_text(text: &str) -> Self {
            Self {
                text: Rc::new(RefCell::new(Some(text.to_owned()))),
                owner: Rc::new(RefCell::new(Some(7))),
                owner_reads: Rc::new(RefCell::new(VecDeque::new())),
                owner_probe_available: Rc::new(RefCell::new(true)),
            }
        }

        fn replace_text(&self, text: &str, owner: u32) {
            *self.text.borrow_mut() = Some(text.to_owned());
            *self.owner.borrow_mut() = Some(owner);
        }

        fn text(&self) -> Option<String> {
            self.text.borrow().clone()
        }

        fn queue_owner_reads(&self, owners: impl IntoIterator<Item = Option<u32>>) {
            self.owner_reads.borrow_mut().extend(owners);
        }

        fn disable_owner_probe(&self) {
            *self.owner_probe_available.borrow_mut() = false;
        }
    }

    struct SharedClipboard {
        state: SharedClipboardState,
    }

    impl ClipboardAccess for SharedClipboard {
        fn get_text(&mut self) -> Result<String, SelectedTextError> {
            self.state.text().ok_or_else(|| {
                SelectedTextError::ClipboardRead(arboard::Error::ContentNotAvailable)
            })
        }

        fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
            self.state.replace_text(value, 11);
            Ok(())
        }

        fn clear(&mut self) -> Result<(), SelectedTextError> {
            *self.state.text.borrow_mut() = None;
            Ok(())
        }

        fn owner_token(&mut self) -> Option<ClipboardOwnerToken> {
            if !*self.state.owner_probe_available.borrow() {
                return None;
            }
            if let Some(owner) = self.state.owner_reads.borrow_mut().pop_front() {
                return owner.map(ClipboardOwnerToken);
            }
            self.state.owner.borrow().map(ClipboardOwnerToken)
        }
    }

    #[test]
    fn foreign_text_wins_before_restore() {
        let state = SharedClipboardState::with_text("previous");
        let mut clipboard = SharedClipboard {
            state: state.clone(),
        };
        let mut transaction = ClipboardTransaction::begin(&mut clipboard);
        transaction
            .write_operation_text(OwnedTextKind::Sentinel, "unique sentinel")
            .unwrap();
        transaction
            .write_operation_text(OwnedTextKind::Converted, "Привет")
            .unwrap();

        state.replace_text("foreign", 22);
        let disposition = transaction.finish_success();

        assert_eq!(disposition, ClipboardDisposition::ExternalChangePreserved);
        assert_eq!(state.text().as_deref(), Some("foreign"));
    }

    #[test]
    fn same_text_from_different_owner_wins_before_restore() {
        let state = SharedClipboardState::with_text("previous");
        let mut clipboard = SharedClipboard {
            state: state.clone(),
        };
        let mut transaction = ClipboardTransaction::begin(&mut clipboard);
        transaction
            .write_operation_text(OwnedTextKind::Converted, "Привет")
            .unwrap();

        state.replace_text("Привет", 22);
        let disposition = transaction.finish_success();

        assert_eq!(disposition, ClipboardDisposition::ExternalChangePreserved);
        assert_eq!(state.text().as_deref(), Some("Привет"));
    }

    #[test]
    fn owner_change_during_observation_skips_restore() {
        let state = SharedClipboardState::with_text("previous");
        let mut clipboard = SharedClipboard {
            state: state.clone(),
        };
        let mut transaction = ClipboardTransaction::begin(&mut clipboard);
        transaction
            .write_operation_text(OwnedTextKind::Converted, "Привет")
            .unwrap();
        state.queue_owner_reads([Some(11), Some(22)]);

        let disposition = transaction.finish_success();

        assert_eq!(disposition, ClipboardDisposition::ExternalChangePreserved);
        assert_eq!(state.text().as_deref(), Some("Привет"));
    }

    #[test]
    fn unavailable_owner_probe_skips_restore() {
        let state = SharedClipboardState::with_text("previous");
        let mut clipboard = SharedClipboard {
            state: state.clone(),
        };
        let mut transaction = ClipboardTransaction::begin(&mut clipboard);
        transaction
            .write_operation_text(OwnedTextKind::Converted, "Привет")
            .unwrap();
        state.disable_owner_probe();

        let disposition = transaction.finish_success();

        assert_eq!(disposition, ClipboardDisposition::ExternalChangePreserved);
        assert_eq!(state.text().as_deref(), Some("Привет"));
    }
}
