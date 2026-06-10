use codex_native_workflow::NativeWorkflowRunContext;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::DEVELOPMENT_CYCLE_WORKFLOW_ID;
use crate::execution::TestResult;
use crate::persistence::DevCycleState;
use crate::persistence::DisregardedFinding;
use crate::persistence::ExperimentDecision;
use crate::pipeline::WriterCommit;
use crate::review_split::ReviewSplitOutput;
use crate::review_stage::VerifiedFinding;
use crate::review_types::ReviewTypeDefinition;

pub(crate) struct FinalOutputParts<'a> {
    pub(crate) status: &'a str,
    pub(crate) ctx: &'a NativeWorkflowRunContext<'a>,
    pub(crate) state: &'a DevCycleState,
    pub(crate) normalized_input: &'a JsonValue,
    pub(crate) selected_review_types: &'a [ReviewTypeDefinition],
    pub(crate) excluded_review_types: &'a [ReviewTypeDefinition],
    pub(crate) writer_commits: &'a [WriterCommit],
    pub(crate) verified_findings: &'a [VerifiedFinding],
    pub(crate) disregarded_findings: &'a [DisregardedFinding],
    pub(crate) test_results: &'a [TestResult],
    pub(crate) integration_branch: Option<&'a str>,
    pub(crate) experiment_decisions: &'a [ExperimentDecision],
    pub(crate) review_split: &'a ReviewSplitOutput,
    pub(crate) blocked_reason: Option<&'a str>,
}

pub(crate) fn final_output(parts: FinalOutputParts<'_>) -> JsonValue {
    json!({
        "workflowId": DEVELOPMENT_CYCLE_WORKFLOW_ID,
        "engine": "rust",
        "status": parts.status,
        "blockedReason": parts.blocked_reason,
        "workingDirectory": parts.ctx.cwd.display().to_string(),
        "stateDatabase": parts.state.path().display().to_string(),
        "settings": parts.normalized_input,
        "selectedReviewTypes": parts.selected_review_types,
        "excludedReviewTypes": parts.excluded_review_types,
        "writerCommits": parts.writer_commits,
        "verifiedFindings": parts.verified_findings,
        "disregardedFindings": parts.disregarded_findings,
        "testResults": parts.test_results,
        "integrationBranch": parts.integration_branch,
        "experimentDecisions": parts.experiment_decisions,
        "reviewSplit": parts.review_split,
    })
}

pub(crate) fn format_markdown(output: &JsonValue) -> String {
    let mut lines = vec![
        "# Development Cycle".to_string(),
        String::new(),
        format!(
            "Status: `{}`",
            output
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown")
        ),
    ];
    if let Some(reason) = output.get("blockedReason").and_then(JsonValue::as_str) {
        lines.push(format!("Blocked: {reason}"));
    }
    lines.push(format!(
        "State DB: `{}`",
        output
            .get("stateDatabase")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
    ));
    push_top_level_split_summary(&mut lines, output);
    lines.extend([String::new(), "## Review Types".to_string()]);
    lines.push(format!(
        "Selected: {}",
        list_or_none(ids(output, "selectedReviewTypes"))
    ));
    lines.push(format!(
        "Excluded: {}",
        list_or_none(ids(output, "excludedReviewTypes"))
    ));
    lines.extend([String::new(), "## Writer Commits".to_string()]);
    push_string_items(&mut lines, output, "writerCommits", "commit");
    lines.extend([String::new(), "## Verified Findings".to_string()]);
    push_findings(&mut lines, output, "verifiedFindings");
    lines.extend([String::new(), "## Disregarded Findings".to_string()]);
    push_disregarded_findings(&mut lines, output);
    lines.extend([String::new(), "## Tests".to_string()]);
    push_test_results(&mut lines, output);
    lines.extend([String::new(), "## Integration".to_string()]);
    lines.push(format!(
        "Branch: `{}`",
        output
            .get("integrationBranch")
            .and_then(JsonValue::as_str)
            .unwrap_or("<none>")
    ));
    lines.extend([String::new(), "## Experiments".to_string()]);
    push_string_items(&mut lines, output, "experimentDecisions", "decision");
    lines.extend([String::new(), "## Review Split".to_string()]);
    push_review_split(&mut lines, output);
    lines.join("\n")
}

