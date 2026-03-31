use super::engine::{ConversionOutcome, LayoutConversionEngine};
use super::SelectedTextSwitchResult;
use crate::daemon::keyboard::{KeyboardController, ModifierState};
use crate::error::{SelectedTextError, SwitcherError};
use arboard::Clipboard;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COPY_POLL_INTERVAL: Duration = Duration::from_millis(10);
const COPY_TIMEOUT: Duration = Duration::from_millis(350);
const PASTE_SETTLE_INTERVAL: Duration = Duration::from_millis(5);
const PASTE_SETTLE_TIMEOUT: Duration = Duration::from_millis(35);

pub(super) trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String, SelectedTextError>;
    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError>;
    fn clear(&mut self) -> Result<(), SelectedTextError>;
}

pub(super) trait SelectionTransport {
    fn copy_selection(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError>;
    fn paste_selection(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError>;
}

pub(super) struct SystemClipboard {
    inner: Clipboard,
}

impl SystemClipboard {
    pub(super) fn new() -> Result<Self, SelectedTextError> {
        Clipboard::new()
            .map(|inner| Self { inner })
            .map_err(SelectedTextError::ClipboardUnavailable)
    }
}

impl ClipboardAccess for SystemClipboard {
    fn get_text(&mut self) -> Result<String, SelectedTextError> {
        self.inner
            .get_text()
            .map_err(SelectedTextError::ClipboardRead)
    }

    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
        self.inner
            .set_text(value.to_string())
            .map_err(SelectedTextError::ClipboardWrite)
    }

    fn clear(&mut self) -> Result<(), SelectedTextError> {
        self.inner
            .clear()
            .map_err(SelectedTextError::ClipboardClear)
    }
}

impl SelectionTransport for KeyboardController {
    fn copy_selection(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.send_copy_shortcut(modifiers)
    }

