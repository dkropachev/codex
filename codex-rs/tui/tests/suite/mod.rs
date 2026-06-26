// Aggregates all former standalone integration tests as modules.
#[path = "account_pool__live.rs"]
mod account_pool_live;
#[path = "mcp__live.rs"]
mod mcp_live;
#[path = "plugins__live.rs"]
mod plugins_live;
mod resize_reflow;
#[path = "skills__live.rs"]
mod skills_live;
mod status_indicator;
mod vt100_history;
mod vt100_live_commit;
#[path = "workflows__mode.rs"]
mod workflow_mode;
#[path = "workflows__slash_autocomplete.rs"]
mod workflow_slash_autocomplete;
