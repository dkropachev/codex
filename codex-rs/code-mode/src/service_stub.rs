use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::runtime::CodeModeNestedToolCall;
use crate::runtime::ExecuteRequest;
use crate::runtime::RuntimeResponse;
use crate::runtime::WaitOutcome;
use crate::runtime::WaitRequest;

/// Host callbacks used by code-mode turn workers.
///
/// Musl builds do not include the V8 runtime, so this stub trait preserves the
/// public service contract while the stub worker never invokes callbacks.
#[async_trait]
pub trait CodeModeTurnHost: Send + Sync {
    async fn invoke_tool(
        &self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> Result<JsonValue, String>;

    async fn notify(&self, call_id: String, cell_id: String, text: String) -> Result<(), String>;
}

struct Inner {
    stored_values: Mutex<HashMap<String, JsonValue>>,
    next_cell_id: AtomicU64,
}

pub struct CodeModeService {
    inner: Arc<Inner>,
}

impl CodeModeService {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                stored_values: Mutex::new(HashMap::new()),
                next_cell_id: AtomicU64::new(1),
            }),
        }
    }

    pub async fn stored_values(&self) -> HashMap<String, JsonValue> {
        self.inner.stored_values.lock().await.clone()
    }

    pub async fn replace_stored_values(&self, values: HashMap<String, JsonValue>) {
        *self.inner.stored_values.lock().await = values;
    }

    pub fn allocate_cell_id(&self) -> String {
        self.inner
            .next_cell_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
    }

    pub async fn execute(&self, request: ExecuteRequest) -> Result<RuntimeResponse, String> {
        Err(format!(
            "exec runtime is unavailable in this musl build for cell {}",
            request.cell_id
        ))
    }

    pub async fn wait(&self, request: WaitRequest) -> Result<WaitOutcome, String> {
        Ok(WaitOutcome::MissingCell(RuntimeResponse::Result {
            cell_id: request.cell_id,
            content_items: Vec::new(),
            stored_values: HashMap::new(),
            error_text: Some("exec runtime is unavailable in this musl build".to_string()),
        }))
    }

    pub fn start_turn_worker(&self, _host: Arc<dyn CodeModeTurnHost>) -> CodeModeTurnWorker {
        CodeModeTurnWorker
    }
}

impl Default for CodeModeService {
    fn default() -> Self {
        Self::new()
    }
}

pub struct CodeModeTurnWorker;
