use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::Utc;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::WorkflowChildStatus as ApiWorkflowChildStatus;
use codex_app_server_protocol::WorkflowMarkdownResultNotification;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowRunApprovalHandling;
use codex_app_server_protocol::WorkflowRunCancelParams;
use codex_app_server_protocol::WorkflowRunCancelResponse;
use codex_app_server_protocol::WorkflowRunCompletedNotification;
use codex_app_server_protocol::WorkflowRunFailedNotification;
use codex_app_server_protocol::WorkflowRunReadParams;
use codex_app_server_protocol::WorkflowRunReadResponse;
use codex_app_server_protocol::WorkflowRunStartResponse;
use codex_app_server_protocol::WorkflowRunStatus;
use codex_app_server_protocol::WorkflowRunWaitParams;
use codex_app_server_protocol::WorkflowRunWaitResponse;
use codex_app_server_protocol::WorkflowStatusUpdate as ApiWorkflowStatusUpdate;
use codex_app_server_protocol::WorkflowThreadStatus as ApiWorkflowThreadStatus;
use codex_core::config::Config;
use codex_workflows::WorkflowCommand;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowCommandOutput;
use codex_workflows::WorkflowInputSource;
use codex_workflows::WorkflowRuntimeContext;
use codex_workflows::WorkflowRuntimeEvent;
use codex_workflows::execute_workflow_command;
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::error_code::invalid_params;
use crate::outgoing_message::OutgoingMessageSender;

#[derive(Clone)]
pub(crate) struct WorkflowRunManager {
    inner: Arc<WorkflowRunManagerInner>,
}

struct WorkflowRunManagerInner {
    runs: Mutex<HashMap<String, ManagedWorkflowRun>>,
    notify: Notify,
    outgoing: Arc<OutgoingMessageSender>,
}

struct ManagedWorkflowRun {
    run: WorkflowRun,
    canceled: Arc<AtomicBool>,
    abort_handle: Option<AbortHandle>,
}

pub(crate) struct WorkflowRunStartArgs {
    pub(crate) config: Arc<Config>,
    pub(crate) workflow_id: String,
    pub(crate) input: Option<JsonValue>,
    pub(crate) thread_id: Option<String>,
    pub(crate) stage_session_id: Option<String>,
    pub(crate) approval_handling: Option<WorkflowRunApprovalHandling>,
    pub(crate) app_server_url: Option<String>,
}

impl WorkflowRunManager {
    pub(crate) fn new(outgoing: Arc<OutgoingMessageSender>) -> Self {
        Self {
            inner: Arc::new(WorkflowRunManagerInner {
                runs: Mutex::new(HashMap::new()),
                notify: Notify::new(),
                outgoing,
            }),
        }
    }

    pub(crate) async fn start(
        &self,
        args: WorkflowRunStartArgs,
    ) -> Result<WorkflowRunStartResponse, JSONRPCErrorError> {
        let run_id = Uuid::new_v4().to_string();
        let now = now_unix_seconds();
        let run = WorkflowRun {
            id: run_id.clone(),
            workflow_id: args.workflow_id.clone(),
            status: WorkflowRunStatus::Running,
            thread_id: args.thread_id.clone(),
            created_at: now,
            started_at: Some(now),
            completed_at: None,
            output: None,
            error: None,
        };
        let canceled = Arc::new(AtomicBool::new(false));
        {
            let mut runs = self.inner.runs.lock().await;
            runs.insert(
                run_id.clone(),
                ManagedWorkflowRun {
                    run: run.clone(),
                    canceled: Arc::clone(&canceled),
                    abort_handle: None,
                },
            );
        }

        let manager = self.clone();
        let task_run_id = run_id.clone();
        let handle = tokio::spawn(async move {
            manager.run_to_completion(task_run_id, args, canceled).await;
        });
        {
            let mut runs = self.inner.runs.lock().await;
            if let Some(record) = runs.get_mut(&run_id) {
                record.abort_handle = Some(handle.abort_handle());
            }
        }

        Ok(WorkflowRunStartResponse { run })
    }

    pub(crate) async fn read(
        &self,
        params: WorkflowRunReadParams,
    ) -> Result<WorkflowRunReadResponse, JSONRPCErrorError> {
        Ok(WorkflowRunReadResponse {
            run: self.read_run(&params.run_id).await?,
        })
    }

