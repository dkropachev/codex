use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_config::config_toml::ModelRouterDiscoveryToml;
use codex_config::config_toml::ModelRouterToml;
use codex_core::model_router_candidate_pool_for_config;
use codex_core::model_router_tune::ModelRouterTuneOptions;
use codex_core::model_router_tune::ModelRouterTuneRuntime;
use codex_core::model_router_tune::tune_model_router;
use codex_features::Feature;
use codex_model_provider_info::DEEPSEEK_PROVIDER_ID;
use codex_model_router::policy::candidate_identity_key;
use codex_models_manager::bundled_models_response;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::ModelProviderAuthInfo;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::UserMessageEvent;
use codex_state::ModelRouterLifecyclePromotionRecord;
use codex_state::ModelRouterShadowEvaluationRecord;
use codex_state::ModelRouterShadowEvaluationSummary;
use codex_state::ModelRouterUsageGroupBy;
use codex_state::ModelRouterUsageQuery;
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const MODEL_ROUTER_TUNE_RESPONSE_TEXT: &str = r#"{"pass":true,"score":1.0,"confidence":1.0}"#;
const MODEL_ROUTER_TUNE_RESPONSE_TOKENS: i64 = 10;

#[derive(Debug, Copy, Clone)]
struct CompletedRolloutSpec<'a> {
    rollout_rel_path: &'a str,
    user_message: &'a str,
    assistant_message: &'a str,
    turn_id: &'a str,
    duration_ms: i64,
    provider: &'a str,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ShadowReportView {
    task_key: Option<String>,
    summaries: Vec<ShadowSummaryView>,
    recent: Vec<ShadowRecordView>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ShadowSummaryView {
    task_key: String,
    phase: String,
    candidate_identity: String,
    base_candidate_identity: String,
    evaluated_count: i64,
    success_count: i64,
    success_rate: f64,
    average_score: Option<f64>,
    average_confidence: f64,
    cost_used_usd_micros: i64,
    tokens_used: i64,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ShadowRecordView {
    task_key: String,
    phase: String,
    candidate_identity: String,
    base_candidate_identity: String,
    success: bool,
    score: Option<f64>,
    confidence: f64,
    cost_usd_micros: i64,
    total_tokens: i64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
async fn model_router_tune_persists_shadow_rows_and_cli_report() -> Result<()> {
    let openai_server = MockServer::start().await;
    let deepseek_server = MockServer::start().await;
    let deepseek_base_url = format!("{}/v1", deepseek_server.uri());

    mount_models_response_n_times(&openai_server, openai_models_response(), 3).await;
    mount_models_response_n_times(&deepseek_server, deepseek_models_response(), 3).await;
    mount_deepseek_chat_response_n_times(&deepseek_server, 2).await;
    let openai_response_mock = mount_sse_sequence(
        &openai_server,
        vec![
            tune_response_sse(),
            tune_response_sse(),
            tune_response_sse(),
            tune_response_sse(),
            tune_response_sse(),
            tune_response_sse(),
        ],
    )
    .await;

    let home = Arc::new(TempDir::new()?);
    let mut builder = test_codex()
        .with_home(home.clone())
        .with_model("gpt-5.4")
        .with_config(move |config| {
            let openai_provider_id = config.model_provider_id.clone();
            let openai_base_url = config.model_provider.base_url.clone();
            if let Err(err) = config.features.enable(Feature::Sqlite) {
                panic!("test config should allow sqlite: {err}");
            }
            config.model_provider.env_key = None;
            config.model_provider.requires_openai_auth = false;
            config.model_provider.supports_websockets = false;
            let timeout_ms = NonZeroU64::new(5_000).unwrap_or_else(|| {
                panic!("timeout should be non-zero");
            });
            config.model_provider.auth = Some(ModelProviderAuthInfo {
                command: if cfg!(windows) {
                    "cmd".to_string()
                } else {
                    "sh".to_string()
                },
                args: if cfg!(windows) {
                    vec!["/C".to_string(), "echo openai-test-token".to_string()]
                } else {
                    vec!["-lc".to_string(), "printf %s openai-test-token".to_string()]
                },
                timeout_ms,
                refresh_interval_ms: 300_000,
                cwd: config.cwd.clone(),
            });
            let openai_provider = match config.model_providers.get_mut(&openai_provider_id) {
                Some(provider) => provider,
                None => panic!("OpenAI provider should be configured"),
            };
            openai_provider.base_url = openai_base_url;
            openai_provider.env_key = None;
            openai_provider.requires_openai_auth = false;
            openai_provider.supports_websockets = false;
            let timeout_ms = NonZeroU64::new(5_000).unwrap_or_else(|| {
                panic!("timeout should be non-zero");
            });
            openai_provider.auth = Some(ModelProviderAuthInfo {
                command: if cfg!(windows) {
                    "cmd".to_string()
                } else {
                    "sh".to_string()
                },
                args: if cfg!(windows) {
                    vec!["/C".to_string(), "echo openai-test-token".to_string()]
                } else {
                    vec!["-lc".to_string(), "printf %s openai-test-token".to_string()]
                },
                timeout_ms,
                refresh_interval_ms: 300_000,
                cwd: config.cwd.clone(),
            });
            config.model_router = Some(ModelRouterToml {
                enabled: true,
                candidates: Vec::new(),
                ..Default::default()
            });

            let deepseek = match config.model_providers.get_mut(DEEPSEEK_PROVIDER_ID) {
                Some(provider) => provider,
                None => panic!("DeepSeek provider should be built in"),
            };
            deepseek.base_url = Some(deepseek_base_url);
            deepseek.env_key = None;
            let timeout_ms = NonZeroU64::new(5_000).unwrap_or_else(|| {
                panic!("timeout should be non-zero");
            });
            deepseek.auth = Some(ModelProviderAuthInfo {
                command: if cfg!(windows) {
                    "cmd".to_string()
                } else {
                    "sh".to_string()
                },
                args: if cfg!(windows) {
                    vec!["/C".to_string(), "echo deepseek-test-token".to_string()]
                } else {
                    vec![
                        "-lc".to_string(),
                        "printf %s deepseek-test-token".to_string(),
                    ]
                },
                timeout_ms,
                refresh_interval_ms: 300_000,
                cwd: config.cwd.clone(),
            });
            config.model_providers.retain(|provider_id, _| {
                provider_id == &openai_provider_id || provider_id == DEEPSEEK_PROVIDER_ID
            });
        });
    let test = builder.build(&openai_server).await?;
    let Some(state_db) = test.codex.state_db() else {
        panic!("state db enabled");
    };
    let models_manager = test.thread_manager.get_models_manager();
    let auth_manager = test.thread_manager.auth_manager();
    let default_provider_id = test.config.model_provider_id.clone();
    let Some(incumbent_model) = test.config.model.as_deref() else {
        panic!("test config should set a model");
    };
    let incumbent_model = incumbent_model.to_string();

    let _ = models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;

    let spark_thread_id = ThreadId::new();
    let spark_rollout_rel_path =
        format!("sessions/2026/01/27/rollout-2026-01-27T12-00-00-{spark_thread_id}.jsonl");
    let spark_rollout_path = write_completed_rollout(
        test.codex_home_path(),
        spark_thread_id,
        CompletedRolloutSpec {
            rollout_rel_path: &spark_rollout_rel_path,
            user_message: "spark replay case",
            assistant_message: "historical spark answer",
            turn_id: "spark-turn-1",
            duration_ms: 100,
            provider: &default_provider_id,
        },
    )?;

    let deepseek_thread_id = ThreadId::new();
    let deepseek_rollout_rel_path =
        format!("sessions/2026/01/27/rollout-2026-01-27T12-00-01-{deepseek_thread_id}.jsonl");
    let deepseek_rollout_path = write_completed_rollout(
        test.codex_home_path(),
        deepseek_thread_id,
        CompletedRolloutSpec {
            rollout_rel_path: &deepseek_rollout_rel_path,
            user_message: "deepseek replay case",
            assistant_message: "historical deepseek answer",
            turn_id: "deepseek-turn-1",
            duration_ms: 200,
            provider: &default_provider_id,
        },
    )?;

    seed_thread_metadata(
        &state_db,
        &test,
        spark_thread_id,
        spark_rollout_path,
        "spark replay case",
        &default_provider_id,
        &incumbent_model,
    )
    .await?;
    seed_thread_metadata(
        &state_db,
        &test,
        deepseek_thread_id,
        deepseek_rollout_path,
        "deepseek replay case",
        &default_provider_id,
        &incumbent_model,
    )
    .await?;

    let discovered_candidates =
        match model_router_candidate_pool_for_config(&test.config, &models_manager).await {
            Ok(candidates) => candidates,
            Err(err) => panic!("candidate pool should build: {err}"),
        };

    let Some(spark_candidate) = discovered_candidates.iter().find(|candidate| {
        candidate.model.as_deref() == Some("gpt-5.3-codex-spark")
            && candidate.model_provider.as_deref() == Some(&default_provider_id)
    }) else {
        panic!("spark candidate should be discovered");
    };
    assert!(
        discovered_candidates
            .iter()
            .any(|candidate| { candidate.model_provider.as_deref() == Some(DEEPSEEK_PROVIDER_ID) })
    );

    let deepseek_candidate = ModelRouterCandidateToml {
        id: Some("auto:deepseek:deepseek-chat".to_string()),
        model: Some("deepseek-chat".to_string()),
        model_provider: Some(DEEPSEEK_PROVIDER_ID.to_string()),
        ..Default::default()
    };

    let spark_identity_key = candidate_identity_key(spark_candidate);
    let deepseek_identity_key = candidate_identity_key(&deepseek_candidate);
    let historical_identity_key =
        historical_production_identity_key(&default_provider_id, &incumbent_model);
    let tune_candidates = vec![spark_candidate.clone(), deepseek_candidate.clone()];
    let expected_identity_keys =
        BTreeSet::from([spark_identity_key.clone(), deepseek_identity_key.clone()]);

    let mut tune_config = test.config.clone();
    tune_config.model_router = Some(ModelRouterToml {
        enabled: true,
        candidates: tune_candidates,
        discovery: Some(ModelRouterDiscoveryToml::Manual),
        ..Default::default()
    });

    state_db
        .upsert_model_router_lifecycle_promotion(ModelRouterLifecyclePromotionRecord {
            task_key: "history.cli".to_string(),
            candidate_identity: spark_identity_key.clone(),
            base_candidate_identity: historical_identity_key.clone(),
            status: "promoted".to_string(),
            rule_id: Some("spark-promotion".to_string()),
            production_model_provider: Some(default_provider_id.clone()),
            production_model: Some(incumbent_model.clone()),
            base_model_provider: Some(default_provider_id.clone()),
            base_model: Some(incumbent_model.clone()),
            promoted_at_ms: Utc::now().timestamp_millis(),
            updated_at_ms: Utc::now().timestamp_millis(),
            reason: Some("seeded promotion for monitoring phase".to_string()),
        })
        .await?;

    let runtime = ModelRouterTuneRuntime::new(auth_manager, models_manager.clone());
    let report = tune_model_router(
        &state_db,
        &tune_config,
        ModelRouterTuneOptions {
            window: "all".to_string(),
            cost_budget_usd: 10.0,
            token_budget: 10_000,
            dry_run: false,
        },
        Some(runtime),
    )
    .await?;

    let report_identity_keys = report
        .candidates
        .iter()
        .map(|candidate| candidate.identity_key.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(report_identity_keys, expected_identity_keys);
    assert_eq!(report.evaluated_count, 2);
    assert_eq!(report.recommendations.len(), 2);

    let openai_requests = openai_response_mock.requests();
    let deepseek_requests = deepseek_server
        .received_requests()
        .await
        .unwrap_or_default();

    assert_eq!(report.budget_used.tokens_used, 80);
    assert_eq!(report.budget_used.cost_used_usd_micros, 0);

    assert_eq!(openai_requests.len(), 6);
    let openai_model_counts = request_model_counts(&openai_requests);
    assert_eq!(openai_model_counts.get("gpt-5.3-codex-spark"), Some(&2));
    assert_eq!(openai_model_counts.get("gpt-5.4"), Some(&4));

    let deepseek_models = deepseek_requests
        .iter()
        .filter(|request| {
            request.method == wiremock::http::Method::GET && request.url.path().ends_with("/models")
        })
        .count();
    assert_eq!(deepseek_models, 1);

    let deepseek_chat_requests = deepseek_requests
        .iter()
        .filter(|request| {
            request.method == wiremock::http::Method::POST
                && request.url.path().ends_with("/chat/completions")
        })
        .collect::<Vec<_>>();
    assert_eq!(deepseek_chat_requests.len(), 2);
    for request in deepseek_chat_requests {
        let body: Value = serde_json::from_slice(&request.body)?;
        assert_eq!(body["model"].as_str(), Some("deepseek-chat"));
    }

    let mut shadow_summary_views = state_db
        .model_router_shadow_evaluation_summaries(Some("history.cli"))
        .await?
        .into_iter()
        .map(ShadowSummaryView::from)
        .collect::<Vec<_>>();
    sort_summary_views(&mut shadow_summary_views);
    assert_eq!(
        shadow_summary_views,
        vec![
            ShadowSummaryView {
                task_key: "history.cli".to_string(),
                phase: "monitoring".to_string(),
                candidate_identity: spark_identity_key.clone(),
                base_candidate_identity: historical_identity_key.clone(),
                evaluated_count: 2,
                success_count: 2,
                success_rate: 1.0,
                average_score: Some(1.0),
                average_confidence: 1.0,
                cost_used_usd_micros: 0,
                tokens_used: 20,
            },
            ShadowSummaryView {
                task_key: "history.cli".to_string(),
                phase: "promotion".to_string(),
                candidate_identity: deepseek_identity_key.clone(),
                base_candidate_identity: historical_identity_key.clone(),
                evaluated_count: 2,
                success_count: 2,
                success_rate: 1.0,
                average_score: Some(1.0),
                average_confidence: 1.0,
                cost_used_usd_micros: 0,
                tokens_used: 20,
            },
        ]
    );

    let mut shadow_recent_views = state_db
        .model_router_shadow_evaluations(Some("history.cli"), 10)
        .await?
        .into_iter()
        .map(ShadowRecordView::from)
        .collect::<Vec<_>>();
    sort_record_views(&mut shadow_recent_views);
    assert_eq!(
        shadow_recent_views,
        vec![
            ShadowRecordView {
                task_key: "history.cli".to_string(),
                phase: "monitoring".to_string(),
                candidate_identity: spark_identity_key.clone(),
                base_candidate_identity: historical_identity_key.clone(),
                success: true,
                score: Some(1.0),
                confidence: 1.0,
                cost_usd_micros: 0,
                total_tokens: 10,
            },
            ShadowRecordView {
                task_key: "history.cli".to_string(),
                phase: "monitoring".to_string(),
                candidate_identity: spark_identity_key.clone(),
                base_candidate_identity: historical_identity_key.clone(),
                success: true,
                score: Some(1.0),
                confidence: 1.0,
                cost_usd_micros: 0,
                total_tokens: 10,
            },
            ShadowRecordView {
                task_key: "history.cli".to_string(),
                phase: "promotion".to_string(),
                candidate_identity: deepseek_identity_key.clone(),
                base_candidate_identity: historical_identity_key.clone(),
                success: true,
                score: Some(1.0),
                confidence: 1.0,
                cost_usd_micros: 0,
                total_tokens: 10,
            },
            ShadowRecordView {
                task_key: "history.cli".to_string(),
                phase: "promotion".to_string(),
                candidate_identity: deepseek_identity_key.clone(),
                base_candidate_identity: historical_identity_key.clone(),
                success: true,
                score: Some(1.0),
                confidence: 1.0,
                cost_usd_micros: 0,
                total_tokens: 10,
            },
        ]
    );

    let usage_summary = state_db
        .model_router_usage_summary(ModelRouterUsageQuery {
            window_start_ms: None,
            window_end_ms: i64::MAX,
            task_key: Some("history.cli".to_string()),
            group_by: ModelRouterUsageGroupBy::RequestKind,
        })
        .await?;
    assert_eq!(usage_summary.totals.request_count, 8);
    assert_eq!(usage_summary.totals.production_request_count, 0);
    assert_eq!(usage_summary.totals.overhead_request_count, 8);
    assert_eq!(usage_summary.totals.token_usage.total_tokens, 80);
    let usage_group_counts = usage_summary
        .groups
        .iter()
        .map(|group| (group.key.clone(), group.totals.request_count))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(usage_group_counts.get("judge"), Some(&4));
    assert_eq!(usage_group_counts.get("shadow"), Some(&4));

    let mut cli_candidates_toml = String::new();
    for candidate in [spark_candidate, &deepseek_candidate] {
        cli_candidates_toml.push_str("\n[[model_router.candidates]]\n");
        if let Some(id) = &candidate.id {
            cli_candidates_toml.push_str(&format!("id = \"{}\"\n", toml_escape(id)));
        }
        if let Some(model) = &candidate.model {
            cli_candidates_toml.push_str(&format!("model = \"{}\"\n", toml_escape(model)));
        }
        if let Some(model_provider) = &candidate.model_provider {
            cli_candidates_toml.push_str(&format!(
                "model_provider = \"{}\"\n",
                toml_escape(model_provider)
            ));
        }
        if let Some(account_pool) = &candidate.account_pool {
            cli_candidates_toml.push_str(&format!(
                "account_pool = \"{}\"\n",
                toml_escape(account_pool)
            ));
        }
        if let Some(account) = &candidate.account {
            cli_candidates_toml.push_str(&format!("account = \"{}\"\n", toml_escape(account)));
        }
    }
    fs::write(
        test.codex_home_path().join("config.toml"),
        format!(
            r#"
model = "{incumbent_model}"
model_provider = "{default_provider_id}"
approval_policy = "never"

[model_router]
enabled = true
discovery = "manual"
{cli_candidates_toml}
"#
        ),
    )?;

    let cli_policy = run_model_router_cli_json(
        test.codex_home_path(),
        &["policy", "--task-key", "history.cli", "--json"],
    )?;
    assert_eq!(cli_policy["enabled"].as_bool(), Some(true));
    assert!(
        cli_policy["candidates"]
            .as_array()
            .expect("policy candidates should be an array")
            .iter()
            .any(|candidate| candidate["identityKey"].as_str() == Some(&spark_identity_key))
    );

    let cli_usage = run_model_router_cli_json(
        test.codex_home_path(),
        &[
            "usage",
            "--window",
            "all",
            "--task-key",
            "history.cli",
            "--group-by",
            "request-kind",
            "--json",
        ],
    )?;
    assert_eq!(
        cli_usage["summary"]["totals"]["requestCount"].as_i64(),
        Some(8)
    );
    assert!(
        cli_usage["summary"]["groups"]
            .as_array()
            .expect("usage groups should be an array")
            .iter()
            .any(|group| group["key"].as_str() == Some("shadow"))
    );

    let cli_lifecycle = run_model_router_cli_json(
        test.codex_home_path(),
        &[
            "lifecycle",
            "--task-key",
            "history.cli",
            "--events",
            "--json",
        ],
    )?;
    assert!(
        cli_lifecycle["promotions"]
            .as_array()
            .expect("lifecycle promotions should be an array")
            .iter()
            .any(|promotion| {
                promotion["candidateIdentity"].as_str() == Some(&spark_identity_key)
                    && promotion["status"].as_str() == Some("promoted")
            })
    );

    let cli_report_value =
        run_model_router_cli_json(test.codex_home_path(), &["shadows", "--json"])?;
    let mut cli_report: ShadowReportView = serde_json::from_value(cli_report_value)?;
    assert_eq!(cli_report.task_key, None);
    sort_summary_views(&mut cli_report.summaries);
    sort_record_views(&mut cli_report.recent);
    assert_eq!(cli_report.summaries, shadow_summary_views);
    assert_eq!(cli_report.recent, shadow_recent_views);

    let report_path = test.codex_home_path().join("model-router-report.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    let report_path_arg = report_path.to_string_lossy().into_owned();
    let cli_report_show = run_model_router_cli_json(
        test.codex_home_path(),
        &["report", "show", &report_path_arg, "--json"],
    )?;
    assert_eq!(cli_report_show["evaluatedCount"].as_i64(), Some(2));

    let cli_report_apply = run_model_router_cli_json(
        test.codex_home_path(),
        &["report", "apply", &report_path_arg, "--dry-run", "--json"],
    )?;
    assert_eq!(cli_report_apply["dryRun"].as_bool(), Some(true));
    assert!(
        cli_report_apply["appliedRecommendations"]
            .as_i64()
            .is_some()
    );

    let cli_promote = run_model_router_cli_json(
        test.codex_home_path(),
        &[
            "promote",
            "--task-key",
            "history.cli",
            "--candidate-identity",
            &deepseek_identity_key,
            "--base-candidate-identity",
            &historical_identity_key,
            "--reason",
            "cli test promotion",
            "--json",
        ],
    )?;
    assert_eq!(
        cli_promote["candidateIdentity"].as_str(),
        Some(deepseek_identity_key.as_str())
    );
    assert_eq!(cli_promote["status"].as_str(), Some("promoted"));

    let cli_demote = run_model_router_cli_json(
        test.codex_home_path(),
        &[
            "demote",
            "--task-key",
            "history.cli",
            "--candidate-identity",
            &deepseek_identity_key,
            "--reason",
            "cli test demotion",
            "--json",
        ],
    )?;
    assert!(
        cli_demote
            .as_array()
            .expect("demote should print promotion rows")
            .iter()
            .any(|promotion| {
                promotion["candidateIdentity"].as_str() == Some(&deepseek_identity_key)
                    && promotion["status"].as_str() == Some("demoted")
            })
    );

    Ok(())
}

fn request_model_counts(requests: &[responses::ResponsesRequest]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for request in requests {
        let model = request
            .body_json()
            .get("model")
            .and_then(Value::as_str)
            .map_or_else(
                || panic!("responses request should include a model"),
                ToString::to_string,
            );
        *counts.entry(model).or_insert(0) += 1;
    }
    counts
}

fn run_model_router_cli_json(codex_home: &Path, args: &[&str]) -> Result<Value> {
    let output = Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .env("CODEX_HOME", codex_home)
        .env("CODEX_SQLITE_HOME", codex_home)
        .arg("model-router")
        .args(args)
        .output()?;
    assert!(
        output.status.success(),
        "model-router command failed with args {args:?}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn sort_summary_views(values: &mut [ShadowSummaryView]) {
    values.sort_by(|left, right| {
        (
            left.task_key.as_str(),
            left.phase.as_str(),
            left.candidate_identity.as_str(),
            left.base_candidate_identity.as_str(),
        )
            .cmp(&(
                right.task_key.as_str(),
                right.phase.as_str(),
                right.candidate_identity.as_str(),
                right.base_candidate_identity.as_str(),
            ))
    });
}

fn sort_record_views(values: &mut [ShadowRecordView]) {
    values.sort_by(|left, right| {
        (
            left.task_key.as_str(),
            left.phase.as_str(),
            left.candidate_identity.as_str(),
            left.base_candidate_identity.as_str(),
            left.total_tokens,
            left.success,
        )
            .cmp(&(
                right.task_key.as_str(),
                right.phase.as_str(),
                right.candidate_identity.as_str(),
                right.base_candidate_identity.as_str(),
                right.total_tokens,
                right.success,
            ))
    });
}

fn openai_models_response() -> codex_protocol::openai_models::ModelsResponse {
    let bundled_models = bundled_models_response().unwrap_or_else(|err| {
        panic!("bundled model catalog should parse: {err}");
    });
    let Some(gpt_54) = bundled_models
        .models
        .iter()
        .find(|model| model.slug == "gpt-5.4")
        .cloned()
    else {
        panic!("bundled gpt-5.4 model should exist");
    };
    codex_protocol::openai_models::ModelsResponse {
        models: vec![gpt_54, model_info_from_slug("gpt-5.3-codex-spark")],
    }
}

fn deepseek_models_response() -> codex_protocol::openai_models::ModelsResponse {
    codex_protocol::openai_models::ModelsResponse {
        models: vec![model_info_from_slug("deepseek-chat")],
    }
}

fn tune_response_sse() -> String {
    responses::sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", MODEL_ROUTER_TUNE_RESPONSE_TEXT),
        ev_completed_with_tokens("resp-1", MODEL_ROUTER_TUNE_RESPONSE_TOKENS),
    ])
}

async fn mount_models_response_n_times(
    server: &MockServer,
    body: codex_protocol::openai_models::ModelsResponse,
    times: u64,
) {
    Mock::given(method("GET"))
        .and(path_regex(".*/models$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(body),
        )
        .up_to_n_times(times)
        .mount(server)
        .await;
}

async fn mount_deepseek_chat_response_n_times(server: &MockServer, times: u64) {
    let body = deepseek_chat_sse_response();
    Mock::given(method("POST"))
        .and(path_regex(".*/chat/completions$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .up_to_n_times(times)
        .expect(times)
        .mount(server)
        .await;
}

fn deepseek_chat_sse_response() -> String {
    let content = serde_json::to_string(MODEL_ROUTER_TUNE_RESPONSE_TEXT).unwrap_or_else(|err| {
        panic!("assistant output text should serialize: {err}");
    });
    format!(
        "data: {{\"id\":\"chatcmpl-1\",\"model\":\"deepseek-chat\",\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\n\
         data: {{\"id\":\"chatcmpl-1\",\"choices\":[],\"usage\":{{\"prompt_tokens\":{MODEL_ROUTER_TUNE_RESPONSE_TOKENS},\"completion_tokens\":0,\"total_tokens\":{MODEL_ROUTER_TUNE_RESPONSE_TOKENS}}}}}\n\n\
         data: [DONE]\n\n",
    )
}

fn write_completed_rollout(
    codex_home: &Path,
    thread_id: ThreadId,
    spec: CompletedRolloutSpec<'_>,
) -> Result<PathBuf> {
    let rollout_path = codex_home.join(spec.rollout_rel_path);
    if let Some(parent) = rollout_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let session_timestamp = "2026-01-27T12:00:00Z".to_string();
    let rollout_lines = vec![
        RolloutLine {
            timestamp: session_timestamp.clone(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    id: thread_id,
                    forked_from_id: None,
                    timestamp: session_timestamp,
                    cwd: codex_home.to_path_buf(),
                    originator: "test".to_string(),
                    cli_version: "test".to_string(),
                    source: SessionSource::Cli,
                    thread_source: None,
                    agent_nickname: None,
                    agent_role: None,
                    agent_path: None,
                    model_provider: Some(spec.provider.to_string()),
                    base_instructions: None,
                    dynamic_tools: None,
                    memory_mode: None,
                },
                git: None,
            }),
        },
        RolloutLine {
            timestamp: "2026-01-27T12:00:01Z".to_string(),
            item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: spec.user_message.to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            })),
        },
        RolloutLine {
            timestamp: "2026-01-27T12:00:02Z".to_string(),
            item: RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: spec.turn_id.to_string(),
                started_at: None,
                model_context_window: Some(272_000),
                collaboration_mode_kind: ModeKind::Default,
            })),
        },
        RolloutLine {
            timestamp: "2026-01-27T12:00:03Z".to_string(),
            item: RolloutItem::ResponseItem(ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: spec.assistant_message.to_string(),
                }],
                phase: None,
            }),
        },
        RolloutLine {
            timestamp: "2026-01-27T12:00:04Z".to_string(),
            item: RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: spec.turn_id.to_string(),
                last_agent_message: None,
                completed_at: None,
                duration_ms: Some(spec.duration_ms),
                time_to_first_token_ms: None,
            })),
        },
    ];

    let jsonl = rollout_lines
        .into_iter()
        .map(|line| {
            serde_json::to_string(&line).unwrap_or_else(|err| {
                panic!("rollout line should serialize: {err}");
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&rollout_path, format!("{jsonl}\n"))?;
    Ok(rollout_path)
}

async fn seed_thread_metadata(
    state_db: &StateRuntime,
    test: &core_test_support::test_codex::TestCodex,
    thread_id: ThreadId,
    rollout_path: PathBuf,
    user_message: &str,
    default_provider: &str,
    model: &str,
) -> Result<()> {
    let now = Utc::now();
    let mut builder = ThreadMetadataBuilder::new(thread_id, rollout_path, now, SessionSource::Cli);
    builder.updated_at = Some(now);
    builder.model_provider = Some(default_provider.to_string());
    builder.cwd = test.codex_home_path().to_path_buf();
    builder.cli_version = Some("test".to_string());

    let mut metadata = builder.build(default_provider);
    metadata.model = Some(model.to_string());
    metadata.first_user_message = Some(user_message.to_string());
    metadata.title = user_message.to_string();
    state_db.upsert_thread(&metadata).await?;
    Ok(())
}

fn historical_production_identity_key(provider: &str, model: &str) -> String {
    candidate_identity_key(&codex_config::config_toml::ModelRouterCandidateToml {
        id: Some("historical_production".to_string()),
        model: Some(model.to_string()),
        model_provider: Some(provider.to_string()),
        ..Default::default()
    })
}

impl From<ModelRouterShadowEvaluationSummary> for ShadowSummaryView {
    fn from(value: ModelRouterShadowEvaluationSummary) -> Self {
        Self {
            task_key: value.task_key,
            phase: value.phase,
            candidate_identity: value.candidate_identity,
            base_candidate_identity: value.base_candidate_identity,
            evaluated_count: value.evaluated_count,
            success_count: value.success_count,
            success_rate: value.success_rate,
            average_score: value.average_score,
            average_confidence: value.average_confidence,
            cost_used_usd_micros: value.cost_used_usd_micros,
            tokens_used: value.tokens_used,
        }
    }
}

impl From<ModelRouterShadowEvaluationRecord> for ShadowRecordView {
    fn from(value: ModelRouterShadowEvaluationRecord) -> Self {
        Self {
            task_key: value.task_key,
            phase: value.phase,
            candidate_identity: value.candidate_identity,
            base_candidate_identity: value.base_candidate_identity,
            success: value.success,
            score: value.score,
            confidence: value.confidence,
            cost_usd_micros: value.cost_usd_micros,
            total_tokens: value.total_tokens,
        }
    }
}
