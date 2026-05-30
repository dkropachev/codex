use crate::session::session::Session;
use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;
use codex_mcp::ToolInfo;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

const MAX_TOOL_GROUPS: usize = 8;
const MAX_TOOLS_PER_GROUP: usize = 8;
const MAX_DESCRIPTION_CHARS: usize = 160;
const MCP_CONTEXT_INTRO: &str = concat!(
    "Codex backend has MCP integrations available for delegated work. Do not claim an app or ",
    "MCP integration is unavailable only because it is absent from the realtime tool list."
);
const MCP_CONTEXT_SEARCH_HINT: &str = concat!(
    "MCP tools may be directly exposed or lazy-loaded through `tool_search` depending on the ",
    "active model and tool-router configuration."
);
const EMPTY_MCP_INVENTORY_CONTEXT: &str = concat!(
    "Codex backend has MCP integrations configured for delegated work, but no ready tool ",
    "inventory was available at realtime startup. Do not claim app or MCP integrations are ",
    "unavailable only because they are absent from the realtime tool list; backend can refresh ",
    "inventory during delegated work."
);

#[derive(Default)]
struct ToolGroup {
    description: Option<String>,
    tools: Vec<String>,
}

#[expect(
    clippy::await_holding_invalid_type,
    reason = "realtime startup context reads cached MCP inventory through the session-owned manager guard"
)]
pub(crate) async fn build_realtime_tool_context(sess: &Session) -> Option<String> {
    let config = sess.get_config().await;
    let configured_servers = config
        .mcp_servers
        .get()
        .iter()
        .filter(|(_, server_config)| server_config.enabled)
        .map(|(server_name, _)| server_name.clone())
        .collect::<Vec<_>>();
    let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
    if !mcp_connection_manager.has_servers() && configured_servers.is_empty() {
        return None;
    }
    let tools = mcp_connection_manager.list_all_tools().await;
    if tools.is_empty() && configured_servers.is_empty() {
        return Some(EMPTY_MCP_INVENTORY_CONTEXT.to_string());
    }
    format_realtime_tool_context(&tools, &configured_servers)
}

