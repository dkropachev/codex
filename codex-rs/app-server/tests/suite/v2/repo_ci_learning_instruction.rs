use anyhow::Context;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RepoCiLearningInstructionReadParams;
use codex_app_server_protocol::RepoCiLearningInstructionReadResponse;
use codex_app_server_protocol::RepoCiLearningInstructionScopeParams;
use codex_app_server_protocol::RepoCiLearningInstructionWriteParams;
use codex_app_server_protocol::RepoCiLearningInstructionWriteResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn repo_ci_learning_instruction_read_write_clear_explicit_repo() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let scope = explicit_repo_scope();
    let read = read_instruction(&mut mcp, scope.clone()).await?;
    assert_eq!(
        read,
        RepoCiLearningInstructionReadResponse {
            scope: "githubRepo:openai/codex".to_string(),
            instruction: None,
        }
    );

    let write = write_instruction(
        &mut mcp,
        scope.clone(),
        "Use nextest.  Skip slow integration tests.",
    )
    .await?;
    assert_eq!(
        write,
        RepoCiLearningInstructionWriteResponse {
            scope: "githubRepo:openai/codex".to_string(),
            old_instruction: None,
            new_instruction: Some("Use nextest. Skip slow integration tests.".to_string()),
        }
    );

    let read = read_instruction(&mut mcp, scope.clone()).await?;
    assert_eq!(read.instruction, write.new_instruction);
    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(config.contains("learning_instruction"));
    assert!(!config.contains("learning_instructions"));

    let clear = write_instruction(&mut mcp, scope, "").await?;
    assert_eq!(clear.old_instruction, write.new_instruction);
    assert_eq!(clear.new_instruction, None);
    Ok(())
}

#[tokio::test]
async fn repo_ci_learning_instruction_cwd_prefers_github_repo_scope() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path())?;
    init_git_repo(codex_home.path())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let write = write_instruction(&mut mcp, cwd_scope(), "Use the repo-local justfile.").await?;
    assert_eq!(
        write,
        RepoCiLearningInstructionWriteResponse {
            scope: "githubRepo:openai/codex".to_string(),
            old_instruction: None,
            new_instruction: Some("Use the repo-local justfile.".to_string()),
        }
    );

    let read = read_instruction(&mut mcp, explicit_repo_scope()).await?;
    assert_eq!(read.instruction, write.new_instruction);
    Ok(())
}

async fn read_instruction(
    mcp: &mut McpProcess,
    scope: RepoCiLearningInstructionScopeParams,
) -> Result<RepoCiLearningInstructionReadResponse> {
    let id = mcp
        .send_repo_ci_learning_instruction_read_request(RepoCiLearningInstructionReadParams {
            scope,
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(id)),
    )
    .await??;
    to_response::<RepoCiLearningInstructionReadResponse>(response)
}

async fn write_instruction(
    mcp: &mut McpProcess,
    scope: RepoCiLearningInstructionScopeParams,
    instruction: &str,
) -> Result<RepoCiLearningInstructionWriteResponse> {
    let id = mcp
        .send_repo_ci_learning_instruction_write_request(RepoCiLearningInstructionWriteParams {
            scope,
            instruction: instruction.to_string(),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(id)),
    )
    .await??;
    to_response::<RepoCiLearningInstructionWriteResponse>(response)
}

fn explicit_repo_scope() -> RepoCiLearningInstructionScopeParams {
    RepoCiLearningInstructionScopeParams {
        cwd: None,
        github_repo: Some("openai/codex".to_string()),
    }
}

fn cwd_scope() -> RepoCiLearningInstructionScopeParams {
    RepoCiLearningInstructionScopeParams {
        cwd: Some(true),
        github_repo: None,
    }
}

fn create_config_toml(codex_home: &Path) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"
model = "mock-model"
approval_policy = "never"
"#,
    )
}

fn init_git_repo(path: &Path) -> Result<()> {
    run_git(path, ["init"])?;
    run_git(
        path,
        [
            "remote",
            "add",
            "origin",
            "https://github.com/openai/codex.git",
        ],
    )
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .context("failed to run git")?;
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!("git failed: {}", String::from_utf8_lossy(&output.stderr))
    }
}