    pub(crate) async fn wait(
        &self,
        params: WorkflowRunWaitParams,
    ) -> Result<WorkflowRunWaitResponse, JSONRPCErrorError> {
        if params.timeout_ms == Some(0) {
            let run = self.read_run(&params.run_id).await?;
            return Ok(WorkflowRunWaitResponse {
                completed: is_terminal(run.status),
                run,
            });
        }

        let wait_for_completion = async {
            loop {
                let notified = self.inner.notify.notified();
                let run = self.read_run(&params.run_id).await?;
                if is_terminal(run.status) {
                    return Ok(run);
                }
                notified.await;
            }
        };

        match params.timeout_ms {
            Some(timeout_ms) => {
                match tokio::time::timeout(Duration::from_millis(timeout_ms), wait_for_completion)
                    .await
                {
                    Ok(Ok(run)) => Ok(WorkflowRunWaitResponse {
                        run,
                        completed: true,
                    }),
                    Ok(Err(err)) => Err(err),
                    Err(_) => {
                        let run = self.read_run(&params.run_id).await?;
                        Ok(WorkflowRunWaitResponse {
                            completed: is_terminal(run.status),
                            run,
                        })
                    }
                }
            }
            None => Ok(WorkflowRunWaitResponse {
                run: wait_for_completion.await?,
                completed: true,
            }),
        }
    }

    pub(crate) async fn cancel(
        &self,
        params: WorkflowRunCancelParams,
    ) -> Result<WorkflowRunCancelResponse, JSONRPCErrorError> {
        let notification = {
            let mut runs = self.inner.runs.lock().await;
            let record = runs
                .get_mut(&params.run_id)
                .ok_or_else(|| invalid_params(format!("unknown workflow run {}", params.run_id)))?;
            if !is_terminal(record.run.status) {
                record.canceled.store(true, Ordering::SeqCst);
                if let Some(abort_handle) = record.abort_handle.take() {
                    abort_handle.abort();
                }
                record.run.status = WorkflowRunStatus::Canceled;
                record.run.completed_at = Some(now_unix_seconds());
                record.run.error = Some("workflow run canceled".to_string());
                Some(ServerNotification::WorkflowRunCompleted(
                    WorkflowRunCompletedNotification {
                        run: record.run.clone(),
                    },
                ))
            } else {
                None
            }
        };
        self.inner.notify.notify_waiters();
        if let Some(notification) = notification {
            self.inner
                .outgoing
                .send_server_notification(notification)
                .await;
        }
        Ok(WorkflowRunCancelResponse {
            run: self.read_run(&params.run_id).await?,
        })
    }

    pub(crate) async fn cancel_all(&self, reason: &str) {
        let notifications = {
            let mut runs = self.inner.runs.lock().await;
            let now = now_unix_seconds();
            runs.values_mut()
                .filter_map(|record| {
                    if is_terminal(record.run.status) {
                        return None;
                    }
                    record.canceled.store(true, Ordering::SeqCst);
                    if let Some(abort_handle) = record.abort_handle.take() {
                        abort_handle.abort();
                    }
                    record.run.status = WorkflowRunStatus::Canceled;
                    record.run.completed_at = Some(now);
                    record.run.error = Some(reason.to_string());
                    Some(ServerNotification::WorkflowRunCompleted(
                        WorkflowRunCompletedNotification {
                            run: record.run.clone(),
                        },
                    ))
                })
                .collect::<Vec<_>>()
        };

        self.inner.notify.notify_waiters();
        for notification in notifications {
            self.inner
                .outgoing
                .send_server_notification(notification)
                .await;
        }
    }

    async fn read_run(&self, run_id: &str) -> Result<WorkflowRun, JSONRPCErrorError> {
        self.inner
            .runs
            .lock()
            .await
            .get(run_id)
            .map(|record| record.run.clone())
            .ok_or_else(|| invalid_params(format!("unknown workflow run {run_id}")))
    }

