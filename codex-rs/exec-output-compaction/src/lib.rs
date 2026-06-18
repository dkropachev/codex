use codex_utils_output_truncation::approx_token_count;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const MIN_RAW_TOKENS: usize = 400;

mod learned;
pub use learned::compact_output_for_suggestions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedOutput {
    pub filter_id: &'static str,
    pub text: String,
    pub original_token_count: usize,
    pub compacted_token_count: usize,
}

pub fn compact_output(command: &[String], output: &str) -> Option<CompactedOutput> {
    let original_token_count = approx_token_count(output);
    if original_token_count < MIN_RAW_TOKENS {
        return None;
    }

    let command_text = command.join(" ");
    let candidate = if looks_like_nextest(command_text.as_str(), output) {
        compact_nextest(output).map(|text| ("nextest-v1", text))
    } else if looks_like_pytest(command_text.as_str(), output) {
        Some(("pytest-v1", compact_pytest(output)))
    } else if looks_like_cargo_test(command_text.as_str(), output) {
        Some(("cargo-test-v1", compact_cargo_test(output)))
    } else if looks_like_go_test(command_text.as_str(), output) {
        Some(("go-test-v1", compact_go_test(output)))
    } else if looks_like_maven_output(command_text.as_str(), output) {
        Some(("maven-v1", compact_maven_output(output)))
    } else if looks_like_cargo_build(command_text.as_str(), output) {
        Some(("cargo-build-v1", compact_cargo_build(output)))
    } else if looks_like_tsc(command_text.as_str(), output) {
        Some(("tsc-v1", compact_tsc(output)))
    } else if looks_like_gradle(command_text.as_str(), output) {
        Some(("gradle-v1", compact_gradle(output)))
    } else if looks_like_terraform_plan(command_text.as_str(), output) {
        compact_plan(output, "terraform plan").map(|text| ("terraform-plan-v1", text))
    } else if looks_like_tofu_plan(command_text.as_str(), output) {
        compact_plan(output, "tofu plan").map(|text| ("tofu-plan-v1", text))
    } else if looks_like_uv_sync(command_text.as_str(), output) {
        Some(("uv-sync-v1", compact_uv_sync(output)))
    } else if looks_like_json(output) {
        compact_json(output).map(|text| ("json-structure-v1", text))
    } else if looks_like_generic_log(output) {
        Some(("generic-log-v1", compact_generic_log(output)))
    } else {
        None
    }?;

    keep_if_worthwhile(candidate.0, candidate.1, original_token_count)
}

fn keep_if_worthwhile(
    filter_id: &'static str,
    text: String,
    original_token_count: usize,
) -> Option<CompactedOutput> {
    let compacted_token_count = approx_token_count(text.as_str());
    if compacted_token_count >= original_token_count {
        return None;
    }
    if compacted_token_count * 4 > original_token_count * 3 {
        return None;
    }

    Some(CompactedOutput {
        filter_id,
        text,
        original_token_count,
        compacted_token_count,
    })
}

fn looks_like_cargo_test(command: &str, output: &str) -> bool {
    command.contains("cargo test")
        || output.contains("test result:")
        || output.contains("running ") && output.contains(" test")
}

fn looks_like_cargo_build(command: &str, output: &str) -> bool {
    command.contains("cargo build")
        || command.contains("cargo check")
        || command.contains("cargo clippy")
        || command.contains("rustc")
        || output.contains("error[")
        || output.contains("warning:") && (output.contains("-->") || output.contains("Compiling "))
}

fn looks_like_go_test(command: &str, output: &str) -> bool {
    command.contains("go test")
        || output.contains("=== RUN")
        || output.contains("--- PASS:")
        || output.contains("--- FAIL:")
        || output.lines().any(|line| {
            matches!(first_ascii_field(line), Some("ok" | "FAIL"))
                || line == "PASS"
                || line == "FAIL"
        })
}

