use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

use crate::input::DevCycleInput;
use crate::input::TestMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestResult {
    pub(crate) command: String,
    pub(crate) success: bool,
    pub(crate) exit_status: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn run_tests(input: &DevCycleInput, cwd: &Path) -> anyhow::Result<Vec<TestResult>> {
    if input.test_mode == TestMode::Off {
        return Ok(Vec::new());
    }
    if input.test_commands.is_empty() {
        return Ok(vec![TestResult {
            command: "auto".to_string(),
            success: true,
            exit_status: "skipped: no test commands provided".to_string(),
            stdout: String::new(),
            stderr: String::new(),
        }]);
    }

    let mut results = Vec::new();
    for command in &input.test_commands {
        let output = shell_command(command, cwd).output()?;
        results.push(TestResult {
            command: command.clone(),
            success: output.status.success(),
            exit_status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    Ok(results)
}

pub(crate) fn prepare_writer_worktree(
    cwd: &Path,
    state_dir: &Path,
    run_id: &str,
    index: u32,
) -> PathBuf {
    let worktree_dir = state_dir
        .join("worktrees")
        .join(format!("{run_id}-writer-{index}"));
    let _ = std::fs::create_dir_all(worktree_dir.parent().unwrap_or(state_dir));
    let inside_git = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if inside_git {
        let branch = format!("codex-dev-cycle/{run_id}/writer-{index}");
        let status = Command::new("git")
            .args(["worktree", "add", "-B", &branch])
            .arg(&worktree_dir)
            .arg("HEAD")
            .current_dir(cwd)
            .status();
        if status.map(|status| status.success()).unwrap_or(false) {
            return worktree_dir;
        }
    }
    cwd.to_path_buf()
}

fn shell_command(command: &str, cwd: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]).current_dir(cwd);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-lc", command]).current_dir(cwd);
        cmd
    }
}