fn ids(output: &JsonValue, field: &str) -> Vec<String> {
    output
        .get(field)
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.get("id").and_then(JsonValue::as_str))
        .map(str::to_string)
        .collect()
}

fn list_or_none(values: Vec<String>) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    }
}

fn push_string_items(lines: &mut Vec<String>, output: &JsonValue, field: &str, key: &str) {
    let mut pushed = false;
    for item in output
        .get(field)
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(value) = item.get(key).and_then(JsonValue::as_str) {
            lines.push(format!("- {value}"));
            pushed = true;
        }
    }
    if !pushed {
        lines.push("- none".to_string());
    }
}

fn push_findings(lines: &mut Vec<String>, output: &JsonValue, field: &str) {
    let mut pushed = false;
    for finding in output
        .get(field)
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let title = string_field(finding, "title").unwrap_or("untitled");
        let review_type = string_field(finding, "reviewTypeId").unwrap_or("unknown");
        let severity = string_field(finding, "severity").unwrap_or("unknown");
        let location = finding_location(finding)
            .map(|location| format!(" at `{location}`"))
            .unwrap_or_default();
        let baseline = if finding
            .get("shadowBaseline")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            " shadow-baseline"
        } else {
            ""
        };
        lines.push(format!(
            "- [{severity}] `{review_type}`{baseline}: {title}{location}"
        ));
        if let Some(reason) = string_field(finding, "reason") {
            lines.push(format!("  - verifier: {reason}"));
        }
        if let Some(strategy) = string_field(finding, "splitStrategyId") {
            lines.push(format!("  - split: `{strategy}`"));
        }
        pushed = true;
    }
    if !pushed {
        lines.push("- none".to_string());
    }
}

fn push_disregarded_findings(lines: &mut Vec<String>, output: &JsonValue) {
    let mut pushed = false;
    for finding in output
        .get("disregardedFindings")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let title = string_field(finding, "title").unwrap_or("untitled");
        let review_type = string_field(finding, "reviewTypeId").unwrap_or("unknown");
        let status = string_field(finding, "status").unwrap_or("unknown");
        let reason = string_field(finding, "reason").unwrap_or("no reason recorded");
        lines.push(format!("- `{review_type}` {title} ({status}): {reason}"));
        pushed = true;
    }
    if !pushed {
        lines.push("- none".to_string());
    }
}

fn push_test_results(lines: &mut Vec<String>, output: &JsonValue) {
    let tests = output
        .get("testResults")
        .and_then(JsonValue::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if tests.is_empty() {
        lines.push("- none".to_string());
        return;
    }

    let passed = tests
        .iter()
        .filter(|test| {
            test.get("success")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false)
        })
        .count();
    let failed = tests.len().saturating_sub(passed);
    lines.push(format!("Passed: `{passed}`; failed: `{failed}`"));
    for test in tests {
        let command = string_field(test, "command").unwrap_or("unknown");
        let status = if test
            .get("success")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            "pass"
        } else {
            "fail"
        };
        let exit_status = string_field(test, "exitStatus").unwrap_or("unknown exit status");
        lines.push(format!("- {status}: `{command}` ({exit_status})"));
        push_non_empty_excerpt(
            lines,
            "stdout",
            test.get("stdout").and_then(JsonValue::as_str),
        );
        push_non_empty_excerpt(
            lines,
            "stderr",
            test.get("stderr").and_then(JsonValue::as_str),
        );
    }
}

fn push_non_empty_excerpt(lines: &mut Vec<String>, label: &str, text: Option<&str>) {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return;
    };
    let excerpt = text.lines().take(3).collect::<Vec<_>>().join("\\n");
    lines.push(format!("  - {label}: `{excerpt}`"));
}