fn format_realtime_tool_context(
    tools: &HashMap<String, ToolInfo>,
    configured_servers: &[String],
) -> Option<String> {
    if tools.is_empty() && configured_servers.is_empty() {
        return None;
    }

    let mut groups = BTreeMap::<String, ToolGroup>::new();
    let mut tool_server_names = HashSet::<&str>::new();
    for tool in tools.values() {
        tool_server_names.insert(tool.server_name.as_str());
        let label = if tool.server_name == CODEX_APPS_MCP_SERVER_NAME {
            tool.connector_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(|name| format!("App `{name}`"))
                .unwrap_or_else(|| "Apps MCP server".to_string())
        } else {
            let server_name = &tool.server_name;
            format!("MCP server `{server_name}`")
        };
        let group = groups.entry(label).or_default();
        if group.description.is_none() {
            let description = tool
                .connector_description
                .as_deref()
                .or(tool.server_instructions.as_deref())
                .or(tool.tool.description.as_deref())
                .map(|description| description.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|description| !description.is_empty())
                .map(|description| {
                    if description.chars().count() <= MAX_DESCRIPTION_CHARS {
                        description
                    } else {
                        let mut truncated = description
                            .chars()
                            .take(MAX_DESCRIPTION_CHARS.saturating_sub(3))
                            .collect::<String>();
                        truncated.push_str("...");
                        truncated
                    }
                });
            group.description = description;
        }
        group.tools.push(tool.canonical_tool_name().display());
    }

    let mut lines = vec![
        MCP_CONTEXT_INTRO.to_string(),
        MCP_CONTEXT_SEARCH_HINT.to_string(),
    ];

    if !groups.is_empty() {
        lines.push(String::new());
        lines.push("Available MCP inventory:".to_string());
        let group_count = groups.len();
        for (index, (label, mut group)) in groups.into_iter().enumerate() {
            if index == MAX_TOOL_GROUPS {
                let remaining_group_count = group_count - MAX_TOOL_GROUPS;
                let plural = if remaining_group_count == 1 { "" } else { "s" };
                lines.push(format!(
                    "- ... {remaining_group_count} more MCP source{plural}"
                ));
                break;
            }
            group.tools.sort();
            group.tools.dedup();
            let tool_count = group.tools.len();
            let shown_tools = group
                .tools
                .iter()
                .take(MAX_TOOLS_PER_GROUP)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let plural = if tool_count == 1 { "" } else { "s" };
            let mut line = format!("- {label}: {tool_count} tool{plural}");
            if let Some(description) = group.description {
                line.push_str(&format!("; {description}"));
            }
            if !shown_tools.is_empty() {
                line.push_str(&format!("; examples: {shown_tools}"));
                if tool_count > MAX_TOOLS_PER_GROUP {
                    let remaining_tool_count = tool_count - MAX_TOOLS_PER_GROUP;
                    line.push_str(&format!(", ... {remaining_tool_count} more"));
                }
            }
            lines.push(line);
        }
    }

    let mut servers_without_tools = configured_servers
        .iter()
        .filter(|server_name| !tool_server_names.contains(server_name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    servers_without_tools.sort();
    servers_without_tools.dedup();
    if !servers_without_tools.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Configured MCP servers with no ready tool inventory at realtime startup: {}. The backend can refresh inventory during delegated work.",
            servers_without_tools.join(", ")
        ));
    }

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rmcp::model::JsonObject;
    use rmcp::model::Tool;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_tool_info(
        server_name: &str,
        callable_namespace: &str,
        callable_name: &str,
        tool_description: &str,
        connector_name: Option<&str>,
        connector_description: Option<&str>,
    ) -> ToolInfo {
        ToolInfo {
            server_name: server_name.to_string(),
            callable_name: callable_name.to_string(),
            callable_namespace: callable_namespace.to_string(),
            server_instructions: None,
            tool: Tool {
                name: callable_name.to_string().into(),
                title: None,
                description: Some(tool_description.to_string().into()),
                input_schema: Arc::new(JsonObject::default()),
                output_schema: None,
                annotations: None,
                execution: None,
                icons: None,
                meta: None,
            },
            connector_id: None,
            connector_name: connector_name.map(str::to_string),
            plugin_display_names: Vec::new(),
            connector_description: connector_description.map(str::to_string),
        }
    }

    #[test]
    fn realtime_tool_context_lists_mcp_servers_and_apps() {
        let tools = HashMap::from([
            (
                "mcp__docs__search".to_string(),
                test_tool_info(
                    "docs",
                    "mcp__docs__",
                    "search",
                    "Search private docs.",
                    /*connector_name*/ None,
                    /*connector_description*/ None,
                ),
            ),
            (
                "mcp__codex_apps__calendar_create_event".to_string(),
                test_tool_info(
                    CODEX_APPS_MCP_SERVER_NAME,
                    "mcp__codex_apps__calendar_",
                    "create_event",
                    "Create an event.",
                    Some("Calendar"),
                    Some("Manage calendar events."),
                ),
            ),
        ]);

        assert_eq!(
            format_realtime_tool_context(&tools, /*configured_servers*/ &[]).expect("tool context"),
            "Codex backend has MCP integrations available for delegated work. Do not claim an app or MCP integration is unavailable only because it is absent from the realtime tool list.\n\
MCP tools may be directly exposed or lazy-loaded through `tool_search` depending on the active model and tool-router configuration.\n\
\n\
Available MCP inventory:\n\
- App `Calendar`: 1 tool; Manage calendar events.; examples: mcp__codex_apps__calendar_create_event\n\
- MCP server `docs`: 1 tool; Search private docs.; examples: mcp__docs__search"
        );
    }

    #[test]
    fn realtime_tool_context_mentions_configured_servers_without_tools() {
        assert_eq!(
            format_realtime_tool_context(
                &HashMap::new(),
                &["docs".to_string(), "calendar".to_string()],
            )
            .expect("tool context"),
            "Codex backend has MCP integrations available for delegated work. Do not claim an app or MCP integration is unavailable only because it is absent from the realtime tool list.\n\
MCP tools may be directly exposed or lazy-loaded through `tool_search` depending on the active model and tool-router configuration.\n\
\n\
Configured MCP servers with no ready tool inventory at realtime startup: calendar, docs. The backend can refresh inventory during delegated work."
        );
    }

    #[test]
    fn realtime_tool_context_omits_empty_inventory() {
        assert_eq!(
            format_realtime_tool_context(&HashMap::new(), /*configured_servers*/ &[]),
            None
        );
    }
}
