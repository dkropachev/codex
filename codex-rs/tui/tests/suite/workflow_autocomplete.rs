use std::collections::HashMap;
use std::io::Write;
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
pub(super) enum WorkflowAutocompletePopupKey {
    Down,
    Enter,
    Escape,
    Tab,
    Up,
}

pub(super) struct WorkflowAutocompleteScenario<'a> {
    pub(super) typed_prefix: &'a str,
    pub(super) completion_text: Option<&'a str>,
    pub(super) popup_snippets: &'a [&'a str],
    pub(super) ordered_popup_snippets: &'a [&'a str],
    pub(super) run_snippets: &'a [&'a str],
    pub(super) post_key_snippets: &'a [&'a str],
    pub(super) forbidden_snippets: &'a [&'a str],
    pub(super) popup_keys: &'a [WorkflowAutocompletePopupKey],
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
            completion_text: None,
            popup_snippets: &["/summary", "Jira Summary"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Workflow summary: starting",
                "Workflow summary: reviewing",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[],
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
            completion_text: None,
            popup_snippets: &["/summary", "Jira Summary"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Workflow summary: starting",
                "Workflow summary: reviewing",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_exact_command_shows_option_hints() -> Result<()> {
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
            completion_text: None,
            popup_snippets: &[
                "/code-review",
                "Code Review",
                "--all-comments",
                "Include all comment bodies",
                "--format <summary|full>",
                "Output format",
                "--report-id <string>",
                "Report identifier",
            ],
            ordered_popup_snippets: &[],
            run_snippets: &[],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[],
        },
    )
    .await
}

