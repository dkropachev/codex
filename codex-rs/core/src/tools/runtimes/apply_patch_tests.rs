use super::*;
use crate::tools::sandboxing::SandboxAttempt;
use codex_apply_patch::MaybeApplyPatchVerified;
use codex_exec_server::LOCAL_FS;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::FileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxType;
use codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy;
use codex_sandboxing::policy_transforms::effective_network_sandbox_policy;
use codex_utils_path_uri::PathUri;
use core_test_support::PathBufExt;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::Path;

fn test_turn_environment(environment_id: &str) -> crate::session::turn_context::TurnEnvironment {
    crate::session::turn_context::TurnEnvironment::new(
        environment_id.to_string(),
        std::sync::Arc::new(codex_exec_server::Environment::default_for_tests()),
        PathUri::from_abs_path(&std::env::temp_dir().abs()),
        /*shell*/ None,
    )
}

async fn verified_patch_action(cwd: &PathUri, patch: &str) -> ApplyPatchAction {
    let argv = vec!["apply_patch".to_string(), patch.to_string()];
    match codex_apply_patch::maybe_parse_apply_patch_verified(
        &argv,
        cwd,
        LOCAL_FS.as_ref(),
        /*sandbox*/ None,
    )
    .await
    {
        MaybeApplyPatchVerified::Body(action) => action,
        other => panic!("expected verified patch, got {other:?}"),
    }
}

async fn validate_review_action(
    checkout: &Path,
    action: ApplyPatchAction,
) -> Result<(), ToolError> {
    let file_paths = action
        .changes()
        .iter()
        .flat_map(|(path, change)| {
            std::iter::once(path.clone()).chain(match change {
                ApplyPatchFileChange::Update {
                    move_path: Some(destination_path),
                    ..
                } => Some(destination_path.clone()),
                ApplyPatchFileChange::Add { .. }
                | ApplyPatchFileChange::Delete { .. }
                | ApplyPatchFileChange::Update {
                    move_path: None, ..
                } => None,
            })
        })
        .collect();
    let checkout = checkout.to_path_buf().abs();
    let checkout_uri = PathUri::from_abs_path(&checkout);
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action,
        file_paths,
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };
    let permissions = PermissionProfile::read_only();
    let manager = SandboxManager::new();
    let attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: false,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &checkout_uri,
        workspace_roots: std::slice::from_ref(&checkout),
        review_protected_paths: &[],
        review_additional_read_paths: &[],
        review_read_deny_entries: &[],
        review_readable_root: None,
        review_writable_root: Some(&checkout_uri),
        review_patch_tool: true,
        review_verification_write_root: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    ApplyPatchRuntime::validate_review_patch_paths(&req, &attempt, /*sandbox*/ None).await
}

fn test_remote_turn_environment() -> crate::session::turn_context::TurnEnvironment {
    crate::session::turn_context::TurnEnvironment::new(
        "remote".to_string(),
        std::sync::Arc::new(
            codex_exec_server::Environment::create_for_tests(Some("ws://127.0.0.1:1".to_string()))
                .expect("remote environment"),
        ),
        PathUri::from_abs_path(&std::env::temp_dir().abs()),
        /*shell*/ None,
    )
}

