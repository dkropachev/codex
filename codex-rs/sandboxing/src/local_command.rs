use std::process::Command;

use anyhow::Context;
use anyhow::bail;

use crate::SandboxExecRequest;

impl SandboxExecRequest {
    /// Converts a transformed request into an unspawned standard-library command.
    ///
    /// The command has an exact, cleared environment but no stdio or process-tree configuration;
    /// callers retain ownership of execution, output bounds, and cancellation.
    pub fn into_std_command(self) -> anyhow::Result<Command> {
        let Self {
            command: argv,
            cwd,
            mut env,
            arg0,
            ..
        } = self;
        let Some((program, args)) = argv.split_first() else {
            bail!("sandbox command was empty after preparation");
        };
        let mut command = Command::new(program);
        #[cfg(unix)]
        if let Some(arg0) = arg0 {
            use std::os::unix::process::CommandExt;
            command.arg0(arg0);
        }
        #[cfg(not(unix))]
        let _ = arg0;
        command.args(args);
        let cwd = cwd
            .to_abs_path()
            .context("prepared sandbox cwd is not valid on this host")?;
        command.current_dir(cwd.as_path());
        env.retain(|name, _| !codex_protocol::shell_environment::is_non_inheritable_env_var(name));
        command.env_clear().envs(env);
        Ok(command)
    }
}

#[cfg(test)]
#[path = "local_command_tests.rs"]
mod tests;
