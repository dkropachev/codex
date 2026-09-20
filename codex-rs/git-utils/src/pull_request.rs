use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;
use url::Url;

use crate::review_branch::resolve_pr_base_ref_with_runner;

pub(crate) const GH_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const REVIEW_COMMAND_OUTPUT_BYTES_CAP: usize = 512 * 1024;
const GH_REPO_ENV_VAR: &str = "GH_REPO";

/// A bounded, non-interactive command used to resolve a code-review scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCommand {
    argv: Vec<String>,
    cwd: PathUri,
    env: HashMap<String, String>,
    timeout: Duration,
    output_bytes_cap: usize,
}

impl ReviewCommand {
    pub(crate) fn new(argv: impl IntoIterator<Item = impl Into<String>>, cwd: PathUri) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd,
            env: HashMap::new(),
            timeout: GIT_COMMAND_TIMEOUT,
            output_bytes_cap: REVIEW_COMMAND_OUTPUT_BYTES_CAP,
        }
    }

    pub(crate) fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn cwd(&self) -> &PathUri {
        &self.cwd
    }

    pub fn env_vars(&self) -> &HashMap<String, String> {
        &self.env
    }

    pub fn command_timeout(&self) -> Duration {
        self.timeout
    }

    pub fn output_bytes_cap(&self) -> usize {
        self.output_bytes_cap
    }
}

/// Captured output from a review-scope command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ReviewCommandOutput {
    pub(crate) fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Executes review-scope probes beside the checkout being reviewed.
///
/// Implementations must honor the request's timeout and output cap, run argv
/// without shell interpolation, apply the supplied environment overrides, and
/// prevent `GH_REPO` from overriding repository discovery.
pub trait ReviewCommandRunner: Send + Sync {
    fn run(
        &self,
        command: ReviewCommand,
    ) -> impl Future<Output = Result<ReviewCommandOutput>> + Send;
}

struct NativeReviewCommandRunner;

impl ReviewCommandRunner for NativeReviewCommandRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        run_native_review_command(command).await
    }
}

/// Pull request metadata used to establish review intent and scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestMetadata {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: String,
    pub base_ref_name: String,
    pub base_ref_oid: String,
    pub head_ref_oid: String,
}

/// Pull request metadata paired with the exact merge base for the local review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPullRequestReview {
    pub metadata: PullRequestMetadata,
    pub merge_base: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequestView {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    url: String,
    state: String,
    base_ref_name: String,
    base_ref_oid: String,
    head_ref_oid: String,
}

/// Resolves a pull request and the merge base that should be reviewed locally.
///
/// GitHub metadata is loaded outside the reviewer model. The PR's base object ID
/// is preferred so a stale local base branch cannot silently change review scope;
/// the named base ref is used only when that object ID is unavailable locally.
pub async fn resolve_pull_request_for_review(
    cwd: &Path,
    url: &str,
) -> Result<ResolvedPullRequestReview> {
    let cwd = PathUri::from_host_native_path(cwd).with_context(|| {
        format!(
            "review working directory is not absolute: {}",
            cwd.display()
        )
    })?;
    resolve_pull_request_for_review_with_runner(&NativeReviewCommandRunner, &cwd, url).await
}

/// Resolves pull request metadata and its exact comparison base using `runner`.
pub async fn resolve_pull_request_for_review_with_runner(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    url: &str,
) -> Result<ResolvedPullRequestReview> {
    let url = url.trim();
    let selected_url = canonical_pull_request_url(url)?;

    let fields = "number,title,body,url,state,baseRefName,baseRefOid,headRefOid";
    let command = ReviewCommand::new(["gh", "pr", "view", url, "--json", fields], cwd.clone())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .timeout(GH_COMMAND_TIMEOUT);
    let output = runner
        .run(command)
        .await
        .with_context(|| format!("failed to run `gh pr view` for {url}"))?;
    if !output.success() {
        bail!(
            "failed to resolve pull request {url}: `gh pr view` exited with {}: {}",
            output.exit_code,
            output.stderr.trim()
        );
    }

    let metadata = parse_pull_request_metadata(output.stdout.as_bytes())
        .with_context(|| format!("failed to parse metadata for pull request {url}"))?;
    let resolved_url = canonical_pull_request_url(&metadata.url)
        .context("GitHub returned an invalid pull request URL")?;
    if resolved_url != selected_url {
        bail!(
            "GitHub resolved {url:?} as a different pull request URL {:?}",
            metadata.url
        );
    }
    ensure_pull_request_is_open(&metadata)?;
    let merge_base = resolve_pull_request_merge_base(runner, cwd, &metadata).await?;
    Ok(ResolvedPullRequestReview {
        metadata,
        merge_base,
    })
}

