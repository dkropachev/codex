//! Registry of model providers supported by Codex.
//!
//! Providers can be defined in two places:
//!   1. Built-in defaults compiled into the binary so Codex works out-of-the-box.
//!   2. User-defined entries inside `~/.codex/config.toml` under the `model_providers`
//!      key. These override or extend the defaults at runtime.

use codex_api::Provider as ApiProvider;
use codex_api::RetryConfig as ApiRetryConfig;
use codex_api::is_azure_responses_provider;
use codex_app_server_protocol::AuthMode;
use codex_protocol::config_types::ModelProviderAuthInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::error::EnvVarError;
use codex_protocol::error::Result as CodexResult;
use http::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_STREAM_MAX_RETRIES: u64 = 5;
const DEFAULT_REQUEST_MAX_RETRIES: u64 = 4;
pub const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
/// Hard cap for user-configured `stream_max_retries`.
const MAX_STREAM_MAX_RETRIES: u64 = 100;
/// Hard cap for user-configured `request_max_retries`.
const MAX_REQUEST_MAX_RETRIES: u64 = 100;

const OPENAI_PROVIDER_NAME: &str = "OpenAI";
pub const OPENAI_PROVIDER_ID: &str = "openai";
const DEEPSEEK_PROVIDER_NAME: &str = "DeepSeek";
pub const DEEPSEEK_PROVIDER_ID: &str = "deepseek";
pub const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";
const AMAZON_BEDROCK_PROVIDER_NAME: &str = "Amazon Bedrock";
pub const AMAZON_BEDROCK_PROVIDER_ID: &str = "amazon-bedrock";
pub const AMAZON_BEDROCK_DEFAULT_BASE_URL: &str =
    "https://bedrock-mantle.us-east-1.api.aws/openai/v1";
pub const LEGACY_OLLAMA_CHAT_PROVIDER_ID: &str = "ollama-chat";
pub const OLLAMA_CHAT_PROVIDER_REMOVED_ERROR: &str = "`ollama-chat` is no longer supported.\nHow to fix: replace `ollama-chat` with `ollama` in `model_provider`, `oss_provider`, or `--local-provider`.\nMore info: https://github.com/openai/codex/discussions/7782";

/// Wire protocol that the provider speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WireApi {
    /// The Responses API exposed by OpenAI at `/v1/responses`.
    #[default]
    Responses,
    /// The OpenAI-compatible Chat Completions API exposed at `/v1/chat/completions`.
    Chat,
}

impl fmt::Display for WireApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Responses => "responses",
            Self::Chat => "chat",
        };
        f.write_str(value)
    }
}

