use super::*;

#[test]
fn evidence_requires_success_and_keeps_apply_patch_paths() {
    let mut collector = ReviewStageEvidenceCollector::default();
    collector.observe_response_item(&ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".to_string(),
        namespace: None,
        arguments: serde_json::json!({"cmd": "git diff --check"}).to_string(),
        call_id: "command-call".to_string(),
        internal_chat_message_metadata_passthrough: None,
    });
    observe_command_started(&mut collector, "command-call", "ignored-shell-wrapper");
    collector.observe_completed_item(&TurnItem::CommandExecution(
        codex_protocol::items::CommandExecutionItem {
            id: "command-call".to_string(),
            process_id: None,
            command: vec!["ignored-shell-wrapper".to_string()],
            cwd: codex_utils_path_uri::PathUri::parse("file:///workspace").expect("cwd"),
            parsed_cmd: Vec::new(),
            source: codex_protocol::protocol::ExecCommandSource::Agent,
            interaction_input: None,
            status: CommandExecutionStatus::Completed,
            stdout: Some(String::new()),
            stderr: Some(String::new()),
            aggregated_output: Some(String::new()),
            exit_code: Some(0),
            duration: None,
            formatted_output: Some(String::new()),
        },
    ));
    collector.observe_completed_item(&TurnItem::FileChange(
        codex_protocol::items::FileChangeItem {
            id: "patch-call".to_string(),
            changes: [(
                std::path::PathBuf::from("src/lib.rs"),
                codex_protocol::protocol::FileChange::Update {
                    unified_diff: "+fixed".to_string(),
                    move_path: Some(std::path::PathBuf::from("src/fixed.rs")),
                },
            )]
            .into_iter()
            .collect(),
            status: Some(PatchApplyStatus::Completed),
            auto_approved: Some(true),
            stdout: Some("Done!".to_string()),
            stderr: Some(String::new()),
        },
    ));

    assert!(
        !collector
            .evidence
            .observed_successful_command_after_last_mutation("git diff --check")
    );
    collector.observe_response_item(&ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".to_string(),
        namespace: None,
        arguments: serde_json::json!({"cmd": "git diff --check"}).to_string(),
        call_id: "command-call-after-patch".to_string(),
        internal_chat_message_metadata_passthrough: None,
    });
    observe_command_started(
        &mut collector,
        "command-call-after-patch",
        "ignored-shell-wrapper",
    );
    collector.observe_completed_item(&TurnItem::CommandExecution(
        codex_protocol::items::CommandExecutionItem {
            id: "command-call-after-patch".to_string(),
            process_id: None,
            command: vec!["ignored-shell-wrapper".to_string()],
            cwd: codex_utils_path_uri::PathUri::parse("file:///workspace").expect("cwd"),
            parsed_cmd: Vec::new(),
            source: codex_protocol::protocol::ExecCommandSource::Agent,
            interaction_input: None,
            status: CommandExecutionStatus::Completed,
            stdout: Some(String::new()),
            stderr: Some(String::new()),
            aggregated_output: Some(String::new()),
            exit_code: Some(0),
            duration: None,
            formatted_output: Some(String::new()),
        },
    ));
    assert!(
        collector
            .evidence
            .observed_successful_command_after_last_mutation("git diff --check")
    );
    assert_eq!(
        collector
            .evidence
            .resolved_file_changes(
                &codex_utils_path_uri::PathUri::parse("file:///workspace").expect("root")
            )
            .expect("resolve changes")
            .len(),
        1
    );
}

