//! Deterministic Git and GitHub discovery for review-scope selection.

use codex_utils_path_uri::PathUri;
use serde::Deserialize;

use crate::CommitLogEntry;
use crate::ReviewCommand;
use crate::ReviewCommandOutput;
use crate::ReviewCommandRunner;
use crate::has_uncommitted_changes;
use crate::resolve_pr_base_ref_with_runner;
use crate::resolve_review_repository_root;
use crate::review_validation::recent_review_commits;

const GIT_DETECTION_ERROR: &str = "Git detection failed";

/// An open pull request associated with the selected checkout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewScopePullRequest {
    pub number: u64,
    pub url: String,
    pub base_branch: Option<String>,
    pub base_branch_target: Option<String>,
}

/// A detected repository default branch and the resolvable Git target for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewDefaultBranch {
    pub display_name: String,
    pub target: String,
}

/// Repository metadata used to populate a review-scope picker.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewScopeResolution {
    pub pull_request: Option<ReviewScopePullRequest>,
    pub default_branch: Option<ReviewDefaultBranch>,
    pub current_branch: Option<String>,
    pub branches: Vec<String>,
    pub has_uncommitted_changes: bool,
    pub commits: Vec<CommitLogEntry>,
    pub git_error: Option<String>,
}

#[derive(Deserialize)]
struct GhPullRequestView {
    number: u64,
    url: String,
    state: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: Option<String>,
}

#[derive(Deserialize)]
struct GhPullRequestApiItem {
    number: u64,
    #[serde(rename = "html_url")]
    url: String,
    state: String,
    base: Option<GhPullRequestApiBase>,
}

#[derive(Deserialize)]
struct GhPullRequestApiBase {
    #[serde(rename = "ref")]
    branch: String,
}

#[derive(Deserialize)]
struct GhRepoView {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: Option<String>,
    parent: Option<GhRepoParent>,
}

