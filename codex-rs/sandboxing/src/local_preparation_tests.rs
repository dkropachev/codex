use std::collections::HashMap;
#[cfg(unix)]
use std::ffi::OsString;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn command(root: &AbsolutePathBuf) -> LocalProcessCommand {
    LocalProcessCommand {
        program: "tool".into(),
        args: vec!["argument".into()],
        cwd: root.clone(),
        env: HashMap::from([("OPENAI_IDENTITY_TOKEN_FILE".into(), "secret".into())]),
    }
}

fn runtime(root: &AbsolutePathBuf) -> LocalSandboxRuntime<'_> {
    LocalSandboxRuntime {
        direct_spawn: SandboxDirectSpawnRuntime {
            codex_home: root,
            windows_sandbox_wrapper_executable: None,
        },
        linux_sandbox_executable: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
    }
}

fn request(
    root: &AbsolutePathBuf,
    selection: LocalSandboxSelection,
    policy: LocalSandboxLaunchPolicy,
) -> LocalSandboxPreparationRequest<'_> {
    LocalSandboxPreparationRequest {
        command: command(root),
        selection,
        policy,
        sandbox_policy_cwd: root,
        workspace_roots: std::slice::from_ref(root),
        runtime: runtime(root),
    }
}

fn unavailable(outcome: LocalSandboxPreparation) -> LocalSandboxUnavailableReason {
    match outcome {
        LocalSandboxPreparation::Prepared(_) => panic!("expected unavailable sandbox"),
        LocalSandboxPreparation::Unavailable(reason) => reason,
    }
}

fn selected(sandbox: SandboxType, permissions: PermissionProfile) -> LocalSandboxSelection {
    LocalSandboxSelection::Selected {
        sandbox,
        permissions,
    }
}

#[test]
fn unavailable_and_platform_selections_fail_closed() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    for (selection, expected) in [
        (
            LocalSandboxSelection::Unavailable,
            LocalSandboxUnavailableReason::SelectionUnavailable,
        ),
        (
            selected(SandboxType::LinuxSeccomp, PermissionProfile::read_only()),
            LocalSandboxUnavailableReason::PlatformPreparation,
        ),
    ] {
        assert_eq!(
            unavailable(
                prepare_local_sandbox_command(request(
                    &root,
                    selection,
                    LocalSandboxLaunchPolicy::Required,
                ))
                .expect("prepare unavailable selection"),
            ),
            expected
        );
    }
}

#[test]
fn mandatory_or_restricted_permissions_reject_forged_unrestricted_selection() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    for (permissions, policy) in [
        (
            PermissionProfile::Disabled,
            LocalSandboxLaunchPolicy::Required,
        ),
        (
            PermissionProfile::read_only(),
            LocalSandboxLaunchPolicy::FollowPermissionProfile,
        ),
    ] {
        assert_eq!(
            unavailable(
                prepare_local_sandbox_command(request(
                    &root,
                    selected(SandboxType::None, permissions),
                    policy,
                ))
                .expect("prepare forged unrestricted selection"),
            ),
            LocalSandboxUnavailableReason::SelectionUnavailable
        );
    }
}

#[test]
fn permitted_unrestricted_selection_preserves_command_and_scrubs_secrets() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let LocalSandboxPreparation::Prepared(prepared) = prepare_local_sandbox_command(request(
        &root,
        selected(SandboxType::None, PermissionProfile::Disabled),
        LocalSandboxLaunchPolicy::FollowPermissionProfile,
    ))
    .expect("prepare unrestricted command") else {
        panic!("expected prepared command");
    };
    assert_eq!(prepared.sandbox(), SandboxType::None);
    let command = prepared.into_command();
    assert_eq!(command.get_program(), "tool");
    assert_eq!(command.get_args().collect::<Vec<_>>(), vec!["argument"]);
    assert_eq!(command.get_current_dir(), Some(root.as_path()));
    assert_eq!(command.get_envs().collect::<Vec<_>>(), Vec::new());
}

#[cfg(unix)]
#[test]
fn permitted_unrestricted_selection_preserves_native_values() {
    use std::os::unix::ffi::OsStringExt;

    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let program = OsString::from_vec(vec![b't', b'o', 0xff]);
    let argument = OsString::from_vec(vec![b'a', 0xfe]);
    let mut request = request(
        &root,
        selected(SandboxType::None, PermissionProfile::Disabled),
        LocalSandboxLaunchPolicy::FollowPermissionProfile,
    );
    request.command.program = program.clone();
    request.command.args = vec![argument.clone()];
    let LocalSandboxPreparation::Prepared(prepared) =
        prepare_local_sandbox_command(request).expect("prepare native unrestricted command")
    else {
        panic!("expected prepared command");
    };
    let command = prepared.into_command();
    assert_eq!(command.get_program(), program);
    assert_eq!(command.get_args().collect::<Vec<_>>(), vec![argument]);
}
