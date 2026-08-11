use super::clipboard_transaction::OwnedTextKind;
use super::clipboard_transaction::{ClipboardAccess, ClipboardSnapshot, ClipboardTransaction};
use super::engine::{ConversionOutcome, LayoutConversionEngine};
use super::SelectedTextSwitchResult;
use super::{log_selected_text_debug, summarize_text};
use crate::daemon::keyboard::SelectionKeyboardTransport;
use crate::error::{SelectedTextError, SwitcherError};
use arboard::Clipboard;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COPY_POLL_INTERVAL: Duration = Duration::from_millis(10);
const COPY_TIMEOUT: Duration = Duration::from_millis(900);
const COPY_CHANGE_STABLE_FOR: Duration = Duration::from_millis(60);
const COPY_MIN_ACCEPT_DELAY: Duration = Duration::from_millis(120);
const SENTINEL_CONFIRM_TIMEOUT: Duration = Duration::from_millis(120);
const PASTE_SETTLE_TIMEOUT: Duration = Duration::from_millis(300);

pub(super) trait SelectionTransport {
    fn copy_selection(&mut self) -> Result<(), SwitcherError>;
    fn paste_selection(&mut self) -> Result<(), SwitcherError>;
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

impl SelectionTransport for SelectionKeyboardTransport {
    fn copy_selection(&mut self) -> Result<(), SwitcherError> {
        self.send_copy_shortcut()
    }

    fn paste_selection(&mut self) -> Result<(), SwitcherError> {
        self.send_paste_shortcut()
    }
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
    ) -> Result<SelectedTextSwitchResult, SwitcherError> {
        let mut transaction = ClipboardTransaction::begin(clipboard);
        let previous_clipboard = transaction.original().clone();
        let sentinel = unique_clipboard_sentinel();

        log_selected_text_debug(
            "start",
            &format!(
                "previous_clipboard={} sentinel={sentinel}",
                describe_clipboard_snapshot(&previous_clipboard)
            ),
        );

        transaction.write_operation_text(OwnedTextKind::Sentinel, &sentinel)?;
        log_selected_text_debug("clipboard-set-sentinel", &format!("sentinel={sentinel}"));
        let sentinel_confirmed = wait_for_clipboard_sentinel(&mut transaction, &sentinel);
        transport.copy_selection()?;
        log_selected_text_debug("copy-sent", "selection copy shortcut dispatched");

        let selected_text = match wait_for_copied_text(
            &mut transaction,
            &sentinel,
            &previous_clipboard,
            sentinel_confirmed,
        )? {
            CopyOutcome::SelectedText(text) => {
                log_selected_text_debug(
                    "copy-received",
                    &format!("selected_text={}", summarize_text(&text)),
                );
                transaction.adopt_selected_text(&text);
                text
            }
            CopyOutcome::TimedOut => {
                log_selected_text_debug("copy-timeout", "clipboard did not change before timeout");
                transaction.finish_no_selected_text();
                return Ok(SelectedTextSwitchResult::NoSelectedText);
            }
        };

        let ConversionOutcome {
            converted_text,
            direction,
        } = converter.convert_selected_text(&selected_text);

        log_selected_text_debug(
            "convert-success",
            &format!(
                "direction={direction:?} converted_text={}",
                summarize_text(&converted_text)
            ),
        );
        transaction.write_operation_text(OwnedTextKind::Converted, &converted_text)?;
        log_selected_text_debug("clipboard-set-converted", &summarize_text(&converted_text));
        transport.paste_selection()?;
        log_selected_text_debug("paste-sent", "selection paste shortcut dispatched");
        wait_for_paste_settle();

        let clipboard_disposition = transaction.finish_success();
        log_selected_text_debug(
            "restore-finished",
            &format!("clipboard_disposition={clipboard_disposition:?}"),
        );

        Ok(SelectedTextSwitchResult::Replaced {
            direction,
            clipboard_disposition,
        })
    }
}