fn push_review_split(lines: &mut Vec<String>, output: &JsonValue) {
    let Some(split) = output.get("reviewSplit") else {
        lines.push("- none".to_string());
        return;
    };

    lines.push(format!("Summary: {}", compact_review_split_summary(split)));
    lines.push(format!(
        "Mode: `{}`; model: `{}`; repo size: `{}`",
        string_field(split, "mode").unwrap_or("unknown"),
        string_field(split, "modelKey").unwrap_or("unknown"),
        string_field(split, "repoTshirtBucket").unwrap_or("unknown")
    ));
    push_strategy_line(lines, "Active", split.get("activeSplit"));
    push_strategy_line(lines, "Primary", split.get("primarySplit"));
    push_strategy_groups(lines, "Primary groups", split.get("primarySplit"));
    if let Some(challenger) = split
        .get("aiProposedChallenger")
        .filter(|value| !value.is_null())
    {
        push_strategy_line(lines, "AI challenger", Some(challenger));
        push_strategy_groups(lines, "Challenger groups", Some(challenger));
    }
    push_baseline_resample(lines, split);
    let lost_evidence_count = split
        .get("lostEvidenceCount")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let lost_evidence_label = if lost_evidence_count == 0 {
        "`0`".to_string()
    } else {
        format!("`{lost_evidence_count}` (grouping failed and was suppressed)")
    };
    lines.push(format!(
        "Savings: `{}` reviewer(s); lost evidence: {lost_evidence_label}",
        split
            .get("costSavings")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0)
    ));
    push_reason(
        lines,
        "Proposal",
        split.get("proposalStatus").filter(|value| !value.is_null()),
    );
    push_optional_reason(lines, "Stop", split, "stopReason");
    push_optional_reason(lines, "Promoted", split, "promotionReason");
    push_optional_reason(lines, "Suppressed", split, "suppressionReason");
    lines.push(format!(
        "Rejected grouping experiments: `{}/{}`",
        split
            .get("rejectedGroupingExperiments")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        split
            .get("maxRejectedGroupingExperiments")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0)
    ));
    if let Some(item_set_key) = string_field(split, "itemSetKey") {
        lines.push(format!("Item set: `{item_set_key}`"));
    }
}

fn push_top_level_split_summary(lines: &mut Vec<String>, output: &JsonValue) {
    let Some(split) = output.get("reviewSplit") else {
        return;
    };
    lines.push(format!(
        "Review Split: {}",
        compact_review_split_summary(split)
    ));
    let lost_evidence_count = split
        .get("lostEvidenceCount")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    if lost_evidence_count > 0 {
        lines.push(format!(
            "Lost Evidence: `{lost_evidence_count}` verifier-accepted baseline finding(s); grouping failed and was suppressed."
        ));
    }
}

fn compact_review_split_summary(split: &JsonValue) -> String {
    let mode = string_field(split, "mode").unwrap_or("unknown");
    let active = split
        .get("activeSplit")
        .and_then(|strategy| string_field(strategy, "strategyId"))
        .unwrap_or("unknown");
    let primary = split
        .get("primarySplit")
        .and_then(|strategy| string_field(strategy, "strategyId"))
        .unwrap_or("unknown");
    let challenger = split
        .get("aiProposedChallenger")
        .and_then(|strategy| string_field(strategy, "strategyId"))
        .unwrap_or("none");
    let cost_savings = split
        .get("costSavings")
        .and_then(JsonValue::as_i64)
        .unwrap_or(0);
    let lost_evidence_count = split
        .get("lostEvidenceCount")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    format!(
        "`{mode}`; active `{active}`; primary `{primary}`; challenger `{challenger}`; savings `{cost_savings}`; {}; lost evidence `{lost_evidence_count}`",
        baseline_resample_summary(split)
    )
}

fn baseline_resample_summary(split: &JsonValue) -> String {
    let Some(baseline) = split.get("baselineResample") else {
        return "baseline unavailable".to_string();
    };
    let ran = baseline
        .get("ran")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let reason = string_field(baseline, "reason").unwrap_or("none");
    if ran {
        format!("baseline ran `{reason}`")
    } else {
        format!("baseline skipped `{reason}`")
    }
}

