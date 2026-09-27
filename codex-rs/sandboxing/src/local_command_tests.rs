use std::collections::HashMap;
use std::ffi::OsString;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;

use super::command_from_direct_spawn_request;
use super::validate_direct_spawn_state;
use crate::SandboxCommand;
use crate::SandboxDirectSpawnRuntime;
use crate::SandboxDirectSpawnTransformRequest;
use crate::SandboxExecRequest;
use crate::SandboxManager;
use crate::SandboxTransformRequest;
use crate::SandboxType;
use crate::WindowsSandboxProxySettingsMode;

fn request(command: Vec<String>, cwd: PathUri) -> SandboxExecRequest {
    SandboxExecRequest {
        command,
        cwd: cwd.clone(),
        sandbox_policy_cwd: cwd,
        env: HashMap::from([("VISIBLE".to_string(), "yes".to_string())]),
        network: None,
        network_environment_id: None,
        sandbox: SandboxType::None,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        permission_profile: PermissionProfile::Disabled,
        arg0: None,
    }
}

#[test]
fn conversion_preserves_argv_cwd_and_only_inheritable_environment() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let permissions = PermissionProfile::Disabled;
    let command = SandboxManager::new()
        .prepare_command_for_direct_spawn_with_runtime(
            SandboxDirectSpawnTransformRequest {
                transform: SandboxTransformRequest {
                    command: SandboxCommand {
                        program: "tool".into(),
                        args: vec!["".to_string(), "two words".to_string(), "λ".to_string()],
                        cwd: cwd_uri.clone(),
                        env: HashMap::from([("VISIBLE".to_string(), "yes".to_string())]),
                        managed_network: None,
                        additional_permissions: None,
                    },
                    permissions: &permissions,
                    sandbox: SandboxType::None,
                    enforce_managed_network: false,
                    environment_id: None,
                    network: None,
                    sandbox_policy_cwd: &cwd_uri,
                    codex_linux_sandbox_exe: None,
                    use_legacy_landlock: false,
                    windows_sandbox_level: WindowsSandboxLevel::Disabled,
                    windows_sandbox_private_desktop: false,
                },
                workspace_roots: std::slice::from_ref(&cwd),
                windows_sandbox_proxy_settings_mode: WindowsSandboxProxySettingsMode::Preserve,
            },
            SandboxDirectSpawnRuntime {
                codex_home: &cwd,
                windows_sandbox_wrapper_executable: None,
            },
        )
        .expect("convert sandbox request");

    assert_eq!(command.get_program(), "tool");
    assert_eq!(
        command.get_args().map(OsString::from).collect::<Vec<_>>(),
        ["", "two words", "λ"].map(OsString::from).to_vec()
    );
    assert_eq!(command.get_current_dir(), Some(cwd.as_path()));
    assert_eq!(
        command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(OsString::from)))
            .collect::<Vec<_>>(),
        vec![("VISIBLE".into(), Some("yes".into()))]
    );
}

#[test]
fn conversion_rejects_empty_argv() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let error =
        command_from_direct_spawn_request(request(Vec::new(), PathUri::from_abs_path(&cwd)))
            .expect_err("empty argv must fail");
    assert_eq!(
        error.to_string(),
        "sandbox command was empty after preparation"
    );
}

#[test]
fn conversion_rejects_sensitive_environment_after_transformation() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let mut request = request(vec!["tool".to_string()], PathUri::from_abs_path(&cwd));
    request.env.insert(
        "OPENAI_IDENTITY_TOKEN_FILE".to_string(),
        "secret".to_string(),
    );
    let error =
        command_from_direct_spawn_request(request).expect_err("sensitive environment must fail");
    assert_eq!(
        error.to_string(),
        "prepared sandbox environment contains a non-inheritable variable"
    );
}

#[test]
fn conversion_rejects_unapplied_managed_network_environment() {
    for (has_network, has_environment_id) in [(true, false), (false, true)] {
        let error = validate_direct_spawn_state(
            SandboxType::None,
            has_network,
            has_environment_id,
            &HashMap::new(),
        )
        .expect_err("managed network must fail");
        assert_eq!(
            error.to_string(),
            "managed network environment must be applied before command conversion"
        );
    }
}

