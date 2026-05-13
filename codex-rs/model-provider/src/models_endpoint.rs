use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use codex_api::ModelsClient;
use codex_api::RequestTelemetry;
use codex_api::ReqwestTransport;
use codex_api::TransportError;
use codex_api::auth_header_telemetry;
use codex_api::map_api_error;
use codex_feedback::FeedbackRequestTags;
use codex_feedback::emit_feedback_request_tags_with_auth_env;
use codex_login::AuthEnvTelemetry;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::collect_auth_env_telemetry;
use codex_login::default_client::build_reqwest_client;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::ModelsEndpointClient;
use codex_otel::TelemetryAuthMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ModelInfo;
use codex_response_debug_context::extract_response_debug_context;
use codex_response_debug_context::telemetry_transport_error_message;
use http::HeaderMap;
use tokio::time::timeout;

use crate::auth::auth_manager_for_provider;
use crate::auth::resolve_provider_auth;

const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const MODELS_ENDPOINT: &str = "/models";

/// Provider-owned OpenAI-compatible `/models` endpoint.
#[derive(Debug)]
pub(crate) struct OpenAiModelsEndpoint {
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
}

impl OpenAiModelsEndpoint {
    pub(crate) fn new(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        Self {
            provider_info,
            auth_manager,
        }
    }

    async fn auth(&self) -> Option<CodexAuth> {
        match self.auth_manager.as_ref() {
            Some(auth_manager) => auth_manager.auth().await,
            None => None,
        }
    }

    fn auth_env(&self) -> AuthEnvTelemetry {
        let codex_api_key_env_enabled = self
            .auth_manager
            .as_ref()
            .is_some_and(|auth_manager| auth_manager.codex_api_key_env_enabled());
        collect_auth_env_telemetry(&self.provider_info, codex_api_key_env_enabled)
    }
}

/// Fetches a provider's OpenAI-compatible `/models` catalog without using the
/// app-wide model cache or first-party OpenAI/ChatGPT auth.
pub async fn list_provider_models_uncached(
    provider_info: ModelProviderInfo,
    client_version: &str,
) -> CoreResult<Vec<ModelInfo>> {
    let auth_manager = auth_manager_for_provider(/*auth_manager*/ None, &provider_info);
    let endpoint = OpenAiModelsEndpoint::new(provider_info, auth_manager);
    let (models, _etag) = endpoint.list_models(client_version).await?;
    Ok(models)
}

#[async_trait]
impl ModelsEndpointClient for OpenAiModelsEndpoint {
    fn has_command_auth(&self) -> bool {
        self.provider_info.has_command_auth()
    }

    async fn uses_codex_backend(&self) -> bool {
        self.auth()
            .await
            .as_ref()
            .is_some_and(CodexAuth::uses_codex_backend)
    }

    async fn list_models(
        &self,
        client_version: &str,
    ) -> CoreResult<(Vec<ModelInfo>, Option<String>)> {
        let _timer =
            codex_otel::start_global_timer("codex.remote_models.fetch_update.duration_ms", &[]);
        let auth = self.auth().await;
        let auth_mode = auth.as_ref().map(CodexAuth::auth_mode);
        let api_provider = self.provider_info.to_api_provider(auth_mode)?;
        let api_auth = resolve_provider_auth(auth.as_ref(), &self.provider_info)?;
        let transport = ReqwestTransport::new(build_reqwest_client());
        let auth_telemetry = auth_header_telemetry(api_auth.as_ref());
        let request_telemetry: Arc<dyn RequestTelemetry> = Arc::new(ModelsRequestTelemetry {
            auth_mode: auth_mode.map(|mode| TelemetryAuthMode::from(mode).to_string()),
            auth_header_attached: auth_telemetry.attached,
            auth_header_name: auth_telemetry.name,
            auth_env: self.auth_env(),
        });
        let client = ModelsClient::new(transport, api_provider, api_auth)
            .with_telemetry(Some(request_telemetry));

        timeout(
            MODELS_REFRESH_TIMEOUT,
            client.list_models(client_version, HeaderMap::new()),
        )
        .await
        .map_err(|_| CodexErr::Timeout)?
        .map_err(map_api_error)
    }
}

#[derive(Clone)]
struct ModelsRequestTelemetry {
    auth_mode: Option<String>,
    auth_header_attached: bool,
    auth_header_name: Option<&'static str>,
    auth_env: AuthEnvTelemetry,
}

