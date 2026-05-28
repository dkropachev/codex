use anyhow::Result;
use tempfile::tempdir;

use super::workflow_autocomplete::WorkflowAutocompletePopupKey;
use super::workflow_autocomplete::WorkflowAutocompleteScenario;
use super::workflow_autocomplete::run_workflow_autocomplete_session;
use super::workflow_autocomplete::write_report_workflow;
use super::workflow_autocomplete::write_review_workflow;
use super::workflow_test_support::ensure_codex_binary;
use super::workflow_test_support::write_trusted_workspace_config;

#[tokio::test]
async fn slash_workflow_exact_command_preserves_quoted_arguments_end_to_end() -> Result<()> {
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
            typed_prefix: r#"/code-review --review-id "two words" --assignee "alice bob""#,
            completion_text: None,
            popup_snippets: &[r#""two words""#, r#""alice bob""#],
            ordered_popup_snippets: &[],
            run_snippets: &[
                "Input text: --review-id two words --assignee alice bob",
                r#"Input argv json: ["--review-id","two words","--assignee","alice bob"]"#,
                "Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &[r#"Input argv json: ["--review-id","two","words""#],
            popup_keys: &[WorkflowAutocompletePopupKey::Enter],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_popup_up_wraps_to_last_match_and_runs_it() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_two_report_workflows(codex_home.path())?;

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
                WorkflowAutocompletePopupKey::Up,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_popup_down_wraps_to_first_match_after_last_and_runs_it() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_two_report_workflows(codex_home.path())?;

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
                "Workflow alpha-report: starting",
                "Workflow alpha-report: drafting",
                "Alpha Workflow Result",
            ],
            post_key_snippets: &[],
            forbidden_snippets: &["Beta Workflow Result"],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Down,
                WorkflowAutocompletePopupKey::Down,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_popup_escape_dismisses_without_running_workflow() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    write_trusted_workspace_config(codex_home.path(), workspace.path())?;
    write_two_report_workflows(codex_home.path())?;

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
            run_snippets: &[],
            post_key_snippets: &[r#"Unrecognized command '/report'"#],
            forbidden_snippets: &[
                "Workflow alpha-report: starting",
                "Workflow beta-report: starting",
                "Alpha Workflow Result",
                "Beta Workflow Result",
            ],
            popup_keys: &[
                WorkflowAutocompletePopupKey::Escape,
                WorkflowAutocompletePopupKey::Enter,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn slash_workflow_dynamic_completion_ignores_stale_result_after_argument_changes()
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
            typed_prefix: "/code-review --slow",
            completion_text: Some(" fast"),
            popup_snippets: &["loading completions..."],
            ordered_popup_snippets: &[],
            run_snippets: &[],
            post_key_snippets: &["--format fresh"],
            forbidden_snippets: &["--slow stale", "Stale report"],
            popup_keys: &[],
        },
    )
    .await
}

fn write_two_report_workflows(codex_home: &std::path::Path) -> Result<()> {
    write_report_workflow(
        &codex_home.join("workflows/reports/alpha-report"),
        "reports/alpha-report",
        "alpha-report",
        "Alpha Report",
        "drafting",
        "Alpha Workflow Result",
    )?;
    write_report_workflow(
        &codex_home.join("workflows/reports/beta-report"),
        "reports/beta-report",
        "beta-report",
        "Beta Report",
        "publishing",
        "Beta Workflow Result",
    )
}
