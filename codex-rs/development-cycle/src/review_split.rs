use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

use crate::agents::AgentExecutionContext;
use crate::agents::AgentSpawnSpec;
use crate::agents::spawn_agent;
use crate::input::DevCycleInput;
use crate::models::ReviewModelChoice;
use crate::persistence::AgentAttemptRecord;
use crate::persistence::DevCycleState;
use crate::review_types::ReviewTypeDefinition;
use crate::split_persistence::SplitProposalRecord;
use crate::work_size::RepoSnapshot;

pub(crate) const SEPARATE_STRATEGY_ID: &str = "separate:v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewSplitGroup {
    pub group_id: String,
    pub review_type_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewSplitStrategy {
    pub strategy_id: String,
    pub groups: Vec<ReviewSplitGroup>,
    pub rationale: String,
    pub expected_reviewer_count_savings: f64,
    pub risk_notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewSplitPlan {
    pub active_strategy: ReviewSplitStrategy,
    pub primary_strategy: ReviewSplitStrategy,
    pub baseline_strategy: Option<ReviewSplitStrategy>,
    pub review_item_count: usize,
    pub item_set_key: String,
    pub repo_tshirt_bucket: String,
    pub model_key: String,
    pub ai_proposed_challenger: Option<ReviewSplitStrategy>,
    pub rejected_grouping_experiments: u32,
    pub max_rejected_grouping_experiments: u32,
    pub stop_reason: Option<String>,
    pub proposal_status: Option<GroupingProposalStatus>,
    pub baseline_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupingProposalStatus {
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewSplitOutcome {
    pub lost_evidence_count: u32,
    pub suppression_reason: Option<String>,
    pub promotion_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewSplitOutput {
    pub active_split: ReviewSplitStrategy,
    pub primary_split: ReviewSplitStrategy,
    pub zero_grouping: bool,
    pub mode: String,
    pub ai_proposed_challenger: Option<ReviewSplitStrategy>,
    pub cost_savings: i64,
    pub lost_evidence_count: u32,
    pub stop_reason: Option<String>,
    pub suppression_reason: Option<String>,
    pub promotion_reason: Option<String>,
    pub proposal_status: Option<GroupingProposalStatus>,
    pub baseline_resample: BaselineResampleOutput,
    pub rejected_grouping_experiments: u32,
    pub max_rejected_grouping_experiments: u32,
    pub repo_tshirt_bucket: String,
    pub item_set_key: String,
    pub model_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaselineResampleOutput {
    pub ran: bool,
    pub reason: Option<String>,
    pub lost_evidence_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProposalRejection {
    pub code: &'static str,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiGroupingProposal {
    #[serde(alias = "id")]
    strategy_id: String,
    groups: Vec<AiGroupingGroup>,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    expected_reviewer_count_savings: f64,
    #[serde(default)]
    risk_notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[serde(rename_all = "camelCase")]
enum AiGroupingGroup {
    Object {
        #[serde(rename = "groupId")]
        #[serde(default)]
        group_id: Option<String>,
        #[serde(rename = "reviewTypeIds")]
        #[serde(default)]
        review_type_ids: Vec<String>,
    },
    Ids(Vec<String>),
}

pub(crate) fn separate_strategy(selected: &[ReviewTypeDefinition]) -> ReviewSplitStrategy {
    ReviewSplitStrategy {
        strategy_id: SEPARATE_STRATEGY_ID.to_string(),
        groups: selected
            .iter()
            .map(|review_type| ReviewSplitGroup {
                group_id: review_type.id.clone(),
                review_type_ids: vec![review_type.id.clone()],
            })
            .collect(),
        rationale: "zero grouping: one reviewer per selected review item".to_string(),
        expected_reviewer_count_savings: 0.0,
        risk_notes: Vec::new(),
    }
}

pub(crate) fn item_set_key(selected: &[ReviewTypeDefinition]) -> String {
    let mut ids = selected
        .iter()
        .map(|review_type| review_type.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    digest(&ids.join("\n"))
}

pub(crate) fn separate_review_split_plan(
    input: &DevCycleInput,
    repo: &RepoSnapshot,
    selected: &[ReviewTypeDefinition],
    model: Option<&ReviewModelChoice>,
    stop_reason: Option<String>,
) -> ReviewSplitPlan {
    let separate = separate_strategy(selected);
    ReviewSplitPlan {
        active_strategy: separate.clone(),
        primary_strategy: separate,
        baseline_strategy: None,
        review_item_count: selected.len(),
        item_set_key: item_set_key(selected),
        repo_tshirt_bucket: repo.work_size.repo_tshirt_bucket.clone(),
        model_key: model
            .map(|model| model.score_key.clone())
            .unwrap_or_else(|| "unavailable".to_string()),
        ai_proposed_challenger: None,
        rejected_grouping_experiments: 0,
        max_rejected_grouping_experiments: input.max_rejected_grouping_experiments,
        stop_reason,
        proposal_status: None,
        baseline_reason: None,
    }
}

pub(crate) async fn prepare_review_split(
    agents: &AgentExecutionContext<'_>,
    input: &DevCycleInput,
    repo: &RepoSnapshot,
    selected: &[ReviewTypeDefinition],
    model: Option<&ReviewModelChoice>,
) -> anyhow::Result<ReviewSplitPlan> {
    let item_set_key = item_set_key(selected);
    let repo_tshirt_bucket = repo.work_size.repo_tshirt_bucket.clone();
    let separate = separate_strategy(selected);
    let Some(model) = model else {
        return Ok(ReviewSplitPlan {
            active_strategy: separate.clone(),
            primary_strategy: separate,
            baseline_strategy: None,
            review_item_count: selected.len(),
            item_set_key,
            repo_tshirt_bucket,
            model_key: "unavailable".to_string(),
            ai_proposed_challenger: None,
            rejected_grouping_experiments: 0,
            max_rejected_grouping_experiments: input.max_rejected_grouping_experiments,
            stop_reason: Some("no review model candidates available".to_string()),
            proposal_status: None,
            baseline_reason: None,
        });
    };

    let mut proposal_status = None;
    let active = agents
        .state
        .best_split_strategy_record(&model.score_key, &repo_tshirt_bucket, &item_set_key)?
        .and_then(|record| {
            match serde_json::from_str::<Vec<ReviewSplitGroup>>(&record.groups_json) {
                Ok(groups) => match validate_strategy(
                    ReviewSplitStrategy {
                        strategy_id: record.strategy_id,
                        groups,
                        rationale: record.rationale,
                        expected_reviewer_count_savings: record.expected_reviewer_count_savings,
                        risk_notes: serde_json::from_str(&record.risk_notes_json)
                            .unwrap_or_default(),
                    },
                    selected,
                    input.max_review_items_per_group,
                ) {
                    Ok(strategy) => Some(strategy),
                    Err(rejection) => {
                        proposal_status = Some(GroupingProposalStatus {
                            status: "ignoredStoredSplit".to_string(),
                            reason: rejection.reason,
                        });
                        None
                    }
                },
                Err(error) => {
                    proposal_status = Some(GroupingProposalStatus {
                        status: "ignoredStoredSplit".to_string(),
                        reason: format!("stored split groups JSON is invalid: {error}"),
                    });
                    None
                }
            }
        })
        .unwrap_or_else(|| separate.clone());
    let rejected_count = agents.state.rejected_grouping_experiment_count(
        &model.score_key,
        &repo_tshirt_bucket,
        &item_set_key,
    )?;
    let mut plan = ReviewSplitPlan {
        active_strategy: active.clone(),
        primary_strategy: active,
        baseline_strategy: None,
        review_item_count: selected.len(),
        item_set_key,
        repo_tshirt_bucket,
        model_key: model.score_key.clone(),
        ai_proposed_challenger: None,
        rejected_grouping_experiments: rejected_count,
        max_rejected_grouping_experiments: input.max_rejected_grouping_experiments,
        stop_reason: None,
        proposal_status,
        baseline_reason: None,
    };

    if !input.grouping_optimization_enabled {
        plan.stop_reason = Some("grouping optimization disabled".to_string());
        return Ok(plan.with_optional_baseline(input, agents.run_id, selected));
    }
    if selected.len() <= 1 {
        plan.stop_reason = Some("only one review item selected".to_string());
        return Ok(plan);
    }
    if rejected_count >= input.max_rejected_grouping_experiments {
        plan.stop_reason = Some(format!(
            "stopped after {rejected_count} rejected grouping experiments"
        ));
        return Ok(plan.with_optional_baseline(input, agents.run_id, selected));
    }
    if !agents
        .state
        .has_split_evidence_for_model(&model.score_key)?
    {
        plan.stop_reason = Some(
            "new model key starts with separate:v1 until baseline evidence exists".to_string(),
        );
        return Ok(plan);
    }
    if !should_sample(
        input.experiment_sample_rate,
        &[
            agents.run_id,
            &model.score_key,
            &plan.repo_tshirt_bucket,
            &plan.item_set_key,
            "grouping-challenger",
        ],
    ) {
        plan.stop_reason = Some("grouping challenger not sampled for this run".to_string());
        return Ok(plan.with_optional_baseline(input, agents.run_id, selected));
    }

    let prompt = match grouping_proposal_prompt(
        agents.state,
        &model.score_key,
        &plan.repo_tshirt_bucket,
        &plan.item_set_key,
        selected,
        input.max_review_items_per_group,
    ) {
        Ok(prompt) => prompt,
        Err(error) => {
            return Ok(plan
                .with_proposal_failure(format!("could not build grouping proposal prompt: {error}"))
                .with_optional_baseline(input, agents.run_id, selected));
        }
    };
    let proposer = match spawn_agent(
        agents,
        AgentSpawnSpec {
            role: "review-split-proposer",
            name: "review-split-proposer".to_string(),
            prompt: prompt.clone(),
            cwd: agents.cwd.to_path_buf(),
            writable: false,
            model: Some(model.model.clone()),
        },
    )
    .await
    {
        Ok(proposer) => proposer,
        Err(error) => {
            return Ok(plan
                .with_proposal_failure(format!("could not start grouping proposal agent: {error}"))
                .with_optional_baseline(input, agents.run_id, selected));
        }
    };
    let output = match agents.runtime.wait_for_output(&proposer.id).await {
        Ok(output) => output,
        Err(error) => {
            let reason = format!("grouping proposal agent failed: {error}");
            let _ = agents.state.record_agent_attempt(AgentAttemptRecord {
                run_id: agents.run_id,
                role: "review-split-proposer",
                name: "review-split-proposer",
                agent_id: Some(&proposer.id),
                model_key: Some(&model.score_key),
                status: "failed",
                prompt: "",
                output_json: Some(&reason),
            });
            let _ = agents.state.record_split_proposal(SplitProposalRecord {
                run_id: agents.run_id,
                model_key: &model.score_key,
                repo_tshirt_bucket: &plan.repo_tshirt_bucket,
                item_set_key: &plan.item_set_key,
                strategy_id: "unavailable",
                groups_json: "[]",
                rationale: "",
                expected_reviewer_count_savings: 0.0,
                risk_notes_json: "[]",
                prompt: &prompt,
                raw_output_json: Some(&reason),
                status: "failed",
                rejection_code: Some("proposer_error"),
                rejection_reason: Some(&reason),
            });
            return Ok(plan.with_proposal_failure(reason).with_optional_baseline(
                input,
                agents.run_id,
                selected,
            ));
        }
    };
    let _ = agents.state.record_agent_attempt(AgentAttemptRecord {
        run_id: agents.run_id,
        role: "review-split-proposer",
        name: "review-split-proposer",
        agent_id: Some(&proposer.id),
        model_key: Some(&model.score_key),
        status: "completed",
        prompt: "",
        output_json: Some(&output.text),
    });

    match validate_ai_output(
        &output.text,
        selected,
        input.max_review_items_per_group,
        agents.state,
        &model.score_key,
        &plan.repo_tshirt_bucket,
        &plan.item_set_key,
    ) {
        Ok(strategy) => {
            let groups_json = serde_json::to_string(&strategy.groups)?;
            let risk_notes_json = serde_json::to_string(&strategy.risk_notes)?;
            let record_result = agents.state.record_split_proposal(SplitProposalRecord {
                run_id: agents.run_id,
                model_key: &model.score_key,
                repo_tshirt_bucket: &plan.repo_tshirt_bucket,
                item_set_key: &plan.item_set_key,
                strategy_id: &strategy.strategy_id,
                groups_json: &groups_json,
                rationale: &strategy.rationale,
                expected_reviewer_count_savings: strategy.expected_reviewer_count_savings,
                risk_notes_json: &risk_notes_json,
                prompt: &prompt,
                raw_output_json: Some(&output.text),
                status: "accepted",
                rejection_code: None,
                rejection_reason: None,
            });
            plan.baseline_strategy = Some(separate);
            plan.baseline_reason = Some("challenger_quality_gate".to_string());
            plan.primary_strategy = strategy.clone();
            plan.ai_proposed_challenger = Some(strategy);
            plan.proposal_status = Some(match record_result {
                Ok(()) => GroupingProposalStatus {
                    status: "accepted".to_string(),
                    reason: "AI proposal passed partition validation".to_string(),
                },
                Err(error) => GroupingProposalStatus {
                    status: "acceptedWithPersistenceWarning".to_string(),
                    reason: format!(
                        "AI proposal passed partition validation, but proposal metadata was not recorded: {error}"
                    ),
                },
            });
            Ok(plan)
        }
        Err(rejection) => {
            let record_result = agents.state.record_split_proposal(SplitProposalRecord {
                run_id: agents.run_id,
                model_key: &model.score_key,
                repo_tshirt_bucket: &plan.repo_tshirt_bucket,
                item_set_key: &plan.item_set_key,
                strategy_id: "invalid",
                groups_json: "[]",
                rationale: "",
                expected_reviewer_count_savings: 0.0,
                risk_notes_json: "[]",
                prompt: &prompt,
                raw_output_json: Some(&output.text),
                status: "rejected",
                rejection_code: Some(rejection.code),
                rejection_reason: Some(&rejection.reason),
            });
            let reason = match record_result {
                Ok(()) => rejection.reason,
                Err(error) => format!(
                    "{}; proposal rejection metadata was not recorded: {error}",
                    rejection.reason
                ),
            };
            plan.proposal_status = Some(GroupingProposalStatus {
                status: "rejected".to_string(),
                reason,
            });
            Ok(plan.with_optional_baseline(input, agents.run_id, selected))
        }
    }
}

pub(crate) fn review_split_output(
    plan: &ReviewSplitPlan,
    outcome: ReviewSplitOutcome,
) -> ReviewSplitOutput {
    let cost_savings = plan.review_item_count as i64 - plan.primary_strategy.groups.len() as i64;
    let zero_grouping = plan.primary_strategy.strategy_id == SEPARATE_STRATEGY_ID;
    ReviewSplitOutput {
        active_split: plan.active_strategy.clone(),
        primary_split: plan.primary_strategy.clone(),
        zero_grouping,
        mode: if zero_grouping {
            "zeroGrouping".to_string()
        } else {
            "grouped".to_string()
        },
        ai_proposed_challenger: plan.ai_proposed_challenger.clone(),
        cost_savings,
        lost_evidence_count: outcome.lost_evidence_count,
        stop_reason: plan.stop_reason.clone(),
        suppression_reason: outcome.suppression_reason,
        promotion_reason: outcome.promotion_reason,
        proposal_status: plan.proposal_status.clone(),
        baseline_resample: BaselineResampleOutput {
            ran: plan.baseline_strategy.is_some(),
            reason: plan.baseline_reason.clone(),
            lost_evidence_count: outcome.lost_evidence_count,
        },
        rejected_grouping_experiments: plan.rejected_grouping_experiments,
        max_rejected_grouping_experiments: plan.max_rejected_grouping_experiments,
        repo_tshirt_bucket: plan.repo_tshirt_bucket.clone(),
        item_set_key: plan.item_set_key.clone(),
        model_key: plan.model_key.clone(),
    }
}

pub(crate) fn validate_strategy(
    proposal: ReviewSplitStrategy,
    selected: &[ReviewTypeDefinition],
    max_review_items_per_group: u32,
) -> Result<ReviewSplitStrategy, ProposalRejection> {
    let selected_ids = selected
        .iter()
        .map(|review_type| review_type.id.as_str())
        .collect::<BTreeSet<_>>();
    if proposal.strategy_id.trim().is_empty() {
        return Err(rejection("missing_strategy_id", "strategyId is required"));
    }
    if proposal
        .strategy_id
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(rejection(
            "invalid_strategy_id",
            "strategyId cannot contain whitespace or control characters",
        ));
    }
    if proposal.strategy_id == SEPARATE_STRATEGY_ID {
        return Err(rejection(
            "no_cost_savings",
            "AI challenger must not propose separate:v1",
        ));
    }
    if proposal.groups.is_empty() {
        return Err(rejection("missing_groups", "proposal must include groups"));
    }
    if proposal.groups.len() >= selected.len() {
        return Err(rejection(
            "no_cost_savings",
            "proposal does not reduce reviewer group count",
        ));
    }
    if proposal.expected_reviewer_count_savings < 0.0 {
        return Err(rejection(
            "invalid_expected_savings",
            "expectedReviewerCountSavings cannot be negative",
        ));
    }

    let mut seen = BTreeSet::new();
    let mut group_ids = BTreeSet::new();
    for group in &proposal.groups {
        if group.group_id.trim().is_empty() {
            return Err(rejection("missing_group_id", "review group id is required"));
        }
        if !group_ids.insert(group.group_id.as_str()) {
            return Err(rejection(
                "duplicate_group_id",
                format!("proposal uses group id '{}' more than once", group.group_id),
            ));
        }
        if group.review_type_ids.is_empty() {
            return Err(rejection("empty_group", "review groups cannot be empty"));
        }
        if group.review_type_ids.len() > max_review_items_per_group as usize {
            return Err(rejection(
                "oversized_group",
                format!(
                    "group '{}' contains {} review items; maxReviewItemsPerGroup is {}",
                    group.group_id,
                    group.review_type_ids.len(),
                    max_review_items_per_group
                ),
            ));
        }
        for id in &group.review_type_ids {
            if !selected_ids.contains(id.as_str()) {
                return Err(rejection(
                    "unknown_item",
                    format!("proposal references unknown review item '{id}'"),
                ));
            }
            if !seen.insert(id.clone()) {
                return Err(rejection(
                    "duplicate_item",
                    format!("proposal assigns review item '{id}' more than once"),
                ));
            }
        }
    }
    let missing = selected_ids
        .into_iter()
        .filter(|id| !seen.contains(*id))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(rejection(
            "missing_item",
            format!("proposal omits review item(s): {}", missing.join(", ")),
        ));
    }
    Ok(proposal)
}

pub(crate) fn finding_fingerprint(
    review_type_id: &str,
    title: &str,
    details: &str,
    file_path: Option<&str>,
    line: Option<u32>,
) -> String {
    let normalized = format!(
        "{}\n{}\n{}\n{}\n{}",
        normalize_for_fingerprint(review_type_id),
        normalize_for_fingerprint(title),
        normalize_for_fingerprint(details),
        normalize_for_fingerprint(file_path.unwrap_or("")),
        line.map(|line| line.to_string()).unwrap_or_default()
    );
    digest(&normalized)
}

pub(crate) fn strategy_neighborhood_keys(strategy: &ReviewSplitStrategy) -> Vec<String> {
    strategy
        .groups
        .iter()
        .filter(|group| group.review_type_ids.len() > 1)
        .map(|group| neighborhood_key(&group.review_type_ids))
        .collect()
}

fn validate_ai_output(
    text: &str,
    selected: &[ReviewTypeDefinition],
    max_review_items_per_group: u32,
    state: &DevCycleState,
    model_key: &str,
    repo_tshirt_bucket: &str,
    item_set_key: &str,
) -> Result<ReviewSplitStrategy, ProposalRejection> {
    let proposal = serde_json::from_str::<AiGroupingProposal>(text)
        .map_err(|error| rejection("invalid_json", format!("invalid proposal JSON: {error}")))?;
    let strategy = normalize_ai_proposal(proposal);
    let strategy = validate_strategy(strategy, selected, max_review_items_per_group)?;
    if state
        .split_strategy_seen(
            model_key,
            repo_tshirt_bucket,
            item_set_key,
            &strategy.strategy_id,
        )
        .map_err(|error| rejection("state_error", error.to_string()))?
    {
        return Err(rejection(
            "duplicate_strategy",
            format!("strategy '{}' was already tried", strategy.strategy_id),
        ));
    }
    let suppressed = state
        .suppressed_split_neighborhoods(model_key, repo_tshirt_bucket, item_set_key)
        .map_err(|error| rejection("state_error", error.to_string()))?;
    for group in &strategy.groups {
        if group.review_type_ids.len() <= 1 {
            continue;
        }
        let proposed = group
            .review_type_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for failed in &suppressed {
            if failed.iter().all(|id| proposed.contains(id)) {
                return Err(rejection(
                    "larger_group_after_failed_neighborhood",
                    format!(
                        "group '{}' includes failed item neighborhood {}",
                        group.group_id,
                        failed.join("+")
                    ),
                ));
            }
        }
    }
    Ok(strategy)
}

fn normalize_ai_proposal(proposal: AiGroupingProposal) -> ReviewSplitStrategy {
    ReviewSplitStrategy {
        strategy_id: proposal.strategy_id,
        groups: proposal
            .groups
            .into_iter()
            .enumerate()
            .map(|(index, group)| match group {
                AiGroupingGroup::Object {
                    group_id,
                    review_type_ids,
                } => ReviewSplitGroup {
                    group_id: group_id.unwrap_or_else(|| format!("group-{}", index + 1)),
                    review_type_ids,
                },
                AiGroupingGroup::Ids(review_type_ids) => ReviewSplitGroup {
                    group_id: format!("group-{}", index + 1),
                    review_type_ids,
                },
            })
            .collect(),
        rationale: proposal.rationale,
        expected_reviewer_count_savings: proposal.expected_reviewer_count_savings,
        risk_notes: proposal.risk_notes,
    }
}

fn grouping_proposal_prompt(
    state: &DevCycleState,
    model_key: &str,
    repo_tshirt_bucket: &str,
    item_set_key: &str,
    selected: &[ReviewTypeDefinition],
    max_review_items_per_group: u32,
) -> anyhow::Result<String> {
    let evidence = state.split_evidence_for_prompt(model_key, repo_tshirt_bucket, item_set_key)?;
    let items = selected
        .iter()
        .map(|review_type| {
            json!({
                "id": review_type.id,
                "shortName": review_type.short_name,
                "description": review_type.description,
            })
        })
        .collect::<Vec<_>>();
    Ok(format!(
        "You are optimizing dev-cycle reviewer grouping for the same target model that will run the review.\n\
         Propose exactly one next grouping experiment that maximizes reviewer-count savings while preserving review quality.\n\
         Evidence never transfers across model keys; use only the evidence below for model `{model_key}`.\n\
         A single verifier-accepted finding lost versus separate:v1 fails a grouping.\n\
         Do not group more than {max_review_items_per_group} review items together.\n\n\
         Selected review items:\n{}\n\n\
         Historical split evidence for this model across all repos and this repo-size bucket:\n{}\n\n\
         Return only JSON with fields strategyId, groups, rationale, expectedReviewerCountSavings, and riskNotes. \
         groups must be a partition of every selected id, for example \
         {{\"strategyId\":\"grouping:v1\",\"groups\":[{{\"groupId\":\"core\",\"reviewTypeIds\":[\"correctness\",\"tests\"]}}],\"rationale\":\"...\",\"expectedReviewerCountSavings\":1,\"riskNotes\":[\"...\"]}}.",
        serde_json::to_string_pretty(&items)?,
        serde_json::to_string_pretty(&evidence)?,
    ))
}

fn should_sample(rate: f64, seed_parts: &[&str]) -> bool {
    if rate <= 0.0 {
        return false;
    }
    if rate >= 1.0 {
        return true;
    }
    let digest = Sha256::digest(seed_parts.join("\n").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let value = u64::from_be_bytes(bytes) as f64 / u64::MAX as f64;
    value < rate
}

fn neighborhood_key(ids: &[String]) -> String {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.join("+")
}

fn normalize_for_fingerprint(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rejection(code: &'static str, reason: impl Into<String>) -> ProposalRejection {
    ProposalRejection {
        code,
        reason: reason.into(),
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

impl ReviewSplitPlan {
    fn with_proposal_failure(mut self, reason: String) -> Self {
        self.stop_reason = Some("grouping proposal unavailable".to_string());
        self.proposal_status = Some(GroupingProposalStatus {
            status: "failed".to_string(),
            reason,
        });
        self
    }

    fn with_optional_baseline(
        mut self,
        input: &DevCycleInput,
        run_id: &str,
        selected: &[ReviewTypeDefinition],
    ) -> Self {
        if self.primary_strategy.strategy_id == SEPARATE_STRATEGY_ID {
            return self;
        }
        if should_sample(
            input.baseline_resample_rate,
            &[
                run_id,
                &self.model_key,
                &self.repo_tshirt_bucket,
                &self.item_set_key,
                "baseline-resample",
            ],
        ) {
            self.baseline_strategy = Some(separate_strategy(selected));
            self.baseline_reason = Some("baseline_resample_rate".to_string());
        }
        self
    }
}

#[cfg(test)]
#[path = "review_split_tests.rs"]
mod tests;
