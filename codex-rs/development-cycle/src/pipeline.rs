use std::collections::BTreeSet;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_native_workflow::NativeWorkflowAgentOutput;
use codex_native_workflow::NativeWorkflowAgentTurnRequest;
use codex_native_workflow::NativeWorkflowRunContext;
use codex_native_workflow::NativeWorkflowRunOutput;
use codex_native_workflow::NativeWorkflowStatusUpdate;
use codex_native_workflow::NativeWorkflowThreadStatus;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::DEVELOPMENT_CYCLE_WORKFLOW_ID;
use crate::agents::AgentExecutionContext;
use crate::agents::AgentSpawnSpec;
use crate::agents::spawn_agent;
use crate::execution::prepare_writer_worktree;
use crate::execution::run_tests;
use crate::experiment::experiment_decisions;
use crate::input::DevCycleInput;
use crate::input::parse_input;
use crate::models::primary_review_model;
use crate::output::FinalOutputParts;
use crate::output::final_output;
use crate::output::format_markdown;
use crate::persistence::AgentAttemptRecord;
use crate::persistence::DevCycleState;
use crate::review_split::ReviewSplitOutcome;
use crate::review_split::ReviewSplitOutput;
use crate::review_split::ReviewSplitPlan;
use crate::review_split::SEPARATE_STRATEGY_ID;
use crate::review_split::finding_fingerprint;
use crate::review_split::prepare_review_split;
use crate::review_split::review_split_output;
use crate::review_split::separate_review_split_plan;
use crate::review_split::strategy_neighborhood_keys;
use crate::review_stage::ReviewRunKind;
use crate::review_stage::ReviewStageContext;
use crate::review_stage::VerifiedFinding;
use crate::review_stage::VerifierStageContext;
use crate::review_stage::run_reviewers;
use crate::review_stage::run_verifiers;
use crate::review_types::merge_review_types;
use crate::review_types::select_review_types;
use crate::split_persistence::SplitAttemptRecord;
use crate::split_persistence::SplitLostEvidenceRecord;
use crate::split_persistence::SplitPromotionRecord;
use crate::split_persistence::SplitScoreRecord;
use crate::split_persistence::SplitSuppressionRecord;
use crate::work_size::repo_snapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WriterCommit {
    writer_agent_id: String,
    commit: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriterOutput {
    #[serde(default)]
    commits: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntegratorOutput {
    #[serde(default)]
    integration_branch: Option<String>,
}

pub(crate) async fn run_dev_cycle(
    ctx: NativeWorkflowRunContext<'_>,
    input: JsonValue,
) -> anyhow::Result<NativeWorkflowRunOutput> {
    ctx.ensure_not_cancelled()?;
    let (input, normalized_input) = parse_input(input)?;
    let definitions = merge_review_types(input.review_type_definitions.clone())?;
    let (selected_review_types, excluded_review_types) =
        select_review_types(&definitions, input.review_types.as_deref())?;
    let repo = repo_snapshot(ctx.cwd);
    let state = DevCycleState::open(ctx.state_dir)?;
    let run_id = new_run_id();
    state.start_run(
        &run_id,
        &repo,
        input.task_description.as_deref(),
        &selected_review_types,
    )?;
    let model_choice = ctx
        .model_provider_catalog
        .and_then(|catalog| primary_review_model(catalog.model_candidates()));
    let decisions = experiment_decisions(
        &state,
        &repo.repo_family,
        &selected_review_types,
        model_choice.as_ref(),
        input.min_evidence_runs,
        input.effort_lowering_enabled,
    )?;
    for decision in &decisions {
        state.record_experiment_decision(&run_id, decision)?;
    }
    let fallback_split_plan = separate_review_split_plan(
        &input,
        &repo,
        &selected_review_types,
        model_choice.as_ref(),
        Some("review split proposer has not run".to_string()),
    );
    let fallback_review_split =
        review_split_output(&fallback_split_plan, ReviewSplitOutcome::default());

    ctx.status(status("planning", [("planner", "creating work packets")]));
    ctx.progress(
        "Started native development cycle",
        Some(json!({
            "workflowId": DEVELOPMENT_CYCLE_WORKFLOW_ID,
            "runId": run_id,
            "stateDatabase": state.path().display().to_string(),
        })),
    );

    let Some(agent_runtime) = ctx.agent_runtime else {
        state.finish_run(&run_id, "blocked")?;
        let output = final_output(FinalOutputParts {
            status: "blocked",
            ctx: &ctx,
            state: &state,
            normalized_input: &normalized_input,
            selected_review_types: &selected_review_types,
            excluded_review_types: &excluded_review_types,
            writer_commits: &[],
            verified_findings: &[],
            disregarded_findings: &[],
            test_results: &[],
            integration_branch: None,
            experiment_decisions: &decisions,
            review_split: &fallback_review_split,
            blocked_reason: Some("native agent runtime is unavailable"),
        });
        let markdown = format_markdown(&output);
        if ctx.output_format == Some("tui.markdown.v1") {
            ctx.report_to_user_markdown(markdown.clone());
        }
        return Ok(NativeWorkflowRunOutput {
            output,
            final_markdown: Some(markdown),
        });
    };
    let agents = AgentExecutionContext {
        runtime: agent_runtime,
        state: &state,
        run_id: &run_id,
        cwd: ctx.cwd,
    };

    let planner_prompt = format!(
        "Plan a development-cycle run for this task.\n\nTask:\n{}\n\nReturn concise work packets and test guidance.",
        input
            .task_description
            .as_deref()
            .unwrap_or("Infer the task from the active Codex thread.")
    );
    let planner = spawn_agent(
        &agents,
        AgentSpawnSpec {
            role: "planner",
            name: "planner".to_string(),
            prompt: planner_prompt,
            cwd: ctx.cwd.to_path_buf(),
            writable: false,
            model: model_choice.as_ref().map(|choice| choice.model.clone()),
        },
    )
    .await?;
    let planner_output = agent_runtime.wait_for_output(&planner.id).await?;
    state.record_agent_attempt(AgentAttemptRecord {
        run_id: &run_id,
        role: "planner",
        name: &planner.name,
        agent_id: Some(&planner.id),
        model_key: None,
        status: "completed",
        prompt: "",
        output_json: Some(&planner_output.text),
    })?;
    ctx.ensure_not_cancelled()?;

    ctx.status(status(
        "implementation",
        [("writer-1", "implementing in isolated worktree")],
    ));
    let writer_cwd = prepare_writer_worktree(ctx.cwd, ctx.state_dir, &run_id, 1);
    let writer_prompt = writer_prompt(&input, &planner_output.text);
    let writer = spawn_agent(
        &agents,
        AgentSpawnSpec {
            role: "writer",
            name: "writer-1".to_string(),
            prompt: writer_prompt,
            cwd: writer_cwd,
            writable: true,
            model: None,
        },
    )
    .await?;
    let writer_output = agent_runtime.wait_for_output(&writer.id).await?;
    let mut writer_commits = parse_writer_commits(&writer.id, &writer_output);
    state.record_agent_attempt(AgentAttemptRecord {
        run_id: &run_id,
        role: "writer",
        name: &writer.name,
        agent_id: Some(&writer.id),
        model_key: None,
        status: "completed",
        prompt: "",
        output_json: Some(&writer_output.text),
    })?;
    ctx.ensure_not_cancelled()?;

    ctx.status(status(
        "review split",
        [("optimizer", "choosing active split and challenger")],
    ));
    let mut split_plan = prepare_review_split(
        &agents,
        &input,
        &repo,
        &selected_review_types,
        model_choice.as_ref(),
    )
    .await?;

    ctx.status(status("review split", split_plan_threads(&split_plan)));
    ctx.status(status(
        "review",
        split_plan.primary_strategy.groups.iter().map(|group| {
            (
                group.group_id.clone(),
                group_review_status(group, "reviewing"),
            )
        }),
    ));
    let review_results = run_reviewers(
        ReviewStageContext {
            agents: &agents,
            writer: &writer,
            model: model_choice.as_ref(),
            excluded: &excluded_review_types,
        },
        &selected_review_types,
        &split_plan.primary_strategy,
        ReviewRunKind::Primary,
    )
    .await?;
    ctx.ensure_not_cancelled()?;

    ctx.status(status(
        "verification",
        [("verifiers", "checking every finding in clean threads")],
    ));
    let (mut verified_findings, mut disregarded_findings) = run_verifiers(
        VerifierStageContext {
            agents: &agents,
            writer: &writer,
            model: model_choice.as_ref(),
            repo_family: &repo.repo_family,
            work_size_units: repo.work_size.work_size_units,
        },
        review_results,
    )
    .await?;
    let mut baseline_verified_findings = Vec::new();
    if let Some(baseline_strategy) = split_plan.baseline_strategy.clone() {
        ctx.status(status(
            "review",
            baseline_strategy.groups.iter().map(|group| {
                (
                    group.group_id.clone(),
                    group_review_status(group, "shadow baseline"),
                )
            }),
        ));
        let baseline_review_results = run_reviewers(
            ReviewStageContext {
                agents: &agents,
                writer: &writer,
                model: model_choice.as_ref(),
                excluded: &excluded_review_types,
            },
            &selected_review_types,
            &baseline_strategy,
            ReviewRunKind::ShadowBaseline,
        )
        .await;
        match baseline_review_results {
            Ok(baseline_review_results) => {
                match run_verifiers(
                    VerifierStageContext {
                        agents: &agents,
                        writer: &writer,
                        model: model_choice.as_ref(),
                        repo_family: &repo.repo_family,
                        work_size_units: repo.work_size.work_size_units,
                    },
                    baseline_review_results,
                )
                .await
                {
                    Ok((baseline_verified, baseline_disregarded)) => {
                        baseline_verified_findings = baseline_verified;
                        disregarded_findings.extend(baseline_disregarded);
                    }
                    Err(error) => {
                        split_plan.baseline_strategy = None;
                        split_plan.baseline_reason =
                            Some(format!("shadow baseline verification failed: {error}"));
                    }
                }
            }
            Err(error) => {
                split_plan.baseline_strategy = None;
                split_plan.baseline_reason =
                    Some(format!("shadow baseline review failed: {error}"));
            }
        }
    }
    let split_outcome = record_review_split_outcome(
        &state,
        &run_id,
        &split_plan,
        &verified_findings,
        &baseline_verified_findings,
    );
    let lost_baseline_findings =
        lost_baseline_findings(&verified_findings, &baseline_verified_findings);
    verified_findings.extend(lost_baseline_findings);
    let review_split = review_split_output(&split_plan, split_outcome);
    ctx.status(status("review split", split_outcome_threads(&review_split)));
    for finding in &verified_findings {
        let fix_prompt = format!(
            "Fix this verified finding in the same writer context and commit the fix.\n\n{}: {}\n{}",
            finding.review_type_id, finding.title, finding.details
        );
        agent_runtime
            .send_follow_up(NativeWorkflowAgentTurnRequest {
                agent_id: writer.id.clone(),
                prompt: fix_prompt,
            })
            .await?;
        let fix_output = agent_runtime.wait_for_output(&writer.id).await?;
        writer_commits.extend(parse_writer_commits(&writer.id, &fix_output));
    }
    ctx.ensure_not_cancelled()?;

    ctx.status(status("tests", [("test runner", "running gates")]));
    let test_results = run_tests(&input, ctx.cwd)?;
    if test_results.iter().any(|result| !result.success) {
        state.finish_run(&run_id, "failed")?;
        let output = final_output(FinalOutputParts {
            status: "failed",
            ctx: &ctx,
            state: &state,
            normalized_input: &normalized_input,
            selected_review_types: &selected_review_types,
            excluded_review_types: &excluded_review_types,
            writer_commits: &writer_commits,
            verified_findings: &verified_findings,
            disregarded_findings: &disregarded_findings,
            test_results: &test_results,
            integration_branch: None,
            experiment_decisions: &decisions,
            review_split: &review_split,
            blocked_reason: Some("test gates failed"),
        });
        let markdown = format_markdown(&output);
        if ctx.output_format == Some("tui.markdown.v1") {
            ctx.report_to_user_markdown(markdown.clone());
        }
        return Ok(NativeWorkflowRunOutput {
            output,
            final_markdown: Some(markdown),
        });
    }
    ctx.status(status(
        "integration",
        [("integrator", "integrating commits")],
    ));
    let integration_prompt = format!(
        "Integrate accepted writer commits into a clean branch/worktree, run final checks, and report JSON {{\"integrationBranch\":\"...\"}}.\n\nCommits:\n{}",
        writer_commits
            .iter()
            .map(|commit| commit.commit.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let integrator = spawn_agent(
        &agents,
        AgentSpawnSpec {
            role: "integrator",
            name: "integrator".to_string(),
            prompt: integration_prompt,
            cwd: ctx.cwd.to_path_buf(),
            writable: true,
            model: None,
        },
    )
    .await?;
    let integration_output = agent_runtime.wait_for_output(&integrator.id).await?;
    let integration_branch = parse_integrator_output(&integration_output.text).integration_branch;
    state.finish_run(&run_id, "succeeded")?;

    let output = final_output(FinalOutputParts {
        status: "succeeded",
        ctx: &ctx,
        state: &state,
        normalized_input: &normalized_input,
        selected_review_types: &selected_review_types,
        excluded_review_types: &excluded_review_types,
        writer_commits: &writer_commits,
        verified_findings: &verified_findings,
        disregarded_findings: &disregarded_findings,
        test_results: &test_results,
        integration_branch: integration_branch.as_deref(),
        experiment_decisions: &decisions,
        review_split: &review_split,
        blocked_reason: None,
    });
    let markdown = format_markdown(&output);
    if ctx.output_format == Some("tui.markdown.v1") {
        ctx.report_to_user_markdown(markdown.clone());
    }
    Ok(NativeWorkflowRunOutput {
        output,
        final_markdown: Some(markdown),
    })
}

fn record_review_split_outcome(
    state: &DevCycleState,
    run_id: &str,
    plan: &ReviewSplitPlan,
    primary_verified: &[VerifiedFinding],
    baseline_verified: &[VerifiedFinding],
) -> ReviewSplitOutcome {
    let groups_json =
        serde_json::to_string(&plan.primary_strategy.groups).unwrap_or_else(|_| "[]".to_string());
    let lost = lost_baseline_findings(primary_verified, baseline_verified);
    let reviewer_count_savings =
        plan.review_item_count as i64 - plan.primary_strategy.groups.len() as i64;
    let baseline_group_count = plan
        .baseline_strategy
        .as_ref()
        .map(|strategy| strategy.groups.len() as u32);
    let primary_is_grouped = plan.primary_strategy.strategy_id != SEPARATE_STRATEGY_ID;
    let checked_against_baseline = plan.baseline_strategy.is_some();
    let accepted_grouping = primary_is_grouped
        && checked_against_baseline
        && lost.is_empty()
        && reviewer_count_savings > 0;
    let status = match (
        primary_is_grouped,
        checked_against_baseline,
        accepted_grouping,
    ) {
        (false, _, _) => "separate",
        (true, false, _) => "grouped_unchecked",
        (true, true, true) => "accepted",
        (true, true, false) => "failed",
    };
    let mut persistence_errors = Vec::new();
    if let Err(error) = state.record_split_attempt(SplitAttemptRecord {
        run_id,
        model_key: &plan.model_key,
        repo_tshirt_bucket: &plan.repo_tshirt_bucket,
        item_set_key: &plan.item_set_key,
        strategy_id: &plan.primary_strategy.strategy_id,
        groups_json: &groups_json,
        reviewer_group_count: plan.primary_strategy.groups.len() as u32,
        baseline_strategy_id: plan
            .baseline_strategy
            .as_ref()
            .map(|strategy| strategy.strategy_id.as_str()),
        baseline_group_count,
        status,
        reviewer_count_savings,
        lost_evidence_count: lost.len() as u32,
    }) {
        persistence_errors.push(format!("record split attempt: {error}"));
    }

    let mut outcome = ReviewSplitOutcome {
        lost_evidence_count: lost.len() as u32,
        suppression_reason: None,
        promotion_reason: None,
    };
    if !primary_is_grouped || !checked_against_baseline {
        apply_persistence_errors(&mut outcome, persistence_errors);
        return outcome;
    }

    let risk_notes_json = serde_json::to_string(&plan.primary_strategy.risk_notes)
        .unwrap_or_else(|_| "[]".to_string());
    if let Err(error) = state.record_split_score(SplitScoreRecord {
        model_key: &plan.model_key,
        repo_tshirt_bucket: &plan.repo_tshirt_bucket,
        item_set_key: &plan.item_set_key,
        strategy_id: &plan.primary_strategy.strategy_id,
        groups_json: &groups_json,
        rationale: &plan.primary_strategy.rationale,
        expected_reviewer_count_savings: plan.primary_strategy.expected_reviewer_count_savings,
        risk_notes_json: &risk_notes_json,
        accepted: accepted_grouping,
        lost_evidence_count: lost.len() as u32,
        reviewer_count_savings,
    }) {
        persistence_errors.push(format!("record split score: {error}"));
    }

    if accepted_grouping {
        let reason = format!(
            "saved {reviewer_count_savings} reviewer group(s) with no lost baseline evidence"
        );
        if let Err(error) = state.record_split_promotion(SplitPromotionRecord {
            model_key: &plan.model_key,
            repo_tshirt_bucket: &plan.repo_tshirt_bucket,
            item_set_key: &plan.item_set_key,
            strategy_id: &plan.primary_strategy.strategy_id,
            reason: &reason,
        }) {
            persistence_errors.push(format!("record split promotion: {error}"));
        }
        outcome.promotion_reason = Some(reason);
        apply_persistence_errors(&mut outcome, persistence_errors);
        return outcome;
    }

    let reason = if lost.is_empty() {
        "grouping failed because it did not reduce reviewer count".to_string()
    } else {
        format!(
            "{} verifier-accepted baseline finding(s) were lost",
            lost.len()
        )
    };
    for finding in &lost {
        let fingerprint = verified_finding_fingerprint(finding);
        if let Err(error) = state.record_split_lost_evidence(SplitLostEvidenceRecord {
            run_id,
            model_key: &plan.model_key,
            repo_tshirt_bucket: &plan.repo_tshirt_bucket,
            item_set_key: &plan.item_set_key,
            strategy_id: &plan.primary_strategy.strategy_id,
            review_type_id: &finding.review_type_id,
            finding_id: &finding.id,
            fingerprint: &fingerprint,
            title: &finding.title,
            reason: &reason,
        }) {
            persistence_errors.push(format!("record split lost evidence: {error}"));
        }
    }
    for item_neighborhood_key in strategy_neighborhood_keys(&plan.primary_strategy) {
        if let Err(error) = state.record_split_suppression(SplitSuppressionRecord {
            model_key: &plan.model_key,
            repo_tshirt_bucket: &plan.repo_tshirt_bucket,
            item_set_key: &plan.item_set_key,
            strategy_id: &plan.primary_strategy.strategy_id,
            item_neighborhood_key: &item_neighborhood_key,
            reason: &reason,
        }) {
            persistence_errors.push(format!("record split suppression: {error}"));
        }
    }
    outcome.suppression_reason = Some(reason);
    apply_persistence_errors(&mut outcome, persistence_errors);
    outcome
}

fn apply_persistence_errors(outcome: &mut ReviewSplitOutcome, errors: Vec<String>) {
    if errors.is_empty() {
        return;
    }
    let reason = format!("split evidence persistence failed: {}", errors.join("; "));
    if outcome.suppression_reason.is_none() {
        outcome.suppression_reason = Some(reason);
    }
}

fn lost_baseline_findings(
    primary_verified: &[VerifiedFinding],
    baseline_verified: &[VerifiedFinding],
) -> Vec<VerifiedFinding> {
    let primary_fingerprints = primary_verified
        .iter()
        .map(verified_finding_fingerprint)
        .collect::<BTreeSet<_>>();
    baseline_verified
        .iter()
        .filter(|finding| !primary_fingerprints.contains(&verified_finding_fingerprint(finding)))
        .cloned()
        .collect()
}

fn verified_finding_fingerprint(finding: &VerifiedFinding) -> String {
    finding_fingerprint(
        &finding.review_type_id,
        &finding.title,
        &finding.details,
        finding.file_path.as_deref(),
        finding.line,
    )
}

fn writer_prompt(input: &DevCycleInput, planner_output: &str) -> String {
    format!(
        "Implement the assigned development-cycle work in this isolated worktree. Finish with a clean committed state and return JSON {{\"commits\":[\"...\"]}}.\n\nTask: {}\nCommit style: {}\nCoding style: {}\nPlanner output:\n{}",
        input
            .task_description
            .as_deref()
            .unwrap_or("Infer the task from the active Codex thread."),
        input.commit_style,
        input.coding_style,
        planner_output
    )
}

fn parse_writer_commits(
    writer_agent_id: &str,
    output: &NativeWorkflowAgentOutput,
) -> Vec<WriterCommit> {
    serde_json::from_str::<WriterOutput>(&output.text)
        .unwrap_or_default()
        .commits
        .into_iter()
        .map(|commit| WriterCommit {
            writer_agent_id: writer_agent_id.to_string(),
            commit,
        })
        .collect()
}

fn parse_integrator_output(text: &str) -> IntegratorOutput {
    serde_json::from_str(text).unwrap_or_default()
}

fn split_plan_threads(plan: &ReviewSplitPlan) -> Vec<(String, String)> {
    let mut threads = vec![(
        "primary".to_string(),
        format!(
            "{}: {}",
            plan.primary_strategy.strategy_id,
            split_group_summary(&plan.primary_strategy.groups)
        ),
    )];
    if let Some(challenger) = &plan.ai_proposed_challenger {
        threads.push((
            "challenger".to_string(),
            format!(
                "{}: {}",
                challenger.strategy_id,
                split_group_summary(&challenger.groups)
            ),
        ));
    } else if let Some(status) = &plan.proposal_status {
        threads.push((
            "proposal".to_string(),
            format!("{}: {}", status.status, status.reason),
        ));
    } else if let Some(reason) = &plan.stop_reason {
        threads.push(("proposal".to_string(), format!("stopped: {reason}")));
    }
    if let Some(reason) = &plan.baseline_reason {
        threads.push(("baseline".to_string(), reason.clone()));
    }
    threads
}

fn split_outcome_threads(review_split: &ReviewSplitOutput) -> Vec<(String, String)> {
    let result = if review_split.lost_evidence_count > 0 {
        format!(
            "grouping failed: {} verifier-accepted baseline finding(s) lost; {}",
            review_split.lost_evidence_count,
            review_split
                .suppression_reason
                .as_deref()
                .unwrap_or("strategy suppressed")
        )
    } else {
        review_split
            .promotion_reason
            .as_deref()
            .map(|reason| format!("promoted: {reason}"))
            .or_else(|| {
                review_split
                    .suppression_reason
                    .as_deref()
                    .map(|reason| format!("suppressed: {reason}"))
            })
            .or_else(|| {
                review_split
                    .stop_reason
                    .as_deref()
                    .map(|reason| format!("stopped: {reason}"))
            })
            .unwrap_or_else(|| {
                format!(
                    "{} saved {} reviewer(s)",
                    review_split.primary_split.strategy_id, review_split.cost_savings
                )
            })
    };
    let challenger = review_split
        .ai_proposed_challenger
        .as_ref()
        .map(|strategy| strategy.strategy_id.as_str())
        .unwrap_or("none");
    let baseline = if review_split.baseline_resample.ran {
        if review_split.baseline_resample.lost_evidence_count > 0 {
            "baseline failed"
        } else {
            "baseline passed"
        }
    } else {
        "baseline skipped"
    };
    let mut threads = vec![
        (
            "summary".to_string(),
            format!(
                "{}; active {}; primary {}; challenger {}; savings {}; {}; lost {}",
                review_split.mode,
                review_split.active_split.strategy_id,
                review_split.primary_split.strategy_id,
                challenger,
                review_split.cost_savings,
                baseline,
                review_split.lost_evidence_count
            ),
        ),
        ("result".to_string(), result),
    ];
    if review_split.baseline_resample.ran {
        let baseline_status = if review_split.baseline_resample.lost_evidence_count > 0 {
            format!(
                "{}; failed with {} lost verified finding(s)",
                review_split
                    .baseline_resample
                    .reason
                    .as_deref()
                    .unwrap_or("ran"),
                review_split.baseline_resample.lost_evidence_count
            )
        } else {
            format!(
                "{}; no lost evidence",
                review_split
                    .baseline_resample
                    .reason
                    .as_deref()
                    .unwrap_or("ran")
            )
        };
        threads.push(("baseline".to_string(), baseline_status));
    }
    threads
}

fn group_review_status(group: &crate::review_split::ReviewSplitGroup, action: &str) -> String {
    format!("{action} {}", group.review_type_ids.join("+"))
}

fn split_group_summary(groups: &[crate::review_split::ReviewSplitGroup]) -> String {
    groups
        .iter()
        .map(|group| format!("{}={}", group.group_id, group.review_type_ids.join("+")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn status<N, S>(
    workflow_status: &str,
    threads: impl IntoIterator<Item = (N, S)>,
) -> NativeWorkflowStatusUpdate
where
    N: Into<String>,
    S: Into<String>,
{
    NativeWorkflowStatusUpdate {
        workflow_name: "dev-cycle".to_string(),
        workflow_status: workflow_status.to_string(),
        threads: threads
            .into_iter()
            .map(|(name, status)| NativeWorkflowThreadStatus {
                name: name.into(),
                status: status.into(),
            })
            .collect(),
        child_statuses: Vec::new(),
    }
}

fn new_run_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("run-{millis}")
}
