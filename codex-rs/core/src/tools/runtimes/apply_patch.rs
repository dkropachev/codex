//! Apply Patch runtime: executes verified patches under the orchestrator.
//!
//! Assumes `apply_patch` verification/approval happened upstream. Reuses the
//! selected turn environment filesystem for both local and remote turns, with
//! sandboxing enforced by the explicit filesystem sandbox context.
use crate::exec::is_likely_sandbox_denied;
use crate::session::turn_context::TurnEnvironment;
use crate::tools::hook_names::HookToolName;
use crate::tools::sandboxing::Approvable;
use crate::tools::sandboxing::ApprovalAction;
use crate::tools::sandboxing::ApprovalCtx;
use crate::tools::sandboxing::ExecApprovalRequirement;
use crate::tools::sandboxing::PermissionRequestPayload;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::Sandboxable;
use crate::tools::sandboxing::ToolCtx;
use crate::tools::sandboxing::ToolError;
use crate::tools::sandboxing::ToolRuntime;
use crate::tools::sandboxing::with_cached_approval;
use codex_apply_patch::AppliedPatchDelta;
use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::ApplyPatchFileChange;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileSystemSandboxContext;
use codex_protocol::error::CodexErr;
use codex_protocol::error::SandboxErr;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::exec_output::StreamOutput;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::ReviewDecision;
use codex_sandboxing::SandboxType;
use codex_sandboxing::SandboxablePreference;
use codex_sandboxing::policy_transforms::effective_permission_profile;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use futures::future::BoxFuture;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub(crate) struct ApplyPatchApprovalKey {
    environment_id: String,
    path: PathUri,
}

#[derive(Debug)]
pub struct ApplyPatchRequest {
    pub turn_environment: TurnEnvironment,
    pub action: ApplyPatchAction,
    pub file_paths: Vec<PathUri>,
    pub changes: std::collections::HashMap<PathBuf, FileChange>,
    pub exec_approval_requirement: ExecApprovalRequirement,
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    pub permissions_preapproved: bool,
}

#[derive(Default)]
pub struct ApplyPatchRuntime {
    committed_delta: AppliedPatchDelta,
}

#[derive(Debug)]
pub struct ApplyPatchRuntimeOutput {
    pub exec_output: ExecToolCallOutput,
    pub delta: AppliedPatchDelta,
}

