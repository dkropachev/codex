use std::collections::HashMap;
#[cfg(any(unix, windows))]
use std::ffi::OsString;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
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

fn permissions(path: FileSystemPath, access: FileSystemAccessMode) -> PermissionProfile {
    PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(path, access)]),
        NetworkSandboxPolicy::Restricted,
    )
}

fn assert_unrepresentable(request: LocalSandboxPreparationRequest<'_>, input: &'static str) {
    let outcome = prepare_local_sandbox_command(request).expect("prepare sandbox command");
    let LocalSandboxPreparation::Unavailable(reason) = outcome else {
        panic!("expected unrepresentable {input}");
    };
    assert_eq!(
        reason,
        LocalSandboxUnavailableReason::UnrepresentableInput(input)
    );
}

#[test]
fn unavailable_and_platform_selections_fail_closed() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let unavailable_sandbox = match get_platform_sandbox(/*windows_sandbox_enabled*/ true) {
        Some(SandboxType::LinuxSeccomp) => SandboxType::MacosSeatbelt,
        Some(SandboxType::MacosSeatbelt | SandboxType::WindowsRestrictedToken)
        | Some(SandboxType::None)
        | None => SandboxType::LinuxSeccomp,
    };
    for (selection, expected) in [
        (
            LocalSandboxSelection::Unavailable,
            LocalSandboxUnavailableReason::SelectionUnavailable,
        ),
        (
            selected(unavailable_sandbox, PermissionProfile::read_only()),
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

#[cfg(target_os = "linux")]
#[test]
fn linux_preparation_validates_inputs_and_preserves_selected_profile() {
    use std::os::unix::ffi::OsStringExt;

    let root = AbsolutePathBuf::current_dir().expect("current directory");
    assert_eq!(
        unavailable(
            prepare_local_sandbox_command(request(
                &root,
                selected(SandboxType::MacosSeatbelt, PermissionProfile::read_only()),
                LocalSandboxLaunchPolicy::Required,
            ))
            .expect("prepare mismatched sandbox"),
        ),
        LocalSandboxUnavailableReason::PlatformPreparation
    );
    assert_eq!(
        unavailable(
            prepare_local_sandbox_command(request(
                &root,
                selected(SandboxType::LinuxSeccomp, PermissionProfile::read_only()),
                LocalSandboxLaunchPolicy::Required,
            ))
            .expect("prepare without helper"),
        ),
        LocalSandboxUnavailableReason::MissingLinuxSandboxExecutable
    );

    let helper = root.join("codex-linux-sandbox");
    let materialized_permissions = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&root));
    let mut valid = request(
        &root,
        selected(SandboxType::LinuxSeccomp, materialized_permissions.clone()),
        LocalSandboxLaunchPolicy::Required,
    );
    valid.runtime.linux_sandbox_executable = Some(&helper);
    valid
        .command
        .env
        .insert("SAFE_ENV".into(), "safe-value".into());
    valid.command.env.insert(
        "OPENAI_IDENTITY_TOKEN_FILE".into(),
        OsString::from_vec(vec![0xff]),
    );
    let LocalSandboxPreparation::Prepared(prepared) =
        prepare_local_sandbox_command(valid).expect("prepare valid command")
    else {
        panic!("expected prepared Linux sandbox command");
    };
    assert_eq!(prepared.sandbox(), SandboxType::LinuxSeccomp);
    let command = prepared.into_command();
    assert_eq!(command.get_program(), helper.as_path());
    assert_eq!(
        command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<Vec<_>>(),
        vec![("SAFE_ENV".into(), Some("safe-value".into()))]
    );
    let args = command
        .get_args()
        .map(|argument| argument.to_str().expect("Unicode sandbox argument"))
        .collect::<Vec<_>>();
    let profile = args
        .windows(2)
        .find_map(|args| (args[0] == "--permission-profile").then_some(args[1]))
        .expect("permission profile argument");
    assert_eq!(
        serde_json::from_str::<PermissionProfile>(profile).expect("deserialize permission profile"),
        materialized_permissions
    );

    let mut invalid = request(
        &root,
        selected(SandboxType::LinuxSeccomp, PermissionProfile::read_only()),
        LocalSandboxLaunchPolicy::Required,
    );
    invalid.runtime.linux_sandbox_executable = Some(&helper);
    invalid.command.args = vec![OsString::from_vec(vec![0xff])];
    assert_unrepresentable(invalid, "command argument");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_preparation_wraps_commands_and_classifies_backend_failures() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory");
    let root = AbsolutePathBuf::from_absolute_path(directory.path()).expect("absolute root");
    let LocalSandboxPreparation::Prepared(prepared) = prepare_local_sandbox_command(request(
        &root,
        selected(SandboxType::MacosSeatbelt, PermissionProfile::read_only()),
        LocalSandboxLaunchPolicy::Required,
    ))
    .expect("prepare macOS sandbox command") else {
        panic!("expected prepared macOS sandbox command");
    };
    assert_eq!(prepared.sandbox(), SandboxType::MacosSeatbelt);
    assert_eq!(
        prepared.into_command().get_program(),
        std::path::Path::new("/usr/bin/sandbox-exec")
    );

    let outside = root.join("outside");
    let writable = root.join("writable-link");
    std::fs::create_dir(&outside).expect("create symlink target");
    symlink(&outside, &writable).expect("create writable symlink");
    let failure = request(
        &root,
        selected(
            SandboxType::MacosSeatbelt,
            permissions(writable.into(), FileSystemAccessMode::Write),
        ),
        LocalSandboxLaunchPolicy::Required,
    );
    assert_eq!(
        unavailable(prepare_local_sandbox_command(failure).expect("classify Seatbelt failure")),
        LocalSandboxUnavailableReason::PlatformPreparation
    );
}

