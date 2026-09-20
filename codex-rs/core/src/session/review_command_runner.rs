use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_exec_server::ExecBackend;
use codex_exec_server::ExecEnvPolicy;
use codex_exec_server::ExecOutputStream;
use codex_exec_server::ExecParams;
use codex_exec_server::ExecProcess;
use codex_exec_server::ProcessId;
use codex_git_utils::ReviewCommand;
use codex_git_utils::ReviewCommandOutput;
use codex_git_utils::ReviewCommandRunner;
use codex_git_utils::ReviewScopeResolution;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use tokio::time::Instant;
use tokio::time::timeout_at;
use uuid::Uuid;

use super::Session;

const READ_CHUNK_BYTES: usize = 64 * 1024;
const READ_WAIT: Duration = Duration::from_secs(1);
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(1);
const REVIEW_ENV_EXCLUDE_PATTERNS: [&str; 2] = ["GIT_*", "GH_REPO"];

struct TerminateProcessOnDrop {
    process: Option<Arc<dyn ExecProcess>>,
}

impl TerminateProcessOnDrop {
    fn new(process: Arc<dyn ExecProcess>) -> Self {
        Self {
            process: Some(process),
        }
    }

    fn disarm(&mut self) {
        self.process = None;
    }
}

impl Drop for TerminateProcessOnDrop {
    fn drop(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(TERMINATE_TIMEOUT, process.terminate()).await;
            });
        }
    }
}

/// Runs review-scope metadata commands through the selected turn executor.
pub(crate) struct ExecutorReviewCommandRunner {
    exec_backend: Arc<dyn ExecBackend>,
    env_policy: ExecEnvPolicy,
}

impl ExecutorReviewCommandRunner {
    pub(crate) fn new(
        exec_backend: Arc<dyn ExecBackend>,
        shell_environment_policy: &ShellEnvironmentPolicy,
    ) -> Self {
        Self {
            exec_backend,
            env_policy: exec_env_policy(shell_environment_policy),
        }
    }
}

impl Session {
    /// Resolves review-picker metadata beside the primary selected environment without starting a
    /// model turn or mutating conversation state.
    pub(crate) async fn resolve_review_scope(&self) -> Result<ReviewScopeResolution> {
        let environment = self
            .services
            .turn_environments
            .resolve_primary_environment()
            .await
            .map_err(|err| anyhow::anyhow!("failed to start review scope environment: {err}"))?
            .context("cannot resolve review scope without a selected environment")?;
        let (shell_environment_policy, windows_sandbox_level, config) = {
            let state = self.state.lock().await;
            let mut config = (*state.session_configuration.original_config_do_not_use).clone();
            state
                .session_configuration
                .apply_permission_profile_to_permissions(&mut config.permissions);
            (
                state
                    .session_configuration
                    .original_config_do_not_use
                    .permissions
                    .shell_environment_policy
                    .clone(),
                state.session_configuration.windows_sandbox_level,
                config,
            )
        };
        let runner = ExecutorReviewCommandRunner::new(
            environment.environment.get_exec_backend(),
            &shell_environment_policy,
        );
        let mut resolution =
            codex_git_utils::resolve_review_scope(&runner, environment.cwd()).await;
        resolution.review_execution_available = environment.environment.is_remote()
            || !cfg!(target_os = "windows")
            || windows_sandbox_level != codex_protocol::config_types::WindowsSandboxLevel::Disabled;
        resolution.review_unavailable_reason =
            (!resolution.review_execution_available).then(|| {
                "Review requires Windows sandboxing for the selected local executor.".to_string()
            });
        let fix_permissions = codex_protocol::models::PermissionProfile::workspace_write_with(
            &[],
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        );
        resolution.fix_execution_available = resolution.review_execution_available
            && config
                .permissions
                .can_set_permission_profile(&fix_permissions)
                .is_ok()
            && config.is_permission_profile_allowed(
                codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE,
                &fix_permissions,
            );
        resolution.fix_unavailable_reason = (!resolution.fix_execution_available).then(|| {
            resolution
                .review_unavailable_reason
                .clone()
                .unwrap_or_else(|| {
                    "Fix is unavailable under the active permission constraints.".to_string()
                })
        });
        Ok(resolution)
    }
}