#[test]
fn conversion_rejects_foreign_host_cwd() {
    #[cfg(unix)]
    let cwd = PathUri::parse("file:///C:/foreign").expect("Windows URI");
    #[cfg(windows)]
    let cwd = PathUri::parse("file:///tmp/foreign").expect("POSIX URI");
    let error = command_from_direct_spawn_request(request(vec!["tool".to_string()], cwd))
        .expect_err("foreign cwd must fail");
    assert_eq!(
        error.to_string(),
        "prepared sandbox cwd is not valid on this host"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn conversion_rejects_unwrapped_native_windows_sandbox_request() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let mut request = request(vec!["tool".to_string()], PathUri::from_abs_path(&cwd));
    request.sandbox = SandboxType::WindowsRestrictedToken;
    let error =
        command_from_direct_spawn_request(request).expect_err("native Windows request must fail");
    assert_eq!(
        error.to_string(),
        "native Windows sandbox request must be wrapped before command conversion"
    );
}

#[cfg(unix)]
#[test]
fn converted_command_clears_ambient_environment() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let output = command_from_direct_spawn_request(request(
        vec!["/usr/bin/env".to_string()],
        PathUri::from_abs_path(&cwd),
    ))
    .expect("convert sandbox request")
    .output()
    .expect("run environment command");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        "VISIBLE=yes\n"
    );
}

#[cfg(unix)]
#[test]
fn conversion_preserves_unix_arg0() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let mut request = request(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf %s \"$0\"".to_string(),
        ],
        PathUri::from_abs_path(&cwd),
    );
    request.arg0 = Some("sandbox-helper".to_string());
    let output = command_from_direct_spawn_request(request)
        .expect("convert sandbox request")
        .output()
        .expect("run command");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"sandbox-helper");
}

#[cfg(target_os = "windows")]
#[test]
fn public_atomic_preparation_wraps_windows_restricted_command() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cwd = AbsolutePathBuf::from_absolute_path(directory.path()).expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let wrapper = cwd.join("codex.exe");
    let inner = cwd.join("bun.exe");
    let permissions = PermissionProfile::read_only();
    let launch_request = || SandboxDirectSpawnTransformRequest {
        transform: SandboxTransformRequest {
            command: SandboxCommand {
                program: inner.as_os_str().to_owned(),
                args: vec!["install".to_string()],
                cwd: cwd_uri.clone(),
                env: HashMap::new(),
                managed_network: None,
                additional_permissions: None,
            },
            permissions: &permissions,
            sandbox: SandboxType::WindowsRestrictedToken,
            enforce_managed_network: false,
            environment_id: None,
            network: None,
            sandbox_policy_cwd: &cwd_uri,
            codex_linux_sandbox_exe: None,
            use_legacy_landlock: false,
            windows_sandbox_level: WindowsSandboxLevel::RestrictedToken,
            windows_sandbox_private_desktop: false,
        },
        workspace_roots: std::slice::from_ref(&cwd),
        windows_sandbox_proxy_settings_mode: WindowsSandboxProxySettingsMode::Preserve,
    };
    let manager = SandboxManager::new();
    let error = manager
        .prepare_command_for_direct_spawn(launch_request())
        .expect_err("restricted launch without runtime must fail");
    assert_eq!(
        error.to_string(),
        "Windows restricted direct spawn requires explicit trusted runtime paths"
    );
    let command = manager
        .prepare_command_for_direct_spawn_with_runtime(
            launch_request(),
            SandboxDirectSpawnRuntime {
                codex_home: &cwd,
                windows_sandbox_wrapper_executable: Some(&wrapper),
            },
        )
        .expect("prepare Windows command");
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let separator = args.iter().position(|arg| arg == "--").expect("separator");
    assert_eq!(command.get_program(), wrapper.as_path());
    assert_eq!(
        &args[separator + 1..],
        &[inner.display().to_string(), "install".to_string()]
    );
}