impl RequestTelemetry for ModelsRequestTelemetry {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<http::StatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        let success = status.is_some_and(|code| code.is_success()) && error.is_none();
        let error_message = error.map(telemetry_transport_error_message);
        let response_debug = error
            .map(extract_response_debug_context)
            .unwrap_or_default();
        let status = status.map(|status| status.as_u16());
        tracing::event!(
            target: "codex_otel.log_only",
            tracing::Level::INFO,
            event.name = "codex.api_request",
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            success = success,
            error.message = error_message.as_deref(),
            attempt = attempt,
            endpoint = MODELS_ENDPOINT,
            auth.header_attached = self.auth_header_attached,
            auth.header_name = self.auth_header_name,
            auth.env_openai_api_key_present = self.auth_env.openai_api_key_env_present,
            auth.env_codex_api_key_present = self.auth_env.codex_api_key_env_present,
            auth.env_codex_api_key_enabled = self.auth_env.codex_api_key_env_enabled,
            auth.env_provider_key_name = self.auth_env.provider_env_key_name.as_deref(),
            auth.env_provider_key_present = self.auth_env.provider_env_key_present,
            auth.env_refresh_token_url_override_present = self.auth_env.refresh_token_url_override_present,
            auth.request_id = response_debug.request_id.as_deref(),
            auth.cf_ray = response_debug.cf_ray.as_deref(),
            auth.error = response_debug.auth_error.as_deref(),
            auth.error_code = response_debug.auth_error_code.as_deref(),
            auth.mode = self.auth_mode.as_deref(),
        );
        tracing::event!(
            target: "codex_otel.trace_safe",
            tracing::Level::INFO,
            event.name = "codex.api_request",
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            success = success,
            error.message = error_message.as_deref(),
            attempt = attempt,
            endpoint = MODELS_ENDPOINT,
            auth.header_attached = self.auth_header_attached,
            auth.header_name = self.auth_header_name,
            auth.env_openai_api_key_present = self.auth_env.openai_api_key_env_present,
            auth.env_codex_api_key_present = self.auth_env.codex_api_key_env_present,
            auth.env_codex_api_key_enabled = self.auth_env.codex_api_key_env_enabled,
            auth.env_provider_key_name = self.auth_env.provider_env_key_name.as_deref(),
            auth.env_provider_key_present = self.auth_env.provider_env_key_present,
            auth.env_refresh_token_url_override_present = self.auth_env.refresh_token_url_override_present,
            auth.request_id = response_debug.request_id.as_deref(),
            auth.cf_ray = response_debug.cf_ray.as_deref(),
            auth.error = response_debug.auth_error.as_deref(),
            auth.error_code = response_debug.auth_error_code.as_deref(),
            auth.mode = self.auth_mode.as_deref(),
        );
        emit_feedback_request_tags_with_auth_env(
            &FeedbackRequestTags {
                endpoint: MODELS_ENDPOINT,
                auth_header_attached: self.auth_header_attached,
                auth_header_name: self.auth_header_name,
                auth_mode: self.auth_mode.as_deref(),
                auth_retry_after_unauthorized: None,
                auth_recovery_mode: None,
                auth_recovery_phase: None,
                auth_connection_reused: None,
                auth_request_id: response_debug.request_id.as_deref(),
                auth_cf_ray: response_debug.cf_ray.as_deref(),
                auth_error: response_debug.auth_error.as_deref(),
                auth_error_code: response_debug.auth_error_code.as_deref(),
                auth_recovery_followup_success: None,
                auth_recovery_followup_status: None,
            },
            &self.auth_env,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;
    use codex_protocol::config_types::ModelProviderAuthInfo;
    use codex_protocol::openai_models::ModelsResponse;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header_regex;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    fn provider_info_with_command_auth() -> ModelProviderInfo {
        ModelProviderInfo {
            auth: Some(ModelProviderAuthInfo {
                command: "print-token".to_string(),
                args: Vec::new(),
                timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
                refresh_interval_ms: 300_000,
                cwd: std::env::current_dir()
                    .expect("current dir should be available")
                    .try_into()
                    .expect("current dir should be absolute"),
            }),
            requires_openai_auth: false,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        }
    }

    fn provider_for(base_url: String) -> ModelProviderInfo {
        ModelProviderInfo {
            name: "mock".into(),
            base_url: Some(base_url),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: None,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: Some(0),
            stream_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

    fn remote_model(slug: &str) -> ModelInfo {
        serde_json::from_value(json!({
            "slug": slug,
            "display_name": slug,
            "description": null,
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [],
            "shell_type": "shell_command",
            "visibility": "list",
            "supported_in_api": true,
            "priority": 0,
            "upgrade": null,
            "base_instructions": "base instructions",
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "supports_image_detail_original": false,
            "context_window": 272_000,
            "max_context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
    }

    #[test]
    fn command_auth_provider_reports_command_auth_without_cached_auth() {
        let endpoint = OpenAiModelsEndpoint::new(
            provider_info_with_command_auth(),
            /*auth_manager*/ None,
        );

        assert!(endpoint.has_command_auth());
    }

    #[test]
    fn provider_without_command_auth_reports_no_command_auth() {
        let endpoint = OpenAiModelsEndpoint::new(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert!(!endpoint.has_command_auth());
    }

    #[tokio::test]
    async fn list_provider_models_uncached_uses_provider_local_bearer_token() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header_regex("Authorization", "Bearer provider-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(ModelsResponse {
                        models: vec![remote_model("provider-model")],
                    }),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut provider_info = provider_for(server.uri());
        provider_info.experimental_bearer_token = Some("provider-token".to_string());
        let models = list_provider_models_uncached(provider_info, "test-client")
            .await
            .expect("models should load");

        assert_eq!(
            models
                .iter()
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["provider-model"]
        );
    }
}
