use codex_config::ConfigLayerStack;
use tokio::process::Command;

use crate::engine::ClaudeHooksEngine;
use crate::engine::CommandShell;
use crate::engine::PluginHookSource;
use crate::events::permission_request::PermissionRequestOutcome;
use crate::events::permission_request::PermissionRequestRequest;
use crate::events::post_tool_use::PostToolUseOutcome;
use crate::events::post_tool_use::PostToolUseRequest;
use crate::events::pre_tool_use::PreToolUseOutcome;
use crate::events::pre_tool_use::PreToolUseRequest;
use crate::events::session_start::SessionStartOutcome;
use crate::events::session_start::SessionStartRequest;
use crate::events::stop::StopOutcome;
use crate::events::stop::StopRequest;
use crate::events::user_prompt_submit::UserPromptSubmitOutcome;
use crate::events::user_prompt_submit::UserPromptSubmitRequest;
use crate::types::Hook;
use crate::types::HookEvent;
use crate::types::HookPayload;
use crate::types::HookResponse;

#[derive(Default, Clone)]
pub struct HooksConfig {
    pub legacy_notify_argv: Option<Vec<String>>,
    pub feature_enabled: bool,
    pub config_layer_stack: Option<ConfigLayerStack>,
    pub plugin_hook_sources: Vec<PluginHookSource>,
    pub shell_program: Option<String>,
    pub shell_args: Vec<String>,
}

#[derive(Clone)]
pub struct Hooks {
    after_agent: Vec<Hook>,
    after_tool_use: Vec<Hook>,
    engine: ClaudeHooksEngine,
}

impl Default for Hooks {
    fn default() -> Self {
        Self::new(HooksConfig::default())
    }
}

impl Hooks {
    pub fn new(config: HooksConfig) -> Self {
        let after_agent = config
            .legacy_notify_argv
            .filter(|argv| !argv.is_empty() && !argv[0].is_empty())
            .map(crate::notify_hook)
            .into_iter()
            .collect();
        let engine = ClaudeHooksEngine::new(
            config.feature_enabled,
            config.config_layer_stack.as_ref(),
            &config.plugin_hook_sources,
            CommandShell {
                program: config.shell_program.unwrap_or_default(),
                args: config.shell_args,
            },
        );
        Self {
            after_agent,
            after_tool_use: Vec::new(),
            engine,
        }
    }

    pub fn startup_warnings(&self) -> &[String] {
        self.engine.warnings()
    }

    fn hooks_for_event(&self, hook_event: &HookEvent) -> &[Hook] {
        match hook_event {
            HookEvent::AfterAgent { .. } => &self.after_agent,
            HookEvent::AfterToolUse { .. } => &self.after_tool_use,
        }
    }

    pub async fn dispatch(&self, hook_payload: HookPayload) -> Vec<HookResponse> {
        let hooks = self.hooks_for_event(&hook_payload.hook_event);
        let mut outcomes = Vec::with_capacity(hooks.len());
        for hook in hooks {
            let outcome = hook.execute(&hook_payload).await;
            let should_abort_operation = outcome.result.should_abort_operation();
            outcomes.push(outcome);
            if should_abort_operation {
                break;
            }
        }

        outcomes
    }

    pub fn preview_session_start(
        &self,
        request: &SessionStartRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_session_start(request)
    }

    pub fn preview_pre_tool_use(
        &self,
        request: &PreToolUseRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_pre_tool_use(request)
    }

    pub fn preview_permission_request(
        &self,
        request: &PermissionRequestRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_permission_request(request)
    }

    pub fn preview_post_tool_use(
        &self,
        request: &PostToolUseRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_post_tool_use(request)
    }

    pub async fn run_session_start(
        &self,
        request: SessionStartRequest,
        turn_id: Option<String>,
    ) -> SessionStartOutcome {
        self.engine.run_session_start(request, turn_id).await
    }

    pub async fn run_pre_tool_use(&self, request: PreToolUseRequest) -> PreToolUseOutcome {
        self.engine.run_pre_tool_use(request).await
    }

