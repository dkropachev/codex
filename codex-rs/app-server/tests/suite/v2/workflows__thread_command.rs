use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_shell_command_sse_response;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ServerRequestResolvedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadWorkflowCommandParams;
use codex_app_server_protocol::ThreadWorkflowCommandResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_protocol::models::MessagePhase;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const WORKFLOW_MARKDOWN: &str = "# Workflow E2E\n\nmarker=workflow-e2e\n";
const WORKFLOW_COMPLETE_FRAME: &str = r##"{"v":1,"id":0,"method":"complete","params":{"markdown":"# Workflow E2E\n\nmarker=workflow-e2e\n"}}"##;
const WORKFLOW_CONTRACT_FRAME: &str = r#"{"v":1,"id":1,"method":"contract","params":{"inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true}}}"#;
const WORKFLOW_USER_INPUT_MARKDOWN_PREFIX: &str = "# Workflow User Input E2E\n\nresponse=";
const WORKFLOW_USER_INPUT_FRAME: &str = r#"{"v":1,"id":2,"method":"requestUserInput","params":{"questions":[{"id":"deploy_target","header":"Target","question":"Where should the workflow deploy?","isOther":false,"isSecret":false,"options":[{"label":"Staging","description":"Deploy to the staging environment."},{"label":"Production","description":"Deploy to the production environment."}]},{"id":"release_note","header":"Note","question":"What should the release note say?","isOther":false,"isSecret":false,"options":null}]}}"#;
const WORKFLOW_USER_INPUT_FRAME_2: &str = r#"{"v":1,"id":3,"method":"requestUserInput","params":{"questions":[{"id":"confirm","header":"Confirm","question":"Continue with this deployment?","isOther":false,"isSecret":false,"options":[{"label":"Yes","description":"Continue."},{"label":"No","description":"Stop."}]}]}}"#;

#[tokio::test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
async fn thread_workflow_command_runs_fresh_scaffold_with_real_bun() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "`thread/workflowCommand` runs on the app-server local environment"
    );

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workflow_dir = scaffold_test_workflow(tmp.path())?;
    let source_path = workflow_dir.join("src/workflow.ts");
    let source = std::fs::read_to_string(&source_path)?;
    std::fs::write(
        &source_path,
        source.replace(
            r#"message: input.message ?? "Workflow completed.""#,
            r#"message: input.workingDirectory ?? "missing workingDirectory""#,
        ),
    )?;
    let server = create_mock_responses_server_sequence(Vec::new()).await;
    write_mock_responses_config_toml(
        codex_home.as_path(),
        &server.uri(),
        &BTreeMap::default(),
        i64::MAX,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "Summarize the conversation.",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let expected_working_directory = mcp.auto_env_params()?.cwd.as_str().to_string();
    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } =
        to_response::<ThreadStartResponse>(read_response(&mut mcp, start_id).await?)?;

    let workflow_id = mcp
        .send_thread_workflow_command_request(ThreadWorkflowCommandParams {
            thread_id: thread.id,
            workflow_dir: workflow_dir.to_string_lossy().to_string(),
            input: json!({}),
        })
        .await?;
    let _: ThreadWorkflowCommandResponse =
        to_response(read_response(&mut mcp, workflow_id).await?)?;
    let _: TurnStartedNotification = read_notification(&mut mcp, "turn/started").await?;
    let markdown = format!("# Workflow Test\n\n{expected_working_directory}\n");
    let completed = wait_for_agent_message_completed(&mut mcp, &markdown).await?;
    assert_agent_message(&completed.item, &markdown);
    let completed: TurnCompletedNotification =
        read_notification(&mut mcp, "turn/completed").await?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    Ok(())
}

