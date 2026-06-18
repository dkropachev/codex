use super::types::OutputOptimizationSuggestion;
use crate::runtime::StateRuntime;
use crate::runtime::tool_router::ToolRouterLedgerEntry;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const MIN_SUGGESTION_SAVINGS_BASIS_POINTS: i64 = 2_500;
const RECOVERY_LOOKBACK_MS: i64 = 10 * 60 * 1000;

impl StateRuntime {
    pub async fn tool_router_recent_duplicate_source_read_for_command(
        &self,
        thread_id: &str,
        current_call_id: &str,
        command: &str,
    ) -> anyhow::Result<bool> {
        let Some(source_read) = parse_source_read(command) else {
            return Ok(false);
        };
        self.thread_has_recent_source_read(
            thread_id,
            source_read.path.as_str(),
            current_call_id,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
    }

    pub(super) async fn output_optimization_suggestions(
        &self,
        entry: &ToolRouterLedgerEntry,
        now_ms: i64,
        min_original_tokens: i64,
    ) -> anyhow::Result<Vec<OutputOptimizationSuggestion>> {
        let Some(command) = command_from_tool_input(entry.tool_input_json.as_deref()) else {
            return Ok(Vec::new());
        };
        let Some(output_text) = output_text_from_tool_output(entry.tool_output_json.as_deref())
        else {
            return Ok(Vec::new());
        };

        let original_output_tokens = entry
            .original_output_tokens
            .max(entry.returned_output_tokens);
        if original_output_tokens < min_original_tokens {
            return Ok(Vec::new());
        }

        let mut suggestions = Vec::new();
        if command_looks_like_rg(command.as_str()) {
            suggestions.extend(build_token_saving_suggestion(
                "exec.rg-summary-v1",
                "Summarize rg output by keeping all matched filenames and representative matches.",
                original_output_tokens,
                entry.returned_output_tokens,
                estimate_rg_summary_tokens(output_text.as_str()),
            ));
        }

        if command_looks_like_test(command.as_str()) {
            suggestions.extend(build_token_saving_suggestion(
                "exec.test-output-filter-v1",
                "Show failure details and concise success summaries for test/build output.",
                original_output_tokens,
                entry.returned_output_tokens,
                estimate_test_output_tokens(command.as_str(), output_text.as_str()),
            ));
        }

        if let Some(source_read) = parse_source_read(command.as_str())
            && self
                .thread_has_recent_source_read(
                    entry.thread_id.as_str(),
                    source_read.path.as_str(),
                    entry.call_id.as_str(),
                    now_ms,
                )
                .await?
        {
            suggestions.extend(build_token_saving_suggestion(
                "exec.source-read-dedupe-v1",
                "Omit source lines already shown earlier in the thread unless the file changed.",
                original_output_tokens,
                entry.returned_output_tokens,
                Some(estimate_source_read_dedupe_tokens(
                    output_text.as_str(),
                    &source_read,
                )),
            ));
        }

        Ok(suggestions)
    }

    async fn thread_has_recent_source_read(
        &self,
        thread_id: &str,
        path: &str,
        current_call_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let cutoff_ms = now_ms.saturating_sub(RECOVERY_LOOKBACK_MS);
        let like_pattern = format!("%{path}%");
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM tool_router_ledger
            WHERE thread_id = ?
              AND call_id != ?
              AND created_at_ms >= ?
              AND tool_input_json LIKE ?
            "#,
        )
        .bind(thread_id)
        .bind(current_call_id)
        .bind(cutoff_ms)
        .bind(like_pattern)
        .fetch_one(self.pool.as_ref())
        .await?;
        Ok(count > 0)
    }
}

pub(super) fn already_optimized_suggestion(
    entry: &ToolRouterLedgerEntry,
    max_average_tokens: i64,
) -> Option<OutputOptimizationSuggestion> {
    let command = command_from_tool_input(entry.tool_input_json.as_deref())?;
    let family = command_family(command.as_str());
    let original_output_tokens = entry
        .original_output_tokens
        .max(entry.returned_output_tokens);
    if original_output_tokens > max_average_tokens {
        return None;
    }
    Some(OutputOptimizationSuggestion {
        suggestion_key: format!("exec.{family}.already-optimized-v1"),
        suggestion_label: "Stop checking this output family after repeated low-volume results."
            .to_string(),
        original_output_tokens,
        returned_output_tokens: entry.returned_output_tokens,
        candidate_output_tokens: entry.returned_output_tokens,
        saved_output_tokens: 0,
    })
}

pub(super) fn recovery_reason(entry: &ToolRouterLedgerEntry) -> Option<String> {
    match entry.tool_name.as_deref() {
        Some("read_exec_output") => Some("raw exec output recovery requested".to_string()),
        _ => None,
    }
}

pub(super) fn basis_points(numerator: i64, denominator: i64) -> i64 {
    if denominator == 0 {
        0
    } else {
        numerator.saturating_mul(10_000) / denominator
    }
}

