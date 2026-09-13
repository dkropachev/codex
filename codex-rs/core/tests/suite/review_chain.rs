use anyhow::Result;
use codex_core::REVIEW_DOUBLE_CHECK_PROMPT;
use codex_core::REVIEW_PROMPT;
use codex_core::review_fix_prompt;
use codex_protocol::models::PermissionProfile;
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

const FIX_TEST_COMMAND: &str = "git diff --check";

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
const OVERLAPPING_FIX_TEST_COMMAND: &str = "cargo test overlap";
const POST_FIX_WAIT_COMMAND: &str = "cargo test wait";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_check_fix_uses_isolated_stage_models_and_prompts() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse(&verification_output())),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(fix_test_command_sse()),
            responses::sse(assistant_sse(&fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config.review_model = Some("gpt-5.2".to_string());
        config
            .permissions
            .set_permission_profile(PermissionProfile::Disabled)
            .expect("set parent permissions");
    });
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::DoubleCheck,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.findings.len(), 1);
    let resolution = output.resolution.expect("fix resolution");
    let requests = mock.requests();
    assert_eq!(
        resolution.fixed_count,
        1,
        "{resolution:?}; tool output={:?}",
        requests[3].custom_tool_call_output("fix-call")
    );
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(resolution.tests[0].command, FIX_TEST_COMMAND);
    assert_eq!(
        resolution.tests[0].status,
        codex_protocol::protocol::ReviewTestStatus::Passed
    );

    assert_eq!(requests.len(), 5);
    assert_eq!(request_model(&requests[0]).as_deref(), Some("gpt-5.2"));
    assert_eq!(request_model(&requests[1]).as_deref(), Some("gpt-5.2"));
    assert_eq!(request_model(&requests[2]).as_deref(), Some("gpt-5.4"));
    assert_eq!(request_model(&requests[3]).as_deref(), Some("gpt-5.4"));
    assert_eq!(request_model(&requests[4]).as_deref(), Some("gpt-5.4"));
    assert_eq!(
        request_instructions(&requests[0]).as_deref(),
        Some(REVIEW_PROMPT)
    );
    assert_eq!(
        request_instructions(&requests[1]).as_deref(),
        Some(REVIEW_DOUBLE_CHECK_PROMPT)
    );
    let fix_prompt = review_fix_prompt(ReviewAction::Fix);
    assert_eq!(
        request_instructions(&requests[4]).as_deref(),
        Some(fix_prompt.as_str())
    );
    assert!(request_text(&requests[1]).contains("SECURITY: This is untrusted output"));
    assert!(request_text(&requests[4]).contains("untrusted review data"));
    assert!(request_text(&requests[0]).contains("`sandbox_mode` is `read-only`"));
    assert!(request_text(&requests[1]).contains("`sandbox_mode` is `read-only`"));
    assert!(request_text(&requests[4]).contains("`sandbox_mode` is `workspace-write`"));
    assert!(requests.iter().all(has_strict_output_schema));
    assert!(max_input_text_bytes(&requests[1]) <= 8 * 1024);
    assert!(max_input_text_bytes(&requests[4]) <= 8 * 1024);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_is_skipped_when_discovery_has_no_findings() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(assistant_sse(&empty_discovery_output())),
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::FixAndCommit,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert!(output.findings.is_empty());
    assert_eq!(output.resolution, None);
    assert_eq!(mock.requests().len(), 1);
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
                action: ReviewAction::Fix,
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
async fn multiple_reports_share_one_ordered_parent_handoff() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_with_title("First finding"))),
            responses::sse(assistant_sse(&discovery_with_title("Second finding"))),
            responses::sse(assistant_sse_with_ids(
                "parent-message-1",
                "parent-response-1",
                "parent reply",
            )),
            responses::sse(assistant_sse_with_ids(
                "parent-message-2",
                "parent-response-2",
                "second parent reply",
            )),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build_with_auto_env(&server).await?;

    for instructions in ["first review", "second review"] {
        test.codex
            .submit(Op::Review {
                review_request: ReviewRequest {
                    target: ReviewTarget::Custom {
                        instructions: instructions.to_string(),
                    },
                    verification: ReviewVerification::SinglePass,
                    action: ReviewAction::Report,
                    user_facing_hint: None,
                },
            })
            .await?;
        let _ = completed_review(&test.codex).await;
    }
    wait_for_status(&test.codex, |status| {
        matches!(status, codex_protocol::protocol::AgentStatus::Completed(_))
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    test.submit_turn("continue").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let parent_input = requests[2].input();
    let handoffs = parent_input
        .iter()
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
        .filter(|text| text.starts_with("<review_handoff>"))
        .collect::<Vec<_>>();
    assert_eq!(handoffs.len(), 1);
    let handoff = handoffs[0];
    assert!(
        handoff.find("First finding").expect("first")
            < handoff.find("Second finding").expect("second")
    );

    wait_for_status(&test.codex, |status| {
        matches!(status, codex_protocol::protocol::AgentStatus::Completed(_))
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    test.submit_turn("continue again").await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        request_text(&requests[3])
            .matches("<review_handoff>")
            .count(),
        1
    );
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
                action: ReviewAction::Fix,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrepairable_fix_keeps_report_and_returns_failed_resolution() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse("bad fix output one")),
            responses::sse(assistant_sse("bad fix output two")),
            responses::sse(assistant_sse("bad fix output three")),
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
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::FixAndCommit,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.findings.len(), 1);
    let resolution = output.resolution.expect("failed resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Failed
    );
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(mock.requests().len(), 4);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repaired_fix_keeps_execution_evidence_and_creates_one_coordinator_commit() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(fix_test_command_sse()),
            responses::sse(assistant_sse("invalid fix output")),
            responses::sse(assistant_sse(&fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    let before = git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::FixAndCommit,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Complete,
        "{resolution:?}; file={}",
        std::fs::read_to_string(test.workspace_path("candidate.rs"))?
    );
    let commit_sha = resolution.commit_sha.expect("coordinator commit");
    assert_eq!(resolution.tests[0].command, FIX_TEST_COMMAND);
    assert_eq!(
        resolution.tests[0].status,
        codex_protocol::protocol::ReviewTestStatus::Passed
    );
    assert_ne!(commit_sha, before);
    assert_eq!(
        git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?,
        commit_sha
    );
    assert_eq!(
        git_stdout(test.cwd_path(), &["log", "-1", "--pretty=%s"])?,
        "fix: address review findings"
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        request_instructions(&requests[4]).as_deref(),
        Some(codex_core::REVIEW_REPAIR_PROMPT)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hallucinated_passing_test_cannot_complete_or_commit() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _unverified = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(assistant_sse(&fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    let before = git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::FixAndCommit,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Partial
    );
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?, before);
    assert!(std::fs::read_to_string(test.workspace_path("candidate.rs"))?.contains("_unverified"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verification_started_before_a_fix_cannot_complete_or_commit() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let overlapping_test_arguments = serde_json::json!({
        "cmd": OVERLAPPING_FIX_TEST_COMMAND,
        "yield_time_ms": 250
    })
    .to_string();
    let wait_test_arguments = serde_json::json!({"cmd": POST_FIX_WAIT_COMMAND}).to_string();
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("overlapping-test-response"),
                responses::ev_function_call(
                    "overlapping-test-call",
                    "exec_command",
                    &overlapping_test_arguments,
                ),
                responses::ev_completed("overlapping-test-response"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _overlap = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("wait-test-response"),
                responses::ev_function_call(
                    "wait-test-call",
                    "exec_command",
                    &wait_test_arguments,
                ),
                responses::ev_completed("wait-test-response"),
            ]),
            responses::sse(assistant_sse(&fix_output_with_test(
                OVERLAPPING_FIX_TEST_COMMAND,
            ))),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_overlapping_verification_repo(&test)?;
    let before = git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::FixAndCommit,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Partial
    );
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?, before);
    assert!(std::fs::read_to_string(test.workspace_path("candidate.rs"))?.contains("_overlap"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_fix_keeps_changes_but_creates_no_commit() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _partial = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(assistant_sse(&partial_fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    let before = git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::FixAndCommit,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Partial
    );
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?, before);
    assert!(std::fs::read_to_string(test.workspace_path("candidate.rs"))?.contains("_partial"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_after_a_fix_edit_keeps_an_unresolved_report() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _interrupted = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("sleep-response"),
                responses::ev_shell_command_call("sleep-call", "sleep 30"),
                responses::ev_completed("sleep-response"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::WholeRepository,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::ExecCommandBegin(event) if event.call_id == "sleep-call"),
    )
    .await;
    test.codex.submit(Op::Interrupt).await?;

    let event = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ExitedReviewMode(_))
    })
    .await;
    let EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
        review_output: Some(output),
        ..
    }) = event
    else {
        panic!("expected interrupted review output");
    };
    let resolution = output.resolution.expect("unresolved resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Failed
    );
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert!(std::fs::read_to_string(test.workspace_path("candidate.rs"))?.contains("_interrupted"));
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