fn wait_for_clipboard_sentinel(clipboard: &mut impl ClipboardAccess, sentinel: &str) -> bool {
    let started = Instant::now();
    let mut attempt = 0usize;

    loop {
        attempt += 1;
        match clipboard.get_text() {
            Ok(text) if text == sentinel => {
                log_selected_text_debug(
                    "sentinel-confirmed",
                    &format!(
                        "attempt={attempt} elapsed_ms={}",
                        started.elapsed().as_millis()
                    ),
                );
                return true;
            }
            Ok(text) => {
                if attempt == 1 || attempt.is_multiple_of(10) {
                    log_selected_text_debug(
                        "sentinel-poll-mismatch",
                        &format!(
                            "attempt={attempt} elapsed_ms={} text={}",
                            started.elapsed().as_millis(),
                            summarize_text(&text)
                        ),
                    );
                }
            }
            Err(error) => {
                if attempt == 1 || attempt.is_multiple_of(10) {
                    log_selected_text_debug(
                        "sentinel-poll-error",
                        &format!(
                            "attempt={attempt} elapsed_ms={} error={error}",
                            started.elapsed().as_millis()
                        ),
                    );
                }
            }
        }

        if started.elapsed() >= SENTINEL_CONFIRM_TIMEOUT {
            log_selected_text_debug(
                "sentinel-timeout",
                &format!("elapsed_ms={}", started.elapsed().as_millis()),
            );
            return false;
        }

        thread::sleep(COPY_POLL_INTERVAL);
    }
}

fn wait_for_copied_text(
    clipboard: &mut impl ClipboardAccess,
    sentinel: &str,
    previous_clipboard: &ClipboardSnapshot,
    sentinel_confirmed_before_copy: bool,
) -> Result<CopyOutcome, SelectedTextError> {
    let started = Instant::now();
    let mut attempt = 0usize;
    let mut candidate_text: Option<String> = None;
    let mut candidate_changed_at: Option<Instant> = None;
    let mut observed_sentinel = sentinel_confirmed_before_copy;

    loop {
        attempt += 1;
        match clipboard.get_text() {
            Ok(text) if text != sentinel => {
                let is_new_candidate = candidate_text.as_deref() != Some(text.as_str());
                if is_new_candidate {
                    candidate_changed_at = Some(Instant::now());
                    candidate_text = Some(text.clone());
                    log_selected_text_debug(
                        "copy-poll-candidate",
                        &format!(
                            "attempt={attempt} elapsed_ms={} text={}",
                            started.elapsed().as_millis(),
                            summarize_text(&text)
                        ),
                    );
                }

                let stable_for = candidate_changed_at
                    .map(|instant| instant.elapsed())
                    .unwrap_or_default();
                let elapsed = started.elapsed();
                let matches_previous = matches!(
                    previous_clipboard,
                    ClipboardSnapshot::RestorableText(previous) if previous == &text
                );
                let should_ignore =
                    elapsed < COPY_MIN_ACCEPT_DELAY || (matches_previous && !observed_sentinel);

                if should_ignore && (is_new_candidate || attempt == 1 || attempt.is_multiple_of(20))
                {
                    log_selected_text_debug(
                            "copy-poll-ignore",
                            &format!(
                                "attempt={attempt} elapsed_ms={} stable_ms={} observed_sentinel={} matches_previous={} text={}",
                                elapsed.as_millis(),
                                stable_for.as_millis(),
                                observed_sentinel,
                                matches_previous,
                                summarize_text(&text)
                            ),
                        );
                }

                if !should_ignore && stable_for >= COPY_CHANGE_STABLE_FOR {
                    log_selected_text_debug(
                        "copy-poll-change",
                        &format!(
                            "attempt={attempt} elapsed_ms={} stable_ms={} observed_sentinel={} text={}",
                            elapsed.as_millis(),
                            stable_for.as_millis(),
                            observed_sentinel,
                            summarize_text(&text)
                        ),
                    );
                    return Ok(CopyOutcome::SelectedText(text));
                }
            }
            Ok(_) => {
                observed_sentinel = true;
                candidate_text = None;
                candidate_changed_at = None;
                if attempt == 1 || attempt.is_multiple_of(20) {
                    log_selected_text_debug(
                        "copy-poll-wait",
                        &format!(
                            "attempt={attempt} elapsed_ms={} clipboard=sentinel",
                            started.elapsed().as_millis()
                        ),
                    );
                }
            }
            Err(error) => {
                if attempt == 1 || attempt.is_multiple_of(20) {
                    log_selected_text_debug(
                        "copy-poll-error",
                        &format!(
                            "attempt={attempt} elapsed_ms={} error={error}",
                            started.elapsed().as_millis()
                        ),
                    );
                }
            }
        }

        if started.elapsed() >= COPY_TIMEOUT {
            if let Some(text) = candidate_text {
                let matches_previous = matches!(
                    previous_clipboard,
                    ClipboardSnapshot::RestorableText(previous) if previous == &text
                );
                if matches_previous && !observed_sentinel {
                    log_selected_text_debug(
                        "copy-poll-timeout-stale",
                        &format!(
                            "elapsed_ms={} text={} matches_previous=true",
                            started.elapsed().as_millis(),
                            summarize_text(&text)
                        ),
                    );
                    return Ok(CopyOutcome::TimedOut);
                }
                log_selected_text_debug(
                    "copy-poll-timeout-with-candidate",
                    &format!(
                        "elapsed_ms={} text={}",
                        started.elapsed().as_millis(),
                        summarize_text(&text)
                    ),
                );
                return Ok(CopyOutcome::SelectedText(text));
            }
            return Ok(CopyOutcome::TimedOut);
        }

        thread::sleep(COPY_POLL_INTERVAL);
    }
}

