use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_file_system::ExecutorFileSystem;

use super::ReviewFixCommitOutcome;
use super::ReviewFixCommitSnapshot;
use super::ensure_index_unchanged;
use super::git::IndexEntry;
use super::git::ensure_head;
use super::git::index_entry;
use super::git::parse_object_id;
use super::git::remove_index_entry;
use super::git::resolve_head_ref;
use super::git::resolve_object;
use super::git::resolve_optional_object;
use super::git::run_git_checked;
use super::git::set_index_entry;
use super::git::write_tree;
use super::index_flags::restore_preserved_index_flags;
use super::index_flags::restore_preserved_index_flags_for_paths;
use super::prepare::IndexMutation;
use crate::ReviewCommandRunner;

pub(super) async fn finish_commit_transaction(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    snapshot: &ReviewFixCommitSnapshot,
    mutations: &[IndexMutation],
    preserved_index_tree: &str,
    commit_sha: &str,
) -> Result<ReviewFixCommitOutcome> {
    let target_ref = snapshot
        .head_ref
        .as_deref()
        .context("review fix commits are unavailable from a detached HEAD")?;
    ensure_head(
        runner,
        &snapshot.repository_root,
        &snapshot.head_sha,
        snapshot.head_ref.as_deref(),
    )
    .await?;
    ensure_index_unchanged(fs, &snapshot.index_path, &snapshot.index_contents).await?;
    install_index(runner, snapshot, mutations, preserved_index_tree).await?;
    if let Err(head_error) = ensure_head(
        runner,
        &snapshot.repository_root,
        &snapshot.head_sha,
        Some(target_ref),
    )
    .await
    {
        return fail_after_index_install(
            runner,
            snapshot,
            mutations,
            preserved_index_tree,
            head_error.context("HEAD changed while installing the review fix index"),
        )
        .await;
    }
    let update_result = update_reference(runner, snapshot, target_ref, commit_sha).await;
    reconcile_reference_update(
        runner,
        snapshot,
        mutations,
        preserved_index_tree,
        target_ref,
        commit_sha,
        update_result.err(),
    )
    .await
}

pub(super) async fn create_commit(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    commit_tree: &str,
    commit_message: &str,
) -> Result<String> {
    let output = run_git_checked(
        runner,
        &snapshot.repository_root,
        Some(&snapshot.index_path),
        [
            "commit-tree",
            commit_tree,
            "-p",
            &snapshot.head_sha,
            "-m",
            commit_message,
        ],
        "git commit-tree",
    )
    .await?;
    parse_object_id(&output.stdout, "git commit-tree")
}

async fn install_index(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    mutations: &[IndexMutation],
    expected_tree: &str,
) -> Result<()> {
    let mut installed = 0;
    let install = async {
        for mutation in mutations {
            require_index_entry(runner, snapshot, &mutation.path, mutation.before.as_ref()).await?;
            write_index_entry(runner, snapshot, &mutation.path, mutation.after.as_ref()).await?;
            installed += 1;
        }
        restore_preserved_index_flags(
            runner,
            &snapshot.repository_root,
            &snapshot.index_path,
            &snapshot.index_flags,
        )
        .await?;
        let actual_tree = write_tree(
            runner,
            &snapshot.repository_root,
            &snapshot.index_path,
            &snapshot.empty_tree,
        )
        .await?;
        if actual_tree != expected_tree {
            bail!("installed Git index does not match the prepared review fix index");
        }
        Ok(())
    }
    .await;
    if let Err(install_err) = install {
        let restore_err = restore_index(runner, snapshot, &mutations[..installed])
            .await
            .err();
        return Err(combine_index_errors(install_err, restore_err));
    }
    Ok(())
}

async fn restore_index(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    mutations: &[IndexMutation],
) -> Result<()> {
    let mut restored_paths = Vec::new();
    let mut conflicts = Vec::new();
    for mutation in mutations.iter().rev() {
        let actual = index_entry(
            runner,
            &snapshot.repository_root,
            &snapshot.index_path,
            &mutation.path,
            &snapshot.empty_tree,
        )
        .await?;
        if actual == mutation.after {
            write_index_entry(runner, snapshot, &mutation.path, mutation.before.as_ref()).await?;
            restored_paths.push(mutation.path.clone());
        } else if actual != mutation.before {
            conflicts.push(mutation.path.clone());
        }
    }
    restore_preserved_index_flags_for_paths(
        runner,
        &snapshot.repository_root,
        &snapshot.index_path,
        &snapshot.index_flags,
        &restored_paths,
    )
    .await?;
    if !conflicts.is_empty() {
        bail!(
            "Git index entries changed during review fix rollback: {}",
            conflicts.join(", ")
        );
    }
    let restored_tree = write_tree(
        runner,
        &snapshot.repository_root,
        &snapshot.index_path,
        &snapshot.empty_tree,
    )
    .await?;
    if restored_tree != snapshot.index_tree {
        bail!("restored Git index does not match the review fix snapshot");
    }
    Ok(())
}

async fn require_index_entry(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    path: &str,
    expected: Option<&IndexEntry>,
) -> Result<()> {
    let actual = index_entry(
        runner,
        &snapshot.repository_root,
        &snapshot.index_path,
        path,
        &snapshot.empty_tree,
    )
    .await?;
    if actual.as_ref() != expected {
        bail!("Git index entry changed during review fix commit: {path}");
    }
    Ok(())
}

