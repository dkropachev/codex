use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use codex_file_system::ExecutorFileSystem;
use codex_protocol::protocol::ReviewExternalReference;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewReference;
use codex_utils_path_uri::PathUri;
use futures::StreamExt;

use super::CollectedReviewContext;
use super::ContextLimits;
use super::SourceRange;
use crate::context::SourceFragmentPacker;
use crate::context::bounded_candidates;
use crate::context::bounded_reference_fragments;
use crate::context::escape_review_markup;

pub(super) async fn collect_review_context_with_limits(
    filesystem: &dyn ExecutorFileSystem,
    checkout_root: &PathUri,
    candidates_json: &str,
    candidate_ranges: &[SourceRange],
    review_ranges: &[SourceRange],
    external_references: &[ReviewExternalReference],
    limits: ContextLimits,
) -> CollectedReviewContext {
    let candidates = bounded_candidates(candidates_json);
    let mut references = Vec::new();
    let mut collected_external_references = external_references
        .iter()
        .take(limits.references)
        .cloned()
        .collect::<Vec<_>>();
    if candidates.was_truncated() {
        references.push(ReviewReference {
            reference: "review candidates".to_string(),
            explanation: "Candidate input was truncated to the 8K-token verifier limit."
                .to_string(),
        });
    }

    let requested = requested_ranges(candidate_ranges, review_ranges, limits, &mut references);
    let canonical_root = fs_call(
        limits.filesystem_timeout,
        filesystem.canonicalize(checkout_root, /*sandbox*/ None),
    )
    .await;
    let mut resolved = Vec::new();
    match canonical_root {
        Ok(canonical_root) => {
            for request in requested {
                match resolve_range(
                    filesystem,
                    checkout_root,
                    &canonical_root,
                    request,
                    limits.filesystem_timeout,
                )
                .await
                {
                    Ok(request) => resolved.push(request),
                    Err(RangeFailure::Reference(reference)) => references.push(reference),
                    Err(RangeFailure::External(reference)) => {
                        collected_external_references.push(reference)
                    }
                }
            }
        }
        Err(_) => references.extend(requested.into_iter().map(|request| {
            reference_for(
                &request.source,
                "Checkout root could not be resolved before the filesystem timeout.",
            )
        })),
    }

    let ranges = merge_resolved_ranges(resolved, limits.range_lines);
    let mut file_cache: Vec<(PathUri, Result<Arc<String>, &'static str>)> = Vec::new();
    let mut file_bytes: Vec<(PathUri, usize)> = Vec::new();
    let mut packer = SourceFragmentPacker::default();
    for range in ranges {
        if packer.is_full(limits.total_source_bytes) {
            push_reference(
                &mut references,
                limits.references,
                reference_for(
                    &range.source(),
                    "Range was not loaded because source excerpts reached the total limit.",
                ),
            );
            continue;
        }
        let cache_index = match file_cache
            .iter()
            .position(|(path, _)| path == &range.canonical_path)
        {
            Some(index) => index,
            None => {
                if file_cache.len() >= limits.files {
                    push_reference(
                        &mut references,
                        limits.references,
                        reference_for(
                            &range.source(),
                            "Range was not loaded because the file-count limit was reached.",
                        ),
                    );
                    continue;
                }
                let contents = load_text_file(
                    filesystem,
                    &range.canonical_path,
                    limits.file_bytes,
                    limits.filesystem_timeout,
                )
                .await;
                file_cache.push((range.canonical_path.clone(), contents));
                file_cache.len() - 1
            }
        };
        let contents = match &file_cache[cache_index].1 {
            Ok(contents) => Arc::clone(contents),
            Err(explanation) => {
                push_reference(
                    &mut references,
                    limits.references,
                    reference_for(&range.source(), explanation),
                );
                continue;
            }
        };
        let rendered = match render_range(&range, &contents) {
            Ok(rendered) => rendered,
            Err(explanation) => {
                push_reference(
                    &mut references,
                    limits.references,
                    reference_for(&range.source(), explanation),
                );
                continue;
            }
        };
        let rendered_bytes = rendered.len();
        let used_bytes = file_bytes
            .iter()
            .find(|(path, _)| path == &range.canonical_path)
            .map_or(0, |(_, bytes)| *bytes);
        if rendered_bytes > limits.file_source_bytes.saturating_sub(used_bytes) {
            push_reference(
                &mut references,
                limits.references,
                reference_for(
                    &range.source(),
                    "Range was not loaded because this file reached the 8K-token source limit.",
                ),
            );
            continue;
        }
        if !packer.try_add(&rendered, limits.total_source_bytes) {
            push_reference(
                &mut references,
                limits.references,
                reference_for(
                    &range.source(),
                    "Range was not loaded because source excerpts reached the total limit.",
                ),
            );
            continue;
        }
        match file_bytes
            .iter_mut()
            .find(|(path, _)| path == &range.canonical_path)
        {
            Some((_, bytes)) => *bytes = bytes.saturating_add(rendered_bytes),
            None => file_bytes.push((range.canonical_path, rendered_bytes)),
        }
    }

    let source_fragments = packer.finish();
    references.truncate(limits.references);
    collected_external_references.truncate(limits.references);
    let reference_fragments =
        bounded_reference_fragments(&references, &collected_external_references);
    CollectedReviewContext {
        candidates,
        source_fragments,
        reference_fragments,
        references,
        external_references: collected_external_references,
    }
}

#[derive(Clone)]
struct RequestedRange {
    source: SourceRange,
    candidate: bool,
    order: usize,
}

fn requested_ranges(
    candidate_ranges: &[SourceRange],
    review_ranges: &[SourceRange],
    limits: ContextLimits,
    references: &mut Vec<ReviewReference>,
) -> Vec<RequestedRange> {
    let total = candidate_ranges.len().saturating_add(review_ranges.len());
    let requested = candidate_ranges
        .iter()
        .map(|source| (source, true))
        .chain(review_ranges.iter().map(|source| (source, false)))
        .take(limits.requested_ranges)
        .enumerate()
        .filter_map(|(order, (source, candidate))| {
            let line_count = source
                .line_range
                .end
                .checked_sub(source.line_range.start)
                .and_then(|difference| difference.checked_add(/*rhs*/ 1));
            if source.line_range.start == 0
                || line_count.is_none_or(|line_count| line_count > limits.range_lines)
            {
                push_reference(
                    references,
                    limits.references,
                    reference_for(
                        source,
                        "Range was not loaded because it is invalid or exceeds 400 lines.",
                    ),
                );
                None
            } else {
                Some(RequestedRange {
                    source: source.clone(),
                    candidate,
                    order,
                })
            }
        })
        .collect::<Vec<_>>();
    if total > limits.requested_ranges {
        push_reference(
            references,
            limits.references,
            ReviewReference {
                reference: "review context ranges".to_string(),
                explanation: format!(
                    "{} additional ranges were omitted at the request-count limit.",
                    total - limits.requested_ranges
                ),
            },
        );
    }
    requested
}

#[derive(Clone)]
struct ResolvedRange {
    canonical_path: PathUri,
    display_path: String,
    line_range: ReviewLineRange,
    candidate: bool,
    order: usize,
}

impl ResolvedRange {
    fn source(&self) -> SourceRange {
        SourceRange {
            path: self.display_path.clone(),
            line_range: self.line_range.clone(),
        }
    }
}

async fn resolve_range(
    filesystem: &dyn ExecutorFileSystem,
    checkout_root: &PathUri,
    canonical_root: &PathUri,
    request: RequestedRange,
    filesystem_timeout: Duration,
) -> Result<ResolvedRange, RangeFailure> {
    let requested_path = PathUri::parse(&request.source.path)
        .or_else(|_| checkout_root.join(&request.source.path))
        .map_err(|_| {
            RangeFailure::Reference(reference_for(
                &request.source,
                "Path is not a valid checkout path.",
            ))
        })?;
    let canonical_path = fs_call(
        filesystem_timeout,
        filesystem.canonicalize(&requested_path, /*sandbox*/ None),
    )
    .await
    .map_err(|failure| {
        RangeFailure::Reference(reference_for(
            &request.source,
            match failure {
                FileSystemFailure::TimedOut => "Path resolution exceeded the filesystem timeout.",
                FileSystemFailure::Io => "Path could not be resolved in the checkout.",
            },
        ))
    })?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(RangeFailure::External(ReviewExternalReference {
            reference: range_reference(&request.source),
            explanation: "Path or symlink target is outside the checkout and was not read."
                .to_string(),
        }));
    }
    Ok(ResolvedRange {
        canonical_path,
        display_path: request.source.path,
        line_range: request.source.line_range,
        candidate: request.candidate,
        order: request.order,
    })
}

