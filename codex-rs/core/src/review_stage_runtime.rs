use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_utils_path_uri::PathUri;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ToolFreeReviewStage;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RestrictedReviewStage;

#[derive(Clone, Debug)]
pub(crate) struct ReviewWritableRoot(pub(crate) PathUri);

#[derive(Clone, Debug)]
pub(crate) struct ReviewReadableRoot(pub(crate) PathUri);

#[derive(Clone, Debug)]
pub(crate) struct ReviewProtectedPaths(pub(crate) Vec<PathUri>);

#[derive(Clone, Debug)]
pub(crate) struct ReviewAdditionalReadPaths(pub(crate) Vec<PathUri>);

#[derive(Clone, Debug)]
pub(crate) struct ReviewReadDenyEntries(pub(crate) Vec<FileSystemSandboxEntry<PathUri>>);

#[derive(Clone, Debug)]
pub(crate) struct ReviewVerificationWriteRoot(pub(crate) PathUri);