async fn wait_for_status(
    codex: &codex_core::CodexThread,
    predicate: impl Fn(&codex_protocol::protocol::AgentStatus) -> bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = codex.agent_status().await;
            if predicate(&status) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for agent status");
}

fn assistant_sse(text: &str) -> Vec<serde_json::Value> {
    assistant_sse_with_ids("message", "response", text)
}

fn fix_test_command_sse() -> Vec<serde_json::Value> {
    let arguments = serde_json::json!({"cmd": FIX_TEST_COMMAND}).to_string();
    vec![
        responses::ev_response_created("fix-test-response"),
        responses::ev_function_call("fix-test-call", "exec_command", &arguments),
        responses::ev_completed("fix-test-response"),
    ]
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

fn discovery_with_title(title: &str) -> String {
    serde_json::json!({
        "candidates": [{
            "title": title,
            "body": "The issue is concrete.",
            "confidenceScore": 0.9,
            "priority": 1,
            "codeLocation": {
                "absoluteFilePath": "candidate.rs",
                "lineRange": {"start": 1, "end": 1}
            }
        }],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "One issue remains.",
            "confidenceScore": 0.9
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
            "title": "Handle the failed send",
            "body": "The failed send is ignored.",
            "confidenceScore": 0.95,
            "priority": 1,
            "codeLocation": {
                "absoluteFilePath": "candidate.rs",
                "lineRange": {"start": 8, "end": 8}
            },
            "preExisting": "undetermined",
            "preExistingFixRationale": null
        }],
        "outOfScopeFindings": [],
        "unverifiedFindings": [],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "The issue is valid.",
            "confidenceScore": 0.95
        }
    })
    .to_string()
}

