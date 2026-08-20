// Aggregates all former standalone integration tests as modules.
#[path = "account_pool__ignore_user_config.rs"]
mod account_pool_ignore_user_config;
mod add_dir;
mod agents_md;
mod apply_patch;
mod approval_policy;
mod auth_env;
mod ephemeral;
mod hooks;
#[path = "mcp__required_exit.rs"]
mod mcp_required_exit;
mod originator;
mod output_schema;
mod prompt_stdin;
mod resume;
mod sandbox;
mod server_error_exit;
