use super::{log_selected_text_debug, SelectedTextSwitchResult, SelectedTextSwitchService};
use crate::daemon::keyboard::log_input_debug;
use crate::daemon::keyboard::SelectionKeyboardTransport;
use crate::error::SwitcherError;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

const SELECTED_TEXT_WORKER_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

fn wait_for_worker_startup_ready(
    ready_rx: &mpsc::Receiver<()>,
    timeout: Duration,
) -> Result<(), SwitcherError> {
    match ready_rx.recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(SwitcherError::InputWorkerStartupTimedOut {
            worker: "selected-text-worker",
            timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SwitcherError::InputWorkerDisconnected {
            worker: "selected-text-worker",
        }),
    }
}

fn run_selected_text_command_loop(
    command_rx: mpsc::Receiver<()>,
    on_ready: impl FnOnce() -> Result<(), SwitcherError>,
    mut dispatch: impl FnMut() -> Result<(), SwitcherError>,
) -> Result<(), SwitcherError> {
    on_ready()?;
    for () in command_rx {
        dispatch()?;
    }
    Ok(())
}

struct SelectedTextWorkerStateGuard {
    alive: Arc<AtomicBool>,
    in_progress: Arc<AtomicBool>,
}

impl Drop for SelectedTextWorkerStateGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        self.in_progress.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct SelectedTextJobRunner {
    command_tx: mpsc::Sender<()>,
    in_progress: Arc<AtomicBool>,
    worker_alive: Arc<AtomicBool>,
}

impl SelectedTextJobRunner {
    pub fn new(mut transport: SelectionKeyboardTransport) -> Result<Self, SwitcherError> {
        let (command_tx, command_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let in_progress = Arc::new(AtomicBool::new(false));
        let worker_in_progress = Arc::clone(&in_progress);
        let worker_alive = Arc::new(AtomicBool::new(false));
        let worker_alive_flag = Arc::clone(&worker_alive);
        let service = SelectedTextSwitchService::default();

        thread::Builder::new()
            .name("open-switcher-selected-text".to_string())
            .spawn(move || {
                let _state_guard = SelectedTextWorkerStateGuard {
                    alive: Arc::clone(&worker_alive_flag),
                    in_progress: Arc::clone(&worker_in_progress),
                };
                log_input_debug("selected-text-worker-start", "worker thread started");
                let loop_result = run_selected_text_command_loop(
                    command_rx,
                    || {
                        worker_alive_flag.store(true, Ordering::SeqCst);
                        if ready_tx.send(()).is_err() {
                            worker_alive_flag.store(false, Ordering::SeqCst);
                            return Err(SwitcherError::InputWorkerDisconnected {
                                worker: "selected-text-worker",
                            });
                        }
                        Ok(())
                    },
                    || {
                        log_input_debug(
                            "selected-text-worker-job-start",
                            "worker received selected-text job",
                        );
                        let result = panic::catch_unwind(AssertUnwindSafe(|| {
                            service.switch_selected_text(&mut transport)
                        }));

                        match result {
                            Ok(result) => {
                                log_selected_text_job_result(&result);
                                log_input_debug(
                                    "selected-text-worker-job-finish",
                                    "worker completed selected-text job",
                                );
                            }
                            Err(payload) => {
                                let reason = if let Some(text) = payload.downcast_ref::<&str>() {
                                    *text
                                } else if let Some(text) = payload.downcast_ref::<String>() {
                                    text.as_str()
                                } else {
                                    "unknown panic payload"
                                };
                                log_selected_text_debug(
                                    "worker-panic",
                                    &format!("reason={reason}"),
                                );
                                log_input_debug(
                                    "selected-text-worker-panic",
                                    &format!("reason={reason}"),
                                );
                                eprintln!("[selected-text] Worker panic: {reason}");
                            }
                        }

                        worker_in_progress.store(false, Ordering::SeqCst);
                        Ok(())
                    },
                );
                if let Err(error) = loop_result {
                    log_input_debug("selected-text-worker-error", &format!("error={error}"));
                }
                log_input_debug("selected-text-worker-stop", "worker thread stopped");
            })
            .map_err(SwitcherError::Io)?;

        wait_for_worker_startup_ready(&ready_rx, SELECTED_TEXT_WORKER_STARTUP_TIMEOUT)?;

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

    pub(crate) fn ensure_ready(&self) -> Result<(), SwitcherError> {
        if self.worker_alive.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SwitcherError::InputWorkerDisconnected {
                worker: "selected-text-worker",
            })
        }
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
    fn selected_text_worker_startup_wait_is_bounded() {
        let (_ready_tx, ready_rx) = mpsc::sync_channel(0);

        let error =
            wait_for_worker_startup_ready(&ready_rx, std::time::Duration::ZERO).unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkerStartupTimedOut {
                worker: "selected-text-worker",
                timeout_ms: 0
            }
        ));
    }

    #[test]
    fn selected_text_worker_disconnect_before_ready_is_recoverable() {
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        drop(ready_tx);

        let error = wait_for_worker_startup_ready(&ready_rx, Duration::from_millis(1)).unwrap_err();

        assert!(matches!(
            error,
            SwitcherError::InputWorkerDisconnected {
                worker: "selected-text-worker"
            }
        ));
        assert!(error.is_recoverable_input_error());
    }

    #[test]
    fn selected_text_ready_is_published_at_command_loop_entry() {
        let alive = Arc::new(AtomicBool::new(false));
        let worker_alive = Arc::clone(&alive);
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let (command_tx, command_rx) = mpsc::channel();
        let (setup_entered_tx, setup_entered_rx) = mpsc::channel();
        let (allow_setup_tx, allow_setup_rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            setup_entered_tx.send(()).unwrap();
            allow_setup_rx.recv().unwrap();
            let result = run_selected_text_command_loop(
                command_rx,
                || {
                    worker_alive.store(true, Ordering::SeqCst);
                    ready_tx
                        .send(())
                        .map_err(|_| SwitcherError::SelectedTextWorkerDisconnected)
                },
                || Ok(()),
            );
            worker_alive.store(false, Ordering::SeqCst);
            result
        });

        setup_entered_rx.recv().unwrap();
        assert!(matches!(
            ready_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!alive.load(Ordering::SeqCst));

        allow_setup_tx.send(()).unwrap();
        ready_rx.recv().unwrap();
        assert!(alive.load(Ordering::SeqCst));
        drop(command_tx);
        worker.join().unwrap().unwrap();
        assert!(!alive.load(Ordering::SeqCst));
    }

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
