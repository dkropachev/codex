use pretty_assertions::assert_eq;

use crate::create_list_dir_tool;
use crate::create_tool_router_tool;

use super::*;

#[test]
fn format_description_includes_required_shape_and_catalog() {
    let router = create_tool_router_tool();
    let description = tool_router_format_description(&router, &[create_list_dir_tool()]);

    assert!(description.contains("<tool_router_format>"));
    assert!(description.contains("`request`"));
    assert!(description.contains("`where.kind`"));
    assert!(description.contains("`action.kind`"));
    assert!(description.contains("`list_dir`"));
}

#[test]
fn guidance_cap_rejects_dynamic_guidance_without_dropping_default() {
    let dynamic = "Prefer a very specific route for this model and toolset.".repeat(80);
    let guidance = compose_tool_router_guidance(Some(&dynamic), 50);

    assert_eq!(
        guidance,
        ToolRouterGuidanceComposition {
            text: TOOL_ROUTER_DEFAULT_GUIDANCE.to_string(),
            tokens: estimate_router_text_tokens(TOOL_ROUTER_DEFAULT_GUIDANCE),
            dynamic_guidance_accepted: false,
        }
    );
}

#[test]
fn guidance_cap_accepts_short_dynamic_guidance() {
    let guidance = compose_tool_router_guidance(
        Some("Use `action.query` for search requests."),
        TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP,
    );

    assert!(guidance.dynamic_guidance_accepted);
    assert!(guidance.text.contains("Use `action.query`"));
    assert!(guidance.tokens <= TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP);
}

#[test]
fn strips_static_tool_guidelines_section() {
    let input = "Keep this.\n# Tool Guidelines\n\n## Shell commands\n\nold guidance";

    assert_eq!(strip_tool_router_static_guidelines(input), "Keep this.\n");
}

#[test]
fn strips_static_tool_guidelines_section_with_crlf_line_endings() {
    let input = "Keep this.\r\n# Tool Guidelines\r\n\r\n## Shell commands\r\n\r\nold guidance";

    assert_eq!(strip_tool_router_static_guidelines(input), "Keep this.\n");
}

#[test]
fn hard_guidance_cap_is_enforced() {
    assert!(validate_tool_router_guidance_cap(TOOL_ROUTER_HARD_GUIDANCE_TOKEN_CAP).is_ok());
    assert!(validate_tool_router_guidance_cap(TOOL_ROUTER_HARD_GUIDANCE_TOKEN_CAP + 1).is_err());
}
