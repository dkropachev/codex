use std::collections::BTreeSet;

use codex_native_workflow::NativeWorkflowAgentHandle;
use serde::Deserialize;
use serde::Serialize;

use crate::agents::AgentExecutionContext;
use crate::agents::AgentSpawnSpec;
use crate::agents::spawn_agent;
use crate::models::ReviewModelChoice;
use crate::persistence::AgentAttemptRecord;
use crate::persistence::DisregardedFinding;
use crate::persistence::FindingRecord;
use crate::persistence::VerifierDecisionRecord;
use crate::review_split::ReviewSplitGroup;
use crate::review_split::ReviewSplitStrategy;
use crate::review_split::SEPARATE_STRATEGY_ID;
use crate::review_types::ReviewTypeDefinition;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CandidateFinding {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    review_type_id: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    details: String,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default = "default_severity")]
    severity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifiedFinding {
    pub(crate) id: String,
    pub(crate) review_type_id: String,
    pub(crate) title: String,
    pub(crate) details: String,
    pub(crate) file_path: Option<String>,
    pub(crate) line: Option<u32>,
    pub(crate) severity: String,
    pub(crate) verifier_agent_id: String,
    pub(crate) reason: String,
    pub(crate) split_strategy_id: String,
    pub(crate) shadow_baseline: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewOutput {
    #[serde(default)]
    findings: Vec<CandidateFinding>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifierOutput {
    #[serde(default)]
    accepted: bool,
    #[serde(default)]
    reason: String,
}

pub(crate) struct ReviewStageContext<'a> {
    pub(crate) agents: &'a AgentExecutionContext<'a>,
    pub(crate) writer: &'a NativeWorkflowAgentHandle,
    pub(crate) model: Option<&'a ReviewModelChoice>,
    pub(crate) excluded: &'a [ReviewTypeDefinition],
}

pub(crate) struct VerifierStageContext<'a> {
    pub(crate) agents: &'a AgentExecutionContext<'a>,
    pub(crate) writer: &'a NativeWorkflowAgentHandle,
    pub(crate) model: Option<&'a ReviewModelChoice>,
    pub(crate) repo_family: &'a str,
    pub(crate) work_size_units: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewRunKind {
    Primary,
    ShadowBaseline,
}

pub(crate) struct ReviewRunResult {
    reviewer_agent_id: String,
    model_key: String,
    split_strategy_id: String,
    kind: ReviewRunKind,
    findings: Vec<CandidateFinding>,
}

pub(crate) async fn run_reviewers(
    ctx: ReviewStageContext<'_>,
    selected: &[ReviewTypeDefinition],
    strategy: &ReviewSplitStrategy,
    kind: ReviewRunKind,
) -> anyhow::Result<Vec<ReviewRunResult>> {
    let mut reviewers = Vec::new();
    for (group_index, group) in strategy.groups.iter().enumerate() {
        let group_types = group
            .review_type_ids
            .iter()
            .filter_map(|id| selected.iter().find(|review_type| review_type.id == *id))
            .cloned()
            .collect::<Vec<_>>();
        let group_id_set = group
            .review_type_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut excluded = ctx.excluded.to_vec();
        excluded.extend(
            selected
                .iter()
                .filter(|review_type| !group_id_set.contains(review_type.id.as_str()))
                .cloned(),
        );
        let mut disregarded = Vec::new();
        for review_type in &group_types {
            disregarded.extend(
                ctx.agents
                    .state
                    .recent_disregarded_findings(&review_type.id, 10)?,
            );
        }
        let reviewer_name = reviewer_name(strategy, kind, group, group_index);
        let handle = spawn_agent(
            ctx.agents,
            AgentSpawnSpec {
                role: "reviewer",
                name: reviewer_name,
                prompt: reviewer_prompt(&group_types, &excluded, &disregarded),
                cwd: ctx.agents.cwd.to_path_buf(),
                writable: false,
                model: ctx.model.map(|choice| choice.model.clone()),
            },
        )
        .await?;
        reviewers.push((group.clone(), handle));
    }

    let mut results = Vec::new();
    for (group, handle) in reviewers {
        let output = ctx.agents.runtime.wait_for_output(&handle.id).await?;
        let parsed = parse_review_output(&output.text);
        let model_key = ctx
            .model
            .map(|choice| choice.score_key.clone())
            .unwrap_or_else(|| "unavailable".to_string());
        let mut attributed_findings = Vec::new();
        for (index, mut finding) in parsed.findings.into_iter().enumerate() {
            if finding.title.trim().is_empty() || finding.details.trim().is_empty() {
                ctx.agents.state.record_disregarded_finding(
                    ctx.agents.run_id,
                    group
                        .review_type_ids
                        .first()
                        .map(String::as_str)
                        .unwrap_or("unknown"),
                    if finding.title.trim().is_empty() {
                        "invalid reviewer finding"
                    } else {
                        &finding.title
                    },
                    "reviewer finding omitted title or details",
                    "invalid_finding_shape",
                )?;
                continue;
            }
            let Some(review_type_id) = attributed_review_type_id(&finding, &group) else {
                ctx.agents.state.record_disregarded_finding(
                    ctx.agents.run_id,
                    group
                        .review_type_ids
                        .first()
                        .map(String::as_str)
                        .unwrap_or("unknown"),
                    if finding.title.is_empty() {
                        "invalid grouped finding attribution"
                    } else {
                        &finding.title
                    },
                    "grouped finding omitted or used an invalid reviewTypeId",
                    "invalid_attribution",
                )?;
                continue;
            };
            finding.review_type_id = Some(review_type_id.clone());
            let finding_id = finding.id.clone().unwrap_or_else(|| {
                format!(
                    "{}-{}-{review_type_id}-{index}",
                    ctx.agents.run_id,
                    safe_id_component(&strategy.strategy_id)
                )
            });
            ctx.agents.state.record_finding(FindingRecord {
                id: &finding_id,
                run_id: ctx.agents.run_id,
                review_type_id: &review_type_id,
                producer_agent_id: &handle.id,
                writer_agent_id: Some(&ctx.writer.id),
                title: &finding.title,
                details: &finding.details,
                file_path: finding.file_path.as_deref(),
                line: finding.line,
                severity: &finding.severity,
                status: "candidate",
            })?;
            attributed_findings.push(finding);
        }
        results.push(ReviewRunResult {
            reviewer_agent_id: handle.id,
            model_key,
            split_strategy_id: strategy.strategy_id.clone(),
            kind,
            findings: attributed_findings,
        });
    }
    Ok(results)
}

pub(crate) async fn run_verifiers(
    ctx: VerifierStageContext<'_>,
    review_results: Vec<ReviewRunResult>,
) -> anyhow::Result<(Vec<VerifiedFinding>, Vec<DisregardedFinding>)> {
    let mut verifier_jobs = Vec::new();
    for review_result in review_results {
        for (index, finding) in review_result.findings.into_iter().enumerate() {
            let review_type_id = finding
                .review_type_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let finding_id = finding.id.clone().unwrap_or_else(|| {
                format!(
                    "{}-{}-{}-{index}",
                    ctx.agents.run_id,
                    safe_id_component(&review_result.split_strategy_id),
                    review_type_id
                )
            });
            let prompt = format!(
                "Verify this finding in a clean separate thread. Return JSON {{\"accepted\":true|false,\"reason\":\"...\"}}.\n\nReview type: {}\nTitle: {}\nDetails: {}\nFile: {}",
                review_type_id,
                finding.title,
                finding.details,
                finding.file_path.as_deref().unwrap_or("<unspecified>")
            );
            let handle = spawn_agent(
                ctx.agents,
                AgentSpawnSpec {
                    role: "verifier",
                    name: format!("verifier-{finding_id}"),
                    prompt,
                    cwd: ctx.agents.cwd.to_path_buf(),
                    writable: false,
                    model: ctx.model.map(|choice| choice.model.clone()),
                },
            )
            .await?;
            verifier_jobs.push((
                review_type_id,
                review_result.model_key.clone(),
                review_result.reviewer_agent_id.clone(),
                review_result.split_strategy_id.clone(),
                review_result.kind,
                finding_id,
                finding,
                handle,
            ));
        }
    }

    let mut verified = Vec::new();
    let mut disregarded = Vec::new();
    for (
        review_type_id,
        model_key,
        _reviewer_agent_id,
        split_strategy_id,
        kind,
        finding_id,
        finding,
        handle,
    ) in verifier_jobs
    {
        let output = ctx.agents.runtime.wait_for_output(&handle.id).await?;
        let decision = parse_verifier_output(&output.text);
        let reason = if decision.reason.is_empty() {
            "verifier returned no reason".to_string()
        } else {
            decision.reason
        };
        ctx.agents
            .state
            .record_verifier_decision(VerifierDecisionRecord {
                finding_id: &finding_id,
                verifier_agent_id: &handle.id,
                model_key: &model_key,
                accepted: decision.accepted,
                reason: &reason,
                work_size_units: ctx.work_size_units,
                repo_family: ctx.repo_family,
                review_type_id: &review_type_id,
            })?;
        if decision.accepted {
            verified.push(VerifiedFinding {
                id: finding_id,
                review_type_id,
                title: finding.title,
                details: finding.details,
                file_path: finding.file_path,
                line: finding.line,
                severity: finding.severity,
                verifier_agent_id: handle.id,
                reason,
                split_strategy_id,
                shadow_baseline: kind == ReviewRunKind::ShadowBaseline,
            });
        } else {
            ctx.agents.state.record_disregarded_finding(
                ctx.agents.run_id,
                &review_type_id,
                &finding.title,
                &reason,
                "verifier_rejected",
            )?;
            disregarded.push(DisregardedFinding {
                review_type_id,
                title: finding.title,
                reason,
                status: "verifier_rejected".to_string(),
            });
        }
    }
    if verified.is_empty() && disregarded.is_empty() {
        ctx.agents.state.record_agent_attempt(AgentAttemptRecord {
            run_id: ctx.agents.run_id,
            role: "verifier",
            name: "verifiers",
            agent_id: Some(&ctx.writer.id),
            model_key: None,
            status: "skipped",
            prompt: "no findings to verify",
            output_json: None,
        })?;
    }
    Ok((verified, disregarded))
}

fn reviewer_prompt(
    review_types: &[ReviewTypeDefinition],
    excluded: &[ReviewTypeDefinition],
    disregarded: &[DisregardedFinding],
) -> String {
    let review_text = review_types
        .iter()
        .map(|definition| {
            format!(
                "- {}: {}\n  {}",
                definition.id,
                definition.description,
                definition.prompt.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let valid_ids = review_types
        .iter()
        .map(|definition| format!("`{}`", definition.id))
        .collect::<Vec<_>>()
        .join(", ");
    let excluded_text = excluded
        .iter()
        .map(|definition| {
            format!(
                "- {}: {}",
                definition.id,
                definition
                    .exclude_prompt
                    .as_deref()
                    .unwrap_or(&definition.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let disregarded_text = disregarded
        .iter()
        .map(|finding| format!("- {} ({})", finding.title, finding.reason))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Review only these dev-cycle review items:\n{}\n\nSet reviewTypeId on every finding to one of: {}.\n\nExcluded topics:\n{}\n\nPreviously disregarded findings not to re-raise:\n{}\n\nReturn JSON {{\"findings\":[{{\"reviewTypeId\":\"...\",\"title\":\"...\",\"details\":\"...\",\"filePath\":\"...\",\"line\":1,\"severity\":\"medium\"}}]}}.",
        if review_text.is_empty() {
            "- none"
        } else {
            &review_text
        },
        if valid_ids.is_empty() {
            "`unknown`"
        } else {
            &valid_ids
        },
        if excluded_text.is_empty() {
            "- none"
        } else {
            &excluded_text
        },
        if disregarded_text.is_empty() {
            "- none"
        } else {
            &disregarded_text
        }
    )
}

fn parse_review_output(text: &str) -> ReviewOutput {
    if let Ok(output) = serde_json::from_str::<ReviewOutput>(text) {
        return output;
    }
    serde_json::from_str::<Vec<CandidateFinding>>(text)
        .map(|findings| ReviewOutput { findings })
        .unwrap_or_default()
}

fn parse_verifier_output(text: &str) -> VerifierOutput {
    if let Ok(output) = serde_json::from_str::<VerifierOutput>(text) {
        return output;
    }
    VerifierOutput {
        accepted: text.to_ascii_lowercase().contains("accepted"),
        reason: text.trim().to_string(),
    }
}

fn attributed_review_type_id(
    finding: &CandidateFinding,
    group: &ReviewSplitGroup,
) -> Option<String> {
    if group.review_type_ids.len() == 1 {
        return group.review_type_ids.first().cloned();
    }
    let review_type_id = finding.review_type_id.as_ref()?;
    group
        .review_type_ids
        .iter()
        .any(|id| id == review_type_id)
        .then(|| review_type_id.clone())
}

fn reviewer_name(
    strategy: &ReviewSplitStrategy,
    kind: ReviewRunKind,
    group: &ReviewSplitGroup,
    group_index: usize,
) -> String {
    if strategy.strategy_id == SEPARATE_STRATEGY_ID
        && kind == ReviewRunKind::Primary
        && group.review_type_ids.len() == 1
    {
        return format!("reviewer-{}", group.review_type_ids[0]);
    }
    let prefix = match kind {
        ReviewRunKind::Primary => "reviewer",
        ReviewRunKind::ShadowBaseline => "baseline-reviewer",
    };
    let mut name = format!(
        "{prefix}-{}-{}",
        safe_id_component(&strategy.strategy_id),
        safe_id_component(
            group
                .review_type_ids
                .first()
                .map(String::as_str)
                .unwrap_or(&group.group_id)
        )
    )
    .chars()
    .take(80)
    .collect::<String>();
    name.push_str(&format!("-{}", group_index + 1));
    name
}

fn safe_id_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn default_severity() -> String {
    "medium".to_string()
}
