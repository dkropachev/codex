use std::cell::RefCell;

use anyhow::Context as _;
use anyhow::Result;

use crate::workflow_runtime::WorkflowRuntimeEvent;
use crate::workflow_runtime::WorkflowRuntimeEventHandler;
use crate::workflow_runtime::WorkflowRuntimeOutput;

pub(crate) type RuneRuntimeEventSender = tokio::sync::mpsc::UnboundedSender<WorkflowRuntimeEvent>;

thread_local! {
    static RUNE_RUNTIME_EVENT_SENDER: RefCell<Option<RuneRuntimeEventSender>> = const { RefCell::new(None) };
}

pub(crate) struct RuneRuntimeEventSenderGuard {
    previous: Option<RuneRuntimeEventSender>,
}

impl RuneRuntimeEventSenderGuard {
    pub(crate) fn new(sender: Option<RuneRuntimeEventSender>) -> Self {
        let previous = RUNE_RUNTIME_EVENT_SENDER.with(|event_sender| {
            let mut event_sender = event_sender.borrow_mut();
            std::mem::replace(&mut *event_sender, sender)
        });
        Self { previous }
    }
}

impl Drop for RuneRuntimeEventSenderGuard {
    fn drop(&mut self) {
        RUNE_RUNTIME_EVENT_SENDER.with(|event_sender| {
            let mut event_sender = event_sender.borrow_mut();
            *event_sender = self.previous.take();
        });
    }
}

pub(crate) fn channel_if_needed(
    event_handler: Option<&WorkflowRuntimeEventHandler<'_>>,
) -> (
    Option<RuneRuntimeEventSender>,
    Option<tokio::sync::mpsc::UnboundedReceiver<WorkflowRuntimeEvent>>,
) {
    if event_handler.is_some() {
        let (event_sender, event_receiver) = tokio::sync::mpsc::unbounded_channel();
        (Some(event_sender), Some(event_receiver))
    } else {
        (None, None)
    }
}

pub(crate) async fn forward_events_until_done(
    task: tokio::task::JoinHandle<Result<WorkflowRuntimeOutput>>,
    event_handler: &WorkflowRuntimeEventHandler<'_>,
    event_receiver: &mut tokio::sync::mpsc::UnboundedReceiver<WorkflowRuntimeEvent>,
) -> Result<WorkflowRuntimeOutput> {
    tokio::pin!(task);
    let mut events_open = true;
    loop {
        tokio::select! {
            result = &mut task => {
                while let Ok(event) = event_receiver.try_recv() {
                    event_handler(&event);
                }
                return result.context("Rune workflow task failed")?;
            }
            event = event_receiver.recv(), if events_open => match event {
                Some(event) => event_handler(&event),
                None => events_open = false,
            },
        }
    }
}

pub(crate) fn emit_runtime_event(event: WorkflowRuntimeEvent) {
    let handled = RUNE_RUNTIME_EVENT_SENDER.with(|event_sender| {
        event_sender
            .borrow()
            .as_ref()
            .is_some_and(|event_sender| event_sender.send(event.clone()).is_ok())
    });
    if handled {
        return;
    }
    match serde_json::to_string(&event) {
        Ok(event) => {
            let prefix = crate::workflow_runtime::WORKFLOW_RUNTIME_EVENT_PREFIX;
            eprintln!("{prefix}{event}");
        }
        Err(err) => eprintln!("failed to encode workflow runtime event: {err}"),
    }
}