    async fn run_to_completion(
        self,
        run_id: String,
        args: WorkflowRunStartArgs,
        canceled: Arc<AtomicBool>,
    ) {
        let result =
            run_workflow_blocking(run_id.clone(), args, Arc::clone(&canceled), self.clone()).await;
        if canceled.load(Ordering::SeqCst) {
            return;
        }

        match result {
            Ok(output) => {
                let run_output = workflow_run_output(output);
                if let Some(run) = self
                    .finish_run(
                        &run_id,
                        WorkflowRunStatus::Succeeded,
                        Some(run_output),
                        /*error*/ None,
                    )
                    .await
                {
                    self.inner
                        .outgoing
                        .send_server_notification(ServerNotification::WorkflowRunCompleted(
                            WorkflowRunCompletedNotification { run },
                        ))
                        .await;
                }
            }
            Err(err) => {
                if let Some(run) = self
                    .finish_run(
                        &run_id,
                        WorkflowRunStatus::Failed,
                        /*output*/ None,
                        Some(err),
                    )
                    .await
                {
                    self.inner
                        .outgoing
                        .send_server_notification(ServerNotification::WorkflowRunFailed(
                            WorkflowRunFailedNotification { run },
                        ))
                        .await;
                }
            }
        }
    }

    async fn finish_run(
        &self,
        run_id: &str,
        status: WorkflowRunStatus,
        output: Option<JsonValue>,
        error: Option<String>,
    ) -> Option<WorkflowRun> {
        let run = {
            let mut runs = self.inner.runs.lock().await;
            let record = runs.get_mut(run_id)?;
            if is_terminal(record.run.status) {
                return None;
            }
            record.run.status = status;
            record.run.completed_at = Some(now_unix_seconds());
            record.run.output = output;
            record.run.error = error;
            record.abort_handle = None;
            record.run.clone()
        };
        self.inner.notify.notify_waiters();
        Some(run)
    }

    fn forward_runtime_event(
        &self,
        run_id: &str,
        thread_id: Option<&str>,
        event: &WorkflowRuntimeEvent,
    ) {
        let notification = match event {
            WorkflowRuntimeEvent::Progress { message, data } => Some(
                ServerNotification::WorkflowRunProgress(WorkflowProgressNotification {
                    run_id: run_id.to_string(),
                    thread_id: thread_id.map(ToString::to_string),
                    message: message.clone(),
                    data: data.clone(),
                    status: None,
                }),
            ),
            WorkflowRuntimeEvent::Status { status } => Some(
                ServerNotification::WorkflowRunProgress(WorkflowProgressNotification {
                    run_id: run_id.to_string(),
                    thread_id: thread_id.map(ToString::to_string),
                    message: String::new(),
                    data: None,
                    status: Some(status_to_api(status.clone())),
                }),
            ),
            WorkflowRuntimeEvent::ReportToUserMarkdown { markdown } => Some(
                ServerNotification::WorkflowRunMarkdownResult(WorkflowMarkdownResultNotification {
                    run_id: run_id.to_string(),
                    thread_id: thread_id.map(ToString::to_string),
                    markdown: markdown.clone(),
                }),
            ),
            WorkflowRuntimeEvent::FinalMarkdown { .. } => None,
        };
        if let Some(notification) = notification {
            self.inner
                .outgoing
                .try_send_server_notification(notification);
        }
    }
}

async fn run_workflow_blocking(
    run_id: String,
    args: WorkflowRunStartArgs,
    canceled: Arc<AtomicBool>,
    manager: WorkflowRunManager,
) -> Result<WorkflowCommandOutput, String> {
    tokio::task::spawn_blocking(move || {
        let input = args
            .input
            .map(|value| WorkflowInputSource::Inline(value.to_string()));
        let (approvals, interactive_request_behavior) =
            approval_runtime_settings(args.approval_handling);
        let runtime = WorkflowRuntimeContext {
            run_id: Some(run_id.clone()),
            origin_thread_id: args.thread_id.clone(),
            app_server_url: args.app_server_url,
            approvals,
            interactive_request_behavior,
            output_format: Some("tui.markdown.v1".to_string()),
            force_process_runtime: true,
            cancellation_flag: Some(Arc::clone(&canceled)),
        };
        let thread_id = args.thread_id.clone();
        let runtime_event_handler = |event: &WorkflowRuntimeEvent| {
            if !canceled.load(Ordering::SeqCst) {
                manager.forward_runtime_event(&run_id, thread_id.as_deref(), event);
            }
        };
        execute_workflow_command(
            WorkflowCommandContext {
                codex_home: args.config.codex_home.as_path(),
                cwd: args.config.cwd.as_path(),
                config: &args.config.workflows,
                codex_self_exe: args.config.codex_self_exe.clone(),
                stage_session_id: args.stage_session_id,
                progress: None,
                runtime_event_handler: Some(&runtime_event_handler),
                runtime,
            },
            WorkflowCommand::Run {
                id: args.workflow_id,
                input,
                input_fields: BTreeMap::new(),
            },
        )
        .map_err(|err| format!("{err:#}"))
    })
    .await
    .map_err(|err| format!("workflow run task failed: {err}"))?
}

