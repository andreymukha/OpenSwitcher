mod clipboard;
mod clipboard_transaction;
mod debug;
mod engine;
mod runner;

use crate::daemon::keyboard::SelectionKeyboardTransport;
use crate::error::SwitcherError;
use clipboard::{SelectedTextOperation, SystemClipboard};
use engine::LayoutConversionEngine;

pub(crate) use debug::log_selected_text_debug;
pub(crate) use debug::summarize_text;
pub use engine::ConversionDirection;
pub use runner::SelectedTextJobRunner;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardDisposition {
    Restored,
    ConvertedTextKept,
    ExternalChangePreserved,
    RestoreFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedTextSwitchResult {
    Replaced {
        direction: ConversionDirection,
        clipboard_disposition: ClipboardDisposition,
    },
    NoSelectedText,
}

#[derive(Default)]
pub struct SelectedTextSwitchService {
    operation: SelectedTextOperation,
    converter: LayoutConversionEngine,
}

impl SelectedTextSwitchService {
    pub fn switch_selected_text(
        &self,
        transport: &mut SelectionKeyboardTransport,
    ) -> Result<SelectedTextSwitchResult, SwitcherError> {
        let mut clipboard = SystemClipboard::new()?;
        self.operation
            .execute(&mut clipboard, transport, &self.converter)
    }
}