impl ApplyPatchRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn committed_delta(&self) -> &AppliedPatchDelta {
        &self.committed_delta
    }

    fn build_guardian_review_request(
        req: &ApplyPatchRequest,
        call_id: &str,
    ) -> std::io::Result<ApprovalAction> {
        // TODO(anp): Remove this conversion once the guardian API supports PathUri.
        let cwd = req.action.cwd.to_abs_path()?;
        let files = req
            .file_paths
            .iter()
            .map(PathUri::to_abs_path)
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(ApprovalAction::ApplyPatch {
            id: call_id.to_string(),
            cwd,
            files,
            patch: req.action.patch.clone(),
        })
    }

    fn file_system_sandbox_context_for_attempt(
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
    ) -> Result<Option<FileSystemSandboxContext>, ToolError> {
        let is_remote = req.turn_environment.environment.is_remote();
        if attempt.sandbox == SandboxType::None && !attempt.sandbox_requested {
            return Ok(None);
        }
        if attempt.sandbox == SandboxType::None && !is_remote {
            return Err(ToolError::Rejected(
                "filesystem sandboxing was requested but is unavailable locally".to_string(),
            ));
        }

        let permissions = if is_remote {
            let review_root = attempt
                .review_writable_root
                .or(attempt.review_readable_root);
            let mut permissions = review_root.map_or_else(
                || {
                    codex_protocol::models::PermissionProfile::<PathUri>::from(
                        effective_permission_profile(
                            attempt.exec_server_permissions,
                            req.additional_permissions.as_ref(),
                        ),
                    )
                },
                |root| {
                    if attempt.review_patch_tool {
                        crate::session::turn_context::review_workspace_permissions(root.clone())
                    } else if attempt.review_writable_root.is_some() {
                        crate::session::turn_context::review_verification_permissions(root.clone())
                    } else {
                        crate::session::turn_context::review_read_permissions(root.clone())
                    }
                },
            );
            crate::session::turn_context::add_read_only_paths(
                &mut permissions,
                attempt.review_protected_paths,
            );
            if !attempt.review_patch_tool {
                crate::session::turn_context::add_read_only_paths(
                    &mut permissions,
                    attempt.review_additional_read_paths,
                );
            }
            crate::session::turn_context::add_file_system_entries(
                &mut permissions,
                attempt.review_read_deny_entries,
            );
            if !attempt.review_patch_tool
                && let Some(root) = attempt.review_verification_write_root
            {
                crate::session::turn_context::add_write_paths(
                    &mut permissions,
                    std::slice::from_ref(root),
                );
            }
            permissions
        } else if attempt.review_writable_root.is_some() {
            attempt.permissions.clone().into()
        } else {
            effective_permission_profile(attempt.permissions, req.additional_permissions.as_ref())
                .into()
        };
        Ok(Some(FileSystemSandboxContext {
            permissions,
            cwd: Some(attempt.sandbox_cwd.clone()),
            workspace_roots: if is_remote {
                Vec::new()
            } else {
                attempt
                    .workspace_roots
                    .iter()
                    .map(PathUri::from_abs_path)
                    .collect()
            },
            windows_sandbox_level: attempt.windows_sandbox_level,
            windows_sandbox_private_desktop: attempt.windows_sandbox_private_desktop,
            use_legacy_landlock: attempt.use_legacy_landlock,
        }))
    }

    async fn validate_review_patch_paths(
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
        sandbox: Option<&FileSystemSandboxContext>,
    ) -> Result<(), ToolError> {
        let Some(writable_root) = attempt.review_writable_root else {
            return Ok(());
        };
        let fs = req.turn_environment.environment.get_filesystem();
        let canonical_root = fs.canonicalize(writable_root, sandbox).await.map_err(|_| {
            ToolError::Rejected("could not validate the review patch writable root".to_string())
        })?;
        let mut canonical_protected_paths = Vec::new();
        for protected_path in attempt.review_protected_paths {
            match fs.canonicalize(protected_path, sandbox).await {
                Ok(path) => canonical_protected_paths.push(path),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(ToolError::Rejected(
                        "could not validate a protected review path".to_string(),
                    ));
                }
            }
        }

        let mut canonical_patch_paths = std::collections::HashSet::new();
        for path in &req.file_paths {
            if !path.starts_with(writable_root) {
                return Err(ToolError::Rejected(
                    "review patches may only modify the selected checkout".to_string(),
                ));
            }
            if attempt
                .review_protected_paths
                .iter()
                .any(|protected_path| path.starts_with(protected_path))
            {
                return Err(ToolError::Rejected(
                    "review patches may not modify protected paths".to_string(),
                ));
            }
            reject_review_patch_symlink_components(fs.as_ref(), path, writable_root, sandbox)
                .await?;
            let canonical_path =
                canonicalize_nearest_existing_ancestor(fs.as_ref(), path, sandbox).await?;
            if canonical_path != *path {
                return Err(ToolError::Rejected(
                    "review patch paths must use their canonical checkout spelling".to_string(),
                ));
            }
            let canonical_key =
                if canonical_path.infer_path_convention() == Some(PathConvention::Windows) {
                    canonical_path.inferred_native_path_string().to_lowercase()
                } else {
                    canonical_path.to_string()
                };
            if !canonical_patch_paths.insert(canonical_key) {
                return Err(ToolError::Rejected(
                    "review patches may not use the same canonical path more than once".to_string(),
                ));
            }
            if !canonical_path.starts_with(&canonical_root) {
                return Err(ToolError::Rejected(
                    "review patch path resolves outside the selected checkout".to_string(),
                ));
            }
            if canonical_protected_paths
                .iter()
                .any(|protected_path| canonical_path.starts_with(protected_path))
            {
                return Err(ToolError::Rejected(
                    "review patch path resolves into a protected path".to_string(),
                ));
            }
        }

        for (source_path, change) in req.action.changes() {
            match change {
                ApplyPatchFileChange::Add { .. } => {
                    require_absent_review_patch_target(fs.as_ref(), source_path, sandbox).await?;
                }
                ApplyPatchFileChange::Delete { .. } => {
                    reject_symlink_review_patch_source(fs.as_ref(), source_path, sandbox).await?;
                }
                ApplyPatchFileChange::Update {
                    move_path: Some(destination_path),
                    ..
                } => {
                    reject_symlink_review_patch_source(fs.as_ref(), source_path, sandbox).await?;
                    require_absent_review_patch_target(fs.as_ref(), destination_path, sandbox)
                        .await?;
                }
                ApplyPatchFileChange::Update {
                    move_path: None, ..
                } => {
                    require_single_link_review_patch_target(fs.as_ref(), source_path, sandbox)
                        .await?;
                }
            }
        }
        Ok(())
    }
}

