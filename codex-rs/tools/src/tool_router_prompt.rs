use crate::ResponsesApiNamespaceTool;
use crate::TOOL_ROUTER_TOOL_NAME;
use crate::ToolSpec;

pub const TOOL_ROUTER_SCHEMA_VERSION: i64 = 1;
pub const TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION: i64 = 2;
pub const TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP: usize = 600;
pub const TOOL_ROUTER_HARD_GUIDANCE_TOKEN_CAP: usize = 1200;
pub const TOOL_ROUTER_MAX_CATALOG_TOOLS: usize = 64;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

pub const TOOL_ROUTER_DEFAULT_GUIDANCE: &str = "Use `tool_router` for routed calls. Prefer exact `action.tool` or deterministic `action.kind` with concrete `cmd`, `patch`, `query`, `session_id`, or `mcp_args`. For independent read-only work, use `action.kind=batch` with `action.commands`/`action.paths`.";

pub fn estimate_router_text_tokens(text: &str) -> usize {
    let non_ws_chars = text.chars().filter(|ch| !ch.is_whitespace()).count();
    let word_like_tokens = text.split_whitespace().count();
    word_like_tokens.max(non_ws_chars.div_ceil(4))
}

pub fn validate_tool_router_guidance_cap(cap: usize) -> Result<(), String> {
    if cap > TOOL_ROUTER_HARD_GUIDANCE_TOKEN_CAP {
        return Err(format!(
            "max guidance tokens must be <= {TOOL_ROUTER_HARD_GUIDANCE_TOKEN_CAP}"
        ));
    }
    Ok(())
}

