use crate::error::ApiError;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use std::collections::HashMap;

pub(super) fn local_shell_response_item(
    call_id: String,
    arguments: &str,
) -> Result<ResponseItem, ApiError> {
    let action = chat_local_shell_action_from_arguments(arguments)?;
    Ok(ResponseItem::LocalShellCall {
        id: Some(call_id.clone()),
        call_id: Some(call_id),
        status: LocalShellStatus::InProgress,
        action,
    })
}

fn chat_local_shell_action_from_arguments(arguments: &str) -> Result<LocalShellAction, ApiError> {
    if let Ok(action) = serde_json::from_str::<LocalShellAction>(arguments) {
        return Ok(action);
    }

    let args: ChatLocalShellArguments = serde_json::from_str(arguments)
        .map_err(|err| ApiError::Stream(format!("failed to parse local_shell arguments: {err}")))?;
    let command = args
        .command
        .map(command_to_vec)
        .ok_or_else(|| ApiError::Stream("local_shell arguments are missing command".to_string()))?;
    Ok(LocalShellAction::Exec(LocalShellExecAction {
        command,
        timeout_ms: args.timeout_ms,
        working_directory: args.working_directory.or(args.workdir),
        env: args.env,
        user: args.user,
    }))
}

#[derive(Debug, Deserialize)]
struct ChatLocalShellArguments {
    command: Option<ChatLocalShellCommand>,
    timeout_ms: Option<u64>,
    working_directory: Option<String>,
    workdir: Option<String>,
    env: Option<HashMap<String, String>>,
    user: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChatLocalShellCommand {
    Argv(Vec<String>),
    Script(String),
}

fn command_to_vec(command: ChatLocalShellCommand) -> Vec<String> {
    match command {
        ChatLocalShellCommand::Argv(argv) => argv,
        ChatLocalShellCommand::Script(script) if cfg!(windows) => {
            vec!["powershell.exe".to_string(), "-Command".to_string(), script]
        }
        ChatLocalShellCommand::Script(script) => {
            vec!["bash".to_string(), "-lc".to_string(), script]
        }
    }
}