    fn paste_selection(&mut self, modifiers: ModifierState) -> Result<(), SwitcherError> {
        self.send_paste_shortcut(modifiers)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ClipboardSnapshot {
    Text(String),
    Unavailable,
}

enum CopyOutcome {
    SelectedText(String),
    TimedOut,
}

#[derive(Default)]
pub(super) struct SelectedTextOperation;

impl SelectedTextOperation {
    pub(super) fn execute(
        &self,
        clipboard: &mut impl ClipboardAccess,
        transport: &mut impl SelectionTransport,
        converter: &LayoutConversionEngine,
        modifiers: ModifierState,
    ) -> Result<SelectedTextSwitchResult, SwitcherError> {
        let previous_clipboard = snapshot_clipboard(clipboard);
        let sentinel = unique_clipboard_sentinel();

        clipboard.set_text(&sentinel)?;
        transport.copy_selection(modifiers)?;

        let selected_text = match wait_for_copied_text(clipboard, &sentinel)? {
            CopyOutcome::SelectedText(text) => text,
            CopyOutcome::TimedOut => {
                let _ = restore_clipboard(clipboard, &previous_clipboard, None);
                return Ok(SelectedTextSwitchResult::NoSelectedText);
            }
        };

        let Some(ConversionOutcome {
            converted_text,
            direction,
        }) = converter.convert_selected_text(&selected_text)
        else {
            let _ = restore_clipboard(clipboard, &previous_clipboard, Some(&selected_text));
            return Ok(SelectedTextSwitchResult::NotConvertible);
        };

        clipboard.set_text(&converted_text)?;
        transport.paste_selection(modifiers)?;
        wait_for_paste_settle();

        let clipboard_restored =
            restore_clipboard(clipboard, &previous_clipboard, Some(&converted_text));

        Ok(SelectedTextSwitchResult::Replaced {
            direction,
            clipboard_restored,
        })
    }
}

fn snapshot_clipboard(clipboard: &mut impl ClipboardAccess) -> ClipboardSnapshot {
    match clipboard.get_text() {
        Ok(text) => ClipboardSnapshot::Text(text),
        Err(_) => ClipboardSnapshot::Unavailable,
    }
}

fn wait_for_copied_text(
    clipboard: &mut impl ClipboardAccess,
    sentinel: &str,
) -> Result<CopyOutcome, SelectedTextError> {
    let started = Instant::now();

    loop {
        match clipboard.get_text() {
            Ok(text) if text != sentinel => return Ok(CopyOutcome::SelectedText(text)),
            Ok(_) => {}
            Err(_) => {}
        }

        if started.elapsed() >= COPY_TIMEOUT {
            return Ok(CopyOutcome::TimedOut);
        }

        thread::sleep(COPY_POLL_INTERVAL);
    }
}

fn wait_for_paste_settle() {
    // The target application does not provide an acknowledgment that paste has landed,
    // so we use a very short bounded settle window before restoring the clipboard.
    let started = Instant::now();
    while started.elapsed() < PASTE_SETTLE_TIMEOUT {
        thread::sleep(PASTE_SETTLE_INTERVAL);
    }
}

fn restore_clipboard(
    clipboard: &mut impl ClipboardAccess,
    previous: &ClipboardSnapshot,
    fallback_text: Option<&str>,
) -> bool {
    match previous {
        ClipboardSnapshot::Text(text) => clipboard.set_text(text).is_ok(),
        ClipboardSnapshot::Unavailable => {
            if clipboard.clear().is_ok() {
                true
            } else if let Some(text) = fallback_text {
                clipboard.set_text(text).is_ok()
            } else {
                false
            }
        }
    }
}

fn unique_clipboard_sentinel() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("__OPEN_SWITCHER_SELECTION_SENTINEL_{nanos}__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::selected_text::engine::ConversionDirection;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct TestClipboard {
        current_text: Option<String>,
        pending_reads: VecDeque<Result<String, SelectedTextError>>,
        clear_calls: usize,
        clear_should_fail: bool,
    }

    impl TestClipboard {
        fn with_current_text(text: impl Into<String>) -> Self {
            Self {
                current_text: Some(text.into()),
                pending_reads: VecDeque::new(),
                clear_calls: 0,
                clear_should_fail: false,
            }
        }

        fn queue_read(&mut self, value: Result<String, SelectedTextError>) {
            self.pending_reads.push_back(value);
        }
    }

    impl ClipboardAccess for TestClipboard {
        fn get_text(&mut self) -> Result<String, SelectedTextError> {
            if let Some(result) = self.pending_reads.pop_front() {
                return result;
            }

            self.current_text.clone().ok_or_else(|| {
                SelectedTextError::ClipboardRead(arboard::Error::Unknown {
                    description: "clipboard is empty".into(),
                })
            })
        }

        fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
            self.current_text = Some(value.to_string());
            Ok(())
        }

        fn clear(&mut self) -> Result<(), SelectedTextError> {
            self.clear_calls += 1;
            if self.clear_should_fail {
                return Err(SelectedTextError::ClipboardClear(arboard::Error::Unknown {
                    description: "clear failed".into(),
                }));
            }
            self.current_text = None;
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestTransport {
        copied: bool,
        pasted: bool,
    }

    impl SelectionTransport for TestTransport {
        fn copy_selection(&mut self, _: ModifierState) -> Result<(), SwitcherError> {
            self.copied = true;
            Ok(())
        }

        fn paste_selection(&mut self, _: ModifierState) -> Result<(), SwitcherError> {
            self.pasted = true;
            Ok(())
        }
    }

    #[test]
    fn polls_until_clipboard_changes_after_copy() {
        let converter = LayoutConversionEngine;
        let operation = SelectedTextOperation;
        let mut clipboard = TestClipboard::with_current_text("previous");
        clipboard.queue_read(Ok("previous".into()));
        clipboard.queue_read(Err(SelectedTextError::ClipboardRead(
            arboard::Error::ContentNotAvailable,
        )));
        clipboard.queue_read(Err(SelectedTextError::ClipboardRead(
            arboard::Error::ClipboardOccupied,
        )));
        clipboard.queue_read(Ok("Ghbdtn".into()));
        let mut transport = TestTransport::default();

        let result = operation
            .execute(
                &mut clipboard,
                &mut transport,
                &converter,
                ModifierState::default(),
            )
            .unwrap();

        assert!(transport.copied);
        assert!(transport.pasted);
        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_restored: true,
            }
        );
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn clears_clipboard_when_previous_text_was_unavailable() {
        let converter = LayoutConversionEngine;
        let operation = SelectedTextOperation;
        let mut clipboard = TestClipboard {
            current_text: None,
            pending_reads: VecDeque::from([
                Err(SelectedTextError::ClipboardRead(arboard::Error::Unknown {
                    description: "clipboard unavailable".into(),
                })),
                Ok("Ghbdtn".into()),
            ]),
            clear_calls: 0,
            clear_should_fail: false,
        };
        let mut transport = TestTransport::default();

        let result = operation
            .execute(
                &mut clipboard,
                &mut transport,
                &converter,
                ModifierState::default(),
            )
            .unwrap();

        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_restored: true,
            }
        );
        assert_eq!(clipboard.clear_calls, 1);
        assert_eq!(clipboard.current_text, None);
    }

    #[test]
    fn falls_back_to_converted_text_when_clear_is_unavailable() {
        let converter = LayoutConversionEngine;
        let operation = SelectedTextOperation;
        let mut clipboard = TestClipboard {
            current_text: None,
            pending_reads: VecDeque::from([
                Err(SelectedTextError::ClipboardRead(arboard::Error::Unknown {
                    description: "clipboard unavailable".into(),
                })),
                Ok("Ghbdtn".into()),
            ]),
            clear_calls: 0,
            clear_should_fail: true,
        };
        let mut transport = TestTransport::default();

        let result = operation
            .execute(
                &mut clipboard,
                &mut transport,
                &converter,
                ModifierState::default(),
            )
            .unwrap();

        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_restored: true,
            }
        );
        assert_eq!(clipboard.current_text.as_deref(), Some("Привет"));
    }
}