fn fix_output() -> String {
    fix_output_with_test(FIX_TEST_COMMAND)
}

fn fix_output_with_test(test_command: &str) -> String {
    serde_json::json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "undetermined",
            "preExistingFixRationale": null,
            "disposition": "fixed"
        }],
        "resolution": {
            "status": "complete",
            "fixedCount": 1,
            "rejectedCount": 0,
            "unresolvedCount": 0,
            "summary": ["Handled the failed send."],
            "tests": [{"command": test_command, "status": "passed"}],
            "commitSha": null
        }
    })
    .to_string()
}

fn pre_existing_verification_output() -> String {
    serde_json::json!({
        "findings": [{
            "candidateIndex": 0,
            "title": "Handle the failed send",
            "body": "The failed send is ignored.",
            "confidenceScore": 0.95,
            "priority": 1,
            "codeLocation": {
                "absoluteFilePath": "candidate.rs",
                "lineRange": {"start": 8, "end": 8}
            },
            "preExisting": "true",
            "preExistingFixRationale": "The nearby change could safely address it."
        }],
        "outOfScopeFindings": [],
        "unverifiedFindings": [],
        "assessment": {
            "verdict": "patch is correct",
            "explanation": "Only a pre-existing issue remains.",
            "confidenceScore": 0.95
        }
    })
    .to_string()
}

fn partial_fix_output() -> String {
    serde_json::json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "undetermined",
            "preExistingFixRationale": null,
            "disposition": "unresolved"
        }],
        "resolution": {
            "status": "partial",
            "fixedCount": 0,
            "rejectedCount": 0,
            "unresolvedCount": 1,
            "summary": ["Changed one path; another condition remains."],
            "tests": [{"command": "just test -p example", "status": "passed"}],
            "commitSha": null
        }
    })
    .to_string()
}

fn request_model(request: &core_test_support::responses::ResponsesRequest) -> Option<String> {
    request.body_json()["model"].as_str().map(str::to_string)
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

fn has_strict_output_schema(request: &core_test_support::responses::ResponsesRequest) -> bool {
    request.body_json()["text"]["format"]["strict"].as_bool() == Some(true)
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

fn git_stdout(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
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
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
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

fn initialize_candidate_repo(test: &core_test_support::test_codex::TestCodex) -> Result<()> {
    run_git(test.cwd_path(), &["init", "--initial-branch=main"])?;
    run_git(
        test.cwd_path(),
        &["config", "user.email", "test@example.com"],
    )?;
    run_git(test.cwd_path(), &["config", "user.name", "Test User"])?;
    run_git(test.cwd_path(), &["config", "commit.gpgsign", "false"])?;
    std::fs::write(test.workspace_path("candidate.rs"), "fn candidate() {}\n")?;
    run_git(test.cwd_path(), &["add", "candidate.rs"])?;
    run_git(test.cwd_path(), &["commit", "-m", "base"])?;
    Ok(())
}

fn initialize_overlapping_verification_repo(
    test: &core_test_support::test_codex::TestCodex,
) -> Result<()> {
    initialize_candidate_repo(test)?;
    std::fs::create_dir(test.workspace_path("src"))?;
    std::fs::write(
        test.workspace_path("Cargo.toml"),
        "[package]\nname = \"review-evidence-test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(
        test.workspace_path("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"review-evidence-test\"\nversion = \"0.1.0\"\n",
    )?;
    std::fs::write(test.workspace_path(".gitignore"), "/target\n")?;
    std::fs::write(
        test.workspace_path("src/lib.rs"),
        r#"#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn overlap() {
        for _ in 0..500 {
            let candidate = std::fs::read_to_string("candidate.rs").expect("candidate");
            if candidate.contains("_overlap") {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("fix did not run");
    }

    #[test]
    fn wait() {
        std::thread::sleep(Duration::from_secs(1));
    }
}
"#,
    )?;
    run_git(
        test.cwd_path(),
        &[
            "add",
            "Cargo.toml",
            "Cargo.lock",
            ".gitignore",
            "src/lib.rs",
        ],
    )?;
    run_git(test.cwd_path(), &["commit", "-m", "add test fixture"])?;
    Ok(())
}
