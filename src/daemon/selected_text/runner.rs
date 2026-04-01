use super::{log_selected_text_debug, SelectedTextSwitchResult, SelectedTextSwitchService};
use crate::daemon::keyboard::log_input_debug;
use crate::daemon::keyboard::SelectionKeyboardTransport;
use crate::error::SwitcherError;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

#[derive(Clone)]
pub struct SelectedTextJobRunner {
    command_tx: mpsc::Sender<()>,
    in_progress: Arc<AtomicBool>,
    worker_alive: Arc<AtomicBool>,
}

impl SelectedTextJobRunner {
    pub fn new(mut transport: SelectionKeyboardTransport) -> Result<Self, SwitcherError> {
        let (command_tx, command_rx) = mpsc::channel::<()>();
        let in_progress = Arc::new(AtomicBool::new(false));
        let worker_in_progress = Arc::clone(&in_progress);
        let worker_alive = Arc::new(AtomicBool::new(true));
        let worker_alive_flag = Arc::clone(&worker_alive);

        thread::spawn(move || {
            log_input_debug("selected-text-worker-start", "worker thread started");
            let service = SelectedTextSwitchService::default();

            for () in command_rx {
                log_input_debug("selected-text-worker-job-start", "worker received selected-text job");
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    service.switch_selected_text(&mut transport)
                }));

                match result {
                    Ok(result) => {
                        log_selected_text_job_result(&result);
                        log_input_debug("selected-text-worker-job-finish", "worker completed selected-text job");
                    }
                    Err(payload) => {
                        let reason = if let Some(text) = payload.downcast_ref::<&str>() {
                            *text
                        } else if let Some(text) = payload.downcast_ref::<String>() {
                            text.as_str()
                        } else {
                            "unknown panic payload"
                        };
                        log_selected_text_debug("worker-panic", &format!("reason={reason}"));
                        log_input_debug("selected-text-worker-panic", &format!("reason={reason}"));
                        eprintln!("[selected-text] Worker panic: {reason}");
                    }
                }

                worker_in_progress.store(false, Ordering::SeqCst);
            }

            worker_alive_flag.store(false, Ordering::SeqCst);
            worker_in_progress.store(false, Ordering::SeqCst);
            log_input_debug("selected-text-worker-stop", "worker thread stopped");
        });

        Ok(Self {
            command_tx,
            in_progress,
            worker_alive,
        })
    }

    pub fn try_start(&self) -> Result<bool, SwitcherError> {
        if !self.worker_alive.load(Ordering::SeqCst) {
            return Err(SwitcherError::SelectedTextWorkerDisconnected);
        }

        if self
            .in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(false);
        }

        if self.command_tx.send(()).is_err() {
            self.in_progress.store(false, Ordering::SeqCst);
            return Err(SwitcherError::SelectedTextWorkerDisconnected);
        }

        Ok(true)
    }

    pub fn is_in_progress(&self) -> bool {
        self.in_progress.load(Ordering::SeqCst)
    }
}

fn log_selected_text_job_result(result: &Result<SelectedTextSwitchResult, SwitcherError>) {
    match result {
        Ok(SelectedTextSwitchResult::Replaced {
            clipboard_restored, ..
        }) => {
            log_selected_text_debug(
                "result",
                &format!("result=Replaced clipboard_restored={clipboard_restored}"),
            );
            if !clipboard_restored {
                eprintln!(
                    "[selected-text] Не удалось восстановить предыдущее содержимое буфера обмена."
                );
            }
        }
        Ok(SelectedTextSwitchResult::NoSelectedText) => {
            log_selected_text_debug("result", "result=NoSelectedText");
            eprintln!("[selected-text] Нет выделенного текста.");
        }
        Err(error) => {
            log_selected_text_debug("result", &format!("result=Error error={error}"));
            eprintln!("[selected-text] {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_flight_state_rejects_second_start_while_running() {
        let state = Arc::new(AtomicBool::new(false));

        assert!(state
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok());
        assert!(state
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err());

        state.store(false, Ordering::SeqCst);
        assert!(state
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok());
    }
}
