use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;

use crate::ReviewCommand;
use crate::ReviewCommandOutput;
use crate::ReviewCommandRunner;
use crate::pull_request::REVIEW_COMMAND_OUTPUT_BYTES_CAP;

const POSIX_DISABLED_HOOKS_PATH: &str = "/dev/null";
const WINDOWS_DISABLED_HOOKS_PATH: &str = "NUL";

pub(super) async fn hash_file_without_filters(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    content_path: &PathUri,
    write_object: bool,
) -> Result<String> {
    let mut args = vec!["hash-object".to_string()];
    if write_object {
        args.push("-w".to_string());
    }
    args.extend([
        "--no-filters".to_string(),
        "--".to_string(),
        content_path.inferred_native_path_string(),
    ]);
    let output = run_git_dynamic_checked(
        runner,
        repository_root,
        /*index_path*/ None,
        args,
        "git hash-object --no-filters",
    )
    .await?;
    parse_object_id(&output.stdout, "git hash-object")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IndexEntry {
    pub(super) mode: String,
    pub(super) object_id: String,
}

pub(super) async fn read_blob_text(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    object_id: &str,
) -> Result<String> {
    let size_output = run_git_checked(
        runner,
        repository_root,
        /*index_path*/ None,
        ["cat-file", "-s", object_id],
        "git cat-file -s",
    )
    .await?;
    let size = parse_single_line(&size_output.stdout, "git cat-file -s")?
        .parse::<usize>()
        .context("git cat-file returned an invalid blob size")?;
    if size >= REVIEW_COMMAND_OUTPUT_BYTES_CAP {
        bail!("review fix update blob exceeds the safe command output limit");
    }
    let output = run_git_checked(
        runner,
        repository_root,
        /*index_path*/ None,
        ["cat-file", "blob", object_id],
        "git cat-file blob",
    )
    .await?;
    if output.stdout.len() != size {
        bail!("git cat-file returned truncated or non-UTF-8 review fix content");
    }
    Ok(output.stdout)
}

pub(super) async fn index_entry(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    path: &str,
    attributes_source: &str,
) -> Result<Option<IndexEntry>> {
    let output = run_git_dynamic_with_attributes(
        runner,
        repository_root,
        Some(index_path),
        vec![
            "ls-files".to_string(),
            "--stage".to_string(),
            "-z".to_string(),
            "--".to_string(),
            path.to_string(),
        ],
        Some(attributes_source),
    )
    .await
    .context("git ls-files --stage")?;
    require_success("git ls-files --stage", &output)?;
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let records = output.stdout.split_terminator('\0').collect::<Vec<_>>();
    let [record] = records.as_slice() else {
        bail!("review fix path has multiple Git index entries: {path}");
    };
    let (metadata, _) = record
        .split_once('\t')
        .context("git ls-files returned an invalid index entry")?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    let [mode, object_id, stage] = fields.as_slice() else {
        bail!("git ls-files returned an invalid index entry");
    };
    if *stage != "0" {
        bail!("review fix path has an unmerged Git index entry: {path}");
    }
    parse_object_id(object_id, "git ls-files")?;
    if !mode.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
        bail!("git ls-files returned an invalid index mode");
    }
    Ok(Some(IndexEntry {
        mode: (*mode).to_string(),
        object_id: (*object_id).to_string(),
    }))
}

pub(super) async fn set_index_entry(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    path: &str,
    entry: &IndexEntry,
    attributes_source: &str,
) -> Result<()> {
    let output = run_git_dynamic_with_attributes(
        runner,
        repository_root,
        Some(index_path),
        vec![
            "update-index".to_string(),
            "--add".to_string(),
            "--cacheinfo".to_string(),
            entry.mode.clone(),
            entry.object_id.clone(),
            path.to_string(),
        ],
        Some(attributes_source),
    )
    .await
    .context("git update-index --cacheinfo")?;
    require_success("git update-index --cacheinfo", &output)?;
    Ok(())
}

