//! Unified Exec: interactive process execution orchestrated with approvals + sandboxing.
//!
//! Responsibilities
//! - Manages interactive processes (create, reuse, buffer output with caps).
//! - Uses the shared ToolOrchestrator to handle approval, sandbox selection, and
//!   retry semantics in a single, descriptive flow.
//! - Spawns the PTY from a sandbox-transformed `ExecRequest`; on sandbox denial,
//!   retries without sandbox when policy allows (no re‑prompt thanks to caching).
//! - Uses the shared `is_likely_sandbox_denied` heuristic to keep denial messages
//!   consistent with other exec paths.
//!
//! Flow at a glance (open process)
//! 1) Build a small request `{ command, cwd }`.
//! 2) Orchestrator: approval (bypass/cache/prompt) → select sandbox → run.
//! 3) Runtime: transform `SandboxTransformRequest` -> `ExecRequest` -> spawn PTY.
//! 4) If denial, orchestrator retries with `SandboxType::None`.
//! 5) Process handle is returned with streaming output + metadata.
//!
//! This keeps policy logic and user interaction centralized while the PTY/process
//! concerns remain isolated here. The implementation is split between:
//! - `process.rs`: PTY process lifecycle + output buffering.
//! - `process_state.rs`: shared exit/failure state for local and remote processes.
//! - `process_manager.rs`: orchestration (approvals, sandboxing, reuse) and request handling.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Weak;

use codex_exec_server::Environment;
use codex_features::Feature;
use codex_network_proxy::NetworkProxy;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_tools::UnifiedExecShellMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_output_truncation::formatted_truncate_text;
use rand::Rng;
use rand::rng;
use tokio::sync::Mutex;
use tracing::warn;

use crate::sandboxing::SandboxPermissions;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::shell::ShellType;
use crate::tools::network_approval::DeferredNetworkApproval;

mod async_watcher;
mod errors;
mod head_tail_buffer;
mod process;
mod process_manager;
mod process_state;

pub(crate) fn set_deterministic_process_ids_for_tests(enabled: bool) {
    process_manager::set_deterministic_process_ids_for_tests(enabled);
}

pub(crate) use errors::UnifiedExecError;
pub(crate) use process::NoopSpawnLifecycle;
#[cfg(unix)]
pub(crate) use process::SpawnLifecycle;
pub(crate) use process::SpawnLifecycleHandle;
pub(crate) use process::UnifiedExecProcess;

pub(crate) const MIN_YIELD_TIME_MS: u64 = 250;
// Minimum yield time for an empty `write_stdin`.
pub(crate) const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
pub(crate) const MAX_YIELD_TIME_MS: u64 = 30_000;
pub(crate) const DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS: u64 = 300_000;
pub(crate) const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
pub(crate) const UNIFIED_EXEC_OUTPUT_MAX_BYTES: usize = 1024 * 1024; // 1 MiB
pub(crate) const UNIFIED_EXEC_OUTPUT_MAX_TOKENS: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES / 4;
pub(crate) const MAX_UNIFIED_EXEC_PROCESSES: usize = 64;
pub(crate) const MAX_ARCHIVED_EXEC_OUTPUTS: usize = 64;
pub(crate) const MAX_ARCHIVED_EXEC_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const SOURCE_READ_DEDUPE_SUGGESTION_KEY: &str = "exec.source-read-dedupe-v1";

pub(crate) struct UnifiedExecContext {
    pub session: Arc<Session>,
    pub turn: Arc<TurnContext>,
    pub call_id: String,
}

