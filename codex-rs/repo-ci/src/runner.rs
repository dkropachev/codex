use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::CapturedExitStatus;
use crate::CapturedRun;
use crate::CapturedStep;
use crate::RepoCiPaths;

const JSONL_ENV: &str = "CODEX_REPO_CI_JSONL";
const REPO_ROOT_ENV: &str = "CODEX_REPO_CI_REPO_ROOT";
const RUNNER_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RUNNER_TERMINATION_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
pub struct RepoCiCancellation {
    cancelled: Arc<AtomicBool>,
}

impl RepoCiCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

pub(crate) fn run_runner(
    paths: &RepoCiPaths,
    arg: &str,
    local_test_time_budget_sec: u64,
) -> Result<ExitStatus> {
    let mut runner = spawn_runner(paths, arg, None, RunnerStdio::Inherit)?;
    match wait_for_runner(
        &mut runner,
        arg,
        local_test_time_budget_sec,
        &RepoCiCancellation::default(),
    )? {
        RunnerCompletion::Finished(status) => Ok(status),
        RunnerCompletion::Stopped { message } => Err(anyhow!(message)),
    }
}

pub(crate) fn capture_runner(
    paths: &RepoCiPaths,
    arg: &str,
    local_test_time_budget_sec: u64,
    cancellation: &RepoCiCancellation,
) -> Result<CapturedRun> {
    let run_dir = paths.state_dir.join("runs");
    fs::create_dir_all(&run_dir)?;
    let now_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros());
    let jsonl_path = run_dir.join(format!("{arg}-{}-{}.jsonl", std::process::id(), now_micros));
    let mut runner = spawn_runner(paths, arg, Some(&jsonl_path), RunnerStdio::Capture)?;
    let stdout = runner
        .child
        .stdout
        .take()
        .context("repo CI stdout pipe was not available")?;
    let stderr = runner
        .child
        .stderr
        .take()
        .context("repo CI stderr pipe was not available")?;
    let stdout_reader = spawn_pipe_reader(stdout);
    let stderr_reader = spawn_pipe_reader(stderr);
    let completion = wait_for_runner(&mut runner, arg, local_test_time_budget_sec, cancellation)?;
    let stdout = join_pipe_reader(stdout_reader, "stdout")?;
    let mut stderr = join_pipe_reader(stderr_reader, "stderr")?;
    let status = match completion {
        RunnerCompletion::Finished(status) => status.into(),
        RunnerCompletion::Stopped { message } => {
            if !stderr.is_empty() && !stderr.ends_with(b"\n") {
                stderr.push(b'\n');
            }
            stderr.extend_from_slice(message.as_bytes());
            stderr.push(b'\n');
            CapturedExitStatus {
                code: None,
                success: false,
            }
        }
    };
    let run = CapturedRun {
        status,
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        steps: read_captured_steps(&jsonl_path)?,
    };
    let _ = fs::remove_file(&jsonl_path);
    Ok(run)
}

#[derive(Clone, Copy)]
enum RunnerStdio {
    Inherit,
    Capture,
}

enum RunnerCompletion {
    Finished(ExitStatus),
    Stopped { message: String },
}

struct ManagedRunner {
    child: Child,
    completed: bool,
    kill_mode: RunnerKillMode,
}

#[derive(Clone, Copy)]
enum RunnerKillMode {
    Process,
    ProcessGroup,
}

impl ManagedRunner {
    fn spawn(mut command: Command, runner_path: &Path, kill_mode: RunnerKillMode) -> Result<Self> {
        let child = command
            .spawn()
            .with_context(|| format!("failed to run {}", runner_path.display()))?;
        Ok(Self {
            child,
            completed: false,
            kill_mode,
        })
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let status = self
            .child
            .try_wait()
            .context("failed to wait for repo CI runner")?;
        if status.is_some() {
            self.completed = true;
        }
        Ok(status)
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        let status = self
            .child
            .wait()
            .context("failed to wait for repo CI runner")?;
        self.completed = true;
        Ok(status)
    }

    fn terminate_after_timeout(&mut self) -> Result<()> {
        self.request_termination();
        if self
            .wait_until(Instant::now() + RUNNER_TERMINATION_GRACE)?
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
        signal_runner(self.child.id(), self.kill_mode, "TERM");
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }

    fn force_kill(&mut self) {
        #[cfg(unix)]
        signal_runner(self.child.id(), self.kill_mode, "KILL");
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }
}

impl Drop for ManagedRunner {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }

        self.request_termination();
        let deadline = Instant::now() + RUNNER_TERMINATION_GRACE;
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

fn spawn_runner(
    paths: &RepoCiPaths,
    arg: &str,
    jsonl_path: Option<&Path>,
    stdio: RunnerStdio,
) -> Result<ManagedRunner> {
    let mut command = Command::new("bash");
    command
        .arg(&paths.runner_path)
        .arg(arg)
        .env(REPO_ROOT_ENV, &paths.repo_root)
        .current_dir(&paths.repo_root);
    if let Some(jsonl_path) = jsonl_path {
        command.env(JSONL_ENV, jsonl_path);
    }
    let kill_mode = match stdio {
        RunnerStdio::Inherit => RunnerKillMode::Process,
        RunnerStdio::Capture => RunnerKillMode::ProcessGroup,
    };
    match stdio {
        RunnerStdio::Inherit => {}
        RunnerStdio::Capture => {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
    }
    #[cfg(unix)]
    if matches!(kill_mode, RunnerKillMode::ProcessGroup) {
        command.process_group(0);
    }
    ManagedRunner::spawn(command, &paths.runner_path, kill_mode)
}

fn wait_for_runner(
    runner: &mut ManagedRunner,
    arg: &str,
    local_test_time_budget_sec: u64,
    cancellation: &RepoCiCancellation,
) -> Result<RunnerCompletion> {
    let timeout = Duration::from_secs(local_test_time_budget_sec.max(1));
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = runner.try_wait()? {
            return Ok(RunnerCompletion::Finished(status));
        }

        if cancellation.is_cancelled() {
            runner.terminate_after_timeout()?;
            return Ok(RunnerCompletion::Stopped {
                message: format!("repo CI {arg} was cancelled; terminated runner process group"),
            });
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining == Duration::ZERO {
            runner.terminate_after_timeout()?;
            return Ok(RunnerCompletion::Stopped {
                message: format!(
                    "repo CI {arg} timed out after {}s; terminated runner process group",
                    timeout.as_secs()
                ),
            });
        }
        sleep_until_next_poll(remaining);
    }
}

fn spawn_pipe_reader<R>(mut reader: R) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        Ok(output)
    })
}

fn join_pipe_reader(
    handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream_name: &str,
) -> Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| anyhow!("repo CI {stream_name} reader panicked"))?
        .with_context(|| format!("failed to read repo CI {stream_name}"))
}

fn sleep_until_next_poll(remaining: Duration) {
    let sleep_for = if remaining < RUNNER_POLL_INTERVAL {
        remaining
    } else {
        RUNNER_POLL_INTERVAL
    };
    if sleep_for != Duration::ZERO {
        thread::sleep(sleep_for);
    }
}

#[cfg(unix)]
fn signal_runner(pid: u32, kill_mode: RunnerKillMode, signal: &str) {
    let target = match kill_mode {
        RunnerKillMode::Process => pid.to_string(),
        RunnerKillMode::ProcessGroup => format!("-{pid}"),
    };
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn read_captured_steps(path: &Path) -> Result<Vec<CapturedStep>> {
    let Ok(data) = fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    data.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("failed to parse repo CI step JSONL"))
        .collect()
}
