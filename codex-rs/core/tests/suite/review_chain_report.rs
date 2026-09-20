use anyhow::Result;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewVerification;
use core_test_support::responses;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolated_review_rejects_sandbox_overrides() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let outside = TempDir::new()?;
    let escaped = outside.path().join("escaped");
    let arguments = serde_json::json!({
        "cmd": format!("touch {}", escaped.display()),
        "sandbox_permissions": "require_escalated"
    })
    .to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("escape-response"),
                responses::ev_function_call("escape-call", "exec_command", &arguments),
                responses::ev_completed("escape-response"),
            ]),
            responses::sse(assistant_sse(&empty_discovery_output())),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.overall_correctness, "patch is correct");
    assert!(!escaped.exists());
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let escape_output = requests[1].function_call_output("escape-call");
    let escape_output_text = escape_output.to_string();
    assert!(
        escape_output_text.contains("review stages cannot override sandbox permissions")
            || escape_output_text.contains("cannot ask for escalated permissions"),
        "unexpected escape output: {escape_output}"
    );
    let first_body = requests[0].body_json();
    let exec_tool = first_body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|tool| tool["name"] == "exec_command")
        .expect("exec_command tool");
    assert!(exec_tool["parameters"]["properties"]["sandbox_permissions"].is_null());
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_review_cannot_read_outside_the_checkout() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let outside = TempDir::new()?;
    let secret_path = outside.path().join("secret.txt");
    std::fs::write(&secret_path, "review-secret-value\n")?;
    let arguments = serde_json::json!({
        "cmd": format!("cat {}", secret_path.display())
    })
    .to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("read-response"),
                responses::ev_function_call("read-call", "exec_command", &arguments),
                responses::ev_completed("read-response"),
            ]),
            responses::sse(assistant_sse(&empty_discovery_output())),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    completed_review(&test.codex).await;

    let output = mock.requests()[1]
        .function_call_output("read-call")
        .to_string();
    assert!(!output.contains("review-secret-value"), "{output}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_review_can_run_git_in_a_linked_worktree() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repository = std::sync::Arc::new(TempDir::new()?);
    run_git(repository.path(), &["init", "--initial-branch=main"])?;
    run_git(
        repository.path(),
        &["config", "user.email", "test@example.com"],
    )?;
    run_git(repository.path(), &["config", "user.name", "Test User"])?;
    std::fs::write(repository.path().join("tracked.txt"), "tracked\n")?;
    run_git(repository.path(), &["add", "tracked.txt"])?;
    run_git(repository.path(), &["commit", "-m", "base"])?;
    run_git(repository.path(), &["branch", "feature"])?;

    let arguments = serde_json::json!({"cmd": "git status --short"}).to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("git-status-response"),
                responses::ev_function_call("git-status-call", "exec_command", &arguments),
                responses::ev_completed("git-status-response"),
            ]),
            responses::sse(assistant_sse(&empty_discovery_output())),
        ],
    )
    .await;
    let worktree_repository = std::sync::Arc::clone(&repository);
    let mut builder = test_codex().with_workspace_setup(move |cwd, _filesystem| async move {
        std::fs::remove_dir(cwd.as_path())?;
        let cwd = cwd
            .as_path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("worktree path is not UTF-8"))?;
        run_git(
            worktree_repository.path(),
            &["worktree", "add", cwd, "feature"],
        )?;
        Ok(())
    });
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    completed_review(&test.codex).await;

    let output = mock.requests()[1]
        .function_call_output("git-status-call")
        .to_string();
    assert!(output.contains("exited with code 0"), "{output}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_and_double_check_cannot_apply_workspace_patches() -> Result<()> {
    skip_if_no_network!(Ok(()));

    for verification in [
        ReviewVerification::SinglePass,
        ReviewVerification::DoubleCheck,
    ] {
        let server = start_mock_server().await;
        let mut responses = vec![responses::sse(vec![
            responses::ev_response_created("patch-response"),
            responses::ev_apply_patch_custom_tool_call(
                "patch-call",
                "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _forbidden = true; }\n*** End Patch\n",
            ),
            responses::ev_completed("patch-response"),
        ])];
        if verification == ReviewVerification::SinglePass {
            responses.push(responses::sse(assistant_sse(&empty_discovery_output())));
        } else {
            responses.insert(0, responses::sse(assistant_sse(&discovery_output())));
            responses.push(responses::sse(assistant_sse(&verification_output())));
        }
        let mock = mount_sse_sequence(&server, responses).await;
        let mut builder = test_codex();
        let test = builder.build_with_auto_env(&server).await?;
        std::fs::write(test.workspace_path("candidate.rs"), "fn candidate() {}\n")?;

        test.codex
            .submit(Op::Review {
                review_request: ReviewRequest {
                    target: ReviewTarget::WholeRepository,
                    verification,
                    action: ReviewAction::Report,
                    user_facing_hint: None,
                },
            })
            .await?;
        completed_review(&test.codex).await;

        assert_eq!(
            std::fs::read_to_string(test.workspace_path("candidate.rs"))?,
            "fn candidate() {}\n"
        );
        let requests = mock.requests();
        let output_request = if verification == ReviewVerification::SinglePass {
            &requests[1]
        } else {
            &requests[2]
        };
        let tool_output = output_request.custom_tool_call_output("patch-call");
        assert!(
            !tool_output.to_string().contains("Success. Updated"),
            "{tool_output}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_discovery_stops_after_two_repairs_without_fix() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse("invalid one")),
            responses::sse(assistant_sse("invalid two")),
            responses::sse(assistant_sse("invalid three")),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::DoubleCheck,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.overall_correctness, "uncertain");
    assert!(
        output
            .overall_explanation
            .contains("review discovery failed")
    );
    assert_eq!(output.resolution, None);
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_instructions(&requests[1]).as_deref(),
        Some(codex_core::REVIEW_REPAIR_PROMPT)
    );
    assert_eq!(
        request_instructions(&requests[2]).as_deref(),
        Some(codex_core::REVIEW_REPAIR_PROMPT)
    );
    assert!(request_text(&requests[1]).contains("<review_repair_input>"));
    assert!(request_text(&requests[1]).contains("invalid one"));
    assert!(
        requests[1]
            .body_json()
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_double_check_stops_after_two_repairs_without_fix() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse("bad verification one")),
            responses::sse(assistant_sse("bad verification two")),
            responses::sse(assistant_sse("bad verification three")),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    write_candidate(&test).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::DoubleCheck,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.overall_correctness, "uncertain");
    assert!(output.overall_explanation.contains("verification failed"));
    assert_eq!(output.resolution, None);
    assert_eq!(mock.requests().len(), 4);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantically_invalid_discovery_is_repaired() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let invalid = serde_json::json!({
        "candidates": [],
        "assessment": {
            "verdict": "banana",
            "explanation": "Invalid verdict.",
            "confidenceScore": 2.0
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&invalid)),
            responses::sse(assistant_sse(&empty_discovery_output())),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.overall_correctness, "patch is correct");
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_instructions(&requests[1]).as_deref(),
        Some(codex_core::REVIEW_REPAIR_PROMPT)
    );
    assert!(max_input_text_bytes(&requests[1]) <= 8 * 1024);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_checked_pre_existing_finding_is_report_only() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse(&pre_existing_verification_output())),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    run_git(test.cwd_path(), &["init", "--initial-branch=main"])?;
    run_git(
        test.cwd_path(),
        &["config", "user.email", "test@example.com"],
    )?;
    run_git(test.cwd_path(), &["config", "user.name", "Test User"])?;
    std::fs::write(test.workspace_path("base.txt"), "base\n")?;
    run_git(test.cwd_path(), &["add", "base.txt"])?;
    run_git(test.cwd_path(), &["commit", "-m", "base"])?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;
    std::fs::write(test.workspace_path("candidate.rs"), "fn candidate() {}\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::DoubleCheck,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.findings.len(), 1);
    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::True);
    assert_eq!(output.resolution, None);
    assert_eq!(mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_check_collects_source_from_selected_executor() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse(&verification_output())),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    write_candidate(&test).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::DoubleCheck,
                action: ReviewAction::Report,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.findings.len(), 1);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let verifier_input = request_text(&requests[1]);
    assert!(verifier_input.contains("<review_source>"));
    assert!(verifier_input.contains("candidate.rs:8: fn candidate() {}"));
    Ok(())
}

