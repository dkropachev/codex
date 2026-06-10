use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
#[cfg(unix)]
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
#[cfg(unix)]
use anyhow::bail;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
#[cfg(unix)]
use codex_app_server::in_process;
#[cfg(unix)]
use codex_app_server::in_process::InProcessServerEvent;
#[cfg(unix)]
use codex_app_server::in_process::InProcessStartArgs;
#[cfg(unix)]
use codex_app_server_protocol::ClientInfo;
#[cfg(unix)]
use codex_app_server_protocol::ClientRequest;
#[cfg(unix)]
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::RequestId;
#[cfg(unix)]
use codex_app_server_protocol::ServerNotification;
#[cfg(unix)]
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::WorkflowDevelopResponse;
use codex_app_server_protocol::WorkflowDiscardResponse;
use codex_app_server_protocol::WorkflowEditResponse;
use codex_app_server_protocol::WorkflowEngine;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowPublishResponse;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRepairActionKind;
use codex_app_server_protocol::WorkflowRepairResponse;
use codex_app_server_protocol::WorkflowRepairStopReason;
#[cfg(unix)]
use codex_app_server_protocol::WorkflowRunStartParams;
#[cfg(unix)]
use codex_app_server_protocol::WorkflowRunStartResponse;
#[cfg(unix)]
use codex_app_server_protocol::WorkflowRunStatus;
#[cfg(unix)]
use codex_app_server_protocol::WorkflowRunWaitParams;
#[cfg(unix)]
use codex_app_server_protocol::WorkflowRunWaitResponse;
use codex_app_server_protocol::WorkflowValidateResponse;
use codex_app_server_protocol::WorkflowValidationFindingInfo;
use codex_app_server_protocol::WorkflowValidationStatus;
#[cfg(unix)]
use codex_arg0::Arg0DispatchPaths;
#[cfg(unix)]
use codex_config::CloudRequirementsLoader;
#[cfg(unix)]
use codex_config::LoaderOverrides;
#[cfg(unix)]
use codex_core::config::ConfigBuilder;
#[cfg(unix)]
use codex_exec_server::EnvironmentManager;
#[cfg(unix)]
use codex_feedback::CodexFeedback;
use pretty_assertions::assert_eq;
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 30);

fn append_workflows_config(codex_home: &TempDir, extra: &str) -> Result<()> {
    fs::OpenOptions::new()
        .append(true)
        .open(codex_home.path().join("config.toml"))?
        .write_all(extra.as_bytes())?;
    Ok(())
}