fn merge_resolved_ranges(ranges: Vec<ResolvedRange>, max_lines: u32) -> Vec<ResolvedRange> {
    let mut files: Vec<(PathUri, Vec<ResolvedRange>)> = Vec::new();
    for range in ranges {
        match files
            .iter_mut()
            .find(|(path, _)| path == &range.canonical_path)
        {
            Some((_, ranges)) => ranges.push(range),
            None => files.push((range.canonical_path.clone(), vec![range])),
        }
    }
    let mut merged = Vec::new();
    for (_, mut ranges) in files {
        ranges.sort_by_key(|range| (range.line_range.start, range.line_range.end));
        let mut file_ranges: Vec<ResolvedRange> = Vec::new();
        for range in ranges {
            if let Some(previous) = file_ranges.last_mut()
                && range.line_range.start <= previous.line_range.end
            {
                previous.line_range.end = previous.line_range.end.max(range.line_range.end);
                previous.candidate |= range.candidate;
                previous.order = previous.order.min(range.order);
                continue;
            }
            file_ranges.push(range);
        }
        for range in file_ranges {
            let mut start = range.line_range.start;
            while start <= range.line_range.end {
                let end = start
                    .saturating_add(max_lines.saturating_sub(/*rhs*/ 1))
                    .min(range.line_range.end);
                merged.push(ResolvedRange {
                    line_range: ReviewLineRange { start, end },
                    ..range.clone()
                });
                let Some(next) = end.checked_add(/*rhs*/ 1) else {
                    break;
                };
                start = next;
            }
        }
    }
    merged.sort_by_key(|range| (!range.candidate, range.order, range.line_range.start));
    merged
}

