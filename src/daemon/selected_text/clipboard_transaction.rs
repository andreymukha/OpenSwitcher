use super::ClipboardDisposition;
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

pub(super) struct ClipboardTransaction<'a, C: ClipboardAccess> {
    clipboard: &'a mut C,
    original: ClipboardSnapshot,
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
        }
    }

    pub(super) fn original(&self) -> &ClipboardSnapshot {
        &self.original
    }

    pub(super) fn write_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
        self.clipboard.set_text(value)
    }

    pub(super) fn finish_success(self) -> ClipboardDisposition {
        match self.original {
            ClipboardSnapshot::RestorableText(previous) => {
                if self.clipboard.set_text(&previous).is_ok() {
                    ClipboardDisposition::Restored
                } else {
                    ClipboardDisposition::RestoreFailed
                }
            }
            ClipboardSnapshot::Unrestorable => ClipboardDisposition::ConvertedTextKept,
        }
    }

    pub(super) fn finish_no_selected_text(self) {
        match self.original {
            ClipboardSnapshot::RestorableText(previous) => {
                let _ = self.clipboard.set_text(&previous);
            }
            ClipboardSnapshot::Unrestorable => {
                let _ = self.clipboard.clear();
            }
        }
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
