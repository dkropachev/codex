use std::path::Path;
use std::process::ExitStatus;

use codex_plugin::PluginCommand;
pub use codex_plugin::PluginCommandSurface;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommandRunOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginCommandRunOptions {
    pub runtime_socket: Option<String>,
}

pub async fn run_plugin_command(
    command: &PluginCommand,
    invocation_args: &[String],
    cwd: &Path,
) -> std::io::Result<PluginCommandRunOutput> {
    run_plugin_command_with_options(
        command,
        invocation_args,
        cwd,
        &PluginCommandRunOptions::default(),
    )
    .await
}

pub async fn run_plugin_command_with_options(
    command: &PluginCommand,
    invocation_args: &[String],
    cwd: &Path,
    options: &PluginCommandRunOptions,
) -> std::io::Result<PluginCommandRunOutput> {
    tokio::fs::create_dir_all(command.state_dir.as_path()).await?;
    let mut process = Command::new(command.program.as_program_str());
    process
        .args(&command.args)
        .args(invocation_args)
        .current_dir(cwd)
        .env("CODEX_PLUGIN_ID", &command.plugin_id)
        .env("CODEX_PLUGIN_ROOT", command.plugin_root.as_path())
        .env("CODEX_PLUGIN_STATE_DIR", command.state_dir.as_path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(runtime_socket) = options.runtime_socket.as_ref() {
        process.env("CODEX_PLUGIN_RUNTIME_SOCKET", runtime_socket);
    }

    let output = process.output().await?;
    Ok(PluginCommandRunOutput {
        exit_code: exit_code(output.status),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

pub fn find_plugin_command<'a>(
    commands: &'a [PluginCommand],
    plugin_id: Option<&str>,
    surface: PluginCommandSurface,
    name: &str,
) -> Option<&'a PluginCommand> {
    commands.iter().find(|command| {
        plugin_id.is_none_or(|plugin_id| command.plugin_id == plugin_id)
            && command.name == name
            && command.surfaces.contains(&surface)
    })
}

fn exit_code(status: ExitStatus) -> Option<i32> {
    status.code()
}

#[cfg(test)]
mod tests {
    use codex_plugin::PluginCommandProgram;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn command_receives_plugin_runtime_socket_environment() {
        let temp = tempdir().expect("tempdir");
        let plugin_root =
            AbsolutePathBuf::try_from(temp.path().join("plugin")).expect("absolute plugin root");
        let state_dir =
            AbsolutePathBuf::try_from(temp.path().join("state")).expect("absolute state dir");
        let command = PluginCommand {
            plugin_id: "sample@test".to_string(),
            plugin_root,
            state_dir,
            name: "sample".to_string(),
            description: "Sample command".to_string(),
            program: shell_program(),
            args: shell_args("printf '%s' \"$CODEX_PLUGIN_RUNTIME_SOCKET\""),
            surfaces: vec![PluginCommandSurface::Cli],
            usage: None,
        };
        let options = PluginCommandRunOptions {
            runtime_socket: Some("runtime.sock".to_string()),
        };

        let output = run_plugin_command_with_options(&command, &[], temp.path(), &options)
            .await
            .expect("run command");

        assert_eq!(
            output,
            PluginCommandRunOutput {
                exit_code: Some(0),
                stdout: "runtime.sock".to_string(),
                stderr: String::new(),
            }
        );
    }

    #[cfg(not(windows))]
    fn shell_program() -> PluginCommandProgram {
        PluginCommandProgram::PathSearch("sh".to_string())
    }

    #[cfg(not(windows))]
    fn shell_args(script: &str) -> Vec<String> {
        vec!["-c".to_string(), script.to_string()]
    }

    #[cfg(windows)]
    fn shell_program() -> PluginCommandProgram {
        PluginCommandProgram::PathSearch("cmd".to_string())
    }

    #[cfg(windows)]
    fn shell_args(_script: &str) -> Vec<String> {
        vec![
            "/C".to_string(),
            "echo|set /p=%CODEX_PLUGIN_RUNTIME_SOCKET%".to_string(),
        ]
    }
}
