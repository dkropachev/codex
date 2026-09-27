use std::collections::HashMap;
use std::ffi::OsString;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn preparation_preserves_command_and_uses_only_explicit_safe_environment() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let command = prepare_unrestricted_command(LocalProcessCommand {
        program: "tool".into(),
        args: ["", "two words", "--flag", "λ"]
            .map(OsString::from)
            .to_vec(),
        cwd: cwd.clone(),
        env: HashMap::from([
            ("VISIBLE".to_string(), "yes".to_string()),
            (
                "OPENAI_IDENTITY_TOKEN_FILE".to_string(),
                "secret".to_string(),
            ),
        ]),
    });

    assert_eq!(command.get_program(), "tool");
    assert_eq!(
        command.get_args().map(OsString::from).collect::<Vec<_>>(),
        ["", "two words", "--flag", "λ"]
            .map(OsString::from)
            .to_vec()
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

#[cfg(unix)]
#[test]
fn preparation_preserves_non_utf8_program_and_arguments() {
    use std::os::unix::ffi::OsStringExt;

    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let program = OsString::from_vec(vec![b't', 0xff]);
    let argument = OsString::from_vec(vec![b'a', 0xff]);
    let command = prepare_unrestricted_command(LocalProcessCommand {
        program: program.clone(),
        args: vec![argument.clone()],
        cwd,
        env: HashMap::new(),
    });

    assert_eq!(command.get_program(), program);
    assert_eq!(command.get_args().collect::<Vec<_>>(), [&argument]);
}

#[cfg(unix)]
#[test]
fn prepared_command_does_not_inherit_ambient_environment() {
    let cwd = AbsolutePathBuf::current_dir().expect("current directory");
    let output = prepare_unrestricted_command(LocalProcessCommand {
        program: "/usr/bin/env".into(),
        args: Vec::new(),
        cwd,
        env: HashMap::from([("VISIBLE".to_string(), "yes".to_string())]),
    })
    .output()
    .expect("run environment command");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        "VISIBLE=yes\n"
    );
}
