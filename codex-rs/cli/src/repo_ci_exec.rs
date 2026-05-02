use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_utils_cli::CliConfigOverrides;
use serde::Deserialize;
use std::fs;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::ChildStderr;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

const MAX_FEEDBACK_BYTES: usize = 16_000;
const REPO_CI_EXEC_MIN_TIMEOUT: Duration = Duration::from_secs(60);
const REPO_CI_EXEC_MAX_TIMEOUT: Duration = Duration::from_secs(600);
const REPO_CI_EXEC_POLL_INTERVAL: Duration = Duration::from_millis(100);
const REPO_CI_EXEC_PROGRESS_INTERVAL: Duration = Duration::from_secs(30);
const REPO_CI_EXEC_TERMINATION_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn repo_ci_exec_timeout(local_test_time_budget_sec: u64) -> Duration {
    let budget = Duration::from_secs(local_test_time_budget_sec.max(1));
    if budget < REPO_CI_EXEC_MIN_TIMEOUT {
        REPO_CI_EXEC_MIN_TIMEOUT
    } else if budget > REPO_CI_EXEC_MAX_TIMEOUT {
        REPO_CI_EXEC_MAX_TIMEOUT
    } else {
        budget
    }
}

pub(crate) async fn run_repo_ci_exec_json<T>(
    root_config_overrides: &CliConfigOverrides,
    repo_root: &Path,
    prompt: &str,
    schema: serde_json::Value,
    action: &str,
    timeout: Duration,
) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let raw_overrides = root_config_overrides.raw_overrides.clone();
    let repo_root = repo_root.to_path_buf();
    let prompt = prompt.to_string();
    let action = action.to_string();
    let worker_action = action.clone();
    let text = tokio::task::spawn_blocking(move || {
        run_repo_ci_exec_json_blocking(
            raw_overrides,
            repo_root,
            prompt,
            schema,
            worker_action,
            timeout,
        )
    })
    .await
    .with_context(|| format!("{action} worker task failed"))??;
    parse_json_payload(&text)
}

pub(crate) fn truncate_for_feedback(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let keep = max_bytes / 2;
    let head_end = floor_char_boundary(text, keep);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(keep));
    format!("{}\n...\n{}", &text[..head_end], &text[tail_start..])
}

fn run_repo_ci_exec_json_blocking(
    raw_overrides: Vec<String>,
    repo_root: PathBuf,
    prompt: String,
    schema: serde_json::Value,
    action: String,
    timeout: Duration,
) -> Result<String> {
    let tempdir =
        tempfile::tempdir().with_context(|| format!("failed to create tempdir for {action}"))?;
    let schema_path = tempdir.path().join("repo-ci-output.schema.json");
    let output_path = tempdir.path().join("repo-ci-output.json");
    fs::write(&schema_path, serde_json::to_vec_pretty(&schema)?)
        .with_context(|| format!("failed to write {}", schema_path.display()))?;

    let codex_exe = resolve_codex_exe(CodexExeResolutionInputs {
        current_exe: std::env::current_exe().map_err(|err| err.to_string()),
        argv0: std::env::args_os().next().map(PathBuf::from),
        path_entries: std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default(),
    })?;
    eprintln!(
        "{action}: starting nested `codex exec` in {} using {} (timeout {}s)",
        repo_root.display(),
        codex_exe.display(),
        timeout.as_secs()
    );
    let mut command = Command::new(&codex_exe);
    command
        .arg("exec")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--output-schema")
        .arg(&schema_path)
        .arg("--output-last-message")
        .arg(&output_path)
        .arg("-C")
        .arg(&repo_root)
        .arg("-")
        .current_dir(&repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for raw_override in &raw_overrides {
        command.arg("--config").arg(raw_override);
    }
    command.arg("--config").arg("approval_policy=never");
    #[cfg(unix)]
    command.process_group(0);

    let child = command.spawn().with_context(|| {
        format!(
            "failed to spawn {action} with {} for {}",
            codex_exe.display(),
            repo_root.display()
        )
    })?;
    eprintln!(
        "{action}: spawned nested `codex exec` as pid {}",
        child.id()
    );
    let mut exec = ManagedRepoCiExec::new(child);
    let stderr = exec
        .child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("{action} stderr was not available"))?;
    let stderr_reader = spawn_stderr_tee(stderr);
    let mut stdin = exec
        .child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("{action} stdin was not available"))?;
    stdin
        .write_all(prompt.as_bytes())
        .with_context(|| format!("failed to send prompt to {action}"))?;
    drop(stdin);
    eprintln!("{action}: prompt sent; streaming nested activity below");

    let completion = wait_for_repo_ci_exec(&mut exec, &action, timeout)?;
    let stderr = join_stderr_tee(stderr_reader, &action)?;
    let stderr_text = String::from_utf8_lossy(&stderr);
    match completion {
        RepoCiExecCompletion::Finished(status) if status.success() => {}
        RepoCiExecCompletion::Finished(status) => {
            return Err(anyhow!(
                "{action} failed with {status}: {}",
                truncate_for_feedback(&stderr_text, MAX_FEEDBACK_BYTES),
            ));
        }
        RepoCiExecCompletion::TimedOut => {
            return Err(anyhow!(
                "{action} timed out after {}s; terminated nested `codex exec`. stderr:\n{}",
                timeout.as_secs(),
                truncate_for_feedback(&stderr_text, MAX_FEEDBACK_BYTES),
            ));
        }
    }

    fs::read_to_string(&output_path)
        .with_context(|| format!("failed to read {}", output_path.display()))
}

