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

#[tokio::test]
async fn slash_workflow_shows_single_line_status_and_final_result_in_terminal_output() -> Result<()>
{
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_single_status_workflow(&codex_home.path().join("workflows/code-review"))?;

    run_workflow_visibility_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        "code-review",
        vec![
            "Workflow code-review: starting".to_string(),
            "Workflow code-review: preparing".to_string(),
            "Workflow Result".to_string(),
        ],
        vec![
            "Workflow started:".to_string(),
            "Workflow finished:".to_string(),
            "__CODEX_WORKFLOW_EVENT__".to_string(),
            "\"workflowName\"".to_string(),
            "-> reviewer-a: scanning".to_string(),
        ],
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn slash_workflow_shows_multi_thread_status_in_terminal_output() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_multi_thread_workflow(codex_home.path())?;

    run_workflow_visibility_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        "parent-review",
        vec![
            "Workflow parent-review: starting".to_string(),
            "Workflow parent-review: coordinating".to_string(),
            "-> reviewer-a: scanning".to_string(),
            "-> reviewer-b: waiting".to_string(),
            "Workflow Result".to_string(),
        ],
        vec![
            "Workflow started:".to_string(),
            "Workflow finished:".to_string(),
            "__CODEX_WORKFLOW_EVENT__".to_string(),
            "\"workflowName\"".to_string(),
            "child-review".to_string(),
        ],
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn slash_workflow_failure_clears_status_and_shows_error_in_terminal_output() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_failing_workflow(&codex_home.path().join("workflows/failing-review"))?;

    run_workflow_visibility_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        "failing-review",
        vec![
            "Workflow failing-review: starting".to_string(),
            "Workflow failing-review: preparing".to_string(),
            "Workflow failed: failing-review".to_string(),
            "intentional workflow failure".to_string(),
        ],
        vec![
            "Workflow Result".to_string(),
            "__CODEX_WORKFLOW_EVENT__".to_string(),
            "\"workflowName\"".to_string(),
        ],
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn slash_workflow_active_interrupt_exits_without_rendering_final_result() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_slow_workflow(&codex_home.path().join("workflows/slow-review"))?;

    run_workflow_active_interrupt_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        "slow-review",
        vec![
            "Workflow slow-review: starting".to_string(),
            "Workflow slow-review: waiting".to_string(),
        ],
        vec![
            "Workflow Result".to_string(),
            "slow workflow completed".to_string(),
            "__CODEX_WORKFLOW_EVENT__".to_string(),
            "\"workflowName\"".to_string(),
        ],
    )
    .await?;

    Ok(())
}

