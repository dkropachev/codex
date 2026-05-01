use std::collections::BTreeMap;
use std::collections::VecDeque;

use codex_config::config_toml::ModelRouterCandidateToml;
use codex_model_router::RouterTaskClass;
use codex_model_router::estimate_task_usage;
use codex_model_router::estimate_token_cost;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_rollout::RolloutRecorder;
use codex_state::ModelRouterTuneRollout;
use codex_state::StateRuntime;

use crate::model_router::token_price_from_candidate;

use super::ModelRouterTuneBudget;
use super::ModelRouterTuneBudgetUsed;

#[derive(Debug, Clone)]
pub(super) struct ReplayCase {
    pub(super) task_key: String,
    pub(super) source: String,
    pub(super) user_message: String,
    pub(super) production_output: String,
    pub(super) prompt_bytes: usize,
    pub(super) duration_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) struct BudgetSelection {
    pub(super) cases: Vec<ReplayCase>,
    pub(super) skipped_count: i64,
    pub(super) budget_used: ModelRouterTuneBudgetUsed,
}

pub(super) async fn collect_replay_cases(
    state_db: &StateRuntime,
    window_start_ms: Option<i64>,
) -> anyhow::Result<Vec<ReplayCase>> {
    let rollouts = state_db.model_router_tune_rollouts(window_start_ms).await?;
    let mut cases = Vec::new();
    for rollout in rollouts {
        match RolloutRecorder::load_rollout_items(&rollout.rollout_path).await {
            Ok((items, _thread_id, _parse_errors)) => {
                cases.extend(completed_turns_from_rollout(&rollout, &items));
            }
            Err(err) => {
                tracing::debug!(path = %rollout.rollout_path.display(), error = %err, "failed to load rollout for model router tune");
            }
        }
    }
    Ok(cases)
}

fn completed_turns_from_rollout(
    rollout: &ModelRouterTuneRollout,
    items: &[RolloutItem],
) -> Vec<ReplayCase> {
    let mut cases = Vec::new();
    let mut last_user_message: Option<String> = None;
    let mut active_turn: Option<PartialTurn> = None;
    for item in items {
        match item {
            RolloutItem::EventMsg(EventMsg::UserMessage(user)) => {
                last_user_message = Some(user.message.clone());
            }
            RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. })
                if role == "user" =>
            {
                last_user_message = Some(content_text(content));
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(started)) => {
                active_turn = Some(PartialTurn {
                    user_message: last_user_message.take(),
                    assistant_message: String::new(),
                    duration_ms: None,
                    task_key: task_key_for_rollout(rollout),
                    source: rollout.source.clone(),
                    prompt_bytes_hint: 0,
                    turn_id: started.turn_id.clone(),
                });
            }
            RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. })
                if role == "assistant" =>
            {
                if let Some(turn) = active_turn.as_mut() {
                    append_message(&mut turn.assistant_message, &content_text(content));
                }
            }
            RolloutItem::EventMsg(EventMsg::AgentMessage(agent)) => {
                if let Some(turn) = active_turn.as_mut() {
                    append_message(&mut turn.assistant_message, &agent.message);
                }
            }
            RolloutItem::TurnContext(turn_context) => {
                if let Some(turn) = active_turn.as_mut()
                    && turn.turn_id == turn_context.turn_id.clone().unwrap_or_default()
                {
                    turn.prompt_bytes_hint = turn_context.model.len();
                }
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(complete)) => {
                if let Some(mut turn) = active_turn.take() {
                    turn.duration_ms = complete.duration_ms;
                    if let Some(last_agent_message) = &complete.last_agent_message {
                        append_message(&mut turn.assistant_message, last_agent_message);
                    }
                    if let Some(case) = turn.into_case() {
                        cases.push(case);
                    }
                }
            }
            RolloutItem::SessionMeta(_)
            | RolloutItem::ResponseItem(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::EventMsg(_) => {}
        }
    }
    cases
}