#[test]
fn wants_no_sandbox_approval_granular_respects_sandbox_flag() {
    let runtime = ApplyPatchRuntime::new();
    assert!(runtime.wants_no_sandbox_approval(AskForApproval::OnRequest));
    assert!(
        !runtime.wants_no_sandbox_approval(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: false,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
    assert!(
        runtime.wants_no_sandbox_approval(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
}

#[tokio::test]
async fn guardian_review_request_includes_patch_context() {
    let path = std::env::temp_dir()
        .join("guardian-apply-patch-test.txt")
        .abs();
    let action =
        ApplyPatchAction::new_add_for_test(&PathUri::from_abs_path(&path), "hello".to_string());
    let expected_cwd = action.cwd.to_abs_path().expect("native patch cwd");
    let expected_patch = action.patch.clone();
    let request = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action,
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::from([(
            path.to_path_buf(),
            FileChange::Add {
                content: "hello".to_string(),
            },
        )]),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };

    let guardian_request = ApplyPatchRuntime::build_guardian_review_request(&request, "call-1")
        .expect("native guardian request cwd");

    assert_eq!(
        guardian_request,
        ApprovalAction::ApplyPatch {
            id: "call-1".to_string(),
            cwd: expected_cwd,
            files: vec![path],
            patch: expected_patch,
        }
    );
}

#[tokio::test]
async fn permission_request_payload_uses_apply_patch_hook_name_and_aliases() {
    let runtime = ApplyPatchRuntime::new();
    let path = std::env::temp_dir()
        .join("apply-patch-permission-request-payload.txt")
        .abs();
    let action =
        ApplyPatchAction::new_add_for_test(&PathUri::from_abs_path(&path), "hello".to_string());
    let expected_patch = action.patch.clone();
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action,
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };

    let payload = runtime
        .permission_request_payload(&req)
        .expect("permission request payload");

    assert_eq!(payload.tool_name.name(), "apply_patch");
    assert_eq!(
        payload.tool_name.matcher_aliases(),
        &["Write".to_string(), "Edit".to_string()]
    );
    assert_eq!(
        payload.tool_input,
        serde_json::json!({ "command": expected_patch })
    );
}

#[tokio::test]
async fn approval_keys_include_environment_id() {
    let runtime = ApplyPatchRuntime::new();
    let path = std::env::temp_dir()
        .join("apply-patch-approval-key.txt")
        .abs();
    let path_uri = PathUri::from_abs_path(&path);
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment("remote"),
        action: ApplyPatchAction::new_add_for_test(&path_uri, "hello".to_string()),
        file_paths: vec![path_uri.clone()],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };

    let keys = runtime.approval_keys(&req);

    assert_eq!(
        serde_json::to_value(&keys).expect("serialize approval keys"),
        serde_json::json!([
            {
                "environment_id": "remote",
                "path": path_uri,
            }
        ])
    );
}

#[tokio::test]
async fn sandbox_cwd_uses_patch_action_cwd() {
    let runtime = ApplyPatchRuntime::new();
    let path = std::env::temp_dir()
        .join("apply-patch-runtime-sandbox-cwd.txt")
        .abs();
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action: ApplyPatchAction::new_add_for_test(
            &PathUri::from_abs_path(&path),
            "hello".to_string(),
        ),
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };

    assert_eq!(runtime.sandbox_cwd(&req), Some(&req.action.cwd));
}

#[tokio::test]
async fn file_system_sandbox_context_uses_active_attempt() {
    let path = std::env::temp_dir()
        .join("apply-patch-runtime-attempt.txt")
        .abs();
    let additional_permissions = AdditionalPermissionProfile {
        network: None,
        file_system: Some(FileSystemPermissions::from_read_write_roots(
            Some(vec![path.clone()]),
            Some(Vec::new()),
        )),
    };
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action: ApplyPatchAction::new_add_for_test(
            &PathUri::from_abs_path(&path),
            "hello".to_string(),
        ),
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: Some(additional_permissions.clone()),
        permissions_preapproved: false,
    };
    let file_system_policy = FileSystemSandboxPolicy::default();
    let permissions = PermissionProfile::from_runtime_permissions(
        &file_system_policy,
        NetworkSandboxPolicy::Restricted,
    );
    let manager = SandboxManager::new();
    let sandbox_policy_cwd = PathUri::from_abs_path(&path);
    let attempt = SandboxAttempt {
        sandbox: SandboxType::MacosSeatbelt,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &sandbox_policy_cwd,
        workspace_roots: std::slice::from_ref(&path),
        review_protected_paths: &[],
        review_additional_read_paths: &[],
        review_read_deny_entries: &[],
        review_readable_root: None,
        review_writable_root: None,
        review_patch_tool: false,
        review_verification_write_root: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: true,
        windows_sandbox_level: WindowsSandboxLevel::RestrictedToken,
        windows_sandbox_private_desktop: true,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    let sandbox = ApplyPatchRuntime::file_system_sandbox_context_for_attempt(&req, &attempt)
        .expect("sandbox context construction")
        .expect("sandbox context");

    let file_system_policy =
        effective_file_system_sandbox_policy(&file_system_policy, Some(&additional_permissions));
    let network_policy = effective_network_sandbox_policy(
        NetworkSandboxPolicy::Restricted,
        Some(&additional_permissions),
    );
    let expected_permissions =
        PermissionProfile::from_runtime_permissions(&file_system_policy, network_policy);
    let native_permissions: PermissionProfile = sandbox
        .permissions
        .clone()
        .try_into()
        .expect("native sandbox permissions");
    assert_eq!(native_permissions, expected_permissions);
    assert_eq!(
        sandbox.cwd,
        Some(codex_utils_path_uri::PathUri::from_abs_path(&path))
    );
    assert_eq!(
        sandbox.windows_sandbox_level,
        WindowsSandboxLevel::RestrictedToken
    );
    assert_eq!(sandbox.windows_sandbox_private_desktop, true);
    assert_eq!(sandbox.use_legacy_landlock, true);
}

