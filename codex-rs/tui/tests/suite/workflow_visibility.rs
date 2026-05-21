use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use tempfile::tempdir;
use tokio::select;
use tokio::time::sleep;
use tokio::time::timeout;

#[tokio::test]
async fn slash_workflow_shows_progress_and_final_result_in_terminal_output() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    std::fs::create_dir_all(workspace.path().join(".git"))?;

    let workflow_dir = codex_home.path().join("workflows/code-review");
    write_test_workflow(&workflow_dir)?;

    let config_contents = format!(
        r#"model = "gpt-oss:20b"
model_provider = "ollama"
check_for_update_on_startup = false
suppress_unstable_features_warning = true

[analytics]
enabled = false

[projects."{workspace}"]
trust_level = "trusted"
"#,
        workspace = workspace.path().display(),
    );
    std::fs::write(codex_home.path().join("config.toml"), config_contents)?;

    let codex = ensure_codex_binary(&repo_root)?;

    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        codex_home.path().display().to_string(),
    );

    let args = vec![
        "--no-alt-screen".to_string(),
        "--enable".to_string(),
        "workflows".to_string(),
        "-C".to_string(),
        workspace.path().display().to_string(),
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
        &repo_root,
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

    let mut answered_cursor_query = false;
    let mut sent_workflow_command = false;
    let mut sent_interrupts = false;
    let mut scheduled_workflow_command = false;
    let mut saw_started_message = false;
    let mut saw_progress_status = false;
    let mut saw_result_markdown = false;
    let mut saw_finished_message = false;

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

                        if answered_cursor_query && !scheduled_workflow_command {
                            scheduled_workflow_command = true;
                            let workflow_writer = workflow_writer.clone();
                            tokio::spawn(async move {
                                sleep(Duration::from_secs(1)).await;
                                let _ = workflow_writer.send(b"/code-review".to_vec()).await;
                                sleep(Duration::from_millis(100)).await;
                                let _ = workflow_writer.send(b"\r".to_vec()).await;
                            });
                        }

                        output.extend_from_slice(&chunk);
                        parser.process(&chunk);

                        let output_text = String::from_utf8_lossy(&output);
                        let screen = parser.screen().contents();
                        sent_workflow_command |= output_text.contains("/code-review")
                            || screen.contains("/code-review")
                            || output_text.contains("Workflow started: code-review")
                            || screen.contains("Workflow started: code-review");
                        saw_started_message |= output_text.contains("Workflow started: code-review")
                            || screen.contains("Workflow started: code-review");
                        saw_progress_status |= screen.contains("Preparing workflow handoff");
                        saw_result_markdown |= screen.contains("Workflow Result")
                            || output_text.contains("Workflow Result");
                        saw_finished_message |= output_text.contains("Workflow finished: code-review")
                            || screen.contains("Workflow finished: code-review");

                        if sent_workflow_command
                            && saw_started_message
                            && saw_progress_status
                            && saw_result_markdown
                            && saw_finished_message
                            && !sent_interrupts
                        {
                            sent_interrupts = true;
                            for _ in 0..4 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(150)).await;
                            }
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
    anyhow::ensure!(
        saw_started_message,
        "workflow start message was not visible; screen: {}\noutput: {}",
        parser.screen().contents(),
        output_text,
    );
    anyhow::ensure!(
        saw_progress_status,
        "workflow progress status was not visible; screen: {}\noutput: {}",
        parser.screen().contents(),
        output_text,
    );
    anyhow::ensure!(
        saw_result_markdown,
        "workflow markdown result was not visible; screen: {}\noutput: {}",
        parser.screen().contents(),
        output_text,
    );
    anyhow::ensure!(
        saw_finished_message,
        "workflow finished message was not visible; screen: {}\noutput: {}",
        parser.screen().contents(),
        output_text,
    );

    Ok(())
}

fn write_test_workflow(workflow_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(workflow_dir.join("src"))?;
    std::fs::create_dir_all(workflow_dir.join("state"))?;
    std::fs::create_dir_all(workflow_dir.join("node_modules/.bin"))?;
    std::fs::create_dir_all(workflow_dir.join(".git"))?;
    std::fs::write(workflow_dir.join("README.md"), "# Code Review\n")?;
    std::fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        r#"id: code-review
command: code-review
title: /code-review
userDescription: Emit progress and final markdown for TUI integration tests.
"#,
    )?;
    std::fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "code-review-workflow-test",
  "private": true,
  "type": "module"
}
"#,
    )?;
    std::fs::write(
        workflow_dir.join("src/workflow.ts"),
        r##"const workflow = {
  async run(ctx) {
    ctx.progress("Preparing workflow handoff", { stage: "testing", step: 1 });
    await new Promise((resolve) => setTimeout(resolve, 250));
    ctx.reportToUserMarkdown("# Workflow Result\n\nVisible from PTY integration test.\n");
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
};

export default workflow;
"##,
    )?;
    std::fs::write(
        workflow_dir.join("node_modules/.bin/tsx"),
        "#!/bin/sh\nprintf '%s\\n' '__CODEX_WORKFLOW_EVENT__{\"type\":\"progress\",\"message\":\"Preparing workflow handoff\",\"data\":{\"stage\":\"testing\",\"step\":1}}' >&2\n/bin/sleep 1\nprintf '%s\\n' '__CODEX_WORKFLOW_EVENT__{\"type\":\"reportToUserMarkdown\",\"markdown\":\"# Workflow Result\\n\\nVisible from PTY integration test.\\n\"}' >&2\n/bin/sleep 1\nprintf '%s\\n' '{\"workflowStatus\":\"done\"}'\n",
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(
        workflow_dir.join("node_modules/.bin/tsx"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    Ok(())
}

fn ensure_codex_binary(repo_root: &Path) -> Result<PathBuf> {
    let build_status = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("codex-cli")
        .arg("--bin")
        .arg("codex")
        .current_dir(repo_root.join("codex-rs"))
        .status()?;
    anyhow::ensure!(build_status.success(), "failed to build codex binary");

    let fallback = repo_root.join("codex-rs/target/debug/codex");
    anyhow::ensure!(
        fallback.is_file(),
        "codex binary is unavailable after build: {}",
        fallback.display()
    );
    Ok(fallback)
}
