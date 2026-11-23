use super::OpenAiDestination;
use super::classify_openai_api_url;
use super::is_openai_api_url;
use pretty_assertions::assert_eq;

#[test]
fn unsupported_openai_origins_are_distinct_from_other_providers() {
    for destination in [
        "https://unknown.api.openai.com/v1",
        "https://api.openai.com../v1",
        "https://api.openai.com:8443/v1",
        "http://api.openai.com/v1",
        "https://user@api.openai.com/v1",
        "https://other.openai.org/v1",
    ] {
        assert_eq!(
            classify_openai_api_url(destination),
            OpenAiDestination::Unsupported,
            "{destination}"
        );
    }
    for destination in [
        "https://api.openai.com.example.test/v1",
        "https://example.test/v1",
        "http://127.0.0.1:8080/v1",
    ] {
        assert_eq!(
            classify_openai_api_url(destination),
            OpenAiDestination::Other,
            "{destination}"
        );
    }
}

#[test]
fn runtime_credentials_require_a_trusted_https_origin() {
    for destination in [
        "https://api.openai.com/v1",
        "https://api.openai.com.:443/v1",
        "https://gov.api.openai.com/v1",
        "https://eu.api.openai.com/v1",
        "https://us.api.openai.com/v1",
        "https://au.api.openai.com/v1",
        "https://ca.api.openai.com/v1",
        "https://jp.api.openai.com/v1",
        "https://in.api.openai.com/v1",
        "https://sg.api.openai.com/v1",
        "https://kr.api.openai.com/v1",
        "https://gb.api.openai.com/v1",
        "https://ae.api.openai.com/v1",
        "https://mtls.api.openai.com/v1",
        "https://mtls-us.api.openai.com/v1",
        "https://mtls-eu.api.openai.com/v1",
        "https://api.openai.org/v1",
        "https://api.openai-staging.com/v1",
        "https://chatgpt.com/backend-api",
        "https://chat.openai.com/backend-api",
        "https://us.chatgpt.com/backend-api",
        "https://chatgpt-staging.com/backend-api",
        "https://us.chatgpt-staging.com/backend-api",
    ] {
        assert!(is_openai_api_url(destination), "{destination}");
    }
    for destination in [
        "http://api.openai.com/v1",
        "https://api.openai.com:8443/v1",
        "https://user@api.openai.com/v1",
        "https://user:password@chatgpt.com/backend-api",
        "https://api.openai.com.example.test/v1",
        "https://eu.api.openai.com.example.test/v1",
        "https://unknown.api.openai.com/v1",
        "https://evilchatgpt.com/backend-api",
        "https://chatgpt.com.example.test/backend-api",
        "https://foo.chat.openai.com/backend-api",
        "http://127.0.0.1:8080/v1",
        "https://example.test/v1",
    ] {
        assert!(!is_openai_api_url(destination), "{destination}");
    }
}