pub(super) async fn remove_index_entry(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    path: &str,
    attributes_source: &str,
) -> Result<()> {
    let output = run_git_dynamic_with_attributes(
        runner,
        repository_root,
        Some(index_path),
        vec![
            "update-index".to_string(),
            "--force-remove".to_string(),
            "--".to_string(),
            path.to_string(),
        ],
        Some(attributes_source),
    )
    .await
    .context("git update-index --force-remove")?;
    require_success("git update-index --force-remove", &output)?;
    Ok(())
}

pub(super) async fn write_tree(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    attributes_source: &str,
) -> Result<String> {
    let output = run_git_dynamic_with_attributes(
        runner,
        repository_root,
        Some(index_path),
        vec!["write-tree".to_string()],
        Some(attributes_source),
    )
    .await
    .context("git write-tree")?;
    require_success("git write-tree", &output)?;
    parse_object_id(&output.stdout, "git write-tree")
}

pub(super) async fn resolve_empty_tree(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<String> {
    let output = run_git_checked(
        runner,
        repository_root,
        /*index_path*/ None,
        ["mktree"],
        "git mktree",
    )
    .await?;
    parse_object_id(&output.stdout, "git mktree")
}

pub(super) async fn read_tree(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    tree: &str,
    attributes_source: &str,
) -> Result<()> {
    let output = run_git_dynamic_with_attributes(
        runner,
        repository_root,
        Some(index_path),
        vec!["read-tree".to_string(), tree.to_string()],
        Some(attributes_source),
    )
    .await
    .context("git read-tree")?;
    require_success("git read-tree", &output)?;
    Ok(())
}

pub(super) async fn resolve_index_path(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<PathUri> {
    let output = run_git_checked(
        runner,
        repository_root,
        /*index_path*/ None,
        ["rev-parse", "--git-path", "index"],
        "failed to resolve the Git index path",
    )
    .await?;
    let path = parse_single_line(&output.stdout, "git rev-parse --git-path index")?;
    repository_root
        .join(path)
        .with_context(|| format!("Git returned an invalid index path {path:?}"))
}

pub(super) async fn resolve_object(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: Option<&PathUri>,
    revision: &str,
    description: &str,
) -> Result<String> {
    let output = run_git_checked(
        runner,
        repository_root,
        index_path,
        ["rev-parse", "--verify", revision],
        &format!("failed to resolve {description}"),
    )
    .await?;
    parse_object_id(&output.stdout, "git rev-parse")
}

pub(super) async fn resolve_optional_object(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    revision: &str,
) -> Result<Option<String>> {
    let output = run_git(
        runner,
        repository_root,
        /*index_path*/ None,
        ["rev-parse", "--verify", "--quiet", revision],
    )
    .await
    .context("failed to resolve optional Git object")?;
    match output.exit_code {
        0 => parse_object_id(&output.stdout, "git rev-parse").map(Some),
        1 => Ok(None),
        _ => Err(command_error("git rev-parse --verify --quiet", &output)),
    }
}

pub(super) async fn resolve_head_ref(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<Option<String>> {
    let output = run_git(
        runner,
        repository_root,
        /*index_path*/ None,
        ["symbolic-ref", "-q", "HEAD"],
    )
    .await
    .context("failed to inspect HEAD")?;
    match output.exit_code {
        0 => {
            let reference = parse_single_line(&output.stdout, "git symbolic-ref")?;
            if !reference.starts_with("refs/") {
                bail!("git symbolic-ref returned an invalid HEAD reference");
            }
            Ok(Some(reference.to_string()))
        }
        1 => Ok(None),
        _ => Err(command_error("git symbolic-ref", &output)),
    }
}

pub(super) async fn ensure_head(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    expected_sha: &str,
    expected_ref: Option<&str>,
) -> Result<()> {
    let actual_sha = resolve_object(
        runner,
        repository_root,
        /*index_path*/ None,
        "HEAD^{commit}",
        "HEAD commit",
    )
    .await?;
    let actual_ref = resolve_head_ref(runner, repository_root).await?;
    if actual_sha != expected_sha || actual_ref.as_deref() != expected_ref {
        bail!("HEAD changed after the review fix snapshot");
    }
    Ok(())
}

pub(super) async fn run_git_checked<const N: usize>(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: Option<&PathUri>,
    args: [&str; N],
    description: &str,
) -> Result<ReviewCommandOutput> {
    let output = run_git(runner, repository_root, index_path, args)
        .await
        .with_context(|| description.to_string())?;
    require_success(description, &output)?;
    Ok(output)
}

async fn run_git<const N: usize>(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: Option<&PathUri>,
    args: [&str; N],
) -> Result<ReviewCommandOutput> {
    run_git_dynamic(
        runner,
        repository_root,
        index_path,
        args.into_iter().map(str::to_string).collect(),
    )
    .await
}

async fn run_git_dynamic(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: Option<&PathUri>,
    args: Vec<String>,
) -> Result<ReviewCommandOutput> {
    run_git_dynamic_with_attributes(runner, repository_root, index_path, args, None).await
}

async fn run_git_dynamic_with_attributes(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: Option<&PathUri>,
    args: Vec<String>,
    attributes_source: Option<&str>,
) -> Result<ReviewCommandOutput> {
    let hooks_path = match repository_root.infer_path_convention() {
        Some(PathConvention::Posix) => POSIX_DISABLED_HOOKS_PATH,
        Some(PathConvention::Windows) => WINDOWS_DISABLED_HOOKS_PATH,
        None => bail!("cannot infer the review repository path convention"),
    };
    let mut command = ReviewCommand::new(
        [
            "git".to_string(),
            "-c".to_string(),
            format!("core.hooksPath={hooks_path}"),
            "-c".to_string(),
            "core.fsmonitor=false".to_string(),
        ]
        .into_iter()
        .chain(args),
        repository_root.clone(),
    )
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_LITERAL_PATHSPECS", "1")
    .env("LC_ALL", "C");
    if let Some(index_path) = index_path {
        command = command.env("GIT_INDEX_FILE", index_path.inferred_native_path_string());
    }
    if let Some(attributes_source) = attributes_source {
        command = command.env("GIT_ATTR_SOURCE", attributes_source);
    }
    runner.run(command).await
}

pub(super) async fn run_git_dynamic_checked(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: Option<&PathUri>,
    args: Vec<String>,
    description: &str,
) -> Result<ReviewCommandOutput> {
    let output = run_git_dynamic(runner, repository_root, index_path, args)
        .await
        .with_context(|| description.to_string())?;
    require_success(description, &output)?;
    Ok(output)
}

fn require_success(description: &str, output: &ReviewCommandOutput) -> Result<()> {
    if output.exit_code == 0 {
        return Ok(());
    }
    Err(command_error(description, output))
}

fn command_error(description: &str, output: &ReviewCommandOutput) -> anyhow::Error {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        anyhow::anyhow!("{description}: Git exited with {}", output.exit_code)
    } else {
        anyhow::anyhow!(
            "{description}: Git exited with {}: {stderr}",
            output.exit_code
        )
    }
}

pub(super) fn parse_object_id(stdout: &str, command: &str) -> Result<String> {
    let object_id = parse_single_line(stdout, command)?;
    if !matches!(object_id.len(), 40 | 64)
        || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("{command} returned an invalid object ID");
    }
    Ok(object_id.to_ascii_lowercase())
}

fn parse_single_line<'a>(stdout: &'a str, command: &str) -> Result<&'a str> {
    let mut lines = stdout.lines();
    let value = lines.next().unwrap_or_default().trim();
    if value.is_empty() || lines.any(|line| !line.trim().is_empty()) {
        bail!("{command} returned invalid output");
    }
    Ok(value)
}
