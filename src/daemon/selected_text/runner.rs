use super::{log_selected_text_debug, SelectedTextSwitchResult, SelectedTextSwitchService};
use crate::daemon::keyboard::SelectionKeyboardTransport;
use crate::error::SwitcherError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

#[derive(Clone)]
pub struct SelectedTextJobRunner {
    command_tx: mpsc::Sender<()>,
    in_progress: Arc<AtomicBool>,
}

impl SelectedTextJobRunner {
    pub fn new(mut transport: SelectionKeyboardTransport) -> Result<Self, SwitcherError> {
        let (command_tx, command_rx) = mpsc::channel::<()>();
        let in_progress = Arc::new(AtomicBool::new(false));
        let worker_in_progress = Arc::clone(&in_progress);

        thread::spawn(move || {
            let service = SelectedTextSwitchService::default();

            for () in command_rx {
                let result = service.switch_selected_text(&mut transport);
                log_selected_text_job_result(&result);
                worker_in_progress.store(false, Ordering::SeqCst);
            }
        });

        Ok(Self {
            command_tx,
            in_progress,
        })
    }

    pub fn try_start(&self) -> Result<bool, SwitcherError> {
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