async fn reject_review_patch_symlink_components(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    writable_root: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ToolError> {
    let mut candidate = path.clone();
    while candidate != *writable_root {
        match fs.get_metadata(&candidate, sandbox).await {
            Ok(metadata) if metadata.is_symlink => {
                return Err(ToolError::Rejected(
                    "review patches may not traverse symlink paths".to_string(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => {
                return Err(ToolError::Rejected(
                    "could not inspect a review patch path component".to_string(),
                ));
            }
        }
        let Some(parent) = candidate.parent() else {
            return Err(ToolError::Rejected(
                "review patch path escaped the selected checkout".to_string(),
            ));
        };
        candidate = parent;
    }
    Ok(())
}

async fn reject_symlink_review_patch_source(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ToolError> {
    match fs.get_metadata(path, sandbox).await {
        Ok(metadata) if metadata.is_symlink => Err(ToolError::Rejected(
            "review patches may not move symlink sources".to_string(),
        )),
        Ok(_) => Ok(()),
        Err(_) => Err(ToolError::Rejected(
            "could not inspect a review patch move source".to_string(),
        )),
    }
}

async fn require_absent_review_patch_target(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ToolError> {
    match fs.get_metadata(path, sandbox).await {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(ToolError::Rejected(
            "review patches may not overwrite an existing add or move destination".to_string(),
        )),
        Err(_) => Err(ToolError::Rejected(
            "could not inspect a review patch destination".to_string(),
        )),
    }
}

async fn require_single_link_review_patch_target(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ToolError> {
    match fs.get_metadata(path, sandbox).await {
        Ok(metadata) if metadata.is_symlink => Err(ToolError::Rejected(
            "review patches may not update symlink targets".to_string(),
        )),
        Ok(metadata) if metadata.hard_link_count == Some(1) => Ok(()),
        Ok(metadata) if metadata.hard_link_count.is_some_and(|count| count > 1) => {
            Err(ToolError::Rejected(
                "review patches may not update files with multiple hard links".to_string(),
            ))
        }
        Ok(_) => Err(ToolError::Rejected(
            "review patch content target has no verified hard-link count".to_string(),
        )),
        Err(_) => Err(ToolError::Rejected(
            "could not inspect a review patch content target".to_string(),
        )),
    }
}

async fn canonicalize_nearest_existing_ancestor(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<PathUri, ToolError> {
    let mut missing_components: Vec<String> = Vec::new();
    for candidate in path.ancestors() {
        match fs.canonicalize(&candidate, sandbox).await {
            Ok(mut canonical) => {
                for component in missing_components.iter().rev() {
                    canonical = canonical.join(component).map_err(|_| {
                        ToolError::Rejected("could not resolve a review patch path".to_string())
                    })?;
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                reject_broken_symlink(fs, &candidate, sandbox).await?;
                let Some(component) = candidate.basename() else {
                    break;
                };
                missing_components.push(component);
            }
            Err(_) => {
                return Err(ToolError::Rejected(
                    "could not validate a review patch path".to_string(),
                ));
            }
        }
    }
    Err(ToolError::Rejected(
        "review patch path has no existing ancestor".to_string(),
    ))
}

async fn reject_broken_symlink(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<(), ToolError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(basename) = path.basename() else {
        return Ok(());
    };
    let entries = match fs.read_directory(&parent, sandbox).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(ToolError::Rejected(
                "could not inspect a review patch path".to_string(),
            ));
        }
    };
    let windows_path = path.infer_path_convention() == Some(PathConvention::Windows);
    if entries.iter().any(|entry| {
        entry.is_symlink
            && (entry.file_name == basename
                || (windows_path && entry.file_name.eq_ignore_ascii_case(&basename)))
    }) {
        return Err(ToolError::Rejected(
            "review patch path is a broken symlink".to_string(),
        ));
    }
    Ok(())
}

impl Sandboxable for ApplyPatchRuntime {
    fn sandbox_preference(&self) -> SandboxablePreference {
        SandboxablePreference::Auto
    }
    fn escalate_on_failure(&self) -> bool {
        true
    }
}

impl Approvable<ApplyPatchRequest> for ApplyPatchRuntime {
    type ApprovalKey = ApplyPatchApprovalKey;

    fn approval_keys(&self, req: &ApplyPatchRequest) -> Vec<Self::ApprovalKey> {
        req.file_paths
            .iter()
            .cloned()
            .map(|path| ApplyPatchApprovalKey {
                environment_id: req.turn_environment.environment_id.clone(),
                path,
            })
            .collect()
    }

    fn start_approval_async<'a>(
        &'a mut self,
        req: &'a ApplyPatchRequest,
        ctx: ApprovalCtx<'a>,
    ) -> BoxFuture<'a, ReviewDecision> {
        let session = ctx.session;
        let turn = ctx.turn;
        let call_id = ctx.call_id.to_string();
        let retry_reason = ctx.retry_reason.clone();
        let approval_keys = self.approval_keys(req);
        let changes = req.changes.clone();
        Box::pin(async move {
            if req.permissions_preapproved && retry_reason.is_none() {
                return ReviewDecision::Approved;
            }
            if let Some(reason) = retry_reason {
                return session
                    .request_patch_approval(
                        turn,
                        call_id,
                        changes.clone(),
                        Some(reason),
                        /*grant_root*/ None,
                    )
                    .await;
            }

            with_cached_approval(
                &session.services,
                "apply_patch",
                approval_keys,
                || async move {
                    session
                        .request_patch_approval(
                            turn, call_id, changes, /*reason*/ None, /*grant_root*/ None,
                        )
                        .await
                },
            )
            .await
        })
    }

    fn approval_action(
        &self,
        req: &ApplyPatchRequest,
        ctx: &ApprovalCtx<'_>,
    ) -> std::io::Result<ApprovalAction> {
        ApplyPatchRuntime::build_guardian_review_request(req, ctx.call_id)
    }

    fn wants_no_sandbox_approval(&self, policy: AskForApproval) -> bool {
        match policy {
            AskForApproval::Never => false,
            AskForApproval::Granular(granular_config) => granular_config.allows_sandbox_approval(),
            AskForApproval::OnRequest => true,
            AskForApproval::UnlessTrusted => true,
        }
    }

    // apply_patch approvals are decided upstream by assess_patch_safety.
    //
    // This override ensures the orchestrator runs the patch approval flow when required instead
    // of falling back to the global exec approval policy.
    fn exec_approval_requirement(
        &self,
        req: &ApplyPatchRequest,
    ) -> Option<ExecApprovalRequirement> {
        Some(req.exec_approval_requirement.clone())
    }

    fn permission_request_payload(
        &self,
        req: &ApplyPatchRequest,
    ) -> Option<PermissionRequestPayload> {
        Some(PermissionRequestPayload {
            tool_name: HookToolName::apply_patch(),
            tool_input: serde_json::json!({ "command": req.action.patch }),
        })
    }
}

impl ToolRuntime<ApplyPatchRequest, ApplyPatchRuntimeOutput> for ApplyPatchRuntime {
    fn execution_environment_is_remote(&self, req: &ApplyPatchRequest) -> Option<bool> {
        Some(req.turn_environment.environment.is_remote())
    }

    fn sandbox_cwd<'a>(&self, req: &'a ApplyPatchRequest) -> Option<&'a PathUri> {
        Some(&req.action.cwd)
    }

    async fn run(
        &mut self,
        req: &ApplyPatchRequest,
        attempt: &SandboxAttempt<'_>,
        _ctx: &ToolCtx,
    ) -> Result<ApplyPatchRuntimeOutput, ToolError> {
        let started_at = Instant::now();
        let fs = req.turn_environment.environment.get_filesystem();
        let sandbox = Self::file_system_sandbox_context_for_attempt(req, attempt)?;
        Self::validate_review_patch_paths(req, attempt, sandbox.as_ref()).await?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = codex_apply_patch::apply_patch(
            &req.action.patch,
            &req.action.cwd,
            &mut stdout,
            &mut stderr,
            fs.as_ref(),
            sandbox.as_ref(),
        )
        .await;
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        let failed = result.is_err();
        let exit_code = if failed { 1 } else { 0 };
        let delta = match result {
            Ok(delta) => delta,
            Err(failure) => failure.into_parts().1,
        };
        self.committed_delta.append(delta);
        let output = ExecToolCallOutput {
            exit_code,
            stdout: StreamOutput::new(stdout.clone()),
            stderr: StreamOutput::new(stderr.clone()),
            aggregated_output: StreamOutput::new(format!("{stdout}{stderr}")),
            duration: started_at.elapsed(),
            timed_out: false,
        };
        if failed && is_likely_sandbox_denied(attempt.sandbox, &output) {
            return Err(ToolError::Codex(CodexErr::Sandbox(SandboxErr::Denied {
                output: Box::new(output),
                network_policy_decision: None,
            })));
        }
        Ok(ApplyPatchRuntimeOutput {
            exec_output: output,
            delta: self.committed_delta.clone(),
        })
    }
}

#[cfg(test)]
#[path = "apply_patch_tests.rs"]
mod tests;
