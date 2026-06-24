#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used)]

use anyhow::Result;
use anyhow::anyhow;
use codex_features::Feature;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::namespace_child_tool;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use wiremock::MockServer;

const MULTI_AGENT_V1_NAMESPACE: &str = "multi_agent_v1";
const SPAWN_AGENT_TOOL_NAME: &str = "spawn_agent";
const DISCOVERY_PROMPT: &str = "inspect the built-in workflow roles";
const WORKFLOW_EXECUTION_PROMPT: &str = "run the workflow role sequence";
const REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(/*millis*/ 20);
const TURN_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 15);

#[derive(Clone, Copy)]
struct WorkflowRole {
    name: &'static str,
    description_snippet: &'static str,
    developer_instruction_snippet: &'static str,
}

#[derive(Clone, Copy)]
struct WorkflowStep {
    call_id: &'static str,
    role: WorkflowRole,
    message: &'static str,
}

const WORKFLOW_ROLES: &[WorkflowRole] = &[
    WorkflowRole {
        name: "workflow-arch-reviewer",
        description_snippet: "Fresh-context reviewer for workflow DESIGN.md.",
        developer_instruction_snippet: "You are a fresh-context workflow architecture reviewer.",
    },
    WorkflowRole {
        name: "workflow-architect",
        description_snippet: "Owns workflow design.",
        developer_instruction_snippet: "You are a workflow architect.",
    },
    WorkflowRole {
        name: "workflow-code-reviewer",
        description_snippet: "Fresh-context reviewer for workflow implementation",
        developer_instruction_snippet: "You are a fresh-context workflow code reviewer.",
    },
    WorkflowRole {
        name: "workflow-coder",
        description_snippet: "Implements workflow code and tests",
        developer_instruction_snippet: "You are a workflow coder.",
    },
    WorkflowRole {
        name: "workflow-resilience-reviewer",
        description_snippet: "Fresh-context reviewer for workflow runtime resilience",
        developer_instruction_snippet: "You are a fresh-context workflow resilience reviewer.",
    },
];

const WORKFLOW_EXECUTION_STEPS: &[WorkflowStep] = &[
    WorkflowStep {
        call_id: "call-workflow-architect",
        role: WORKFLOW_ROLES[1],
        message: "plan the workflow design",
    },
    WorkflowStep {
        call_id: "call-workflow-arch-reviewer",
        role: WORKFLOW_ROLES[0],
        message: "review the workflow design",
    },
    WorkflowStep {
        call_id: "call-workflow-coder",
        role: WORKFLOW_ROLES[3],
        message: "implement the workflow code and tests",
    },
    WorkflowStep {
        call_id: "call-workflow-code-reviewer",
        role: WORKFLOW_ROLES[2],
        message: "review the workflow implementation",
    },
    WorkflowStep {
        call_id: "call-workflow-resilience-reviewer",
        role: WORKFLOW_ROLES[4],
        message: "review workflow resilience and recovery behavior",
    },
    WorkflowStep {
        call_id: "call-workflow-coder-repair",
        role: WORKFLOW_ROLES[3],
        message: "repair workflow code review findings",
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn built_in_workflow_roles_are_discoverable_from_spawn_agent_tool() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-discovery-1"),
            ev_assistant_message("msg-discovery-1", "done"),
            ev_completed("resp-discovery-1"),
        ]),
    )
    .await;

    let mut builder = workflow_agent_test_builder();
    let test = builder.build(&server).await?;
    test.submit_turn(DISCOVERY_PROMPT).await?;

    let agent_type_description =
        spawn_agent_agent_type_description(&mock.single_request().body_json())?;
    let missing_roles: Vec<&str> = WORKFLOW_ROLES
        .iter()
        .filter(|role| {
            role_block(&agent_type_description, role.name)
                .is_none_or(|block| !block.contains(role.description_snippet))
        })
        .map(|role| role.name)
        .collect();
    assert_eq!(missing_roles, Vec::<&str>::new());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_spawn_path_applies_planning_implementation_review_and_repair_roles() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    for step in WORKFLOW_EXECUTION_STEPS {
        let server = start_mock_server().await;
        let parent_prompt = format!("{WORKFLOW_EXECUTION_PROMPT}: {}", step.call_id);
        let (_parent_mock, child_mock, completion_mock) =
            mount_workflow_step_responses(&server, *step, parent_prompt.clone()).await?;
        let mut builder = workflow_agent_test_builder();
        let test = builder.build(&server).await?;
        test.submit_turn(&parent_prompt).await?;

        let spawn_output = completion_mock
            .function_call_output_text(step.call_id)
            .ok_or_else(|| anyhow!("missing spawn_agent output for {}", step.call_id))?;
        assert!(
            spawn_output.contains("\"agent_id\""),
            "expected successful spawn output for {}, got {spawn_output:?}",
            step.call_id
        );
        let child_request =
            wait_for_matching_request(&child_mock, step.role.name, is_child_request).await?;
        assert!(
            child_request.body_contains_text(step.message),
            "expected {} child request to contain {:?}",
            step.role.name,
            step.message
        );
        assert!(
            child_request.body_contains_text(step.role.developer_instruction_snippet),
            "expected {} child request to contain role prompt {:?}",
            step.role.name,
            step.role.developer_instruction_snippet
        );
    }

    Ok(())
}

