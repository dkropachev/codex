use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Command;

use codex_utils_absolute_path::AbsolutePathBuf;

/// Declarative host-local command input for explicit unrestricted execution.
///
/// This type does not imply trust. Callers must independently authorize unrestricted execution
/// before calling [`prepare_unrestricted_command`].
pub struct LocalProcessCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: AbsolutePathBuf,
    pub env: HashMap<String, String>,
}

/// Prepares an explicitly authorized unrestricted command without spawning it.
///
/// The returned command inherits no ambient environment and excludes Codex variables that must
/// never reach model-controlled children. Stdio and process-tree configuration remain unset so the
/// caller retains ownership of cancellation, output bounds, and execution.
pub fn prepare_unrestricted_command(request: LocalProcessCommand) -> Command {
    let LocalProcessCommand {
        program,
        args,
        cwd,
        mut env,
    } = request;
    env.retain(|name, _| !codex_protocol::shell_environment::is_non_inheritable_env_var(name));
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd.as_path())
        .env_clear()
        .envs(env);
    command
}

#[cfg(test)]
#[path = "unrestricted_command_tests.rs"]
mod tests;
