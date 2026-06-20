mod account;
#[path = "account_pool__app_server_account.rs"]
mod account_pool_app_server_account;
mod analytics;
mod app_list;
mod attestation;
mod client_metadata;
mod collaboration_mode_list;
#[cfg(unix)]
mod command_exec;
mod compaction;
mod config_rpc;
mod connection_handling_websocket;
#[cfg(unix)]
mod connection_handling_websocket_unix;
mod dynamic_tools;
mod experimental_api;
mod experimental_feature_list;
mod external_agent_config;
mod fs;
mod hooks_list;
mod imagegen_extension;
mod initialize;
#[path = "mcp__resource.rs"]
mod mcp_resource;
#[path = "mcp__server_elicitation.rs"]
mod mcp_server_elicitation;
#[path = "mcp__server_status.rs"]
mod mcp_server_status;
#[path = "mcp__tool.rs"]
mod mcp_tool;
mod memory_reset;
mod model_list;
mod model_provider_capabilities_read;
mod output_schema;
mod permission_profile_list;
mod plan_item;
#[path = "plugins__install.rs"]
mod plugin_install;
#[path = "plugins__list.rs"]
mod plugin_list;
#[path = "plugins__read.rs"]
mod plugin_read;
#[path = "plugins__share.rs"]
mod plugin_share;
#[path = "plugins__uninstall.rs"]
mod plugin_uninstall;
#[path = "plugins__marketplace_add.rs"]
mod plugins_marketplace_add;
#[path = "plugins__marketplace_remove.rs"]
mod plugins_marketplace_remove;
#[path = "plugins__marketplace_upgrade.rs"]
mod plugins_marketplace_upgrade;
mod process_exec;
mod rate_limits;
mod realtime_conversation;
mod remote_control;
#[cfg(debug_assertions)]
mod remote_thread_store;
mod request_permissions;
mod request_user_input;
mod review;
mod safety_check_downgrade;
#[path = "skills__list.rs"]
mod skills_list;
mod thread_archive;
mod thread_fork;
mod thread_inject_items;
mod thread_list;
mod thread_loaded_list;
mod thread_memory_mode_set;
mod thread_metadata_update;
mod thread_name_websocket;
mod thread_read;
mod thread_resume;
mod thread_rollback;
mod thread_settings_update;
mod thread_shell_command;
mod thread_start;
mod thread_status;
mod thread_unarchive;
mod thread_unsubscribe;
#[cfg(unix)]
#[path = "workflows__thread_command.rs"]
mod thread_workflow_command;
mod turn_interrupt;
mod turn_start;
mod turn_start_zsh_fork;
mod turn_steer;
mod web_search;
mod windows_sandbox_setup;
