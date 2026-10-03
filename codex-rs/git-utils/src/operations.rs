use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use codex_protocol::shell_environment::scrub_non_inheritable_env_vars;

use crate::GitToolingError;

const DISABLED_HOOKS_PATH: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Encodes Git overrides without treating equals signs in keys as separators.
pub fn git_config_override_env(
    overrides: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let overrides: Vec<_> = overrides.into_iter().collect();
    if overrides.is_empty() {
        return Vec::new();
    }
    let mut environment = vec![("GIT_CONFIG_COUNT".to_owned(), overrides.len().to_string())];
    for (index, (key, value)) in overrides.into_iter().enumerate() {
        environment.push((format!("GIT_CONFIG_KEY_{index}"), key));
        environment.push((format!("GIT_CONFIG_VALUE_{index}"), value));
    }
    environment
}
pub(crate) fn run_git_for_status<I, S>(
    dir: &Path,
    args: I,
    env: Option<&[(OsString, OsString)]>,
) -> Result<(), GitToolingError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let iterator = args.into_iter();
    let (lower, upper) = iterator.size_hint();
    let mut args_vec = Vec::with_capacity(upper.unwrap_or(lower) + 4);
    args_vec.push(OsString::from("-c"));
    args_vec.push(OsString::from(crate::SAFE_BARE_REPOSITORY_CONFIG));
    // Keep internal Git helper commands independent of configured hook directories.
    args_vec.push(OsString::from("-c"));
    args_vec.push(OsString::from(format!(
        "core.hooksPath={DISABLED_HOOKS_PATH}"
    )));
    for arg in iterator {
        args_vec.push(OsString::from(arg.as_ref()));
    }
    let command_string = build_command_string(&args_vec);
    let mut command = Command::new("git");
    command.current_dir(dir);
    if let Some(envs) = env {
        for (key, value) in envs {
            command.env(key, value);
        }
    }
    command.args(&args_vec);
    scrub_non_inheritable_env_vars(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(GitToolingError::GitCommand {
            command: command_string,
            status: output.status,
            stderr,
        });
    }
    Ok(())
}

fn build_command_string(args: &[OsString]) -> String {
    if args.is_empty() {
        return "git".to_string();
    }
    let joined = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    format!("git {joined}")
}