impl UnifiedExecContext {
    pub fn new(session: Arc<Session>, turn: Arc<TurnContext>, call_id: String) -> Self {
        Self {
            session,
            turn,
            call_id,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ExecCommandRequest {
    pub command: Vec<String>,
    pub shell_type: ShellType,
    pub hook_command: String,
    pub process_id: i32,
    pub yield_time_ms: u64,
    pub max_output_tokens: Option<usize>,
    pub cwd: AbsolutePathBuf,
    pub sandbox_cwd: AbsolutePathBuf,
    pub environment: Arc<Environment>,
    pub shell_mode: UnifiedExecShellMode,
    pub network: Option<NetworkProxy>,
    pub tty: bool,
    pub sandbox_permissions: SandboxPermissions,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    pub additional_permissions_preapproved: bool,
    pub justification: Option<String>,
    pub prefix_rule: Option<Vec<String>>,
}

#[derive(Debug)]
pub(crate) struct WriteStdinRequest<'a> {
    pub process_id: i32,
    pub input: &'a str,
    pub yield_time_ms: u64,
    pub max_output_tokens: Option<usize>,
    pub truncation_policy: TruncationPolicy,
}

#[derive(Default)]
pub(crate) struct ProcessStore {
    processes: HashMap<i32, ProcessEntry>,
    reserved_process_ids: HashSet<i32>,
    archived_outputs: VecDeque<ArchivedExecOutput>,
    archived_output_bytes: usize,
}

impl ProcessStore {
    fn remove(&mut self, process_id: i32) -> Option<ProcessEntry> {
        self.reserved_process_ids.remove(&process_id);
        self.processes.remove(&process_id)
    }
}

#[derive(Clone)]
struct ArchivedExecOutput {
    chunk_id: String,
    output: Vec<u8>,
}

pub(crate) struct UnifiedExecProcessManager {
    process_store: Mutex<ProcessStore>,
    max_write_stdin_yield_time_ms: u64,
}

impl UnifiedExecProcessManager {
    pub(crate) fn new(max_write_stdin_yield_time_ms: u64) -> Self {
        Self {
            process_store: Mutex::new(ProcessStore::default()),
            max_write_stdin_yield_time_ms: max_write_stdin_yield_time_ms
                .max(MIN_EMPTY_YIELD_TIME_MS),
        }
    }
}

impl Default for UnifiedExecProcessManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS)
    }
}

struct ProcessEntry {
    process: Arc<UnifiedExecProcess>,
    call_id: String,
    process_id: i32,
    hook_command: String,
    tty: bool,
    network_approval: Option<DeferredNetworkApproval>,
    session: Weak<Session>,
    last_used: tokio::time::Instant,
    exec_output_compaction_enabled: bool,
    tool_router_output_optimization_enabled: bool,
    model_slug: String,
    model_provider: String,
}

pub(crate) fn clamp_yield_time(yield_time_ms: u64) -> u64 {
    yield_time_ms.clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS)
}

pub(crate) fn resolve_max_tokens(max_tokens: Option<usize>) -> usize {
    max_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
}

pub(crate) fn generate_chunk_id() -> String {
    let mut rng = rng();
    (0..6)
        .map(|_| format!("{:x}", rng.random_range(0..16)))
        .collect()
}

pub(crate) fn compact_exec_output(
    command: &[String],
    output: &str,
    max_output_tokens: Option<usize>,
    truncation_policy: TruncationPolicy,
) -> Option<crate::tools::context::ExecOutputCompaction> {
    let compaction = codex_exec_output_compaction::compact_output(command, output)?;
    compact_candidate_for_response(compaction, output, max_output_tokens, truncation_policy)
}

pub(crate) struct ExecOutputCompactionPolicy<'a> {
    pub(crate) state_db: Option<&'a codex_state::StateRuntime>,
    pub(crate) model_slug: &'a str,
    pub(crate) model_provider: &'a str,
    pub(crate) tool_name: &'a str,
    pub(crate) tool_router_output_optimization_enabled: bool,
    pub(crate) thread_id: Option<&'a str>,
    pub(crate) call_id: Option<&'a str>,
}

pub(crate) async fn compact_exec_output_with_policy(
    policy: ExecOutputCompactionPolicy<'_>,
    command: &[String],
    output: &str,
    max_output_tokens: Option<usize>,
    truncation_policy: TruncationPolicy,
) -> Option<crate::tools::context::ExecOutputCompaction> {
    let static_compaction =
        compact_exec_output(command, output, max_output_tokens, truncation_policy);
    if static_compaction.is_some() {
        return static_compaction;
    }
    if !policy.tool_router_output_optimization_enabled {
        return None;
    }

    let state_db = policy.state_db?;
    let accepted_keys = match accepted_output_optimization_keys(state_db, &policy, command).await {
        Ok(accepted_keys) => accepted_keys,
        Err(err) => {
            warn!("failed to load accepted exec output optimization keys: {err:#}");
            return None;
        }
    };
    if accepted_keys.is_empty() {
        return None;
    }

    codex_exec_output_compaction::compact_output_for_suggestions(command, output, &accepted_keys)
        .and_then(|compaction| {
            compact_candidate_for_response(compaction, output, max_output_tokens, truncation_policy)
        })
}