#[cfg(unix)]
#[test]
fn sandbox_preflight_rejects_non_unicode_paths() {
    use std::os::unix::ffi::OsStringExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let root = AbsolutePathBuf::from_absolute_path(directory.path()).expect("absolute path");
    let invalid = root.join(std::path::PathBuf::from(OsString::from_vec(vec![0xff])));
    let helper = root.join("codex-linux-sandbox");
    let sandbox =
        get_platform_sandbox(/*windows_sandbox_enabled*/ false).expect("Unix platform sandbox");

    for (command_cwd, policy_cwd, expected) in [
        (&invalid, &root, "command cwd"),
        (&root, &invalid, "sandbox policy cwd"),
    ] {
        let mut preflight = request(
            &root,
            selected(sandbox, PermissionProfile::read_only()),
            LocalSandboxLaunchPolicy::Required,
        );
        preflight.command.cwd = command_cwd.clone();
        preflight.sandbox_policy_cwd = policy_cwd;
        preflight.runtime.linux_sandbox_executable = Some(&helper);
        assert_unrepresentable(preflight, expected);
    }

    #[cfg(target_os = "linux")]
    {
        let mut preflight = request(
            &root,
            selected(sandbox, PermissionProfile::read_only()),
            LocalSandboxLaunchPolicy::Required,
        );
        preflight.runtime.linux_sandbox_executable = Some(&invalid);
        assert_unrepresentable(preflight, "Linux sandbox executable");
    }

    let mut preflight = request(
        &root,
        selected(
            sandbox,
            permissions(invalid.into(), FileSystemAccessMode::Read),
        ),
        LocalSandboxLaunchPolicy::Required,
    );
    preflight.runtime.linux_sandbox_executable = Some(&helper);
    assert_unrepresentable(preflight, "permission path");

    for (access, expected) in [
        (FileSystemAccessMode::Read, "resolved readable root"),
        (FileSystemAccessMode::Deny, "resolved unreadable root"),
    ] {
        let mut preflight = request(
            &root,
            selected(
                sandbox,
                permissions(
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::project_roots(Some(
                            "invalid\0subpath".into(),
                        )),
                    },
                    access,
                ),
            ),
            LocalSandboxLaunchPolicy::Required,
        );
        preflight.runtime.linux_sandbox_executable = Some(&helper);
        assert_unrepresentable(preflight, expected);
    }

    let mut preflight = request(
        &root,
        selected(
            sandbox,
            permissions(
                FileSystemPath::GlobPattern {
                    pattern: "private\0/**".to_string(),
                },
                FileSystemAccessMode::Deny,
            ),
        ),
        LocalSandboxLaunchPolicy::Required,
    );
    preflight.runtime.linux_sandbox_executable = Some(&helper);
    assert_unrepresentable(preflight, "permission glob");
}

