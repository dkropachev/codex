use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;

use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::ReadDirectoryEntry;
use codex_protocol::permissions::ReadDenyMatcher;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_string::take_bytes_at_char_boundary;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::env_path::format_oai_env_uri;
use crate::tools::handlers::env_path::parse_oai_env_uri;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::resolve_tool_environment;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct ListDirHandler;

const DENY_READ_POLICY_MESSAGE: &str =
    "access denied: reading this path is blocked by filesystem deny_read policy";
const MAX_ENTRY_LENGTH: usize = 500;
const INDENTATION_SPACES: usize = 2;

fn default_offset() -> usize {
    1
}

fn default_limit() -> usize {
    25
}

fn default_depth() -> usize {
    2
}

#[derive(Deserialize)]
struct ListDirArgs {
    dir_path: String,
    environment_id: Option<String>,
    #[serde(default = "default_offset")]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_depth")]
    depth: usize,
}

impl ToolHandler for ListDirHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation { payload, turn, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "list_dir handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: ListDirArgs = parse_arguments(&arguments)?;

        let ListDirArgs {
            dir_path,
            environment_id,
            offset,
            limit,
            depth,
        } = args;

        if offset == 0 {
            return Err(FunctionCallError::RespondToModel(
                "offset must be a 1-indexed entry number".to_string(),
            ));
        }

        if limit == 0 {
            return Err(FunctionCallError::RespondToModel(
                "limit must be greater than zero".to_string(),
            ));
        }

        if depth == 0 {
            return Err(FunctionCallError::RespondToModel(
                "depth must be greater than zero".to_string(),
            ));
        }

        let environment_path = parse_oai_env_uri(&dir_path)?;
        if let Some(path_environment_id) = environment_path
            .as_ref()
            .map(|environment_path| environment_path.environment_id.as_str())
            && let Some(environment_id) = environment_id.as_deref()
            && environment_id != path_environment_id
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "environment_id `{environment_id}` does not match path environment `{path_environment_id}`"
            )));
        }

        let selected_environment_id = environment_path
            .as_ref()
            .map(|environment_path| environment_path.environment_id.clone())
            .or(environment_id);
        let Some(turn_environment) =
            resolve_tool_environment(turn.as_ref(), selected_environment_id.as_deref())?
        else {
            return Err(FunctionCallError::RespondToModel(
                "list_dir is unavailable in this session".to_string(),
            ));
        };

        let cwd = turn_environment.cwd.clone();
        let path = match environment_path {
            Some(environment_path) => environment_path.path,
            None => resolve_dir_path(&dir_path, &cwd)?,
        };
        let file_system_sandbox_policy = turn.file_system_sandbox_policy();
        let read_deny_matcher = ReadDenyMatcher::new(file_system_sandbox_policy, &cwd);
        if read_deny_matcher
            .as_ref()
            .is_some_and(|matcher| matcher.is_read_denied(path.as_path()))
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "{DENY_READ_POLICY_MESSAGE}: `{}`",
                path.as_path().display()
            )));
        }

        let file_system = turn_environment.environment.get_filesystem();
        let entries = list_dir_slice_with_policy(
            file_system.as_ref(),
            /*sandbox*/ None,
            &path,
            offset,
            limit,
            depth,
            read_deny_matcher.as_ref(),
        )
        .await?;
        let mut output = Vec::with_capacity(entries.len() + 1);
        if let Some(environment_id) = selected_environment_id.as_deref() {
            output.push(format!(
                "Environment path: {}",
                format_oai_env_uri(environment_id, &path)
            ));
        } else {
            output.push(format!("Absolute path: {}", path.as_path().display()));
        }
        output.extend(entries);
        Ok(FunctionToolOutput::from_text(output.join("\n"), Some(true)))
    }
}

fn resolve_dir_path(
    dir_path: &str,
    cwd: &AbsolutePathBuf,
) -> Result<AbsolutePathBuf, FunctionCallError> {
    let path = PathBuf::from(dir_path);
    if path.is_absolute() {
        AbsolutePathBuf::from_absolute_path(&path)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
    } else {
        Ok(cwd.join(path))
    }
}