fn workflow_run_output(output: WorkflowCommandOutput) -> JsonValue {
    output
        .data
        .get("stdout")
        .and_then(JsonValue::as_str)
        .and_then(|stdout| serde_json::from_str(stdout).ok())
        .unwrap_or(output.data)
}

fn approval_runtime_settings(
    approval_handling: Option<WorkflowRunApprovalHandling>,
) -> (Option<String>, Option<String>) {
    match approval_handling {
        Some(WorkflowRunApprovalHandling::Delegate) => {
            (Some("delegate".to_string()), Some("defer".to_string()))
        }
        Some(WorkflowRunApprovalHandling::Decline) => {
            (Some("decline".to_string()), Some("decline".to_string()))
        }
        None => (None, None),
    }
}

fn status_to_api(status: codex_workflows::WorkflowStatusUpdate) -> ApiWorkflowStatusUpdate {
    ApiWorkflowStatusUpdate {
        workflow_name: status.workflow_name,
        workflow_status: status.workflow_status,
        threads: status
            .threads
            .into_iter()
            .map(thread_status_to_api)
            .collect(),
        child_statuses: status
            .child_statuses
            .into_iter()
            .map(child_status_to_api)
            .collect(),
    }
}

fn child_status_to_api(status: codex_workflows::WorkflowChildStatus) -> ApiWorkflowChildStatus {
    ApiWorkflowChildStatus {
        workflow_name: status.workflow_name,
        workflow_status: status.workflow_status,
        threads: status
            .threads
            .into_iter()
            .map(thread_status_to_api)
            .collect(),
    }
}

fn thread_status_to_api(status: codex_workflows::WorkflowThreadStatus) -> ApiWorkflowThreadStatus {
    ApiWorkflowThreadStatus {
        name: status.name,
        status: status.status,
    }
}

fn is_terminal(status: WorkflowRunStatus) -> bool {
    matches!(
        status,
        WorkflowRunStatus::Succeeded | WorkflowRunStatus::Failed | WorkflowRunStatus::Canceled
    )
}