#[tokio::test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
async fn thread_workflow_command_reports_canonical_and_legacy_contract_failures() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "`thread/workflowCommand` runs on the app-server local environment"
    );

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workflow_dir = scaffold_test_workflow(tmp.path())?;
    let source_path = workflow_dir.join("src/workflow.ts");
    let canonical_source = std::fs::read_to_string(&source_path)?;
    let server = create_mock_responses_server_sequence(Vec::new()).await;
    write_mock_responses_config_toml(
        codex_home.as_path(),
        &server.uri(),
        &BTreeMap::default(),
        i64::MAX,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "Summarize the conversation.",
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } =
        to_response::<ThreadStartResponse>(read_response(&mut mcp, start_id).await?)?;

    std::fs::write(
        &source_path,
        canonical_source.replacen(
            r#"message: { type: "string" },"#,
            r#"message: { type: "number" },"#,
            /*count*/ 1,
        ),
    )?;
    run_workflow_expect_failure(
        &mut mcp,
        &thread.id,
        &workflow_dir,
        json!({ "message": "not a number" }),
        "Workflow output failed schema validation",
    )
    .await?;

    std::fs::write(
        &source_path,
        canonical_source.replace(
            "export default defineWorkflow({",
            "const malformedWorkflow = defineWorkflow({",
        ),
    )?;
    run_workflow_expect_failure(
        &mut mcp,
        &thread.id,
        &workflow_dir,
        json!({}),
        "`default defineWorkflow` export",
    )
    .await?;

    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: workflow\ncommand: workflow-test\ntitle: Workflow Test\nuserDescription: Legacy workflow\n",
    )?;
    run_workflow_expect_failure(
        &mut mcp,
        &thread.id,
        &workflow_dir,
        json!({}),
        "legacy workflow metadata field",
    )
    .await?;

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: thread.id,
            include_turns: true,
        })
        .await?;
    let ThreadReadResponse { thread, .. } =
        to_response::<ThreadReadResponse>(read_response(&mut mcp, read_id).await?)?;
    assert!(
        thread
            .turns
            .iter()
            .flat_map(|turn| &turn.items)
            .all(|item| !matches!(item, ThreadItem::AgentMessage { .. })),
        "contract failures must not persist formatted workflow output"
    );
    Ok(())
}

#[tokio::test]
async fn thread_workflow_command_records_assistant_output_and_next_turn_context() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "`thread/workflowCommand` runs on the app-server local environment"
    );

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workflow_dir = scaffold_test_workflow(tmp.path())?;

    let fake_bin = tmp.path().join("fake_bin");
    std::fs::create_dir(&fake_bin)?;
    write_fake_bun(fake_bin.as_path())?;
    let path_value = path_with_prepended_dir(fake_bin.as_path())?;

    let responses = vec![create_final_assistant_message_sse_response(
        "follow-up answer",
    )?];
    let server = create_mock_responses_server_sequence(responses).await;
    write_mock_responses_config_toml(
        codex_home.as_path(),
        &server.uri(),
        &BTreeMap::default(),
        i64::MAX,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "Summarize the conversation.",
    )?;

    let env = [("PATH", Some(path_value.as_str()))];
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .with_env_overrides(&env)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let workflow_id = mcp
        .send_thread_workflow_command_request(ThreadWorkflowCommandParams {
            thread_id: thread.id.clone(),
            workflow_dir: workflow_dir.to_string_lossy().to_string(),
            input: json!({
                "marker": "workflow-e2e",
                "workingDirectory": workspace.to_string_lossy().to_string(),
            }),
        })
        .await?;
    let workflow_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(workflow_id)),
    )
    .await??;
    let _: ThreadWorkflowCommandResponse =
        to_response::<ThreadWorkflowCommandResponse>(workflow_resp)?;

    let workflow_turn_started: TurnStartedNotification = serde_json::from_value(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("turn/started"),
        )
        .await??
        .params
        .context("missing workflow turn/started params")?,
    )?;
    assert_eq!(workflow_turn_started.thread_id, thread.id);
    assert_eq!(workflow_turn_started.turn.status, TurnStatus::InProgress);
    let workflow_turn_id = workflow_turn_started.turn.id;

    let started = wait_for_agent_message_started(&mut mcp, WORKFLOW_MARKDOWN).await?;
    assert_agent_message(&started.item, WORKFLOW_MARKDOWN);
    let completed = wait_for_agent_message_completed(&mut mcp, WORKFLOW_MARKDOWN).await?;
    assert_agent_message(&completed.item, WORKFLOW_MARKDOWN);

    let workflow_turn_completed: TurnCompletedNotification =
        read_notification(&mut mcp, "turn/completed").await?;
    assert_eq!(workflow_turn_completed.thread_id, thread.id);
    assert_eq!(workflow_turn_completed.turn.id, workflow_turn_id);

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: thread.id.clone(),
            include_turns: true,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let ThreadReadResponse { thread, .. } = to_response::<ThreadReadResponse>(read_resp)?;
    assert_eq!(thread.turns.len(), 1);
    assert!(
        thread.turns[0]
            .items
            .iter()
            .any(|item| agent_message_text(item) == Some(WORKFLOW_MARKDOWN)),
        "thread/read should persist assistant workflow output"
    );

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "follow up after workflow".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace),
            ..Default::default()
        })
        .await?;
    let _: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch mock model requests")?;
    assert_eq!(requests.len(), 1);
    let request_body = requests[0]
        .body_json::<serde_json::Value>()
        .context("model request body should be JSON")?
        .to_string();
    assert!(request_body.contains("follow up after workflow"));
    assert!(request_body.contains("# Workflow E2E"));
    assert!(request_body.contains("marker=workflow-e2e"));

    Ok(())
}

