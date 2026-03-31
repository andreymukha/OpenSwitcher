mod clipboard;
mod engine;

use crate::daemon::keyboard::{KeyboardController, ModifierState};
use crate::error::SwitcherError;
use clipboard::{SelectedTextOperation, SystemClipboard};
use engine::LayoutConversionEngine;

pub use engine::ConversionDirection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedTextSwitchResult {
    Replaced {
        direction: ConversionDirection,
        clipboard_restored: bool,
    },
    NoSelectedText,
    NotConvertible,
}

#[derive(Default)]
pub struct SelectedTextSwitchService {
    operation: SelectedTextOperation,
    converter: LayoutConversionEngine,
}

impl SelectedTextSwitchService {
    pub fn switch_selected_text(
        &self,
        keyboard: &mut KeyboardController,
        modifiers: ModifierState,
    ) -> Result<SelectedTextSwitchResult, SwitcherError> {
        let mut clipboard = SystemClipboard::new()?;
        self.operation
            .execute(&mut clipboard, keyboard, &self.converter, modifiers)
    }
}
