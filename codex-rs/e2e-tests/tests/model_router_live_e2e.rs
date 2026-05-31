use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use chrono::Utc;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_model_provider_info::DEEPSEEK_PROVIDER_ID;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_router::policy::candidate_identity_key;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
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
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

#[derive(Debug, Copy, Clone)]
struct CompletedRolloutSpec<'a> {
    rollout_rel_path: &'a str,
    user_message: &'a str,
    assistant_message: &'a str,
    turn_id: &'a str,
    duration_ms: i64,
    provider: &'a str,
}

#[test]
fn live_model_router_env_credentials_start_openai_and_deepseek_shadowing() -> Result<()> {
    let real_codex_home = real_codex_home()?;
    let Some(credentials) = LiveCredentials::load(&real_codex_home)? else {
        return Ok(());
    };

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let home = TempDir::new()?;
        credentials.seed_openai_auth(home.path())?;
        fs::write(
            home.path().join("config.toml"),
            r#"
model = "gpt-5.4"
model_provider = "openai"
approval_policy = "never"

[features]
sqlite = true

[model_router]
enabled = true
discovery = "from_rules"

[[model_router.models.rules]]
id = "live-openai-deepseek"
type = "require"
models = [
  { provider = "openai", model = "gpt-5.4" },
  { provider = "deepseek", model = "deepseek-chat" },
]
"#,
        )?;

        let state_db =
            StateRuntime::init(home.path().to_path_buf(), OPENAI_PROVIDER_ID.to_string()).await?;
        let thread_id = ThreadId::new();
        let rollout_rel_path =
            format!("sessions/2026/01/27/rollout-2026-01-27T12-00-02-{thread_id}.jsonl");
        let user_message = "Reply with exactly: model router live shadow.";
        let rollout_path = write_completed_rollout(
            home.path(),
            thread_id,
            CompletedRolloutSpec {
                rollout_rel_path: &rollout_rel_path,
                user_message,
                assistant_message: "model router live shadow.",
                turn_id: "live-shadow-turn-1",
                duration_ms: 100,
                provider: OPENAI_PROVIDER_ID,
            },
        )?;
        seed_thread_metadata_for_home(
            &state_db,
            home.path(),
            thread_id,
            rollout_path,
            user_message,
            OPENAI_PROVIDER_ID,
            "gpt-5.4",
        )
        .await?;

        let policy =
            run_model_router_live_cli_json(home.path(), &["policy", "--json"], &credentials)?;
        let policy_candidates = policy["candidates"]
            .as_array()
            .expect("policy candidates should be an array");
        assert!(
            policy_candidates.iter().any(|candidate| {
                candidate["modelProvider"].as_str() == Some(OPENAI_PROVIDER_ID)
                    && candidate["model"].as_str() == Some("gpt-5.4")
            }),
            "policy should discover the OpenAI candidate: {policy:#}"
        );
        assert!(
            policy_candidates.iter().any(|candidate| {
                candidate["modelProvider"].as_str() == Some(DEEPSEEK_PROVIDER_ID)
                    && candidate["model"].as_str() == Some("deepseek-chat")
            }),
            "policy should discover the DeepSeek env-key candidate: {policy:#}"
        );

        let tune = run_model_router_live_cli_json(
            home.path(),
            &[
                "tune",
                "--window",
                "all",
                "--token-budget",
                "8000",
                "--cost-budget-usd",
                "0.10",
                "--dry-run",
                "--json",
            ],
            &credentials,
        )?;
        assert!(
            tune["budgetUsed"]["tokensUsed"]
                .as_i64()
                .is_some_and(|tokens| tokens > 0),
            "tune should spend tokens on live shadow requests: {tune:#}"
        );

        let openai_identity_key = candidate_identity_key(&ModelRouterCandidateToml {
            model: Some("gpt-5.4".to_string()),
            model_provider: Some(OPENAI_PROVIDER_ID.to_string()),
            ..Default::default()
        });
        let deepseek_identity_key = candidate_identity_key(&ModelRouterCandidateToml {
            model: Some("deepseek-chat".to_string()),
            model_provider: Some(DEEPSEEK_PROVIDER_ID.to_string()),
            ..Default::default()
        });
        let recommendations = tune["recommendations"]
            .as_array()
            .expect("tune recommendations should be an array");
        for identity_key in [&openai_identity_key, &deepseek_identity_key] {
            assert!(
                recommendations.iter().any(|recommendation| {
                    recommendation["candidateIdentityKey"].as_str() == Some(identity_key.as_str())
                        && recommendation["evaluatedCount"]
                            .as_i64()
                            .is_some_and(|count| count >= 1)
                        && !recommendation["reason"]
                            .as_str()
                            .unwrap_or_default()
                            .starts_with("evaluation failed")
                }),
                "tune should attempt live shadowing for {identity_key}: {tune:#}"
            );
        }

        Ok::<(), anyhow::Error>(())
    })
}

fn run_model_router_live_cli(
    codex_home: &Path,
    args: &[&str],
    credentials: &LiveCredentials,
) -> Result<std::process::Output> {
    let mut command = Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    command
        .env("CODEX_HOME", codex_home)
        .env("CODEX_SQLITE_HOME", codex_home)
        .arg("model-router")
        .args(args);
    credentials.apply_env(&mut command);
    Ok(command.output()?)
}