#[test]
fn evidence_rejects_failed_commands_and_patches() {
    let mut collector = ReviewStageEvidenceCollector::default();
    collector.observe_response_item(&ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".to_string(),
        namespace: None,
        arguments: serde_json::json!({"cmd": "sed -i s/old/new/ src/lib.rs"}).to_string(),
        call_id: "command-call".to_string(),
        internal_chat_message_metadata_passthrough: None,
    });
    observe_command_started(
        &mut collector,
        "command-call",
        "sed -i s/old/new/ src/lib.rs",
    );
    collector.observe_completed_item(&TurnItem::CommandExecution(
        codex_protocol::items::CommandExecutionItem {
            id: "command-call".to_string(),
            process_id: None,
            command: vec![
                "sed".to_string(),
                "-i".to_string(),
                "s/old/new/".to_string(),
                "src/lib.rs".to_string(),
            ],
            cwd: codex_utils_path_uri::PathUri::parse("file:///workspace").expect("cwd"),
            parsed_cmd: Vec::new(),
            source: codex_protocol::protocol::ExecCommandSource::Agent,
            interaction_input: None,
            status: CommandExecutionStatus::Failed,
            stdout: Some(String::new()),
            stderr: Some(String::new()),
            aggregated_output: Some(String::new()),
            exit_code: Some(1),
            duration: None,
            formatted_output: Some(String::new()),
        },
    ));
    collector.observe_completed_item(&TurnItem::FileChange(
        codex_protocol::items::FileChangeItem {
            id: "patch-call".to_string(),
            changes: [(
                std::path::PathBuf::from("src/lib.rs"),
                codex_protocol::protocol::FileChange::Add {
                    content: "not applied".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            status: Some(PatchApplyStatus::Failed),
            auto_approved: Some(true),
            stdout: Some(String::new()),
            stderr: Some("failed".to_string()),
        },
    ));

    assert!(
        !collector
            .evidence
            .observed_successful_command_after_last_mutation("sed -i s/old/new/ src/lib.rs")
    );
    assert!(collector.evidence.has_potentially_mutating_command());
    assert!(
        collector
            .evidence
            .resolved_file_changes(
                &codex_utils_path_uri::PathUri::parse("file:///workspace").expect("root")
            )
            .expect("resolve changes")
            .is_empty()
    );
}

#[test]
fn evidence_rejects_compound_commands_disguised_as_verification() {
    let mut collector = ReviewStageEvidenceCollector::default();
    observe_command(
        &mut collector,
        "compound",
        "cargo test -p example && sed -i s/old/new/ src/lib.rs",
        /*success*/ true,
    );

    assert!(collector.evidence.has_potentially_mutating_command());
}

#[test]
fn fix_permissions_write_only_the_checkout_and_explicit_verification_root() {
    let root = tempfile::tempdir().expect("tempdir");
    let checkout = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        root.path().join("checkout"),
    )
    .expect("checkout");
    let other = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        std::env::current_dir()
            .expect("current directory")
            .join("other-review-root"),
    )
    .expect("other");
    let git_dir = checkout.join(".git");
    let verification_root = checkout.join(".codex-review-build-test");
    let mut profile = crate::session::turn_context::review_workspace_permissions(checkout.clone());
    crate::session::turn_context::add_read_only_paths(&mut profile, std::slice::from_ref(&git_dir));
    let (policy, _) = profile.to_runtime_permissions();

    assert_eq!(
        policy.resolve_access_with_cwd(checkout.join("src/lib.rs").as_path(), checkout.as_path(),),
        codex_protocol::permissions::FileSystemAccessMode::Write
    );
    assert_eq!(
        policy.resolve_access_with_cwd(git_dir.join("index").as_path(), checkout.as_path(),),
        codex_protocol::permissions::FileSystemAccessMode::Read
    );
    assert_eq!(
        policy.resolve_access_with_cwd(other.join("file.rs").as_path(), checkout.as_path(),),
        codex_protocol::permissions::FileSystemAccessMode::Read
    );
    assert_eq!(
        policy.resolve_access_with_cwd(
            root.path().join("review-test-temp-file").as_path(),
            checkout.as_path(),
        ),
        codex_protocol::permissions::FileSystemAccessMode::Read
    );

    crate::session::turn_context::add_write_paths(
        &mut profile,
        std::slice::from_ref(&verification_root),
    );
    let (policy, _) = profile.to_runtime_permissions();
    assert_eq!(
        policy.resolve_access_with_cwd(
            verification_root.join("test-output").as_path(),
            checkout.as_path(),
        ),
        codex_protocol::permissions::FileSystemAccessMode::Write
    );
}

#[test]
fn read_stage_permissions_limit_source_reads_and_allow_external_git_metadata() {
    let root = tempfile::tempdir().expect("tempdir");
    let checkout = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        root.path().join("checkout"),
    )
    .expect("checkout");
    let git_dir = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        root.path().join("repository/.git/worktrees/checkout"),
    )
    .expect("git dir");
    let outside = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        root.path().join("outside/secret.txt"),
    )
    .expect("outside");
    let mut profile = crate::session::turn_context::review_read_permissions(checkout.clone());
    crate::session::turn_context::add_read_only_paths(&mut profile, std::slice::from_ref(&git_dir));
    let (policy, _) = profile.to_runtime_permissions();

    assert_eq!(
        policy.resolve_access_with_cwd(checkout.join("src/lib.rs").as_path(), checkout.as_path()),
        codex_protocol::permissions::FileSystemAccessMode::Read
    );
    assert_eq!(
        policy.resolve_access_with_cwd(git_dir.join("HEAD").as_path(), checkout.as_path()),
        codex_protocol::permissions::FileSystemAccessMode::Read
    );
    assert_eq!(
        policy.resolve_access_with_cwd(outside.as_path(), checkout.as_path()),
        codex_protocol::permissions::FileSystemAccessMode::Deny
    );
}

