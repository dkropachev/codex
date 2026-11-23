//! Classify OpenAI API destinations without treating an unsupported origin as another provider.

use crate::is_allowed_chatgpt_host;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiDestination {
    Supported,
    Unsupported,
    Other,
}

pub fn classify_openai_api_url(destination: &str) -> OpenAiDestination {
    let Ok(url) = reqwest::Url::parse(destination) else {
        return OpenAiDestination::Other;
    };
    let Some(host) = url.host_str() else {
        return OpenAiDestination::Other;
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let openai_owned = [
        "openai.com",
        "openai.org",
        "openai-staging.com",
        "chatgpt.com",
        "chatgpt-staging.com",
    ]
    .iter()
    .any(|domain| {
        let host = host.trim_end_matches('.');
        host == *domain
            || host
                .strip_suffix(*domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    });
    if !openai_owned {
        return OpenAiDestination::Other;
    }
    let supported_host = matches!(
        host,
        "api.openai.com"
            | "gov.api.openai.com"
            | "us.api.openai.com"
            | "eu.api.openai.com"
            | "au.api.openai.com"
            | "ca.api.openai.com"
            | "jp.api.openai.com"
            | "in.api.openai.com"
            | "sg.api.openai.com"
            | "kr.api.openai.com"
            | "gb.api.openai.com"
            | "ae.api.openai.com"
            | "mtls.api.openai.com"
            | "mtls-us.api.openai.com"
            | "mtls-eu.api.openai.com"
            | "api.openai.org"
            | "api.openai-staging.com"
    ) || is_allowed_chatgpt_host(host);
    if supported_host
        && url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
    {
        OpenAiDestination::Supported
    } else {
        OpenAiDestination::Unsupported
    }
}

pub fn is_openai_api_url(destination: &str) -> bool {
    classify_openai_api_url(destination) == OpenAiDestination::Supported
}

#[cfg(test)]
#[path = "openai_endpoints_tests.rs"]
mod tests;
