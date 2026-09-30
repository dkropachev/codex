#[cfg(target_os = "windows")]
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::permissions::project_roots_glob_pattern;
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
    let external = PermissionProfile::External {
        network: NetworkSandboxPolicy::Enabled,
    };
    assert_eq!(
        select_local_sandbox_for_platform(
            &external,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            /*platform_sandbox*/ None,
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::None,
            permissions: external.clone(),
        }
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &external,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::Required,
            /*platform_sandbox*/ None,
        ),
        LocalSandboxSelection::Unavailable
    );
}

#[test]
fn selection_rejects_required_linux_sandbox_that_would_apply_no_restrictions() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    for permissions in [
        PermissionProfile::Disabled,
        PermissionProfile::External {
            network: NetworkSandboxPolicy::Enabled,
        },
        PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                FileSystemAccessMode::Write,
            )]),
            NetworkSandboxPolicy::Enabled,
        ),
    ] {
        assert_eq!(
            select_local_sandbox_for_platform(
                &permissions,
                std::slice::from_ref(&root),
                LocalSandboxLaunchPolicy::Required,
                Some(SandboxType::LinuxSeccomp),
            ),
            LocalSandboxSelection::Unavailable
        );
    }

    let network_restricted = PermissionProfile::External {
        network: NetworkSandboxPolicy::Restricted,
    };
    assert_eq!(
        select_local_sandbox_for_platform(
            &network_restricted,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::Required,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::LinuxSeccomp,
            permissions: network_restricted,
        }
    );

    let filesystem_restricted = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::read_only(),
        NetworkSandboxPolicy::Enabled,
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &filesystem_restricted,
            std::slice::from_ref(&root),
            LocalSandboxLaunchPolicy::Required,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::LinuxSeccomp,
            permissions: filesystem_restricted,
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

#[cfg(unix)]
#[test]
fn selection_rejects_unrepresentable_workspace_root_materialization() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let invalid = root.join(std::path::PathBuf::from(OsString::from_vec(vec![0xff])));
    let permissions = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                pattern: project_roots_glob_pattern(std::path::Path::new("**/*.env")),
            },
            FileSystemAccessMode::Deny,
        )]),
        NetworkSandboxPolicy::Restricted,
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &permissions,
            std::slice::from_ref(&invalid),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Unavailable
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &PermissionProfile::workspace_write(),
            std::slice::from_ref(&invalid),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Unavailable
    );
    assert_eq!(
        select_local_sandbox_for_platform(
            &PermissionProfile::Disabled,
            std::slice::from_ref(&invalid),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::None,
            permissions: PermissionProfile::Disabled,
        }
    );
    let read_only = PermissionProfile::read_only();
    assert_eq!(
        select_local_sandbox_for_platform(
            &read_only,
            &[invalid],
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
            Some(SandboxType::LinuxSeccomp),
        ),
        LocalSandboxSelection::Selected {
            sandbox: SandboxType::LinuxSeccomp,
            permissions: read_only,
        }
    );
}

#[test]
fn selection_rejects_workspace_root_glob_metacharacters() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let permissions = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                pattern: project_roots_glob_pattern(std::path::Path::new("**/*.env")),
            },
            FileSystemAccessMode::Deny,
        )]),
        NetworkSandboxPolicy::Restricted,
    );
    let mut metacharacters = vec!["workspace*", "workspace?", "workspace[1]", "workspace{1}"];
    if cfg!(unix) {
        metacharacters.push("workspace\\1");
    }
    for component in metacharacters {
        assert_eq!(
            select_local_sandbox_for_platform(
                &permissions,
                &[root.join(component)],
                LocalSandboxLaunchPolicy::FollowPermissionProfile,
                Some(SandboxType::LinuxSeccomp),
            ),
            LocalSandboxSelection::Unavailable,
            "workspace root component {component:?} must not alter the deny glob"
        );
    }

    let workspace_write = PermissionProfile::workspace_write();
    let metacharacter_root = root.join("workspace[1]");
    let expected = workspace_write
        .clone()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&metacharacter_root));
    assert_eq!(
        select_local_sandbox_for_platform(
            &workspace_write,
            std::slice::from_ref(&metacharacter_root),
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
    for level in [
        WindowsSandboxLevel::RestrictedToken,
        WindowsSandboxLevel::Elevated,
    ] {
        assert_eq!(
            select_local_sandbox(
                &PermissionProfile::Disabled,
                std::slice::from_ref(&root),
                LocalSandboxLaunchPolicy::Required,
                level,
            ),
            LocalSandboxSelection::Selected {
                sandbox: SandboxType::WindowsRestrictedToken,
                permissions: PermissionProfile::Disabled,
            }
        );
    }
}