pub fn compose_tool_router_guidance(
    dynamic_guidance: Option<&str>,
    cap: usize,
) -> ToolRouterGuidanceComposition {
    let default_guidance = TOOL_ROUTER_DEFAULT_GUIDANCE.trim();
    let default_tokens = estimate_router_text_tokens(default_guidance);

    let Some(dynamic_guidance) = dynamic_guidance
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return ToolRouterGuidanceComposition {
            text: default_guidance.to_string(),
            tokens: default_tokens,
            dynamic_guidance_accepted: false,
        };
    };

    let combined = format!("{default_guidance}\n{dynamic_guidance}");
    let combined_tokens = estimate_router_text_tokens(&combined);
    if combined_tokens <= cap {
        ToolRouterGuidanceComposition {
            text: combined,
            tokens: combined_tokens,
            dynamic_guidance_accepted: true,
        }
    } else {
        ToolRouterGuidanceComposition {
            text: default_guidance.to_string(),
            tokens: default_tokens,
            dynamic_guidance_accepted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterGuidanceComposition {
    pub text: String,
    pub tokens: usize,
    pub dynamic_guidance_accepted: bool,
}

pub fn tool_router_static_guidelines_tokens() -> usize {
    estimate_router_text_tokens(TOOL_ROUTER_STATIC_GUIDELINES)
}

pub fn strip_tool_router_static_guidelines(instructions: &str) -> String {
    let Some(start) = instructions
        .find("\n# Tool Guidelines\n")
        .or_else(|| instructions.find("\n# Tool Guidelines\r\n"))
    else {
        return instructions.to_string();
    };
    let mut stripped = instructions[..start].trim_end().to_string();
    stripped.push('\n');
    stripped
}

pub fn tool_router_format_description(router_spec: &ToolSpec, routed_tools: &[ToolSpec]) -> String {
    let mut lines = vec![
        "<tool_router_format>".to_string(),
        "Submit one JSON object: `request`, `where.kind`, `targets`, and `action.kind` are required; use `action.tool` for an exact internal tool.".to_string(),
        "Payload keys include `cmd`, `command`, `commands`, `paths`, `patch`, `query`, `session_id`, `mcp_args`, and `input`.".to_string(),
        "For independent read-only work, use `action.kind=batch` with `action.commands` or `action.paths`.".to_string(),
        format!("Schema version: {TOOL_ROUTER_SCHEMA_VERSION}; router: {}.", tool_summary(router_spec)),
        "Active routed tool catalog:".to_string(),
    ];

    let total_catalog_entries = routed_tools
        .iter()
        .map(tool_catalog_entry_count)
        .sum::<usize>();
    let mut appended_catalog_entries = 0usize;
    for spec in routed_tools {
        if appended_catalog_entries >= TOOL_ROUTER_MAX_CATALOG_TOOLS {
            break;
        }
        appended_catalog_entries += append_tool_catalog_lines(
            &mut lines,
            spec,
            TOOL_ROUTER_MAX_CATALOG_TOOLS - appended_catalog_entries,
        );
    }
    if total_catalog_entries > appended_catalog_entries {
        lines.push(format!(
            "- ... {} additional routed tools omitted; use `tool_search` when you need more.",
            total_catalog_entries - appended_catalog_entries
        ));
    }
    lines.push("</tool_router_format>".to_string());
    lines.join("\n")
}

pub fn toolset_hash_from_specs(specs: &[ToolSpec]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for spec in specs {
        let serialized = serde_json::to_string(spec).unwrap_or_else(|_| spec.name().to_string());
        hash = fnv1a_update(hash, serialized.as_bytes());
        hash = fnv1a_update(hash, b"\n");
    }
    format!("{hash:016x}")
}

fn append_tool_catalog_lines(lines: &mut Vec<String>, spec: &ToolSpec, remaining: usize) -> usize {
    if remaining == 0 {
        return 0;
    }

    match spec {
        ToolSpec::Function(tool) => {
            if tool.name != TOOL_ROUTER_TOOL_NAME {
                lines.push(format!(
                    "- `{}`: {}",
                    tool.name,
                    compact_description(&tool.description)
                ));
                1
            } else {
                0
            }
        }
        ToolSpec::Freeform(tool) => {
            lines.push(format!(
                "- `{}`: {}",
                tool.name,
                compact_description(&tool.description)
            ));
            1
        }
        ToolSpec::Namespace(namespace) => {
            let mut appended = 0usize;
            for tool in namespace.tools.iter().take(remaining) {
                match tool {
                    ResponsesApiNamespaceTool::Function(tool) => lines.push(format!(
                        "- `{}.{}`: {}",
                        namespace.name,
                        tool.name,
                        compact_description(&tool.description)
                    )),
                }
                appended += 1;
            }
            appended
        }
        ToolSpec::ToolSearch { description, .. } => {
            lines.push(format!(
                "- `tool_search`: {}",
                compact_description(description)
            ));
            1
        }
        ToolSpec::LocalShell {} => {
            lines.push("- `local_shell`: execute a local shell action.".to_string());
            1
        }
        ToolSpec::ImageGeneration { .. } => {
            lines.push("- `image_generation`: generate or edit bitmap images.".to_string());
            1
        }
        ToolSpec::WebSearch { .. } => {
            lines.push("- `web_search`: search the web.".to_string());
            1
        }
    }
}

fn tool_catalog_entry_count(spec: &ToolSpec) -> usize {
    match spec {
        ToolSpec::Function(tool) => usize::from(tool.name != TOOL_ROUTER_TOOL_NAME),
        ToolSpec::Namespace(namespace) => namespace.tools.len(),
        ToolSpec::Freeform(_)
        | ToolSpec::ToolSearch { .. }
        | ToolSpec::LocalShell {}
        | ToolSpec::ImageGeneration { .. }
        | ToolSpec::WebSearch { .. } => 1,
    }
}

fn tool_summary(spec: &ToolSpec) -> String {
    match spec {
        ToolSpec::Function(tool) => format!(
            "`{}` requires `request`, `where`, `targets`, and `action`",
            tool.name
        ),
        _ => format!("`{}`", spec.name()),
    }
}

fn compact_description(description: &str) -> String {
    let first_line = description.lines().next().unwrap_or_default().trim();
    if first_line.len() <= 120 {
        first_line.to_string()
    } else {
        let truncated = first_line.chars().take(120).collect::<String>();
        format!("{}...", truncated.trim_end())
    }
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

const TOOL_ROUTER_STATIC_GUIDELINES: &str = r#"# Tool Guidelines

## Shell commands

When using the shell, you must adhere to the following guidelines:

- When searching for text or files, prefer using `rg` or `rg --files` respectively because `rg` is much faster than alternatives like `grep`. (If the `rg` command is not found, then use alternatives.)
- Do not use python scripts to attempt to output larger chunks of a file.

## `update_plan`

A tool named `update_plan` is available to you. You can use it to keep an up-to-date, step-by-step plan for the task.

To create a new plan, call `update_plan` with a short list of 1-sentence steps (no more than 5-7 words each) with a `status` for each step (`pending`, `in_progress`, or `completed`).

When steps have been completed, use `update_plan` to mark each finished step as `completed` and the next step you are working on as `in_progress`. There should always be exactly one `in_progress` step until everything is done. You can mark multiple items as complete in a single `update_plan` call.

If all steps are complete, ensure you call `update_plan` to mark all steps as `completed`.
"#;

#[cfg(test)]
#[path = "tool_router_prompt_tests.rs"]
mod tests;
