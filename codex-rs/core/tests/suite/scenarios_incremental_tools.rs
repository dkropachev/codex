//! Request-history coverage for opt-in incremental tools on Responses Lite.

use anyhow::Result;
use codex_core::ConfigRefreshOutcome;
use codex_core::StartThreadOptions;
use codex_features::Feature;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_history::RolloutItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::openai_models::CodeModeToolMessages;
use codex_protocol::openai_models::IncrementalToolMessages;
use codex_protocol::openai_models::ToolMessage;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_thread_store::LoadThreadHistoryParams;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use serde_json::json;
use std::sync::Arc;
use test_case::test_case;

use super::super::rmcp_client::remote_aware_environment_id;
use super::super::rmcp_client::remote_aware_stdio_server_bin;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_large_catalog_removal_keeps_notices_separate() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let mut test = test_codex()
        .with_model_info_override("gpt-5.4", |model| {
            model.use_responses_lite = true;
            model.tool_mode = Some(ToolMode::Direct);
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::IncrementalTools)
                .expect("enable incremental tools");
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex.shutdown_and_wait().await?;
    let namespace = |names: Vec<String>| {
        DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
            name: "service".to_string(),
            description: "Service tools.".to_string(),
            tools: names
                .into_iter()
                .map(|name| {
                    DynamicToolNamespaceTool::Function(DynamicToolFunctionSpec {
                        name,
                        description: "Look up a value.".to_string(),
                        input_schema: json!({"type": "object", "properties": {}}),
                        defer_loading: false,
                    })
                })
                .collect(),
        })
    };
    let removed = (0..100)
        .map(|index| format!("lookup_{index:03}"))
        .collect::<Vec<_>>();
    let initial = test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![namespace(
                std::iter::once("retained".to_string())
                    .chain(removed.iter().cloned())
                    .collect(),
            )],
            environments: Some(vec![test.executor_environment().request()]),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?;
    test.codex = initial.thread;
    test.session_configured = initial.session_configured;
    let first_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("initial")]),
    )
    .await;
    test.submit_turn("Start with the full service catalog.")
        .await?;
    let original = first_mock.single_request().input();
    test.codex.shutdown_and_wait().await?;
    let history = test
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    let resumed = test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![namespace(vec!["retained".to_string()])],
            initial_history: InitialHistory::Resumed(ResumedHistory {
                history_revision: None,
                conversation_id: history.thread_id,
                history: Arc::new(history.items),
                rollout_path: test.session_configured.rollout_path.clone(),
            }),
            environments: Some(vec![test.executor_environment().request()]),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?;
    test.codex = resumed.thread;
    test.session_configured = resumed.session_configured;
    let second_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("resumed")]),
    )
    .await;
    test.submit_turn("Continue after the service catalog shrinks.")
        .await?;
    let input = second_mock.single_request().input();
    assert_eq!(&input[..original.len()], original.as_slice());
    let notices = input[original.len()..]
        .iter()
        .filter(|item| {
            item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                == json!(["tools.removed_definition"])
        })
        .collect::<Vec<_>>();
    assert!(notices.len() > 1);
    let mut actual = Vec::new();
    for notice in notices {
        assert_eq!(notice["role"], "developer");
        let content = notice["content"].as_array().expect("message content");
        assert_eq!(content.len(), 1);
        let text = content[0]["text"].as_str().expect("removal text");
        assert!(text.len() <= 768);
        actual.extend(
            text.lines()
                .filter_map(|line| line.strip_prefix("- "))
                .map(str::to_string),
        );
    }
    assert_eq!(
        actual,
        removed
            .into_iter()
            .map(|name| format!("service.{name}"))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test_case(true; "responses_lite")]