fn build_token_saving_suggestion(
    suggestion_key: &str,
    suggestion_label: &str,
    original_output_tokens: i64,
    returned_output_tokens: i64,
    candidate_output_tokens: Option<i64>,
) -> Vec<OutputOptimizationSuggestion> {
    let Some(candidate_output_tokens) = candidate_output_tokens else {
        return Vec::new();
    };
    if candidate_output_tokens >= original_output_tokens {
        return Vec::new();
    }

    let saved_output_tokens = original_output_tokens.saturating_sub(candidate_output_tokens);
    if basis_points(saved_output_tokens, original_output_tokens)
        < MIN_SUGGESTION_SAVINGS_BASIS_POINTS
    {
        return Vec::new();
    }

    vec![OutputOptimizationSuggestion {
        suggestion_key: suggestion_key.to_string(),
        suggestion_label: suggestion_label.to_string(),
        original_output_tokens,
        returned_output_tokens,
        candidate_output_tokens,
        saved_output_tokens,
    }]
}

fn command_from_tool_input(tool_input_json: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<Value>(tool_input_json?).ok()?;
    for key in ["cmd", "command"] {
        if let Some(command) = value.get(key).and_then(Value::as_str) {
            return Some(command.to_string());
        }
    }
    value.as_str().map(str::to_string)
}

fn output_text_from_tool_output(tool_output_json: Option<&str>) -> Option<String> {
    let value = serde_json::from_str::<Value>(tool_output_json?).ok()?;
    let mut texts = Vec::new();
    collect_output_text(&value, &mut texts);
    let text = texts.join("\n");
    if text.is_empty() { None } else { Some(text) }
}