#[tokio::test]
async fn no_sandbox_attempt_has_no_file_system_context() {
    let path = std::env::temp_dir()
        .join("apply-patch-runtime-none.txt")
        .abs();
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action: ApplyPatchAction::new_add_for_test(
            &PathUri::from_abs_path(&path),
            "hello".to_string(),
        ),
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };
    let permissions = PermissionProfile::Disabled;
    let manager = SandboxManager::new();
    let sandbox_policy_cwd = PathUri::from_abs_path(&path);
    let attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: false,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &sandbox_policy_cwd,
        workspace_roots: std::slice::from_ref(&path),
        review_protected_paths: &[],
        review_additional_read_paths: &[],
        review_read_deny_entries: &[],
        review_readable_root: None,
        review_writable_root: None,
        review_patch_tool: false,
        review_verification_write_root: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    assert_eq!(
        ApplyPatchRuntime::file_system_sandbox_context_for_attempt(&req, &attempt)
            .expect("sandbox context construction"),
        None
    );
}

#[tokio::test]
async fn requested_local_sandbox_cannot_resolve_to_none() {
    let path = std::env::temp_dir()
        .join("apply-patch-runtime-local-none.txt")
        .abs();
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action: ApplyPatchAction::new_add_for_test(
            &PathUri::from_abs_path(&path),
            "hello".to_string(),
        ),
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };
    let permissions = PermissionProfile::read_only();
    let manager = SandboxManager::new();
    let cwd = PathUri::from_abs_path(&path);
    let attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &cwd,
        workspace_roots: std::slice::from_ref(&path),
        review_protected_paths: &[],
        review_additional_read_paths: &[],
        review_read_deny_entries: &[],
        review_readable_root: None,
        review_writable_root: None,
        review_patch_tool: false,
        review_verification_write_root: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    assert!(matches!(
        ApplyPatchRuntime::file_system_sandbox_context_for_attempt(&req, &attempt),
        Err(ToolError::Rejected(_))
    ));
}

#[test]
fn requested_remote_sandbox_uses_exec_server_context() {
    let root = std::env::temp_dir().abs();
    let path = root.join("apply-patch-runtime-remote-none.txt");
    let req = ApplyPatchRequest {
        turn_environment: test_remote_turn_environment(),
        action: ApplyPatchAction::new_add_for_test(
            &PathUri::from_abs_path(&path),
            "hello".to_string(),
        ),
        file_paths: vec![PathUri::from_abs_path(&path)],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };
    let permissions = PermissionProfile::read_only();
    let manager = SandboxManager::new();
    let cwd = PathUri::from_abs_path(&root);
    let attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &cwd,
        workspace_roots: std::slice::from_ref(&root),
        review_protected_paths: &[],
        review_additional_read_paths: &[],
        review_read_deny_entries: &[],
        review_readable_root: None,
        review_writable_root: Some(&cwd),
        review_patch_tool: true,
        review_verification_write_root: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    let sandbox = ApplyPatchRuntime::file_system_sandbox_context_for_attempt(&req, &attempt)
        .expect("sandbox context construction")
        .expect("remote sandbox context");

    assert_eq!(sandbox.workspace_roots, Vec::<PathUri>::new());
    assert_eq!(
        sandbox.permissions,
        crate::session::turn_context::review_workspace_permissions(cwd)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn review_patch_validation_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::create_dir_all(&outside).expect("outside");
    symlink(&outside, checkout.join("escape")).expect("symlink");
    let checkout = checkout.abs();
    let target = checkout.join("escape/victim.txt");
    let target_uri = PathUri::from_abs_path(&target);
    let checkout_uri = PathUri::from_abs_path(&checkout);
    let req = ApplyPatchRequest {
        turn_environment: test_turn_environment(codex_exec_server::LOCAL_ENVIRONMENT_ID),
        action: ApplyPatchAction::new_add_for_test(&target_uri, "hello".to_string()),
        file_paths: vec![target_uri],
        changes: HashMap::new(),
        exec_approval_requirement: ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        },
        additional_permissions: None,
        permissions_preapproved: false,
    };
    let permissions = PermissionProfile::read_only();
    let manager = SandboxManager::new();
    let attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: false,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &checkout_uri,
        workspace_roots: std::slice::from_ref(&checkout),
        review_protected_paths: &[],
        review_additional_read_paths: &[],
        review_read_deny_entries: &[],
        review_readable_root: None,
        review_writable_root: Some(&checkout_uri),
        review_patch_tool: true,
        review_verification_write_root: None,
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    let result =
        ApplyPatchRuntime::validate_review_patch_paths(&req, &attempt, /*sandbox*/ None).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
}

