use std::process::Command;

use anyhow::Context;
use anyhow::bail;

use crate::SandboxDirectSpawnRuntime;
use crate::SandboxDirectSpawnTransformRequest;
use crate::SandboxExecRequest;
use crate::SandboxManager;
use crate::SandboxType;

impl SandboxManager {
    /// Converts a transformed request into an unspawned standard-library command.
    pub fn prepare_command_for_direct_spawn(
        &self,
        request: SandboxDirectSpawnTransformRequest<'_>,
    ) -> anyhow::Result<Command> {
        #[cfg(target_os = "windows")]
        if request.transform.sandbox == SandboxType::WindowsRestrictedToken {
            bail!("Windows restricted direct spawn requires explicit trusted runtime paths");
        }
        command_from_direct_spawn_request(self.transform_for_direct_spawn(request)?)
    }

    /// Converts an arbitrary direct-spawn request using explicit trusted runtime paths.
    pub fn prepare_command_for_direct_spawn_with_runtime(
        &self,
        request: SandboxDirectSpawnTransformRequest<'_>,
        runtime: SandboxDirectSpawnRuntime<'_>,
    ) -> anyhow::Result<Command> {
        command_from_direct_spawn_request(
            self.transform_for_direct_spawn_with_runtime(request, runtime)?,
        )
    }
}

fn command_from_direct_spawn_request(request: SandboxExecRequest) -> anyhow::Result<Command> {
    let SandboxExecRequest {
        command: argv,
        cwd,
        env,
        arg0,
        sandbox,
        network,
        network_environment_id,
        ..
    } = request;
    validate_direct_spawn_state(
        sandbox,
        network.is_some(),
        network_environment_id.is_some(),
        &env,
    )?;
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
    command.env_clear().envs(env);
    Ok(command)
}

fn validate_direct_spawn_state(
    sandbox: SandboxType,
    has_network: bool,
    has_network_environment_id: bool,
    env: &std::collections::HashMap<String, String>,
) -> anyhow::Result<()> {
    if sandbox == SandboxType::WindowsRestrictedToken {
        bail!("native Windows sandbox request must be wrapped before command conversion");
    }
    if has_network || has_network_environment_id {
        bail!("managed network environment must be applied before command conversion");
    }
    if env
        .keys()
        .any(|name| codex_protocol::shell_environment::is_non_inheritable_env_var(name))
    {
        bail!("prepared sandbox environment contains a non-inheritable variable");
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_command_tests.rs"]
mod tests;
