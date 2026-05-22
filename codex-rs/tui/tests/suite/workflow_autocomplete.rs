use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tempfile::tempdir;
use tokio::select;
use tokio::time::sleep;
use tokio::time::timeout;

use super::workflow_test_support::ensure_codex_binary;
use super::workflow_test_support::write_trusted_workspace_config;
use super::workflow_test_support::write_workflow_fixture;
use super::workflow_test_support::write_workflow_fixture_with_metadata;

#[derive(Clone, Copy)]
enum WorkflowAutocompletePopupKey {
    Down,
    Enter,
    Tab,
}

struct WorkflowAutocompleteScenario<'a> {
    typed_prefix: &'a str,
    popup_snippets: &'a [&'a str],
    run_snippets: &'a [&'a str],
    workflow_status_prefix: &'a str,
    popup_keys: &'a [WorkflowAutocompletePopupKey],
}

#[tokio::test]
async fn slash_workflow_autocomplete_completes_title_prefix_and_runs_workflow_end_to_end()
-> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_autocomplete_workflow(&codex_home.path().join("workflows/reports/jira-summary"))?;

    run_workflow_autocomplete_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        WorkflowAutocompleteScenario {
            typed_prefix: "/jira",
            popup_snippets: &["/summary", "Jira Summary"],
            run_snippets: &[
                "Workflow summary: starting",
                "Workflow summary: reviewing",
                "Workflow Result",
            ],
            workflow_status_prefix: "Workflow summary:",
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_autocomplete_completes_search_term_and_runs_workflow_end_to_end()
-> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_autocomplete_workflow(&codex_home.path().join("workflows/reports/jira-summary"))?;

    run_workflow_autocomplete_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        WorkflowAutocompleteScenario {
            typed_prefix: "/report",
            popup_snippets: &["/summary", "Jira Summary"],
            run_snippets: &[
                "Workflow summary: starting",
                "Workflow summary: reviewing",
                "Workflow Result",
            ],
            workflow_status_prefix: "Workflow summary:",
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_exact_command_shows_option_hints_and_runs_workflow_end_to_end() -> Result<()>
{
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_review_workflow(&codex_home.path().join("workflows/code-review"))?;

    run_workflow_autocomplete_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        WorkflowAutocompleteScenario {
            typed_prefix: "/code-review",
            popup_snippets: &[
                "/code-review",
                "Code Review",
                "--review-id <string>",
                "--format <summary|full>",
                "--review-id review-123",
            ],
            run_snippets: &[
                "Workflow code-review: starting",
                "Workflow code-review: reviewing",
                "Workflow Result",
                "review-123",
            ],
            workflow_status_prefix: "Workflow code-review:",
            popup_keys: &[
                WorkflowAutocompletePopupKey::Down,
                WorkflowAutocompletePopupKey::Enter,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

async fn run_workflow_autocomplete_session(
    repo_root: &Path,
    codex: &Path,
    codex_home: &Path,
    workspace: &Path,
    scenario: WorkflowAutocompleteScenario<'_>,
) -> Result<()> {
    let mut env = HashMap::new();
    env.insert("CODEX_HOME".to_string(), codex_home.display().to_string());
    env.insert(
        "CODEX_WORKFLOW_RUNTIME_MODE".to_string(),
        "process".to_string(),
    );

    let args = vec![
        "--no-alt-screen".to_string(),
        "--enable".to_string(),
        "workflows".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
        "-a".to_string(),
        "never".to_string(),
        "-s".to_string(),
        "danger-full-access".to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ];

    let spawned = codex_utils_pty::spawn_pty_process(
        codex.to_string_lossy().as_ref(),
        &args,
        repo_root,
        &env,
        &None,
        codex_utils_pty::TerminalSize {
            rows: 32,
            cols: 100,
        },
    )
    .await?;

    let mut parser = vt100::Parser::new(
        /*rows*/ 24, /*cols*/ 80, /*scrollback_len*/ 0,
    );
    let mut output = Vec::new();
    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let input_writer = writer_tx.clone();
    let complete_writer = writer_tx.clone();
    let _interrupt_writer = writer_tx.clone();

    let mut answered_cursor_query = false;
    let mut scheduled_input = false;
    let mut scheduled_completion = false;
    let mut sent_interrupts = false;
    let mut saw_popup = vec![false; scenario.popup_snippets.len()];
    let mut saw_run = vec![false; scenario.run_snippets.len()];

    let exit_code_result = timeout(Duration::from_secs(30), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        let has_cursor_query = chunk.windows(4).any(|window| window == b"\x1b[6n");
                        if has_cursor_query {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }

                        if answered_cursor_query && !scheduled_input {
                            scheduled_input = true;
                            let input_writer = input_writer.clone();
                            let typed_prefix = scenario.typed_prefix.to_string();
                            tokio::spawn(async move {
                                sleep(Duration::from_secs(1)).await;
                                let _ = input_writer.send(typed_prefix.into_bytes()).await;
                            });
                        }

                        output.extend_from_slice(&chunk);
                        parser.process(&chunk);

                        let output_text = String::from_utf8_lossy(&output);
                        let screen = parser.screen().contents();

                        for (seen, snippet) in saw_popup.iter_mut().zip(scenario.popup_snippets.iter()) {
                            *seen |= output_text.contains(snippet) || screen.contains(snippet);
                        }

                        if saw_popup.iter().all(|seen| *seen) && !scheduled_completion {
                            scheduled_completion = true;
                            let complete_writer = complete_writer.clone();
                            let popup_keys = scenario.popup_keys.to_vec();
                            tokio::spawn(async move {
                                for key in popup_keys {
                                    sleep(Duration::from_millis(150)).await;
                                    let payload = match key {
                                        WorkflowAutocompletePopupKey::Down => b"\x1b[B".to_vec(),
                                        WorkflowAutocompletePopupKey::Enter => b"\r".to_vec(),
                                        WorkflowAutocompletePopupKey::Tab => b"\t".to_vec(),
                                    };
                                    let _ = complete_writer.send(payload).await;
                                }
                            });
                        }

                        for (seen, snippet) in saw_run.iter_mut().zip(scenario.run_snippets.iter()) {
                            *seen |= output_text.contains(snippet) || screen.contains(snippet);
                        }

                        if saw_run.iter().all(|seen| *seen)
                            && !screen.contains(scenario.workflow_status_prefix)
                            && !sent_interrupts
                        {
                            sent_interrupts = true;
                            let interrupt_writer = writer_tx.clone();
                            tokio::spawn(async move {
                                for _ in 0..4 {
                                    let _ = interrupt_writer.send(vec![3]).await;
                                    sleep(Duration::from_millis(150)).await;
                                }
                            });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                result = &mut exit_rx => break result,
            }
        }
    })
    .await;

    let exit_code = match exit_code_result {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            anyhow::bail!(
                "timed out waiting for workflow autocomplete test to exit; screen: {}\nraw output: {}",
                parser.screen().contents(),
                String::from_utf8_lossy(&output),
            );
        }
    };

    let output_text = String::from_utf8_lossy(&output);
    let interrupt_only_output = {
        let trimmed_output = output_text.trim();
        !trimmed_output.is_empty()
            && trimmed_output
                .chars()
                .all(|character| character == '^' || character == 'C' || character.is_whitespace())
    };
    anyhow::ensure!(
        exit_code == 0 || exit_code == 130 || (exit_code == 1 && interrupt_only_output),
        "unexpected exit code from workflow autocomplete test: {exit_code}; output: {output_text}",
    );

    let missing_popup = scenario
        .popup_snippets
        .iter()
        .zip(saw_popup.iter())
        .filter_map(|(snippet, seen)| (!seen).then_some((*snippet).to_string()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing_popup.is_empty(),
        "workflow autocomplete test missed popup snippets {:?}; screen: {}\noutput: {}",
        missing_popup,
        parser.screen().contents(),
        output_text,
    );

    let missing_run = scenario
        .run_snippets
        .iter()
        .zip(saw_run.iter())
        .filter_map(|(snippet, seen)| (!seen).then_some((*snippet).to_string()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing_run.is_empty(),
        "workflow autocomplete test missed run snippets {:?}; screen: {}\noutput: {}",
        missing_run,
        parser.screen().contents(),
        output_text,
    );

    Ok(())
}

fn write_autocomplete_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        "reports/jira-summary",
        "summary",
        "Jira Summary",
        r##"const workflow = {
  async run(ctx) {
    ctx.status({ workflowName: "summary", workflowStatus: "reviewing" });
    await new Promise((resolve) => setTimeout(resolve, 250));
    ctx.reportToUserMarkdown("# Workflow Result\n\nAutocomplete integration test.\n");
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
};

export default workflow;
"##,
    )
}

fn write_review_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture_with_metadata(
        workflow_dir,
        "code-review",
        "code-review",
        "Code Review",
        r##"const workflow = {
  async complete(_ctx, request) {
    if (Array.isArray(request.argv) && request.argv.length === 0) {
      return [
        {
          display: "--review-id review-123",
          insertText: "--review-id review-123",
          description: "Pending review",
        },
      ];
    }
    return [];
  },

  async run(ctx) {
    ctx.status({ workflowName: "code-review", workflowStatus: "reviewing" });
    await new Promise((resolve) => setTimeout(resolve, 250));
    ctx.reportToUserMarkdown(`# Workflow Result\n\nCode review complete.\n`);
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
};

export default workflow;
"##,
        r#"api:
  inputSchema:
    type: object
    required:
      - reviewId
    properties:
      reviewId:
        type: string
        description: Review identifier
      format:
        type: string
        enum:
          - summary
          - full
        description: Output format
      includeComments:
        type: boolean
        description: Include comment bodies
"#,
    )
}