fn push_strategy_line(lines: &mut Vec<String>, label: &str, strategy: Option<&JsonValue>) {
    let Some(strategy) = strategy else {
        return;
    };
    let id = string_field(strategy, "strategyId").unwrap_or("unknown");
    let rationale = string_field(strategy, "rationale")
        .filter(|rationale| !rationale.is_empty())
        .map(|rationale| format!(" - {rationale}"))
        .unwrap_or_default();
    lines.push(format!("{label}: `{id}`{rationale}"));
}

fn push_strategy_groups(lines: &mut Vec<String>, label: &str, strategy: Option<&JsonValue>) {
    let Some(groups) = strategy
        .and_then(|strategy| strategy.get("groups"))
        .and_then(JsonValue::as_array)
    else {
        return;
    };
    if groups.is_empty() {
        return;
    }

    lines.push(format!("{label}:"));
    for group in groups {
        let group_id = string_field(group, "groupId").unwrap_or("group");
        let ids = group
            .get("reviewTypeIds")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(JsonValue::as_str)
            .collect::<Vec<_>>();
        lines.push(format!("- {group_id}: {}", ids.join(" + ")));
    }
}

fn push_baseline_resample(lines: &mut Vec<String>, split: &JsonValue) {
    let Some(baseline) = split.get("baselineResample") else {
        return;
    };
    let ran = baseline
        .get("ran")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let reason = string_field(baseline, "reason").unwrap_or("none");
    let lost = baseline
        .get("lostEvidenceCount")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    lines.push(format!(
        "Baseline: {}; reason: `{reason}`; lost evidence: `{lost}`",
        if ran { "ran" } else { "skipped" }
    ));
}

fn push_reason(lines: &mut Vec<String>, label: &str, value: Option<&JsonValue>) {
    let Some(value) = value else {
        return;
    };
    let status = string_field(value, "status").unwrap_or("unknown");
    let reason = string_field(value, "reason").unwrap_or("no reason recorded");
    lines.push(format!("{label}: {status} - {reason}"));
}

fn push_optional_reason(lines: &mut Vec<String>, label: &str, value: &JsonValue, key: &str) {
    if let Some(reason) = string_field(value, key) {
        lines.push(format!("{label}: {reason}"));
    }
}

fn finding_location(finding: &JsonValue) -> Option<String> {
    let path = string_field(finding, "filePath")?;
    match finding.get("line").and_then(JsonValue::as_u64) {
        Some(line) => Some(format!("{path}:{line}")),
        None => Some(path.to_string()),
    }
}

