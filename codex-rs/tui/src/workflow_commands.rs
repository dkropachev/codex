//! Workflow command discovery and invocation are owned by `codex-workflows`.
//!
//! Keep this re-export while TUI-internal call sites migrate so downstream users do not need to
//! depend on TUI for the workflow package contract.

pub use codex_workflows::WorkflowCommand;
pub use codex_workflows::WorkflowCommandOptionHint;
pub use codex_workflows::WorkflowInvocation;
pub use codex_workflows::WorkflowInvocationError;
pub use codex_workflows::build_hosted_workflow_invocation;
pub use codex_workflows::build_workflow_invocation;
#[allow(deprecated)]
pub use codex_workflows::build_workflow_shell_command;
pub use codex_workflows::discover_workflow_commands;
pub use codex_workflows::hosted_workflow_invocation_input;
pub use codex_workflows::workflow_invocation_input;
pub use codex_workflows::workflow_invocation_input_from_args;