fn looks_like_maven_output(command: &str, output: &str) -> bool {
    let command = command.to_ascii_lowercase();
    let command_mentions_maven = command
        .split_ascii_whitespace()
        .any(|part| part == "mvn" || part == "mvnw" || part.contains("maven"));
    let output_mentions_maven = output.lines().take(8).any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("mvn ")
            || trimmed.starts_with("./mvnw ")
            || trimmed.contains("Apache Maven")
    });

    (command_mentions_maven || output_mentions_maven)
        && (output.contains("BUILD SUCCESS")
            || output.contains("BUILD FAILURE")
            || output.contains("Tests run:"))
}

fn looks_like_nextest(command: &str, output: &str) -> bool {
    command.contains("cargo nextest")
        || command.contains("nextest run")
        || output.contains("Nextest run ID")
        || output
            .lines()
            .any(|line| line.trim_start().starts_with("Summary [") && line.contains("tests run:"))
}

fn looks_like_pytest(command: &str, output: &str) -> bool {
    command
        .split_ascii_whitespace()
        .any(|part| part == "pytest" || part.ends_with("/pytest"))
        || command.contains("python -m pytest")
        || output.contains(" short test summary ")
        || output.contains(" FAILURES ")
        || output
            .lines()
            .rev()
            .take(8)
            .any(|line| looks_like_pytest_summary(line.trim()))
}

fn looks_like_tsc(command: &str, output: &str) -> bool {
    command
        .split_ascii_whitespace()
        .any(|part| part == "tsc" || part.ends_with("/tsc"))
        || output
            .lines()
            .take(32)
            .find_map(parse_tsc_diagnostic_line)
            .is_some()
}

fn looks_like_gradle(command: &str, output: &str) -> bool {
    command.split_ascii_whitespace().any(|part| {
        matches!(part, "gradle" | "gradlew" | "./gradlew")
            || part.ends_with("/gradle")
            || part.ends_with("/gradlew")
    }) || output.contains("BUILD SUCCESSFUL")
        || output.contains("BUILD FAILED")
        || output
            .lines()
            .take(64)
            .any(|line| line.starts_with("> Task :"))
}

fn looks_like_terraform_plan(command: &str, output: &str) -> bool {
    command.contains("terraform plan")
        || output.contains("Terraform will perform the following actions:")
        || output.contains("No changes. Your infrastructure matches the configuration.")
}

fn looks_like_tofu_plan(command: &str, output: &str) -> bool {
    command.contains("tofu plan") || output.contains("OpenTofu will perform the following actions:")
}

fn looks_like_uv_sync(command: &str, output: &str) -> bool {
    command.contains("uv sync")
        || command.contains("uv pip install")
        || output.lines().take(16).any(|line| {
            line.trim_start().starts_with("Resolved ")
                || line.trim_start().starts_with("Audited ")
                || line.trim_start().starts_with("Installed ")
        })
}

fn looks_like_json(output: &str) -> bool {
    let trimmed = output.trim_start();
    trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed
            .lines()
            .take(8)
            .filter(|line| serde_json::from_str::<Value>(line.trim()).is_ok())
            .count()
            >= 3
}

fn looks_like_generic_log(output: &str) -> bool {
    output
        .lines()
        .filter(|line| is_log_signal(line))
        .take(8)
        .count()
        >= 3
}

fn compact_cargo_test(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut keep = BTreeSet::new();
    let mut in_failures = false;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "failures:" {
            in_failures = true;
            keep.insert(idx);
            continue;
        }
        if trimmed.starts_with("test result:") {
            in_failures = false;
            keep.insert(idx);
            continue;
        }
        if in_failures || is_cargo_test_signal(trimmed) {
            add_context(&mut keep, idx, lines.len(), /*context*/ 2);
        }
    }

    render_compacted_lines("cargo test output", &lines, &keep)
}