async fn run_workflow_visibility_session(
    repo_root: &Path,
    codex: &Path,
    codex_home: &Path,
    workspace: &Path,
    command: &str,
    required_snippets: Vec<String>,
    forbidden_snippets: Vec<String>,
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
        codex_utils_pty::TerminalSize::default(),
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
    let interrupt_writer = writer_tx.clone();
    let workflow_writer = writer_tx.clone();

    let workflow_status_prefix = format!("Workflow {command}:");
    let workflow_command = format!("/{command}");
    let mut saw_required = vec![false; required_snippets.len()];
    let mut answered_cursor_query = false;
    let mut sent_workflow_command = false;
    let mut sent_interrupts = false;
    let mut scheduled_workflow_command = false;

    let exit_code_result = timeout(Duration::from_secs(/*secs*/ 90), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        let has_cursor_query = chunk.windows(4).any(|window| window == b"\x1b[6n");
                        if has_cursor_query {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }

                        if answered_cursor_query && !scheduled_workflow_command {
                            scheduled_workflow_command = true;
                            let workflow_writer = workflow_writer.clone();
                            let workflow_command = workflow_command.clone();
                            tokio::spawn(async move {
                                sleep(Duration::from_secs(/*secs*/ 1)).await;
                                let _ = workflow_writer.send(workflow_command.into_bytes()).await;
                                sleep(Duration::from_millis(/*millis*/ 100)).await;
                                let _ = workflow_writer.send(b"\r".to_vec()).await;
                            });
                        }

                        output.extend_from_slice(&chunk);
                        parser.process(&chunk);

                        let output_text = String::from_utf8_lossy(&output);
                        let screen = parser.screen().contents();
                        sent_workflow_command |= output_text.contains(&workflow_command)
                            || screen.contains(&workflow_command)
                            || screen.contains(&workflow_status_prefix);

                        for (seen, snippet) in saw_required.iter_mut().zip(required_snippets.iter()) {
                            *seen |= output_text.contains(snippet) || screen.contains(snippet);
                        }

                        if sent_workflow_command
                            && saw_required.iter().all(|seen| *seen)
                            && !screen.contains(&workflow_status_prefix)
                            && !sent_interrupts
                        {
                            sent_interrupts = true;
                            let interrupt_writer = interrupt_writer.clone();
                            tokio::spawn(async move {
                                // The first interrupts can be consumed by the just-finished
                                // workflow; keep retrying until the TUI is idle and exits.
                                for _ in 0..20 {
                                    let _ = interrupt_writer.send(vec![3]).await;
                                    sleep(Duration::from_millis(/*millis*/ 500)).await;
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
                "timed out waiting for codex workflow visibility test to exit; screen: {}\nraw output: {}",
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
        "unexpected exit code from codex workflow visibility test: {exit_code}; output: {output_text}",
    );

    anyhow::ensure!(
        sent_workflow_command,
        "workflow command was never sent; output: {output_text}"
    );
    let missing = required_snippets
        .iter()
        .zip(saw_required.iter())
        .filter_map(|(snippet, seen)| (!seen).then_some(snippet.clone()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "workflow visibility test missed snippets {:?}; screen: {}\noutput: {}",
        missing,
        parser.screen().contents(),
        output_text,
    );
    let final_screen = parser.screen().contents();
    for snippet in forbidden_snippets {
        anyhow::ensure!(
            !output_text.contains(&snippet) && !final_screen.contains(&snippet),
            "workflow visibility test saw forbidden snippet `{snippet}`; screen: {final_screen}\noutput: {output_text}",
        );
    }

    Ok(())
}

async fn run_workflow_active_interrupt_session(
    repo_root: &Path,
    codex: &Path,
    codex_home: &Path,
    workspace: &Path,
    command: &str,
    required_snippets: Vec<String>,
    forbidden_snippets: Vec<String>,
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
        codex_utils_pty::TerminalSize::default(),
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
    let interrupt_writer = writer_tx.clone();
    let workflow_writer = writer_tx.clone();

    let workflow_status_prefix = format!("Workflow {command}:");
    let workflow_cancel_snippet = format!("Workflow canceled: {command}");
    let workflow_command = format!("/{command}");
    let mut saw_required = vec![false; required_snippets.len()];
    let mut saw_workflow_canceled = false;
    let mut answered_cursor_query = false;
    let mut sent_workflow_command = false;
    let mut sent_interrupts = false;
    let mut sent_exit_keys = false;
    let mut scheduled_workflow_command = false;

    let exit_code_result = timeout(Duration::from_secs(/*secs*/ 45), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        let has_cursor_query = chunk.windows(4).any(|window| window == b"\x1b[6n");
                        if has_cursor_query {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }

                        if answered_cursor_query && !scheduled_workflow_command {
                            scheduled_workflow_command = true;
                            let workflow_writer = workflow_writer.clone();
                            let workflow_command = workflow_command.clone();
                            tokio::spawn(async move {
                                sleep(Duration::from_secs(/*secs*/ 1)).await;
                                let _ = workflow_writer.send(workflow_command.into_bytes()).await;
                                sleep(Duration::from_millis(/*millis*/ 100)).await;
                                let _ = workflow_writer.send(b"\r".to_vec()).await;
                            });
                        }

                        output.extend_from_slice(&chunk);
                        parser.process(&chunk);

                        let output_text = String::from_utf8_lossy(&output);
                        let screen = parser.screen().contents();
                        sent_workflow_command |= output_text.contains(&workflow_command)
                            || screen.contains(&workflow_command)
                            || screen.contains(&workflow_status_prefix);
                        saw_workflow_canceled |= output_text.contains(&workflow_cancel_snippet)
                            || screen.contains(&workflow_cancel_snippet);

                        for (seen, snippet) in saw_required.iter_mut().zip(required_snippets.iter()) {
                            *seen |= output_text.contains(snippet) || screen.contains(snippet);
                        }

                        if sent_workflow_command
                            && saw_required.iter().all(|seen| *seen)
                            && screen.contains(&workflow_status_prefix)
                            && !sent_interrupts
                        {
                            sent_interrupts = true;
                            let interrupt_writer = interrupt_writer.clone();
                            tokio::spawn(async move {
                                let _ = interrupt_writer.send(vec![3]).await;
                            });
                        }

                        if saw_workflow_canceled && !sent_exit_keys {
                            sent_exit_keys = true;
                            let interrupt_writer = interrupt_writer.clone();
                            tokio::spawn(async move {
                                for _ in 0..2 {
                                    let _ = interrupt_writer.send(vec![4]).await;
                                    sleep(Duration::from_millis(/*millis*/ 100)).await;
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
                "timed out waiting for codex workflow active interrupt test to exit; screen: {}\nraw output: {}",
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
        "unexpected exit code from codex workflow active interrupt test: {exit_code}; output: {output_text}",
    );

    anyhow::ensure!(
        sent_workflow_command,
        "workflow command was never sent; output: {output_text}"
    );
    anyhow::ensure!(
        sent_interrupts,
        "workflow active interrupt test never sent interrupts; screen: {}\noutput: {}",
        parser.screen().contents(),
        output_text,
    );
    let missing = required_snippets
        .iter()
        .zip(saw_required.iter())
        .filter_map(|(snippet, seen)| (!seen).then_some(snippet.clone()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "workflow active interrupt test missed snippets {:?}; screen: {}\noutput: {}",
        missing,
        parser.screen().contents(),
        output_text,
    );
    let final_screen = parser.screen().contents();
    for snippet in forbidden_snippets {
        anyhow::ensure!(
            !output_text.contains(&snippet) && !final_screen.contains(&snippet),
            "workflow active interrupt test saw forbidden snippet `{snippet}`; screen: {final_screen}\noutput: {output_text}",
        );
    }

    Ok(())
}

fn write_single_status_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        "code-review",
        "code-review",
        "Code Review",
        r##"const workflow = {
  async run(ctx) {
    ctx.status({ workflowName: "code-review", workflowStatus: "preparing" });
    await new Promise((resolve) => setTimeout(resolve, 250));
    ctx.reportToUserMarkdown("# Workflow Result\n\nVisible from PTY integration test.\n");
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
};

export default workflow;
"##,
    )
}

fn write_failing_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        "failing-review",
        "failing-review",
        "Failing Review",
        r##"const workflow = {
  async run(ctx) {
    ctx.status({ workflowName: "failing-review", workflowStatus: "preparing" });
    await new Promise((resolve) => setTimeout(resolve, 250));
    throw new Error("intentional workflow failure");
  },
};

export default workflow;
"##,
    )
}

fn write_slow_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        "slow-review",
        "slow-review",
        "Slow Review",
        r##"const workflow = {
  async run(ctx) {
    ctx.status({ workflowName: "slow-review", workflowStatus: "waiting" });
    await new Promise((resolve) => setTimeout(resolve, 3000));
    ctx.reportToUserMarkdown("# Workflow Result\n\nslow workflow completed\n");
    return { workflowStatus: "done" };
  },
};

export default workflow;
"##,
    )
}

fn write_multi_thread_workflow(codex_home: &Path) -> Result<()> {
    write_workflow_fixture(
        &codex_home.join("workflows/parent-review"),
        "parent-review",
        "parent-review",
        "Parent Review",
        r##"const workflow = {
  async run(ctx) {
    ctx.status({
      workflowName: "parent-review",
      workflowStatus: "coordinating",
      threads: [
        { name: "reviewer-a", status: "scanning" },
        { name: "reviewer-b", status: "waiting" },
      ],
    });
    ctx.reportToUserMarkdown("# Workflow Result\n\nVisible from PTY integration test.\n");
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
};

export default workflow;
"##,
    )
}
