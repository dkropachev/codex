use std::collections::HashMap;
use std::sync::Arc;

use codex_extension_api::ExtensionDataInit;
use codex_features::Feature;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::items::CommandExecutionStatus;
use codex_protocol::items::TurnItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::PatchApplyStatus;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use codex_utils_path_uri::PathUri;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::codex_delegate::DelegateContextPolicy;
use crate::codex_delegate::RestrictedReviewStage;
use crate::codex_delegate::ReviewProtectedPaths;
use crate::codex_delegate::ToolFreeReviewStage;
use crate::codex_delegate::run_codex_thread_one_shot;
use crate::config::Config;
use crate::config::Constrained;
use crate::config::PermissionProfileSnapshot;
use crate::context::PullRequestContext;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_git_utils::ReviewFixFileChange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StagePermissions {
    ReadOnly,
    WorkspaceWrite,
    ToolFree,
}

pub(super) struct ReviewStageRequest {
    pub(super) model: String,
    pub(super) system_prompt: String,
    pub(super) context_items: Vec<ResponseItem>,
    pub(super) user_prompt: String,
    pub(super) output_schema: Value,
    pub(super) permissions: StagePermissions,
    pub(super) include_pull_request_context: bool,
}

#[derive(Debug, Default)]
pub(super) struct ReviewStageEvidence {
    command_results: HashMap<String, CommandEvidence>,
    successful_file_changes: Vec<std::collections::HashMap<std::path::PathBuf, FileChange>>,
    potentially_mutating_commands: Vec<String>,
    last_mutation_sequence: u64,
}

#[derive(Debug)]
struct CommandEvidence {
    started_sequence: Option<u64>,
    completed_sequence: u64,
    success: bool,
}

impl ReviewStageEvidence {
    pub(super) fn observed_successful_command_after_last_mutation(&self, command: &str) -> bool {
        self.command_results.get(command).is_some_and(|result| {
            result.success
                && result
                    .started_sequence
                    .is_some_and(|sequence| sequence > self.last_mutation_sequence)
                && result.completed_sequence > self.last_mutation_sequence
        })
    }

    pub(super) fn resolved_file_changes(
        &self,
        checkout_root: &PathUri,
    ) -> anyhow::Result<Vec<ReviewFixFileChange>> {
        let resolve = |path: &std::path::Path| {
            let path = path.to_string_lossy();
            PathUri::parse(path.as_ref()).or_else(|_| checkout_root.join(path.as_ref()))
        };
        let mut resolved = Vec::new();
        for changes in &self.successful_file_changes {
            let mut changes = changes.iter().collect::<Vec<_>>();
            changes.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (path, change) in changes {
                let path = resolve(path)?;
                resolved.push(match change {
                    FileChange::Add { content } => ReviewFixFileChange::Add {
                        path,
                        content: content.clone(),
                    },
                    FileChange::Delete { content } => ReviewFixFileChange::Delete {
                        path,
                        content: content.clone(),
                    },
                    FileChange::Update {
                        unified_diff,
                        move_path,
                    } => ReviewFixFileChange::Update {
                        path,
                        unified_diff: unified_diff.clone(),
                        move_path: move_path.as_ref().map(|path| resolve(path)).transpose()?,
                    },
                });
            }
        }
        Ok(resolved)
    }

    pub(super) fn has_potentially_mutating_command(&self) -> bool {
        !self.potentially_mutating_commands.is_empty()
    }
}

pub(super) struct ReviewStageResponse {
    pub(super) output: Option<String>,
    pub(super) evidence: ReviewStageEvidence,
}

#[derive(Default)]
struct ReviewStageEvidenceCollector {
    evidence: ReviewStageEvidence,
    pending_commands: HashMap<String, String>,
    command_start_sequences: HashMap<String, u64>,
    next_sequence: u64,
}

impl ReviewStageEvidenceCollector {
    fn observe_started_item(&mut self, item: &TurnItem) {
        self.next_sequence = self.next_sequence.saturating_add(1);
        if let TurnItem::CommandExecution(command) = item {
            self.command_start_sequences
                .entry(command.id.clone())
                .or_insert(self.next_sequence);
        }
    }

