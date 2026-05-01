use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::router::ToolRouter;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_state::ToolRouterGuidanceKey;
use codex_tools::TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP;
use codex_tools::compose_tool_router_guidance;
use codex_tools::strip_tool_router_static_guidelines;

pub(crate) async fn build_router_prompt_item(
    session: &Session,
    router: &ToolRouter,
    turn_context: &TurnContext,
) -> Option<ResponseItem> {
    let info = router.tool_router_prompt_info()?;
    let dynamic_guidance = if let Some(state_db) = session.services.state_db.as_deref() {
        let key = ToolRouterGuidanceKey {
            model_slug: turn_context.model_info.slug.clone(),
            model_provider: turn_context.config.model_provider_id.clone(),
            toolset_hash: info.toolset_hash.clone(),
            router_schema_version: info.router_schema_version,
        };
        match state_db.lookup_tool_router_guidance(&key).await {
            Ok(record) => record.map(|record| record.guidance_text),
            Err(err) => {
                tracing::warn!("failed to read tool_router guidance: {err}");
                None
            }
        }
    } else {
        None
    };
    let guidance = compose_tool_router_guidance(
        dynamic_guidance.as_deref(),
        TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP,
    );
    let guidance_version = if guidance.dynamic_guidance_accepted {
        "dynamic"
    } else {
        "default"
    };
    let text = format!(
        "{}\n<tool_router_guidance version=\"{}\" tokens=\"{}\">\n{}\n</tool_router_guidance>",
        info.format_description, guidance_version, guidance.tokens, guidance.text
    );

    Some(ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText { text }],
        phase: None,
    })
}

pub(crate) fn base_instructions_for_router_prompt(
    mut base_instructions: BaseInstructions,
    current_model_default_instructions: &str,
    router_enabled: bool,
) -> BaseInstructions {
    if router_enabled && base_instructions.text == current_model_default_instructions {
        base_instructions.text = strip_tool_router_static_guidelines(&base_instructions.text);
    }
    base_instructions
}

#[cfg(test)]
mod tests {
    use codex_protocol::models::BASE_INSTRUCTIONS_DEFAULT;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn strips_static_guidelines_only_when_router_enabled_and_default_matches() {
        let base = BaseInstructions {
            text: BASE_INSTRUCTIONS_DEFAULT.to_string(),
        };

        let stripped = base_instructions_for_router_prompt(
            base.clone(),
            BASE_INSTRUCTIONS_DEFAULT,
            /*router_enabled*/ true,
        );
        let not_router = base_instructions_for_router_prompt(
            base,
            BASE_INSTRUCTIONS_DEFAULT,
            /*router_enabled*/ false,
        );

        assert!(!stripped.text.contains("# Tool Guidelines"));
        assert!(not_router.text.contains("# Tool Guidelines"));
    }

    #[test]
    fn custom_instructions_are_not_rewritten() {
        let custom = BaseInstructions {
            text: "Custom instructions.\n# Tool Guidelines\n\nKeep this.".to_string(),
        };

        let result = base_instructions_for_router_prompt(
            custom.clone(),
            BASE_INSTRUCTIONS_DEFAULT,
            /*router_enabled*/ true,
        );

        assert_eq!(result, custom);
    }
}