impl ReviewCommandRunner for ExecutorReviewCommandRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let deadline = Instant::now() + command.command_timeout();
        let started = timeout_at(
            deadline,
            self.exec_backend.start(ExecParams {
                process_id: ProcessId::from(format!("review-scope-{}", Uuid::new_v4())),
                argv: command.argv().to_vec(),
                cwd: command.cwd().clone(),
                env_policy: Some(self.env_policy.clone()),
                env: command.env_vars().clone(),
                tty: false,
                pipe_stdin: false,
                arg0: None,
                sandbox: None,
                enforce_managed_network: false,
                managed_network: None,
            }),
        )
        .await
        .context("review command timed out while starting")??;
        let process = started.process;
        let mut terminate_on_drop = TerminateProcessOnDrop::new(Arc::clone(&process));
        let collected = timeout_at(
            deadline,
            collect_output(process.as_ref(), command.output_bytes_cap()),
        )
        .await;
        match collected {
            Ok(Ok(output)) => {
                terminate_on_drop.disarm();
                Ok(output)
            }
            Ok(Err(err)) => {
                let terminate_deadline = Instant::now() + TERMINATE_TIMEOUT;
                let _ = timeout_at(terminate_deadline, process.terminate()).await;
                Err(err)
            }
            Err(_) => {
                let terminate_deadline = Instant::now() + TERMINATE_TIMEOUT;
                let _ = timeout_at(terminate_deadline, process.terminate()).await;
                bail!("review command timed out")
            }
        }
    }
}

async fn collect_output(
    process: &dyn codex_exec_server::ExecProcess,
    output_bytes_cap: usize,
) -> Result<ReviewCommandOutput> {
    let mut after_seq = None;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let response = process
            .read(
                after_seq,
                Some(READ_CHUNK_BYTES),
                Some(READ_WAIT.as_millis() as u64),
            )
            .await?;
        for chunk in response.chunks {
            after_seq = Some(after_seq.map_or(chunk.seq, |seq: u64| seq.max(chunk.seq)));
            let output = match chunk.stream {
                ExecOutputStream::Stdout | ExecOutputStream::Pty => &mut stdout,
                ExecOutputStream::Stderr => &mut stderr,
            };
            append_capped(output, &chunk.chunk.into_inner(), output_bytes_cap);
        }
        after_seq = response.next_seq.checked_sub(1).or(after_seq);
        if let Some(failure) = response.failure {
            bail!("review command failed: {failure}");
        }
        if response.closed {
            let exit_code = response
                .exit_code
                .context("review command closed without an exit code")?;
            return Ok(ReviewCommandOutput {
                exit_code,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
    }
}

fn append_capped(output: &mut Vec<u8>, chunk: &[u8], output_bytes_cap: usize) {
    let remaining = output_bytes_cap.saturating_sub(output.len());
    output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}

fn exec_env_policy(policy: &ShellEnvironmentPolicy) -> ExecEnvPolicy {
    let policy = sanitized_review_shell_environment_policy(policy);
    let exclude = policy
        .exclude
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    ExecEnvPolicy {
        inherit: policy.inherit.clone(),
        ignore_default_excludes: policy.ignore_default_excludes,
        exclude,
        r#set: policy.r#set,
        include_only: policy
            .include_only
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

pub(super) fn sanitized_review_shell_environment_policy(
    policy: &ShellEnvironmentPolicy,
) -> ShellEnvironmentPolicy {
    let mut policy = policy.clone();
    for pattern in REVIEW_ENV_EXCLUDE_PATTERNS {
        if !policy
            .exclude
            .iter()
            .any(|existing| existing.to_string().eq_ignore_ascii_case(pattern))
        {
            policy.exclude.push(
                codex_protocol::config_types::EnvironmentVariablePattern::new_case_insensitive(
                    pattern,
                ),
            );
        }
    }
    policy.r#set.retain(|key, _| {
        !key.eq_ignore_ascii_case("GH_REPO") && !key.to_ascii_uppercase().starts_with("GIT_")
    });
    policy
}

#[cfg(test)]
#[path = "review_command_runner_tests.rs"]
mod tests;