fn is_cargo_test_signal(line: &str) -> bool {
    line.contains("FAILED")
        || line.contains("panicked at")
        || line.starts_with("thread '")
        || line.starts_with("error:")
        || line.starts_with("error[")
        || line.starts_with("warning:")
        || line.starts_with("failures:")
}

fn compact_cargo_build(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut keep = BTreeSet::new();
    let mut in_diagnostic = false;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if is_compiler_diagnostic_start(trimmed) {
            in_diagnostic = true;
            add_context(&mut keep, idx, lines.len(), /*context*/ 1);
            continue;
        }
        if in_diagnostic {
            keep.insert(idx);
            if trimmed.is_empty() {
                in_diagnostic = false;
            }
            continue;
        }
        if trimmed.starts_with("Finished ")
            || trimmed.starts_with("error: could not compile")
            || trimmed.starts_with("warning: build failed")
        {
            keep.insert(idx);
        }
    }

    render_compacted_lines("cargo build output", &lines, &keep)
}

fn compact_nextest(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    let summary = lines
        .iter()
        .rev()
        .find(|line| line.trim_start().starts_with("Summary [") && line.contains("tests run:"))?
        .trim()
        .to_string();

    let has_failure = lines.iter().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("FAIL [")
            || trimmed.contains(" test failed")
            || trimmed.contains(" failed,")
    });
    if has_failure {
        return None;
    }

    let pass_lines = lines
        .iter()
        .filter(|line| line.trim_start().starts_with("PASS ["))
        .count();
    let compile_lines = lines
        .iter()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("Compiling ") || trimmed.starts_with("Checking ")
        })
        .count();
    let warning_lines = lines
        .iter()
        .filter(|line| line.trim_start().starts_with("warning:"))
        .count();
    let mut sections = vec![format!("nextest output: {summary}")];

    let slow_lines = lines
        .iter()
        .filter(|line| line.trim_start().starts_with("SLOW ["))
        .take(12)
        .map(|line| line.trim().to_string())
        .collect::<Vec<_>>();
    if !slow_lines.is_empty() {
        sections.push("Slow tests:".to_string());
        sections.extend(slow_lines);
    }

    if warning_lines > 0 {
        let first_warning = lines
            .iter()
            .find(|line| line.trim_start().starts_with("warning:"))
            .map_or("warning details omitted", |line| line.trim());
        sections.push(format!(
            "Build warnings omitted: {warning_lines}. First warning: {first_warning}"
        ));
    }
    sections.push(format!(
        "Omitted {pass_lines} passing test lines and {compile_lines} compile/check progress lines."
    ));
    Some(sections.join("\n"))
}

fn compact_pytest(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let summary = lines
        .iter()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            looks_like_pytest_summary(trimmed).then(|| trimmed.trim_matches('=').trim().to_string())
        })
        .unwrap_or_else(|| "pytest summary unavailable".to_string());
    let failed = summary.contains(" failed")
        || summary.contains(" error")
        || lines
            .iter()
            .any(|line| line.trim_start().starts_with("FAILED ") || line.contains(" FAILURES "));

    if !failed {
        return format!("pytest output: {summary}\nOmitted passing-test progress noise.");
    }

    let mut keep = BTreeSet::new();
    let mut in_failures = false;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("===") && trimmed.contains("FAILURES") {
            in_failures = true;
            keep.insert(idx);
            continue;
        }
        if trimmed.starts_with("===") && trimmed.contains("short test summary") {
            in_failures = false;
            keep.insert(idx);
            continue;
        }
        if in_failures
            || trimmed.starts_with("FAILED ")
            || trimmed.starts_with("ERROR ")
            || trimmed.starts_with("XFAIL ")
            || trimmed.starts_with("XPASS ")
            || looks_like_pytest_summary(trimmed)
        {
            add_context(&mut keep, idx, lines.len(), /*context*/ 1);
        }
    }

    render_compacted_lines("pytest output", &lines, &keep)
}

