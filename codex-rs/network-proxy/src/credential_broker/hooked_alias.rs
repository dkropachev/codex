//! Match selected broker credentials to aliases of hosts protected by MITM hooks.

use super::CredentialBroker;
use super::registry::select_credentials;
use crate::policy::normalize_host;
use rama_http::HeaderMap;
use url::Url;

impl CredentialBroker {
    pub(crate) fn request_matches_hooked_host_alias(
        &self,
        destination: &str,
        headers: &HeaderMap,
        hooked_hosts: &[String],
        environment_id: Option<&str>,
    ) -> bool {
        let request = if destination.contains("://") {
            Url::parse(destination).ok()
        } else {
            None
        };
        let normalized_host = normalize_host(
            request
                .as_ref()
                .and_then(Url::host_str)
                .unwrap_or(destination),
        );
        let state = self.read_state();
        if !state.enabled {
            return false;
        }

        let selected_headers =
            select_credentials(headers, destination, &state.credentials, environment_id)
                .into_iter()
                .map(|(_, name, _)| name)
                .collect::<Vec<_>>();
        if selected_headers.is_empty() {
            return false;
        }
        let matching_credentials = state
            .credentials
            .iter()
            .filter(|credential| {
                credential.belongs_to_environment(environment_id)
                    && credential
                        .host_bindings()
                        .any(|binding| binding.matches_request(&normalized_host, request.as_ref()))
            })
            .collect::<Vec<_>>();

        // Ambiguity removes the whole header, so every translation for a surviving header
        // contributed through equivalent deduplication or Basic-field merging.
        matching_credentials.iter().any(|credential| {
            hooked_hosts.iter().any(|hooked_host| {
                hooked_host != &normalized_host
                    && credential
                        .host_bindings()
                        .any(|binding| binding.matches_hostname(hooked_host))
            }) && matching_credentials.iter().any(|candidate| {
                candidate.provider.same_provider(&credential.provider)
                    && candidate.real_value == credential.real_value
                    && credential
                        .provider
                        .translate_request_headers(
                            headers,
                            &candidate.dummy_value,
                            &credential.real_value,
                        )
                        .iter()
                        .any(|(name, _)| selected_headers.contains(name))
            })
        })
    }
}