fn canonical_pull_request_url(url: &str) -> Result<String> {
    let parsed = Url::parse(url).context("pull request target must be an absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        bail!("pull request target must be an HTTP(S) URL");
    }
    let segments = parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if segments.len() != 4 || segments[2] != "pull" {
        bail!("pull request target must use an /owner/repository/pull/number URL");
    }
    let owner = segments[0];
    let repository = segments[1];
    let number = segments[3];
    if owner.is_empty() || repository.is_empty() || number.parse::<u64>().is_err() {
        bail!("pull request target must use an /owner/repository/pull/number URL");
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let authority = parsed
        .port_or_known_default()
        .map_or_else(|| host.clone(), |port| format!("{host}:{port}"));
    let (owner, repository) = if host == "github.com" {
        (owner.to_ascii_lowercase(), repository.to_ascii_lowercase())
    } else {
        (owner.to_string(), repository.to_string())
    };
    Ok(format!("{authority}/{owner}/{repository}/pull/{number}"))
}

/// Resolves the merge base between `HEAD` and a branch beside the runner's checkout.
pub async fn merge_base_with_head_with_runner(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    branch: &str,
) -> Result<Option<String>> {
    let repository = run_git(runner, cwd, ["rev-parse", "--is-inside-work-tree"]).await?;
    if !repository.success() || repository.stdout.trim() != "true" {
        bail!(
            "review working directory {cwd} is not a Git repository: {}",
            repository.stderr.trim()
        );
    }
    let head = run_git(runner, cwd, ["rev-parse", "--verify", "HEAD"]).await?;
    if !head.success() || head.stdout.trim().is_empty() {
        return Ok(None);
    }
    let branch_revision = run_git(
        runner,
        cwd,
        ["rev-parse", "--verify", "--end-of-options", branch],
    )
    .await?;
    if !branch_revision.success() || branch_revision.stdout.trim().is_empty() {
        return Ok(None);
    }

    let mut preferred_revision = branch_revision.stdout.trim().to_string();
    let local_branch = branch
        .strip_prefix("refs/heads/")
        .or_else(|| (!branch.starts_with("refs/")).then_some(branch));
    if let Some(local_branch) = local_branch {
        let upstream_spec = format!("{local_branch}@{{upstream}}");
        let upstream = run_git(
            runner,
            cwd,
            [
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "--end-of-options",
                &upstream_spec,
            ],
        )
        .await?;
        if upstream.success() {
            let upstream = upstream.stdout.trim();
            if !upstream.is_empty() {
                let upstream_revision = run_git(
                    runner,
                    cwd,
                    ["rev-parse", "--verify", "--end-of-options", upstream],
                )
                .await?;
                if upstream_revision.success() && !upstream_revision.stdout.trim().is_empty() {
                    let range = format!(
                        "{}...{}",
                        branch_revision.stdout.trim(),
                        upstream_revision.stdout.trim()
                    );
                    let counts =
                        run_git(runner, cwd, ["rev-list", "--left-right", "--count", &range])
                            .await?;
                    let remote_is_ahead = counts.success()
                        && counts
                            .stdout
                            .split_whitespace()
                            .nth(1)
                            .and_then(|count| count.parse::<u64>().ok())
                            .is_some_and(|count| count > 0);
                    if remote_is_ahead {
                        preferred_revision = upstream_revision.stdout.trim().to_string();
                    }
                }
            }
        }
    }

    let merge_base = run_git(
        runner,
        cwd,
        ["merge-base", head.stdout.trim(), &preferred_revision],
    )
    .await?;
    if !merge_base.success() {
        bail!(
            "`git merge-base` exited with {}: {}",
            merge_base.exit_code,
            merge_base.stderr.trim()
        );
    }
    let merge_base = merge_base.stdout.trim();
    if merge_base.is_empty() {
        bail!("`git merge-base` returned no commit");
    }
    Ok(Some(merge_base.to_string()))
}

fn ensure_pull_request_is_open(metadata: &PullRequestMetadata) -> Result<()> {
    if metadata.state.eq_ignore_ascii_case("open") {
        return Ok(());
    }
    bail!(
        "pull request {} is no longer open (state: {})",
        metadata.url,
        metadata.state
    )
}

async fn resolve_pull_request_merge_base(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    metadata: &PullRequestMetadata,
) -> Result<String> {
    let mut failures = Vec::new();
    if !metadata.base_ref_oid.trim().is_empty() {
        match merge_base_with_revision(runner, cwd, &metadata.base_ref_oid).await {
            Ok(RevisionMergeBase::Resolved(merge_base)) => return Ok(merge_base),
            Ok(RevisionMergeBase::Unavailable) => {
                failures.push(format!(
                    "base object ID {:?} was not found locally",
                    metadata.base_ref_oid
                ));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to resolve base object ID {:?} for pull request {}",
                        metadata.base_ref_oid, metadata.url
                    )
                });
            }
        }
    }

    if !metadata.base_ref_name.trim().is_empty() {
        let base_ref =
            resolve_pr_base_ref_with_runner(runner, cwd, &metadata.base_ref_name, &metadata.url)
                .await
                .with_context(|| {
                    format!(
                        "failed to resolve base ref {:?} for pull request {}",
                        metadata.base_ref_name, metadata.url
                    )
                })?;
        if let Some(base_ref) = base_ref {
            match merge_base_with_revision(runner, cwd, &base_ref).await {
                Ok(RevisionMergeBase::Resolved(merge_base)) => return Ok(merge_base),
                Ok(RevisionMergeBase::Unavailable) => {
                    failures.push(format!("base ref {base_ref:?} was not found locally"));
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to resolve base ref {base_ref:?} for pull request {}",
                            metadata.url
                        )
                    });
                }
            }
        } else {
            failures.push(format!(
                "base ref {:?} was not found locally",
                metadata.base_ref_name
            ));
        }
    }

    let details = if failures.is_empty() {
        "the pull request did not provide a base object ID or base ref".to_string()
    } else {
        failures.join("; ")
    };
    bail!(
        "failed to resolve the base for pull request {}: {details}",
        metadata.url
    )
}