#[tokio::test]
async fn thread_workflow_command_round_trips_choice_and_freeform_user_input() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "`thread/workflowCommand` runs on the app-server local environment"
    );
    let mut fixture = start_user_input_workflow().await?;

    let request = timeout(
        DEFAULT_READ_TIMEOUT,
        fixture.mcp.read_stream_until_request_message(),
    )
    .await??;
    let original_request = request.clone();
    let ServerRequest::ToolRequestUserInput {
        request_id: _,
        params,
    } = request
    else {
        panic!("expected workflow request_user_input request, got {request:?}");
    };
    assert_eq!(
        serde_json::to_value(params)?,
        expected_workflow_user_input_params(
            &fixture.thread_id,
            &fixture.turn_id,
            /*item_id*/ 2,
            WORKFLOW_USER_INPUT_FRAME,
        )?
    );

    let ThreadResumeResponse { thread, .. } =
        resume_thread(&mut fixture.mcp, &fixture.thread_id).await?;
    assert_eq!(thread.id, fixture.thread_id);
    assert!(
        thread
            .turns
            .iter()
            .any(|turn| turn.id == fixture.turn_id && turn.status == TurnStatus::InProgress)
    );

    let replayed_request = timeout(
        DEFAULT_READ_TIMEOUT,
        fixture.mcp.read_stream_until_request_message(),
    )
    .await??;
    assert_eq!(replayed_request, original_request);
    let ServerRequest::ToolRequestUserInput { request_id, .. } = replayed_request else {
        panic!("expected replayed workflow request_user_input request");
    };

    let response_value = json!({
        "answers": {
            "deploy_target": { "answers": ["Staging"] },
            "release_note": { "answers": ["user_note: Ship after smoke tests."] },
        }
    });
    let resolved_request_id = request_id.clone();
    fixture
        .mcp
        .send_response(request_id, response_value.clone())
        .await?;
    let resolved: ServerRequestResolvedNotification =
        read_notification(&mut fixture.mcp, "serverRequest/resolved").await?;
    assert_eq!(resolved.thread_id, fixture.thread_id);
    assert_eq!(resolved.request_id, resolved_request_id);

    let request = timeout(
        DEFAULT_READ_TIMEOUT,
        fixture.mcp.read_stream_until_request_message(),
    )
    .await??;
    let second_original_request = request.clone();
    let ServerRequest::ToolRequestUserInput {
        request_id: _,
        params,
    } = request
    else {
        panic!("expected second workflow request_user_input request, got {request:?}");
    };
    assert_eq!(
        serde_json::to_value(params)?,
        expected_workflow_user_input_params(
            &fixture.thread_id,
            &fixture.turn_id,
            /*item_id*/ 3,
            WORKFLOW_USER_INPUT_FRAME_2,
        )?
    );

    let ThreadResumeResponse { thread, .. } =
        resume_thread(&mut fixture.mcp, &fixture.thread_id).await?;
    assert!(
        thread
            .turns
            .iter()
            .any(|turn| turn.id == fixture.turn_id && turn.status == TurnStatus::InProgress)
    );
    let replayed_request = timeout(
        DEFAULT_READ_TIMEOUT,
        fixture.mcp.read_stream_until_request_message(),
    )
    .await??;
    assert_eq!(replayed_request, second_original_request);
    let ServerRequest::ToolRequestUserInput { request_id, .. } = replayed_request else {
        panic!("expected replayed second workflow request_user_input request");
    };

    let second_response = json!({ "answers": { "confirm": { "answers": ["Yes"] } } });
    let resolved_request_id = request_id.clone();
    fixture
        .mcp
        .send_response(request_id, second_response.clone())
        .await?;
    let resolved: ServerRequestResolvedNotification =
        read_notification(&mut fixture.mcp, "serverRequest/resolved").await?;
    assert_eq!(resolved.request_id, resolved_request_id);

    let started: ItemStartedNotification =
        read_notification(&mut fixture.mcp, "item/started").await?;
    let workflow_markdown = agent_message_text(&started.item)
        .context("workflow output should be an agent message")?
        .to_string();
    let response_frame = workflow_markdown
        .strip_prefix(WORKFLOW_USER_INPUT_MARKDOWN_PREFIX)
        .and_then(|text| text.strip_suffix('\n'))
        .context("workflow output should contain the exact response frame")?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(response_frame)?,
        json!([
            { "v": 1, "id": 2, "result": response_value },
            { "v": 1, "id": 3, "result": second_response },
        ])
    );

    let completed = wait_for_agent_message_completed(&mut fixture.mcp, &workflow_markdown).await?;
    assert_agent_message(&completed.item, &workflow_markdown);
    let workflow_turn_completed: TurnCompletedNotification =
        read_notification(&mut fixture.mcp, "turn/completed").await?;
    assert_eq!(workflow_turn_completed.thread_id, fixture.thread_id);
    assert_eq!(workflow_turn_completed.turn.id, fixture.turn_id);
    assert_eq!(workflow_turn_completed.turn.status, TurnStatus::Completed);
    assert_child_process_exited(&fixture.child_pid_path).await?;

    let ThreadResumeResponse { thread, .. } =
        resume_thread(&mut fixture.mcp, &fixture.thread_id).await?;
    assert_eq!(thread.id, fixture.thread_id);
    assert!(
        timeout(
            Duration::from_millis(/*millis*/ 100),
            fixture.mcp.read_stream_until_request_message(),
        )
        .await
        .is_err(),
        "resolved workflow input requests must not replay"
    );

    Ok(())
}