fn looks_like_pytest_summary(line: &str) -> bool {
    let line = line.trim_matches('=').trim();
    if line.starts_with("test result:") {
        return false;
    }
    (line.contains(" passed")
        || line.contains(" failed")
        || line.contains(" errors")
        || line.contains(" error")
        || line.contains(" skipped")
        || line.contains(" xfailed")
        || line.contains(" xpassed"))
        && line.contains(" in ")
}

#[derive(Debug)]
struct TscDiagnostic {
    file: String,
    line: String,
    column: String,
    severity: String,
    code: String,
    message: String,
    context: Vec<String>,
}

fn compact_tsc(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut diagnostics = Vec::<TscDiagnostic>::new();
    let mut idx = 0usize;
    while idx < lines.len() {
        let Some(mut diagnostic) = parse_tsc_diagnostic_line(lines[idx]) else {
            idx += 1;
            continue;
        };
        idx += 1;
        while idx < lines.len()
            && parse_tsc_diagnostic_line(lines[idx]).is_none()
            && (lines[idx].starts_with(' ') || lines[idx].starts_with('\t'))
        {
            diagnostic
                .context
                .push(truncate_chars(lines[idx].trim(), /*max_chars*/ 160));
            idx += 1;
        }
        diagnostics.push(diagnostic);
    }

    if diagnostics.is_empty() {
        return "TypeScript: no diagnostics found".to_string();
    }

    let mut by_file = BTreeMap::<String, Vec<&TscDiagnostic>>::new();
    let mut by_code = BTreeMap::<String, usize>::new();
    for diagnostic in &diagnostics {
        by_file
            .entry(diagnostic.file.clone())
            .or_default()
            .push(diagnostic);
        *by_code.entry(diagnostic.code.clone()).or_default() += 1;
    }

    let mut sections = vec![format!(
        "TypeScript: {} diagnostics in {} files.",
        diagnostics.len(),
        by_file.len()
    )];
    let mut code_counts = by_code.into_iter().collect::<Vec<_>>();
    code_counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    sections.push(format!(
        "Top codes: {}",
        code_counts
            .iter()
            .take(6)
            .map(|(code, count)| format!("{code} ({count}x)"))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    for (file, diagnostics) in by_file.iter().take(40) {
        sections.push(format!("{file} ({} diagnostics)", diagnostics.len()));
        for diagnostic in diagnostics.iter().take(12) {
            sections.push(format!(
                "  L{}:{} {} {}: {}",
                diagnostic.line,
                diagnostic.column,
                diagnostic.severity,
                diagnostic.code,
                truncate_chars(diagnostic.message.as_str(), /*max_chars*/ 180)
            ));
            for context in diagnostic.context.iter().take(3) {
                sections.push(format!("    {context}"));
            }
        }
        if diagnostics.len() > 12 {
            sections.push(format!(
                "  ... {} additional diagnostics in this file omitted",
                diagnostics.len() - 12
            ));
        }
    }
    if by_file.len() > 40 {
        sections.push(format!(
            "... {} additional files with diagnostics omitted",
            by_file.len() - 40
        ));
    }
    sections.join("\n")
}

fn parse_tsc_diagnostic_line(line: &str) -> Option<TscDiagnostic> {
    let (file_and_pos, rest) = line.split_once("): ")?;
    let open = file_and_pos.rfind('(')?;
    let file = &file_and_pos[..open];
    let (line_number, column) = file_and_pos[open + 1..].split_once(',')?;
    if line_number.parse::<usize>().is_err() || column.parse::<usize>().is_err() {
        return None;
    }

    let mut parts = rest.splitn(3, ' ');
    let severity = parts.next()?;
    if !matches!(severity, "error" | "warning") {
        return None;
    }
    let code = parts.next()?.strip_suffix(':')?;
    if !code.starts_with("TS") {
        return None;
    }
    let message = parts.next().unwrap_or_default();
    Some(TscDiagnostic {
        file: file.to_string(),
        line: line_number.to_string(),
        column: column.to_string(),
        severity: severity.to_string(),
        code: code.to_string(),
        message: message.to_string(),
        context: Vec::new(),
    })
}

fn compact_gradle(output: &str) -> String {
    let cleaned = strip_ansi_codes(output);
    let mut omitted = 0usize;
    let mut kept = Vec::new();
    for line in cleaned.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("> Configuring project")
            || trimmed.starts_with("> Resolving dependencies")
            || trimmed.starts_with("> Transform ")
            || trimmed.starts_with("Downloading ")
            || trimmed.starts_with("Download ")
            || trimmed.starts_with("Starting a Gradle Daemon")
            || trimmed.starts_with("Daemon will be stopped")
            || (trimmed.starts_with("> Task :")
                && (trimmed.ends_with("UP-TO-DATE")
                    || trimmed.ends_with("NO-SOURCE")
                    || trimmed.ends_with("FROM-CACHE")))
        {
            omitted += 1;
            continue;
        }
        kept.push(truncate_chars(line, /*max_chars*/ 180));
    }

    let mut sections = vec![format!(
        "Compacted gradle output: kept {} lines, omitted {omitted} noisy lines.",
        kept.len()
    )];
    sections.extend(limit_lines(kept, /*max_lines*/ 80, "gradle output"));
    sections.join("\n")
}

fn compact_plan(output: &str, label: &str) -> Option<String> {
    let cleaned = strip_ansi_codes(output);
    let mut omitted = 0usize;
    let mut kept = Vec::new();
    for line in cleaned.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Refreshing state")
            || trimmed.contains(": Refreshing state")
            || trimmed.ends_with(": Reading...")
            || trimmed.contains(": Read complete after ")
            || trimmed.starts_with("Acquiring state lock")
            || trimmed.starts_with("Releasing state lock")
            || (trimmed.starts_with('#') && trimmed.contains("unchanged"))
        {
            omitted += 1;
            continue;
        }
        kept.push(line.to_string());
    }

    if kept.is_empty() {
        return None;
    }
    let has_signal = kept.iter().any(|line| {
        line.contains("Plan:")
            || line.contains("No changes.")
            || line.contains("will perform the following actions")
            || line.contains("Error:")
    });
    if !has_signal {
        return None;
    }

    let mut sections = vec![format!(
        "Compacted {label} output: kept {} lines, omitted {omitted} refresh/lock/no-change lines.",
        kept.len()
    )];
    sections.extend(limit_lines(kept, /*max_lines*/ 120, label));
    Some(sections.join("\n"))
}

