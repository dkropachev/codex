use std::collections::HashMap;
use std::ffi::OsString;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;

use crate::SandboxExecRequest;
use crate::SandboxType;

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
    let command = request(
        vec![
            "tool".to_string(),
            "".to_string(),
            "two words".to_string(),
            "λ".to_string(),
        ],
        PathUri::from_abs_path(&cwd),
    )
    .into_std_command()
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
    let error = request(Vec::new(), PathUri::from_abs_path(&cwd))
        .into_std_command()
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
    let error = request
        .into_std_command()
        .expect_err("sensitive environment must fail");
    assert_eq!(
        error.to_string(),
        "prepared sandbox environment contains a non-inheritable variable"
    );
}

#[test]
fn conversion_rejects_unapplied_managed_network_environment() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let mut request = request(vec!["tool".to_string()], PathUri::from_abs_path(&cwd));
    request.network_environment_id = Some("environment".to_string());
    let error = request
        .into_std_command()
        .expect_err("managed network must fail");
    assert_eq!(
        error.to_string(),
        "managed network environment must be applied before command conversion"
    );
}

#[test]
fn conversion_rejects_foreign_host_cwd() {
    #[cfg(unix)]
    let cwd = PathUri::parse("file:///C:/foreign").expect("Windows URI");
    #[cfg(windows)]
    let cwd = PathUri::parse("file:///tmp/foreign").expect("POSIX URI");
    let error = request(vec!["tool".to_string()], cwd)
        .into_std_command()
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
    let error = request
        .into_std_command()
        .expect_err("native Windows request must fail");
    assert_eq!(
        error.to_string(),
        "native Windows sandbox request must be wrapped before command conversion"
    );
}

#[cfg(unix)]
#[test]
fn converted_command_clears_ambient_environment() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let output = request(
        vec!["/usr/bin/env".to_string()],
        PathUri::from_abs_path(&cwd),
    )
    .into_std_command()
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
    let output = request
        .into_std_command()
        .expect("convert sandbox request")
        .output()
        .expect("run command");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"sandbox-helper");
}