fn string_field<'a>(value: &'a JsonValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsonValue::as_str)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::format_markdown;

    #[test]
    fn markdown_summarizes_review_split_findings_and_tests() {
        let markdown = format_markdown(&json!({
            "status": "succeeded",
            "stateDatabase": "/tmp/dev_cycle.sqlite3",
            "selectedReviewTypes": [
                {"id": "correctness"},
                {"id": "tests"}
            ],
            "excludedReviewTypes": [],
            "writerCommits": [
                {"writerAgentId": "writer-1", "commit": "abc123"}
            ],
            "verifiedFindings": [
                {
                    "reviewTypeId": "correctness",
                    "title": "Broken behavior",
                    "details": "details",
                    "filePath": "lib.rs",
                    "line": 7,
                    "severity": "high",
                    "verifierAgentId": "verifier-1",
                    "reason": "confirmed",
                    "splitStrategyId": "grouping:v1",
                    "shadowBaseline": false
                }
            ],
            "disregardedFindings": [
                {
                    "reviewTypeId": "tests",
                    "title": "Missing attribution",
                    "reason": "grouped finding omitted reviewTypeId",
                    "status": "invalid_attribution"
                }
            ],
            "testResults": [
                {
                    "command": "cargo test",
                    "success": false,
                    "exitStatus": "exit status: 101",
                    "stdout": "",
                    "stderr": "failure line\nsecond line"
                }
            ],
            "integrationBranch": "integration/dev-cycle",
            "experimentDecisions": [
                {"decision": "use-primary"}
            ],
            "reviewSplit": {
                "activeSplit": {
                    "strategyId": "separate:v1",
                    "groups": [
                        {"groupId": "correctness", "reviewTypeIds": ["correctness"]},
                        {"groupId": "tests", "reviewTypeIds": ["tests"]}
                    ],
                    "rationale": "zero grouping",
                    "expectedReviewerCountSavings": 0,
                    "riskNotes": []
                },
                "primarySplit": {
                    "strategyId": "grouping:v1",
                    "groups": [
                        {"groupId": "quality", "reviewTypeIds": ["correctness", "tests"]}
                    ],
                    "rationale": "correctness and tests overlap",
                    "expectedReviewerCountSavings": 1,
                    "riskNotes": []
                },
                "zeroGrouping": false,
                "mode": "grouped",
                "aiProposedChallenger": {
                    "strategyId": "grouping:v1",
                    "groups": [
                        {"groupId": "quality", "reviewTypeIds": ["correctness", "tests"]}
                    ],
                    "rationale": "correctness and tests overlap",
                    "expectedReviewerCountSavings": 1,
                    "riskNotes": []
                },
                "costSavings": 1,
                "lostEvidenceCount": 0,
                "stopReason": null,
                "suppressionReason": null,
                "promotionReason": "saved 1 reviewer group(s) with no lost baseline evidence",
                "proposalStatus": {
                    "status": "accepted",
                    "reason": "AI proposal passed partition validation"
                },
                "baselineResample": {
                    "ran": true,
                    "reason": "challenger_quality_gate",
                    "lostEvidenceCount": 0
                },
                "rejectedGroupingExperiments": 0,
                "maxRejectedGroupingExperiments": 3,
                "repoTshirtBucket": "XS",
                "itemSetKey": "items",
                "modelKey": "openai:gpt-5.5:xhigh"
            }
        }));

        let expected = [
            "# Development Cycle",
            "",
            "Status: `succeeded`",
            "State DB: `/tmp/dev_cycle.sqlite3`",
            "Review Split: `grouped`; active `separate:v1`; primary `grouping:v1`; challenger `grouping:v1`; savings `1`; baseline ran `challenger_quality_gate`; lost evidence `0`",
            "",
            "## Review Types",
            "Selected: correctness, tests",
            "Excluded: none",
            "",
            "## Writer Commits",
            "- abc123",
            "",
            "## Verified Findings",
            "- [high] `correctness`: Broken behavior at `lib.rs:7`",
            "  - verifier: confirmed",
            "  - split: `grouping:v1`",
            "",
            "## Disregarded Findings",
            "- `tests` Missing attribution (invalid_attribution): grouped finding omitted reviewTypeId",
            "",
            "## Tests",
            "Passed: `0`; failed: `1`",
            "- fail: `cargo test` (exit status: 101)",
            "  - stderr: `failure line\\nsecond line`",
            "",
            "## Integration",
            "Branch: `integration/dev-cycle`",
            "",
            "## Experiments",
            "- use-primary",
            "",
            "## Review Split",
            "Summary: `grouped`; active `separate:v1`; primary `grouping:v1`; challenger `grouping:v1`; savings `1`; baseline ran `challenger_quality_gate`; lost evidence `0`",
            "Mode: `grouped`; model: `openai:gpt-5.5:xhigh`; repo size: `XS`",
            "Active: `separate:v1` - zero grouping",
            "Primary: `grouping:v1` - correctness and tests overlap",
            "Primary groups:",
            "- quality: correctness + tests",
            "AI challenger: `grouping:v1` - correctness and tests overlap",
            "Challenger groups:",
            "- quality: correctness + tests",
            "Baseline: ran; reason: `challenger_quality_gate`; lost evidence: `0`",
            "Savings: `1` reviewer(s); lost evidence: `0`",
            "Proposal: accepted - AI proposal passed partition validation",
            "Promoted: saved 1 reviewer group(s) with no lost baseline evidence",
            "Rejected grouping experiments: `0/3`",
            "Item set: `items`",
        ]
        .join("\n");
        assert_eq!(markdown, expected);
    }

    #[test]
    fn markdown_highlights_lost_evidence_as_grouping_failure() {
        let split = json!({
            "activeSplit": {
                "strategyId": "separate:v1",
                "groups": [
                    {"groupId": "correctness", "reviewTypeIds": ["correctness"]},
                    {"groupId": "tests", "reviewTypeIds": ["tests"]}
                ],
                "rationale": "zero grouping",
                "expectedReviewerCountSavings": 0,
                "riskNotes": []
            },
            "primarySplit": {
                "strategyId": "grouping:v1",
                "groups": [
                    {"groupId": "quality", "reviewTypeIds": ["correctness", "tests"]}
                ],
                "rationale": "correctness and tests overlap",
                "expectedReviewerCountSavings": 1,
                "riskNotes": []
            },
            "zeroGrouping": false,
            "mode": "grouped",
            "aiProposedChallenger": null,
            "costSavings": 1,
            "lostEvidenceCount": 1,
            "stopReason": null,
            "suppressionReason": "1 verifier-accepted baseline finding(s) were lost",
            "promotionReason": null,
            "proposalStatus": null,
            "baselineResample": {
                "ran": true,
                "reason": "baseline_resample",
                "lostEvidenceCount": 1
            },
            "rejectedGroupingExperiments": 1,
            "maxRejectedGroupingExperiments": 3,
            "repoTshirtBucket": "S",
            "itemSetKey": "items",
            "modelKey": "openai:gpt-5.5:xhigh"
        });
        let markdown = format_markdown(&json!({
            "status": "succeeded",
            "stateDatabase": "/tmp/dev_cycle.sqlite3",
            "selectedReviewTypes": [],
            "excludedReviewTypes": [],
            "writerCommits": [],
            "verifiedFindings": [],
            "disregardedFindings": [],
            "testResults": [],
            "integrationBranch": null,
            "experimentDecisions": [],
            "reviewSplit": split
        }));

        let expected = [
            "# Development Cycle",
            "",
            "Status: `succeeded`",
            "State DB: `/tmp/dev_cycle.sqlite3`",
            "Review Split: `grouped`; active `separate:v1`; primary `grouping:v1`; challenger `none`; savings `1`; baseline ran `baseline_resample`; lost evidence `1`",
            "Lost Evidence: `1` verifier-accepted baseline finding(s); grouping failed and was suppressed.",
            "",
            "## Review Types",
            "Selected: none",
            "Excluded: none",
            "",
            "## Writer Commits",
            "- none",
            "",
            "## Verified Findings",
            "- none",
            "",
            "## Disregarded Findings",
            "- none",
            "",
            "## Tests",
            "- none",
            "",
            "## Integration",
            "Branch: `<none>`",
            "",
            "## Experiments",
            "- none",
            "",
            "## Review Split",
            "Summary: `grouped`; active `separate:v1`; primary `grouping:v1`; challenger `none`; savings `1`; baseline ran `baseline_resample`; lost evidence `1`",
            "Mode: `grouped`; model: `openai:gpt-5.5:xhigh`; repo size: `S`",
            "Active: `separate:v1` - zero grouping",
            "Primary: `grouping:v1` - correctness and tests overlap",
            "Primary groups:",
            "- quality: correctness + tests",
            "Baseline: ran; reason: `baseline_resample`; lost evidence: `1`",
            "Savings: `1` reviewer(s); lost evidence: `1` (grouping failed and was suppressed)",
            "Suppressed: 1 verifier-accepted baseline finding(s) were lost",
            "Rejected grouping experiments: `1/3`",
            "Item set: `items`",
        ]
        .join("\n");
        assert_eq!(markdown, expected);
    }
}