fn compact_uv_sync(output: &str) -> String {
    let cleaned = strip_ansi_codes(output);
    if cleaned.contains("Audited ")
        && !cleaned.lines().any(|line| {
            let trimmed = line.trim_start();
            is_log_signal(line)
                || line.contains(" - ")
                || line.contains(" + ")
                || trimmed.starts_with("Installed ")
                || trimmed.starts_with("Uninstalled ")
        })
    {
        return "uv: ok (up to date)".to_string();
    }

    let mut omitted = 0usize;
    let mut kept = Vec::new();
    for line in cleaned.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty()
            || trimmed.starts_with("Downloading ")
            || trimmed.starts_with("Using cached ")
            || trimmed.starts_with("Preparing ")
        {
            omitted += 1;
            continue;
        }
        kept.push(truncate_chars(line, /*max_chars*/ 180));
    }

    if kept.is_empty() {
        return "uv: ok".to_string();
    }
    let mut sections = vec![format!(
        "Compacted uv output: kept {} lines, omitted {omitted} download/cache lines.",
        kept.len()
    )];
    sections.extend(limit_lines(kept, /*max_lines*/ 60, "uv output"));
    sections.join("\n")
}

fn compact_go_test(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let failed = lines.iter().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("--- FAIL:") || trimmed.starts_with("FAIL\t") || trimmed == "FAIL"
    });

    if !failed {
        return summarize_successful_go_test(&lines);
    }

    let mut keep = BTreeSet::new();
    let mut run_starts = BTreeMap::<String, usize>::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(name) = go_test_name(trimmed, "=== RUN") {
            run_starts.insert(name, idx);
            continue;
        }
        if let Some(name) = go_test_name(trimmed, "=== CONT") {
            run_starts.entry(name).or_insert(idx);
            continue;
        }
        if let Some(name) = go_test_name(trimmed, "--- FAIL:") {
            let start = run_starts
                .get(&name)
                .copied()
                .unwrap_or_else(|| idx.saturating_sub(20));
            for line_idx in start..=idx {
                keep.insert(line_idx);
            }
            continue;
        }
        if trimmed.starts_with("FAIL\t") || trimmed == "FAIL" || is_log_signal(trimmed) {
            add_context(&mut keep, idx, lines.len(), /*context*/ 2);
        }
    }

    render_compacted_lines("go test output", &lines, &keep)
}

