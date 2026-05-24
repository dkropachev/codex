use crate::registry::WorkflowSummary;
use crate::repair::types::WorkflowRepairAction;
use crate::repair::types::WorkflowRepairResult;
use crate::repair::types::WorkflowRepairStopReason;
use crate::validation_finding::WorkflowValidationFinding;

pub(crate) fn repair_output_message(
    workflow: &WorkflowSummary,
    repair: &WorkflowRepairResult,
) -> String {
    let workflow_name = workflow
        .command
        .as_deref()
        .unwrap_or_else(|| workflow.id.split('/').next_back().unwrap_or(&workflow.id));
    let mut lines = vec![format!(
        "Repairing workflow `{workflow_name}` with `{}` mode.",
        repair.mode
    )];

    match repair.stop_reason {
        WorkflowRepairStopReason::Valid => {
            if repair.changed {
                lines.push(format!(
                    "Applied {} fix{} across {} cycle{}.",
                    repair.applied_fixes.len(),
                    if repair.applied_fixes.len() == 1 {
                        ""
                    } else {
                        "es"
                    },
                    repair.repair_cycles_run,
                    if repair.repair_cycles_run == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
                append_fix_section(&mut lines, &repair.applied_fixes);
                lines.push("Validation passed.".to_string());
            } else {
                lines.push("Workflow was already valid. No changes were needed.".to_string());
            }
        }
        WorkflowRepairStopReason::BlockedByRepairMode => {
            lines.push(format!(
                "Stopped after {} cycle{}: repair mode `{}` blocked the remaining findings.",
                repair.repair_cycles_run,
                if repair.repair_cycles_run == 1 {
                    ""
                } else {
                    "s"
                },
                repair.mode
            ));
            append_finding_section(
                &mut lines,
                "Blocked findings",
                if repair.blocked_findings.is_empty() {
                    &repair.remaining_findings
                } else {
                    &repair.blocked_findings
                },
            );
        }
        WorkflowRepairStopReason::UnsupportedFindings => {
            lines.push(format!(
                "Stopped after {} cycle{}: unsupported findings prevented an automatic fix.",
                repair.repair_cycles_run,
                if repair.repair_cycles_run == 1 {
                    ""
                } else {
                    "s"
                }
            ));
            append_finding_section(
                &mut lines,
                "Unsupported findings",
                if repair.unsupported_findings.is_empty() {
                    &repair.remaining_findings
                } else {
                    &repair.unsupported_findings
                },
            );
        }
        WorkflowRepairStopReason::RepairBudgetExhausted => {
            lines.push(format!(
                "Used all {} repair cycle{}; validation is still failing.",
                repair.max_repair_cycles,
                if repair.max_repair_cycles == 1 {
                    ""
                } else {
                    "s"
                }
            ));
            append_fix_section(&mut lines, &repair.applied_fixes);
            append_finding_section(&mut lines, "Remaining findings", &repair.remaining_findings);
        }
        WorkflowRepairStopReason::NoChangesApplied => {
            lines.push("No applicable fixes were found.".to_string());
            append_finding_section(&mut lines, "Remaining findings", &repair.remaining_findings);
        }
    }

    lines.join("\n")
}

fn append_fix_section(lines: &mut Vec<String>, fixes: &[WorkflowRepairAction]) {
    if fixes.is_empty() {
        return;
    }

    lines.push("Applied fixes:".to_string());
    lines.extend(fixes.iter().map(|fix| format!("- {}", fix.detail)));
}

fn append_finding_section(
    lines: &mut Vec<String>,
    heading: &str,
    findings: &[WorkflowValidationFinding],
) {
    if findings.is_empty() {
        return;
    }

    lines.push(format!("{heading}:"));
    lines.extend(findings.iter().map(format_finding_line));
}

fn format_finding_line(finding: &WorkflowValidationFinding) -> String {
    format!(
        "- {} {}: {}",
        finding.rule_id(),
        finding.title(),
        finding.message()
    )
}
