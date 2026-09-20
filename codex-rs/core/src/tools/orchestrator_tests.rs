use super::*;

#[test]
fn restricted_review_rejects_requested_but_unavailable_local_sandbox() {
    let result = reject_unenforced_local_review_sandbox(
        /*restricted_review_sandboxed_stage*/ true,
        /*sandbox_requested*/ true,
        SandboxType::None,
        Some(/*execution_environment_is_remote*/ false),
    );

    assert!(matches!(result, Err(ToolError::Rejected(_))));
}

#[test]
fn restricted_review_allows_remote_sandbox_enforcement() {
    let result = reject_unenforced_local_review_sandbox(
        /*restricted_review_sandboxed_stage*/ true,
        /*sandbox_requested*/ true,
        SandboxType::None,
        Some(/*execution_environment_is_remote*/ true),
    );

    assert!(result.is_ok());
}

#[test]
fn restricted_review_allows_concrete_local_sandbox() {
    let result = reject_unenforced_local_review_sandbox(
        /*restricted_review_sandboxed_stage*/ true,
        /*sandbox_requested*/ true,
        SandboxType::LinuxSeccomp,
        Some(/*execution_environment_is_remote*/ false),
    );

    assert!(result.is_ok());
}

#[test]
fn tool_free_review_allows_an_absent_platform_sandbox() {
    let result = reject_unenforced_local_review_sandbox(
        /*restricted_review_sandboxed_stage*/ false,
        /*sandbox_requested*/ true,
        SandboxType::None,
        Some(/*execution_environment_is_remote*/ false),
    );

    assert!(result.is_ok());
}
