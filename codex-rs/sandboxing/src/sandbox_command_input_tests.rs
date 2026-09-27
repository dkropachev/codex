use std::collections::HashMap;
use std::ffi::OsString;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn command(root: &AbsolutePathBuf) -> LocalProcessCommand {
    LocalProcessCommand {
        program: "tool".into(),
        args: ["", "two words", "lambda: λ"].map(OsString::from).to_vec(),
        cwd: root.clone(),
        env: HashMap::from([
            ("Visible".into(), "yes".into()),
            ("OPENAI_IDENTITY_TOKEN_FILE".into(), "secret\0".into()),
        ]),
    }
}

fn error(command: LocalProcessCommand) -> SandboxCommandInputError {
    prepare_sandbox_command(command).expect_err("sandbox input should be rejected")
}

#[test]
fn conversion_preserves_safe_fields_and_scrubs_protected_environment() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let command = prepare_sandbox_command(command(&root)).expect("prepare sandbox command");
    #[cfg(windows)]
    let expected_name = "VISIBLE";
    #[cfg(not(windows))]
    let expected_name = "Visible";

    assert_eq!(command.program, OsString::from("tool"));
    assert_eq!(
        command.args,
        vec![
            "".to_string(),
            "two words".to_string(),
            "lambda: λ".to_string()
        ]
    );
    assert_eq!(command.cwd, PathUri::from_abs_path(&root));
    assert_eq!(
        command.env,
        HashMap::from([(expected_name.to_string(), "yes".to_string())])
    );
    assert_eq!(command.managed_network, None);
    assert_eq!(command.additional_permissions, None);
}

#[test]
fn conversion_rejects_invalid_string_backed_inputs() {
    let root = AbsolutePathBuf::current_dir().expect("current directory");
    for (command, expected) in [
        (
            LocalProcessCommand {
                program: "".into(),
                ..command(&root)
            },
            "command program",
        ),
        (
            LocalProcessCommand {
                args: vec!["nul\0argument".into()],
                ..command(&root)
            },
            "command argument",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([("".into(), "value".into())]),
                ..command(&root)
            },
            "environment name",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([("PATH=shadow".into(), "value".into())]),
                ..command(&root)
            },
            "environment name",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([("NAME".into(), "nul\0value".into())]),
                ..command(&root)
            },
            "environment value",
        ),
    ] {
        assert_eq!(error(command).input(), expected);
    }
}

#[cfg(unix)]
#[test]
fn conversion_rejects_non_utf8_values_but_keeps_unix_names_case_sensitive() {
    use std::os::unix::ffi::OsStringExt;

    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let invalid = OsString::from_vec(vec![0xff]);
    for (command, expected) in [
        (
            LocalProcessCommand {
                program: invalid.clone(),
                ..command(&root)
            },
            "command program",
        ),
        (
            LocalProcessCommand {
                args: vec![invalid.clone()],
                ..command(&root)
            },
            "command argument",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([(invalid.clone(), "value".into())]),
                ..command(&root)
            },
            "environment name",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([("NAME".into(), invalid)]),
                ..command(&root)
            },
            "environment value",
        ),
    ] {
        assert_eq!(error(command).input(), expected);
    }

    let mut command = command(&root);
    command.env = HashMap::from([
        ("PATH".into(), "first".into()),
        ("Path".into(), "second".into()),
    ]);
    assert_eq!(
        prepare_sandbox_command(command)
            .expect("prepare case-sensitive Unix environment")
            .env,
        HashMap::from([
            ("PATH".to_string(), "first".to_string()),
            ("Path".to_string(), "second".to_string()),
        ])
    );
}

#[cfg(windows)]
#[test]
fn conversion_canonicalizes_windows_environment_names_and_rejects_ambiguity() {
    use std::os::windows::ffi::OsStringExt;

    let root = AbsolutePathBuf::current_dir().expect("current directory");
    let mut mixed_case = command(&root);
    mixed_case.env = HashMap::from([("Path".into(), "value".into())]);
    assert_eq!(
        prepare_sandbox_command(mixed_case)
            .expect("prepare canonical Windows environment")
            .env,
        HashMap::from([("PATH".to_string(), "value".to_string())])
    );

    let mut duplicate = command(&root);
    duplicate.env = HashMap::from([
        ("PATH".into(), "first".into()),
        ("Path".into(), "second".into()),
    ]);
    assert_eq!(error(duplicate).input(), "environment name");

    let mut non_ascii = command(&root);
    non_ascii.env = HashMap::from([("café".into(), "value".into())]);
    assert_eq!(error(non_ascii).input(), "environment name");

    let invalid = OsString::from_wide(&[0xd800]);
    for (command, expected) in [
        (
            LocalProcessCommand {
                program: invalid.clone(),
                ..command(&root)
            },
            "command program",
        ),
        (
            LocalProcessCommand {
                args: vec![invalid.clone()],
                ..command(&root)
            },
            "command argument",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([(invalid.clone(), "value".into())]),
                ..command(&root)
            },
            "environment name",
        ),
        (
            LocalProcessCommand {
                env: HashMap::from([("NAME".into(), invalid.clone())]),
                ..command(&root)
            },
            "environment value",
        ),
    ] {
        assert_eq!(error(command).input(), expected);
    }
}