#[derive(Debug, Clone)]
struct PartialTurn {
    user_message: Option<String>,
    assistant_message: String,
    duration_ms: Option<i64>,
    task_key: String,
    source: String,
    prompt_bytes_hint: usize,
    turn_id: String,
}

impl PartialTurn {
    fn into_case(self) -> Option<ReplayCase> {
        let user_message = self.user_message?;
        if user_message.trim().is_empty() || self.assistant_message.trim().is_empty() {
            return None;
        }
        let prompt_bytes = user_message.len().saturating_add(self.prompt_bytes_hint);
        Some(ReplayCase {
            task_key: self.task_key,
            source: self.source,
            user_message,
            production_output: self.assistant_message,
            prompt_bytes,
            duration_ms: self.duration_ms,
        })
    }
}

pub(super) fn select_budgeted_cases(
    cases: Vec<ReplayCase>,
    candidates: &[ModelRouterCandidateToml],
    budget: ModelRouterTuneBudget,
) -> BudgetSelection {
    let mut strata: BTreeMap<String, VecDeque<ReplayCase>> = BTreeMap::new();
    for case in cases {
        strata
            .entry(stratum_key(&case))
            .or_default()
            .push_back(case);
    }
    let mut selected = Vec::new();
    let mut budget_used = ModelRouterTuneBudgetUsed {
        cost_used_usd_micros: 0,
        tokens_used: 0,
    };
    let mut skipped_count = 0;
    loop {
        let mut progressed = false;
        for queue in strata.values_mut() {
            let Some(case) = queue.pop_front() else {
                continue;
            };
            let case_budget = case_budget(&case, candidates);
            let next_cost = budget_used
                .cost_used_usd_micros
                .saturating_add(case_budget.cost_used_usd_micros);
            let next_tokens = budget_used
                .tokens_used
                .saturating_add(case_budget.tokens_used);
            if next_cost > budget.cost_budget_usd_micros || next_tokens > budget.token_budget {
                skipped_count += 1;
                continue;
            }
            budget_used.cost_used_usd_micros = next_cost;
            budget_used.tokens_used = next_tokens;
            selected.push(case);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    skipped_count += strata.values().map(|queue| queue.len() as i64).sum::<i64>();
    BudgetSelection {
        cases: selected,
        skipped_count,
        budget_used,
    }
}

fn case_budget(
    case: &ReplayCase,
    candidates: &[ModelRouterCandidateToml],
) -> ModelRouterTuneBudgetUsed {
    let task_class = RouterTaskClass::infer(&case.task_key, case.prompt_bytes);
    let usage = estimate_task_usage(case.prompt_bytes, task_class);
    let candidate_count = i64::try_from(candidates.len().max(1)).unwrap_or(i64::MAX);
    let cost_used_usd_micros = candidates
        .iter()
        .filter_map(token_price_from_candidate)
        .map(|price| estimate_token_cost(&usage, &price, 1.0).usd_micros)
        .sum();
    ModelRouterTuneBudgetUsed {
        cost_used_usd_micros,
        tokens_used: usage.total_tokens.saturating_mul(candidate_count),
    }
}

fn task_key_for_rollout(rollout: &ModelRouterTuneRollout) -> String {
    let source = rollout.source.to_ascii_lowercase();
    if source.contains("repo_ci") {
        "module.repo_ci.history".to_string()
    } else if source.contains("subagent") {
        "subagent.history".to_string()
    } else {
        format!("history.{}", source.replace(' ', "_"))
    }
}

fn stratum_key(case: &ReplayCase) -> String {
    let prompt_bucket = if case.prompt_bytes <= 8 * 1024 {
        "small"
    } else if case.prompt_bytes <= 64 * 1024 {
        "medium"
    } else {
        "large"
    };
    let task_class = RouterTaskClass::infer(&case.task_key, case.prompt_bytes);
    format!("{task_class:?}|{}|{prompt_bucket}", case.source)
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_message(target: &mut String, message: &str) {
    if message.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(message);
}
