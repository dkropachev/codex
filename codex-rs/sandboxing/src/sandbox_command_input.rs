use std::collections::HashMap;
use std::ffi::OsString;

use codex_utils_path_uri::PathUri;

use crate::LocalProcessCommand;
use crate::SandboxCommand;

/// Identifies an input that cannot cross the string-backed sandbox transport losslessly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SandboxCommandInputError {
    input: &'static str,
}

impl SandboxCommandInputError {
    fn new(input: &'static str) -> Self {
        Self { input }
    }

    pub fn input(&self) -> &'static str {
        self.input
    }
}

impl std::fmt::Display for SandboxCommandInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} cannot be represented by the sandbox transport",
            self.input
        )
    }
}

impl std::error::Error for SandboxCommandInputError {}

/// Losslessly converts a host-local command into string-backed sandbox input.
///
/// Non-inheritable variables are removed before their values are inspected so
/// protected launch context cannot leak into a platform wrapper's argv. On
/// Windows, ASCII environment names are canonicalized to uppercase because the
/// native environment namespace is case-insensitive while downstream maps are
/// not.
pub fn prepare_sandbox_command(
    command: LocalProcessCommand,
) -> Result<SandboxCommand, SandboxCommandInputError> {
    let LocalProcessCommand {
        program,
        args,
        cwd,
        mut env,
    } = command;
    env.retain(|name, _| {
        !name
            .to_str()
            .is_some_and(codex_protocol::shell_environment::is_non_inheritable_env_var)
    });
    let program = representable_string(program, "command program")?;
    if program.is_empty() {
        return Err(SandboxCommandInputError::new("command program"));
    }
    Ok(SandboxCommand {
        program: program.into(),
        args: args
            .into_iter()
            .map(|argument| representable_string(argument, "command argument"))
            .collect::<Result<_, _>>()?,
        cwd: PathUri::from_abs_path(&cwd),
        env: env.into_iter().try_fold(
            HashMap::<String, String>::new(),
            |mut prepared, (name, value)| {
                let name = environment_name(name)?;
                if prepared.contains_key(&name) {
                    return Err(SandboxCommandInputError::new("environment name"));
                }
                prepared.insert(name, representable_string(value, "environment value")?);
                Ok(prepared)
            },
        )?,
        managed_network: None,
        additional_permissions: None,
    })
}

fn representable_string(
    value: OsString,
    input: &'static str,
) -> Result<String, SandboxCommandInputError> {
    let value = value
        .into_string()
        .map_err(|_| SandboxCommandInputError::new(input))?;
    if value.contains('\0') {
        return Err(SandboxCommandInputError::new(input));
    }
    Ok(value)
}

fn environment_name(name: OsString) -> Result<String, SandboxCommandInputError> {
    let name = representable_string(name, "environment name")?;
    if name.is_empty() || name.contains('=') {
        return Err(SandboxCommandInputError::new("environment name"));
    }
    #[cfg(windows)]
    let name = {
        if !name.is_ascii() {
            return Err(SandboxCommandInputError::new("environment name"));
        }
        name.to_ascii_uppercase()
    };
    Ok(name)
}

#[cfg(test)]
#[path = "sandbox_command_input_tests.rs"]
mod tests;