/// Serializable representation of a provider definition.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModelProviderInfo {
    /// Friendly display name.
    #[serde(default)]
    pub name: String,
    /// Base URL for the provider's OpenAI-compatible API.
    pub base_url: Option<String>,
    /// Environment variable that stores the user's API key for this provider.
    pub env_key: Option<String>,

    /// Optional instructions to help the user get a valid value for the
    /// variable and set it.
    pub env_key_instructions: Option<String>,
    /// Value to use with `Authorization: Bearer <token>` header. Use of this
    /// config is discouraged in favor of `env_key` for security reasons, but
    /// this may be necessary when using this programmatically. The built-in
    /// `deepseek` provider also accepts this as `token` in config.toml.
    #[serde(alias = "token")]
    pub experimental_bearer_token: Option<String>,
    /// Command-backed bearer-token configuration for this provider.
    pub auth: Option<ModelProviderAuthInfo>,
    /// AWS SigV4 auth configuration for this provider.
    pub aws: Option<ModelProviderAwsAuthInfo>,
    /// Optional query parameters to append to the base URL.
    pub query_params: Option<HashMap<String, String>>,
    /// Additional HTTP headers to include in requests to this provider where
    /// the (key, value) pairs are the header name and value.
    pub http_headers: Option<HashMap<String, String>>,
    /// Optional HTTP headers to include in requests to this provider where the
    /// (key, value) pairs are the header name and _environment variable_ whose
    /// value should be used. If the environment variable is not set, or the
    /// value is empty, the header will not be included in the request.
    pub env_http_headers: Option<HashMap<String, String>>,
    /// Maximum number of times to retry a failed HTTP request to this provider.
    pub request_max_retries: Option<u64>,
    /// Number of times to retry reconnecting a dropped streaming response before failing.
    pub stream_max_retries: Option<u64>,
    /// Idle timeout (in milliseconds) to wait for activity on a streaming response before treating
    /// the connection as lost.
    pub stream_idle_timeout_ms: Option<u64>,
    /// Maximum time (in milliseconds) to wait for a websocket connection attempt before treating
    /// it as failed.
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Does this provider require an OpenAI API Key or ChatGPT login token? If true,
    /// user is presented with login screen on first run, and login preference and token/key
    /// are stored in auth.json. If false (which is the default), login screen is skipped,
    /// and API key (if needed) comes from the "env_key" environment variable.
    #[serde(default)]
    pub requires_openai_auth: bool,
    /// Whether this provider supports the Responses API WebSocket transport.
    #[serde(default)]
    pub supports_websockets: bool,
}

/// AWS SigV4 auth configuration for a model provider.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelProviderAwsAuthInfo {
    /// AWS profile name to use. When unset, the AWS SDK default chain decides.
    pub profile: Option<String>,
    /// AWS region to use for provider-specific endpoints.
    pub region: Option<String>,
}

impl ModelProviderInfo {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.aws.is_some() {
            if self.supports_websockets {
                // TODO(celia-oai): Support AWS SigV4 signing for WebSocket
                // upgrade requests before allowing AWS-authenticated providers
                // to enable Responses-over-WebSocket.
                return Err("provider aws cannot be combined with supports_websockets".to_string());
            }

            let mut conflicts = Vec::new();
            if self.env_key.is_some() {
                conflicts.push("env_key");
            }
            if self.experimental_bearer_token.is_some() {
                conflicts.push("experimental_bearer_token");
            }
            if self.auth.is_some() {
                conflicts.push("auth");
            }
            if self.requires_openai_auth {
                conflicts.push("requires_openai_auth");
            }

            if !conflicts.is_empty() {
                return Err(format!(
                    "provider aws cannot be combined with {}",
                    conflicts.join(", ")
                ));
            }
        }

        let Some(auth) = self.auth.as_ref() else {
            return Ok(());
        };

        if auth.command.trim().is_empty() {
            return Err("provider auth.command must not be empty".to_string());
        }

        let mut conflicts = Vec::new();
        if self.env_key.is_some() {
            conflicts.push("env_key");
        }
        if self.experimental_bearer_token.is_some() {
            conflicts.push("experimental_bearer_token");
        }
        if self.requires_openai_auth {
            conflicts.push("requires_openai_auth");
        }