fn run_model_router_live_cli_json(
    codex_home: &Path,
    args: &[&str],
    credentials: &LiveCredentials,
) -> Result<Value> {
    let output = run_model_router_live_cli(codex_home, args, credentials)?;
    assert!(
        output.status.success(),
        "live model-router command failed with args {args:?}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

struct LiveCredentials {
    openai: OpenAiCredential,
    deepseek_api_key: String,
}

enum OpenAiCredential {
    ApiKey(String),
    ChatgptTokens(serde_json::Map<String, Value>),
}

impl LiveCredentials {
    fn load(codex_home: &Path) -> Result<Option<Self>> {
        let openai = if let Some(api_key) = optional_env_secret("OPENAI_API_KEY")
            .or_else(|| config_provider_token(codex_home, "openai"))
        {
            Some(OpenAiCredential::ApiKey(api_key))
        } else {
            load_chatgpt_tokens(codex_home)?
        };

        let Some(openai) = openai else {
            eprintln!("skipping live model router e2e because no OpenAI credential is configured");
            return Ok(None);
        };

        let Some(deepseek_api_key) = optional_env_secret("DEEPSEEK_API_KEY")
            .or_else(|| config_provider_token(codex_home, "deepseek"))
        else {
            eprintln!(
                "skipping live model router e2e because no DeepSeek credential is configured"
            );
            return Ok(None);
        };

        Ok(Some(Self {
            openai,
            deepseek_api_key,
        }))
    }

    fn seed_openai_auth(&self, codex_home: &Path) -> Result<()> {
        match &self.openai {
            OpenAiCredential::ApiKey(api_key) => write_api_key_auth(codex_home, api_key),
            OpenAiCredential::ChatgptTokens(tokens) => write_chatgpt_token_auth(codex_home, tokens),
        }
    }

    fn apply_env(&self, command: &mut Command) {
        command.env("DEEPSEEK_API_KEY", &self.deepseek_api_key);
        if let OpenAiCredential::ApiKey(api_key) = &self.openai {
            command.env("OPENAI_API_KEY", api_key);
        }
    }
}

fn real_codex_home() -> Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .context("CODEX_HOME is unset and HOME is unavailable")
}

fn load_chatgpt_tokens(codex_home: &Path) -> Result<Option<OpenAiCredential>> {
    let mut stale_tokens = Vec::new();
    for (label, path) in auth_candidates(codex_home)? {
        let Some(data) = read_json_file(&path)? else {
            continue;
        };
        let Some(tokens) = data.get("tokens").and_then(Value::as_object) else {
            continue;
        };
        let Some(access_token) = tokens
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        match token_expires_at(access_token) {
            Ok(expires_at) if expires_at > Utc::now().timestamp() + 300 => {
                return Ok(Some(OpenAiCredential::ChatgptTokens(tokens.clone())));
            }
            Ok(_) => stale_tokens.push(label),
            Err(err) => {
                eprintln!(
                    "skipping {label} because its access token expiry could not be inspected: {err}"
                );
            }
        }
    }

    if stale_tokens.is_empty() {
        return Ok(None);
    }
    bail!(
        "found only expired or near-expiry Codex access tokens: {}",
        stale_tokens.join(", ")
    )
}

fn write_api_key_auth(codex_home: &Path, api_key: &str) -> Result<()> {
    fs::write(
        codex_home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": api_key,
            "tokens": null,
            "last_refresh": null,
        }))? + "\n",
    )?;
    Ok(())
}

fn write_chatgpt_token_auth(
    codex_home: &Path,
    tokens: &serde_json::Map<String, Value>,
) -> Result<()> {
    let mut isolated_tokens = Value::Object(tokens.clone());
    isolated_tokens["refresh_token"] = json!("");
    fs::write(
        codex_home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "auth_mode": "chatgptAuthTokens",
            "OPENAI_API_KEY": null,
            "tokens": isolated_tokens,
            "last_refresh": Utc::now().to_rfc3339(),
        }))? + "\n",
    )?;
    Ok(())
}

fn optional_env_secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn config_provider_token(codex_home: &Path, provider: &str) -> Option<String> {
    let contents = fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value
        .get("model_providers")?
        .get(provider)?
        .get("token")?
        .as_str()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
}

fn auth_candidates(codex_home: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut candidates = vec![("default".to_string(), codex_home.join("auth.json"))];
    let accounts_dir = codex_home.join("accounts");
    if accounts_dir.is_dir() {
        let mut account_dirs = fs::read_dir(accounts_dir)?
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        account_dirs.sort_by_key(std::fs::DirEntry::file_name);
        candidates.extend(account_dirs.into_iter().map(|entry| {
            (
                format!("account:{}", entry.file_name().to_string_lossy()),
                entry.path().join("auth.json"),
            )
        }));
    }
    Ok(candidates)
}

fn read_json_file(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn token_expires_at(access_token: &str) -> Result<i64> {
    let payload_segment = access_token
        .split('.')
        .nth(1)
        .context("access token is not a JWT")?;
    let mut padded = payload_segment.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let decoded = URL_SAFE.decode(padded)?;
    let payload: Value = serde_json::from_slice(&decoded)?;
    payload["exp"]
        .as_i64()
        .context("access token payload does not contain numeric exp")
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

async fn seed_thread_metadata_for_home(
    state_db: &StateRuntime,
    codex_home: &Path,
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
    builder.cwd = codex_home.to_path_buf();
    builder.cli_version = Some("test".to_string());

    let mut metadata = builder.build(default_provider);
    metadata.model = Some(model.to_string());
    metadata.first_user_message = Some(user_message.to_string());
    metadata.title = user_message.to_string();
    state_db.upsert_thread(&metadata).await?;
    Ok(())
}
