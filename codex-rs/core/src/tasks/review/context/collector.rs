use std::future::Future;
use std::io;
use std::time::Duration;

use codex_file_system::ExecutorFileSystem;
use codex_file_system::FileSystemSandboxContext;
use codex_protocol::protocol::ReviewExternalReference;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewReference;
use codex_utils_path_uri::PathUri;
use futures::StreamExt;

use super::CollectedReviewContext;
use super::ContextLimits;
use super::ReviewContextInput;
use super::SourceRange;
use crate::context::MAX_REVIEW_SOURCE_PAYLOAD_BYTES;
use crate::context::SourceFragmentPacker;
use crate::context::bounded_candidates;
use crate::context::bounded_reference_fragments;
use crate::context::escape_review_markup;

const MAX_CONCURRENT_PATH_RESOLUTIONS: usize = 16;

type RenderedRange = (ReviewLineRange, Result<String, &'static str>);
type RenderedFile = Result<Vec<RenderedRange>, &'static str>;

pub(super) async fn collect_review_context_with_limits(
    filesystem: &dyn ExecutorFileSystem,
    checkout_root: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
    input: ReviewContextInput<'_>,
    limits: ContextLimits,
) -> CollectedReviewContext {
    let ReviewContextInput {
        candidates_json,
        candidate_ranges,
        review_ranges,
        external_references,
    } = input;
    let candidates = bounded_candidates(candidates_json);
    let mut references = Vec::new();
    let mut collected_external_references = external_references
        .iter()
        .take(limits.references)
        .cloned()
        .collect::<Vec<_>>();
    let mut omitted_external_references =
        external_references.len().saturating_sub(limits.references);
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
        filesystem.canonicalize(checkout_root, sandbox),
    )
    .await;
    let mut resolved = Vec::new();
    match canonical_root {
        Ok(canonical_root) => {
            let mut request_groups: Vec<Vec<RequestedRange>> = Vec::new();
            for request in requested {
                match request_groups
                    .iter_mut()
                    .find(|group| group[0].source.path == request.source.path)
                {
                    Some(group) => group.push(request),
                    None => request_groups.push(vec![request]),
                }
            }
            let resolutions = futures::stream::iter(request_groups.into_iter().map(|requests| {
                let request = requests[0].clone();
                let canonical_root = &canonical_root;
                async move {
                    let resolution = resolve_range(
                        filesystem,
                        checkout_root,
                        canonical_root,
                        sandbox,
                        request,
                        limits.filesystem_timeout,
                    )
                    .await;
                    (requests, resolution)
                }
            }))
            .buffered(MAX_CONCURRENT_PATH_RESOLUTIONS)
            .collect::<Vec<_>>()
            .await;
            for (requests, resolution) in resolutions {
                match resolution {
                    Ok(resolution) => {
                        resolved.extend(requests.into_iter().map(|request| ResolvedRange {
                            canonical_path: resolution.canonical_path.clone(),
                            display_path: request.source.path,
                            line_range: request.source.line_range,
                            candidate: request.candidate,
                            order: request.order,
                        }));
                    }
                    Err(RangeFailure::Reference(reference)) => {
                        references.extend(
                            requests.into_iter().map(|request| {
                                reference_for(&request.source, &reference.explanation)
                            }),
                        );
                    }
                    Err(RangeFailure::External(reference)) => {
                        collected_external_references.extend(requests.into_iter().map(|request| {
                            ReviewExternalReference {
                                reference: range_reference(&request.source),
                                explanation: reference.explanation.clone(),
                            }
                        }));
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
    let mut file_cache: Vec<(PathUri, RenderedFile)> = Vec::new();
    let mut file_bytes: Vec<(PathUri, usize)> = Vec::new();
    let mut packer = SourceFragmentPacker::default();
    for range in &ranges {
        if packer.is_full(limits.total_source_bytes) {
            references.push(reference_for(
                &range.source(),
                "Range was not loaded because source excerpts reached the total limit.",
            ));
            continue;
        }
        let used_bytes = file_bytes
            .iter()
            .find(|(path, _)| path == &range.canonical_path)
            .map_or(0, |(_, bytes)| *bytes);
        if used_bytes >= limits.file_source_bytes {
            references.push(reference_for(
                &range.source(),
                "Range was not loaded because this file reached the 8K-token source limit.",
            ));
            continue;
        }
        let cache_index = match file_cache
            .iter()
            .position(|(path, _)| path == &range.canonical_path)
        {
            Some(index) => index,
            None => {
                if file_cache.len() >= limits.files {
                    references.push(reference_for(
                        &range.source(),
                        "Range was not loaded because the file-count limit was reached.",
                    ));
                    continue;
                }
                let contents = load_text_file(
                    filesystem,
                    &range.canonical_path,
                    sandbox,
                    limits.file_bytes,
                    limits.filesystem_timeout,
                )
                .await
                .map(|contents| {
                    render_file_ranges(
                        &range.canonical_path,
                        &ranges,
                        &contents,
                        limits.file_source_bytes,
                    )
                });
                file_cache.push((range.canonical_path.clone(), contents));
                file_cache.len() - 1
            }
        };
        let rendered_ranges = match &file_cache[cache_index].1 {
            Ok(rendered_ranges) => rendered_ranges,
            Err(explanation) => {
                references.push(reference_for(&range.source(), explanation));
                continue;
            }
        };
        let rendered = match rendered_ranges
            .iter()
            .find(|(line_range, _)| line_range == &range.line_range)
            .map(|(_, rendered)| rendered)
        {
            Some(Ok(rendered)) => rendered,
            Some(Err(explanation)) => {
                references.push(reference_for(&range.source(), explanation));
                continue;
            }
            None => {
                references.push(reference_for(
                    &range.source(),
                    "Range was not indexed while loading the file.",
                ));
                continue;
            }
        };
        let rendered_bytes = rendered.len();
        if rendered_bytes > limits.file_source_bytes.saturating_sub(used_bytes) {
            references.push(reference_for(
                &range.source(),
                "Range was not loaded because this file reached the 8K-token source limit.",
            ));
            continue;
        }
        if !packer.try_add(rendered, limits.total_source_bytes) {
            references.push(reference_for(
                &range.source(),
                "Range was not loaded because source excerpts reached the total limit.",
            ));
            continue;
        }
        match file_bytes
            .iter_mut()
            .find(|(path, _)| path == &range.canonical_path)
        {
            Some((_, bytes)) => *bytes = bytes.saturating_add(rendered_bytes),
            None => file_bytes.push((range.canonical_path.clone(), rendered_bytes)),
        }
    }

    let source_fragments = packer.finish();
    omitted_external_references = omitted_external_references.saturating_add(
        collected_external_references
            .len()
            .saturating_sub(limits.references),
    );
    collected_external_references.truncate(limits.references);
    if references.len() > limits.references || omitted_external_references > 0 {
        if limits.references > 0 {
            let retained_references = limits.references - 1;
            let omitted_references = references.len().saturating_sub(retained_references);
            references.truncate(retained_references);
            let source_suffix = if omitted_references == 1 { "" } else { "s" };
            let external_suffix = if omitted_external_references == 1 {
                ""
            } else {
                "s"
            };
            let explanation = format!(
                "{omitted_references} source reference{source_suffix} and {omitted_external_references} external reference{external_suffix} were omitted at the reference-count limit."
            );
            references.insert(
                0,
                ReviewReference {
                    reference: "review context limits".to_string(),
                    explanation,
                },
            );
        } else {
            references.clear();
        }
    }
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
    let raw_limit = limits.requested_ranges.saturating_mul(2);
    let mut omitted = total.saturating_sub(raw_limit);
    let mut requested = Vec::new();
    for (order, (source, candidate)) in candidate_ranges
        .iter()
        .map(|source| (source, true))
        .chain(review_ranges.iter().map(|source| (source, false)))
        .take(raw_limit)
        .enumerate()
    {
        let line_count = source
            .line_range
            .end
            .checked_sub(source.line_range.start)
            .and_then(|difference| difference.checked_add(/*rhs*/ 1));
        if source.line_range.start == 0
            || line_count.is_none_or(|line_count| line_count > limits.range_lines)
        {
            references.push(reference_for(
                source,
                "Range was not loaded because it is invalid or exceeds 400 lines.",
            ));
        } else if requested.len() < limits.requested_ranges {
            requested.push(RequestedRange {
                source: source.clone(),
                candidate,
                order,
            });
        } else {
            omitted = omitted.saturating_add(1);
        }
    }
    if omitted > 0 {
        references.push(ReviewReference {
            reference: "review context ranges".to_string(),
            explanation: format!(
                "{omitted} additional ranges were omitted at the request-count limit."
            ),
        });
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
    sandbox: Option<&FileSystemSandboxContext>,
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
        filesystem.canonicalize(&requested_path, sandbox),
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
    for (_, ranges) in files {
        let merge_overlaps = |mut ranges: Vec<ResolvedRange>| {
            ranges.sort_by_key(|range| (range.line_range.start, range.line_range.end));
            let mut merged_ranges: Vec<ResolvedRange> = Vec::new();
            for range in ranges {
                if let Some(previous) = merged_ranges.last_mut()
                    && range.line_range.start <= previous.line_range.end
                {
                    previous.line_range.end = previous.line_range.end.max(range.line_range.end);
                    previous.order = previous.order.min(range.order);
                    continue;
                }
                merged_ranges.push(range);
            }
            merged_ranges
        };
        let subtract_covered = |range: ResolvedRange, covered: &[ResolvedRange]| {
            let mut uncovered = vec![range];
            for covered in covered {
                let mut next = Vec::new();
                for segment in uncovered {
                    if covered.line_range.end < segment.line_range.start
                        || covered.line_range.start > segment.line_range.end
                    {
                        next.push(segment);
                        continue;
                    }
                    if segment.line_range.start < covered.line_range.start {
                        let mut prefix = segment.clone();
                        prefix.line_range.end = covered.line_range.start - 1;
                        next.push(prefix);
                    }
                    if covered.line_range.end < segment.line_range.end {
                        let mut suffix = segment;
                        suffix.line_range.start = covered.line_range.end + 1;
                        next.push(suffix);
                    }
                }
                uncovered = next;
            }
            uncovered
        };
        let mut candidates = ranges
            .iter()
            .filter(|range| range.candidate)
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|range| range.order);
        let mut candidate_ranges = Vec::new();
        for range in candidates {
            candidate_ranges.extend(subtract_covered(range, &candidate_ranges));
        }
        let review_ranges = merge_overlaps(
            ranges
                .into_iter()
                .filter(|range| !range.candidate)
                .collect(),
        );
        let mut file_ranges = candidate_ranges.clone();
        for range in review_ranges {
            file_ranges.extend(subtract_covered(range, &candidate_ranges));
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
    sandbox: Option<&FileSystemSandboxContext>,
    max_bytes: usize,
    filesystem_timeout: Duration,
) -> Result<String, &'static str> {
    let result = fs_call(filesystem_timeout, async {
        let metadata = filesystem.get_metadata(path, sandbox).await?;
        if !metadata.is_file {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
        }
        if metadata.size > max_bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "file too large",
            ));
        }
        let bytes = filesystem.read_file(path, sandbox).await?;
        if bytes.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "file too large",
            ));
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
    String::from_utf8(bytes).map_err(|_| "File was not loaded because it is not valid UTF-8.")
}

fn render_file_ranges(
    canonical_path: &PathUri,
    ranges: &[ResolvedRange],
    contents: &str,
    max_rendered_bytes: usize,
) -> Vec<RenderedRange> {
    let mut ranges = ranges
        .iter()
        .filter(|range| &range.canonical_path == canonical_path)
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| (range.line_range.start, range.line_range.end));
    let mut rendered = ranges.iter().map(|_| Ok(String::new())).collect::<Vec<_>>();
    let mut range_index = 0usize;
    let mut line_count = 0usize;
    for (line_index, line) in contents.lines().enumerate() {
        let line_number = line_index + 1;
        line_count = line_number;
        while range_index < ranges.len()
            && usize::try_from(ranges[range_index].line_range.end).unwrap_or(usize::MAX)
                < line_number
        {
            range_index += 1;
        }
        let Some(range) = ranges.get(range_index) else {
            continue;
        };
        let start = usize::try_from(range.line_range.start).unwrap_or(usize::MAX);
        if line_number < start {
            continue;
        }
        let display_path = escape_review_markup(&range.display_path.replace(['\r', '\n'], "�"));
        let line = escape_review_markup(line);
        let rendered_line = format!("{display_path}:{line_number}: {line}\n");
        if rendered_line.len() > MAX_REVIEW_SOURCE_PAYLOAD_BYTES {
            rendered[range_index] =
                Err("Range contains a source line that exceeds the fragment limit.");
        } else {
            let exceeds_file_limit = rendered[range_index].as_ref().is_ok_and(|rendered| {
                rendered_line.len() > max_rendered_bytes.saturating_sub(rendered.len())
            });
            if exceeds_file_limit {
                rendered[range_index] = Err("Range contains more source than the per-file limit.");
            } else if let Ok(rendered) = &mut rendered[range_index] {
                rendered.push_str(&rendered_line);
            }
        }
    }
    for (range, rendered) in ranges.iter().zip(&mut rendered) {
        let start = usize::try_from(range.line_range.start).unwrap_or(usize::MAX);
        let end = usize::try_from(range.line_range.end).unwrap_or(usize::MAX);
        if start == 0 || start > line_count {
            *rendered = Err("Range starts beyond the end of the file.");
        } else if end > line_count {
            *rendered = Err("Range ends beyond the end of the file.");
        }
    }
    ranges
        .into_iter()
        .zip(rendered)
        .map(|(range, rendered)| (range.line_range.clone(), rendered))
        .collect()
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