pub(crate) struct ExecOutputCompactionTurnRequest<'a> {
    pub(crate) session: &'a Session,
    pub(crate) turn: &'a TurnContext,
    pub(crate) tool_name: &'a str,
    pub(crate) call_id: &'a str,
    pub(crate) command: &'a [String],
    pub(crate) output: &'a str,
    pub(crate) max_output_tokens: Option<usize>,
    pub(crate) truncation_policy: TruncationPolicy,
}

pub(crate) async fn compact_exec_output_for_turn(
    request: ExecOutputCompactionTurnRequest<'_>,
) -> Option<crate::tools::context::ExecOutputCompaction> {
    let ExecOutputCompactionTurnRequest {
        session,
        turn,
        tool_name,
        call_id,
        command,
        output,
        max_output_tokens,
        truncation_policy,
    } = request;
    if !turn.features.enabled(Feature::ExecOutputCompaction) {
        return None;
    }

    let state_db = session.state_db();
    let thread_id = session.thread_id.to_string();
    compact_exec_output_with_policy(
        ExecOutputCompactionPolicy {
            state_db: state_db.as_deref(),
            model_slug: turn.model_info.slug.as_str(),
            model_provider: turn.config.model_provider_id.as_str(),
            tool_name,
            tool_router_output_optimization_enabled: turn.features.enabled(Feature::ToolRouter),
            thread_id: Some(thread_id.as_str()),
            call_id: Some(call_id),
        },
        command,
        output,
        max_output_tokens,
        truncation_policy,
    )
    .await
}

async fn accepted_output_optimization_keys(
    state_db: &codex_state::StateRuntime,
    policy: &ExecOutputCompactionPolicy<'_>,
    command: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut tool_names = vec![policy.tool_name];
    if policy.tool_name == "write_stdin" {
        tool_names.push("exec_command");
    }

    let mut keys = BTreeSet::new();
    for tool_name in tool_names {
        for key in state_db
            .list_accepted_tool_router_output_optimization_keys_for_tool(
                policy.model_slug,
                policy.model_provider,
                "",
                tool_name,
            )
            .await?
        {
            keys.insert(key);
        }
    }

    if keys.contains(SOURCE_READ_DEDUPE_SUGGESTION_KEY) {
        let source_read_dedupe_allowed = match (policy.thread_id, policy.call_id) {
            (Some(thread_id), Some(call_id)) => {
                let command_text = command.join(" ");
                state_db
                    .tool_router_recent_duplicate_source_read_for_command(
                        thread_id,
                        call_id,
                        command_text.as_str(),
                    )
                    .await?
            }
            (None, _) | (_, None) => false,
        };
        if !source_read_dedupe_allowed {
            keys.remove(SOURCE_READ_DEDUPE_SUGGESTION_KEY);
        }
    }

    Ok(keys.into_iter().collect())
}

fn compact_candidate_for_response(
    compaction: codex_exec_output_compaction::CompactedOutput,
    output: &str,
    max_output_tokens: Option<usize>,
    truncation_policy: TruncationPolicy,
) -> Option<crate::tools::context::ExecOutputCompaction> {
    let max_tokens = resolve_max_tokens(max_output_tokens).min(truncation_policy.token_budget());
    let raw_returned_tokens = approx_token_count(&formatted_truncate_text(
        output,
        TruncationPolicy::Tokens(max_tokens),
    ));
    let compacted_returned_tokens = approx_token_count(&formatted_truncate_text(
        compaction.text.as_str(),
        TruncationPolicy::Tokens(max_tokens),
    ));

    if compacted_returned_tokens >= raw_returned_tokens {
        return None;
    }

    Some({
        crate::tools::context::ExecOutputCompaction {
            filter_id: compaction.filter_id.to_string(),
            compacted_output: compaction.text,
            compacted_token_count: compaction.compacted_token_count,
        }
    })
}

#[cfg(test)]
#[cfg(unix)]
#[path = "process_tests.rs"]
mod process_tests;
#[cfg(test)]
#[cfg(unix)]
#[path = "mod_tests.rs"]
mod tests;