#[derive(Debug)]
struct CodexExeResolutionInputs {
    current_exe: std::result::Result<PathBuf, String>,
    argv0: Option<PathBuf>,
    path_entries: Vec<PathBuf>,
}

fn resolve_codex_exe(inputs: CodexExeResolutionInputs) -> Result<PathBuf> {
    if let Ok(path) = &inputs.current_exe
        && path.is_file()
    {
        return Ok(path.clone());
    }

    let current_exe_description = match &inputs.current_exe {
        Ok(path) => format!("`{}`", path.display()),
        Err(err) => format!("unavailable ({err})"),
    };
    let argv0_path = inputs.argv0.ok_or_else(|| {
        anyhow!(
            "current executable path {current_exe_description} is not usable and process argv[0] is not available"
        )
    })?;
    if argv0_path.is_absolute() || argv0_path.components().count() > 1 {
        if argv0_path.is_file() {
            return absolute_existing_file_path(argv0_path);
        }
        anyhow::bail!(
            "current executable path {current_exe_description} is not usable and argv[0] `{}` is not a file",
            argv0_path.display()
        );
    }

    inputs
        .path_entries
        .into_iter()
        .map(|dir| dir.join(&argv0_path))
        .find(|candidate| candidate.is_file())
        .map(absolute_existing_file_path)
        .transpose()?
        .ok_or_else(|| {
            anyhow!(
                "current executable path {current_exe_description} is not usable and argv[0] `{}` was not found on PATH",
                argv0_path.display()
            )
        })
}

fn absolute_existing_file_path(path: PathBuf) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to resolve executable path {}", path.display()))
}

enum RepoCiExecCompletion {
    Finished(ExitStatus),
    TimedOut,
}

struct ManagedRepoCiExec {
    child: Child,
    completed: bool,
}

impl ManagedRepoCiExec {
    fn new(child: Child) -> Self {
        Self {
            child,
            completed: false,
        }
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let status = self
            .child
            .try_wait()
            .context("failed to wait for nested repo-ci exec")?;
        if status.is_some() {
            self.completed = true;
        }
        Ok(status)
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        let status = self
            .child
            .wait()
            .context("failed to wait for nested repo-ci exec")?;
        self.completed = true;
        Ok(status)
    }

    fn terminate_after_timeout(&mut self) -> Result<()> {
        self.request_termination();
        if self
            .wait_until(Instant::now() + REPO_CI_EXEC_TERMINATION_GRACE)?
            .is_some()
        {
            return Ok(());
        }

        self.force_kill();
        let _ = self.wait();
        Ok(())
    }

    fn wait_until(&mut self, deadline: Instant) -> Result<Option<ExitStatus>> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining == Duration::ZERO {
                return Ok(None);
            }
            sleep_until_next_poll(remaining);
        }
    }

    fn request_termination(&mut self) {
        #[cfg(unix)]
        signal_repo_ci_exec(self.child.id(), "TERM");
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }

    fn force_kill(&mut self) {
        #[cfg(unix)]
        signal_repo_ci_exec(self.child.id(), "KILL");
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }
}

impl Drop for ManagedRepoCiExec {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }

        self.request_termination();
        let deadline = Instant::now() + REPO_CI_EXEC_TERMINATION_GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            sleep_until_next_poll(deadline.saturating_duration_since(Instant::now()));
        }
        self.force_kill();
        let _ = self.child.wait();
    }
}

fn wait_for_repo_ci_exec(
    exec: &mut ManagedRepoCiExec,
    action: &str,
    timeout: Duration,
) -> Result<RepoCiExecCompletion> {
    let started = Instant::now();
    let mut next_progress = REPO_CI_EXEC_PROGRESS_INTERVAL;
    loop {
        if let Some(status) = exec.try_wait()? {
            eprintln!(
                "{action}: nested `codex exec` finished with {status} after {}s",
                started.elapsed().as_secs()
            );
            return Ok(RepoCiExecCompletion::Finished(status));
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            eprintln!(
                "{action}: timed out after {}s; terminating nested `codex exec`",
                timeout.as_secs()
            );
            exec.terminate_after_timeout()?;
            return Ok(RepoCiExecCompletion::TimedOut);
        }

        if elapsed >= next_progress {
            eprintln!(
                "{action}: still running after {}s (timeout {}s)",
                elapsed.as_secs(),
                timeout.as_secs()
            );
            next_progress += REPO_CI_EXEC_PROGRESS_INTERVAL;
        }

        let remaining_timeout = timeout.saturating_sub(elapsed);
        let remaining_progress = next_progress.saturating_sub(elapsed);
        let sleep_for = REPO_CI_EXEC_POLL_INTERVAL
            .min(remaining_timeout)
            .min(remaining_progress);
        if sleep_for != Duration::ZERO {
            thread::sleep(sleep_for);
        }
    }
}

