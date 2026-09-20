use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;

use super::ReviewSnapshotCommandRunner;
use crate::ReviewCommand;
use crate::ReviewCommandOutput;

const POSIX_DISABLED_HOOKS_PATH: &str = "/dev/null";
const WINDOWS_DISABLED_HOOKS_PATH: &str = "NUL";

pub(super) async fn write_tree(
    runner: &impl ReviewSnapshotCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    attributes_source: &str,
) -> Result<String> {
    let output = run_git(
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
    runner: &impl ReviewSnapshotCommandRunner,
    repository_root: &PathUri,
) -> Result<String> {
    let output = run_git(
        runner,
        repository_root,
        /*index_path*/ None,
        vec!["mktree".to_string()],
        /*attributes_source*/ None,
    )
    .await
    .context("git mktree")?;
    require_success("git mktree", &output)?;
    parse_object_id(&output.stdout, "git mktree")
}

pub(super) async fn resolve_index_path(
    runner: &impl ReviewSnapshotCommandRunner,
    repository_root: &PathUri,
) -> Result<PathUri> {
    let output = run_git(
        runner,
        repository_root,
        /*index_path*/ None,
        ["rev-parse", "--git-path", "index"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        /*attributes_source*/ None,
    )
    .await
    .context("failed to resolve the Git index path")?;
    require_success("git rev-parse --git-path index", &output)?;
    let path = parse_path_output(&output.stdout, "git rev-parse --git-path index")?;
    repository_root
        .join(path)
        .with_context(|| format!("Git returned an invalid index path {path:?}"))
}

pub(super) async fn resolve_object(
    runner: &impl ReviewSnapshotCommandRunner,
    repository_root: &PathUri,
    revision: &str,
    description: &str,
) -> Result<String> {
    let output = run_git(
        runner,
        repository_root,
        /*index_path*/ None,
        ["rev-parse", "--verify", revision]
            .into_iter()
            .map(str::to_string)
            .collect(),
        /*attributes_source*/ None,
    )
    .await
    .with_context(|| format!("failed to resolve {description}"))?;
    require_success(description, &output)?;
    parse_object_id(&output.stdout, "git rev-parse")
}

pub(super) async fn resolve_head_ref(
    runner: &impl ReviewSnapshotCommandRunner,
    repository_root: &PathUri,
) -> Result<Option<String>> {
    let output = run_git(
        runner,
        repository_root,
        /*index_path*/ None,
        ["symbolic-ref", "-q", "HEAD"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        /*attributes_source*/ None,
    )
    .await
    .context("failed to inspect HEAD")?;
    match output.exit_code {
        0 => {
            let reference = parse_single_line(&output.stdout, "git symbolic-ref")?;
            if !reference.starts_with("refs/heads/") {
                bail!("git symbolic-ref did not return a local branch");
            }
            Ok(Some(reference.to_string()))
        }
        1 => Ok(None),
        _ => Err(command_error("git symbolic-ref", &output)),
    }
}

pub(super) async fn ensure_head(
    runner: &impl ReviewSnapshotCommandRunner,
    repository_root: &PathUri,
    expected_sha: &str,
    expected_ref: Option<&str>,
) -> Result<()> {
    let actual_sha =
        resolve_object(runner, repository_root, "HEAD^{commit}", "HEAD commit").await?;
    let actual_ref = resolve_head_ref(runner, repository_root).await?;
    if actual_sha != expected_sha || actual_ref.as_deref() != expected_ref {
        bail!("HEAD changed while capturing the review fix snapshot");
    }
    Ok(())
}

async fn run_git(
    runner: &impl ReviewSnapshotCommandRunner,
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
    .env("GIT_NO_REPLACE_OBJECTS", "1")
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

fn require_success(description: &str, output: &ReviewCommandOutput) -> Result<()> {
    if output.exit_code == 0 {
        Ok(())
    } else {
        Err(command_error(description, output))
    }
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

fn parse_object_id(stdout: &str, command: &str) -> Result<String> {
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

fn parse_path_output<'a>(stdout: &'a str, command: &str) -> Result<&'a str> {
    let value = stdout
        .strip_suffix('\n')
        .context(format!("{command} returned unterminated output"))?;
    if value.is_empty() || value.contains('\0') {
        bail!("{command} returned an invalid path");
    }
    Ok(value)
}
