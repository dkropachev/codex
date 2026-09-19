use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_file_system::ExecutorFileSystem;
use codex_utils_path_uri::PathUri;

use super::ReviewFixCommitSnapshot;
use super::change::ReviewFixFileChange;
use super::change::ValidatedReviewFixChange;
use super::change::affected_pathspecs;
use super::change::apply_update_diff;
use super::git::IndexEntry;
use super::git::hash_file_without_filters;
use super::git::index_entry;
use super::git::read_blob_text;
use super::git::read_tree;
use super::git::remove_index_entry;
use super::git::set_index_entry;
use super::git::write_tree;
use super::index_flags::restore_preserved_index_flags;
use crate::ReviewCommandRunner;

pub(super) struct PreparedCommitTrees {
    pub(super) commit_tree: String,
    pub(super) preserved_index_tree: String,
    pub(super) index_mutations: Vec<IndexMutation>,
}

#[derive(Clone)]
pub(super) struct IndexMutation {
    pub(super) path: String,
    pub(super) before: Option<IndexEntry>,
    pub(super) after: Option<IndexEntry>,
}

pub(super) async fn prepare_commit_trees(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    snapshot: &ReviewFixCommitSnapshot,
    artifacts: &[PathUri],
    changes: &[ValidatedReviewFixChange<'_>],
) -> Result<Option<PreparedCommitTrees>> {
    let [commit_index, preserved_index, change_content] = artifacts else {
        unreachable!("review commit preparation uses three temporary artifacts");
    };
    read_tree(
        runner,
        &snapshot.repository_root,
        commit_index,
        &snapshot.head_tree,
        &snapshot.empty_tree,
    )
    .await?;
    fs.write_file(
        preserved_index,
        snapshot.index_contents.clone(),
        /*sandbox*/ None,
    )
    .await
    .with_context(|| format!("failed to create temporary Git index {preserved_index}"))?;
    let paths = affected_pathspecs(changes);
    let before = read_entries(runner, snapshot, preserved_index, &paths).await?;

    for change in changes {
        apply_change_to_indexes(
            runner,
            fs,
            &snapshot.repository_root,
            &snapshot.empty_tree,
            [commit_index, preserved_index],
            change_content,
            change,
        )
        .await?;
    }

    let commit_tree = write_tree(
        runner,
        &snapshot.repository_root,
        commit_index,
        &snapshot.empty_tree,
    )
    .await?;
    if commit_tree == snapshot.head_tree {
        return Ok(None);
    }
    restore_preserved_index_flags(
        runner,
        &snapshot.repository_root,
        preserved_index,
        &snapshot.index_flags,
    )
    .await?;
    let preserved_index_tree = write_tree(
        runner,
        &snapshot.repository_root,
        preserved_index,
        &snapshot.empty_tree,
    )
    .await?;
    let after = read_entries(runner, snapshot, preserved_index, &paths).await?;
    let index_mutations = paths
        .into_iter()
        .zip(before)
        .zip(after)
        .filter_map(|((path, before), after)| {
            (before != after).then_some(IndexMutation {
                path,
                before,
                after,
            })
        })
        .collect();
    Ok(Some(PreparedCommitTrees {
        commit_tree,
        preserved_index_tree,
        index_mutations,
    }))
}

async fn read_entries(
    runner: &impl ReviewCommandRunner,
    snapshot: &ReviewFixCommitSnapshot,
    index: &PathUri,
    paths: &[String],
) -> Result<Vec<Option<IndexEntry>>> {
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        entries.push(
            index_entry(
                runner,
                &snapshot.repository_root,
                index,
                path,
                &snapshot.empty_tree,
            )
            .await?,
        );
    }
    Ok(entries)
}

async fn apply_change_to_indexes(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    repository_root: &PathUri,
    attributes_source: &str,
    indexes: [&PathUri; 2],
    content_path: &PathUri,
    change: &ValidatedReviewFixChange<'_>,
) -> Result<()> {
    match change.change {
        ReviewFixFileChange::Add { content, .. } => {
            let object_id =
                hash_content(runner, fs, repository_root, content_path, content).await?;
            let entry = IndexEntry {
                mode: "100644".to_string(),
                object_id,
            };
            for index in indexes {
                if index_entry(
                    runner,
                    repository_root,
                    index,
                    &change.path,
                    attributes_source,
                )
                .await?
                .is_some()
                {
                    bail!(
                        "review fix add would overwrite an existing index entry: {}",
                        change.path
                    );
                }
                set_index_entry(
                    runner,
                    repository_root,
                    index,
                    &change.path,
                    &entry,
                    attributes_source,
                )
                .await?;
            }
        }
        ReviewFixFileChange::Delete { content, .. } => {
            let expected = hash_content(runner, fs, repository_root, content_path, content).await?;
            for index in indexes {
                let entry = index_entry(
                    runner,
                    repository_root,
                    index,
                    &change.path,
                    attributes_source,
                )
                .await?
                .with_context(|| format!("review fix delete path is absent: {}", change.path))?;
                if entry.object_id != expected {
                    bail!(
                        "review fix delete does not match the indexed content: {}",
                        change.path
                    );
                }
                remove_index_entry(
                    runner,
                    repository_root,
                    index,
                    &change.path,
                    attributes_source,
                )
                .await?;
            }
        }
        ReviewFixFileChange::Update { unified_diff, .. } => {
            if !unified_diff.is_empty() {
                for index in indexes {
                    let mut entry = index_entry(
                        runner,
                        repository_root,
                        index,
                        &change.path,
                        attributes_source,
                    )
                    .await?
                    .with_context(|| {
                        format!("review fix update path is absent: {}", change.path)
                    })?;
                    let original =
                        read_blob_text(runner, repository_root, &entry.object_id).await?;
                    let content = apply_update_diff(&original, unified_diff)?
                        .context("review fix update contains no changes")?;
                    entry.object_id =
                        hash_content(runner, fs, repository_root, content_path, &content).await?;
                    set_index_entry(
                        runner,
                        repository_root,
                        index,
                        &change.path,
                        &entry,
                        attributes_source,
                    )
                    .await?;
                }
            }
            if let Some(move_path) = change.move_path.as_deref() {
                if move_path == change.path {
                    bail!(
                        "review fix move source and destination are the same: {}",
                        change.path
                    );
                }
                for index in indexes {
                    if index_entry(runner, repository_root, index, move_path, attributes_source)
                        .await?
                        .is_some()
                    {
                        bail!(
                            "review fix move would overwrite an existing index entry: {move_path}"
                        );
                    }
                    let entry = index_entry(
                        runner,
                        repository_root,
                        index,
                        &change.path,
                        attributes_source,
                    )
                    .await?
                    .with_context(|| {
                        format!("review fix move source is absent: {}", change.path)
                    })?;
                    set_index_entry(
                        runner,
                        repository_root,
                        index,
                        move_path,
                        &entry,
                        attributes_source,
                    )
                    .await?;
                    remove_index_entry(
                        runner,
                        repository_root,
                        index,
                        &change.path,
                        attributes_source,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn hash_content(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    repository_root: &PathUri,
    content_path: &PathUri,
    content: &str,
) -> Result<String> {
    fs.write_file(
        content_path,
        content.as_bytes().to_vec(),
        /*sandbox*/ None,
    )
    .await
    .with_context(|| format!("failed to write exact review fix content {content_path}"))?;
    hash_file_without_filters(
        runner,
        repository_root,
        content_path,
        /*write_object*/ true,
    )
    .await
}
