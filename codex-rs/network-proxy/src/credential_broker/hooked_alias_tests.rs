use super::*;

#[test]
fn selected_openai_credential_matches_ipv6_alias_with_plaintext_fallback() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let mut env = env_map([
        ("OPENAI_API_KEY", "sk-real"),
        ("OPENAI_BASE_URL", "https://[fd00::1]/v1"),
    ]);
    broker.virtualize_child_env(&mut env);
    for destination in ["https://[fd00::1]/v1/models", "fd00::1"] {
        assert!(broker.request_matches_hooked_host_alias(
            destination,
            &headers_with_bearer(&env["OPENAI_API_KEY"]),
            &["api.openai.com".to_string()],
            /*environment_id*/ None,
        ));
    }
}

#[test]
fn selected_alias_checks_equivalent_and_merged_credential_bindings() {
    let broker = CredentialBroker::new(/*enabled*/ true);
    let register = |env_var: &str, token: &str, host: &str| {
        let mut env = env_map([("GH_HOST", host), (env_var, token)]);
        broker.virtualize_child_env_for_environment(&mut env, Some("selected"));
        env.insert("GH_HOST".to_string(), "alias.example".to_string());
        broker.virtualize_child_env_for_environment(&mut env, Some("selected"));
        env[env_var].clone()
    };
    let first = register("GH_ENTERPRISE_TOKEN", "ghp-first-real", "alias.example");
    let equivalent = register(
        "GITHUB_ENTERPRISE_TOKEN",
        "ghp-first-real",
        "hooked.example",
    );
    let second = register(
        "GITHUB_ENTERPRISE_TOKEN",
        "ghp-second-real",
        "second-hooked.example",
    );
    assert_ne!(first, equivalent);

    assert!(broker.request_matches_hooked_host_alias(
        "alias.example",
        &headers_with_bearer(&equivalent),
        &["hooked.example".to_string()],
        Some("selected"),
    ));
    let basic = STANDARD.encode(format!("{first}:{second}"));
    let headers = headers_with_authorization(&format!("Basic {basic}"));
    assert!(broker.request_matches_hooked_host_alias(
        "alias.example",
        &headers,
        &["second-hooked.example".to_string()],
        Some("selected"),
    ));
    assert!(!broker.request_matches_hooked_host_alias(
        "alias.example",
        &headers,
        &["second-hooked.example".to_string()],
        Some("other"),
    ));
}
