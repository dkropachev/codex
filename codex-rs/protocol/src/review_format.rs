use crate::protocol::ReviewFinding;
use crate::protocol::ReviewOutputEvent;
use crate::protocol::ReviewPreExisting;
use crate::protocol::ReviewResolutionStatus;
use crate::protocol::ReviewTestStatus;

fn format_location(item: &ReviewFinding) -> String {
    let path = item.code_location.absolute_file_path.display();
    let start = item.code_location.line_range.start;
    let end = item.code_location.line_range.end;
    format!("{path}:{start}-{end}")
}

fn pre_existing_label(pre_existing: ReviewPreExisting) -> &'static str {
    match pre_existing {
        ReviewPreExisting::True => "yes",
        ReviewPreExisting::False => "no",
        ReviewPreExisting::Undetermined => "undetermined",
    }
}

fn finding_title(finding: &ReviewFinding) -> String {
    let title = finding.title.strip_prefix("[P").and_then(|suffix| {
        let (priority, title) = suffix.split_once("] ")?;
        priority
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then_some(title)
    });
    format!(
        "[P{}] {}",
        finding.priority.clamp(0, 3),
        title.unwrap_or(&finding.title)
    )
}

/// Fallback text used when a review contains no displayable output.
pub const REVIEW_FALLBACK_MESSAGE: &str = "Reviewer failed to output a response.";
/// Text shown when a review exits because its turn was interrupted.
pub const REVIEW_INTERRUPTED_MESSAGE: &str = "Review interrupted.";

/// Format findings as concise issue blocks.
pub fn format_review_findings_block(
    findings: &[ReviewFinding],
    selection: Option<&[bool]>,
) -> String {
    let mut blocks = Vec::with_capacity(findings.len());
    for (index, finding) in findings.iter().enumerate() {
        let marker = selection.map(|flags| {
            if flags.get(index).copied().unwrap_or(true) {
                "[x] "
            } else {
                "[ ] "
            }
        });
        let mut lines = vec![format!(
            "{}{title} — {location}",
            marker.unwrap_or_default(),
            title = finding_title(finding),
            location = format_location(finding)
        )];
        if !finding.body.trim().is_empty() {
            lines.push(finding.body.trim().to_string());
        }
        let pre_existing = pre_existing_label(finding.pre_existing);
        let rationale = finding
            .pre_existing_fix_rationale
            .as_deref()
            .filter(|_| finding.pre_existing == ReviewPreExisting::True)
            .map(|rationale| format!(" — {}", rationale.trim()))
            .unwrap_or_default();
        lines.push(format!("Pre-existing: {pre_existing}{rationale}"));
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

fn findings_section(title: &str, findings: &[ReviewFinding]) -> Option<String> {
    (!findings.is_empty()).then(|| {
        format!(
            "{title}\n\n{}",
            format_review_findings_block(findings, /*selection*/ None)
        )
    })
}

fn assessment_section(output: &ReviewOutputEvent) -> Option<String> {
    let title = if output.resolution.is_some() {
        "Assessment before fixes"
    } else {
        "Assessment"
    };
    let verdict = output.overall_correctness.trim();
    let explanation = output.overall_explanation.trim();
    match (verdict.is_empty(), explanation.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!(
            "{title}\n\n{verdict} (confidence {:.2})",
            output.overall_confidence_score
        )),
        (true, false) => Some(format!("{title}\n\n{explanation}")),
        (false, false) => Some(format!(
            "{title}\n\n{verdict} (confidence {:.2})\n{explanation}",
            output.overall_confidence_score
        )),
    }
}

fn references_section(output: &ReviewOutputEvent) -> Option<String> {
    (!output.references.is_empty()).then(|| {
        let references = output
            .references
            .iter()
            .map(|reference| format!("- {} — {}", reference.reference, reference.explanation))
            .collect::<Vec<_>>()
            .join("\n");
        format!("References\n\n{references}")
    })
}

fn external_references_section(output: &ReviewOutputEvent) -> Option<String> {
    (!output.external_references.is_empty()).then(|| {
        let references = output
            .external_references
            .iter()
            .map(|reference| format!("- {} — {}", reference.reference, reference.explanation))
            .collect::<Vec<_>>()
            .join("\n");
        format!("External references\n\n{references}")
    })
}

fn resolution_section(output: &ReviewOutputEvent) -> Option<String> {
    let resolution = output.resolution.as_ref()?;
    let status = match resolution.status {
        ReviewResolutionStatus::Complete => "complete",
        ReviewResolutionStatus::Partial => "partial",
        ReviewResolutionStatus::Failed => "failed",
    };
    let mut lines = vec![
        "Resolution".to_string(),
        String::new(),
        format!(
            "Status: {status}. Fixed: {}. Rejected: {}. Unresolved: {}.",
            resolution.fixed_count, resolution.rejected_count, resolution.unresolved_count
        ),
    ];
    lines.extend(
        resolution
            .summary
            .iter()
            .take(5)
            .map(|summary| format!("- {}", summary.trim())),
    );
    if !resolution.tests.is_empty() {
        lines.push("Tests:".to_string());
        lines.extend(resolution.tests.iter().map(|test| {
            let status = match test.status {
                ReviewTestStatus::Passed => "passed",
                ReviewTestStatus::Failed => "failed",
                ReviewTestStatus::NotRun => "not run",
            };
            format!("- `{}` — {status}", test.command)
        }));
    }
    if let Some(commit_sha) = resolution.commit_sha.as_deref() {
        lines.push(format!("Commit: {commit_sha}"));
    }
    Some(lines.join("\n"))
}

/// Render a user-facing report in stable section order.
pub fn render_review_output_text(output: &ReviewOutputEvent) -> String {
    let sections = [
        assessment_section(output),
        findings_section("Findings", &output.findings),
        findings_section("Out-of-scope findings", &output.out_of_scope_findings),
        findings_section("Unverified", &output.unverified_findings),
        references_section(output),
        external_references_section(output),
        resolution_section(output),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    if sections.is_empty() {
        REVIEW_FALLBACK_MESSAGE.to_string()
    } else {
        sections.join("\n\n")
    }
}

#[cfg(test)]
#[path = "review_format_tests.rs"]
mod tests;