async fn write_index_entry(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    path: &str,
    entry: Option<&IndexEntry>,
) -> Result<()> {
    match entry {
        Some(entry) => {
            set_index_entry(
                runner,
                &snapshot.repository_root,
                &snapshot.index_path,
                path,
                entry,
                &snapshot.empty_tree,
            )
            .await
        }
        None => {
            remove_index_entry(
                runner,
                &snapshot.repository_root,
                &snapshot.index_path,
                path,
                &snapshot.empty_tree,
            )
            .await
        }
    }
}

fn combine_index_errors(primary: anyhow::Error, restore: Option<anyhow::Error>) -> anyhow::Error {
    match restore {
        Some(restore) => anyhow::anyhow!("{primary:#}; additionally, {restore:#}"),
        None => primary,
    }
}

async fn reconcile_reference_update(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    mutations: &[IndexMutation],
    installed_index_tree: &str,
    target_ref: &str,
    commit_sha: &str,
    update_error: Option<anyhow::Error>,
) -> Result<ReviewFixCommitOutcome> {
    let target_sha = current_target_sha(runner, snapshot, target_ref).await;
    let head = current_head(runner, snapshot).await;
    let index_tree = current_index_tree(runner, snapshot).await;
    let installed = matches!(&target_sha, Ok(Some(sha)) if sha == commit_sha)
        && matches!(&head, Ok((sha, Some(head_ref))) if sha == commit_sha && head_ref == target_ref)
        && matches!(&index_tree, Ok(tree) if tree == installed_index_tree);
    if installed {
        return Ok(ReviewFixCommitOutcome::Committed {
            commit_sha: commit_sha.to_string(),
        });
    }

    let state_error = anyhow::anyhow!(
        "review fix commit did not leave HEAD, its target ref, and the index in one consistent state"
    );
    let primary = match update_error {
        Some(update_error) => anyhow::anyhow!("{update_error:#}; {state_error}"),
        None => state_error,
    };
    let ref_restore = match target_sha {
        Ok(Some(sha)) if sha == commit_sha => {
            rollback_reference(runner, snapshot, target_ref, commit_sha)
                .await
                .err()
        }
        Ok(_) => None,
        Err(error) => {
            let inspect_error = error.context("could not inspect the review fix target ref");
            match rollback_reference(runner, snapshot, target_ref, commit_sha).await {
                Ok(()) => Some(inspect_error),
                Err(rollback_error) => Some(anyhow::anyhow!(
                    "{inspect_error:#}; additionally, {rollback_error:#}"
                )),
            }
        }
    };
    let index_restore =
        restore_index_if_installed(runner, snapshot, mutations, installed_index_tree)
            .await
            .err();
    Err(combine_transaction_errors(
        primary,
        ref_restore,
        index_restore,
    ))
}

async fn fail_after_index_install(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    mutations: &[IndexMutation],
    installed_index_tree: &str,
    error: anyhow::Error,
) -> Result<ReviewFixCommitOutcome> {
    let restore_error =
        restore_index_if_installed(runner, snapshot, mutations, installed_index_tree)
            .await
            .err();
    Err(combine_index_errors(error, restore_error))
}

async fn current_head(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
) -> Result<(String, Option<String>)> {
    let sha = resolve_object(
        runner,
        &snapshot.repository_root,
        /*index_path*/ None,
        "HEAD^{commit}",
        "HEAD commit",
    )
    .await?;
    let head_ref = resolve_head_ref(runner, &snapshot.repository_root).await?;
    Ok((sha, head_ref))
}

async fn current_target_sha(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    target_ref: &str,
) -> Result<Option<String>> {
    resolve_optional_object(
        runner,
        &snapshot.repository_root,
        &format!("{target_ref}^{{commit}}"),
    )
    .await
}

async fn current_index_tree(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
) -> Result<String> {
    write_tree(
        runner,
        &snapshot.repository_root,
        &snapshot.index_path,
        &snapshot.empty_tree,
    )
    .await
}

async fn restore_index_if_installed(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    mutations: &[IndexMutation],
    _installed_index_tree: &str,
) -> Result<()> {
    restore_index(runner, snapshot, mutations).await
}

fn combine_transaction_errors(
    primary: anyhow::Error,
    reference: Option<anyhow::Error>,
    index: Option<anyhow::Error>,
) -> anyhow::Error {
    let error = combine_index_errors(primary, reference);
    combine_index_errors(error, index)
}

async fn update_reference(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    target_ref: &str,
    commit_sha: &str,
) -> Result<()> {
    run_git_checked(
        runner,
        &snapshot.repository_root,
        Some(&snapshot.index_path),
        [
            "update-ref",
            "-m",
            "review: apply verified fixes",
            target_ref,
            commit_sha,
            &snapshot.head_sha,
        ],
        "failed to update HEAD for the review fix commit",
    )
    .await?;
    Ok(())
}

async fn rollback_reference(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    target_ref: &str,
    commit_sha: &str,
) -> Result<()> {
    let rollback = run_git_checked(
        runner,
        &snapshot.repository_root,
        Some(&snapshot.index_path),
        [
            "update-ref",
            "-m",
            "review: roll back interrupted fix",
            target_ref,
            &snapshot.head_sha,
            commit_sha,
        ],
        "failed to roll back the review fix ref",
    )
    .await;
    let current = current_target_sha(runner, snapshot, target_ref).await?;
    if current.as_deref() == Some(snapshot.head_sha.as_str()) {
        return Ok(());
    }
    match rollback {
        Ok(_) => bail!("review fix target ref changed after rollback"),
        Err(error) => Err(error),
    }
}