fn parse_pull_request_metadata(bytes: &[u8]) -> Result<PullRequestMetadata> {
    let view: GhPullRequestView = serde_json::from_slice(bytes)?;
    if view.url.trim().is_empty() {
        bail!("GitHub returned an empty pull request URL");
    }
    if view.base_ref_name.trim().is_empty() && view.base_ref_oid.trim().is_empty() {
        bail!("GitHub returned no pull request base");
    }
    if view.head_ref_oid.trim().is_empty() {
        bail!("GitHub returned an empty pull request head object ID");
    }
    Ok(PullRequestMetadata {
        number: view.number,
        title: view.title,
        body: view.body,
        url: view.url,
        state: view.state,
        base_ref_name: view.base_ref_name,
        base_ref_oid: view.base_ref_oid,
        head_ref_oid: view.head_ref_oid,
    })
}

enum RevisionMergeBase {
    Unavailable,
    Resolved(String),
}

async fn merge_base_with_revision(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    revision: &str,
) -> Result<RevisionMergeBase> {
    let Some(revision_oid) = resolve_revision_oid(runner, cwd, revision).await? else {
        return Ok(RevisionMergeBase::Unavailable);
    };

    let merge_base = run_git(runner, cwd, ["merge-base", "HEAD", &revision_oid]).await?;
    if !merge_base.success() {
        bail!(
            "`git merge-base HEAD {revision_oid}` exited with {}: {}",
            merge_base.exit_code,
            merge_base.stderr.trim()
        );
    }
    let merge_base = merge_base.stdout.trim();
    if merge_base.is_empty() {
        bail!("`git merge-base HEAD {revision_oid}` returned no commit");
    }
    Ok(RevisionMergeBase::Resolved(merge_base.to_string()))
}

pub(crate) async fn resolve_revision_oid(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    revision: &str,
) -> Result<Option<String>> {
    let commit_revision = format!("{revision}^{{commit}}");
    let verify = run_git(
        runner,
        cwd,
        [
            "rev-parse",
            "--verify",
            "--end-of-options",
            &commit_revision,
        ],
    )
    .await?;
    if !verify.success() {
        return Ok(None);
    }
    let revision_oid = verify.stdout.trim();
    if revision_oid.is_empty() {
        return Ok(None);
    }
    Ok(Some(revision_oid.to_string()))
}

pub(crate) async fn run_git<const N: usize>(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    args: [&str; N],
) -> Result<ReviewCommandOutput> {
    runner
        .run(
            ReviewCommand::new(std::iter::once("git").chain(args), cwd.clone())
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0"),
        )
        .await
}

async fn run_native_review_command(command: ReviewCommand) -> Result<ReviewCommandOutput> {
    let cwd = command.cwd.to_abs_path().with_context(|| {
        format!(
            "review command cwd is not local to this host: {}",
            command.cwd
        )
    })?;
    let (program, args) = command
        .argv
        .split_first()
        .context("review command argv cannot be empty")?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .envs(&command.env)
        .env_remove(GH_REPO_ENV_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start review command {program}"))?;
    let mut stdout = child
        .stdout
        .take()
        .context("review command stdout missing")?;
    let mut stderr = child
        .stderr
        .take()
        .context("review command stderr missing")?;
    let output_bytes_cap = command.output_bytes_cap;
    let collect = async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_capped(&mut stdout, output_bytes_cap),
            read_capped(&mut stderr, output_bytes_cap),
        );
        Ok::<_, anyhow::Error>((status?, stdout?, stderr?))
    };
    let (status, stdout, stderr) = timeout(command.timeout, collect)
        .await
        .with_context(|| format!("review command {program} timed out"))??;
    Ok(ReviewCommandOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

async fn read_capped(
    reader: &mut (impl AsyncRead + Unpin),
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(cap.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(retained);
        }
        let retain = read.min(cap.saturating_sub(retained.len()));
        retained.extend_from_slice(&chunk[..retain]);
    }
}

#[cfg(test)]
#[path = "pull_request_tests.rs"]
mod tests;