#[tokio::test]
async fn thread_workflow_command_interrupt_clears_pending_user_input() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "`thread/workflowCommand` runs on the app-server local environment"
    );
    let mut fixture = start_user_input_workflow().await?;

    let request = timeout(
        DEFAULT_READ_TIMEOUT,
        fixture.mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ToolRequestUserInput { request_id, params } = request else {
        panic!("expected workflow request_user_input request, got {request:?}");
    };
    assert_eq!(
        serde_json::to_value(params)?,
        expected_workflow_user_input_params(
            &fixture.thread_id,
            &fixture.turn_id,
            /*item_id*/ 2,
            WORKFLOW_USER_INPUT_FRAME,
        )?
    );

    let interrupt_id = fixture
        .mcp
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: fixture.thread_id.clone(),
            turn_id: fixture.turn_id.clone(),
        })
        .await?;
    let interrupt_resp = read_response(&mut fixture.mcp, interrupt_id).await?;
    let _: TurnInterruptResponse = to_response(interrupt_resp)?;
    let resolved: ServerRequestResolvedNotification =
        read_notification(&mut fixture.mcp, "serverRequest/resolved").await?;
    assert_eq!(resolved.thread_id, fixture.thread_id);
    assert_eq!(resolved.request_id, request_id);

    let workflow_turn_completed: TurnCompletedNotification =
        read_notification(&mut fixture.mcp, "turn/completed").await?;
    assert_eq!(workflow_turn_completed.thread_id, fixture.thread_id);
    assert_eq!(workflow_turn_completed.turn.id, fixture.turn_id);
    assert_eq!(workflow_turn_completed.turn.status, TurnStatus::Interrupted);
    assert_child_process_exited(&fixture.child_pid_path).await?;

    let read_id = fixture
        .mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: fixture.thread_id.clone(),
            include_turns: true,
        })
        .await?;
    let read_resp = read_response(&mut fixture.mcp, read_id).await?;
    let ThreadReadResponse { thread, .. } = to_response::<ThreadReadResponse>(read_resp)?;
    assert!(
        thread
            .turns
            .iter()
            .flat_map(|turn| &turn.items)
            .all(|item| !matches!(item, ThreadItem::AgentMessage { .. })),
        "interrupted workflow should not persist partial assistant output"
    );

    Ok(())
}