#[cfg(windows)]
#[test]
fn windows_preflight_validates_wrapper_and_every_string_backed_path() {
    use std::os::windows::ffi::OsStringExt;

    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let wrapper = root.join("codex.exe");
    let invalid = root.join(std::path::PathBuf::from(OsString::from_wide(&[0xd800])));
    let preflight = |command_cwd: &AbsolutePathBuf,
                     policy_cwd: &AbsolutePathBuf,
                     permissions: PermissionProfile,
                     codex_home: &AbsolutePathBuf,
                     wrapper: Option<&AbsolutePathBuf>,
                     workspace_roots: &[AbsolutePathBuf],
                     level| {
        let mut request = request(
            &root,
            selected(SandboxType::WindowsRestrictedToken, permissions),
            LocalSandboxLaunchPolicy::Required,
        );
        request.command.cwd = command_cwd.clone();
        request.sandbox_policy_cwd = policy_cwd;
        request.workspace_roots = workspace_roots;
        request.runtime.direct_spawn.codex_home = codex_home;
        request
            .runtime
            .direct_spawn
            .windows_sandbox_wrapper_executable = wrapper;
        request.runtime.windows_sandbox_level = level;
        unavailable(prepare_local_sandbox_command(request).expect("preflight Windows sandbox"))
    };
    assert_eq!(
        preflight(
            &root,
            &root,
            PermissionProfile::read_only(),
            &root,
            Some(&wrapper),
            std::slice::from_ref(&root),
            WindowsSandboxLevel::Disabled,
        ),
        LocalSandboxUnavailableReason::PlatformPreparation
    );
    assert_eq!(
        preflight(
            &root,
            &root,
            PermissionProfile::read_only(),
            &root,
            /*wrapper*/ None,
            std::slice::from_ref(&root),
            WindowsSandboxLevel::RestrictedToken,
        ),
        LocalSandboxUnavailableReason::MissingWindowsSandboxWrapper
    );
    let invalid_permissions = permissions(invalid.clone().into(), FileSystemAccessMode::Read);
    for (reason, input) in [
        (
            preflight(
                &invalid,
                &root,
                PermissionProfile::read_only(),
                &root,
                Some(&wrapper),
                std::slice::from_ref(&root),
                WindowsSandboxLevel::RestrictedToken,
            ),
            "command cwd",
        ),
        (
            preflight(
                &root,
                &invalid,
                PermissionProfile::read_only(),
                &root,
                Some(&wrapper),
                std::slice::from_ref(&root),
                WindowsSandboxLevel::RestrictedToken,
            ),
            "sandbox policy cwd",
        ),
        (
            preflight(
                &root,
                &root,
                invalid_permissions,
                &root,
                Some(&wrapper),
                std::slice::from_ref(&root),
                WindowsSandboxLevel::RestrictedToken,
            ),
            "permission path",
        ),
        (
            preflight(
                &root,
                &root,
                PermissionProfile::read_only(),
                &invalid,
                Some(&wrapper),
                std::slice::from_ref(&root),
                WindowsSandboxLevel::RestrictedToken,
            ),
            "Codex home",
        ),
        (
            preflight(
                &root,
                &root,
                PermissionProfile::read_only(),
                &root,
                Some(&invalid),
                std::slice::from_ref(&root),
                WindowsSandboxLevel::RestrictedToken,
            ),
            "Windows sandbox wrapper",
        ),
        (
            preflight(
                &root,
                &root,
                PermissionProfile::read_only(),
                &root,
                Some(&wrapper),
                std::slice::from_ref(&invalid),
                WindowsSandboxLevel::RestrictedToken,
            ),
            "workspace root",
        ),
    ] {
        assert_eq!(
            reason,
            LocalSandboxUnavailableReason::UnrepresentableInput(input)
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_preparation_uses_trusted_wrapper_and_scrubbed_inner_environment() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = AbsolutePathBuf::from_absolute_path(directory.path()).expect("absolute root");
    let wrapper = root.join("codex.exe");
    let inner = root.join("bun.exe");
    let mut request = request(
        &root,
        selected(
            SandboxType::WindowsRestrictedToken,
            PermissionProfile::read_only(),
        ),
        LocalSandboxLaunchPolicy::Required,
    );
    request.command.program = inner.as_os_str().to_owned();
    request
        .command
        .env
        .insert("SAFE_ENV".into(), "safe-value".into());
    request
        .runtime
        .direct_spawn
        .windows_sandbox_wrapper_executable = Some(&wrapper);
    request.runtime.windows_sandbox_level = WindowsSandboxLevel::RestrictedToken;
    let LocalSandboxPreparation::Prepared(prepared) =
        prepare_local_sandbox_command(request).expect("prepare Windows sandbox command")
    else {
        panic!("expected prepared Windows sandbox command");
    };
    assert_eq!(prepared.sandbox(), SandboxType::WindowsRestrictedToken);
    let command = prepared.into_command();
    assert_eq!(command.get_program(), wrapper.as_path());
    let args = command
        .get_args()
        .map(|argument| argument.to_str().expect("Unicode wrapper argument"))
        .collect::<Vec<_>>();
    assert!(args.contains(&"--preserve-proxy-settings"));
    let environment = args
        .windows(2)
        .find_map(|args| (args[0] == "--env-json").then_some(args[1]))
        .expect("inner environment argument");
    assert_eq!(
        serde_json::from_str::<HashMap<String, String>>(environment)
            .expect("deserialize inner environment"),
        HashMap::from([("SAFE_ENV".to_string(), "safe-value".to_string())])
    );
    let separator = args.iter().position(|arg| *arg == "--").expect("separator");
    assert_eq!(
        &args[separator + 1..],
        &[
            inner.as_path().to_str().expect("Unicode inner path"),
            "argument"
        ]
    );
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
