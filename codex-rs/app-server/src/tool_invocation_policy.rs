use codex_core::prompt_context::ToolInvocationPolicyInput;
use codex_core::prompt_context::ToolPolicy;
use std::borrow::Cow;

pub(crate) fn command_exec_denial(policy: &ToolPolicy, command: &[String]) -> Option<String> {
    let raw_input = codex_shell_command::parse_command::shlex_join(command);
    let json_input = serde_json::json!({
        "command": command,
    });
    policy.deny_invocation(ToolInvocationPolicyInput {
        tool_name: "command/exec",
        mcp_server: None,
        mcp_tool: None,
        raw_input: Cow::Owned(raw_input),
        json_input: Some(&json_input),
    })
}