#[tokio::test]
async fn thread_workflow_command_rejects_active_turn() -> Result<()> {
    skip_if_remote!(
        Ok(()),
        "`thread/workflowCommand` runs on the app-server local environment"
    );

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workflow_dir = scaffold_test_workflow(tmp.path())?;

    let responses = vec![
        create_shell_command_sse_response(
            vec![
                "python3".to_string(),
                "-c".to_string(),
                "print(42)".to_string(),
            ],
            /*workdir*/ None,
            Some(5000),
            "call-approve",
        )?,
        create_final_assistant_message_sse_response("done after decline")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    write_mock_responses_config_toml(
        codex_home.as_path(),
        &server.uri(),
        &BTreeMap::default(),
        i64::MAX,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "Summarize the conversation.",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "run python".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace),
            approval_policy: Some(AskForApproval::UnlessTrusted),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, .. } = server_req else {
        panic!("expected approval request");
    };

    let workflow_id = mcp
        .send_thread_workflow_command_request(ThreadWorkflowCommandParams {
            thread_id: thread.id.clone(),
            workflow_dir: workflow_dir.to_string_lossy().to_string(),
            input: json!({}),
        })
        .await?;
    let workflow_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(workflow_id)),
    )
    .await??;
    assert_eq!(
        workflow_error.error.message,
        "Cannot run workflow command while a turn is in progress."
    );

    mcp.send_response(
        request_id,
        serde_json::to_value(CommandExecutionRequestApprovalResponse {
            decision: CommandExecutionApprovalDecision::Decline,
        })?,
    )
    .await?;
    let completed: TurnCompletedNotification =
        read_notification(&mut mcp, "turn/completed").await?;
    assert_eq!(completed.turn.id, turn.id);

    Ok(())
}

struct RunningUserInputWorkflow {
    _temp_dir: TempDir,
    _server: wiremock::MockServer,
    mcp: TestAppServer,
    thread_id: String,
    turn_id: String,
    child_pid_path: PathBuf,
}

async fn start_user_input_workflow() -> Result<RunningUserInputWorkflow> {
    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workflow_dir = scaffold_test_workflow(tmp.path())?;
    let child_pid_path = workflow_dir.join("child.pid");
    let fake_bin = tmp.path().join("fake_bin");
    std::fs::create_dir(&fake_bin)?;
    write_fake_bun(fake_bin.as_path())?;
    let path_value = path_with_prepended_dir(fake_bin.as_path())?;

    let server = create_mock_responses_server_sequence(Vec::new()).await;
    write_mock_responses_config_toml(
        codex_home.as_path(),
        &server.uri(),
        &BTreeMap::default(),
        i64::MAX,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "Summarize the conversation.",
    )?;
    let env = [("PATH", Some(path_value.as_str()))];
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .with_env_overrides(&env)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let start_resp = read_response(&mut mcp, start_id).await?;
    let ThreadStartResponse { thread, .. } = to_response(start_resp)?;
    let workflow_id = mcp
        .send_thread_workflow_command_request(ThreadWorkflowCommandParams {
            thread_id: thread.id.clone(),
            workflow_dir: workflow_dir.to_string_lossy().to_string(),
            input: json!({ "marker": "workflow-user-input" }),
        })
        .await?;
    let workflow_resp = read_response(&mut mcp, workflow_id).await?;
    let _: ThreadWorkflowCommandResponse = to_response(workflow_resp)?;
    let started: TurnStartedNotification = read_notification(&mut mcp, "turn/started").await?;
    assert_eq!(started.thread_id, thread.id);
    assert_eq!(started.turn.status, TurnStatus::InProgress);

    Ok(RunningUserInputWorkflow {
        _temp_dir: tmp,
        _server: server,
        mcp,
        thread_id: thread.id,
        turn_id: started.turn.id,
        child_pid_path,
    })
}