fn summarize_successful_go_test(lines: &[&str]) -> String {
    let mut passed_tests = 0usize;
    let mut skipped_tests = 0usize;
    let mut ok_packages = 0usize;
    let mut no_test_packages = 0usize;
    let mut no_matching_notices = 0usize;
    let mut signal_lines = Vec::new();

    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("--- PASS:") {
            passed_tests += 1;
        } else if trimmed.starts_with("--- SKIP:") {
            skipped_tests += 1;
        } else if first_ascii_field(trimmed) == Some("ok") {
            ok_packages += 1;
        } else if first_ascii_field(trimmed) == Some("?") && trimmed.contains("[no test files]") {
            no_test_packages += 1;
        } else if trimmed.contains("[no tests to run]")
            || trimmed == "testing: warning: no tests to run"
        {
            no_matching_notices += 1;
        } else if is_log_signal(trimmed) && signal_lines.len() < 8 {
            signal_lines.push(trimmed.to_string());
        }
    }

    let mut sections = vec![format!(
        "go test output: PASS. {passed_tests} tests passed, {skipped_tests} skipped; {ok_packages} packages ok; {no_test_packages} packages with no test files; {no_matching_notices} no-matching-test notices omitted."
    )];
    if !signal_lines.is_empty() {
        sections.push("Retained warning/error lines:".to_string());
        sections.extend(signal_lines);
    }
    sections.push("Omitted verbose passing-test and package noise.".to_string());
    sections.join("\n")
}

fn go_test_name(line: &str, marker: &str) -> Option<String> {
    let rest = line.strip_prefix(marker)?.trim_start();
    let name = rest.split_once(" (").map_or(rest, |(name, _)| name).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn first_ascii_field(line: &str) -> Option<&str> {
    line.split_ascii_whitespace().next()
}

fn compact_maven_output(output: &str) -> String {
    let cleaned = strip_ansi_codes(output);
    let lines: Vec<String> = cleaned.lines().map(str::to_string).collect();
    let failed = lines.iter().any(|line| {
        let trimmed = line.trim();
        trimmed.contains("BUILD FAILURE")
            || trimmed.contains("[ERROR]")
            || maven_test_summary_failed(trimmed)
            || trimmed.contains("<<< FAILURE!")
            || trimmed.contains("<<< ERROR!")
    });

    if !failed {
        return summarize_successful_maven_output(&lines);
    }

    let mut keep = BTreeSet::new();
    let mut current_test_start = None;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("[INFO] Running ") {
            current_test_start = Some(idx);
            continue;
        }

        let failure_signal = trimmed.contains("BUILD FAILURE")
            || trimmed.contains("[ERROR]")
            || maven_test_summary_failed(trimmed)
            || trimmed.contains("<<< FAILURE!")
            || trimmed.contains("<<< ERROR!")
            || trimmed.contains("Failed tests:")
            || trimmed.contains("Tests in error:")
            || trimmed.contains("There are test failures")
            || trimmed.starts_with("Caused by:");

        if failure_signal {
            if maven_test_summary_failed(trimmed) || trimmed.contains("<<< FAILURE!") {
                let start = current_test_start.unwrap_or_else(|| idx.saturating_sub(20));
                for line_idx in start..=idx {
                    keep.insert(line_idx);
                }
            }
            add_context(&mut keep, idx, lines.len(), /*context*/ 2);
        }
    }

    render_compacted_lines("maven output", &lines, &keep)
}