fn write_test_bun_stub(workflow_dir: &Path) -> Result<()> {
    let bin_dir = workflow_dir.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir)?;
    let bun_path = if cfg!(windows) {
        bin_dir.join("bun.cmd")
    } else {
        bin_dir.join("bun")
    };
    let contents = if cfg!(windows) {
        "@echo off\r\necho %* | findstr /C:\"process.exit(1)\" >nul && (\r\n  echo out\r\n  echo err 1>&2\r\n  exit /b 1\r\n)\r\necho {\"ok\":true}\r\nexit /b 0\r\n"
    } else {
        "#!/bin/sh\ncase \"$*\" in *\"process.exit(1)\"*) printf 'out\\n'; printf 'err\\n' >&2; exit 1;; *) printf '{\"ok\":true}\\n'; exit 0;; esac\n"
    };
    fs::write(&bun_path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bun_path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_assertion_failure_bun_stub(workflow_dir: &Path) -> Result<()> {
    let bin_dir = workflow_dir.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir)?;
    let bun_path = bin_dir.join("bun");
    fs::write(
        &bun_path,
        "#!/bin/sh\ncase \"$*\" in *\"test src/tests\"*) printf 'AssertionError: Expected values to be strictly equal\\n' >&2; exit 1;; *) printf '{\"ok\":true}\\n'; exit 0;; esac\n",
    )?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bun_path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn write_valid_workflow(
    workflow_dir: &Path,
    id: &str,
    title: &str,
    description: &str,
) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    write_test_bun_stub(workflow_dir)?;
    fs::write(
        workflow_dir.join(".gitignore"),
        "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "id: {id}\ntitle: {title}\nuserDescription: {description}\ndependencies:\n  development:\n    - \"@types/node\"\n    - typescript\nvalidation:\n  commands:\n    - bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk\n    - bun test src/tests\n  contractSmoke:\n    input:\n      input: example\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        format!(
            "# {title}\n\n## Usage\n\nRun this workflow from Codex.\n\n## Workflow Runtime\n\nRuns on the managed Bun runtime.\n\n## Dependencies\n\nUses local package dependencies only.\n\n## Validation\n\nBuild, test, and contract smoke run through Bun.\n\n## Maintenance\n\nKeep workflow metadata, docs, and tests aligned.\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("DESIGN.md"),
        format!(
            "# {title} Design\n\n## Overview\n\nValid workflow fixture.\n\n## Architecture\n\nSource lives under src/ with tests under src/tests/.\n\n## Data Flow\n\nInput is mapped to a simple output.\n\n## Failure Handling\n\nUnexpected failures surface through the workflow result.\n\n## Recovery Behavior\n\nNo recovery behavior is required for this fixture.\n\n## Test Matrix\n\nPositive, negative, load, and autocomplete coverage markers are present.\n\n## Maintenance Notes\n\nKeep Bun validation commands current.\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("package.json"),
        format!(
            "{{\n  \"name\": \"codex-workflow-{}\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": {{\n    \"build\": \"bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk\",\n    \"test\": \"bun test src/tests\",\n    \"run\": \"bun src/workflow.ts\"\n  }},\n  \"devDependencies\": {{\n    \"@types/node\": \"1.0.0\",\n    \"typescript\": \"1.0.0\"\n  }}\n}}\n",
            id.replace('/', "-")
        ),
    )?;
    fs::write(
        workflow_dir.join("tsconfig.json"),
        "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; input?: string; }\nexport const WorkflowOutput = { toTuiMarkdown(_result: WorkflowOutput) { return { markdown: \"done\" }; } };\nexport default async function runWorkflow(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {\n  return { ok: true, input: input.input };\n}\nexport async function complete() { return []; }\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nimport \"../workflow.ts\";\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )?;
    fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    Ok(())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let pid = pid.to_string();
    std::process::Command::new("kill")
        .args(["-0", pid.as_str()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn write_bun_test_assertion_failure_fixture(workflow_dir: &Path) -> Result<()> {
    write_valid_workflow(
        workflow_dir,
        "broken/fix",
        "Assertion Failure",
        "Exercise assertion failure repair classification",
    )?;
    write_assertion_failure_bun_stub(workflow_dir)?;
    Ok(())
}

fn write_broken_repair_fixture(workflow_dir: &Path) -> Result<()> {
    fs::create_dir_all(workflow_dir.join(".git"))?;
    write_test_bun_stub(workflow_dir)?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: broken/other\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        "# Broken\n\n## Usage\n\n## Workflow Runtime\n",
    )?;
    fs::write(workflow_dir.join("DESIGN.md"), "# Broken Design\n")?;
    fs::write(
        workflow_dir.join("package.json"),
        "{\n  \"name\": \"broken\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; input: WorkflowInput; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function run(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> { return { ok: true, input: { ...input } }; }\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.load.test.ts"),
        "// workflow-covers: load\nimport \"./workflow.ts\";\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )?;
    Ok(())
}

fn write_unsupported_command_fixture(workflow_dir: &Path) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    write_test_bun_stub(workflow_dir)?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: broken/fix\nvalidation:\n  commands:\n    - bun --eval \"console.log('out'); console.error('err'); process.exit(1)\" # build test\n  contractSmoke:\n    input:\n      input: example\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        "# Workflow\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
    )?;
    fs::write(
        workflow_dir.join("DESIGN.md"),
        "# Workflow Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
    )?;
    fs::write(
        workflow_dir.join("package.json"),
        "{\n  \"name\": \"codex-workflow-failing-command\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": {\n    \"build\": \"bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk\",\n    \"test\": \"bun test src/tests\",\n    \"run\": \"bun src/workflow.ts\"\n  },\n  \"devDependencies\": {\n    \"@types/node\": \"1.0.0\",\n    \"typescript\": \"1.0.0\"\n  }\n}\n",
    )?;
    fs::write(
        workflow_dir.join("tsconfig.json"),
        "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function workflow() { return { ok: true }; }\nexport async function complete() { return []; }\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nimport \"../workflow.ts\";\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )?;
    fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    Ok(())
}

fn write_schema_repair_fixture(
    workflow_dir: &Path,
    id: &str,
    api: serde_json::Value,
    tool: Option<codex_workflows::WorkflowToolSpec>,
) -> Result<()> {
    write_valid_workflow(workflow_dir, id, "Schema Repair", "Repair schema metadata")?;
    codex_workflows::write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &codex_workflows::WorkflowSpec {
            id: id.to_string(),
            title: Some("Schema Repair".to_string()),
            user_description: Some("Repair schema metadata".to_string()),
            api,
            tool,
            dependencies: json!({
                "development": ["@types/node", "typescript"],
            }),
            validation: json!({
                "commands": [
                    "bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk",
                    "bun test src/tests"
                ],
                "contractSmoke": { "input": { "input": "example" } },
                "coverage": {
                    "positive": true,
                    "negative": true,
                    "progress": true,
                    "finalResult": true,
                    "failureUx": true,
                    "load": true,
                    "autocomplete": true,
                    "recovery": false,
                }
            }),
            ..Default::default()
        },
    )?;
    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_list_returns_discovered_workflows() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/reports/jira-summary");
    write_valid_workflow(
        &workflow_dir,
        "reports/jira-summary",
        "Jira Summary",
        "Summarize Jira work",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/list", Some(json!({})))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowListResponse = to_response(response)?;

    assert_eq!(response.workflows.len(), 1);
    assert_eq!(response.workflows[0].id, "reports/jira-summary");
    assert_eq!(
        response.workflows[0].title,
        Some("Jira Summary".to_string())
    );
    assert_eq!(
        response.workflows[0].validation.status,
        WorkflowValidationStatus::Valid
    );

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_list_and_read_include_enabled_native_workflow() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(&codex_home, "\n[workflows.engines.rust]\nenabled = true\n")?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/list", Some(json!({})))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowListResponse = to_response(response)?;

    assert_eq!(response.workflows.len(), 1);
    assert_eq!(response.workflows[0].id, "dev-cycle");
    assert_eq!(response.workflows[0].engine, WorkflowEngine::Rust);
    assert_eq!(
        response.workflows[0].title,
        Some("Development Cycle".to_string())
    );
    assert!(
        response.workflows[0]
            .command_option_hints
            .iter()
            .any(|hint| hint.display == "--stage-tests <auto|on|off>")
    );

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "dev-cycle" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowReadResponse = to_response(response)?;

    assert_eq!(response.workflow.engine, WorkflowEngine::Rust);
    assert_eq!(response.readme, None);
    assert!(response.workflow_yaml.contains("engine: rust"));
    assert!(response.workflow_yaml.contains("taskDescription"));

    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn workflow_run_start_runs_enabled_native_workflow_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(&codex_home, "\n[workflows.engines.rust]\nenabled = true\n")?;

    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(cwd.path().to_path_buf()))
        .loader_overrides(loader_overrides.clone())
        .build()
        .await?;
    let mut client = in_process::start(InProcessStartArgs {
        arg0_paths: Arg0DispatchPaths::default(),
        config: Arc::new(config),
        cli_overrides: Vec::new(),
        loader_overrides,
        cloud_requirements: CloudRequirementsLoader::default(),
        thread_config_loader: Arc::new(codex_config::NoopThreadConfigLoader),
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db: None,
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        config_warnings: Vec::new(),
        session_source: SessionSource::Cli.into(),
        enable_codex_api_key_env: false,
        initialize: InitializeParams {
            client_info: ClientInfo {
                name: "codex-app-server-tests".to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            capabilities: None,
        },
        channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        expose_workflow_app_server: true,
    })
    .await?;
    let result = client
        .request(ClientRequest::WorkflowRunStart {
            request_id: RequestId::Integer(1),
            params: WorkflowRunStartParams {
                id: "dev-cycle".to_string(),
                input: Some(json!({ "stageTests": "off" })),
                thread_id: None,
                stage_session_id: None,
                approval_handling: None,
            },
        })
        .await?
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let response: WorkflowRunStartResponse = serde_json::from_value(result)?;
    assert_eq!(response.run.status, WorkflowRunStatus::Running);
    let run_id = response.run.id.clone();

    let mut saw_planning_status = false;
    let mut saw_started_progress = false;
    let mut saw_markdown_result = false;
    timeout(DEFAULT_READ_TIMEOUT, async {
        while !(saw_planning_status && saw_started_progress && saw_markdown_result) {
            let event = client
                .next_event()
                .await
                .ok_or_else(|| anyhow::anyhow!("in-process event stream closed"))?;
            match event {
                InProcessServerEvent::ServerNotification(
                    ServerNotification::WorkflowRunProgress(notification),
                ) if notification.run_id == run_id => {
                    if notification.message == "Started native development cycle" {
                        saw_started_progress = true;
                    }
                    if let Some(status) = notification.status
                        && status.workflow_status == "planning"
                    {
                        assert_eq!(
                            status,
                            codex_app_server_protocol::WorkflowStatusUpdate {
                                workflow_name: "dev-cycle".to_string(),
                                workflow_status: "planning".to_string(),
                                threads: vec![codex_app_server_protocol::WorkflowThreadStatus {
                                    name: "planner".to_string(),
                                    status: "creating work packets".to_string(),
                                },],
                                child_statuses: Vec::new(),
                            }
                        );
                        saw_planning_status = true;
                    }
                }
                InProcessServerEvent::ServerNotification(
                    ServerNotification::WorkflowRunMarkdownResult(notification),
                ) if notification.run_id == run_id => {
                    assert!(notification.markdown.contains("# Development Cycle"));
                    assert!(notification.markdown.contains("Review Split:"));
                    saw_markdown_result = true;
                }
                _ => {}
            }
        }
        anyhow::Ok(())
    })
    .await??;

    let result = client
        .request(ClientRequest::WorkflowRunWait {
            request_id: RequestId::Integer(2),
            params: WorkflowRunWaitParams {
                run_id,
                timeout_ms: Some(/*timeout_ms*/ 5_000),
            },
        })
        .await?
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let response: WorkflowRunWaitResponse = serde_json::from_value(result)?;

    assert!(response.completed);
    assert_eq!(response.run.status, WorkflowRunStatus::Succeeded);
    let output = response.run.output.expect("native workflow output");
    assert_eq!(output["status"], json!("blocked"));
    assert_eq!(
        output["blockedReason"],
        json!("native agent runtime is unavailable")
    );
    assert_eq!(output["settings"]["stages"]["tests"], json!("off"));
    client.shutdown().await?;

    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn workflow_run_shutdown_kills_active_runtime_process_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/reports/stuck-review");
    write_valid_workflow(
        &workflow_dir,
        "reports/stuck-review",
        "Stuck Review",
        "Exercise workflow runtime shutdown",
    )?;
    let pid_path = codex_home.path().join("stuck-workflow-runtime.pid");
    let bin_dir = workflow_dir.join("node_modules/.bin");
    fs::create_dir_all(&bin_dir)?;
    let bun_path = bin_dir.join("bun");
    let pid_path_display = pid_path.display();
    fs::write(
        &bun_path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path_display}'\nexec sleep 600\n"),
    )?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bun_path, fs::Permissions::from_mode(0o755))?;

    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .fallback_cwd(Some(cwd.path().to_path_buf()))
        .loader_overrides(loader_overrides.clone())
        .build()
        .await?;
    let client = in_process::start(InProcessStartArgs {
        arg0_paths: Arg0DispatchPaths::default(),
        config: Arc::new(config),
        cli_overrides: Vec::new(),
        loader_overrides,
        cloud_requirements: CloudRequirementsLoader::default(),
        thread_config_loader: Arc::new(codex_config::NoopThreadConfigLoader),
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db: None,
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        config_warnings: Vec::new(),
        session_source: SessionSource::Cli.into(),
        enable_codex_api_key_env: false,
        initialize: InitializeParams {
            client_info: ClientInfo {
                name: "codex-app-server-tests".to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            capabilities: None,
        },
        channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        expose_workflow_app_server: true,
    })
    .await?;
    let result = client
        .request(ClientRequest::WorkflowRunStart {
            request_id: RequestId::Integer(1),
            params: WorkflowRunStartParams {
                id: "reports/stuck-review".to_string(),
                input: Some(json!({})),
                thread_id: None,
                stage_session_id: None,
                approval_handling: None,
            },
        })
        .await?
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let response: WorkflowRunStartResponse = serde_json::from_value(result)?;
    assert_eq!(response.run.status, WorkflowRunStatus::Running);

    let pid = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            if let Ok(pid) = fs::read_to_string(&pid_path)
                && let Ok(pid) = pid.trim().parse::<u32>()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for workflow runtime pid file"))?;
    assert!(
        process_is_alive(pid),
        "workflow runtime process should be alive before shutdown"
    );

    client.shutdown().await?;
    if timeout(DEFAULT_READ_TIMEOUT, async {
        while process_is_alive(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_err()
    {
        let pid = pid.to_string();
        let _ = std::process::Command::new("kill")
            .args(["-9", pid.as_str()])
            .status();
        bail!("workflow runtime process {pid} stayed alive after app-server shutdown");
    }

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_develop_location_project_overrides_global_default() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ndefault_location = \"global\"\n",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/develop",
            Some(json!({
                "description": "Create a project workflow",
                "id": "project-only",
                "command": "project-only",
                "location": "project",
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowDevelopResponse = to_response(response)?;

    assert_eq!(response.exit_code, 0);
    assert!(
        codex_home
            .path()
            .join(".codex/workflows/project-only/workflow.yaml")
            .is_file()
    );
    assert!(
        !codex_home
            .path()
            .join("workflows/project-only/workflow.yaml")
            .exists()
    );

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_validate_response_includes_non_zero_exit_code_for_invalid_status() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/reports/jira-summary");
    write_valid_workflow(
        &workflow_dir,
        "reports/jira-summary",
        "Jira Summary",
        "Summarize Jira work",
    )?;
    fs::remove_file(workflow_dir.join("DESIGN.md"))?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/validate",
            Some(json!({ "id": "reports/jira-summary" })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowValidateResponse = to_response(response)?;

    assert_eq!(response.exit_code, 1);
    assert!(response.message.contains("missing DESIGN.md"));

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_read_round_trips_new_validation_finding_variants() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/review/finding");
    write_valid_workflow(
        &workflow_dir,
        "review/finding",
        "Review Finding",
        "Exercise validation finding serialization",
    )?;
    let workflow_yaml_path = workflow_dir.join("workflow.yaml");
    let mut spec = codex_workflows::read_workflow_spec(&workflow_yaml_path)?;
    spec.validation = json!({
        "commands": ["bun test src/tests"],
        "contractSmoke": { "input": { "input": "example" } },
        "coverage": {
            "positive": true,
            "negative": true,
            "progress": true,
            "finalResult": true,
            "failureUx": true,
            "load": true,
            "autocomplete": true,
            "recovery": false,
        }
    });
    codex_workflows::write_workflow_spec(&workflow_yaml_path, &spec)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "review/finding" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowReadResponse = to_response(response)?;

    assert_eq!(
        response.workflow.validation.status,
        WorkflowValidationStatus::Invalid
    );
    let expected_finding = WorkflowValidationFindingInfo::MissingBuildValidationCommand {
        path: Path::new("workflow.yaml").to_path_buf(),
    };
    assert!(
        response
            .workflow
            .validation
            .findings
            .contains(&expected_finding),
        "findings: {:?}",
        response.workflow.validation.findings
    );

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_treats_bun_test_assertion_failure_as_unsupported_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\nrepair_mode = \"threshold:3\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    write_bun_test_assertion_failure_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::UnsupportedFindings
    );
    assert!(!response.repair.changed);
    assert!(response.repair.blocked_findings.is_empty());
    assert!(response.repair.unsupported_findings.iter().any(|finding| {
        matches!(
            finding,
            WorkflowValidationFindingInfo::ValidationCommandFailed { command, stderr, .. }
                if command == "bun test src/tests"
                    && stderr.contains("AssertionError")
        )
    }));
    assert_eq!(response.validation_command_results.len(), 2);
    assert!(response.validation_command_results[0].succeeded);
    assert!(!response.validation_command_results[1].succeeded);

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_returns_structured_result() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir)?;
    write_broken_repair_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert!(response.message.contains("Repairing workflow"));
    assert!(response.message.contains("Validation passed."));
    assert_eq!(response.repair.stop_reason, WorkflowRepairStopReason::Valid);
    assert!(response.repair.changed);
    assert!(!response.repair.applied_fixes.is_empty());
    assert_eq!(response.validation.status, WorkflowValidationStatus::Valid);
    assert!(response.validation.findings.is_empty());

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_returns_blocked_mode_result() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\nrepair_mode = \"metadata\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir)?;
    write_broken_repair_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::BlockedByRepairMode
    );
    assert!(response.message.contains("Blocked findings:"));
    assert!(!response.repair.blocked_findings.is_empty());
    assert!(response.repair.unsupported_findings.is_empty());

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_returns_unsupported_command_result() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir)?;
    write_unsupported_command_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::UnsupportedFindings
    );
    assert!(response.message.contains("Unsupported findings:"));
    assert!(!response.repair.unsupported_findings.is_empty());
    assert!(!response.repair.changed);
    assert_eq!(response.validation_command_results.len(), 1);
    assert!(
        response.validation_command_results[0]
            .stdout
            .contains("out")
    );
    assert!(
        response.validation_command_results[0]
            .stderr
            .contains("err")
    );

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_repairs_missing_design_and_schema_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/schema");
    write_schema_repair_fixture(
        &workflow_dir,
        "broken/schema",
        json!({
            "inputSchema": { "type": "object", "additionalProperties": true },
            "outputSchema": {
                "type": "object",
                "properties": {
                    "nested": { "type": "object" }
                }
            }
        }),
        Some(codex_workflows::WorkflowToolSpec {
            description: "Run broken/schema".to_string(),
            input_schema: json!({ "type": "object", "additionalProperties": true }),
            output_schema: json!({ "type": "object" }),
            ..Default::default()
        }),
    )?;
    fs::remove_file(workflow_dir.join("DESIGN.md"))?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/schema" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(response.repair.stop_reason, WorkflowRepairStopReason::Valid);
    assert!(response.repair.changed);
    assert_eq!(response.validation.status, WorkflowValidationStatus::Valid);
    assert!(response.validation.findings.is_empty());
    assert!(
        response
            .repair
            .applied_fixes
            .iter()
            .any(|fix| { fix.kind == WorkflowRepairActionKind::RepairDesign })
    );
    assert!(
        response
            .repair
            .applied_fixes
            .iter()
            .any(|fix| { fix.kind == WorkflowRepairActionKind::NormalizeValidationMetadata })
    );
    assert!(workflow_dir.join("DESIGN.md").is_file());

    let spec = codex_workflows::read_workflow_spec(&workflow_dir.join("workflow.yaml"))?;
    assert!(spec.api.is_null());
    let tool = spec.tool.expect("tool registration metadata is retained");
    assert_eq!(tool.output_schema["additionalProperties"], true);

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_blocked_schema_finding_round_trips_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\nrepair_mode = \"none\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/schema");
    write_schema_repair_fixture(
        &workflow_dir,
        "broken/schema",
        json!({
            "inputSchema": { "type": "object", "additionalProperties": true },
            "outputSchema": { "type": "object" }
        }),
        /*tool*/ None,
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/schema" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::BlockedByRepairMode
    );
    assert!(response.repair.blocked_findings.iter().any(|finding| {
        matches!(
            finding,
            WorkflowValidationFindingInfo::AmbiguousWorkflowOutputSchema { schema_path, .. }
                if schema_path == "api.outputSchema"
        )
    }));

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_repair_blocked_runtime_state_finding_round_trips_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\nrepair_mode = \"metadata\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/runtime-state");
    write_valid_workflow(
        &workflow_dir,
        "broken/runtime-state",
        "Runtime State",
        "Repair runtime state metadata",
    )?;
    fs::remove_file(workflow_dir.join(".gitignore"))?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/repair",
            Some(json!({ "id": "broken/runtime-state" })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::BlockedByRepairMode
    );
    assert!(response.repair.blocked_findings.iter().any(|finding| {
        matches!(
            finding,
            WorkflowValidationFindingInfo::RuntimeStateGitignoreMissing { patterns, .. }
                if patterns.iter().any(|pattern| pattern == "state/*")
        )
    }));

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_stage_session_id_keeps_edits_private_until_done() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/review/fix");
    write_valid_workflow(
        &workflow_dir,
        "review/fix",
        "Review Fix",
        "Repair a workflow",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/edit",
            Some(json!({
                "id": "review/fix",
                "instruction": "staged note",
                "stageSessionId": "session-123"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowEditResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "review/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let live_before_done: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        live_before_done
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("staged note")),
        false
    );

    let request_id = mcp
        .send_raw_request(
            "workflow/read",
            Some(json!({
                "id": "review/fix",
                "stageSessionId": "session-123"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let staged_read: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        staged_read
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("staged note")),
        true
    );

    let request_id = mcp
        .send_raw_request(
            "workflow/publish",
            Some(json!({
                "stageSessionId": "session-123"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowPublishResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "review/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let live_after_done: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        live_after_done
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("staged note")),
        true
    );

    Ok(())
}

#[tokio::test]
#[serial(workflow_process)]
async fn workflow_stage_session_id_discard_removes_staged_changes() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\ndependency_update_policy = \"manual\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/review/fix");
    write_valid_workflow(
        &workflow_dir,
        "review/fix",
        "Review Fix",
        "Repair a workflow",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/edit",
            Some(json!({
                "id": "review/fix",
                "instruction": "discarded note",
                "stageSessionId": "session-456"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowEditResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request(
            "workflow/discard",
            Some(json!({
                "stageSessionId": "session-456"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowDiscardResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request(
            "workflow/read",
            Some(json!({
                "id": "review/fix",
                "stageSessionId": "session-456"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let read_after_discard: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        read_after_discard
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("discarded note")),
        false
    );

    Ok(())
}