fn expected_workflow_user_input_params(
    thread_id: &str,
    turn_id: &str,
    item_id: u64,
    frame: &str,
) -> Result<serde_json::Value> {
    let frame: serde_json::Value = serde_json::from_str(frame)?;
    Ok(json!({
        "threadId": thread_id,
        "turnId": turn_id,
        "itemId": format!("workflow-user-input-{turn_id}-{item_id}"),
        "questions": frame["params"]["questions"],
        "autoResolutionMs": null,
    }))
}

async fn run_workflow_expect_failure(
    mcp: &mut TestAppServer,
    thread_id: &str,
    workflow_dir: &Path,
    input: serde_json::Value,
    expected_error: &str,
) -> Result<()> {
    let request_id = mcp
        .send_thread_workflow_command_request(ThreadWorkflowCommandParams {
            thread_id: thread_id.to_string(),
            workflow_dir: workflow_dir.to_string_lossy().to_string(),
            input,
        })
        .await?;
    let _: ThreadWorkflowCommandResponse = to_response(read_response(mcp, request_id).await?)?;
    let started: TurnStartedNotification = read_notification(mcp, "turn/started").await?;
    let error: ErrorNotification = read_notification(mcp, "error").await?;
    assert_eq!(error.thread_id, thread_id);
    assert_eq!(error.turn_id, started.turn.id);
    assert!(!error.will_retry);
    assert!(
        error.error.message.contains(expected_error),
        "expected {expected_error:?}, got {:?}",
        error.error.message
    );
    let completed: TurnCompletedNotification = read_notification(mcp, "turn/completed").await?;
    assert_eq!(completed.turn.id, started.turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    Ok(())
}

async fn read_response(mcp: &mut TestAppServer, id: i64) -> Result<JSONRPCResponse> {
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(id)),
    )
    .await?
}

async fn resume_thread(mcp: &mut TestAppServer, thread_id: &str) -> Result<ThreadResumeResponse> {
    let resume_id = mcp
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.to_string(),
            ..Default::default()
        })
        .await?;
    to_response(read_response(mcp, resume_id).await?)
}

async fn read_notification<T: DeserializeOwned>(
    mcp: &mut TestAppServer,
    method: &str,
) -> Result<T> {
    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message(method),
    )
    .await??;
    Ok(serde_json::from_value(
        notification.params.context("missing notification params")?,
    )?)
}