fn collect_output_text(value: &Value, texts: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if text.contains('\n') || text.contains("Output:") || text.len() > 80 {
                texts.push(text.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_output_text(item, texts);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if !matches!(key.as_str(), "call_id" | "type" | "name") {
                    collect_output_text(value, texts);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn command_family(command: &str) -> &'static str {
    if command_looks_like_rg(command) {
        "rg"
    } else if command_looks_like_test(command) {
        "test"
    } else if parse_source_read(command).is_some() {
        "source-read"
    } else {
        "generic"
    }
}

fn command_looks_like_rg(command: &str) -> bool {
    command
        .split_ascii_whitespace()
        .any(|part| matches!(part, "rg" | "ripgrep") || part.ends_with("/rg"))
}

fn command_looks_like_test(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    command.contains("cargo test")
        || command.contains("cargo nextest")
        || command.contains("nextest run")
        || command.contains("go test")
        || command.contains("pytest")
        || command.contains("python -m pytest")
        || command.contains("just test")
        || command
            .split_ascii_whitespace()
            .any(|part| matches!(part, "mvn" | "mvnw") || part.ends_with("/mvn"))
}

#[derive(Debug)]
struct SourceRead {
    path: String,
    start_line: i64,
    end_line: i64,
}

fn parse_source_read(command: &str) -> Option<SourceRead> {
    let sed_marker = "sed -n ";
    let sed_idx = command.find(sed_marker)?;
    let after_sed = &command[sed_idx + sed_marker.len()..];
    let (range, rest) = quoted_prefix(after_sed)?;
    let (start_line, end_line) = parse_sed_range(range)?;
    let path = rest
        .split([' ', '|', ';', '&'])
        .find(|part| !part.is_empty() && !part.starts_with('-'))
        .or_else(|| path_before_nl_pipe(command, sed_idx))?;
    Some(SourceRead {
        path: path.trim_matches(['"', '\'']).to_string(),
        start_line,
        end_line,
    })
}

fn quoted_prefix(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    let quote = text.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let rest = &text[quote.len_utf8()..];
    let end = rest.find(quote)?;
    let range = &rest[..end];
    Some((range, rest[end + quote.len_utf8()..].trim_start()))
}

fn parse_sed_range(range: &str) -> Option<(i64, i64)> {
    let range = range.strip_suffix('p')?;
    if let Some((start, end)) = range.split_once(',') {
        return Some((start.parse().ok()?, end.parse().ok()?));
    }
    let line = range.parse().ok()?;
    Some((line, line))
}

fn path_before_nl_pipe(command: &str, sed_idx: usize) -> Option<&str> {
    let prefix = &command[..sed_idx];
    let pipe_idx = prefix.rfind('|')?;
    let before_pipe = prefix[..pipe_idx].trim_end();
    before_pipe
        .split_ascii_whitespace()
        .rev()
        .find(|part| !part.starts_with('-') && *part != "nl")
}

fn estimate_rg_summary_tokens(output: &str) -> Option<i64> {
    let mut files = BTreeMap::<String, (i64, Vec<String>)>::new();
    for line in output.lines() {
        let Some((file, rest)) = line.split_once(':') else {
            continue;
        };
        let Some((line_number, text)) = rest.split_once(':') else {
            continue;
        };
        if line_number.parse::<i64>().is_err() {
            continue;
        }
        let entry = files.entry(file.to_string()).or_insert((0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < 2 {
            entry
                .1
                .push(format!("{file}:{line_number}: {}", text.trim()));
        }
    }
    if files.is_empty() {
        return None;
    }

    let mut summary = format!(
        "rg summary: {} matches in {} files.\nMatched files:\n",
        files.values().map(|(count, _)| count).sum::<i64>(),
        files.len()
    );
    for (file, (count, _)) in &files {
        summary.push_str(format!("- {file} ({count})\n").as_str());
    }
    summary.push_str("Sample matches:\n");
    for (_, (_, samples)) in files.iter().take(30) {
        for sample in samples {
            summary.push_str(sample);
            summary.push('\n');
        }
    }
    Some(estimate_text_tokens(summary.as_str()))
}

fn estimate_test_output_tokens(command: &str, output: &str) -> Option<i64> {
    let command = command.to_ascii_lowercase();
    if command.contains("go test") {
        return Some(estimate_go_test_output_tokens(output));
    }
    if command.contains("cargo test") {
        return Some(estimate_cargo_test_output_tokens(output));
    }
    if command.contains("cargo nextest")
        || command.contains("nextest run")
        || command.contains("just test")
    {
        return Some(estimate_nextest_output_tokens(output));
    }
    if command.contains("pytest") || command.contains("python -m pytest") {
        return Some(estimate_pytest_output_tokens(output));
    }
    if command
        .split_ascii_whitespace()
        .any(|part| matches!(part, "mvn" | "mvnw") || part.ends_with("/mvn"))
    {
        return Some(estimate_maven_output_tokens(output));
    }
    None
}

fn estimate_go_test_output_tokens(output: &str) -> i64 {
    let failed = output
        .lines()
        .any(|line| line.starts_with("--- FAIL:") || line == "FAIL" || line.starts_with("FAIL\t"));
    if !failed {
        return 80;
    }
    estimate_signal_lines_tokens(output)
}

fn estimate_cargo_test_output_tokens(output: &str) -> i64 {
    if output.contains("test result: ok") && !output.contains("test result: FAILED") {
        return 80;
    }
    estimate_signal_lines_tokens(output)
}

fn estimate_nextest_output_tokens(output: &str) -> i64 {
    let failed = output.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("FAIL [") || line.contains(" test failed") || line.contains(" failed,")
    });
    if !failed && output.contains("tests run:") {
        return 80;
    }
    estimate_signal_lines_tokens(output)
}

fn estimate_pytest_output_tokens(output: &str) -> i64 {
    let failed = output.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("FAILED ")
            || line.starts_with("ERROR ")
            || line.contains(" FAILURES ")
            || line.contains(" failed")
            || line.contains(" error")
    });
    if !failed {
        return 80;
    }
    estimate_signal_lines_tokens(output)
}

fn estimate_maven_output_tokens(output: &str) -> i64 {
    if output.contains("BUILD SUCCESS") && !output.contains("BUILD FAILURE") {
        return 180;
    }
    estimate_signal_lines_tokens(output)
}

fn estimate_signal_lines_tokens(output: &str) -> i64 {
    let mut kept = BTreeSet::new();
    let lines = output.lines().collect::<Vec<_>>();
    for (idx, line) in lines.iter().enumerate() {
        if is_signal_line(line) {
            for keep_idx in idx.saturating_sub(2)..=(idx + 4).min(lines.len().saturating_sub(1)) {
                kept.insert(keep_idx);
            }
        }
    }
    if kept.is_empty() {
        return 160;
    }
    let mut text = String::new();
    for idx in kept {
        text.push_str(lines[idx]);
        text.push('\n');
    }
    estimate_text_tokens(text.as_str())
}

fn is_signal_line(line: &str) -> bool {
    let line = line.trim_start();
    line.contains("ERROR")
        || line.contains("FAIL")
        || line.contains("FAILED")
        || line.contains("BUILD FAILURE")
        || line.contains("panic:")
        || line.contains("panicked at")
        || line.contains("cannot find")
        || line.contains("Tests run:")
        || line.starts_with("error:")
        || line.starts_with("error[")
        || line.starts_with("warning:")
}

fn estimate_source_read_dedupe_tokens(output: &str, source_read: &SourceRead) -> i64 {
    let line_count = source_read
        .end_line
        .saturating_sub(source_read.start_line)
        .saturating_add(1);
    let estimated_new_lines = line_count.min(20);
    let average_line_tokens = (estimate_text_tokens(output) / line_count.max(1)).max(1);
    32 + estimated_new_lines.saturating_mul(average_line_tokens)
}

pub(super) fn estimate_text_tokens(text: &str) -> i64 {
    i64::try_from(text.len().div_ceil(4)).unwrap_or(i64::MAX)
}