#[test]
fn evidence_requires_the_latest_test_run_to_follow_the_latest_patch() {
    let mut collector = ReviewStageEvidenceCollector::default();
    observe_command(&mut collector, "first", "just test", /*success*/ true);
    collector.observe_completed_item(&TurnItem::FileChange(
        codex_protocol::items::FileChangeItem {
            id: "patch".to_string(),
            changes: [(
                std::path::PathBuf::from("src/lib.rs"),
                codex_protocol::protocol::FileChange::Add {
                    content: "changed".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            status: Some(PatchApplyStatus::Completed),
            auto_approved: Some(true),
            stdout: Some(String::new()),
            stderr: Some(String::new()),
        },
    ));
    assert!(
        !collector
            .evidence
            .observed_successful_command_after_last_mutation("just test")
    );

    observe_command(&mut collector, "second", "just test", /*success*/ true);
    observe_command(&mut collector, "third", "just test", /*success*/ false);

    assert!(
        !collector
            .evidence
            .observed_successful_command_after_last_mutation("just test")
    );
}

#[test]
fn evidence_rejects_a_command_that_overlaps_the_latest_patch() {
    let mut collector = ReviewStageEvidenceCollector::default();
    collector.observe_response_item(&ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".to_string(),
        namespace: None,
        arguments: serde_json::json!({"cmd": "just test"}).to_string(),
        call_id: "overlapping-test".to_string(),
        internal_chat_message_metadata_passthrough: None,
    });
    observe_command_started(&mut collector, "overlapping-test", "just test");
    collector.observe_completed_item(&TurnItem::FileChange(
        codex_protocol::items::FileChangeItem {
            id: "patch".to_string(),
            changes: [(
                std::path::PathBuf::from("src/lib.rs"),
                codex_protocol::protocol::FileChange::Add {
                    content: "changed".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            status: Some(PatchApplyStatus::Completed),
            auto_approved: Some(true),
            stdout: Some(String::new()),
            stderr: Some(String::new()),
        },
    ));
    observe_command_completed(
        &mut collector,
        "overlapping-test",
        "just test",
        /*success*/ true,
    );

    assert!(
        !collector
            .evidence
            .observed_successful_command_after_last_mutation("just test")
    );
}

#[test]
fn evidence_treats_a_failed_patch_as_a_mutation_boundary() {
    let mut collector = ReviewStageEvidenceCollector::default();
    observe_command(&mut collector, "before", "just test", /*success*/ true);
    collector.observe_completed_item(&TurnItem::FileChange(
        codex_protocol::items::FileChangeItem {
            id: "failed-patch".to_string(),
            changes: std::collections::HashMap::new(),
            status: Some(PatchApplyStatus::Failed),
            auto_approved: Some(true),
            stdout: Some(String::new()),
            stderr: Some("partial write".to_string()),
        },
    ));

    assert!(
        !collector
            .evidence
            .observed_successful_command_after_last_mutation("just test")
    );
    observe_command(&mut collector, "after", "just test", /*success*/ true);
    assert!(
        collector
            .evidence
            .observed_successful_command_after_last_mutation("just test")
    );
    assert!(collector.evidence.has_failed_file_change());
}

fn observe_command(
    collector: &mut ReviewStageEvidenceCollector,
    call_id: &str,
    command: &str,
    success: bool,
) {
    collector.observe_response_item(&ResponseItem::FunctionCall {
        id: None,
        name: "exec_command".to_string(),
        namespace: None,
        arguments: serde_json::json!({"cmd": command}).to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    });
    observe_command_started(collector, call_id, command);
    observe_command_completed(collector, call_id, command, success);
}

fn observe_command_started(
    collector: &mut ReviewStageEvidenceCollector,
    call_id: &str,
    command: &str,
) {
    collector.observe_started_item(&TurnItem::CommandExecution(
        codex_protocol::items::CommandExecutionItem {
            id: call_id.to_string(),
            process_id: None,
            command: vec![command.to_string()],
            cwd: codex_utils_path_uri::PathUri::parse("file:///workspace").expect("cwd"),
            parsed_cmd: Vec::new(),
            source: codex_protocol::protocol::ExecCommandSource::Agent,
            interaction_input: None,
            status: CommandExecutionStatus::InProgress,
            stdout: None,
            stderr: None,
            aggregated_output: None,
            exit_code: None,
            duration: None,
            formatted_output: None,
        },
    ));
}

fn observe_command_completed(
    collector: &mut ReviewStageEvidenceCollector,
    call_id: &str,
    command: &str,
    success: bool,
) {
    collector.observe_completed_item(&TurnItem::CommandExecution(
        codex_protocol::items::CommandExecutionItem {
            id: call_id.to_string(),
            process_id: None,
            command: vec![command.to_string()],
            cwd: codex_utils_path_uri::PathUri::parse("file:///workspace").expect("cwd"),
            parsed_cmd: Vec::new(),
            source: codex_protocol::protocol::ExecCommandSource::Agent,
            interaction_input: None,
            status: if success {
                CommandExecutionStatus::Completed
            } else {
                CommandExecutionStatus::Failed
            },
            stdout: Some(String::new()),
            stderr: Some(String::new()),
            aggregated_output: Some(String::new()),
            exit_code: Some(if success { 0 } else { 1 }),
            duration: None,
            formatted_output: Some(String::new()),
        },
    ));
}

#[tokio::test]
async fn stage_config_disables_external_context_sources() {
    let mut config = crate::config::test_config().await;
    config.experimental_request_user_input_enabled = true;
    let request = ReviewStageRequest {
        model: "gpt-5.4".to_string(),
        system_prompt: "review".to_string(),
        context_items: Vec::new(),
        user_prompt: "review".to_string(),
        output_schema: serde_json::json!({"type": "object"}),
        permissions: StagePermissions::ReadOnly,
        workspace_read_root: None,
        workspace_write_root: None,
        include_pull_request_context: false,
    };

    let stage = review_stage_config(&config, &request).expect("stage config");

    assert!(stage.mcp_servers.get().is_empty());
    assert!(!stage.include_apps_instructions);
    assert!(!stage.include_skill_instructions);
    assert_eq!(stage.project_doc_max_bytes, 0);
    assert_eq!(stage.tool_output_token_limit, Some(2 * 1024));
    assert_eq!(stage.compact_prompt, None);
    assert!(!stage.experimental_request_user_input_enabled);
    for feature in [
        Feature::CodexHooks,
        Feature::Apps,
        Feature::Plugins,
        Feature::CodeMode,
        Feature::MemoryTool,
        Feature::WebSearchRequest,
    ] {
        assert!(
            !stage.features.enabled(feature),
            "{} remained enabled",
            feature.key()
        );
    }
    assert_eq!(
        stage.permissions.permission_profile(),
        &PermissionProfile::read_only()
    );
}

#[tokio::test]
async fn repair_config_is_read_only_and_tool_reduced() {
    let config = crate::config::test_config().await;
    let request = ReviewStageRequest {
        model: "gpt-5.4".to_string(),
        system_prompt: "repair".to_string(),
        context_items: Vec::new(),
        user_prompt: "repair".to_string(),
        output_schema: serde_json::json!({"type": "object"}),
        permissions: StagePermissions::ToolFree,
        workspace_read_root: None,
        workspace_write_root: None,
        include_pull_request_context: false,
    };

    let stage = review_stage_config(&config, &request).expect("repair config");

    assert_eq!(
        stage.permissions.permission_profile(),
        &PermissionProfile::read_only()
    );
    assert!(!stage.features.enabled(Feature::ShellTool));
    assert!(!stage.features.enabled(Feature::UnifiedExec));
    assert!(!stage.features.enabled(Feature::ToolRouter));
}

#[tokio::test]
async fn verification_environment_keeps_temp_files_in_the_explicit_root() {
    let mut config = crate::config::test_config().await;
    let root = PathUri::parse("file:///workspace/.codex-review-build-test").expect("root");

    configure_verification_environment(&mut config, &root).expect("verification environment");

    let expected_root = root.inferred_native_path_string();
    for name in ["TMPDIR", "TMP", "TEMP"] {
        assert_eq!(
            config.permissions.shell_environment_policy.r#set.get(name),
            Some(&expected_root)
        );
    }
}
