use super::CompactedOutput;
use super::MIN_RAW_TOKENS;
use super::compact_output;
use super::keep_if_worthwhile;
use codex_utils_output_truncation::approx_token_count;
use std::collections::BTreeMap;

const RG_SUMMARY_SUGGESTION_KEY: &str = "exec.rg-summary-v1";
const TEST_OUTPUT_FILTER_SUGGESTION_KEY: &str = "exec.test-output-filter-v1";
const SOURCE_READ_DEDUPE_SUGGESTION_KEY: &str = "exec.source-read-dedupe-v1";
const MAX_RG_FILES: usize = 200;
const MAX_RG_LINE_NUMBERS_PER_FILE: usize = 8;
const MAX_RG_SAMPLES: usize = 80;
const MAX_RG_SIGNAL_LINES: usize = 20;
const MAX_RG_SAMPLE_CHARS: usize = 180;

#[derive(Default)]
struct RgFileSummary {
    count: usize,
    line_numbers: Vec<String>,
    samples: Vec<String>,
}

pub fn compact_output_for_suggestions(
    command: &[String],
    output: &str,
    suggestion_keys: &[String],
) -> Option<CompactedOutput> {
    let original_token_count = approx_token_count(output);
    if original_token_count < MIN_RAW_TOKENS {
        return None;
    }

    let mut best = None;
    for suggestion_key in suggestion_keys {
        let Some(candidate) = compact_output_for_suggestion(
            command,
            output,
            suggestion_key.as_str(),
            original_token_count,
        ) else {
            continue;
        };

        if best.as_ref().is_none_or(|best: &CompactedOutput| {
            candidate.compacted_token_count < best.compacted_token_count
        }) {
            best = Some(candidate);
        }
    }
    best
}

fn compact_output_for_suggestion(
    command: &[String],
    output: &str,
    suggestion_key: &str,
    original_token_count: usize,
) -> Option<CompactedOutput> {
    match suggestion_key {
        RG_SUMMARY_SUGGESTION_KEY if command_looks_like_rg(command) => keep_if_worthwhile(
            RG_SUMMARY_SUGGESTION_KEY,
            compact_rg_summary(output)?,
            original_token_count,
        ),
        TEST_OUTPUT_FILTER_SUGGESTION_KEY => compact_output(command, output).filter(|candidate| {
            matches!(
                candidate.filter_id,
                "cargo-test-v1" | "go-test-v1" | "maven-v1" | "nextest-v1" | "pytest-v1"
            )
        }),
        SOURCE_READ_DEDUPE_SUGGESTION_KEY => keep_if_worthwhile(
            SOURCE_READ_DEDUPE_SUGGESTION_KEY,
            compact_source_read_dedupe(command, output)?,
            original_token_count,
        ),
        _ => None,
    }
}

fn command_looks_like_rg(command: &[String]) -> bool {
    command.iter().any(|part| {
        part.split_ascii_whitespace()
            .any(|word| matches!(word, "rg" | "ripgrep") || word.ends_with("/rg"))
    })
}

fn compact_rg_summary(output: &str) -> Option<String> {
    let mut files = BTreeMap::<String, RgFileSummary>::new();
    let mut total_matches = 0usize;
    let mut signal_lines = Vec::new();

    for line in output.lines() {
        if let Some((path, line_number, text)) = parse_rg_match_line(line) {
            total_matches += 1;
            let summary = files.entry(path.to_string()).or_default();
            summary.count += 1;
            if summary.line_numbers.len() < MAX_RG_LINE_NUMBERS_PER_FILE
                && !summary
                    .line_numbers
                    .iter()
                    .any(|existing| existing == line_number)
            {
                summary.line_numbers.push(line_number.to_string());
            }
            if summary.samples.len() < 2 {
                summary
                    .samples
                    .push(truncate_chars(text, MAX_RG_SAMPLE_CHARS));
            }
        } else if is_rg_signal_line(line) && signal_lines.len() < MAX_RG_SIGNAL_LINES {
            signal_lines.push(line.trim().to_string());
        }
    }

    if total_matches == 0 {
        return None;
    }

    let mut sections = vec![format!(
        "rg summary: {total_matches} matches in {} files.",
        files.len()
    )];
    sections.push("Matched files:".to_string());
    for (path, summary) in files.iter().take(MAX_RG_FILES) {
        let line_numbers = if summary.line_numbers.is_empty() {
            "unknown lines".to_string()
        } else {
            format!("lines {}", summary.line_numbers.join(", "))
        };
        sections.push(format!(
            "- {path} ({} matches; {line_numbers})",
            summary.count
        ));
    }
    if files.len() > MAX_RG_FILES {
        sections.push(format!(
            "- ... {} additional matched files omitted",
            files.len() - MAX_RG_FILES
        ));
    }

    let mut sample_count = 0usize;
    sections.push("Representative matches:".to_string());
    'samples: for (path, summary) in &files {
        for (idx, sample) in summary.samples.iter().enumerate() {
            let line_number = summary
                .line_numbers
                .get(idx)
                .or_else(|| summary.line_numbers.first())
                .map_or("?", String::as_str);
            sections.push(format!("{path}:{line_number}:{sample}"));
            sample_count += 1;
            if sample_count >= MAX_RG_SAMPLES {
                break 'samples;
            }
        }
    }

    if !signal_lines.is_empty() {
        sections.push("Retained rg warnings/errors:".to_string());
        sections.extend(signal_lines);
    }
    sections.push(format!(
        "Omitted {} redundant match lines.",
        total_matches.saturating_sub(sample_count)
    ));
    Some(sections.join("\n"))
}

fn parse_rg_match_line(line: &str) -> Option<(&str, &str, &str)> {
    let mut search_from = 0usize;
    while let Some(relative_colon) = line[search_from..].find(':') {
        let path_end = search_from + relative_colon;
        let after_path = &line[path_end + 1..];
        let next_colon = after_path.find(':')?;
        let candidate_line_number = &after_path[..next_colon];
        if !candidate_line_number.is_empty()
            && candidate_line_number
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            let path = &line[..path_end];
            let remainder = &after_path[next_colon + 1..];
            if path.is_empty() || remainder.is_empty() {
                return None;
            }
            if let Some((column, text)) = remainder.split_once(':')
                && !column.is_empty()
                && column.bytes().all(|byte| byte.is_ascii_digit())
                && !text.is_empty()
            {
                return Some((path, candidate_line_number, text.trim_start()));
            }
            return Some((path, candidate_line_number, remainder.trim_start()));
        }
        search_from = path_end + 1;
    }
    None
}

fn is_rg_signal_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("warning")
        || lower.contains("error")
        || lower.contains("permission denied")
        || lower.contains("no files were searched")
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

#[derive(Debug)]
struct SourceRead {
    path: String,
    start_line: i64,
    end_line: i64,
}

fn compact_source_read_dedupe(command: &[String], output: &str) -> Option<String> {
    let source_read = parse_source_read_command(command)?;
    let output_lines = output.lines().count();
    Some(format!(
        "source read dedupe: omitted {output_lines} lines from {} (requested lines {}-{}) because this source range was already shown earlier in the thread.\nRaw output remains available via read_exec_output using this response's chunk_id.",
        source_read.path, source_read.start_line, source_read.end_line
    ))
}

fn parse_source_read_command(command: &[String]) -> Option<SourceRead> {
    command
        .iter()
        .find_map(|part| parse_source_read(part.as_str()))
        .or_else(|| parse_source_read(command.join(" ").as_str()))
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
