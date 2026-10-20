//! Background discovery retains a bounded slot until each admitted RPC settles.

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ExperimentalFeature;
use codex_protocol::ThreadId;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::Weak;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::time::timeout_at;
use uuid::Uuid;

const MAX_OUTSTANDING: usize = 8;
static GATES: Mutex<Vec<Weak<Gate>>> = Mutex::new(Vec::new());

struct Gate {
    handle: AppServerRequestHandle,
    slots: Arc<Semaphore>,
}

fn connection_gate(handle: AppServerRequestHandle) -> Arc<Gate> {
    let mut gates = GATES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut matching = None;
    gates.retain(|weak| {
        let Some(gate) = weak.upgrade() else {
            return false;
        };
        if gate.handle.same_connection(&handle) {
            matching = Some(gate);
        }
        true
    });
    if let Some(gate) = matching {
        return gate;
    }
    let gate = Arc::new(Gate {
        handle,
        slots: Arc::new(Semaphore::new(MAX_OUTSTANDING)),
    });
    gates.push(Arc::downgrade(&gate));
    gate
}

pub(crate) async fn fetch(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<Vec<ExperimentalFeature>, String> {
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 5);
    let gate = connection_gate(request_handle);
    // Waiting callers stay in their own future: cancellation before admission
    // must not leave an acquisition task or an RPC behind.
    let permit = match timeout_at(deadline, Arc::clone(&gate.slots).acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => return Err("Experimental feature request failed".to_string()),
        Err(_) => return Err("Experimental feature discovery timed out".to_string()),
    };
    let (response_tx, response_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _permit = permit;
        let request_id = format!("tui-dynamic-{}", Uuid::new_v4());
        // Once admitted, retain the slot through the actual reply/error, even
        // if the caller times out or drops while the command is being queued.
        // This also preserves UUID correlation for late replies. Explicit
        // shutdown/disconnect ends these jobs; ordinary client drop need not.
        let result = super::discover(&gate.handle, Some(thread_id), &request_id, || {
            response_tx.is_closed() || Instant::now() >= deadline
        })
        .await;
        let _ = response_tx.send(result);
    });
    match timeout_at(deadline, response_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("Experimental feature request failed".to_string()),
        Err(_) => Err("Experimental feature discovery timed out".to_string()),
    }
}
