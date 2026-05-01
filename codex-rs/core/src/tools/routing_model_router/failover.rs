use crate::function_tool::FunctionCallError;
use crate::model_router::ModelRouterRouteExclusion;
use crate::model_router::ModelRouterSource;
use crate::model_router::apply_model_router_with_exclusions;
use crate::model_router::available_router_models;
use crate::model_router::model_router_failure_scope;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_protocol::error::CodexErr;

use super::ModelRouterDecision;
use super::ModelRouterRouteUsage;
use super::model_router_decision;

pub(super) enum ModelRouterDecisionError {
    Codex {
        context: &'static str,
        err: CodexErr,
    },
    Function(FunctionCallError),
}

pub(super) async fn routed_model_router_decision(
    session: &Session,
    turn: &TurnContext,
    prompt_text: String,
) -> Result<(ModelRouterDecision, ModelRouterRouteUsage), FunctionCallError> {
    let available_models = available_router_models(&session.services.models_manager);
    let base_config = turn.config.as_ref().clone();
    let mut exclusions = Vec::<ModelRouterRouteExclusion>::new();

    loop {
        let mut routed_config = base_config.clone();
        let route = apply_model_router_with_exclusions(
            &mut routed_config,
            ModelRouterSource::Module("tool_router.resolve"),
            prompt_text.len(),
            &available_models,
            &exclusions,
        )
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to apply tool_router model router config: {err}"
            ))
        })?;
        if route.is_none() && !exclusions.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "tool_router model-router fallback has no eligible failover route".to_string(),
            ));
        }

        match model_router_decision(session, turn, prompt_text.clone(), &routed_config).await {
            Ok(result) => return Ok(result),
            Err(ModelRouterDecisionError::Codex { context, err }) => {
                let Some(route) = route.as_ref() else {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "{context}: {err}"
                    )));
                };
                let Some(scope) = model_router_failure_scope(&err) else {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "{context}: {err}"
                    )));
                };
                let exclusion = route.exclusion_for_failure(scope);
                if exclusions.contains(&exclusion) {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "{context}: {err}"
                    )));
                }
                tracing::warn!(
                    error = %err,
                    task_key = route.task_key.as_str(),
                    scope = ?scope,
                    exclusion = ?exclusion,
                    "tool_router model router route failed; trying next eligible route"
                );
                exclusions.push(exclusion);
            }
            Err(ModelRouterDecisionError::Function(err)) => return Err(err),
        }
    }
}
