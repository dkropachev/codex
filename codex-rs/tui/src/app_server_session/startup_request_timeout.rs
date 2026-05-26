use codex_app_server_client::AppServerClient;
use codex_app_server_protocol::ClientRequest;
use color_eyre::eyre::Result;
use serde::de::DeserializeOwned;
use std::time::Duration;
use tokio::time::timeout;

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 30);

pub(crate) async fn request_typed<T>(
    client: &AppServerClient,
    request: ClientRequest,
    request_timeout: Duration,
    context: &'static str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    match timeout(request_timeout, client.request_typed(request)).await {
        Ok(result) => result.map_err(|err| color_eyre::eyre::eyre!("{context}: {err}")),
        Err(_) => {
            let timeout_ms = request_timeout.as_millis();
            Err(color_eyre::eyre::eyre!(
                "{context}: timed out after {timeout_ms}ms waiting for app-server response"
            ))
        }
    }
}