#[test_case(false; "responses_api")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_tools_append_changed_catalog_without_rewriting_history(
    use_responses_lite: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        (1..=4)
            .map(|index| responses::sse(vec![responses::ev_completed(&format!("resp-{index}"))]))
            .collect(),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", move |model| {
            model.use_responses_lite = use_responses_lite;
            model.tool_mode = Some(ToolMode::CodeMode);
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::IncrementalTools)
                .expect("enable incremental tools");
            config.base_instructions =
                Some("Use the available tools to help the user.".to_string());
            config.code_mode.disable_in_process_fallback = true;
            let catalog = config.model_catalog.as_mut().expect("model catalog");
            let mut updated = catalog
                .models
                .iter()
                .find(|model| model.slug == "gpt-5.4")
                .expect("source model")
                .clone();
            updated.slug = "updated-tools-model".to_string();
            updated
                .model_messages
                .get_or_insert_default()
                .tools
                .get_or_insert_default()
                .code_mode = Some(CodeModeToolMessages {
                exec: Some(ToolMessage {
                    description: Some("Updated execution instructions.".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            catalog.models.push(updated);
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("Begin work.").await?;
    test.submit_turn("Continue with the same tools.").await?;
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some("updated-tools-model".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn("Continue with the updated execution instructions.")
        .await?;
    test.submit_text_turn("Continue with the same updated tools.")
        .await?;

    let requests = mock.requests();
    insta::assert_snapshot!(
        if use_responses_lite {
            "incremental_tools"
        } else {
            "incremental_tools_responses_api"
        },
        context_snapshot::format_request_history_snapshot(
            if use_responses_lite {
                "Tool definitions enter history in one batch; a catalog change appends one developer notice before only the changed exec definition, without modifying namespace descriptions. The next unchanged turn appends only user input."
            } else {
                "Regular Responses requests carry the full current tool catalog without an incremental notice; changed execution instructions replace the catalog on subsequent requests."
            },
            &requests,
            &ContextSnapshotOptions::default()
                .rewrite_known_segments()
                .include_request_settings(),
        )
    );
    Ok(())
}

#[test_case(false; "bundled")]
#[test_case(true; "catalog_messages")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_removal_appends_a_standalone_notice_without_rewriting_history(
    catalog_messages: bool,
) -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let command = remote_aware_stdio_server_bin()?;
    let environment_id = remote_aware_environment_id();
    let server_config = |enabled_tools: &[&str], instructions: &str| {
        serde_json::from_value(json!({
            "command": command,
            "environment_id": environment_id,
            "env": {"MCP_TEST_SERVER_INSTRUCTIONS": instructions},
            "enabled_tools": enabled_tools,
            "default_tools_approval_mode": "approve",
        }))
    };
    let initial_server = server_config(&["echo", "cwd"], "Echo service instructions.")?;
    let reduced_server = server_config(&["echo"], "Echo service instructions.")?;
    let restored_server = server_config(&["echo", "cwd"], "Echo service instructions.")?;
    let test = test_codex()
        .with_model_info_override("gpt-5.5", move |model| {
            model.use_responses_lite = true;
            model.supports_search_tool = false;
            model.tool_mode = Some(ToolMode::Direct);
            if catalog_messages {
                model
                    .model_messages
                    .get_or_insert_default()
                    .tools
                    .get_or_insert_default()
                    .incremental_tools = Some(IncrementalToolMessages {
                    tool_update_hint: Some(
                        "Catalog update: retain earlier tools unless explicitly removed."
                            .to_string(),
                    ),
                    removed_tools_header: Some("Unavailable tools:".to_string()),
                    removed_namespaces_header: Some("Unavailable namespaces:".to_string()),
                    namespace_instructions_prefix: Some("Catalog {name}: ".to_string()),
                    namespace_instructions_cleared: Some("Cleared {name}.".to_string()),
                });
            }
        })
        .with_config(move |config| {
            config
                .features
                .enable(Feature::IncrementalTools)
                .expect("enable incremental tools");
            if catalog_messages {
                config
                    .features
                    .enable(Feature::NonPrefixedMcpToolNames)
                    .expect("enable unprefixed MCP namespaces");
                config.non_prefixed_mcp_tool_servers = Some(vec!["functions".to_string()]);
            }
            config
                .mcp_servers
                .set(std::collections::HashMap::from([(
                    "echo_service".to_string(),
                    initial_server,
                )]))
                .expect("set MCP fixture");
        })
        .build_with_auto_env(&server)
        .await?;
    wait_for_mcp_server(&test.codex, "echo_service").await?;
    let response_count = if catalog_messages { 11 } else { 7 };
    let mock = responses::mount_sse_sequence(
        &server,
        (1..=response_count)
            .map(|index| responses::sse(vec![responses::ev_completed(&format!("resp-{index}"))]))
            .collect(),
    )
    .await;
    test.submit_text_turn("Begin with the echo service.")
        .await?;

    // Remove one member while keeping the namespace instructions and echo schema unchanged.
    let current_config = test.codex.config().await;
    let mut reduced_config = current_config.as_ref().clone();
    reduced_config
        .mcp_servers
        .set(std::collections::HashMap::from([(
            "echo_service".to_string(),
            reduced_server,
        )]))?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, reduced_config)
        .await;
    // Reconcile the refreshed runtime through a public call before the next model request.
    test.codex
        .call_mcp_tool(
            "echo_service",
            "echo",
            Some(json!({"message": "ready after removal"})),
            /*meta*/ None,
        )
        .await?;
    test.submit_text_turn("Continue after disabling the cwd tool.")
        .await?;
    test.submit_text_turn("Continue with the remaining echo tool.")
        .await?;

    // Remove the whole server, then reconcile even though its tools can no longer be called.
    let current_config = test.codex.config().await;
    let mut removed_config = current_config.as_ref().clone();
    removed_config.mcp_servers.set(Default::default())?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, removed_config)
        .await;
    let _ = test
        .codex
        .call_mcp_tool(
            "echo_service",
            "echo",
            Some(json!({"message": "unavailable"})),
            /*meta*/ None,
        )
        .await;
    test.submit_text_turn("Continue after removing the echo service.")
        .await?;
    test.submit_text_turn("Continue without the echo service.")
        .await?;

    // Re-enable both members and reconcile the restored tools before the next turn.
    let current_config = test.codex.config().await;
    let mut restored_config = current_config.as_ref().clone();
    restored_config
        .mcp_servers
        .set(std::collections::HashMap::from([(
            "echo_service".to_string(),
            restored_server,
        )]))?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, restored_config)
        .await;
    for (tool, arguments) in [
        ("echo", json!({"message": "ready after restoration"})),
        ("cwd", json!({})),
    ] {
        test.codex
            .call_mcp_tool("echo_service", tool, Some(arguments), /*meta*/ None)
            .await?;
    }
    test.submit_text_turn("Continue with the restored echo service.")
        .await?;
    test.submit_text_turn("Continue with the same restored tools.")
        .await?;

    if catalog_messages {
        // The functions namespace preserves empty instructions instead of adding fallback text.
        // Move the service there, then change and clear instructions with unchanged tool schemas.
        for (instructions, prompt) in [
            (
                "Use these echo tools.",
                "Continue with the echo tools in the functions namespace.",
            ),
            (
                "Keep {name} and {instructions} literal.",
                "Continue with updated echo instructions.",
            ),
            ("", "Continue after clearing echo instructions."),
        ] {
            let current_config = test.codex.config().await;
            let mut updated_config = current_config.as_ref().clone();
            updated_config
                .mcp_servers
                .set(std::collections::HashMap::from([(
                    "functions".to_string(),
                    server_config(&["echo", "cwd"], instructions)?,
                )]))?;
            let _ = test
                .codex
                .refresh_mcp_config(current_config, updated_config)
                .await;
            test.codex
                .call_mcp_tool(
                    "functions",
                    "echo",
                    Some(json!({"message": "ready after instructions refresh"})),
                    /*meta*/ None,
                )
                .await?;
            test.submit_text_turn(prompt).await?;
        }
        test.submit_text_turn("Continue with the same cleared instructions.")
            .await?;
    }

    let requests = mock.requests();
    let (snapshot_name, scenario) = if catalog_messages {
        (
            "incremental_tools_catalog_messages",
            "Catalog overrides supply the update hint, both removal headers, and namespace instructions notices. MCP refreshes remove and restore tools, then change and clear only the namespace instructions. Runtime instructions remain verbatim, and unchanged turns do not repeat notices.",
        )
    } else {
        (
            "incremental_tool_removals",
            "MCP refreshes remove one tool, then its whole namespace, before restoring both tools. Removal notices append alone; restoration appends one developer notice before both namespace declarations. Unchanged turns do not repeat updates.",
        )
    };
    insta::assert_snapshot!(
        snapshot_name,
        context_snapshot::format_request_history_snapshot(
            scenario,
            &requests,
            &ContextSnapshotOptions::default()
                .rewrite_known_segments()
                .include_request_settings(),
        )
    );
    Ok(())
}