fn summarize_successful_maven_output(lines: &[String]) -> String {
    let test_summary = lines
        .iter()
        .rev()
        .find(|line| line.contains("Tests run:"))
        .map(|line| trim_maven_log_prefix(line));
    let succeeded_modules = lines
        .iter()
        .filter(|line| {
            line.contains(" SUCCESS")
                && !line.contains("BUILD SUCCESS")
                && !line.contains("Reactor Summary")
        })
        .count();
    let debug_lines = lines.iter().filter(|line| line.contains("[DEBUG]")).count();
    let download_lines = lines
        .iter()
        .filter(|line| line.contains("Downloading from") || line.contains("Downloaded from"))
        .count();
    let progress_lines = lines
        .iter()
        .filter(|line| line.contains("Progress ("))
        .count();
    let warning_lines = lines
        .iter()
        .filter(|line| line.contains("[WARNING]"))
        .collect::<Vec<_>>();
    let total_time = lines
        .iter()
        .find(|line| line.contains("Total time:"))
        .map(|line| trim_maven_log_prefix(line));

    let mut sections = vec!["Maven output: BUILD SUCCESS.".to_string()];
    if let Some(test_summary) = test_summary {
        sections.push(test_summary);
    }
    if succeeded_modules > 0 {
        sections.push(format!("Reactor modules succeeded: {succeeded_modules}."));
    }
    if let Some(total_time) = total_time {
        sections.push(total_time);
    }
    if !warning_lines.is_empty() {
        sections.push(format!(
            "Warnings omitted: {}. First warning: {}",
            warning_lines.len(),
            trim_maven_log_prefix(warning_lines[0])
        ));
    }
    sections.push(format!(
        "Omitted Maven lifecycle/download/progress/passing-test noise: {debug_lines} debug lines, {download_lines} download lines, {progress_lines} progress lines."
    ));
    sections.join("\n")
}

fn trim_maven_log_prefix(line: &str) -> String {
    let trimmed = line.trim();
    trimmed
        .strip_prefix("[INFO]")
        .or_else(|| trimmed.strip_prefix("[WARNING]"))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn maven_test_summary_failed(line: &str) -> bool {
    line.contains("Tests run:")
        && (!line.contains("Failures: 0")
            || !line.contains("Errors: 0")
            || line.contains("<<< FAILURE!")
            || line.contains("<<< ERROR!"))
}

fn strip_ansi_codes(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if ch != '\r' {
            stripped.push(ch);
        }
    }
    stripped
}

fn is_compiler_diagnostic_start(line: &str) -> bool {
    line.starts_with("error")
        || line.starts_with("warning:")
        || line.starts_with("note:")
        || line.starts_with("help:")
        || line.starts_with("-->")
}

