#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use crossterm::event::KeyCode;
#[cfg(unix)]
use crossterm::event::KeyEvent;
#[cfg(unix)]
use crossterm::event::KeyModifiers;
#[cfg(unix)]
use tokio::task::JoinHandle;

#[cfg(unix)]
use crate::app_event::AppEvent;
#[cfg(unix)]
use crate::app_event_sender::AppEventSender;

#[cfg(unix)]
const SIGINT_FORCE_EXIT_WINDOW: Duration = Duration::from_secs(/*secs*/ 2);
#[cfg(unix)]
const SIGINT_FORCE_EXIT_COUNT: usize = 3;

#[cfg(unix)]
pub(crate) struct SigintHandler {
    state: Arc<Mutex<SigintHandlerState>>,
    task: Option<JoinHandle<()>>,
}

#[cfg(unix)]
struct SigintHandlerState {
    app_event_tx: Option<AppEventSender>,
    pending_interrupts: usize,
    recent_interrupts: VecDeque<Instant>,
}

#[cfg(unix)]
impl SigintHandler {
    pub(crate) fn spawn() -> Self {
        let state = Arc::new(Mutex::new(SigintHandlerState {
            app_event_tx: None,
            pending_interrupts: 0,
            recent_interrupts: VecDeque::new(),
        }));
        let task = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(mut sigint) => {
                let state = state.clone();
                Some(tokio::spawn(async move {
                    while sigint.recv().await.is_some() {
                        let event_tx = {
                            let mut state = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let now = Instant::now();
                            state.recent_interrupts.push_back(now);
                            while state.recent_interrupts.front().is_some_and(|at| {
                                now.duration_since(*at) > SIGINT_FORCE_EXIT_WINDOW
                            }) {
                                state.recent_interrupts.pop_front();
                            }

                            if state.recent_interrupts.len() >= SIGINT_FORCE_EXIT_COUNT {
                                tracing::warn!("forcing Codex exit after repeated SIGINT");
                                let _ = crate::tui::restore();
                                std::process::exit(130);
                            }

                            if state.app_event_tx.is_none() {
                                state.pending_interrupts =
                                    state.pending_interrupts.saturating_add(1);
                            }
                            state.app_event_tx.clone()
                        };

                        if let Some(event_tx) = event_tx {
                            send_ctrl_c_key(&event_tx);
                        }
                    }
                }))
            }
            Err(err) => {
                tracing::warn!("failed to install SIGINT handler: {err}");
                None
            }
        };

        Self { state, task }
    }

    pub(crate) fn attach_app_event_tx(&self, app_event_tx: AppEventSender) {
        let pending_interrupts = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.app_event_tx = Some(app_event_tx.clone());
            std::mem::take(&mut state.pending_interrupts)
        };

        for _ in 0..pending_interrupts {
            send_ctrl_c_key(&app_event_tx);
        }
    }
}

#[cfg(unix)]
impl Drop for SigintHandler {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(unix)]
fn send_ctrl_c_key(app_event_tx: &AppEventSender) {
    app_event_tx.send(AppEvent::SyntheticKey(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
}
