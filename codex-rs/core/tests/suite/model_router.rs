use anyhow::Result;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_config::config_toml::ModelRouterDiscoveryToml;
use codex_config::config_toml::ModelRouterModelRuleToml;
use codex_config::config_toml::ModelRouterModelRuleTypeToml;
use codex_config::config_toml::ModelRouterModelSelectorToml;
use codex_config::config_toml::ModelRouterModelsToml;
use codex_config::config_toml::ModelRouterReasoningEffortToml;
use codex_config::config_toml::ModelRouterToml;
use codex_features::Feature;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::Settings;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelRerouteReason;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use codex_state::ModelRouterUsageGroupBy;
use codex_state::ModelRouterUsageQuery;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

const DEFAULT_MODEL: &str = "gpt-5.4";
const ROUTED_MODEL: &str = "gpt-5.2";

fn disabled_turn(test: &TestCodex, prompt: &str) -> Op {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.cwd_path());
    Op::UserInput {
        items: vec![UserInput::Text {
            text: prompt.to_string(),
            text_elements: Vec::new(),
        }],
        environments: None,
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: ThreadSettingsOverrides {
            cwd: Some(test.config.cwd.clone()),
            approval_policy: Some(AskForApproval::Never),
            sandbox_policy: Some(sandbox_policy),
            permission_profile,
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: DEFAULT_MODEL.to_string(),
                    reasoning_effort: test.config.model_reasoning_effort.clone(),
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        },
    }
}

fn require_routed_model_router() -> ModelRouterToml {
    ModelRouterToml {
        enabled: true,
        discovery: Some(ModelRouterDiscoveryToml::Manual),
        candidates: vec![ModelRouterCandidateToml {
            id: Some("routed".to_string()),
            model: Some(ROUTED_MODEL.to_string()),
            reasoning_effort: Some(ModelRouterReasoningEffortToml::Low),
            intelligence_score: Some(0.99),
            success_rate: Some(1.0),
            median_latency_ms: Some(1),
            input_price_per_million: Some(1.0),
            cached_input_price_per_million: Some(0.1),
            output_price_per_million: Some(2.0),
            ..Default::default()
        }],
        models: Some(ModelRouterModelsToml {
            rules: vec![ModelRouterModelRuleToml {
                id: Some("force-routed-model".to_string()),
                rule_type: ModelRouterModelRuleTypeToml::Require,
                tasks: vec!["chat.codex".to_string()],
                except_tasks: Vec::new(),
                models: vec![ModelRouterModelSelectorToml {
                    provider: None,
                    model: Some(ROUTED_MODEL.to_string()),
                }],
            }],
        }),
        ..Default::default()
    }
}

fn same_model_service_tier_router() -> ModelRouterToml {
    ModelRouterToml {
        enabled: true,
        discovery: Some(ModelRouterDiscoveryToml::Manual),
        candidates: vec![ModelRouterCandidateToml {
            id: Some("fast-same-model".to_string()),
            model: Some(DEFAULT_MODEL.to_string()),
            service_tier: Some(ServiceTier::Fast),
            reasoning_effort: Some(ModelRouterReasoningEffortToml::Low),
            intelligence_score: Some(1.0),
            success_rate: Some(1.0),
            median_latency_ms: Some(1),
            input_price_per_million: Some(0.01),
            cached_input_price_per_million: Some(0.001),
            output_price_per_million: Some(0.02),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regular_turn_uses_routed_model_and_records_usage() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_completed_with_tokens("resp-1", /*total_tokens*/ 11),
        ]),
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::Sqlite)
            .expect("test config should allow feature update");
        config.model = Some(DEFAULT_MODEL.to_string());
        config.model_router = Some(require_routed_model_router());
    });
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_turn(&test, "route this turn"))
        .await?;

    let reroute = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ModelReroute(_))
    })
    .await;
    let EventMsg::ModelReroute(reroute) = reroute else {
        panic!("expected model reroute event");
    };
    assert_eq!(reroute.from_model, DEFAULT_MODEL);
    assert_eq!(reroute.to_model, ROUTED_MODEL);
    assert_eq!(reroute.reason, ModelRerouteReason::ModelRouterPolicy);

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let body = response_mock.single_request().body_json();
    assert_eq!(body["model"].as_str(), Some(ROUTED_MODEL));
    assert_eq!(
        body.pointer("/reasoning/effort")
            .and_then(|value| value.as_str()),
        Some("low")
    );

    let db = test.codex.state_db().expect("state db enabled");
    let summary = db
        .model_router_usage_summary(ModelRouterUsageQuery {
            window_start_ms: None,
            window_end_ms: i64::MAX,
            task_key: Some("chat.codex".to_string()),
            group_by: ModelRouterUsageGroupBy::Model,
        })
        .await?;
    assert_eq!(summary.totals.request_count, 1);
    assert_eq!(summary.totals.production_request_count, 1);
    assert_eq!(summary.totals.token_usage.total_tokens, 11);
    assert_eq!(summary.groups.len(), 1);
    assert_eq!(summary.groups[0].key, "openai/gpt-5.2");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regular_turn_warns_when_router_changes_non_model_settings() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = mount_sse_once(&server, sse_completed("resp-1")).await;
    let mut builder = test_codex().with_config(|config| {
        config.model = Some(DEFAULT_MODEL.to_string());
        config.model_reasoning_effort = Some(ReasoningEffort::High);
        config.model_router = Some(same_model_service_tier_router());
    });
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_turn(&test, "keep model but route settings"))
        .await?;

    let warning = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Warning(_))).await;
    let EventMsg::Warning(warning) = warning else {
        panic!("expected warning event");
    };
    assert_eq!(
        warning.message,
        "Model router updated this turn: service tier default -> priority, reasoning effort high -> low."
    );

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let body = response_mock.single_request().body_json();
    assert_eq!(body["model"].as_str(), Some(DEFAULT_MODEL));
    assert_eq!(body["service_tier"].as_str(), Some("priority"));
    assert_eq!(
        body.pointer("/reasoning/effort")
            .and_then(|value| value.as_str()),
        Some("low")
    );

    Ok(())
}
