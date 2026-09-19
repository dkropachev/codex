use anyhow::Result;
use codex_core::REVIEW_DOUBLE_CHECK_PROMPT;
use codex_core::REVIEW_FIX_SCOPE_PROMPT;
use codex_core::REVIEW_PROMPT;
use codex_core::config::CurrentTimeReminderConfig;
use codex_core::config::PermissionProfileSnapshot;
use codex_core::review_fix_prompt;
use codex_exec_server::CreateDirectoryOptions;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::items::TurnItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewVerification;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_with_timeout;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

const FIX_TEST_COMMAND: &str = "git diff --check";
const CARGO_FIX_TEST_COMMAND: &str = "cargo test --quiet";
#[cfg(unix)]
const HARD_LINK_ESCAPE_COMMAND: &str =
    "ln candidate.rs \"$TMPDIR/linked.rs\" && printf 'hacked\\n' > \"$TMPDIR/linked.rs\"";
#[cfg(windows)]
const HARD_LINK_ESCAPE_COMMAND: &str = concat!(
    "New-Item -ItemType HardLink -Path \"$env:TEMP\\linked.rs\" ",
    "-Target \"candidate.rs\"; Set-Content -NoNewline ",
    "-Path \"$env:TEMP\\linked.rs\" -Value \"hacked\""
);

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
#[cfg(unix)]
async fn rerouted_fix_cannot_write_outside_the_checkout_or_git_metadata() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let outside = tempfile::Builder::new()
        .prefix("codex-review-outside-")
        .tempdir_in("/var/tmp")?;
    let outside_path = outside.path().join("outside.rs");
    std::fs::write(&outside_path, "outside\n")?;
    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    let head_path = test.workspace_path(".git/HEAD");
    let head = std::fs::read_to_string(&head_path)?;
    let outside_patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-outside\n+escaped\n*** End Patch\n",
        outside_path.display()
    );
    let head_patch = format!(
        "*** Begin Patch\n*** Update File: .git/HEAD\n@@\n-{}\n+corrupt\n*** End Patch\n",
        head.trim_end()
    );
    let first_fix_response = sse_response(responses::sse(vec![
        responses::ev_response_created("outside-response"),
        responses::ev_apply_patch_custom_tool_call("outside-call", &outside_patch),
        responses::ev_completed("outside-response"),
    ]))
    .insert_header("OpenAI-Model", "gpt-5.2");
    let mock = mount_response_sequence(
        &server,
        vec![
            sse_response(responses::sse(assistant_sse(&discovery_output()))),
            sse_response(responses::sse(baseline_free_fix_scope_sse("valid"))),
            first_fix_response,
            sse_response(responses::sse(vec![
                responses::ev_response_created("git-response"),
                responses::ev_apply_patch_custom_tool_call("git-call", &head_patch),
                responses::ev_completed("git-response"),
            ])),
            sse_response(responses::sse(assistant_sse(&rejected_fix_output()))),
        ],
    )
    .await;

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
    let mut output = None;
    let mut saw_model_reroute = false;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::ModelReroute(_) => saw_model_reroute = true,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output, .. }) => {
                output = review_output
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    let output = output.expect("review output");

    assert_eq!(std::fs::read_to_string(outside_path)?, "outside\n");
    assert_eq!(std::fs::read_to_string(head_path)?, head);
    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.rejected_count, 1);
    let requests = mock.requests();
    assert_eq!(requests.len(), 5);
    assert!(saw_model_reroute);
    assert!(
        !requests[3]
            .custom_tool_call_output("outside-call")
            .to_string()
            .contains("Success. Updated")
    );
    assert!(
        !requests[4]
            .custom_tool_call_output("git-call")
            .to_string()
            .contains("Success. Updated")
    );
    Ok(())
}