async fn completed_review(codex: &codex_core::CodexThread) -> ReviewOutputEvent {
    let event = wait_for_event(codex, |event| {
        matches!(event, EventMsg::ExitedReviewMode(_))
    })
    .await;
    let EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
        review_output: Some(output),
        ..
    }) = event
    else {
        panic!("expected completed review output");
    };
    wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    output
}

fn assistant_sse(text: &str) -> Vec<serde_json::Value> {
    assistant_sse_with_ids("message", "response", text)
}

fn assistant_sse_with_ids(
    message_id: &str,
    response_id: &str,
    text: &str,
) -> Vec<serde_json::Value> {
    vec![
        responses::ev_assistant_message(message_id, text),
        responses::ev_completed(response_id),
    ]
}

fn empty_discovery_output() -> String {
    serde_json::json!({
        "candidates": [],
        "assessment": {
            "verdict": "patch is correct",
            "explanation": "No issues found.",
            "confidenceScore": 1.0
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string()
}

fn discovery_output() -> String {
    serde_json::json!({
        "candidates": [{
            "title": "Handle the failed send",
            "body": "The failed send is ignored.",
            "confidenceScore": 0.95,
            "priority": 1,
            "codeLocation": {
                "absoluteFilePath": "candidate.rs",
                "lineRange": {"start": 8, "end": 8}
            }
        }],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "One issue may remain.",
            "confidenceScore": 0.8
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string()
}

fn verification_output() -> String {
    serde_json::json!({
        "findings": [{
            "candidateIndex": 0,
            "preExisting": "undetermined",
            "preExistingFixRationale": null
        }],
        "outOfScopeFindings": [],
        "unverifiedFindings": [],
        "rejectedCandidateIndices": [],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "The issue is valid.",
            "confidenceScore": 0.95
        }
    })
    .to_string()
}

fn pre_existing_verification_output() -> String {
    serde_json::json!({
        "findings": [{
            "candidateIndex": 0,
            "preExisting": "true",
            "preExistingFixRationale": "The nearby change could safely address it."
        }],
        "outOfScopeFindings": [],
        "unverifiedFindings": [],
        "rejectedCandidateIndices": [],
        "assessment": {
            "verdict": "patch is correct",
            "explanation": "Only a pre-existing issue remains.",
            "confidenceScore": 0.95
        }
    })
    .to_string()
}

fn request_instructions(
    request: &core_test_support::responses::ResponsesRequest,
) -> Option<String> {
    request.body_json()["instructions"]
        .as_str()
        .map(str::to_string)
}

fn request_text(request: &core_test_support::responses::ResponsesRequest) -> String {
    request
        .input()
        .iter()
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn max_input_text_bytes(request: &core_test_support::responses::ResponsesRequest) -> usize {
    request
        .input()
        .iter()
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
        .map(str::len)
        .max()
        .unwrap_or_default()
}

fn run_git(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn write_candidate(test: &core_test_support::test_codex::TestCodex) -> Result<()> {
    let path = test
        .executor_environment()
        .selection()
        .cwd
        .join("candidate.rs")?;
    test.fs()
        .write_file(
            &path,
            b"\n\n\n\n\n\n\nfn candidate() {}\n".to_vec(),
            /*sandbox*/ None,
        )
        .await?;
    Ok(())
}
