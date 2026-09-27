use std::path::Path;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::project_roots_glob_pattern;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::SandboxManager;
use crate::SandboxType;
use crate::SandboxablePreference;
use crate::get_platform_sandbox;

/// Determines whether platform sandbox enforcement is mandatory for a local launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalSandboxLaunchPolicy {
    Required,
    FollowPermissionProfile,
}

/// Materialized permissions and platform sandbox selected for a local launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalSandboxSelection {
    Selected {
        sandbox: SandboxType,
        permissions: PermissionProfile,
    },
    Unavailable,
}

/// Materializes symbolic workspace-root permissions and selects local sandbox enforcement.
pub fn select_local_sandbox(
    permissions: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    policy: LocalSandboxLaunchPolicy,
    windows_sandbox_level: WindowsSandboxLevel,
) -> LocalSandboxSelection {
    select_local_sandbox_for_platform(
        permissions,
        workspace_roots,
        policy,
        get_platform_sandbox(windows_sandbox_level != WindowsSandboxLevel::Disabled),
    )
}

fn select_local_sandbox_for_platform(
    permissions: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    policy: LocalSandboxLaunchPolicy,
    platform_sandbox: Option<SandboxType>,
) -> LocalSandboxSelection {
    let has_non_utf8_workspace_root = workspace_roots
        .iter()
        .any(|root| root.as_path().to_str().is_none());
    let project_roots_glob_prefix = project_roots_glob_pattern(Path::new(""));
    let filesystem_policy = permissions.file_system_sandbox_policy();
    let has_symbolic_project_roots_glob = filesystem_policy.entries.iter().any(|entry| {
        matches!(
            &entry.path,
            FileSystemPath::GlobPattern { pattern }
                if pattern.starts_with(&project_roots_glob_prefix)
        )
    });
    let has_symbolic_project_roots_path = filesystem_policy.entries.iter().any(|entry| {
        matches!(
            &entry.path,
            FileSystemPath::Special {
                value: FileSystemSpecialPath::ProjectRoots { .. },
            }
        )
    });
    let has_glob_metacharacter = workspace_roots.iter().any(|root| {
        root.as_path().to_str().is_some_and(|root| {
            root.chars()
                .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
                || cfg!(unix) && root.contains('\\')
        })
    });
    if (has_non_utf8_workspace_root
        && (has_symbolic_project_roots_path || has_symbolic_project_roots_glob))
        || (has_glob_metacharacter && has_symbolic_project_roots_glob)
    {
        return LocalSandboxSelection::Unavailable;
    }
    let permissions = permissions
        .clone()
        .materialize_project_roots_with_workspace_roots(workspace_roots);
    let preference = match policy {
        LocalSandboxLaunchPolicy::Required => SandboxablePreference::Require,
        LocalSandboxLaunchPolicy::FollowPermissionProfile => SandboxablePreference::Auto,
    };
    let required = SandboxManager::new().should_sandbox(
        &permissions,
        preference,
        /*has_managed_network_requirements*/ false,
    );
    let sandbox = if required {
        let Some(sandbox) = platform_sandbox else {
            return LocalSandboxSelection::Unavailable;
        };
        sandbox
    } else {
        SandboxType::None
    };
    LocalSandboxSelection::Selected {
        sandbox,
        permissions,
    }
}

#[cfg(test)]
#[path = "local_selection_tests.rs"]
mod tests;