    fn observe_response_item(&mut self, item: &ResponseItem) {
        match item {
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } if matches!(name.as_str(), "exec_command" | "shell" | "shell_command") => {
                let Ok(arguments) = serde_json::from_str::<Value>(arguments) else {
                    return;
                };
                let Some(command) = arguments
                    .get("cmd")
                    .or_else(|| arguments.get("command"))
                    .and_then(Value::as_str)
                else {
                    return;
                };
                self.pending_commands
                    .insert(call_id.clone(), command.to_string());
            }
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                action: codex_protocol::models::LocalShellAction::Exec(action),
                ..
            } => {
                self.pending_commands.insert(
                    call_id.clone(),
                    codex_shell_command::parse_command::shlex_join(&action.command),
                );
            }
            _ => {}
        }
    }

    fn observe_completed_item(&mut self, item: &TurnItem) {
        self.next_sequence = self.next_sequence.saturating_add(1);
        match item {
            TurnItem::CommandExecution(command) => {
                if command.status == CommandExecutionStatus::InProgress {
                    return;
                }
                let invoked_command = self.pending_commands.remove(&command.id);
                let invoked_command = invoked_command.unwrap_or_else(|| {
                    codex_shell_command::parse_command::shlex_join(&command.command)
                });
                self.evidence.command_results.insert(
                    invoked_command.clone(),
                    CommandEvidence {
                        started_sequence: self.command_start_sequences.remove(&command.id),
                        completed_sequence: self.next_sequence,
                        success: command.status == CommandExecutionStatus::Completed
                            && command.exit_code == Some(0),
                    },
                );
                if command.status == CommandExecutionStatus::Completed
                    && command.exit_code == Some(0)
                    && !codex_shell_command::is_safe_command::is_known_safe_command(
                        &command.command,
                    )
                    && !looks_like_verification_command(&invoked_command)
                {
                    self.evidence
                        .potentially_mutating_commands
                        .push(invoked_command);
                }
            }
            TurnItem::FileChange(file_change)
                if file_change.status == Some(PatchApplyStatus::Completed) =>
            {
                self.evidence.last_mutation_sequence = self.next_sequence;
                self.evidence
                    .successful_file_changes
                    .push(file_change.changes.clone());
            }
            _ => {}
        }
    }
}

fn looks_like_verification_command(command: &str) -> bool {
    let words = command
        .split_whitespace()
        .take(3)
        .map(|word| word.trim_matches(['\'', '"']).to_ascii_lowercase())
        .collect::<Vec<_>>();
    matches!(
        words.as_slice(),
        [tool, action, ..]
            if matches!(tool.as_str(), "cargo" | "just")
                && matches!(action.as_str(), "test" | "nextest" | "check" | "clippy")
    ) || matches!(words.as_slice(), [tool, ..] if matches!(tool.as_str(), "pytest" | "nextest"))
        || matches!(words.as_slice(), [tool, action, ..] if tool == "go" && action == "test")
        || matches!(words.as_slice(), [tool, action, ..] if matches!(tool.as_str(), "npm" | "pnpm" | "yarn") && action == "test")
}

