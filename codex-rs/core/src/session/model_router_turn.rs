use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use codex_protocol::user_input::UserInput;
use tracing::warn;

use super::session::Session;
use super::session::SessionConfiguration;
use super::turn_context::TurnContext;
use crate::config::Config;
use crate::model_router::ModelRouterSource;
use crate::model_router::apply_model_router_with_state_and_exclusions;
use crate::model_router::available_router_models;
use crate::model_router::config_account_pool_default;
use crate::skills_load_input_from_config;

impl Session {
    pub(crate) async fn model_router_prompt_bytes_for_turn(&self, input: &[UserInput]) -> usize {
        let history = self.clone_history().await;
        let history_bytes = history
            .raw_items()
            .iter()
            .filter_map(|item| serde_json::to_vec(item).ok())
            .map(|item| item.len())
            .sum::<usize>();
        let input_bytes = input
            .iter()
            .filter_map(|item| serde_json::to_vec(item).ok())
            .map(|item| item.len())
            .sum::<usize>();
        history_bytes + input_bytes + self.get_base_instructions().await.text.len()
    }

    pub(crate) async fn route_regular_turn_context_for_model_router(
        &self,
        turn_context: Arc<TurnContext>,
        input: &[UserInput],
    ) -> Arc<TurnContext> {
        let prompt_bytes = self.model_router_prompt_bytes_for_turn(input).await;
        let mode = turn_context.collaboration_mode.mode;
        self.route_turn_context_for_model_router(
            turn_context,
            ModelRouterSource::Chat(mode),
            prompt_bytes,
        )
        .await
    }

    pub(crate) async fn route_turn_context_for_model_router(
        &self,
        turn_context: Arc<TurnContext>,
        source: ModelRouterSource,
        prompt_bytes: usize,
    ) -> Arc<TurnContext> {
        let mut routed_config = turn_context.config.as_ref().clone();
        let previous_model = routed_config.model.clone();
        let previous_provider_id = routed_config.model_provider_id.clone();
        let previous_account_pool = config_account_pool_default(&routed_config);
        let previous_service_tier = routed_config.service_tier;
        let previous_reasoning_effort = routed_config.model_reasoning_effort;
        let had_accounting = routed_config.model_router_accounting.is_some();
        let task_key = source.task_key();
        let available_models = available_router_models(
            &routed_config,
            &self.services.models_manager,
            &self.services.model_router_discovery_cache,
        )
        .await;
        let route = match apply_model_router_with_state_and_exclusions(
            &mut routed_config,
            source,
            prompt_bytes,
            &available_models,
            self.services.state_db.as_deref(),
            &[],
        )
        .await
        {
            Ok(route) => route,
            Err(err) => {
                warn!(task_key = task_key.as_str(), error = %err, "failed to apply model router for turn");
                routed_config.model_router_accounting = None;
                if had_accounting {
                    return Arc::new(
                        self.rebuild_turn_context_from_config(
                            turn_context.as_ref(),
                            routed_config,
                            /*model_router_route_changed*/ false,
                        )
                        .await,
                    );
                }
                return turn_context;
            }
        };

        let accounting_cleared = had_accounting && routed_config.model_router_accounting.is_none();
        if route.is_none() && routed_config.model_router_accounting.is_none() && !accounting_cleared
        {
            return turn_context;
        }

        let model_router_route_changed = routed_config.model != previous_model
            || routed_config.model_provider_id != previous_provider_id
            || config_account_pool_default(&routed_config) != previous_account_pool
            || routed_config.service_tier != previous_service_tier
            || routed_config.model_reasoning_effort != previous_reasoning_effort;
        Arc::new(
            self.rebuild_turn_context_from_config(
                turn_context.as_ref(),
                routed_config,
                model_router_route_changed,
            )
            .await,
        )
    }

    async fn rebuild_turn_context_from_config(
        &self,
        previous: &TurnContext,
        per_turn_config: Config,
        model_router_route_changed: bool,
    ) -> TurnContext {
        let mut session_configuration: SessionConfiguration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        session_configuration.provider = per_turn_config.model_provider.clone();
        let model = per_turn_config
            .model
            .clone()
            .unwrap_or_else(|| session_configuration.collaboration_mode.model().to_string());
        session_configuration.collaboration_mode =
            session_configuration.collaboration_mode.with_updates(
                Some(model.clone()),
                Some(per_turn_config.model_reasoning_effort),
                None,
            );
        session_configuration.service_tier = per_turn_config.service_tier;

        let model_info = self
            .services
            .models_manager
            .get_model_info(model.as_str(), &per_turn_config.to_models_manager_config())
            .await;
        let plugin_outcome = self
            .services
            .plugins_manager
            .plugins_for_config(&per_turn_config)
            .await;
        let effective_skill_roots = plugin_outcome.effective_skill_roots();
        let skills_input = skills_load_input_from_config(&per_turn_config, effective_skill_roots);
        let fs = previous
            .environment
            .as_ref()
            .map(|environment| environment.get_filesystem());
        let skills_outcome = Arc::new(
            self.services
                .skills_manager
                .skills_for_config(&skills_input, fs)
                .await,
        );
        let mut rebuilt = Self::make_turn_context(
            self.conversation_id,
            Some(Arc::clone(&self.services.auth_manager)),
            &self.services.session_telemetry,
            per_turn_config.model_provider.clone(),
            &session_configuration,
            self.services.user_shell.as_ref(),
            self.services.shell_zsh_path.as_ref(),
            self.services.main_execve_wrapper_exe.as_ref(),
            per_turn_config,
            model_info,
            &self.services.models_manager,
            previous.network.clone(),
            previous.environment.clone(),
            previous.environments.clone(),
            previous.cwd.clone(),
            previous.sub_id.clone(),
            skills_outcome,
            previous.implement_requested,
        );
        rebuilt.trace_id = previous.trace_id.clone();
        rebuilt.realtime_active = previous.realtime_active;
        rebuilt.final_output_json_schema = previous.final_output_json_schema.clone();
        rebuilt.turn_metadata_state = Arc::clone(&previous.turn_metadata_state);
        rebuilt.turn_timing_state = Arc::clone(&previous.turn_timing_state);
        rebuilt.model_response_ordinal = AtomicI64::new(previous.model_response_ordinal());
        rebuilt.server_model_warning_emitted = AtomicBool::new(
            previous
                .server_model_warning_emitted
                .load(Ordering::Relaxed),
        );
        rebuilt.model_verification_emitted =
            AtomicBool::new(previous.model_verification_emitted.load(Ordering::Relaxed));
        rebuilt.model_router_route_changed = model_router_route_changed;
        rebuilt
    }
}
