//! Authenticated backend usage/reset transport owned by login to avoid a client dependency cycle.
use super::*;
use crate::default_client::ClientRedirectPolicy;
use crate::default_client::create_client_for_route_async;
use codex_http_client::ClientRouteClass;
use serde_json::Value;

pub(super) enum UsageOperation<'a> {
    Read,
    ConsumeReset { redeem_request_id: &'a str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UsageRefreshFailure {
    pub(super) message: String,
    pub(super) has_usable_credentials: bool,
}

impl UsageRefreshFailure {
    pub(super) fn with_usable_credentials(message: String) -> Self {
        Self {
            message,
            has_usable_credentials: true,
        }
    }

    fn auth_rejected(message: String) -> Self {
        Self {
            message,
            has_usable_credentials: false,
        }
    }
}

pub(super) async fn request(
    base_url: &str,
    auth: &CodexAuth,
    auth_route_config: &AuthRouteConfig,
    operation: UsageOperation<'_>,
) -> Result<Value, UsageRefreshFailure> {
    if !auth.uses_codex_backend() {
        return Err(UsageRefreshFailure::auth_rejected(
            "chatgpt authentication required to refresh usage".to_string(),
        ));
    }

    let mut base_url = base_url.trim_end_matches('/').to_string();
    if (base_url.starts_with("https://chatgpt.com")
        || base_url.starts_with("https://chat.openai.com"))
        && !base_url.contains("/backend-api")
    {
        base_url = format!("{base_url}/backend-api");
    }
    let prefix = if base_url.contains("/backend-api") {
        "wham"
    } else {
        "api/codex"
    };
    let path = match operation {
        UsageOperation::Read => "usage",
        UsageOperation::ConsumeReset { .. } => "rate-limit-reset-credits/consume",
    };
    let url = format!("{base_url}/{prefix}/{path}");
    let token = auth.get_token().map_err(|err| {
        UsageRefreshFailure::auth_rejected(format!("failed to read auth token: {err}"))
    })?;
    let client = create_client_for_route_async(
        auth_route_config.http_client_factory().clone(),
        url.clone(),
        ClientRouteClass::Api,
        ClientRedirectPolicy::Default,
    )
    .await
    .map_err(|err| {
        UsageRefreshFailure::with_usable_credentials(format!(
            "failed to create client for codex usage: {err}"
        ))
    })?;
    let mut request = match operation {
        UsageOperation::Read => client.get(&url),
        UsageOperation::ConsumeReset { redeem_request_id } => client
            .post(&url)
            .json(&serde_json::json!({ "redeem_request_id": redeem_request_id })),
    }
    .bearer_auth(token);
    if let Some(account_id) = auth.get_account_id() {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    if auth.is_fedramp_account() {
        request = request.header("X-OpenAI-Fedramp", "true");
    }

    let response = request.send().await.map_err(|err| {
        UsageRefreshFailure::with_usable_credentials(format!("failed to fetch codex usage: {err}"))
    })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = format!("failed to fetch codex usage: {status}; body={body}");
        if matches!(
            status,
            http::StatusCode::UNAUTHORIZED | http::StatusCode::FORBIDDEN
        ) {
            return Err(UsageRefreshFailure::auth_rejected(message));
        }
        return Err(UsageRefreshFailure::with_usable_credentials(message));
    }
    let payload: Value = serde_json::from_str(&body).map_err(|err| {
        UsageRefreshFailure::with_usable_credentials(format!(
            "failed to decode codex usage response: {err}"
        ))
    })?;
    Ok(payload)
}