async fn assert_child_process_exited(pid_path: &Path) -> Result<()> {
    let pid = std::fs::read_to_string(pid_path)?;
    for _ in 0..50 {
        if !std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?
            .success()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
    anyhow::bail!("workflow child process {pid} survived workflow termination")
}

async fn wait_for_agent_message_started(
    mcp: &mut TestAppServer,
    expected_text: &str,
) -> Result<ItemStartedNotification> {
    loop {
        let notif = mcp
            .read_stream_until_notification_message("item/started")
            .await?;
        let started: ItemStartedNotification = serde_json::from_value(
            notif
                .params
                .context("missing item/started notification params")?,
        )?;
        if agent_message_text(&started.item) == Some(expected_text) {
            return Ok(started);
        }
    }
}

async fn wait_for_agent_message_completed(
    mcp: &mut TestAppServer,
    expected_text: &str,
) -> Result<ItemCompletedNotification> {
    loop {
        let notif = mcp
            .read_stream_until_notification_message("item/completed")
            .await?;
        let completed: ItemCompletedNotification = serde_json::from_value(
            notif
                .params
                .context("missing item/completed notification params")?,
        )?;
        if agent_message_text(&completed.item) == Some(expected_text) {
            return Ok(completed);
        }
    }
}

fn assert_agent_message(item: &ThreadItem, expected_text: &str) {
    let ThreadItem::AgentMessage { text, phase, .. } = item else {
        panic!("expected agent message item, got {item:?}");
    };
    assert_eq!(text, expected_text);
    assert_eq!(*phase, Some(MessagePhase::FinalAnswer));
}

fn agent_message_text(item: &ThreadItem) -> Option<&str> {
    match item {
        ThreadItem::AgentMessage { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn write_fake_bun(fake_bin: &Path) -> Result<()> {
    let bun_path = fake_bin.join("bun");
    std::fs::write(
        &bun_path,
        format!(
            r##"#!/bin/sh
set -eu
if [ ! -f "${{1:-}}" ]; then
  echo "missing materialized runner" >&2
  exit 64
fi
if ! grep -q '"markdown.v1"' "$1"; then
  echo "runner did not request markdown.v1" >&2
  exit 65
fi
case "${{2:-}}" in
inspect)
  printf '%s\n' '{{"apiVersion":1,"id":"workflow","title":"Workflow Test","callableName":"workflow-test","inputSchema":{{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{{"workingDirectory":{{"type":"string"}}}},"additionalProperties":true}},"outputSchema":{{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true}},"hasComplete":true}}'
  exit 0
  ;;
scan)
  printf '%s\n' '[{{"path":"src/workflow.ts","exports":["inputSchema","outputSchema"],"imports":[]}}]'
  exit 0
  ;;
run) ;;
*)
  echo "missing run operation" >&2
  exit 66
  ;;
esac
test -s "${{4:?}}"
workflow_input=$(cat "${{3:?}}")
printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{WORKFLOW_CONTRACT_FRAME}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
IFS= read -r _contract_ack
case "$workflow_input" in
  *'"marker":"workflow-e2e"'*)
    printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{{"v":1,"id":2,"method":"validateOutput","params":{{"output":{{}}}}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
    IFS= read -r _output_ack
    printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{WORKFLOW_COMPLETE_FRAME}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
    IFS= read -r _completion_ack
    ;;
  *'"marker":"workflow-user-input"'*)
    (while :; do printf 'worker noise\n' >&2; sleep 0.01; done) &
    printf '%s\n' "$!" > child.pid
    printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{WORKFLOW_USER_INPUT_FRAME}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
    IFS= read -r first_response
    printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{WORKFLOW_USER_INPUT_FRAME_2}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
    IFS= read -r second_response
    printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{{"v":1,"id":4,"method":"validateOutput","params":{{"output":{{}}}}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
    IFS= read -r _output_ack
    responses=$(printf '[%s,%s]' "$first_response" "$second_response" | sed 's/\\/\\\\/g; s/"/\\"/g')
    printf '\036CODEX_WORKFLOW_CONTROL {{"v":1,"id":0,"method":"complete","params":{{"markdown":"# Workflow User Input E2E\\n\\nresponse=%s\\n"}}}}\n' "$responses" >> "$CODEX_WORKFLOW_CONTROL_PATH"
    IFS= read -r _completion_ack
    ;;
  *)
    echo "workflow input missing marker" >&2
    exit 67
    ;;
esac
"##
        ),
    )?;
    let mut permissions = std::fs::metadata(&bun_path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bun_path, permissions)?;
    Ok(())
}

fn scaffold_test_workflow(root: &Path) -> Result<PathBuf> {
    scaffold_workflow(
        root,
        &ScaffoldRequest {
            id: "workflow".to_string(),
            title: "Workflow Test".to_string(),
            callable_name: "workflow-test".to_string(),
            description: "Exercise hosted workflow execution.".to_string(),
        },
    )
}

fn path_with_prepended_dir(dir: &Path) -> Result<String> {
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&existing_path));
    Ok(std::env::join_paths(paths)?.to_string_lossy().to_string())
}
