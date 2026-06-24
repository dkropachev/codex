// Aggregates all former standalone integration tests as modules.
#[path = "mcp__live.rs"]
mod mcp_live;
mod resize_reflow;
mod status_indicator;
mod vt100_history;
mod vt100_live_commit;
#[path = "workflows__mode.rs"]
mod workflow_mode;
#[path = "workflows__slash_autocomplete.rs"]
mod workflow_slash_autocomplete;