fn workflow_agent_test_builder() -> TestCodexBuilder {
    test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should allow feature update");
        config
            .features
            .disable(Feature::EnableRequestCompression)
            .expect("test config should allow feature update");
        config.multi_agent_v2.hide_spawn_agent_metadata = false;
    })
}

fn spawn_agent_agent_type_description(body: &Value) -> Result<String> {
    namespace_child_tool(body, MULTI_AGENT_V1_NAMESPACE, SPAWN_AGENT_TOOL_NAME)
        .and_then(|tool| tool_parameter_description(tool, "agent_type"))
        .ok_or_else(|| anyhow!("spawn_agent agent_type description should be present"))
}

fn tool_parameter_description(tool: &Value, parameter_name: &str) -> Option<String> {
    tool.get("parameters")
        .and_then(|parameters| parameters.get("properties"))
        .and_then(|properties| properties.get(parameter_name))
        .and_then(|parameter| parameter.get("description"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn role_block(description: &str, role_name: &str) -> Option<String> {
    let role_header = format!("{role_name}: {{");
    let mut lines = description.lines().skip_while(|line| *line != role_header);
    let first_line = lines.next()?;
    let mut block = vec![first_line];
    for line in lines {
        if line.ends_with(": {") {
            break;
        }
        block.push(line);
    }
    Some(block.join("\n"))
}

fn is_child_request(request: &ResponsesRequest) -> bool {
    request.header("x-openai-subagent").as_deref() == Some("collab_spawn")
}

async fn wait_for_matching_request<F>(
    mock: &ResponseMock,
    label: &str,
    mut predicate: F,
) -> Result<ResponsesRequest>
where
    F: FnMut(&ResponsesRequest) -> bool,
{
    tokio::time::timeout(TURN_TIMEOUT, async {
        loop {
            if let Some(request) = mock
                .requests()
                .into_iter()
                .find(|request| predicate(request))
            {
                return request;
            }
            tokio::time::sleep(REQUEST_POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for {label} child request"))
}

async fn mount_workflow_step_responses(
    server: &MockServer,
    step: WorkflowStep,
    parent_prompt: String,
) -> Result<(ResponseMock, ResponseMock, ResponseMock)> {
    let args = serde_json::to_string(&json!({
        "message": step.message,
        "agent_type": step.role.name,
    }))?;
    let response_id = format!("resp-parent-{}", step.call_id);
    let parent_prompt_for_match = parent_prompt.clone();
    let parent_mock = mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            request_body_contains(request, &parent_prompt_for_match)
                && request_header(request, "x-openai-subagent").is_none()
        },
        sse(vec![
            ev_response_created(&response_id),
            ev_function_call_with_namespace(
                step.call_id,
                MULTI_AGENT_V1_NAMESPACE,
                SPAWN_AGENT_TOOL_NAME,
                &args,
            ),
            ev_completed(&response_id),
        ]),
    )
    .await;

    let response_id = format!("resp-child-{}", step.call_id);
    let message_id = format!("msg-child-{}", step.call_id);
    let child_mock = mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            request_header(request, "x-openai-subagent") == Some("collab_spawn")
        },
        sse(vec![
            ev_response_created(&response_id),
            ev_assistant_message(&message_id, "done"),
            ev_completed(&response_id),
        ]),
    )
    .await;

    let response_id = format!("resp-complete-{}", step.call_id);
    let message_id = format!("msg-complete-{}", step.call_id);
    let completion_mock = mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            request_body_contains(request, step.call_id)
                && request_header(request, "x-openai-subagent").is_none()
        },
        sse(vec![
            ev_response_created(&response_id),
            ev_assistant_message(&message_id, "done"),
            ev_completed(&response_id),
        ]),
    )
    .await;

    Ok((parent_mock, child_mock, completion_mock))
}

fn request_body_contains(request: &wiremock::Request, text: &str) -> bool {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    let bytes = if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
    } else {
        Some(request.body.clone())
    };
    bytes
        .and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

fn request_header<'a>(request: &'a wiremock::Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}