#[derive(Deserialize)]
struct GhRepoParent {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

/// Resolves the preferred review target and branch-picker contents beside `runner`'s checkout.
///
/// Pull-request and default-branch discovery start concurrently. An open pull request for the
/// current branch wins; otherwise repositories containing `HEAD` are searched parent before fork,
/// with the lowest open pull-request number used as a stable tie-breaker.
pub async fn resolve_review_scope(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> ReviewScopeResolution {
    let repository_root = match resolve_review_repository_root(runner, cwd).await {
        Ok(repository_root) => repository_root,
        Err(_) => {
            return ReviewScopeResolution {
                git_error: Some(GIT_DETECTION_ERROR.to_string()),
                ..Default::default()
            };
        }
    };
    let (
        mut pull_request,
        default_branch,
        current_branch,
        mut branches,
        has_uncommitted_changes,
        commits,
    ) = tokio::join!(
        open_pull_request(runner, &repository_root),
        default_branch(runner, &repository_root),
        current_branch(runner, &repository_root),
        local_branches(runner, &repository_root),
        has_uncommitted_changes(runner, &repository_root),
        recent_review_commits(runner, &repository_root),
    );
    let (has_uncommitted_changes, uncommitted_error) = match has_uncommitted_changes {
        Ok(has_uncommitted_changes) => (has_uncommitted_changes, false),
        Err(_) => (false, true),
    };
    let commits = commits.unwrap_or_default();
    let pull_request_base_name = pull_request
        .as_ref()
        .and_then(|pull_request| pull_request.base_branch.clone());
    let pull_request_base = match pull_request_base_name.as_deref() {
        Some(base_branch) => {
            let pull_request_url = pull_request
                .as_ref()
                .map(|pull_request| pull_request.url.as_str())
                .unwrap_or_default();
            resolve_pr_base_ref_with_runner(runner, &repository_root, base_branch, pull_request_url)
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    if let Some(pull_request) = pull_request.as_mut() {
        pull_request.base_branch_target = pull_request_base.clone();
    }
    let (preferred_branch, equivalent_branch) = if pull_request_base.is_some() {
        (
            pull_request_base,
            pull_request_base_name.map(|base_branch| format!("refs/heads/{base_branch}")),
        )
    } else {
        (
            default_branch.as_ref().map(|branch| branch.target.clone()),
            default_branch
                .as_ref()
                .map(|branch| format!("refs/heads/{}", branch.display_name)),
        )
    };
    prioritize_branch(
        &mut branches,
        preferred_branch.as_deref(),
        equivalent_branch.as_deref(),
    );

    ReviewScopeResolution {
        pull_request,
        default_branch,
        current_branch,
        branches,
        has_uncommitted_changes,
        commits,
        git_error: uncommitted_error.then(|| GIT_DETECTION_ERROR.to_string()),
    }
}

fn prioritize_branch(
    branches: &mut Vec<String>,
    preferred_branch: Option<&str>,
    equivalent_branch: Option<&str>,
) {
    let Some(preferred_branch) = preferred_branch.filter(|branch| !branch.is_empty()) else {
        return;
    };
    branches
        .retain(|branch| branch != preferred_branch && Some(branch.as_str()) != equivalent_branch);
    branches.insert(0, preferred_branch.to_string());
}

async fn open_pull_request(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Option<ReviewScopePullRequest> {
    if let Some(pull_request) = open_pull_request_for_current_branch(runner, cwd).await {
        return Some(pull_request);
    }
    open_pull_request_for_head_commit(runner, cwd).await
}

async fn open_pull_request_for_current_branch(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Option<ReviewScopePullRequest> {
    let output = run_gh(
        runner,
        cwd,
        ["pr", "view", "--json", "number,url,state,baseRefName"],
    )
    .await?;
    if !output.success() {
        return None;
    }
    let pull_request = serde_json::from_str::<GhPullRequestView>(&output.stdout).ok()?;
    pull_request
        .state
        .eq_ignore_ascii_case("open")
        .then(|| ReviewScopePullRequest {
            number: pull_request.number,
            url: pull_request.url,
            base_branch: non_empty(pull_request.base_ref_name),
            base_branch_target: None,
        })
}

async fn open_pull_request_for_head_commit(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Option<ReviewScopePullRequest> {
    let head_sha = git_stdout(runner, cwd, ["rev-parse", "HEAD"]).await?;
    for repo in gh_repo_search_order(runner, cwd).await? {
        let endpoint = format!("repos/{repo}/commits/{head_sha}/pulls");
        let Some(output) = run_gh(
            runner,
            cwd,
            [
                "api",
                "--paginate",
                "--slurp",
                "-H",
                "Accept: application/vnd.github+json",
                &endpoint,
            ],
        )
        .await
        else {
            continue;
        };
        if output.success()
            && let Some(pull_request) = pull_request_from_api_output(&output.stdout)
        {
            return Some(pull_request);
        }
    }
    None
}

fn pull_request_from_api_output(stdout: &str) -> Option<ReviewScopePullRequest> {
    let pull_requests = serde_json::from_str::<Vec<Vec<GhPullRequestApiItem>>>(stdout)
        .map(|pages| pages.into_iter().flatten().collect())
        .or_else(|_| serde_json::from_str::<Vec<GhPullRequestApiItem>>(stdout))
        .ok()?;
    pull_requests
        .into_iter()
        .filter(|pull_request| pull_request.state.eq_ignore_ascii_case("open"))
        .min_by_key(|pull_request| pull_request.number)
        .map(|pull_request| ReviewScopePullRequest {
            number: pull_request.number,
            url: pull_request.url,
            base_branch: pull_request
                .base
                .map(|base| base.branch)
                .filter(|branch| !branch.is_empty()),
            base_branch_target: None,
        })
}

async fn gh_repo_search_order(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Option<Vec<String>> {
    let output = run_gh(
        runner,
        cwd,
        ["repo", "view", "--json", "nameWithOwner,parent"],
    )
    .await?;
    if !output.success() {
        return None;
    }
    let repo = serde_json::from_str::<GhRepoView>(&output.stdout).ok()?;
    let mut repos = Vec::new();
    if let Some(parent) = repo.parent {
        repos.push(parent.name_with_owner);
    }
    if let Some(name_with_owner) = non_empty(repo.name_with_owner)
        && !repos.contains(&name_with_owner)
    {
        repos.push(name_with_owner);
    }
    (!repos.is_empty()).then_some(repos)
}

async fn default_branch(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Option<ReviewDefaultBranch> {
    let mut remotes: Vec<String> = git_stdout(runner, cwd, ["remote"])
        .await
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(origin_index) = remotes.iter().position(|remote| remote == "origin") {
        let origin = remotes.remove(origin_index);
        remotes.insert(0, origin);
    }

    for remote in remotes {
        let remote_head = format!("refs/remotes/{remote}/HEAD");
        if let Some(symbolic_ref) =
            git_stdout(runner, cwd, ["symbolic-ref", "--quiet", &remote_head]).await
        {
            let remote_prefix = format!("refs/remotes/{remote}/");
            if let Some(branch) = symbolic_ref.strip_prefix(&remote_prefix)
                && git_ref_exists(runner, cwd, &symbolic_ref).await
            {
                return Some(ReviewDefaultBranch {
                    display_name: branch.to_string(),
                    target: symbolic_ref,
                });
            }
        }

        if let Some(remote_show) = git_stdout(runner, cwd, ["remote", "show", &remote]).await {
            for line in remote_show.lines() {
                let Some(branch) = line.trim().strip_prefix("HEAD branch:").map(str::trim) else {
                    continue;
                };
                let remote_ref = format!("refs/remotes/{remote}/{branch}");
                if !branch.is_empty() && git_ref_exists(runner, cwd, &remote_ref).await {
                    return Some(ReviewDefaultBranch {
                        display_name: branch.to_string(),
                        target: remote_ref,
                    });
                }
            }
        }
    }

    for candidate in ["main", "master"] {
        let local_ref = format!("refs/heads/{candidate}");
        if git_ref_exists(runner, cwd, &local_ref).await {
            return Some(ReviewDefaultBranch {
                display_name: candidate.to_string(),
                target: local_ref,
            });
        }
    }
    None
}

async fn current_branch(runner: &impl ReviewCommandRunner, cwd: &PathUri) -> Option<String> {
    git_stdout(runner, cwd, ["branch", "--show-current"])
        .await
        .filter(|branch| !branch.is_empty())
}

async fn local_branches(runner: &impl ReviewCommandRunner, cwd: &PathUri) -> Vec<String> {
    let mut branches: Vec<String> = git_stdout(
        runner,
        cwd,
        ["for-each-ref", "--format=%(refname)", "refs/heads"],
    )
    .await
    .unwrap_or_default()
    .lines()
    .map(str::trim)
    .filter(|branch| !branch.is_empty())
    .map(str::to_string)
    .collect();
    branches.sort_unstable();
    branches.dedup();
    branches
}

async fn git_ref_exists(runner: &impl ReviewCommandRunner, cwd: &PathUri, reference: &str) -> bool {
    run_git(runner, cwd, ["rev-parse", "--verify", "--quiet", reference])
        .await
        .is_some_and(|output| output.success())
}

async fn git_stdout<const N: usize>(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    args: [&str; N],
) -> Option<String> {
    let output = run_git(runner, cwd, args).await?;
    output.success().then(|| output.stdout.trim().to_string())
}

async fn run_git<const N: usize>(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    args: [&str; N],
) -> Option<ReviewCommandOutput> {
    runner
        .run(
            ReviewCommand::new(std::iter::once("git").chain(args), cwd.clone())
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C"),
        )
        .await
        .ok()
}

async fn run_gh<const N: usize>(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    args: [&str; N],
) -> Option<ReviewCommandOutput> {
    runner
        .run(
            ReviewCommand::new(std::iter::once("gh").chain(args), cwd.clone())
                .env("GH_PROMPT_DISABLED", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .timeout(super::pull_request::GH_COMMAND_TIMEOUT),
        )
        .await
        .ok()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
#[path = "review_scope_tests.rs"]
mod tests;