#[tokio::test]
async fn slash_native_workflow_exact_command_shows_stage_option_hints() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(codex_home.path().join("config.toml"))?
        .write_all(b"\n[workflows.engines.rust]\nenabled = true\n")?;

    run_workflow_autocomplete_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        WorkflowAutocompleteScenario {
            typed_prefix: "/dev-cycle --stage-t",
            completion_text: None,
            popup_snippets: &[
                "/dev-cycle",
                "Development Cycle",
                "--stage-tests <auto|on|off>",
                "Test stage mode.",
            ],
            ordered_popup_snippets: &[],
            run_snippets: &[],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_autocomplete_shows_static_option_popup_for_argument_prefix() -> Result<()> {
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
            typed_prefix: "/code-review --a",
            completion_text: None,
            popup_snippets: &[
                "--all-comments",
                "Include all comment bodies",
                "--archive",
                "Archive the reviewed branch",
                "--assignee <string>",
                "Reviewer assignment",
            ],
            ordered_popup_snippets: &[],
            run_snippets: &[],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_autocomplete_completes_dynamic_argument_prefix_and_runs_workflow_end_to_end()
-> Result<()> {
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
            typed_prefix: "/code-review --report-id ",
            completion_text: Some("1034"),
            popup_snippets: &[
                "--report-id 1034",
                "Primary report",
                "--report-id 1035",
                "Fallback report",
            ],
            ordered_popup_snippets: &[],
            run_snippets: &["Input argv: --report-id 1034", "Workflow Result"],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[WorkflowAutocompletePopupKey::Enter],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_static_option_tab_does_not_commit_placeholder() -> Result<()> {
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
            typed_prefix: "/code-review --a",
            completion_text: None,
            popup_snippets: &["--assignee <string>", "Reviewer assignment"],
            ordered_popup_snippets: &[],
            run_snippets: &[],
            post_key_snippets: &["unexpected argument '--a'"],
            forbidden_snippets: &[
                "Input text: --assignee <string>",
                "Input argv: --assignee <string>",
                "Workflow Result",
            ],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_static_option_tab_completes_unique_option_name_prefix() -> Result<()> {
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
            typed_prefix: "/code-review --appl",
            completion_text: None,
            popup_snippets: &["--apply-patch", "Apply patch"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Input text: --apply-patch",
                "Input argv: --apply-patch",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_autocomplete_tab_completes_unique_dynamic_value_prefix() -> Result<()> {
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
            typed_prefix: "/code-review --report-id 103",
            completion_text: None,
            popup_snippets: &["--report-id 1034"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Input text: --report-id 1034",
                "Input argv: --report-id 1034",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &["Input text: --report-id 103\n"],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_autocomplete_commits_unique_dynamic_preview_and_runs_workflow_end_to_end()
-> Result<()> {
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
            typed_prefix: "/code-review --report-id 1034",
            completion_text: None,
            popup_snippets: &["--format summary"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Input text: --report-id 1034 --format summary",
                "Input argv: --report-id 1034 --format summary",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Tab,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_exact_command_with_args_enter_runs_workflow_without_committing_preview()
-> Result<()> {
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
            typed_prefix: "/code-review --report-id 1034",
            completion_text: None,
            popup_snippets: &["--format summary"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Input text: --report-id 1034",
                "Input argv: --report-id 1034",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[
                "Input text: --report-id 1034 --format summary",
                "Input argv: --report-id 1034 --format summary",
            ],
            popup_keys: &[WorkflowAutocompletePopupKey::Enter],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_search_term_enter_runs_unambiguous_workflow() -> Result<()> {
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
            completion_text: None,
            popup_snippets: &["/summary", "Jira Summary"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Workflow summary: starting",
                "Workflow summary: reviewing",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[r#"Unrecognized command '/report'"#],
            popup_keys: &[WorkflowAutocompletePopupKey::Enter],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_search_term_enter_after_explicit_selection_runs_workflow() -> Result<()> {
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
            completion_text: None,
            popup_snippets: &["/summary", "Jira Summary"],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Workflow summary: starting",
                "Workflow summary: reviewing",
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[r#"Unrecognized command '/report'"#],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Down,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_search_term_orders_multiple_matches_and_runs_selected_workflow()
-> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_report_workflow(
        &codex_home.path().join("workflows/reports/alpha-report"),
        "reports/alpha-report",
        "alpha-report",
        "Alpha Report",
        "drafting",
        "Alpha Workflow Result",
    )?;
    write_report_workflow(
        &codex_home.path().join("workflows/reports/beta-report"),
        "reports/beta-report",
        "beta-report",
        "Beta Report",
        "publishing",
        "Beta Workflow Result",
    )?;

    run_workflow_autocomplete_session(
        &repo_root,
        &codex,
        codex_home.path(),
        workspace.path(),
        WorkflowAutocompleteScenario {
            typed_prefix: "/report",
            completion_text: None,
            popup_snippets: &[
                "/alpha-report",
                "Alpha Report",
                "/beta-report",
                "Beta Report",
            ],
            ordered_popup_snippets: &["/alpha-report", "/beta-report"],
            run_snippets: &[
                "Workflow beta-report: starting",
                "Workflow beta-report: publishing",
                "Beta Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &["Alpha Workflow Result"],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Down,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

pub(super) async fn run_workflow_autocomplete_session(
    repo_root: &Path,
    codex: &Path,
    codex_home: &Path,
    workspace: &Path,
    scenario: WorkflowAutocompleteScenario<'_>,
) -> Result<()> {
    let _workflow_e2e_guard = super::workflow_test_support::workflow_e2e_lock().await;
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
        /*rows*/ 32, /*cols*/ 80, /*scrollback_len*/ 0,
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
    let mut saw_ordered_popup = scenario.ordered_popup_snippets.is_empty();
    let mut saw_run = vec![false; scenario.run_snippets.len()];
    let mut saw_post_key = vec![false; scenario.post_key_snippets.len()];
    let should_run = !scenario.run_snippets.is_empty();
    let should_wait_for_post_key = !scenario.post_key_snippets.is_empty();

    let exit_code_result = timeout(Duration::from_secs(90), async {
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
                        saw_ordered_popup |= snippets_in_order(&output_text, scenario.ordered_popup_snippets)
                            || snippets_in_order(&screen, scenario.ordered_popup_snippets);

                        let popup_ready_on_screen = scenario
                            .popup_snippets
                            .iter()
                            .all(|snippet| screen.contains(snippet))
                            && (scenario.ordered_popup_snippets.is_empty()
                                || snippets_in_order(&screen, scenario.ordered_popup_snippets));
                        if popup_ready_on_screen && !scheduled_completion {
                            scheduled_completion = true;
                            let complete_writer = complete_writer.clone();
                            let completion_text = scenario.completion_text.map(str::to_string);
                            let popup_keys = scenario.popup_keys.to_vec();
                            tokio::spawn(async move {
                                if let Some(completion_text) = completion_text {
                                    sleep(Duration::from_millis(500)).await;
                                    let _ = complete_writer.send(completion_text.into_bytes()).await;
                                }
                                for key in popup_keys {
                                    sleep(Duration::from_millis(500)).await;
                                    let payload = match key {
                                        WorkflowAutocompletePopupKey::Down => b"\x1b[B".to_vec(),
                                        WorkflowAutocompletePopupKey::Enter => b"\r".to_vec(),
                                        WorkflowAutocompletePopupKey::Escape => b"\x1b".to_vec(),
                                        WorkflowAutocompletePopupKey::Tab => b"\t".to_vec(),
                                        WorkflowAutocompletePopupKey::Up => b"\x1b[A".to_vec(),
                                    };
                                    let _ = complete_writer.send(payload).await;
                                }
                            });
                        }

                        for (seen, snippet) in saw_run.iter_mut().zip(scenario.run_snippets.iter()) {
                            *seen |= output_text.contains(snippet) || screen.contains(snippet);
                        }

                        for (seen, snippet) in saw_post_key.iter_mut().zip(scenario.post_key_snippets.iter()) {
                            *seen |= output_text.contains(snippet) || screen.contains(snippet);
                        }

                        if should_run {
                            if saw_run.iter().all(|seen| *seen) && !sent_interrupts {
                                sent_interrupts = true;
                                session.terminate();
                            }
                        } else if should_wait_for_post_key {
                            if scheduled_completion
                                && saw_post_key.iter().all(|seen| *seen)
                                && !sent_interrupts
                            {
                                sent_interrupts = true;
                                session.terminate();
                            }
                        } else if scheduled_completion
                            && saw_popup.iter().all(|seen| *seen)
                            && !sent_interrupts
                        {
                            sent_interrupts = true;
                            let interrupt_writer = writer_tx.clone();
                            tokio::spawn(async move {
                                sleep(Duration::from_millis(500)).await;
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

    let output_text = String::from_utf8_lossy(&output);
    let exit_code = match exit_code_result {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            let screen = parser.screen().contents();
            if output_text.contains("Workflow Result") || screen.contains("Workflow Result") {
                1
            } else {
                anyhow::bail!(
                    "timed out waiting for workflow autocomplete test to exit; screen: {}\nraw output: {}",
                    parser.screen().contents(),
                    output_text,
                );
            }
        }
    };

    let interrupt_only_output = {
        let trimmed_output = output_text.trim();
        !trimmed_output.is_empty()
            && trimmed_output
                .chars()
                .all(|character| character == '^' || character == 'C' || character.is_whitespace())
    };

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
    anyhow::ensure!(
        saw_ordered_popup,
        "workflow autocomplete test did not see ordered popup snippets {:?}; screen: {}\noutput: {}",
        scenario.ordered_popup_snippets,
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

    let missing_post_key = scenario
        .post_key_snippets
        .iter()
        .zip(saw_post_key.iter())
        .filter_map(|(snippet, seen)| (!seen).then_some((*snippet).to_string()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing_post_key.is_empty(),
        "workflow autocomplete test missed post-key snippets {:?}; screen: {}\noutput: {}",
        missing_post_key,
        parser.screen().contents(),
        output_text,
    );

    anyhow::ensure!(
        exit_code == 0
            || exit_code == 130
            || (exit_code == 1 && (interrupt_only_output || missing_run.is_empty())),
        "unexpected exit code from workflow autocomplete test: {exit_code}; output: {output_text}",
    );

    let final_screen = parser.screen().contents();
    for snippet in scenario.forbidden_snippets {
        anyhow::ensure!(
            !output_text.contains(snippet) && !final_screen.contains(snippet),
            "workflow autocomplete test saw forbidden snippet `{snippet}`; screen: {final_screen}\noutput: {output_text}",
        );
    }

    Ok(())
}

fn snippets_in_order(haystack: &str, snippets: &[&str]) -> bool {
    let mut remainder = haystack;
    for snippet in snippets {
        let Some(index) = remainder.find(snippet) else {
            return false;
        };
        remainder = &remainder[index + snippet.len()..];
    }
    true
}

fn write_autocomplete_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        "reports/jira-summary",
        "summary",
        "Jira Summary",
        r##"interface WorkflowContext {
  status(status: { workflowName: string; workflowStatus: string }): void;
  reportToUserMarkdown(markdown: string): void;
}

export interface WorkflowInput {}

export interface WorkflowOutput {
  workflowStatus: string;
}

interface DefinedWorkflow<Input, Output> {
  run(ctx: WorkflowContext, input: Input): Promise<Output>;
}

function defineWorkflow<Input, Output>(workflow: DefinedWorkflow<Input, Output>): DefinedWorkflow<Input, Output> {
  return workflow;
}

export default defineWorkflow<WorkflowInput, WorkflowOutput>({
  async run(ctx, _input) {
    ctx.status({ workflowName: "summary", workflowStatus: "reviewing" });
    await new Promise((resolve) => setTimeout(resolve, 250));
    ctx.reportToUserMarkdown("# Workflow Result\n\nAutocomplete integration test.\n");
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
});
"##,
    )
}

pub(super) fn write_report_workflow(
    workflow_dir: &Path,
    id: &str,
    command: &str,
    title: &str,
    status: &str,
    result: &str,
) -> Result<()> {
    let workflow_source = format!(
        r##"interface WorkflowContext {{
  status(status: {{ workflowName: string; workflowStatus: string }}): void;
  reportToUserMarkdown(markdown: string): void;
}}

export interface WorkflowInput {{}}

export interface WorkflowOutput {{
  workflowStatus: string;
}}

interface DefinedWorkflow<Input, Output> {{
  run(ctx: WorkflowContext, input: Input): Promise<Output>;
}}

function defineWorkflow<Input, Output>(workflow: DefinedWorkflow<Input, Output>): DefinedWorkflow<Input, Output> {{
  return workflow;
}}

export default defineWorkflow<WorkflowInput, WorkflowOutput>({{
  async run(ctx, _input) {{
    ctx.status({{ workflowName: "{command}", workflowStatus: "{status}" }});
    await new Promise((resolve) => setTimeout(resolve, 250));
    ctx.reportToUserMarkdown("# Workflow Result\n\n{result}\n");
    await new Promise((resolve) => setTimeout(resolve, 250));
    return {{ workflowStatus: "done" }};
  }},
}});
"##
    );
    write_workflow_fixture(workflow_dir, id, command, title, &workflow_source)
}

pub(super) fn write_review_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture_with_metadata(
        workflow_dir,
        "code-review",
        "code-review",
        "Code Review",
        r##"interface WorkflowContext {
  status(status: { workflowName: string; workflowStatus: string }): void;
  reportToUserMarkdown(markdown: string): void;
}

export interface WorkflowInput {
  /** Review identifier */
  reviewId?: string;
  /** Reviewer assignment */
  assignee?: string;
  /** Archive the reviewed branch */
  archive?: boolean;
  /** Apply patch */
  applyPatch?: boolean;
  /** Include all comment bodies */
  allComments?: boolean;
  /** Report identifier */
  reportId?: string;
  /** Output format */
  format?: "summary" | "full";
  /** Include comment bodies */
  includeComments?: boolean;
}

export interface WorkflowOutput {
  workflowStatus: string;
}

type WorkflowCompletionMode = "field" | "value";

interface WorkflowCompletionRequest<Input> {
  input: Partial<Input>;
  activeField?: string;
  prefix: string;
  mode: WorkflowCompletionMode;
  replacementPrefix?: string;
}

type WorkflowCompletionSuggestion<Input> =
  | { type: "field"; field: string; display?: string; insertText?: string; description?: string }
  | { type: "value"; value: string | number | boolean; display?: string; insertText?: string; description?: string }
  | { type: "patch"; insertText: string; display?: string; description?: string };

interface DefinedWorkflow<Input, Output> {
  run(ctx: WorkflowContext, input: Input): Promise<Output>;
  complete?(
    ctx: WorkflowContext,
    request: WorkflowCompletionRequest<Input>,
  ): Promise<WorkflowCompletionSuggestion<Input>[]>;
}

function defineWorkflow<Input, Output>(workflow: DefinedWorkflow<Input, Output>): DefinedWorkflow<Input, Output> {
  return workflow;
}

function fullValueSuggestion<Input>(
  request: WorkflowCompletionRequest<Input>,
  value: string,
  description: string,
): WorkflowCompletionSuggestion<Input> {
  const display = `${request.replacementPrefix ?? ""}${value}`;
  return { type: "value", value, display, description };
}

function argvFromInput(input: WorkflowInput): string[] {
  const argv: string[] = [];
  const push = (flag: string, value: string | boolean | undefined) => {
    if (value === undefined || value === false) {
      return;
    }
    argv.push(flag);
    if (value !== true) {
      argv.push(String(value));
    }
  };

  push("--review-id", input.reviewId);
  push("--report-id", input.reportId);
  push("--format", input.format);
  push("--assignee", input.assignee);
  push("--archive", input.archive);
  push("--apply-patch", input.applyPatch);
  push("--all-comments", input.allComments);
  push("--include-comments", input.includeComments);
  return argv;
}

export default defineWorkflow<WorkflowInput, WorkflowOutput>({
  async complete(_ctx, request) {
    if (request.mode === "field" && request.prefix === "slow") {
      await new Promise((resolve) => setTimeout(resolve, 1500));
      return [
        {
          type: "patch",
          display: "--slow stale",
          insertText: "--slow stale",
          description: "Stale report",
        },
      ];
    }

    if (request.activeField === "slow" && request.prefix === "fast") {
      return [
        {
          type: "value",
          value: "fast --format fresh",
          display: "--slow fast --format fresh",
          description: "Fresh report",
        },
      ];
    }

    if (request.activeField === "reportId" && request.prefix === "103") {
      return [
        fullValueSuggestion(request, "1034", "Primary report"),
      ];
    }

    if (request.activeField === "reportId" && request.prefix === "1034") {
      return [
        fullValueSuggestion(request, "1034 --format summary", "Focused summary output"),
      ];
    }

    if (request.activeField === "reportId" && request.prefix === "") {
      return [
        fullValueSuggestion(request, "1034", "Primary report"),
        fullValueSuggestion(request, "1035", "Fallback report"),
      ];
    }

    if (request.mode === "field" && request.prefix === "") {
      return [
        {
          type: "patch",
          display: "--review-id review-123",
          insertText: "--review-id review-123",
          description: "Pending review",
        },
      ];
    }
    return [];
  },

  async run(ctx, input) {
    ctx.status({ workflowName: "code-review", workflowStatus: "reviewing" });
    await new Promise((resolve) => setTimeout(resolve, 250));
    const argvArray = argvFromInput(input ?? {});
    const argv = argvArray.join(" ");
    const argvJson = JSON.stringify(argvArray);
    const text = argv;
    ctx.reportToUserMarkdown(
      `# Workflow Result\n\nInput text: ${text}\nInput argv: ${argv}\nInput argv json: ${argvJson}\nInput json: ${JSON.stringify(input ?? {})}\n\nCode review complete.\n`,
    );
    await new Promise((resolve) => setTimeout(resolve, 250));
    return { workflowStatus: "done" };
  },
});
"##,
        "",
    )
}