    pub async fn run_permission_request(
        &self,
        request: PermissionRequestRequest,
    ) -> PermissionRequestOutcome {
        self.engine.run_permission_request(request).await
    }

    pub async fn run_post_tool_use(&self, request: PostToolUseRequest) -> PostToolUseOutcome {
        self.engine.run_post_tool_use(request).await
    }

    pub fn preview_user_prompt_submit(
        &self,
        request: &UserPromptSubmitRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_user_prompt_submit(request)
    }

    pub async fn run_user_prompt_submit(
        &self,
        request: UserPromptSubmitRequest,
    ) -> UserPromptSubmitOutcome {
        self.engine.run_user_prompt_submit(request).await
    }

    pub fn preview_stop(
        &self,
        request: &StopRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_stop(request)
    }

    pub async fn run_stop(&self, request: StopRequest) -> StopOutcome {
        self.engine.run_stop(request).await
    }
}

pub fn command_from_argv(argv: &[String]) -> Option<Command> {
    let (program, args) = argv.split_first()?;
    if program.is_empty() {
        return None;
    }
    let mut command = Command::new(program);
    command.args(args);
    Some(command)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use codex_protocol::ThreadId;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;
    use crate::events::user_prompt_submit::UserPromptSubmitRequest;

    #[tokio::test]
    async fn plugin_hooks_receive_plugin_environment_and_metadata() {
        let temp = tempdir().expect("tempdir");
        let plugin_root = temp.path().join("plugin");
        let state_dir = temp.path().join("state");
        let hooks_path = plugin_root.join("hooks.json");
        fs::create_dir_all(&plugin_root).expect("create plugin root");
        fs::write(
            &hooks_path,
            r#"{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "printf '%s\n%s\n%s\n%s\n' \"$CODEX_PLUGIN_ID\" \"$CODEX_PLUGIN_ROOT\" \"$CODEX_PLUGIN_STATE_DIR\" \"$CODEX_PLUGIN_RUNTIME_SOCKET\" > \"$CODEX_PLUGIN_STATE_DIR/env.txt\""
          }
        ]
      }
    ]
  }
}"#,
        )
        .expect("write hooks");

        let hooks = Hooks::new(HooksConfig {
            feature_enabled: true,
            plugin_hook_sources: vec![PluginHookSource {
                plugin_id: "sample@test".to_string(),
                plugin_root: plugin_root.abs(),
                state_dir: state_dir.abs(),
                runtime_socket: Some("runtime.sock".to_string()),
                path: hooks_path.abs(),
            }],
            ..HooksConfig::default()
        });

        let preview = hooks.preview_user_prompt_submit(&UserPromptSubmitRequest {
            session_id: ThreadId::new(),
            turn_id: "turn-1".to_string(),
            cwd: AbsolutePathBuf::try_from(temp.path().to_path_buf()).expect("absolute cwd"),
            transcript_path: None,
            model: "model".to_string(),
            permission_mode: "default".to_string(),
            prompt: "hello".to_string(),
        });
        assert_eq!(preview[0].plugin_id, Some("sample@test".to_string()));

        let outcome = hooks
            .run_user_prompt_submit(UserPromptSubmitRequest {
                session_id: ThreadId::new(),
                turn_id: "turn-1".to_string(),
                cwd: AbsolutePathBuf::try_from(temp.path().to_path_buf()).expect("absolute cwd"),
                transcript_path: None,
                model: "model".to_string(),
                permission_mode: "default".to_string(),
                prompt: "hello".to_string(),
            })
            .await;

        assert_eq!(
            outcome.hook_events[0].run.plugin_id,
            Some("sample@test".to_string())
        );
        assert_eq!(
            fs::read_to_string(state_dir.join("env.txt")).expect("read env file"),
            format!(
                "sample@test\n{}\n{}\nruntime.sock\n",
                plugin_root.display(),
                state_dir.display()
            )
        );
    }
}
