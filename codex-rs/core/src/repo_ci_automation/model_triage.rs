use std::sync::Arc;

use anyhow::Result;
use codex_protocol::error::CodexErr;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use tracing::warn;

use super::TRIAGE_BASE_INSTRUCTIONS;
use super::TriageInput;
use super::TriageResult;
use super::append_response_item_text;
use super::parse_triage_result;
use super::triage_output_schema;
use super::triage_prompt_text;
use crate::Prompt;
use crate::ResponseEvent;
use crate::model_router::AvailableRouterModel;
use crate::model_router::ModelRouterAppliedRoute;
use crate::model_router::ModelRouterRouteExclusion;
use crate::model_router::ModelRouterSource;
use crate::model_router::apply_model_router_with_state_and_exclusions;
use crate::model_router::auth_manager_for_config;
use crate::model_router::available_router_models;
use crate::model_router::model_router_failure_scope;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

pub(super) async fn run_model_triage(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    input: &TriageInput<'_>,
) -> Result<TriageResult> {
    let triage_prompt = triage_prompt_text(input);
    let available_models = available_router_models(&sess.services.models_manager);
    let base_config = turn_context.config.as_ref().clone();
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: triage_prompt.clone(),
            }],
            phase: None,
        }],
        base_instructions: BaseInstructions {
            text: TRIAGE_BASE_INSTRUCTIONS.to_string(),
        },
        output_schema: Some(triage_output_schema()),
        output_schema_strict: true,
        ..Default::default()
    };
    let mut exclusions = Vec::new();

    loop {
        let (policy_config, route) = repo_ci_phase_route_from_base(
            base_config.clone(),
            ModelRouterSource::Module("repo_ci.triage"),
            triage_prompt.len(),
            &available_models,
            sess.services.state_db.as_deref(),
            &exclusions,
        )
        .await;
        if route.is_none() && !exclusions.is_empty() {
            anyhow::bail!("repo CI model router has no eligible failover route");
        }
        match run_model_triage_attempt(sess, turn_context, &policy_config, &prompt).await {
            Ok((output, model_used)) => {
                let mut triage = parse_triage_result(&output)?;
                triage.model_used = Some(model_used);
                return Ok(triage);
            }
            Err(err) => {
                let Some(route) = route.as_ref() else {
                    return Err(err.into());
                };
                let Some(scope) = model_router_failure_scope(&err) else {
                    return Err(err.into());
                };
                let exclusion = route.exclusion_for_failure(scope);
                if exclusions.contains(&exclusion) {
                    return Err(err.into());
                }
                warn!(
                    error = %err,
                    task_key = route.task_key.as_str(),
                    scope = ?scope,
                    exclusion = ?exclusion,
                    "repo CI model router route failed; trying next eligible route"
                );
                exclusions.push(exclusion);
            }
        }
    }
}

async fn repo_ci_phase_route_from_base(
    mut config: crate::config::Config,
    model_router_source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    state_db: Option<&codex_state::StateRuntime>,
    exclusions: &[ModelRouterRouteExclusion],
) -> (crate::config::Config, Option<ModelRouterAppliedRoute>) {
    let route = match apply_model_router_with_state_and_exclusions(
        &mut config,
        model_router_source,
        prompt_bytes,
        available_models,
        state_db,
        exclusions,
    )
    .await
    {
        Ok(route) => route,
        Err(err) => {
            warn!("failed to apply repo CI model router: {err}");
            None
        }
    };
    (config, route)
}

async fn run_model_triage_attempt(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    policy_config: &crate::config::Config,
    prompt: &Prompt,
) -> std::result::Result<(String, String), CodexErr> {
    let model = policy_config
        .model
        .clone()
        .unwrap_or_else(|| turn_context.model_info.slug.clone());
    let model_info = if policy_config.model.as_deref()
        != Some(turn_context.model_info.slug.as_str())
        || policy_config.model_provider_id != turn_context.config.model_provider_id
    {
        sess.services
            .models_manager
            .get_model_info(&model, &policy_config.to_models_manager_config())
            .await
    } else {
        turn_context.model_info.clone()
    };
    let effort = policy_config
        .model_reasoning_effort
        .or(turn_context.reasoning_effort)
        .or(model_info.default_reasoning_level);

    let routed_auth_manager = auth_manager_for_config(policy_config, &sess.services.auth_manager);
    let routed_model_client = sess.services.model_client.with_provider_info(
        policy_config.model_provider.clone(),
        Some(routed_auth_manager),
    );
    let mut client_session = routed_model_client.new_session();
    let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
    let mut stream = client_session
        .stream(
            prompt,
            &model_info,
            &turn_context.session_telemetry,
            effort,
            turn_context.reasoning_summary,
            policy_config.service_tier,
            turn_metadata_header.as_deref(),
            &InferenceTraceContext::disabled(),
        )
        .await?;
    let mut output = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::OutputTextDelta(delta) => output.push_str(&delta),
            ResponseEvent::OutputItemDone(item) => append_response_item_text(&mut output, &item),
            ResponseEvent::Completed { .. } => break,
            ResponseEvent::Created
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::ServerModel(_)
            | ResponseEvent::ModelVerifications(_)
            | ResponseEvent::ServerReasoningIncluded(_)
            | ResponseEvent::ToolCallInputDelta { .. }
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::ReasoningSummaryPartAdded { .. }
            | ResponseEvent::RateLimits(_)
            | ResponseEvent::ModelsEtag(_) => {}
        }
    }
    Ok((output, model_info.slug))
}