async fn load_text_file(
    filesystem: &dyn ExecutorFileSystem,
    path: &PathUri,
    max_bytes: usize,
    filesystem_timeout: Duration,
) -> Result<Arc<String>, &'static str> {
    let result = fs_call(filesystem_timeout, async {
        let metadata = filesystem.get_metadata(path, /*sandbox*/ None).await?;
        if !metadata.is_file {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
        }
        if metadata.size > max_bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "file too large",
            ));
        }
        let mut stream = filesystem.read_file_stream(path, /*sandbox*/ None).await?;
        let mut bytes = Vec::with_capacity(/*capacity*/ metadata.size as usize);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "file too large",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    })
    .await;
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(FileSystemFailure::TimedOut) => {
            return Err("File read exceeded the filesystem timeout.");
        }
        Err(FileSystemFailure::Io) => return Err("Path is not a readable regular file."),
    };
    if bytes.contains(&0) {
        return Err("File was not loaded because it appears to be binary.");
    }
    String::from_utf8(bytes)
        .map(Arc::new)
        .map_err(|_| "File was not loaded because it is not valid UTF-8.")
}

fn render_range(range: &ResolvedRange, contents: &str) -> Result<String, &'static str> {
    let lines = contents.lines().collect::<Vec<_>>();
    let start = usize::try_from(range.line_range.start).unwrap_or(usize::MAX);
    if start == 0 || start > lines.len() {
        return Err("Range starts beyond the end of the file.");
    }
    let end = usize::try_from(range.line_range.end)
        .unwrap_or(usize::MAX)
        .min(lines.len());
    let display_path = escape_review_markup(&range.display_path.replace(['\r', '\n'], "�"));
    let mut rendered = String::new();
    for (offset, line) in lines[start - 1..end].iter().enumerate() {
        let line_number = start + offset;
        let line = escape_review_markup(line);
        let rendered_line = format!("{display_path}:{line_number}: {line}\n");
        if rendered_line.len() > crate::context::MAX_REVIEW_FRAGMENT_BYTES / 2 {
            return Err("Range contains a source line that exceeds the fragment limit.");
        }
        rendered.push_str(&rendered_line);
    }
    Ok(rendered)
}

fn reference_for(source: &SourceRange, explanation: &str) -> ReviewReference {
    ReviewReference {
        reference: range_reference(source),
        explanation: explanation.to_string(),
    }
}

fn range_reference(source: &SourceRange) -> String {
    let path = &source.path;
    let start = source.line_range.start;
    let end = source.line_range.end;
    format!("{path}:{start}-{end}")
}

fn push_reference(references: &mut Vec<ReviewReference>, limit: usize, reference: ReviewReference) {
    if references.len() < limit {
        references.push(reference);
    }
}

enum RangeFailure {
    Reference(ReviewReference),
    External(ReviewExternalReference),
}

#[derive(Clone, Copy)]
enum FileSystemFailure {
    TimedOut,
    Io,
}

async fn fs_call<T>(
    timeout: Duration,
    operation: impl Future<Output = io::Result<T>>,
) -> Result<T, FileSystemFailure> {
    tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| FileSystemFailure::TimedOut)?
        .map_err(|_| FileSystemFailure::Io)
}
