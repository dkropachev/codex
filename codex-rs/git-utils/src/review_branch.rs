use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathUri;

use crate::ReviewCommandRunner;
use crate::canonicalize_git_remote_url;
use crate::pull_request::resolve_revision_oid;
use crate::pull_request::run_git;

/// Resolves a PR base branch to an unambiguous remote-tracking ref for its repository.
///
/// Multiple matching remotes are accepted only when their refs resolve to the same commit. This
/// avoids silently using a fork remote, tag, or divergent tracking branch with the same short name.
pub async fn resolve_pr_base_ref_with_runner(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    branch: &str,
    pull_request_url: &str,
) -> Result<Option<String>> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Ok(None);
    }
    let repository_url = pull_request_url
        .rsplit_once("/pull/")
        .map(|(repository_url, _)| repository_url)
        .and_then(canonicalize_git_remote_url)
        .ok_or_else(|| anyhow::anyhow!("cannot identify repository from {pull_request_url:?}"))?;

    let remotes = run_git(runner, cwd, ["remote"]).await?;
    if !remotes.success() {
        bail!(
            "`git remote` exited with {}: {}",
            remotes.exit_code,
            remotes.stderr.trim()
        );
    }
    let mut matching_remotes = Vec::new();
    for remote in remotes
        .stdout
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
    {
        let url = run_git(runner, cwd, ["remote", "get-url", remote]).await?;
        if url.success()
            && canonicalize_git_remote_url(url.stdout.trim()).as_deref() == Some(&repository_url)
        {
            matching_remotes.push(remote.to_string());
        }
    }
    matching_remotes.sort_unstable();
    matching_remotes.dedup();

    let mut resolved = Vec::new();
    for remote in matching_remotes {
        let reference = format!("refs/remotes/{remote}/{branch}");
        if let Some(oid) = resolve_revision_oid(runner, cwd, &reference).await? {
            resolved.push((reference, oid));
        }
    }
    let Some((selected_ref, selected_oid)) = resolved.first() else {
        return Ok(None);
    };
    if resolved.iter().any(|(_, oid)| oid != selected_oid) {
        let refs = resolved
            .iter()
            .map(|(reference, oid)| format!("{reference} ({oid})"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!("branch {branch:?} is ambiguous across base-repository refs: {refs}");
    }
    Ok(Some(selected_ref.clone()))
}