        if conflicts.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "provider auth cannot be combined with {}",
                conflicts.join(", ")
            ))
        }
    }

    fn build_header_map(&self) -> CodexResult<HeaderMap> {
        let capacity = self.http_headers.as_ref().map_or(0, HashMap::len)
            + self.env_http_headers.as_ref().map_or(0, HashMap::len);
        let mut headers = HeaderMap::with_capacity(capacity);
        if let Some(extra) = &self.http_headers {
            for (k, v) in extra {
                if let (Ok(name), Ok(value)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
                    headers.insert(name, value);
                }
            }
        }

        if let Some(env_headers) = &self.env_http_headers {
            for (header, env_var) in env_headers {
                if let Ok(val) = std::env::var(env_var)
                    && !val.trim().is_empty()
                    && let (Ok(name), Ok(value)) =
                        (HeaderName::try_from(header), HeaderValue::try_from(val))
                {
                    headers.insert(name, value);
                }
            }
        }

        Ok(headers)
    }

    pub fn to_api_provider(&self, auth_mode: Option<AuthMode>) -> CodexResult<ApiProvider> {
        let default_base_url = if matches!(
            auth_mode,
            Some(AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens | AuthMode::AgentIdentity)
        ) {
            "https://chatgpt.com/backend-api/codex"
        } else {
            "https://api.openai.com/v1"
        };
        let base_url = self
            .base_url
            .clone()
            .unwrap_or_else(|| default_base_url.to_string());

        let headers = self.build_header_map()?;
        let retry = ApiRetryConfig {
            max_attempts: self.request_max_retries(),
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        };

        Ok(ApiProvider {
            name: self.name.clone(),
            base_url,
            query_params: self.query_params.clone(),
            headers,
            retry,
            stream_idle_timeout: self.stream_idle_timeout(),
        })
    }

    /// If `env_key` is Some, returns the API key for this provider if present
    /// (and non-empty) in the environment. If `env_key` is required but
    /// cannot be found, returns an error.
    pub fn api_key(&self) -> CodexResult<Option<String>> {
        match &self.env_key {
            Some(env_key) => {
                let api_key = std::env::var(env_key)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or_else(|| {
                        CodexErr::EnvVar(EnvVarError {
                            var: env_key.clone(),
                            instructions: self.env_key_instructions.clone(),
                        })
                    })?;
                Ok(Some(api_key))
            }
            None => Ok(None),
        }
    }

    /// Effective maximum number of request retries for this provider.
    pub fn request_max_retries(&self) -> u64 {
        self.request_max_retries
            .unwrap_or(DEFAULT_REQUEST_MAX_RETRIES)
            .min(MAX_REQUEST_MAX_RETRIES)
    }

    /// Effective maximum number of stream reconnection attempts for this provider.
    pub fn stream_max_retries(&self) -> u64 {
        self.stream_max_retries
            .unwrap_or(DEFAULT_STREAM_MAX_RETRIES)
            .min(MAX_STREAM_MAX_RETRIES)
    }

    /// Effective idle timeout for streaming responses.
    pub fn stream_idle_timeout(&self) -> Duration {
        self.stream_idle_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(DEFAULT_STREAM_IDLE_TIMEOUT_MS))
    }

    /// Effective timeout for websocket connect attempts.
    pub fn websocket_connect_timeout(&self) -> Duration {
        self.websocket_connect_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS))
    }

    pub fn create_openai_provider(base_url: Option<String>) -> ModelProviderInfo {
        ModelProviderInfo {
            name: OPENAI_PROVIDER_NAME.into(),
            base_url,
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: None,
            query_params: None,
            http_headers: Some(
                [("version".to_string(), env!("CARGO_PKG_VERSION").to_string())]
                    .into_iter()
                    .collect(),
            ),
            env_http_headers: Some(
                [
                    (
                        "OpenAI-Organization".to_string(),
                        "OPENAI_ORGANIZATION".to_string(),
                    ),
                    ("OpenAI-Project".to_string(), "OPENAI_PROJECT".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            // Use global defaults for retry/timeout unless overridden in config.toml.
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            websocket_connect_timeout_ms: None,
            requires_openai_auth: true,
            supports_websockets: true,
        }
    }

    pub fn create_deepseek_provider() -> ModelProviderInfo {
        ModelProviderInfo {
            name: DEEPSEEK_PROVIDER_NAME.into(),
            base_url: Some(DEEPSEEK_DEFAULT_BASE_URL.into()),
            env_key: Some("DEEPSEEK_API_KEY".to_string()),
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: None,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

    pub fn create_amazon_bedrock_provider(
        aws: Option<ModelProviderAwsAuthInfo>,
    ) -> ModelProviderInfo {
        ModelProviderInfo {
            name: AMAZON_BEDROCK_PROVIDER_NAME.into(),
            base_url: Some(AMAZON_BEDROCK_DEFAULT_BASE_URL.into()),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: Some(aws.unwrap_or(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
            })),
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

    pub fn is_openai(&self) -> bool {
        self.name == OPENAI_PROVIDER_NAME
    }

    pub fn is_amazon_bedrock(&self) -> bool {
        self.name == AMAZON_BEDROCK_PROVIDER_NAME
    }

    pub fn supports_remote_compaction(&self) -> bool {
        self.is_openai() || is_azure_responses_provider(&self.name, self.base_url.as_deref())
    }

    pub fn has_command_auth(&self) -> bool {
        self.auth.is_some()
    }

    pub fn is_config_ready(&self, _provider_id: &str) -> bool {
        if self
            .base_url
            .as_deref()
            .is_none_or(|base_url| base_url.trim().is_empty())
        {
            return false;
        }
        if self
            .experimental_bearer_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
        {
            return true;
        }
        if self.auth.is_some()
            || self
                .http_headers
                .as_ref()
                .is_some_and(|headers| headers.values().any(|value| !value.trim().is_empty()))
        {
            return true;
        }
        if self.env_http_headers.as_ref().is_some_and(|headers| {
            headers
                .values()
                .any(|env_key| std::env::var(env_key).is_ok_and(|value| !value.trim().is_empty()))
        }) {
            return true;
        }
        if let Some(aws) = self.aws.as_ref() {
            return aws
                .profile
                .as_deref()
                .is_some_and(|profile| !profile.trim().is_empty())
                || aws
                    .region
                    .as_deref()
                    .is_some_and(|region| !region.trim().is_empty())
                || env_var_has_non_empty_value("AWS_PROFILE")
                || (env_var_has_non_empty_value("AWS_ACCESS_KEY_ID")
                    && env_var_has_non_empty_value("AWS_SECRET_ACCESS_KEY"))
                || (env_var_has_non_empty_value("AWS_BEARER_TOKEN_BEDROCK")
                    && (env_var_has_non_empty_value("AWS_REGION")
                        || env_var_has_non_empty_value("AWS_DEFAULT_REGION")))
                || (env_var_has_non_empty_value("AWS_WEB_IDENTITY_TOKEN_FILE")
                    && env_var_has_non_empty_value("AWS_ROLE_ARN"));
        }
        if let Some(env_key) = self.env_key.as_deref() {
            return env_var_has_non_empty_value(env_key);
        }
        !self.requires_openai_auth
    }
}

fn env_var_has_non_empty_value(env_key: &str) -> bool {
    std::env::var(env_key).is_ok_and(|value| !value.trim().is_empty())
}

pub const DEFAULT_LMSTUDIO_PORT: u16 = 1234;
pub const DEFAULT_OLLAMA_PORT: u16 = 11434;

pub const LMSTUDIO_OSS_PROVIDER_ID: &str = "lmstudio";
pub const OLLAMA_OSS_PROVIDER_ID: &str = "ollama";

/// Built-in default provider list.
pub fn built_in_model_providers(
    openai_base_url: Option<String>,
) -> HashMap<String, ModelProviderInfo> {
    use ModelProviderInfo as P;
    let openai_provider = P::create_openai_provider(openai_base_url);
    let deepseek_provider = P::create_deepseek_provider();
    let amazon_bedrock_provider = P::create_amazon_bedrock_provider(/*aws*/ None);

    // Keep bundled third-party providers narrow and explicit. Users can add
    // more providers through `model_providers` in config.toml.
    [
        (OPENAI_PROVIDER_ID, openai_provider),
        (DEEPSEEK_PROVIDER_ID, deepseek_provider),
        (AMAZON_BEDROCK_PROVIDER_ID, amazon_bedrock_provider),
        (OLLAMA_OSS_PROVIDER_ID, create_oss_provider()),
        (LMSTUDIO_OSS_PROVIDER_ID, create_oss_provider()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// Merge configured providers into the built-in provider catalog.
///
/// Configured providers extend the built-in set. Built-in providers are not
/// generally overridable, but specific providers allow narrow overrides:
/// Amazon Bedrock supports `aws.profile` and `aws.region`, and DeepSeek
/// supports `token` for a config-backed bearer token.
pub fn merge_configured_model_providers(
    mut model_providers: HashMap<String, ModelProviderInfo>,
    configured_model_providers: HashMap<String, ModelProviderInfo>,
) -> Result<HashMap<String, ModelProviderInfo>, String> {
    for (key, mut provider) in configured_model_providers {
        if key == AMAZON_BEDROCK_PROVIDER_ID {
            let aws_override = provider.aws.take();
            if provider != ModelProviderInfo::default() {
                return Err(format!(
                    "model_providers.{AMAZON_BEDROCK_PROVIDER_ID} only supports changing \
`aws.profile` and `aws.region`; other non-default provider fields are not supported"
                ));
            }

            if let Some(aws_override) = aws_override
                && let Some(built_in_provider) = model_providers.get_mut(AMAZON_BEDROCK_PROVIDER_ID)
                && let Some(built_in_aws) = built_in_provider.aws.as_mut()
            {
                if let Some(profile) = aws_override.profile {
                    built_in_aws.profile = Some(profile);
                }
                if let Some(region) = aws_override.region {
                    built_in_aws.region = Some(region);
                }
            }
        } else if key == DEEPSEEK_PROVIDER_ID {
            let token_override = provider.experimental_bearer_token.take();
            if provider != ModelProviderInfo::default() {
                return Err(format!(
                    "model_providers.{DEEPSEEK_PROVIDER_ID} only supports changing `token`; \
other non-default provider fields are not supported"
                ));
            }

            if let Some(token) = token_override
                && let Some(built_in_provider) = model_providers.get_mut(DEEPSEEK_PROVIDER_ID)
            {
                built_in_provider.experimental_bearer_token = Some(token);
                built_in_provider.env_key = None;
            }
        } else if matches!(
            key.as_str(),
            OLLAMA_OSS_PROVIDER_ID | LMSTUDIO_OSS_PROVIDER_ID
        ) {
            let base_url_override = provider.base_url.take();
            if provider != ModelProviderInfo::default() {
                return Err(format!(
                    "model_providers.{key} only supports changing `base_url`; other non-default provider fields are not supported"
                ));
            }

            if let Some(base_url) = base_url_override
                && let Some(built_in_provider) = model_providers.get_mut(key.as_str())
            {
                built_in_provider.base_url = Some(base_url);
            }
        } else {
            model_providers.entry(key).or_insert(provider);
        }
    }

    Ok(model_providers)
}

pub fn create_oss_provider() -> ModelProviderInfo {
    match oss_provider_base_url_from_env() {
        Some(base_url) => create_oss_provider_with_base_url(&base_url),
        None => create_unconfigured_oss_provider(),
    }
}

fn oss_provider_base_url_from_env() -> Option<String> {
    // These CODEX_OSS_ environment variables are experimental: we may
    // switch to reading values from config.toml instead.
    if let Some(base_url) = std::env::var("CODEX_OSS_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(base_url);
    }
    std::env::var("CODEX_OSS_PORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| value.parse::<u16>().ok())
        .map(|port| format!("http://localhost:{port}/v1"))
}

fn create_unconfigured_oss_provider() -> ModelProviderInfo {
    ModelProviderInfo {
        name: "gpt-oss".into(),
        base_url: None,
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

pub fn create_oss_provider_with_base_url(base_url: &str) -> ModelProviderInfo {
    ModelProviderInfo {
        name: "gpt-oss".into(),
        base_url: Some(base_url.into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

#[cfg(test)]
#[path = "model_provider_info_tests.rs"]
mod tests;
