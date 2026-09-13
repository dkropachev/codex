use std::fmt::Write;

use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_utils_string::take_bytes_at_char_boundary;

const MAX_REPORT_ITEMS: usize = 64;

use super::MAX_REPORT_FIELD_BYTES;

pub(super) fn bounded_output(mut output: ReviewOutputEvent) -> ReviewOutputEvent {
    bound_field(&mut output.overall_correctness);
    bound_field(&mut output.overall_explanation);
    bound_findings(&mut output.findings);
    bound_findings(&mut output.out_of_scope_findings);
    bound_findings(&mut output.unverified_findings);
    output.references.truncate(MAX_REPORT_ITEMS);
    output.external_references.truncate(MAX_REPORT_ITEMS);
    for reference in &mut output.references {
        bound_field(&mut reference.reference);
        bound_field(&mut reference.explanation);
    }
    for reference in &mut output.external_references {
        bound_field(&mut reference.reference);
        bound_field(&mut reference.explanation);
    }
    if let Some(resolution) = output.resolution.as_mut() {
        resolution.summary.truncate(5);
        resolution.tests.truncate(MAX_REPORT_ITEMS);
        for summary in &mut resolution.summary {
            bound_field(summary);
        }
        for test in &mut resolution.tests {
            bound_field(&mut test.command);
        }
        if let Some(commit_sha) = resolution.commit_sha.as_mut() {
            bound_field(commit_sha);
        }
    }
    output
}

fn bound_findings(findings: &mut Vec<ReviewFinding>) {
    findings.truncate(MAX_REPORT_ITEMS);
    for finding in findings {
        bound_field(&mut finding.title);
        bound_field(&mut finding.body);
        let path = finding
            .code_location
            .absolute_file_path
            .display()
            .to_string();
        finding.code_location.absolute_file_path = bound_string(&path).into();
        if let Some(rationale) = finding.pre_existing_fix_rationale.as_mut() {
            bound_field(rationale);
        }
    }
}

fn bound_field(field: &mut String) {
    *field = bound_string(field);
}

fn bound_string(value: &str) -> String {
    if value.len() <= MAX_REPORT_FIELD_BYTES {
        return value.to_string();
    }
    let budget = MAX_REPORT_FIELD_BYTES.saturating_sub(" [truncated]".len());
    format!("{} [truncated]", take_bytes_at_char_boundary(value, budget))
}

pub(super) fn compact_report(output: &ReviewOutputEvent) -> String {
    let mut report = String::new();
    let _ = writeln!(
        report,
        "Assessment: {} (confidence {:.2})",
        escape_untrusted_markup(&output.overall_correctness),
        output.overall_confidence_score
    );
    compact_findings(&mut report, "Findings", &output.findings);
    compact_findings(
        &mut report,
        "Out-of-scope findings",
        &output.out_of_scope_findings,
    );
    compact_findings(&mut report, "Unverified", &output.unverified_findings);
    for reference in &output.references {
        let _ = writeln!(
            report,
            "Reference: {} — {}",
            escape_untrusted_markup(&reference.reference),
            escape_untrusted_markup(&reference.explanation)
        );
    }
    for reference in &output.external_references {
        let _ = writeln!(
            report,
            "External reference: {} — {}",
            escape_untrusted_markup(&reference.reference),
            escape_untrusted_markup(&reference.explanation)
        );
    }
    if let Some(resolution) = output.resolution.as_ref() {
        let _ = writeln!(
            report,
            "Resolution: {:?}; fixed {}; rejected {}; unresolved {}",
            resolution.status,
            resolution.fixed_count,
            resolution.rejected_count,
            resolution.unresolved_count
        );
        for test in &resolution.tests {
            let _ = writeln!(
                report,
                "Test: {} — {:?}",
                escape_untrusted_markup(&test.command),
                test.status
            );
        }
        if let Some(commit_sha) = resolution.commit_sha.as_deref() {
            let _ = writeln!(report, "Commit: {}", escape_untrusted_markup(commit_sha));
        }
    }
    report
}