fn now_unix_seconds() -> i64 {
    Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outgoing_message::OutgoingEnvelope;
    use crate::outgoing_message::OutgoingMessage;
    use codex_app_server_protocol::WorkflowStatusUpdate as ApiWorkflowStatusUpdate;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;

    fn running_run(run_id: &str) -> WorkflowRun {
        WorkflowRun {
            id: run_id.to_string(),
            workflow_id: "reports/jira".to_string(),
            status: WorkflowRunStatus::Running,
            thread_id: Some("thread-1".to_string()),
            created_at: 1,
            started_at: Some(1),
            completed_at: None,
            output: None,
            error: None,
        }
    }

    fn test_manager() -> (WorkflowRunManager, mpsc::Receiver<OutgoingEnvelope>) {
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let outgoing = Arc::new(OutgoingMessageSender::new(
            outgoing_tx,
            codex_analytics::AnalyticsEventsClient::disabled(),
        ));
        (WorkflowRunManager::new(outgoing), outgoing_rx)
    }

    async fn insert_running(manager: &WorkflowRunManager, run: WorkflowRun) {
        manager.inner.runs.lock().await.insert(
            run.id.clone(),
            ManagedWorkflowRun {
                run,
                canceled: Arc::new(AtomicBool::new(false)),
                abort_handle: None,
            },
        );
    }

    #[tokio::test]
    async fn wait_times_out_for_running_run() {
        let (manager, _outgoing_rx) = test_manager();
        insert_running(&manager, running_run("run-1")).await;

        let response = manager
            .wait(WorkflowRunWaitParams {
                run_id: "run-1".to_string(),
                timeout_ms: Some(0),
            })
            .await
            .expect("wait should return current run on timeout");

        assert_eq!(response.completed, false);
        assert_eq!(response.run.status, WorkflowRunStatus::Running);
    }

    #[tokio::test]
    async fn cancel_marks_run_canceled_and_emits_completion() {
        let (manager, mut outgoing_rx) = test_manager();
        insert_running(&manager, running_run("run-1")).await;

        let response = manager
            .cancel(WorkflowRunCancelParams {
                run_id: "run-1".to_string(),
            })
            .await
            .expect("cancel should succeed");

        assert_eq!(response.run.status, WorkflowRunStatus::Canceled);
        let envelope = outgoing_rx
            .recv()
            .await
            .expect("cancel should emit a completion notification");
        let OutgoingEnvelope::Broadcast { message } = envelope else {
            panic!("expected broadcast notification");
        };
        let OutgoingMessage::AppServerNotification(ServerNotification::WorkflowRunCompleted(
            notification,
        )) = message
        else {
            panic!("expected workflowRun/completed notification");
        };
        assert_eq!(notification.run.status, WorkflowRunStatus::Canceled);
    }

    #[tokio::test]
    async fn cancel_all_marks_running_runs_canceled_and_emits_completion() {
        let (manager, mut outgoing_rx) = test_manager();
        insert_running(&manager, running_run("run-1")).await;
        let canceled = manager
            .inner
            .runs
            .lock()
            .await
            .get("run-1")
            .expect("run exists")
            .canceled
            .clone();

        manager.cancel_all("runtime shutting down").await;

        let run = manager
            .read_run("run-1")
            .await
            .expect("canceled run should still be readable");
        assert_eq!(run.status, WorkflowRunStatus::Canceled);
        assert_eq!(run.error.as_deref(), Some("runtime shutting down"));
        assert!(canceled.load(Ordering::SeqCst));
        let envelope = outgoing_rx
            .recv()
            .await
            .expect("cancel_all should emit a completion notification");
        let OutgoingEnvelope::Broadcast { message } = envelope else {
            panic!("expected broadcast notification");
        };
        let OutgoingMessage::AppServerNotification(ServerNotification::WorkflowRunCompleted(
            notification,
        )) = message
        else {
            panic!("expected workflowRun/completed notification");
        };
        assert_eq!(notification.run, run);
    }

    #[tokio::test]
    async fn runtime_events_forward_as_workflow_run_notifications() {
        let (manager, mut outgoing_rx) = test_manager();
        manager.forward_runtime_event(
            "run-1",
            Some("thread-1"),
            &WorkflowRuntimeEvent::Status {
                status: codex_workflows::WorkflowStatusUpdate {
                    workflow_name: "Review".to_string(),
                    workflow_status: "checking".to_string(),
                    threads: vec![codex_workflows::WorkflowThreadStatus {
                        name: "worker".to_string(),
                        status: "running".to_string(),
                    }],
                    child_statuses: Vec::new(),
                },
            },
        );
        manager.forward_runtime_event(
            "run-1",
            Some("thread-1"),
            &WorkflowRuntimeEvent::ReportToUserMarkdown {
                markdown: "# Done".to_string(),
            },
        );

        let mut methods = Vec::new();
        while methods.len() < 2 {
            let envelope = outgoing_rx
                .recv()
                .await
                .expect("expected forwarded notification");
            let OutgoingEnvelope::Broadcast { message } = envelope else {
                continue;
            };
            if let OutgoingMessage::AppServerNotification(notification) = message {
                methods.push(notification);
            }
        }

        match &methods[0] {
            ServerNotification::WorkflowRunProgress(notification) => {
                assert_eq!(notification.run_id, "run-1");
                assert_eq!(notification.thread_id.as_deref(), Some("thread-1"));
                assert_eq!(
                    notification.status,
                    Some(ApiWorkflowStatusUpdate {
                        workflow_name: "Review".to_string(),
                        workflow_status: "checking".to_string(),
                        threads: vec![ApiWorkflowThreadStatus {
                            name: "worker".to_string(),
                            status: "running".to_string(),
                        }],
                        child_statuses: Vec::new(),
                    })
                );
            }
            other => panic!("expected workflowRun/progress, got {other:?}"),
        }
        match &methods[1] {
            ServerNotification::WorkflowRunMarkdownResult(notification) => {
                assert_eq!(notification.markdown, "# Done");
            }
            other => panic!("expected workflowRun/reportToUserMarkdown, got {other:?}"),
        }
    }
}