#[cfg(any(unix, windows))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_rejects_an_in_checkout_hard_link_to_an_outside_file() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    let candidate_path = test.workspace_path("candidate.rs");
    let checkout = candidate_path.parent().expect("checkout root");
    let outside = tempfile::Builder::new()
        .prefix("codex-review-hard-link-")
        .tempdir_in(checkout.parent().expect("checkout parent"))?;
    let outside_path = outside.path().join("outside.rs");
    std::fs::write(&outside_path, "fn candidate() {}\n")?;
    std::fs::remove_file(&candidate_path)?;
    std::fs::hard_link(&outside_path, &candidate_path)?;
    let unresolved = serde_json::json!({
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
            "summary": ["The linked file was not changed."],
            "tests": [],
            "commitSha": null
        }
    })
    .to_string();
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(vec![
                responses::ev_response_created("hard-link-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "hard-link-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _changed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("hard-link-response"),
            ]),
            responses::sse(assistant_sse(&unresolved)),
        ],
    )
    .await;

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
    let output = completed_review(&test.codex).await;

    assert_eq!(
        std::fs::read_to_string(&outside_path)?,
        "fn candidate() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(&candidate_path)?,
        "fn candidate() {}\n"
    );
    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 0);
    assert!(resolution.unresolved_count > 0);
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[3]
            .custom_tool_call_output("hard-link-call")
            .to_string()
            .contains("multiple hard links")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steered_user_input_receives_a_pending_review_handoff() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let sleep_arguments = serde_json::json!({"duration_ms": 3_600_000}).to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("sleep-response"),
                responses::ev_function_call_with_namespace(
                    "sleep-call",
                    "clock",
                    "sleep",
                    &sleep_arguments,
                ),
                responses::ev_completed("sleep-response"),
            ]),
            responses::sse(assistant_sse("continued")),
        ],
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::CurrentTimeReminder)
            .expect("enable time reminder");
        config.current_time_reminder = Some(CurrentTimeReminderConfig {
            sleep_tool: true,
            ..CurrentTimeReminderConfig::default()
        });
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
    wait_for_status(&test.codex, |status| {
        matches!(status, codex_protocol::protocol::AgentStatus::Completed(_))
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    test.codex
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::try_from("/root/worker").expect("worker path"),
                AgentPath::root(),
                Vec::new(),
                "background work".to_string(),
                /*trigger_turn*/ true,
            ),
        })
        .await?;
    wait_for_event_with_timeout(
        &test.codex,
        |event| {
            matches!(
                event,
                EventMsg::ItemStarted(item)
                    if matches!(&item.item, TurnItem::Sleep(sleep) if sleep.id == "sleep-call")
            )
        },
        std::time::Duration::from_secs(30),
    )
    .await;
    test.codex
        .steer_input(
            vec![UserInput::Text {
                text: "use the review result".to_string(),
                text_elements: Vec::new(),
            }],
            Default::default(),
            /*expected_turn_id*/ None,
            /*client_user_message_id*/ None,
            /*responsesapi_client_metadata*/ None,
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to steer review handoff test: {error:?}"))?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let follow_up = request_text(&requests[2]);
    assert!(follow_up.contains("<review_handoff>"), "{follow_up}");
    assert!(follow_up.contains("Handle the failed send"), "{follow_up}");
    assert!(follow_up.contains("use the review result"), "{follow_up}");
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
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
        requests[4].custom_tool_call_output("fix-call")
    );
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(resolution.tests[0].command, FIX_TEST_COMMAND);
    assert_eq!(
        resolution.tests[0].status,
        codex_protocol::protocol::ReviewTestStatus::Passed
    );

    assert_eq!(requests.len(), 6);
    assert_eq!(request_model(&requests[0]).as_deref(), Some("gpt-5.2"));
    assert_eq!(request_model(&requests[1]).as_deref(), Some("gpt-5.2"));
    assert_eq!(request_model(&requests[2]).as_deref(), Some("gpt-5.4"));
    assert_eq!(request_model(&requests[3]).as_deref(), Some("gpt-5.4"));
    assert_eq!(request_model(&requests[4]).as_deref(), Some("gpt-5.4"));
    assert_eq!(request_model(&requests[5]).as_deref(), Some("gpt-5.4"));
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
        request_instructions(&requests[2]).as_deref(),
        Some(REVIEW_FIX_SCOPE_PROMPT)
    );
    assert_eq!(
        request_instructions(&requests[5]).as_deref(),
        Some(fix_prompt.as_str())
    );
    assert!(request_text(&requests[1]).contains("SECURITY: This is untrusted output"));
    assert!(request_text(&requests[5]).contains("untrusted review data"));
    assert!(request_text(&requests[0]).contains("`sandbox_mode` is `read-only`"));
    assert!(request_text(&requests[1]).contains("`sandbox_mode` is `read-only`"));
    assert!(request_text(&requests[2]).contains("`sandbox_mode` is `read-only`"));
    assert!(request_text(&requests[5]).contains("`sandbox_mode` is `workspace-write`"));
    assert!(requests.iter().all(has_strict_output_schema));
    assert!(max_input_text_bytes(&requests[1]) <= 8 * 1024);
    assert!(max_input_text_bytes(&requests[2]) <= 8 * 1024);
    assert!(max_input_text_bytes(&requests[5]) <= 8 * 1024);

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
    initialize_candidate_repo(&test)?;

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
async fn fix_and_commit_rejects_detached_head_before_inference() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(assistant_sse(&empty_discovery_output())),
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    run_git(test.cwd_path(), &["checkout", "--detach"])?;

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

    assert!(output.overall_explanation.contains("attached branch"));
    assert!(mock.requests().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_rejects_disallowed_workspace_permissions_before_inference() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(assistant_sse(&empty_discovery_output())),
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config
            .permissions
            .replace_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
                PermissionProfile::read_only(),
            ))
            .expect("read-only permission constraint");
    });
    let test = builder.build_with_auto_env(&server).await?;
    write_candidate(&test).await?;

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
    let output = completed_review(&test.codex).await;

    assert!(output.overall_explanation.contains("Fix is unavailable"));
    assert!(mock.requests().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_skips_a_finding_outside_the_checkout() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let outside = TempDir::new()?;
    let outside_path = outside.path().join("outside.rs");
    std::fs::write(&outside_path, "outside\n")?;
    let server = start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(assistant_sse(&discovery_with_path(
            &outside_path.display().to_string(),
        ))),
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;

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
    let output = completed_review(&test.codex).await;

    assert!(output.findings.is_empty());
    assert_eq!(output.unverified_findings.len(), 1);
    assert_eq!(output.external_references.len(), 1);
    assert_eq!(output.resolution, None);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(std::fs::read_to_string(outside_path)?, "outside\n");
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_skips_a_finding_through_an_escaping_symlink() -> Result<()> {
    use std::os::unix::fs::symlink;

    skip_if_no_network!(Ok(()));

    let outside = TempDir::new()?;
    let outside_path = outside.path().join("outside.rs");
    std::fs::write(&outside_path, "outside\n")?;
    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let link = test.workspace_path("escape");
    symlink(outside.path(), &link)?;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(assistant_sse(&discovery_with_path(
            &link.join("outside.rs").display().to_string(),
        ))),
    )
    .await;

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
    let output = completed_review(&test.codex).await;

    assert!(output.findings.is_empty());
    assert_eq!(output.unverified_findings.len(), 1);
    assert_eq!(output.external_references.len(), 1);
    assert_eq!(output.resolution, None);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(std::fs::read_to_string(outside_path)?, "outside\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_fix_runs_outside_a_git_repository() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let rejected = serde_json::json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "undetermined",
            "preExistingFixRationale": null,
            "disposition": "rejected"
        }],
        "resolution": {
            "status": "complete",
            "fixedCount": 0,
            "rejectedCount": 1,
            "unresolvedCount": 0,
            "summary": ["The candidate is not valid."],
            "tests": [],
            "commitSha": null
        }
    })
    .to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(assistant_sse(&rejected)),
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
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.rejected_count, 1);
    assert_eq!(resolution.unresolved_count, 0);
    assert_eq!(mock.requests().len(), 3);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_fix_rejects_apply_then_revert_outside_git() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const VERIFY_COMMAND: &str = "test -f candidate.rs";
    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(vec![
                responses::ev_response_created("apply-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "apply-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("apply-response"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("revert-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "revert-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() { let _fixed = true; }\n+fn candidate() {}\n*** End Patch\n",
                ),
                responses::ev_completed("revert-response"),
            ]),
            responses::sse(fix_test_command_sse_with(VERIFY_COMMAND)),
            responses::sse(assistant_sse(&fix_output_with_test(VERIFY_COMMAND))),
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
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    let candidate = test
        .fs()
        .read_file(
            &test
                .executor_environment()
                .selection()
                .cwd
                .join("candidate.rs")?,
            /*sandbox*/ None,
        )
        .await?;
    assert!(!String::from_utf8(candidate)?.contains("_fixed"));
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
                action: ReviewAction::Fix,
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
async fn invalid_fix_output_retains_the_issue_report() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_fix_scope_sse("false", "valid")),
            responses::sse(assistant_sse("bad fix one")),
            responses::sse(assistant_sse("bad fix two")),
            responses::sse(assistant_sse("bad fix three")),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
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
    assert_eq!(mock.requests().len(), 5);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_fix_scope_is_repaired_with_the_coding_model() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse("invalid fix scope")),
            responses::sse(baseline_free_fix_scope_sse("rejected")),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config.review_model = Some("gpt-5.2".to_string());
    });
    let test = builder.build_with_auto_env(&server).await?;
    write_candidate(&test).await?;

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
    let output = completed_review(&test.codex).await;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.rejected_count, 1);
    assert_eq!(resolution.fixed_count, 0);
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(request_model(&requests[0]).as_deref(), Some("gpt-5.2"));
    assert_eq!(request_model(&requests[1]).as_deref(), Some("gpt-5.4"));
    assert_eq!(request_model(&requests[2]).as_deref(), Some("gpt-5.4"));
    assert_eq!(
        request_instructions(&requests[1]).as_deref(),
        Some(REVIEW_FIX_SCOPE_PROMPT)
    );
    assert_eq!(
        request_instructions(&requests[2]).as_deref(),
        Some(codex_core::REVIEW_REPAIR_PROMPT)
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
async fn review_handoff_preserves_leading_context_before_the_user_message() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(assistant_sse("parent reply")),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
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
    wait_for_status(&test.codex, |status| {
        matches!(status, codex_protocol::protocol::AgentStatus::Completed(_))
    })
    .await;
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "use the review".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: std::collections::BTreeMap::from([(
                "browser_info".to_string(),
                AdditionalContextEntry {
                    value: "leading context".to_string(),
                    kind: AdditionalContextKind::Untrusted,
                },
            )]),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let input = request_text(&requests[1]);
    let context = input
        .find("<external_browser_info>leading context")
        .expect("context");
    let handoff = input.find("<review_handoff>").expect("handoff");
    let user = input.find("use the review").expect("user");
    assert!(context < handoff && handoff < user, "{input}");
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
async fn single_pass_fix_classifies_pre_existing_before_granting_write_access() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_fix_scope_sse("true", "valid")),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(
        std::fs::read_to_string(test.workspace_path("candidate.rs"))?,
        "fn candidate() {}\n"
    );
    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::True);
    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 0);
    assert_eq!(resolution.commit_sha, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_scope_passes_only_eligible_findings_to_the_mutation_child() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&two_candidate_discovery_output())),
            responses::sse(mixed_fix_scope_sse()),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(fix_test_command_sse()),
            responses::sse(assistant_sse(&fixed_false_fix_output_with_test(
                FIX_TEST_COMMAND,
            ))),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::write(test.workspace_path("preexisting.rs"), "fn old_bug() {}\n")?;
    run_git(test.cwd_path(), &["add", "preexisting.rs"])?;
    run_git(
        test.cwd_path(),
        &["commit", "-m", "add pre-existing fixture"],
    )?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert!(std::fs::read_to_string(test.workspace_path("candidate.rs"))?.contains("_fixed"));
    assert_eq!(
        std::fs::read_to_string(test.workspace_path("preexisting.rs"))?,
        "fn old_bug() {}\n"
    );
    assert_eq!(output.findings[1].pre_existing, ReviewPreExisting::True);
    let mutation_input = request_text(&mock.requests()[2]);
    assert!(mutation_input.contains("Handle the failed send"));
    assert!(!mutation_input.contains("Fix the old bug"));
    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 1);
    assert_eq!(resolution.unresolved_count, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_rejects_shell_mutation_before_report_only_classification() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let mutate_arguments = serde_json::json!({
        "cmd": "sed -i 's/candidate/hacked/' candidate.rs"
    })
    .to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_fix_scope_sse("false", "valid")),
            responses::sse(vec![
                responses::ev_response_created("mutate-response"),
                responses::ev_function_call("mutate-call", "exec_command", &mutate_arguments),
                responses::ev_completed("mutate-response"),
            ]),
            responses::sse(assistant_sse(&unresolved_false_fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(
        std::fs::read_to_string(test.workspace_path("candidate.rs"))?,
        "fn candidate() {}\n"
    );
    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::False);
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        !requests[3]
            .function_call_output("mutate-call")
            .to_string()
            .contains("Exit code: 0")
    );
    Ok(())
}

