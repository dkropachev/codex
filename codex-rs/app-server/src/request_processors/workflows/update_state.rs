use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::WorkflowCheckUpdatesResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity;
use codex_app_server_protocol::WorkflowUpdateEntry;
use codex_app_server_protocol::WorkflowUpdateStatus;
use codex_app_server_protocol::WorkflowUpdatesChangedNotification;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_app_server_protocol::WorkflowUpdatesReadResponse;
use codex_core::config::Config;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowRecord;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ManagedWorkflowUpdate;
use futures::StreamExt;
use tokio::sync::RwLock;
use tokio::sync::watch;

use crate::error_code::invalid_params;
use crate::outgoing_message::OutgoingMessageSender;

const MAX_CHECKS_IN_FLIGHT: usize = 4;
const MAX_PAGE_LIMIT: usize = 100;
const MAX_SNAPSHOT_ENTRIES: usize = 1_024;
const MAX_ERROR_CHARS: usize = 2_048;

#[derive(Default)]
struct Snapshot {
    generation: u64,
    scan_id: u64,
    scanning: bool,
    data: Vec<WorkflowUpdateEntry>,
    error: Option<String>,
}

pub(super) struct WorkflowUpdates {
    snapshot: Arc<RwLock<Snapshot>>,
    recovered: watch::Receiver<bool>,
    config: Arc<Config>,
    notification_tx: watch::Sender<u64>,
}

impl WorkflowUpdates {
    pub(super) fn start(config: Arc<Config>, outgoing: Arc<OutgoingMessageSender>) -> Self {
        let snapshot = Arc::new(RwLock::new(Snapshot {
            scan_id: 1,
            scanning: true,
            ..Snapshot::default()
        }));
        let (recovered_tx, recovered) = watch::channel(false);
        let (notification_tx, mut notification_rx) = watch::channel(0);
        tokio::spawn(async move {
            while notification_rx.changed().await.is_ok() {
                let generation = *notification_rx.borrow_and_update();
                outgoing
                    .send_server_notification(ServerNotification::WorkflowUpdatesChanged(
                        WorkflowUpdatesChangedNotification { generation },
                    ))
                    .await;
            }
        });
        tokio::spawn(run_scan(
            Arc::clone(&config),
            notification_tx.clone(),
            Arc::clone(&snapshot),
            Some(recovered_tx),
        ));
        Self {
            snapshot,
            recovered,
            config,
            notification_tx,
        }
    }

    pub(super) async fn wait_for_recovery(&self) {
        let mut recovered = self.recovered.clone();
        if !*recovered.borrow() {
            let _ = recovered.changed().await;
        }
    }

    pub(super) async fn refresh(&self) -> WorkflowCheckUpdatesResponse {
        self.wait_for_recovery().await;
        let scan_id = {
            let mut state = self.snapshot.write().await;
            if state.scanning {
                return WorkflowCheckUpdatesResponse {
                    scan_id: state.scan_id,
                    started: false,
                };
            }
            state.scanning = true;
            state.scan_id = state.scan_id.saturating_add(1);
            state.generation = state.generation.saturating_add(1);
            state.data.clear();
            state.error = None;
            self.notification_tx.send_replace(state.generation);
            state.scan_id
        };
        tokio::spawn(run_scan(
            Arc::clone(&self.config),
            self.notification_tx.clone(),
            Arc::clone(&self.snapshot),
            None,
        ));
        WorkflowCheckUpdatesResponse {
            scan_id,
            started: true,
        }
    }

    pub(super) async fn read(
        &self,
        params: WorkflowUpdatesReadParams,
    ) -> Result<WorkflowUpdatesReadResponse, JSONRPCErrorError> {
        let limit = params.limit.unwrap_or(50) as usize;
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(invalid_params(
                "workflow updates limit must be between 1 and 100",
            ));
        }
        let offset = match params.cursor {
            Some(cursor) if cursor.len() <= 16 => cursor
                .parse::<usize>()
                .ok()
                .filter(|offset| *offset <= MAX_SNAPSHOT_ENTRIES)
                .ok_or_else(|| invalid_params("invalid workflow updates cursor"))?,
            Some(_) => return Err(invalid_params("workflow updates cursor is too long")),
            None => 0,
        };
        let snapshot = self.snapshot.read().await;
        let data = snapshot
            .data
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_cursor =
            (offset + data.len() < snapshot.data.len()).then(|| (offset + data.len()).to_string());
        Ok(WorkflowUpdatesReadResponse {
            generation: snapshot.generation,
            scan_id: snapshot.scan_id,
            scanning: snapshot.scanning,
            data,
            next_cursor,
            error: snapshot.error.clone(),
        })
    }
}