async fn list_dir_slice_with_policy(
    file_system: &dyn ExecutorFileSystem,
    sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    path: &AbsolutePathBuf,
    offset: usize,
    limit: usize,
    depth: usize,
    read_deny_matcher: Option<&ReadDenyMatcher>,
) -> Result<Vec<String>, FunctionCallError> {
    let mut entries = Vec::new();
    collect_entries(
        file_system,
        sandbox,
        path,
        Path::new(""),
        depth,
        read_deny_matcher,
        &mut entries,
    )
    .await?;

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    let start_index = offset - 1;
    if start_index >= entries.len() {
        return Err(FunctionCallError::RespondToModel(
            "offset exceeds directory entry count".to_string(),
        ));
    }

    let remaining_entries = entries.len() - start_index;
    let capped_limit = limit.min(remaining_entries);
    let end_index = start_index + capped_limit;
    let selected_entries = &entries[start_index..end_index];
    let mut formatted = Vec::with_capacity(selected_entries.len());

    for entry in selected_entries {
        formatted.push(format_entry_line(entry));
    }

    if end_index < entries.len() {
        formatted.push(format!("More than {capped_limit} entries found"));
    }

    Ok(formatted)
}

async fn collect_entries(
    file_system: &dyn ExecutorFileSystem,
    sandbox: Option<&codex_exec_server::FileSystemSandboxContext>,
    dir_path: &AbsolutePathBuf,
    relative_prefix: &Path,
    depth: usize,
    read_deny_matcher: Option<&ReadDenyMatcher>,
    entries: &mut Vec<DirEntry>,
) -> Result<(), FunctionCallError> {
    let mut queue = VecDeque::new();
    queue.push_back((dir_path.clone(), relative_prefix.to_path_buf(), depth));

    while let Some((current_dir, prefix, remaining_depth)) = queue.pop_front() {
        let directory_entries = file_system
            .read_directory(&current_dir, sandbox)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to read directory: {err}"))
            })?;
        let mut dir_entries = Vec::new();

        for entry in directory_entries {
            let entry_path = current_dir.join(&entry.file_name);
            if let Some(read_deny_matcher) = read_deny_matcher
                && read_deny_matcher.is_read_denied(entry_path.as_path())
            {
                continue;
            }

            let relative_path = if prefix.as_os_str().is_empty() {
                PathBuf::from(&entry.file_name)
            } else {
                prefix.join(&entry.file_name)
            };

            let display_name = format_entry_component(&entry.file_name);
            let display_depth = prefix.components().count();
            let sort_key = format_entry_name(&relative_path);
            let kind = DirEntryKind::from(&entry);
            dir_entries.push((
                entry_path,
                relative_path,
                kind,
                DirEntry {
                    name: sort_key,
                    display_name,
                    depth: display_depth,
                    kind,
                },
            ));
        }

        dir_entries.sort_unstable_by(|a, b| a.3.name.cmp(&b.3.name));

        for (entry_path, relative_path, kind, dir_entry) in dir_entries {
            let can_recurse = !dir_entry.display_name.contains(char::REPLACEMENT_CHARACTER);
            if kind == DirEntryKind::Directory && remaining_depth > 1 && can_recurse {
                queue.push_back((entry_path, relative_path, remaining_depth - 1));
            }
            entries.push(dir_entry);
        }
    }

    Ok(())
}

fn format_entry_name(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace("\\", "/");
    if normalized.len() > MAX_ENTRY_LENGTH {
        take_bytes_at_char_boundary(&normalized, MAX_ENTRY_LENGTH).to_string()
    } else {
        normalized
    }
}

fn format_entry_component(name: &str) -> String {
    if name.len() > MAX_ENTRY_LENGTH {
        take_bytes_at_char_boundary(name, MAX_ENTRY_LENGTH).to_string()
    } else {
        name.to_string()
    }
}

fn format_entry_line(entry: &DirEntry) -> String {
    let indent = " ".repeat(entry.depth * INDENTATION_SPACES);
    let mut name = entry.display_name.clone();
    match entry.kind {
        DirEntryKind::Directory => name.push('/'),
        DirEntryKind::Symlink => name.push('@'),
        DirEntryKind::Other => name.push('?'),
        DirEntryKind::File => {}
    }
    format!("{indent}{name}")
}

#[derive(Clone)]
struct DirEntry {
    name: String,
    display_name: String,
    depth: usize,
    kind: DirEntryKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

impl From<&ReadDirectoryEntry> for DirEntryKind {
    fn from(entry: &ReadDirectoryEntry) -> Self {
        if entry.is_symlink {
            DirEntryKind::Symlink
        } else if entry.is_directory {
            DirEntryKind::Directory
        } else if entry.is_file {
            DirEntryKind::File
        } else {
            DirEntryKind::Other
        }
    }
}

#[cfg(test)]
#[path = "list_dir_tests.rs"]
mod tests;