#[cfg(any(unix, windows))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_verification_cannot_write_source_through_a_build_root_hard_link() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let arguments = serde_json::json!({"cmd": HARD_LINK_ESCAPE_COMMAND}).to_string();
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(vec![
                responses::ev_response_created("hard-link-command-response"),
                responses::ev_function_call("hard-link-command-call", "exec_command", &arguments),
                responses::ev_completed("hard-link-command-response"),
            ]),
            responses::sse(assistant_sse(&partial_fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    write_candidate(&test).await?;

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
    completed_review(&test.codex).await;

    assert_eq!(
        std::fs::read_to_string(test.workspace_path("candidate.rs"))?,
        "\n\n\n\n\n\n\nfn candidate() {}\n"
    );
    let command_output = mock.requests()[3]
        .function_call_output("hard-link-command-call")
        .to_string();
    assert!(
        !command_output.contains("exited with code 0"),
        "{command_output}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_fix_classifies_before_writing_and_then_verifies() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_fix_scope_sse("false", "valid")),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(fix_test_command_sse_with(CARGO_FIX_TEST_COMMAND)),
            responses::sse(assistant_sse(&fixed_false_fix_output_with_test(
                CARGO_FIX_TEST_COMMAND,
            ))),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::create_dir(test.workspace_path("src"))?;
    std::fs::write(
        test.workspace_path("Cargo.toml"),
        "[package]\nname = \"review-fix-test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(
        test.workspace_path("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"review-fix-test\"\nversion = \"0.1.0\"\n",
    )?;
    std::fs::write(
        test.workspace_path("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )?;
    run_git(test.cwd_path(), &["add", "."])?;
    run_git(test.cwd_path(), &["commit", "-m", "add test crate"])?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    let output = completed_review(&test.codex).await;

    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::False);
    assert!(std::fs::read_to_string(test.workspace_path("candidate.rs"))?.contains("_fixed"));
    assert!(!test.workspace_path("target").exists());
    let resolution = output.resolution.expect("resolution");
    assert_eq!(
        resolution.fixed_count,
        1,
        "{resolution:?}; requests={:?}",
        mock.requests()
            .iter()
            .map(core_test_support::responses::ResponsesRequest::body_json)
            .collect::<Vec<_>>()
    );
    assert_eq!(mock.requests().len(), 5);
    assert_eq!(
        request_instructions(&mock.requests()[1]).as_deref(),
        Some(REVIEW_FIX_SCOPE_PROMPT)
    );
    assert!(request_text(&mock.requests()[1]).contains("`sandbox_mode` is `read-only`"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_fix_runs_in_the_selected_auto_environment() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(vec![
                responses::ev_response_created("fix-tool-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "fix-call",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("fix-tool-response"),
            ]),
            responses::sse(fix_test_command_sse_with(CARGO_FIX_TEST_COMMAND)),
            responses::sse(assistant_sse(&fix_output_with_test(
                CARGO_FIX_TEST_COMMAND,
            ))),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build_with_auto_env(&server).await?;
    write_candidate(&test).await?;
    let cwd = &test.executor_environment().selection().cwd;
    let source_dir = cwd.join("src")?;
    test.fs()
        .create_directory(
            &source_dir,
            CreateDirectoryOptions { recursive: false },
            /*sandbox*/ None,
        )
        .await?;
    for (path, contents) in [
        (
            "Cargo.toml",
            "[package]\nname = \"review-fix-test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        ),
        (
            "Cargo.lock",
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"review-fix-test\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub fn value() -> u8 { 1 }\n"),
    ] {
        test.fs()
            .write_file(
                &cwd.join(path)?,
                contents.as_bytes().to_vec(),
                /*sandbox*/ None,
            )
            .await?;
    }

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
    let output = completed_review(&test.codex).await;

    let candidate = test
        .fs()
        .read_file(&cwd.join("candidate.rs")?, /*sandbox*/ None)
        .await?;
    assert!(String::from_utf8(candidate)?.contains("_fixed"));
    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 1, "{resolution:?}");
    assert_eq!(resolution.tests[0].command, CARGO_FIX_TEST_COMMAND);
    assert_eq!(mock.requests().len(), 5);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_fix_cannot_patch_before_classification() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(vec![
                responses::ev_response_created("early-patch-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "early-patch",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _early = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("early-patch-response"),
            ]),
            responses::sse(baseline_fix_scope_sse("false", "valid")),
            responses::sse(assistant_sse(&unresolved_false_fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::write(test.workspace_path("change.txt"), "change\n")?;

    test.codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::UncommittedChanges,
                verification: ReviewVerification::SinglePass,
                action: ReviewAction::Fix,
                user_facing_hint: None,
            },
        })
        .await?;
    completed_review(&test.codex).await;

    assert_eq!(
        std::fs::read_to_string(test.workspace_path("candidate.rs"))?,
        "fn candidate() {}\n"
    );
    assert!(
        !mock.requests()[2]
            .custom_tool_call_output("early-patch")
            .to_string()
            .contains("Success. Updated")
    );
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(assistant_sse("bad fix output one")),
            responses::sse(assistant_sse("bad fix output two")),
            responses::sse(assistant_sse("bad fix output three")),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    initialize_candidate_repo(&test)?;

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
    assert_eq!(mock.requests().len(), 5);
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
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
    assert_eq!(requests.len(), 6);
    let fix_prompt = review_fix_prompt(ReviewAction::FixAndCommit);
    assert_eq!(
        request_instructions(&requests[4]).as_deref(),
        Some(fix_prompt.as_str())
    );
    assert_eq!(
        request_instructions(&requests[5]).as_deref(),
        Some(codex_core::REVIEW_REPAIR_PROMPT)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fix_and_commit_ignores_inherited_git_repository_routing() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let other = TempDir::new()?;
    run_git(other.path(), &["init", "--initial-branch=main"])?;
    run_git(other.path(), &["config", "user.email", "test@example.com"])?;
    run_git(other.path(), &["config", "user.name", "Test User"])?;
    std::fs::write(other.path().join("other.txt"), "other\n")?;
    run_git(other.path(), &["add", "other.txt"])?;
    run_git(other.path(), &["commit", "-m", "other base"])?;
    let other_head = git_stdout(other.path(), &["rev-parse", "HEAD"])?;
    let other_git_dir = other.path().join(".git").display().to_string();
    let other_work_tree = other.path().display().to_string();

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
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
    let mut builder = test_codex()
        .with_model("gpt-5.4")
        .with_config(move |config| {
            config
                .permissions
                .shell_environment_policy
                .r#set
                .insert("GIT_DIR".to_string(), other_git_dir);
            config
                .permissions
                .shell_environment_policy
                .r#set
                .insert("GIT_WORK_TREE".to_string(), other_work_tree);
        });
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
    let commit_sha = resolution.commit_sha.expect("coordinator commit");
    assert_ne!(commit_sha, before);
    assert_eq!(
        git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?,
        commit_sha
    );
    assert_eq!(
        git_stdout(other.path(), &["rev-parse", "HEAD"])?,
        other_head
    );
    assert_eq!(git_stdout(other.path(), &["status", "--short"])?, "");
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_file_change_prevents_a_later_verified_commit() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_sse(&discovery_output())),
            responses::sse(baseline_free_fix_scope_sse("valid")),
            responses::sse(vec![
                responses::ev_response_created("failed-patch-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "failed-patch",
                    "*** Begin Patch\n*** Update File: locked/src.txt\n*** Move to: out/dst.txt\n@@\n-line\n+line2\n*** End Patch\n",
                ),
                responses::ev_completed("failed-patch-response"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("good-patch-response"),
                responses::ev_apply_patch_custom_tool_call(
                    "good-patch",
                    "*** Begin Patch\n*** Update File: candidate.rs\n@@\n-fn candidate() {}\n+fn candidate() { let _fixed = true; }\n*** End Patch\n",
                ),
                responses::ev_completed("good-patch-response"),
            ]),
            responses::sse(fix_test_command_sse()),
            responses::sse(assistant_sse(&fix_output())),
        ],
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    initialize_candidate_repo(&test)?;
    std::fs::create_dir(test.workspace_path("locked"))?;
    std::fs::create_dir(test.workspace_path("out"))?;
    std::fs::write(test.workspace_path("locked/src.txt"), "line\n")?;
    run_git(test.cwd_path(), &["add", "."])?;
    run_git(test.cwd_path(), &["commit", "-m", "add move fixture"])?;
    std::fs::set_permissions(
        test.workspace_path("locked"),
        std::fs::Permissions::from_mode(0o555),
    )?;
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
    std::fs::set_permissions(
        test.workspace_path("locked"),
        std::fs::Permissions::from_mode(0o755),
    )?;

    let resolution = output.resolution.expect("resolution");
    assert_eq!(
        resolution.status,
        codex_protocol::protocol::ReviewResolutionStatus::Partial
    );
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.commit_sha, None);
    assert_eq!(git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?, before);
    assert!(
        resolution
            .summary
            .iter()
            .any(|summary| summary.contains("file change failed"))
    );
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
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
            responses::sse(baseline_free_fix_scope_sse("valid")),
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
    fix_test_command_sse_with(FIX_TEST_COMMAND)
}

fn fix_test_command_sse_with(command: &str) -> Vec<serde_json::Value> {
    let arguments = serde_json::json!({"cmd": command}).to_string();
    vec![
        responses::ev_response_created("fix-test-response"),
        responses::ev_function_call("fix-test-call", "exec_command", &arguments),
        responses::ev_completed("fix-test-response"),
    ]
}

fn baseline_fix_scope_sse(pre_existing: &str, validity: &str) -> Vec<serde_json::Value> {
    assistant_sse(
        &serde_json::json!({
            "hasComparisonBaseline": true,
            "classifications": [{
                "findingIndex": 0,
                "preExisting": pre_existing,
                "preExistingFixRationale": if pre_existing == "true" {
                    Some("The issue predates the selected change.")
                } else {
                    None
                },
                "validity": validity
            }]
        })
        .to_string(),
    )
}

fn baseline_free_fix_scope_sse(validity: &str) -> Vec<serde_json::Value> {
    assistant_sse(
        &serde_json::json!({
            "hasComparisonBaseline": false,
            "classifications": [{
                "findingIndex": 0,
                "preExisting": "undetermined",
                "preExistingFixRationale": null,
                "validity": validity
            }]
        })
        .to_string(),
    )
}

fn mixed_fix_scope_sse() -> Vec<serde_json::Value> {
    assistant_sse(
        &serde_json::json!({
            "hasComparisonBaseline": true,
            "classifications": [
                {
                    "findingIndex": 0,
                    "preExisting": "false",
                    "preExistingFixRationale": null,
                    "validity": "valid"
                },
                {
                    "findingIndex": 1,
                    "preExisting": "true",
                    "preExistingFixRationale": "The issue predates the selected change.",
                    "validity": "valid"
                }
            ]
        })
        .to_string(),
    )
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

fn two_candidate_discovery_output() -> String {
    serde_json::json!({
        "candidates": [
            {
                "title": "Handle the failed send",
                "body": "The failed send is ignored.",
                "confidenceScore": 0.95,
                "priority": 1,
                "codeLocation": {
                    "absoluteFilePath": "candidate.rs",
                    "lineRange": {"start": 1, "end": 1}
                }
            },
            {
                "title": "Fix the old bug",
                "body": "The old bug predates this change.",
                "confidenceScore": 0.9,
                "priority": 1,
                "codeLocation": {
                    "absoluteFilePath": "preexisting.rs",
                    "lineRange": {"start": 1, "end": 1}
                }
            }
        ],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "Two candidates need scope checks.",
            "confidenceScore": 0.9
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

fn discovery_with_path(path: &str) -> String {
    serde_json::json!({
        "candidates": [{
            "title": "Handle the unsafe path",
            "body": "The location is outside the checkout.",
            "confidenceScore": 0.95,
            "priority": 1,
            "codeLocation": {
                "absoluteFilePath": path,
                "lineRange": {"start": 1, "end": 1}
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

fn fix_output() -> String {
    fix_output_with_test(FIX_TEST_COMMAND)
}

fn rejected_fix_output() -> String {
    serde_json::json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "undetermined",
            "preExistingFixRationale": null,
            "disposition": "rejected"
        }],
        "resolution": {
            "status": "complete",
            "fixedCount": 0,
            "rejectedCount": 1,
            "unresolvedCount": 0,
            "summary": ["No eligible change was made."],
            "tests": [],
            "commitSha": null
        }
    })
    .to_string()
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

fn fixed_false_fix_output_with_test(test_command: &str) -> String {
    serde_json::json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "false",
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

fn unresolved_false_fix_output() -> String {
    serde_json::json!({
        "classificationUpdates": [{
            "findingIndex": 0,
            "preExisting": "false",
            "preExistingFixRationale": null,
            "disposition": "unresolved"
        }],
        "resolution": {
            "status": "partial",
            "fixedCount": 0,
            "rejectedCount": 0,
            "unresolvedCount": 1,
            "summary": ["No safe source change was made."],
            "tests": [],
            "commitSha": null
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