async fn run_scan(
    config: Arc<Config>,
    notification_tx: watch::Sender<u64>,
    snapshot: Arc<RwLock<Snapshot>>,
    recovered_tx: Option<watch::Sender<bool>>,
) {
    let prepared = match tokio::task::spawn_blocking(move || prepare(config.as_ref())).await {
        Ok(result) => result,
        Err(error) => Err(error.into()),
    };
    {
        let mut state = snapshot.write().await;
        state.generation = state.generation.saturating_add(1);
        state.data.clear();
        state.error = None;
        match &prepared {
            Ok(Some((_, records))) => {
                state.scanning = !records.is_empty();
                state.data = records.iter().map(pending_entry).collect();
            }
            Ok(None) => state.scanning = false,
            Err(error) => {
                state.scanning = false;
                state.error = Some(bounded_error(format!("{error:#}")));
            }
        }
        notification_tx.send_replace(state.generation);
    }
    if let Some(recovered_tx) = recovered_tx {
        let _ = recovered_tx.send(true);
    }
    if let Ok(Some((service, records))) = prepared
        && !records.is_empty()
    {
        run_checks(service, records, snapshot, notification_tx).await;
    }
}

fn prepare(
    config: &Config,
) -> anyhow::Result<Option<(Arc<ManagedWorkflowService>, Vec<ManagedWorkflowRecord>)>> {
    let home = &config.codex_home;
    if !has_entries(&home.join(".workflow-management/receipts"))?
        && !has_entries(&home.join(".workflow-management/journals"))?
        && !has_entries(&home.join(".workflow-management/bun/operations"))?
    {
        return Ok(None);
    }
    let service = ManagedWorkflowService::new(home, &home.join("workflows"))?;
    let records = service.list_installed()?;
    anyhow::ensure!(
        records.len() <= MAX_SNAPSHOT_ENTRIES,
        "managed workflow snapshot exceeds its entry limit"
    );
    Ok(Some((Arc::new(service), records)))
}

async fn run_checks(
    service: Arc<ManagedWorkflowService>,
    records: Vec<ManagedWorkflowRecord>,
    snapshot: Arc<RwLock<Snapshot>>,
    notification_tx: watch::Sender<u64>,
) {
    let mut checks = futures::stream::iter(records.into_iter().map(|record| {
        let service = Arc::clone(&service);
        async move {
            let id = record.id.clone();
            let checked = tokio::task::spawn_blocking(move || {
                service.check_update(&id, &AtomicBool::new(false))
            })
            .await;
            let update = match checked {
                Ok(Ok(check)) => check.update,
                Ok(Err(error)) => ManagedWorkflowUpdate::Error(format!("{error:#}")),
                Err(error) => ManagedWorkflowUpdate::Error(error.to_string()),
            };
            update_entry(record, update)
        }
    }))
    .buffer_unordered(MAX_CHECKS_IN_FLIGHT);
    while let Some(entry) = checks.next().await {
        {
            let mut state = snapshot.write().await;
            if let Ok(index) = state.data.binary_search_by(|row| row.id.cmp(&entry.id)) {
                state.data[index] = entry;
            }
            state.generation = state.generation.saturating_add(1);
            notification_tx.send_replace(state.generation);
        }
    }
    {
        let mut state = snapshot.write().await;
        state.scanning = false;
        state.generation = state.generation.saturating_add(1);
        notification_tx.send_replace(state.generation);
    }
}

fn has_entries(root: &AbsolutePathBuf) -> anyhow::Result<bool> {
    Ok(root.as_path().is_dir() && fs::read_dir(root.as_path())?.next().is_some())
}

fn pending_entry(record: &ManagedWorkflowRecord) -> WorkflowUpdateEntry {
    WorkflowUpdateEntry {
        id: record.id.clone(),
        status: WorkflowUpdateStatus::Pending,
        release: None,
        dismissed: false,
        error: None,
    }
}

fn update_entry(
    record: ManagedWorkflowRecord,
    update: ManagedWorkflowUpdate,
) -> WorkflowUpdateEntry {
    let (status, release, dismissed, error) = match update {
        ManagedWorkflowUpdate::Current => (WorkflowUpdateStatus::Current, None, false, None),
        ManagedWorkflowUpdate::Available { release, dismissed } => (
            WorkflowUpdateStatus::Available,
            Some(WorkflowReleaseIdentity {
                tag: release.tag,
                version: release.version,
                commit: release.commit,
            }),
            dismissed,
            None,
        ),
        ManagedWorkflowUpdate::Error(error) => (
            WorkflowUpdateStatus::Error,
            None,
            false,
            Some(bounded_error(error)),
        ),
    };
    WorkflowUpdateEntry {
        id: record.id,
        status,
        release,
        dismissed,
        error,
    }
}

fn bounded_error(error: String) -> String {
    error.chars().take(MAX_ERROR_CHARS).collect()
}