#[cfg(unix)]
#[tokio::test]
async fn review_patch_validation_rejects_an_in_checkout_symlink_update() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(checkout.join("target.txt"), "original\n").expect("target");
    symlink("target.txt", checkout.join("linked.txt")).expect("symlink");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Update File: linked.txt\n@@\n-original\n+changed\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
    assert_eq!(
        std::fs::read_to_string(checkout.join("target.txt")).expect("target contents"),
        "original\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn review_patch_validation_rejects_an_in_checkout_symlink_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    let real = checkout.join("real");
    std::fs::create_dir_all(&real).expect("real directory");
    symlink("real", checkout.join("alias")).expect("symlink");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Add File: alias/new.txt\n+new\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
    assert!(!real.join("new.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn review_patch_validation_rejects_symlink_delete_and_move() {
    use std::os::unix::fs::symlink;

    for patch in [
        "*** Begin Patch\n*** Delete File: linked.txt\n*** End Patch",
        "*** Begin Patch\n*** Update File: linked.txt\n*** Move to: moved.txt\n@@\n-original\n+changed\n*** End Patch",
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let checkout = temp.path().join("checkout");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::write(checkout.join("target.txt"), "original\n").expect("target");
        symlink("target.txt", checkout.join("linked.txt")).expect("symlink");
        let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
        let action = verified_patch_action(&cwd, patch).await;

        let result = validate_review_action(&checkout, action).await;

        assert!(matches!(result, Err(ToolError::Rejected(_))));
        assert!(checkout.join("linked.txt").is_symlink());
        assert_eq!(
            std::fs::read_to_string(checkout.join("target.txt")).expect("target contents"),
            "original\n"
        );
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn review_patch_validation_rejects_hard_link_content_update() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    let outside = temp.path().join("outside.txt");
    let linked = checkout.join("linked.txt");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(&outside, "original\n").expect("outside file");
    std::fs::hard_link(&outside, &linked).expect("hard link");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Update File: linked.txt\n@@\n-original\n+changed\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
    assert_eq!(
        std::fs::read_to_string(outside).expect("outside contents"),
        "original\n"
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn review_patch_validation_allows_hard_link_delete() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    let outside = temp.path().join("outside.txt");
    let linked = checkout.join("linked.txt");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(&outside, "original\n").expect("outside file");
    std::fs::hard_link(&outside, &linked).expect("hard link");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Delete File: linked.txt\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(result.is_ok(), "{result:?}");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn review_patch_validation_allows_hard_link_move_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    let outside = temp.path().join("outside.txt");
    let linked = checkout.join("linked.txt");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(&outside, "original\n").expect("outside file");
    std::fs::hard_link(&outside, &linked).expect("hard link");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let patch = "*** Begin Patch\n*** Update File: linked.txt\n*** Move to: moved.txt\n@@\n-original\n+changed\n*** End Patch";
    let action = verified_patch_action(&cwd, patch).await;

    let result = validate_review_action(&checkout, action).await;

    assert!(result.is_ok(), "{result:?}");
    codex_apply_patch::apply_patch(
        patch,
        &cwd,
        &mut Vec::new(),
        &mut Vec::new(),
        LOCAL_FS.as_ref(),
        /*sandbox*/ None,
    )
    .await
    .expect("move patch should apply");
    assert_eq!(
        std::fs::read_to_string(outside).expect("outside contents"),
        "original\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("moved.txt")).expect("moved contents"),
        "changed\n"
    );
    assert!(!linked.exists());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn review_patch_validation_rejects_hard_link_move_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    let outside = temp.path().join("outside.txt");
    let source = checkout.join("source.txt");
    let destination = checkout.join("destination.txt");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(&source, "source\n").expect("source file");
    std::fs::write(&outside, "outside\n").expect("outside file");
    std::fs::hard_link(&outside, &destination).expect("hard link");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Update File: source.txt\n*** Move to: destination.txt\n@@\n-source\n+changed\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
}

#[tokio::test]
async fn review_patch_validation_rejects_an_existing_add_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(checkout.join("existing.txt"), "unrelated\n").expect("existing file");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Add File: existing.txt\n+replacement\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
    assert_eq!(
        std::fs::read_to_string(checkout.join("existing.txt")).expect("existing contents"),
        "unrelated\n"
    );
}

#[tokio::test]
async fn review_patch_validation_rejects_an_existing_move_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let checkout = temp.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::write(checkout.join("source.txt"), "source\n").expect("source file");
    std::fs::write(checkout.join("destination.txt"), "unrelated\n").expect("destination file");
    let cwd = PathUri::from_host_native_path(&checkout).expect("checkout URI");
    let action = verified_patch_action(
        &cwd,
        "*** Begin Patch\n*** Update File: source.txt\n*** Move to: destination.txt\n@@\n-source\n+changed\n*** End Patch",
    )
    .await;

    let result = validate_review_action(&checkout, action).await;

    assert!(matches!(result, Err(ToolError::Rejected(_))));
    assert_eq!(
        std::fs::read_to_string(checkout.join("destination.txt")).expect("destination contents"),
        "unrelated\n"
    );
}
