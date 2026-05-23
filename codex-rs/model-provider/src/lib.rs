mod amazon_bedrock;
mod auth;
mod bearer_auth_provider;
mod models_endpoint;
mod provider;

use codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ModelInfo;

pub use auth::auth_provider_from_auth;
pub use auth::unauthenticated_auth_provider;
pub use bearer_auth_provider::BearerAuthProvider;
pub use bearer_auth_provider::BearerAuthProvider as CoreAuthProvider;
pub use codex_protocol::account::ProviderAccount;
pub use models_endpoint::list_provider_models_uncached;
pub use provider::ModelProvider;
pub use provider::ProviderAccountError;
pub use provider::ProviderAccountResult;
pub use provider::ProviderAccountState;
pub use provider::SharedModelProvider;
pub use provider::create_model_provider;

pub async fn list_provider_models_for_discovery(
    provider_id: &str,
    provider_info: &ModelProviderInfo,
    client_version: &str,
) -> CoreResult<Vec<ModelInfo>> {
    if !provider_info.is_config_ready(provider_id) {
        return Ok(Vec::new());
    }
    if provider_id == AMAZON_BEDROCK_PROVIDER_ID {
        return Ok(amazon_bedrock::static_model_catalog().models);
    }
    list_provider_models_uncached(provider_info.clone(), client_version).await
}
