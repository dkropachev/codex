use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::DeveloperPromptBlocks;
use codex_app_server_protocol::DeveloperPromptPolicy;
use codex_app_server_protocol::InstructionPolicy;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PromptBlockMode;
use codex_app_server_protocol::PromptContextPolicy;
use codex_app_server_protocol::PromptContextPreset;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadPromptContextReadParams;
use codex_app_server_protocol::ThreadPromptContextReadResponse;
use codex_app_server_protocol::ThreadPromptContextUpdateParams;
use codex_app_server_protocol::ThreadPromptContextUpdateResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ToolPolicy;
use codex_app_server_protocol::ToolSetPolicy;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserContextBlocks;
use codex_app_server_protocol::UserContextPromptPolicy;
use codex_app_server_protocol::UserInput as V2UserInput;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::collections::BTreeMap;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[tokio::test]
async fn prompt_context_and_tool_policy_shape_responses_request() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message("msg-1", "Done"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 200_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_request = mcp
        .send_thread_start_request(ThreadStartParams {
            prompt_context: Some(PromptContextPolicy {
                system_instructions: Some(InstructionPolicy::Set {
                    text: "SYSTEM_TEST".to_string(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;
    let thread_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_request)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(thread_response)?;

    let turn_request = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            prompt_context: Some(PromptContextPolicy {
                preset: Some(PromptContextPreset::Minimal),
                developer: Some(DeveloperPromptPolicy {
                    instructions: Some(InstructionPolicy::Set {
                        text: "TURN_DEVELOPER_TEST".to_string(),
                    }),
                    blocks: Some(DeveloperPromptBlocks {
                        permissions: Some(PromptBlockMode::Omit),
                        ..Default::default()
                    }),
                }),
                user_context: Some(UserContextPromptPolicy {
                    blocks: Some(UserContextBlocks {
                        environment: Some(PromptBlockMode::Omit),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            tool_policy: Some(ToolPolicy {
                builtins: Some(ToolSetPolicy::None),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_request)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let developer_text = request.message_input_texts("developer").join("\n");
    let body = request.body_json();
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert_eq!(request.instructions_text(), "SYSTEM_TEST");
    assert!(
        developer_text.contains("TURN_DEVELOPER_TEST"),
        "developer prompt should contain turn-level promptContext instructions: {developer_text}"
    );
    assert!(
        !developer_text.contains("Approval policy"),
        "developer prompt should omit permissions block: {developer_text}"
    );
    assert!(
        !body.to_string().contains("<environment_context>"),
        "request should omit environment context: {body}"
    );
    assert_eq!(tools, Vec::<Value>::new());

    Ok(())
}

#[tokio::test]
async fn thread_prompt_context_read_and_update_use_server_resolved_instructions() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message("msg-1", "Done"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 200_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_request = mcp
        .send_thread_start_request(ThreadStartParams {
            base_instructions: Some("BASE_START".to_string()),
            developer_instructions: Some("DEV_START".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_request)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(thread_response)?;

    let read_request = mcp
        .send_thread_prompt_context_read_request(ThreadPromptContextReadParams {
            thread_id: thread.id.clone(),
        })
        .await?;
    let read_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_request)),
    )
    .await??;
    let initial: ThreadPromptContextReadResponse = to_response(read_response)?;
    assert_eq!(
        initial,
        ThreadPromptContextReadResponse {
            system_instructions: "BASE_START".to_string(),
            developer_instructions: "DEV_START".to_string(),
            user_instructions: String::new(),
        }
    );

    let update_request = mcp
        .send_thread_prompt_context_update_request(ThreadPromptContextUpdateParams {
            thread_id: thread.id.clone(),
            prompt_context: Some(PromptContextPolicy {
                system_instructions: Some(InstructionPolicy::Set {
                    text: "SYSTEM_UPDATED".to_string(),
                }),
                developer: Some(DeveloperPromptPolicy {
                    instructions: Some(InstructionPolicy::Set {
                        text: "DEV_UPDATED".to_string(),
                    }),
                    ..Default::default()
                }),
                user_context: Some(UserContextPromptPolicy {
                    instructions: Some(InstructionPolicy::Set {
                        text: "USER_UPDATED".to_string(),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            tool_policy: Some(ToolPolicy {
                builtins: Some(ToolSetPolicy::None),
                ..Default::default()
            }),
        })
        .await?;
    let update_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(update_request)),
    )
    .await??;
    let _: ThreadPromptContextUpdateResponse = to_response(update_response)?;

    let read_request = mcp
        .send_thread_prompt_context_read_request(ThreadPromptContextReadParams {
            thread_id: thread.id.clone(),
        })
        .await?;
    let read_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_request)),
    )
    .await??;
    let updated: ThreadPromptContextReadResponse = to_response(read_response)?;
    assert_eq!(
        updated,
        ThreadPromptContextReadResponse {
            system_instructions: "SYSTEM_UPDATED".to_string(),
            developer_instructions: "DEV_UPDATED".to_string(),
            user_instructions: "USER_UPDATED".to_string(),
        }
    );

    let turn_request = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_request)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let developer_text = request.message_input_texts("developer").join("\n");
    let body = request.body_json();
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert_eq!(request.instructions_text(), "SYSTEM_UPDATED");
    assert!(
        developer_text.contains("DEV_UPDATED"),
        "developer prompt should use updated promptContext instructions: {developer_text}"
    );
    assert!(
        body.to_string().contains("USER_UPDATED"),
        "request should use updated user-context instructions: {body}"
    );
    assert_eq!(tools, Vec::<Value>::new());

    Ok(())
}

#[tokio::test]
async fn strict_prompt_context_rejects_unhonorable_policy_by_default() -> Result<()> {
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 200_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_thread_start_request(ThreadStartParams {
            prompt_context: Some(PromptContextPolicy {
                user_context: Some(UserContextPromptPolicy {
                    blocks: Some(UserContextBlocks {
                        environment: Some(PromptBlockMode::Omit),
                        subagents: Some(PromptBlockMode::Include),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.id, RequestId::Integer(request_id));
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        error.error.message,
        "promptContext.userContext.blocks.subagents=include cannot be honored while userContext.blocks.environment=omit"
    );

    Ok(())
}

#[tokio::test]
async fn strict_false_allows_best_effort_prompt_context_policy() -> Result<()> {
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 200_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_thread_start_request(ThreadStartParams {
            prompt_context: Some(PromptContextPolicy {
                user_context: Some(UserContextPromptPolicy {
                    blocks: Some(UserContextBlocks {
                        environment: Some(PromptBlockMode::Omit),
                        subagents: Some(PromptBlockMode::Include),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                strict: false,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: ThreadStartResponse = to_response(response)?;

    Ok(())
}

#[tokio::test]
async fn tool_policy_rejects_unknown_builtin_tools() -> Result<()> {
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 200_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_thread_start_request(ThreadStartParams {
            tool_policy: Some(ToolPolicy {
                builtins: Some(ToolSetPolicy::AllowOnly {
                    tools: vec!["not_a_builtin_tool".to_string()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.id, RequestId::Integer(request_id));
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        error.error.message,
        "toolPolicy.builtins references unknown tool(s): not_a_builtin_tool"
    );

    Ok(())
}
