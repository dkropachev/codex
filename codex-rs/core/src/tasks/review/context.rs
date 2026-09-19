use std::time::Duration;

use codex_file_system::ExecutorFileSystem;
use codex_file_system::FileSystemSandboxContext;
use codex_protocol::protocol::ReviewExternalReference;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewReference;
use codex_utils_path_uri::PathUri;

use crate::context::ContextualUserFragment;
use crate::context::ReviewCandidatesFragment;
use crate::context::ReviewReferencesFragment;
use crate::context::ReviewSourceFragment;
use crate::context::bounded_candidates;
use crate::context::bounded_reference_fragments;

mod collector;

const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RANGE_LINES: u32 = 400;
// One UTF-8 byte per token is the conservative bound for untrusted text.
const MAX_FILE_SOURCE_BYTES: usize = 8 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 64 * 1024;
const MAX_REQUESTED_RANGES: usize = 256;
const MAX_FILES: usize = 64;
const MAX_REFERENCES: usize = 128;
const FILESYSTEM_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);
const TOTAL_SCAN_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 15);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceRange {
    pub(crate) path: String,
    pub(crate) line_range: ReviewLineRange,
}

#[derive(Debug)]
pub(crate) struct CollectedReviewContext {
    pub(crate) candidates: ReviewCandidatesFragment,
    pub(crate) source_fragments: Vec<ReviewSourceFragment>,
    pub(crate) reference_fragments: Vec<ReviewReferencesFragment>,
    pub(crate) references: Vec<ReviewReference>,
    pub(crate) external_references: Vec<ReviewExternalReference>,
}

impl CollectedReviewContext {
    pub(crate) fn into_fragments(self) -> Vec<Box<dyn ContextualUserFragment>> {
        let mut fragments: Vec<Box<dyn ContextualUserFragment>> =
            Vec::with_capacity(/*capacity*/ {
                1 + self.source_fragments.len() + self.reference_fragments.len()
            });
        fragments.push(Box::new(self.candidates));
        fragments.extend(
            self.source_fragments
                .into_iter()
                .map(|fragment| Box::new(fragment) as Box<dyn ContextualUserFragment>),
        );
        fragments.extend(
            self.reference_fragments
                .into_iter()
                .map(|fragment| Box::new(fragment) as Box<dyn ContextualUserFragment>),
        );
        fragments
    }
}

#[derive(Clone, Copy)]
struct ContextLimits {
    file_bytes: usize,
    range_lines: u32,
    file_source_bytes: usize,
    total_source_bytes: usize,
    requested_ranges: usize,
    files: usize,
    references: usize,
    filesystem_timeout: Duration,
    total_scan_timeout: Duration,
}

impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            file_bytes: MAX_FILE_BYTES,
            range_lines: MAX_RANGE_LINES,
            file_source_bytes: MAX_FILE_SOURCE_BYTES,
            total_source_bytes: MAX_TOTAL_SOURCE_BYTES,
            requested_ranges: MAX_REQUESTED_RANGES,
            files: MAX_FILES,
            references: MAX_REFERENCES,
            filesystem_timeout: FILESYSTEM_TIMEOUT,
            total_scan_timeout: TOTAL_SCAN_TIMEOUT,
        }
    }
}

#[cfg(test)]
pub(crate) async fn collect_review_context(
    filesystem: &dyn ExecutorFileSystem,
    checkout_root: &PathUri,
    candidates_json: &str,
    candidate_ranges: &[SourceRange],
    review_ranges: &[SourceRange],
    external_references: &[ReviewExternalReference],
) -> CollectedReviewContext {
    let limits = ContextLimits::default();
    collect_review_context_with_limits_and_timeout(
        filesystem,
        checkout_root,
        /*sandbox*/ None,
        candidates_json,
        candidate_ranges,
        review_ranges,
        external_references,
        limits,
    )
    .await
}

pub(crate) async fn collect_review_context_with_sandbox(
    filesystem: &dyn ExecutorFileSystem,
    checkout_root: &PathUri,
    sandbox: &FileSystemSandboxContext,
    candidates_json: &str,
    candidate_ranges: &[SourceRange],
    review_ranges: &[SourceRange],
    external_references: &[ReviewExternalReference],
) -> CollectedReviewContext {
    collect_review_context_with_limits_and_timeout(
        filesystem,
        checkout_root,
        Some(sandbox),
        candidates_json,
        candidate_ranges,
        review_ranges,
        external_references,
        ContextLimits::default(),
    )
    .await
}

async fn collect_review_context_with_limits_and_timeout(
    filesystem: &dyn ExecutorFileSystem,
    checkout_root: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
    candidates_json: &str,
    candidate_ranges: &[SourceRange],
    review_ranges: &[SourceRange],
    external_references: &[ReviewExternalReference],
    limits: ContextLimits,
) -> CollectedReviewContext {
    let collection = collector::collect_review_context_with_limits(
        filesystem,
        checkout_root,
        sandbox,
        candidates_json,
        candidate_ranges,
        review_ranges,
        external_references,
        limits,
    );
    match tokio::time::timeout(limits.total_scan_timeout, collection).await {
        Ok(context) => context,
        Err(_) => {
            let references = vec![ReviewReference {
                reference: "review context".to_string(),
                explanation: "Source collection exceeded the overall timeout.".to_string(),
            }];
            let external_references = external_references
                .iter()
                .take(limits.references)
                .cloned()
                .collect::<Vec<_>>();
            CollectedReviewContext {
                candidates: bounded_candidates(candidates_json),
                source_fragments: Vec::new(),
                reference_fragments: bounded_reference_fragments(&references, &external_references),
                references,
                external_references,
            }
        }
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
