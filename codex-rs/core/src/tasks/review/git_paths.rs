use anyhow::Context as _;
use codex_git_utils::resolve_review_git_directories;
use codex_utils_path_uri::PathUri;

use crate::session::ExecutorReviewCommandRunner;
use crate::session::turn_context::TurnContext;

pub(super) async fn resolve_git_protected_paths(
    ctx: &TurnContext,
    checkout_root: &PathUri,
    parent_sandbox: &codex_file_system::FileSystemSandboxContext,
) -> anyhow::Result<Vec<PathUri>> {
    let environment = ctx
        .environments
        .primary()
        .context("review requires a selected environment")?;
    let runner = ExecutorReviewCommandRunner::new(
        environment.environment.get_exec_backend(),
        &ctx.config.permissions.shell_environment_policy,
    );
    let filesystem = environment.environment.get_filesystem();
    let mut paths = Vec::new();
    for path in resolve_review_git_directories(&runner, checkout_root).await? {
        if path.starts_with(checkout_root) {
            paths.push(path);
        } else if let Ok(path) = filesystem.canonicalize(&path, Some(parent_sandbox)).await {
            paths.push(path);
        }
    }
    let mut object_directories = paths
        .iter()
        .filter_map(|path| path.join("objects").ok())
        .collect::<std::collections::VecDeque<_>>();
    let mut seen_object_directories = std::collections::HashSet::new();
    while let Some(objects) = object_directories.pop_front() {
        if seen_object_directories.len() == 64 || !seen_object_directories.insert(objects.clone()) {
            continue;
        }
        let objects = if objects.starts_with(checkout_root) {
            objects
        } else {
            let Ok(objects) = filesystem
                .canonicalize(&objects, Some(parent_sandbox))
                .await
            else {
                continue;
            };
            objects
        };
        paths.push(objects.clone());
        let Ok(alternates_path) = objects.join("info/alternates") else {
            continue;
        };
        let Ok(contents) = filesystem
            .read_file(&alternates_path, Some(parent_sandbox))
            .await
        else {
            continue;
        };
        let Ok(contents) = String::from_utf8(contents) else {
            continue;
        };
        object_directories.extend(
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .filter_map(|line| objects.join(line).ok()),
        );
    }
    paths.push(checkout_root.join(".git")?);
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    Ok(paths)
}