pub(super) fn minimal_report(output: &ReviewOutputEvent, max_bytes: usize) -> String {
    let correctness = bounded_inline(&output.overall_correctness, 128);
    let mut report = format!(
        "Assessment: {} (confidence {:.2})\nFindings: {}; out-of-scope: {}; unverified: {}; references: {}; external references: {}",
        escape_untrusted_markup(&correctness),
        output.overall_confidence_score,
        output.findings.len(),
        output.out_of_scope_findings.len(),
        output.unverified_findings.len(),
        output.references.len(),
        output.external_references.len(),
    );
    let mut omitted_metadata = 0usize;
    for (section, findings) in [
        ("finding", output.findings.as_slice()),
        ("out-of-scope", output.out_of_scope_findings.as_slice()),
        ("unverified", output.unverified_findings.as_slice()),
    ] {
        for finding in findings {
            let title = bounded_inline(&finding.title, 48);
            let path = bounded_inline(
                &finding
                    .code_location
                    .absolute_file_path
                    .display()
                    .to_string(),
                48,
            );
            let rationale = finding
                .pre_existing_fix_rationale
                .as_deref()
                .map(|value| format!("; rationale={}", bounded_inline(value, 48)))
                .unwrap_or_default();
            let line = format!(
                "\n{section}: P{}; title={}; location={}:{}-{}; preExisting={:?}{rationale}",
                finding.priority,
                escape_untrusted_markup(&title),
                escape_untrusted_markup(&path),
                finding.code_location.line_range.start,
                finding.code_location.line_range.end,
                finding.pre_existing,
            );
            if report.len().saturating_add(line.len() + 64) <= max_bytes {
                report.push_str(&line);
            } else {
                omitted_metadata += 1;
            }
        }
    }
    if let Some(resolution) = output.resolution.as_ref() {
        let line = format!(
            "\nResolution: {:?}; fixed {}; rejected {}; unresolved {}",
            resolution.status,
            resolution.fixed_count,
            resolution.rejected_count,
            resolution.unresolved_count
        );
        if report.len().saturating_add(line.len() + 64) <= max_bytes {
            report.push_str(&line);
        } else {
            omitted_metadata += 1;
        }
        for test in &resolution.tests {
            let line = format!(
                "\nTest: {} — {:?}",
                escape_untrusted_markup(&bounded_inline(&test.command, 64)),
                test.status
            );
            if report.len().saturating_add(line.len() + 64) <= max_bytes {
                report.push_str(&line);
            } else {
                omitted_metadata += 1;
            }
        }
        if let Some(commit_sha) = resolution.commit_sha.as_deref() {
            let commit_sha = bounded_inline(commit_sha, 128);
            let line = format!("\nCommit: {}", escape_untrusted_markup(&commit_sha));
            if report.len().saturating_add(line.len() + 64) <= max_bytes {
                report.push_str(&line);
            } else {
                omitted_metadata += 1;
            }
        }
    }
    if omitted_metadata > 0 {
        let notice = format!("\n{omitted_metadata} metadata entries truncated.");
        let remaining = max_bytes.saturating_sub(report.len());
        report.push_str(take_bytes_at_char_boundary(&notice, remaining));
    }
    report
}

fn bounded_inline(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        value.to_string()
    } else {
        format!(
            "{}…",
            take_bytes_at_char_boundary(value, max_bytes.saturating_sub('…'.len_utf8()))
        )
    }
}

fn compact_findings(report: &mut String, section: &str, findings: &[ReviewFinding]) {
    for finding in findings {
        let path = escape_untrusted_markup(
            &finding
                .code_location
                .absolute_file_path
                .display()
                .to_string(),
        );
        let start = finding.code_location.line_range.start;
        let end = finding.code_location.line_range.end;
        let rationale = finding
            .pre_existing_fix_rationale
            .as_deref()
            .map(|rationale| format!(" — {}", escape_untrusted_markup(rationale)))
            .unwrap_or_default();
        let _ = writeln!(
            report,
            "{section}: [P{}] {} — {path}:{start}-{end}; pre-existing: {:?}{rationale}",
            finding.priority,
            escape_untrusted_markup(&finding.title),
            finding.pre_existing
        );
    }
}

pub(super) fn escape_untrusted_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
