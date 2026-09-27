use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn selection_matrix_never_falls_back_from_required_or_restricted() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    assert_eq!(
        select_local_sandbox_for_platform(
            &PermissionProfile::Disabled,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::Required,
            /*platform_sandbox*/ None,
        ),
        LocalSandboxSelection::Unavailable
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &PermissionProfile::read_only(),
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            /*platform_sandbox*/ None,
        ),
        LocalSandboxSelection::Unavailable
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &PermissionProfile::Disabled,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            /*platform_sandbox*/ None,
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::None,
            permissions: PermissionProfile::Disabled,
        }
    );
}

#[test]
fn selection_materializes_every_workspace_root_before_enforcement() {
    let first = AbsolutePathBuf::current_dir().expect("current directory");
    let second = first.join("second-workspace");
    let roots = vec![first, second];
    let permissions = PermissionProfile::workspace_write();
    let expected = permissions
        .clone()
        .materialize_project_roots_with_workspace_roots(&roots);

    assert_eq!(
        select_local_sandbox_for_platform(
            &permissions,
            &roots,
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::LinuxSeccomp,
            permissions: expected,
        }
    );
}

#[cfg(target_os = "windows")]
#[test]
fn public_selection_observes_disabled_windows_sandbox() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    assert_eq!(
        select_local_sandbox(
            &PermissionProfile::Disabled,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::Required,
            WindowsSandboxLevel::Disabled,
        ),
        LocalSandboxSelection::Unavailable
    );
}