fn compact_json(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(summarize_json_value(&value));
    }

    let mut count = 0usize;
    let mut object_keys = BTreeMap::<String, usize>::new();
    let mut representatives = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return None;
        };
        count += 1;
        if representatives.len() < 3 {
            representatives.push(summarize_json_value(&value));
        }
        if let Value::Object(object) = value {
            for key in object.keys() {
                *object_keys.entry(key.clone()).or_insert(0) += 1;
            }
        }
    }

    if count == 0 {
        return None;
    }

    let keys = object_keys.keys().take(24).cloned().collect::<Vec<_>>();
    let mut sections = vec![format!("NDJSON: {count} records")];
    if !keys.is_empty() {
        sections.push(format!("Object keys observed: {}", keys.join(", ")));
    }
    sections.push("Representative records:".to_string());
    sections.extend(representatives);
    Some(sections.join("\n"))
}

fn summarize_json_value(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let keys = object.keys().take(24).cloned().collect::<Vec<_>>();
            let mut sections = vec![format!(
                "JSON object with {} top-level keys: {}",
                object.len(),
                keys.join(", ")
            )];
            for (key, value) in object.iter().take(12) {
                sections.push(format!("- {key}: {}", json_shape(value)));
            }
            sections.join("\n")
        }
        Value::Array(items) => {
            let mut sections = vec![format!("JSON array with {} items", items.len())];
            if let Some(first) = items.first() {
                sections.push(format!("First item shape: {}", json_shape(first)));
            }
            for (idx, value) in items.iter().take(3).enumerate() {
                sections.push(format!("- item {idx}: {}", json_shape(value)));
            }
            sections.join("\n")
        }
        value => format!("JSON scalar: {}", json_shape(value)),
    }
}

fn json_shape(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(text) => format!("string({} chars)", text.chars().count()),
        Value::Array(items) => format!("array({} items)", items.len()),
        Value::Object(object) => {
            let keys = object.keys().take(8).cloned().collect::<Vec<_>>();
            format!("object({} keys: {})", object.len(), keys.join(", "))
        }
    }
}

fn compact_generic_log(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut keep = BTreeSet::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for (idx, line) in lines.iter().enumerate() {
        if is_log_signal(line) {
            add_context(&mut keep, idx, lines.len(), /*context*/ 1);
            *counts.entry(line.trim().to_string()).or_insert(0) += 1;
        }
    }

    let mut sections = vec!["Grouped warning/error log output".to_string()];
    for (line, count) in counts.iter().filter(|(_, count)| **count > 1).take(12) {
        sections.push(format!("- repeated {count}x: {line}"));
    }
    sections.push(render_compacted_lines("log output", &lines, &keep));
    sections.join("\n")
}

fn is_log_signal(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("warning")
        || lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("exception")
}

fn add_context(keep: &mut BTreeSet<usize>, idx: usize, len: usize, context: usize) {
    let start = idx.saturating_sub(context);
    let end = idx.saturating_add(context).min(len.saturating_sub(1));
    for line_idx in start..=end {
        keep.insert(line_idx);
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx == max_chars {
            truncated.push_str("...");
            return truncated;
        }
        truncated.push(ch);
    }
    truncated
}

fn limit_lines(mut lines: Vec<String>, max_lines: usize, label: &str) -> Vec<String> {
    if lines.len() <= max_lines {
        return lines;
    }
    let omitted = lines.len() - max_lines;
    lines.truncate(max_lines);
    lines.push(format!("... {omitted} additional {label} lines omitted"));
    lines
}

fn render_compacted_lines<S: AsRef<str>>(
    label: &str,
    lines: &[S],
    keep: &BTreeSet<usize>,
) -> String {
    let omitted = lines.len().saturating_sub(keep.len());
    let mut sections = Vec::new();
    sections.push(format!(
        "Compacted {label}: kept {} of {} lines, omitted {omitted} noisy lines.",
        keep.len(),
        lines.len()
    ));

    let mut previous = None;
    for idx in keep {
        if previous.is_some_and(|previous_idx| idx.saturating_sub(previous_idx) > 1) {
            sections.push("[... omitted lines ...]".to_string());
        }
        sections.push(lines[*idx].as_ref().to_string());
        previous = Some(*idx);
    }

    sections.join("\n")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