pub(super) async fn run_review_stage(
    session: Arc<Session>,
    ctx: Arc<TurnContext>,
    request: ReviewStageRequest,
    cancellation_token: CancellationToken,
) -> anyhow::Result<ReviewStageResponse> {
    let mut config = review_stage_config(ctx.config.as_ref(), &request)?;
    config.model = Some(request.model);
    config.base_instructions = Some(request.system_prompt);
    let input = vec![UserInput::Text {
        text: request.user_prompt,
        text_elements: Vec::new(),
    }];
    let initial_history = InitialHistory::Forked(
        request
            .context_items
            .into_iter()
            .map(RolloutItem::ResponseItem)
            .collect(),
    );
    let mut extension_data = ExtensionDataInit::default();
    extension_data.insert(RestrictedReviewStage);
    if request.include_pull_request_context
        && let Some(context) = ctx.extension_data.get::<PullRequestContext>()
    {
        extension_data.insert(context.as_ref().clone());
    }
    if request.permissions == StagePermissions::ToolFree {
        extension_data.insert(ToolFreeReviewStage);
    }
    if request.permissions == StagePermissions::WorkspaceWrite
        && let Some(paths) = ctx.extension_data.get::<ReviewProtectedPaths>()
    {
        extension_data.insert(paths.as_ref().clone());
    }
    let child = run_codex_thread_one_shot(
        config,
        session.services.auth_manager.clone(),
        session.services.models_manager.clone(),
        input,
        session.clone(),
        ctx.clone(),
        cancellation_token,
        SubAgentSource::Review,
        Some(request.output_schema),
        Some(initial_history),
        extension_data,
        DelegateContextPolicy::Isolated,
    )
    .await?;

    let mut evidence = ReviewStageEvidenceCollector::default();
    while let Ok(event) = child.next_event().await {
        match event.msg {
            EventMsg::AgentMessage(_)
            | EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent { .. })
            | EventMsg::UserMessage(_)
            | EventMsg::TurnStarted(_) => {}
            EventMsg::RawResponseItem(event) => evidence.observe_response_item(&event.item),
            EventMsg::ItemStarted(event) => {
                evidence.observe_started_item(&event.item);
                if !is_private_stage_item(&event.item) {
                    session
                        .send_event(ctx.as_ref(), EventMsg::ItemStarted(event))
                        .await;
                }
            }
            EventMsg::ItemCompleted(event) => {
                evidence.observe_completed_item(&event.item);
                if !is_private_stage_item(&event.item) {
                    session
                        .send_event(ctx.as_ref(), EventMsg::ItemCompleted(event))
                        .await;
                }
            }
            EventMsg::TurnComplete(completed) => {
                return Ok(ReviewStageResponse {
                    output: completed.last_agent_message,
                    evidence: evidence.evidence,
                });
            }
            EventMsg::TurnAborted(_) => {
                return Ok(ReviewStageResponse {
                    output: None,
                    evidence: evidence.evidence,
                });
            }
            other => session.send_event(ctx.as_ref(), other).await,
        }
    }
    Ok(ReviewStageResponse {
        output: None,
        evidence: evidence.evidence,
    })
}

fn is_private_stage_item(item: &TurnItem) -> bool {
    matches!(
        item,
        TurnItem::UserMessage(_)
            | TurnItem::HookPrompt(_)
            | TurnItem::AgentMessage(_)
            | TurnItem::Plan(_)
            | TurnItem::Reasoning(_)
    )
}

fn review_stage_config(parent: &Config, request: &ReviewStageRequest) -> anyhow::Result<Config> {
    let mut config = parent.clone();
    config.include_apps_instructions = false;
    config.include_skill_instructions = false;
    config.include_collaboration_mode_instructions = false;
    config.include_environment_context = false;
    config.developer_instructions = None;
    config.project_doc_max_bytes = 0;
    config.tool_output_token_limit = Some(2 * 1024);
    config.compact_prompt = None;
    config.notify = None;
    config.memories.use_memories = false;
    config.memories.dedicated_tools = false;
    config.mcp_servers.set(HashMap::new())?;
    for feature in [
        Feature::WebSearchRequest,
        Feature::WebSearchCached,
        Feature::StandaloneWebSearch,
        Feature::Goals,
        Feature::SpawnCsv,
        Feature::Collab,
        Feature::MultiAgentV2,
        Feature::TokenBudget,
        Feature::CodeMode,
        Feature::CodeModeOnly,
        Feature::CodexHooks,
        Feature::Apps,
        Feature::Plugins,
        Feature::MemoryTool,
        Feature::ImageGeneration,
        Feature::Artifact,
        Feature::ExecPermissionApprovals,
        Feature::RequestPermissionsTool,
    ] {
        config.features.disable(feature)?;
    }
    config.web_search_mode.set(WebSearchMode::Disabled)?;
    if matches!(
        request.permissions,
        StagePermissions::ReadOnly | StagePermissions::ToolFree
    ) {
        config.permissions.approval_policy = Constrained::allow_only(AskForApproval::Never);
        config
            .permissions
            .replace_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
                PermissionProfile::read_only(),
            ))?;
    }
    if request.permissions == StagePermissions::WorkspaceWrite {
        config.permissions.approval_policy = Constrained::allow_only(AskForApproval::Never);
        config
            .permissions
            .replace_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
                PermissionProfile::workspace_write_with(
                    &[],
                    NetworkSandboxPolicy::Restricted,
                    /*exclude_tmpdir_env_var*/ false,
                    /*exclude_slash_tmp*/ false,
                ),
            ))?;
    }
    if request.permissions == StagePermissions::ToolFree {
        for feature in [
            Feature::ShellTool,
            Feature::UnifiedExec,
            Feature::ToolRouter,
        ] {
            config.features.disable(feature)?;
        }
    }
    Ok(config)
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
