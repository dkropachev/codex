use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
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
    if workspace_roots
        .iter()
        .any(|root| root.as_path().to_str().is_none())
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