fn wait_for_paste_settle() {
    // The target application does not provide an acknowledgment that paste has landed.
    // On X11 the clipboard owner may be queried a bit later, so we keep a bounded
    // grace window before restoring the previous clipboard contents.
    thread::sleep(PASTE_SETTLE_TIMEOUT);
}

fn unique_clipboard_sentinel() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("__OPEN_SWITCHER_SELECTION_SENTINEL_{nanos}__")
}

fn describe_clipboard_snapshot(snapshot: &ClipboardSnapshot) -> String {
    match snapshot {
        ClipboardSnapshot::RestorableText(text) => {
            format!("RestorableText({})", summarize_text(text))
        }
        ClipboardSnapshot::Unrestorable => "Unrestorable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::selected_text::engine::ConversionDirection;
    use crate::daemon::selected_text::ClipboardDisposition;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct TestClipboard {
        current_text: Option<String>,
        pending_reads: VecDeque<Result<String, SelectedTextError>>,
        write_calls: usize,
        clear_calls: usize,
        fail_write_call: Option<usize>,
        fail_write_after_mutation: bool,
        clear_should_fail: bool,
    }

    impl TestClipboard {
        fn with_current_text(text: impl Into<String>) -> Self {
            Self {
                current_text: Some(text.into()),
                pending_reads: VecDeque::new(),
                write_calls: 0,
                clear_calls: 0,
                fail_write_call: None,
                fail_write_after_mutation: false,
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
                if let Ok(text) = &result {
                    self.current_text = Some(text.clone());
                }
                return result;
            }

            self.current_text.clone().ok_or_else(|| {
                SelectedTextError::ClipboardRead(arboard::Error::Unknown {
                    description: "clipboard is empty".into(),
                })
            })
        }

        fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
            self.write_calls += 1;
            let should_fail = self.fail_write_call == Some(self.write_calls);
            if should_fail && !self.fail_write_after_mutation {
                return Err(clipboard_write_error("write failed before mutation"));
            }
            self.current_text = Some(value.to_string());
            if should_fail {
                return Err(clipboard_write_error("write failed after mutation"));
            }
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
        copy_should_fail: bool,
        paste_should_fail: bool,
        copy_should_panic: bool,
        paste_should_panic: bool,
    }

    impl SelectionTransport for TestTransport {
        fn copy_selection(&mut self) -> Result<(), SwitcherError> {
            assert!(!self.copy_should_panic, "copy panic");
            if self.copy_should_fail {
                return Err(std::io::Error::other("copy failed").into());
            }
            self.copied = true;
            Ok(())
        }

        fn paste_selection(&mut self) -> Result<(), SwitcherError> {
            assert!(!self.paste_should_panic, "paste panic");
            if self.paste_should_fail {
                return Err(std::io::Error::other("paste failed").into());
            }
            self.pasted = true;
            Ok(())
        }
    }

    fn clipboard_write_error(description: &'static str) -> SelectedTextError {
        SelectedTextError::ClipboardWrite(arboard::Error::Unknown {
            description: description.into(),
        })
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
            .execute(&mut clipboard, &mut transport, &converter)
            .unwrap();

        assert!(transport.copied);
        assert!(transport.pasted);
        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_disposition: ClipboardDisposition::Restored,
            }
        );
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn waits_for_stable_selection_after_transient_clipboard_change() {
        let converter = LayoutConversionEngine;
        let operation = SelectedTextOperation;
        let mut clipboard = TestClipboard::with_current_text("previous");
        clipboard.queue_read(Ok("previous".into()));
        clipboard.queue_read(Ok("Ghbdtn".into()));
        let mut transport = TestTransport::default();

        let result = operation
            .execute(&mut clipboard, &mut transport, &converter)
            .unwrap();

        assert!(transport.copied);
        assert!(transport.pasted);
        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_disposition: ClipboardDisposition::Restored,
            }
        );
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn ignores_previous_clipboard_text_until_real_copy_arrives() {
        let converter = LayoutConversionEngine;
        let operation = SelectedTextOperation;
        let mut clipboard = TestClipboard::with_current_text("old clipboard");
        clipboard.queue_read(Ok("old clipboard".into()));
        clipboard.queue_read(Ok("old clipboard".into()));
        clipboard.queue_read(Ok("old clipboard".into()));
        clipboard.queue_read(Ok("Ghbdtn".into()));
        let mut transport = TestTransport::default();

        let result = operation
            .execute(&mut clipboard, &mut transport, &converter)
            .unwrap();

        assert!(transport.copied);
        assert!(transport.pasted);
        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_disposition: ClipboardDisposition::Restored,
            }
        );
        assert_eq!(clipboard.current_text.as_deref(), Some("old clipboard"));
    }

    #[test]
    fn unrestorable_clipboard_keeps_converted_text_without_clear() {
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
            ..TestClipboard::default()
        };
        let mut transport = TestTransport::default();

        let result = operation
            .execute(&mut clipboard, &mut transport, &converter)
            .unwrap();

        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_disposition: ClipboardDisposition::ConvertedTextKept,
            }
        );
        assert_eq!(clipboard.clear_calls, 0);
        assert_eq!(clipboard.current_text.as_deref(), Some("Привет"));
    }

    #[test]
    fn unrestorable_clipboard_does_not_depend_on_clear_support() {
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
            clear_should_fail: true,
            ..TestClipboard::default()
        };
        let mut transport = TestTransport::default();

        let result = operation
            .execute(&mut clipboard, &mut transport, &converter)
            .unwrap();

        assert_eq!(
            result,
            SelectedTextSwitchResult::Replaced {
                direction: ConversionDirection::EnToRu,
                clipboard_disposition: ClipboardDisposition::ConvertedTextKept,
            }
        );
        assert_eq!(clipboard.clear_calls, 0);
        assert_eq!(clipboard.current_text.as_deref(), Some("Привет"));
    }

    #[test]
    fn times_out_when_clipboard_never_changes_from_previous_text() {
        let converter = LayoutConversionEngine;
        let operation = SelectedTextOperation;
        let mut clipboard = TestClipboard::with_current_text("old clipboard");
        let mut transport = TestTransport::default();

        let started = Instant::now();
        let result = operation
            .execute(&mut clipboard, &mut transport, &converter)
            .unwrap();

        assert!(transport.copied);
        assert!(!transport.pasted);
        assert_eq!(result, SelectedTextSwitchResult::NoSelectedText);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(clipboard.current_text.as_deref(), Some("old clipboard"));
    }

    #[test]
    fn accepts_selected_text_matching_previous_clipboard_after_sentinel_was_observed() {
        let selected_text = "А терминал можно вынести отдельно ";
        let previous_clipboard = ClipboardSnapshot::RestorableText(selected_text.to_string());
        let mut clipboard = TestClipboard::default();
        clipboard.queue_read(Ok("sentinel".into()));
        clipboard.queue_read(Ok(selected_text.into()));

        let result =
            wait_for_copied_text(&mut clipboard, "sentinel", &previous_clipboard, true).unwrap();

        assert!(matches!(
            result,
            CopyOutcome::SelectedText(text) if text == selected_text
        ));
    }

    #[test]
    fn paste_settle_window_covers_delayed_x11_clipboard_requests() {
        assert!(PASTE_SETTLE_TIMEOUT >= Duration::from_millis(300));
    }

    #[test]
    fn copy_failure_restores_previous_text() {
        let mut clipboard = TestClipboard::with_current_text("previous");
        let mut transport = TestTransport {
            copy_should_fail: true,
            ..TestTransport::default()
        };

        let error = SelectedTextOperation
            .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
            .unwrap_err();

        assert_eq!(error.to_string(), "copy failed");
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn paste_failure_restores_previous_text() {
        let mut clipboard = TestClipboard::with_current_text("previous");
        clipboard.queue_read(Ok("previous".into()));
        clipboard.queue_read(Ok("Ghbdtn".into()));
        let mut transport = TestTransport {
            paste_should_fail: true,
            ..TestTransport::default()
        };

        let error = SelectedTextOperation
            .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
            .unwrap_err();

        assert_eq!(error.to_string(), "paste failed");
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn copy_panic_runs_drop_rollback() {
        let mut clipboard = TestClipboard::with_current_text("previous");
        let mut transport = TestTransport {
            copy_should_panic: true,
            ..TestTransport::default()
        };

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = SelectedTextOperation.execute(
                &mut clipboard,
                &mut transport,
                &LayoutConversionEngine,
            );
        }));

        assert!(unwind.is_err());
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn ambiguous_sentinel_write_failure_restores_previous_text() {
        let mut clipboard = TestClipboard::with_current_text("previous");
        clipboard.fail_write_call = Some(1);
        clipboard.fail_write_after_mutation = true;
        let mut transport = TestTransport::default();

        assert!(SelectedTextOperation
            .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
            .is_err());

        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn rollback_failure_preserves_primary_copy_error() {
        let mut clipboard = TestClipboard::with_current_text("previous");
        clipboard.fail_write_call = Some(2);
        let mut transport = TestTransport {
            copy_should_fail: true,
            ..TestTransport::default()
        };

        let error = SelectedTextOperation
            .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
            .unwrap_err();

        assert_eq!(error.to_string(), "copy failed");
        assert_eq!(clipboard.write_calls, 2);
    }

    #[test]
    fn paste_panic_runs_drop_rollback() {
        let mut clipboard = TestClipboard::with_current_text("previous");
        clipboard.queue_read(Ok("previous".into()));
        clipboard.queue_read(Ok("Ghbdtn".into()));
        let mut transport = TestTransport {
            paste_should_panic: true,
            ..TestTransport::default()
        };

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = SelectedTextOperation.execute(
                &mut clipboard,
                &mut transport,
                &LayoutConversionEngine,
            );
        }));

        assert!(unwind.is_err());
        assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
    }

    #[test]
    fn copy_failure_clears_owned_sentinel_when_original_is_unrestorable() {
        let mut clipboard = TestClipboard::default();
        let mut transport = TestTransport {
            copy_should_fail: true,
            ..TestTransport::default()
        };

        assert!(SelectedTextOperation
            .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
            .is_err());

        assert_eq!(clipboard.clear_calls, 1);
        assert_eq!(clipboard.current_text, None);
    }
}