fn spawn_stderr_tee(mut stderr: ChildStderr) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0; 8192];
        let mut parent_stderr = std::io::stderr().lock();
        loop {
            let bytes = stderr.read(&mut buffer)?;
            if bytes == 0 {
                break;
            }
            let chunk = &buffer[..bytes];
            let _ = parent_stderr.write_all(chunk);
            let _ = parent_stderr.flush();
            captured.extend_from_slice(chunk);
        }
        Ok(captured)
    })
}

fn join_stderr_tee(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    action: &str,
) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow!("{action} stderr reader panicked"))?
        .with_context(|| format!("failed to read {action} stderr"))
}

fn sleep_until_next_poll(remaining: Duration) {
    let sleep_for = if remaining < REPO_CI_EXEC_POLL_INTERVAL {
        remaining
    } else {
        REPO_CI_EXEC_POLL_INTERVAL
    };
    if sleep_for != Duration::ZERO {
        thread::sleep(sleep_for);
    }
}

#[cfg(unix)]
fn signal_repo_ci_exec(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn parse_json_payload<T>(text: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = text.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(trimmed);
    Ok(serde_json::from_str(json_text)?)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn truncate_for_feedback_keeps_ends() {
        let truncated = truncate_for_feedback("abcdefghij", 6);
        assert_eq!(truncated, "abc\n...\nhij");
    }

    #[test]
    fn truncate_for_feedback_handles_utf8_boundaries() {
        let truncated = truncate_for_feedback("abé🙂xyz", 7);
        assert_eq!(truncated, "ab\n...\nxyz");
    }

    #[test]
    fn repo_ci_exec_timeout_clamps_to_minimum() {
        assert_eq!(repo_ci_exec_timeout(1), REPO_CI_EXEC_MIN_TIMEOUT);
    }

    #[test]
    fn repo_ci_exec_timeout_allows_normal_budget() {
        assert_eq!(repo_ci_exec_timeout(300), Duration::from_secs(300));
    }

    #[test]
    fn repo_ci_exec_timeout_clamps_to_maximum() {
        assert_eq!(repo_ci_exec_timeout(1_200), REPO_CI_EXEC_MAX_TIMEOUT);
    }

    #[test]
    fn resolve_codex_exe_prefers_existing_current_exe() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let current_exe = tempdir.path().join("current-codex");
        let path_candidate_dir = tempdir.path().join("bin");
        fs::create_dir(&path_candidate_dir)?;
        let path_candidate = path_candidate_dir.join("codex");
        write_file(&current_exe)?;
        write_file(&path_candidate)?;

        let resolved = resolve_codex_exe(CodexExeResolutionInputs {
            current_exe: Ok(current_exe.clone()),
            argv0: Some(PathBuf::from("codex")),
            path_entries: vec![path_candidate_dir],
        })?;

        assert_eq!(resolved, current_exe);
        Ok(())
    }

    #[test]
    fn resolve_codex_exe_falls_back_to_relative_argv0() -> Result<()> {
        let cwd = std::env::current_dir()?;
        let tempdir = tempfile::Builder::new()
            .prefix("repo-ci-exe-test")
            .tempdir_in(&cwd)?;
        let exe = tempdir.path().join("codex");
        write_file(&exe)?;
        let relative_exe = exe.strip_prefix(&cwd)?.to_path_buf();

        let resolved = resolve_codex_exe(CodexExeResolutionInputs {
            current_exe: Ok(tempdir.path().join("deleted-codex")),
            argv0: Some(relative_exe),
            path_entries: Vec::new(),
        })?;

        assert_eq!(resolved, exe.canonicalize()?);
        Ok(())
    }

    #[test]
    fn resolve_codex_exe_falls_back_to_path() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let path_candidate_dir = tempdir.path().join("bin");
        fs::create_dir(&path_candidate_dir)?;
        let path_candidate = path_candidate_dir.join("codex");
        write_file(&path_candidate)?;

        let resolved = resolve_codex_exe(CodexExeResolutionInputs {
            current_exe: Err("current exe unavailable".to_string()),
            argv0: Some(PathBuf::from("codex")),
            path_entries: vec![path_candidate_dir],
        })?;

        assert_eq!(resolved, path_candidate.canonicalize()?);
        Ok(())
    }

    #[test]
    fn resolve_codex_exe_reports_unusable_current_exe_and_missing_argv0() {
        let err = resolve_codex_exe(CodexExeResolutionInputs {
            current_exe: Ok(PathBuf::from("/missing/current-codex")),
            argv0: None,
            path_entries: Vec::new(),
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "current executable path `/missing/current-codex` is not usable and process argv[0] is not available"
        );
    }

    fn write_file(path: &Path) -> Result<()> {
        fs::write(path, b"codex")?;
        Ok(())
    }
}
