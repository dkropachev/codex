use std::env;
use std::fs;
use std::io;
use std::io::BufRead;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitCode;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_repo_ci::AutomationMode;
use codex_repo_ci::LearnOptions;
use codex_repo_ci::RunMode;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

const MAX_OUTPUT_BYTES: usize = 48 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        return Ok(ExitCode::from(2));
    };
    let args: Vec<String> = args.collect();
    match command.as_str() {
        "enable" => set_enabled_state(true),
        "disable" => set_enabled_state(false),
        "trust" => simple_state_command("trusted", "Repo CI trusted this repository."),
        "models" => {
            println!(
                "Repo CI uses the active Codex model unless plugin config supplies triage models."
            );
            Ok(ExitCode::SUCCESS)
        }
        "mode" => mode(&args),
        "status" => status(&args),
        "learn" => learn(&args),
        "prepare" => prepare(&args),
        "run" => run_checks(&args),
        "watch-pr" => watch_pr(&args),
        "hook-user-prompt-submit" => hook_user_prompt_submit(),
        "hook-stop" => hook_stop(),
        _ => {
            print_usage();
            Ok(ExitCode::from(2))
        }
    }
}

fn codex_home() -> Result<PathBuf> {
    if let Some(state_dir) = env::var_os("CODEX_PLUGIN_STATE_DIR") {
        return Ok(PathBuf::from(state_dir));
    }
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("CODEX_PLUGIN_STATE_DIR or CODEX_HOME must be set"))
}

fn cwd(args: &[String]) -> Result<PathBuf> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--cwd" {
            let Some(value) = args.get(index + 1) else {
                bail!("--cwd requires a path");
            };
            return Ok(PathBuf::from(value));
        }
        index += 1;
    }
    env::current_dir().context("failed to read current directory")
}

fn automation(args: &[String]) -> Result<AutomationMode> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--automation" {
            let Some(value) = args.get(index + 1) else {
                bail!("--automation requires a value");
            };
            return match value.as_str() {
                "local" => Ok(AutomationMode::Local),
                "remote" => Ok(AutomationMode::Remote),
                "local-and-remote" | "both" => Ok(AutomationMode::LocalAndRemote),
                _ => bail!("unknown automation mode `{value}`"),
            };
        }
        index += 1;
    }
    Ok(AutomationMode::LocalAndRemote)
}

fn simple_state_command(file_name: &str, message: &str) -> Result<ExitCode> {
    let state_dir = codex_home()?;
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    std::fs::write(state_dir.join(file_name), b"true\n")
        .with_context(|| format!("failed to write {file_name}"))?;
    println!("{message}");
    Ok(ExitCode::SUCCESS)
}

fn set_enabled_state(enabled: bool) -> Result<ExitCode> {
    let state_dir = codex_home()?;
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let (write_file, remove_file, message) = if enabled {
        (
            "enabled",
            "disabled",
            "Repo CI enabled for this plugin state.",
        )
    } else {
        (
            "disabled",
            "enabled",
            "Repo CI disabled for this plugin state.",
        )
    };
    std::fs::write(state_dir.join(write_file), b"true\n")
        .with_context(|| format!("failed to write {write_file}"))?;
    match std::fs::remove_file(state_dir.join(remove_file)) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("failed to remove {remove_file}")),
    }
    println!("{message}");
    Ok(ExitCode::SUCCESS)
}

fn status(args: &[String]) -> Result<ExitCode> {
    let codex_home = codex_home()?;
    let cwd = cwd(args)?;
    let status = codex_repo_ci::status(&codex_home, &cwd)?;
    if let Some(manifest) = status.manifest {
        println!("Repo CI manifest: {}", status.paths.manifest_path.display());
        println!("Automation: {}", manifest.automation.as_str());
        println!("Fast steps: {}", manifest.fast_steps.len());
        println!("Full steps: {}", manifest.full_steps.len());
        if status.stale_sources.is_empty() {
            println!("Learning sources: up to date");
        } else {
            println!("Learning sources: {} stale", status.stale_sources.len());
        }
    } else {
        println!("Repo CI has not learned this repository yet.");
    }
    Ok(ExitCode::SUCCESS)
}

fn learn(args: &[String]) -> Result<ExitCode> {
    let codex_home = codex_home()?;
    let cwd = cwd(args)?;
    let outcome = codex_repo_ci::learn(
        &codex_home,
        &cwd,
        LearnOptions {
            automation: automation(args)?,
            local_test_time_budget_sec: 900,
        },
    )?;
    println!("Repo CI learned {}.", outcome.paths.manifest_path.display());
    Ok(exit_code(outcome.validation_exit_code))
}

fn prepare(args: &[String]) -> Result<ExitCode> {
    let codex_home = codex_home()?;
    let cwd = cwd(args)?;
    Ok(exit_code(codex_repo_ci::prepare(&codex_home, &cwd)?.code()))
}

fn run_checks(args: &[String]) -> Result<ExitCode> {
    let codex_home = codex_home()?;
    let cwd = cwd(args)?;
    let mode = if args.iter().any(|arg| arg == "--full") {
        RunMode::Full
    } else {
        RunMode::Fast
    };
    Ok(exit_code(
        codex_repo_ci::run(&codex_home, &cwd, mode)?.code(),
    ))
}

fn watch_pr(args: &[String]) -> Result<ExitCode> {
    let cwd = cwd(args)?;
    Ok(exit_code(codex_repo_ci::watch_pr(&cwd)?.code()))
}

fn mode(args: &[String]) -> Result<ExitCode> {
    let Some(raw_mode) = args.first() else {
        println!("Usage: codex repo-ci mode <inherit|off|local|remote|local-and-remote>");
        return Ok(ExitCode::from(2));
    };
    let config = match raw_mode.as_str() {
        "inherit" => serde_json::json!({}),
        "off" | "disable" | "disabled" => serde_json::json!({ "enabled": false }),
        "local" => serde_json::json!({ "enabled": true, "automation": "local" }),
        "remote" => serde_json::json!({ "enabled": true, "automation": "remote" }),
        "local-and-remote" | "both" => {
            serde_json::json!({ "enabled": true, "automation": "local-and-remote" })
        }
        _ => bail!("unknown repo-ci mode `{raw_mode}`"),
    };
    if runtime_request(
        "plugin.config.writeSession",
        serde_json::json!({ "config": config }),
    )
    .is_some()
    {
        println!("Repo CI session mode set to {raw_mode}.");
        return Ok(ExitCode::SUCCESS);
    }

    let state_dir = codex_home()?;
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    fs::write(state_dir.join("mode"), raw_mode)
        .with_context(|| format!("failed to write {}", state_dir.join("mode").display()))?;
    println!("Repo CI mode fallback set to {raw_mode}.");
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookInput {
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorktreeSnapshot {
    changed_paths: Vec<String>,
    diff_summary: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoCiConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_automation")]
    automation: AutomationMode,
    #[serde(default = "default_retry_limit")]
    retry_limit: u32,
    #[serde(default = "default_local_test_time_budget_sec")]
    local_test_time_budget_sec: u64,
}

fn default_automation() -> AutomationMode {
    AutomationMode::LocalAndRemote
}

fn default_retry_limit() -> u32 {
    2
}

fn default_local_test_time_budget_sec() -> u64 {
    900
}

fn hook_user_prompt_submit() -> Result<ExitCode> {
    let input = read_hook_input()?;
    let state_dir = codex_home()?;
    let snapshot = WorktreeSnapshot::capture(&input.cwd)?;
    write_json(&snapshot_path(&state_dir, &input), &snapshot)?;
    write_continue();
    Ok(ExitCode::SUCCESS)
}

fn hook_stop() -> Result<ExitCode> {
    let input = read_hook_input()?;
    let state_dir = codex_home()?;
    let config = effective_config(&state_dir)?;
    if !config.enabled {
        write_continue();
        return Ok(ExitCode::SUCCESS);
    }

    let current_snapshot = WorktreeSnapshot::capture(&input.cwd)?;
    let snapshot_path = snapshot_path(&state_dir, &input);
    let initial_snapshot =
        read_json::<WorktreeSnapshot>(&snapshot_path).unwrap_or_else(|_| current_snapshot.clone());
    if current_snapshot == initial_snapshot {
        emit_event(
            &input,
            "status",
            serde_json::json!({
                "phase": "local",
                "state": "skipped",
                "scope": "none",
                "message": "Repo CI skipped because the worktree did not change this turn."
            }),
        );
        write_continue();
        return Ok(ExitCode::SUCCESS);
    }

    if local_enabled(config.automation) {
        emit_status(
            &input,
            "local",
            "started",
            "local",
            "Repo CI local checks started.",
            None,
            None,
        );
        match run_local_checks(&state_dir, &input.cwd, &config)? {
            CheckOutcome::Skipped(message) => {
                emit_status(&input, "local", "skipped", "local", message, None, None);
            }
            CheckOutcome::Passed => {
                reset_retry(&state_dir, &input, "local")?;
                write_json(&snapshot_path, &current_snapshot)?;
                emit_status(
                    &input,
                    "local",
                    "passed",
                    "local",
                    "Repo CI local checks passed.",
                    None,
                    None,
                );
            }
            CheckOutcome::Failed { output } => {
                if let Some(prompt) = retry_or_exhaust(
                    &state_dir,
                    &input,
                    &config,
                    "local",
                    &output,
                    &current_snapshot,
                )? {
                    write_block(&prompt);
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    }

    if remote_enabled(config.automation) {
        emit_status(
            &input,
            "remote",
            "started",
            "remote",
            "Repo CI remote checks started.",
            None,
            None,
        );
        match run_remote_checks(&input.cwd)? {
            CheckOutcome::Skipped(message) => {
                emit_status(&input, "remote", "skipped", "remote", message, None, None);
            }
            CheckOutcome::Passed => {
                reset_retry(&state_dir, &input, "remote")?;
                emit_status(
                    &input,
                    "remote",
                    "passed",
                    "remote",
                    "Repo CI remote checks passed.",
                    None,
                    None,
                );
            }
            CheckOutcome::Failed { output } => {
                if let Some(prompt) = retry_or_exhaust(
                    &state_dir,
                    &input,
                    &config,
                    "remote",
                    &output,
                    &current_snapshot,
                )? {
                    write_block(&prompt);
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    }

    write_continue();
    Ok(ExitCode::SUCCESS)
}

impl WorktreeSnapshot {
    fn capture(cwd: &Path) -> Result<Self> {
        let changed_paths = git_output(cwd, ["status", "--porcelain=v1"])
            .unwrap_or_default()
            .lines()
            .filter_map(status_path)
            .collect::<Vec<_>>();
        let diff_summary = git_output(cwd, ["diff", "--stat"]).unwrap_or_default();
        Ok(Self {
            changed_paths,
            diff_summary,
        })
    }
}

enum CheckOutcome {
    Skipped(String),
    Passed,
    Failed { output: String },
}

fn run_local_checks(state_dir: &Path, cwd: &Path, config: &RepoCiConfig) -> Result<CheckOutcome> {
    let status = codex_repo_ci::status(state_dir, cwd)?;
    if status.manifest.is_none() || !status.stale_sources.is_empty() {
        codex_repo_ci::learn(
            state_dir,
            cwd,
            LearnOptions {
                automation: config.automation,
                local_test_time_budget_sec: config.local_test_time_budget_sec,
            },
        )?;
    }
    let run = codex_repo_ci::run_capture(state_dir, cwd, RunMode::Fast)?;
    if run.status.success {
        Ok(CheckOutcome::Passed)
    } else {
        Ok(CheckOutcome::Failed {
            output: truncate_middle(
                &format!(
                    "local fast runner failed\n\nsteps:\n{}\n\nstdout:\n{}\n\nstderr:\n{}",
                    format_steps(&run.steps),
                    run.stdout,
                    run.stderr
                ),
                MAX_OUTPUT_BYTES,
            ),
        })
    }
}

fn run_remote_checks(cwd: &Path) -> Result<CheckOutcome> {
    if !command_success(Command::new("gh").args(["auth", "status"]).current_dir(cwd)) {
        return Ok(CheckOutcome::Skipped(
            "Repo CI remote checks skipped because `gh auth status` failed.".to_string(),
        ));
    }
    let status = codex_repo_ci::watch_pr(cwd)?;
    if status.success() {
        Ok(CheckOutcome::Passed)
    } else {
        Ok(CheckOutcome::Failed {
            output: "GitHub PR checks failed. Run `codex repo-ci watch-pr --cwd .` for details."
                .to_string(),
        })
    }
}

fn retry_or_exhaust(
    state_dir: &Path,
    input: &HookInput,
    config: &RepoCiConfig,
    scope: &str,
    output: &str,
    snapshot: &WorktreeSnapshot,
) -> Result<Option<String>> {
    let retry_path = retry_path(state_dir, input, scope);
    let attempts = read_retry(&retry_path)?;
    if attempts >= config.retry_limit {
        emit_status(
            input,
            scope,
            "exhausted",
            scope,
            format!("Repo CI {scope} checks are still failing after {attempts} repair attempts."),
            Some(attempts),
            Some(config.retry_limit),
        );
        return Ok(None);
    }
    let next_attempt = attempts.saturating_add(1);
    fs::write(&retry_path, next_attempt.to_string())
        .with_context(|| format!("failed to write {}", retry_path.display()))?;
    emit_status(
        input,
        scope,
        "retrying",
        scope,
        format!(
            "Repo CI {scope} checks failed; starting repair attempt {next_attempt} of {}.",
            config.retry_limit
        ),
        Some(next_attempt),
        Some(config.retry_limit),
    );
    Ok(Some(repair_prompt(
        scope,
        output,
        snapshot,
        next_attempt,
        config.retry_limit,
    )))
}

fn repair_prompt(
    scope: &str,
    output: &str,
    snapshot: &WorktreeSnapshot,
    attempt: u32,
    limit: u32,
) -> String {
    format!(
        "Repo CI {scope} checks failed. Repair the failure, then stop again so Repo CI can rerun.\n\nAttempt: {attempt}/{limit}\nChanged paths:\n{}\n\nDiff summary:\n{}\n\nFailure output:\n{}",
        snapshot.changed_paths.join("\n"),
        snapshot.diff_summary,
        output
    )
}

fn read_hook_input() -> Result<HookInput> {
    let input = io::read_to_string(io::stdin()).context("failed to read hook stdin")?;
    serde_json::from_str(&input).context("failed to parse hook input")
}

fn effective_config(state_dir: &Path) -> Result<RepoCiConfig> {
    let mut value = runtime_request("plugin.config.read", Value::Null).unwrap_or_else(|| {
        serde_json::json!({
            "enabled": state_dir.join("enabled").is_file(),
            "automation": "local-and-remote",
            "retryLimit": 2,
            "localTestTimeBudgetSec": 900
        })
    });
    if state_dir.join("disabled").is_file() {
        value["enabled"] = Value::Bool(false);
    } else if state_dir.join("enabled").is_file() {
        value["enabled"] = Value::Bool(true);
    }
    if let Ok(mode) = fs::read_to_string(state_dir.join("mode")) {
        apply_mode_fallback(&mut value, mode.trim());
    }
    serde_json::from_value(value).context("failed to parse repo-ci plugin config")
}

fn apply_mode_fallback(value: &mut Value, mode: &str) {
    match mode {
        "inherit" => {}
        "off" | "disable" | "disabled" => value["enabled"] = Value::Bool(false),
        "local" | "remote" | "local-and-remote" => {
            value["enabled"] = Value::Bool(true);
            value["automation"] = Value::String(mode.to_string());
        }
        "both" => {
            value["enabled"] = Value::Bool(true);
            value["automation"] = Value::String("local-and-remote".to_string());
        }
        _ => {}
    }
}

fn snapshot_path(state_dir: &Path, input: &HookInput) -> PathBuf {
    hook_state_dir(state_dir, input).join("initial-snapshot.json")
}

fn retry_path(state_dir: &Path, input: &HookInput, scope: &str) -> PathBuf {
    hook_state_dir(state_dir, input).join(format!("{scope}-retry-count"))
}

fn hook_state_dir(state_dir: &Path, input: &HookInput) -> PathBuf {
    state_dir
        .join("hooks")
        .join(sanitize_path_segment(&input.session_id))
        .join(sanitize_path_segment(&input.turn_id))
}

fn read_retry(path: &PathBuf) -> Result<u32> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(value.trim().parse().unwrap_or(0)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn reset_retry(state_dir: &Path, input: &HookInput, scope: &str) -> Result<()> {
    let path = retry_path(state_dir, input, scope);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> Result<T> {
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn write_continue() {
    println!(r#"{{"continue":true,"suppressOutput":true}}"#);
}

fn write_block(reason: &str) {
    println!(
        "{}",
        serde_json::json!({
            "continue": true,
            "suppressOutput": true,
            "decision": "block",
            "reason": reason
        })
    );
}

fn emit_status(
    input: &HookInput,
    phase: &str,
    state: &str,
    scope: &str,
    message: impl Into<String>,
    attempt: Option<u32>,
    max_attempts: Option<u32>,
) {
    emit_event(
        input,
        "status",
        serde_json::json!({
            "phase": phase,
            "state": state,
            "scope": scope,
            "message": message.into(),
            "attempt": attempt,
            "maxAttempts": max_attempts
        }),
    );
}

fn emit_event(input: &HookInput, event: &str, payload: Value) {
    let _ = runtime_request(
        "plugin.event.emit",
        serde_json::json!({
            "threadId": input.session_id,
            "event": event,
            "payload": payload
        }),
    );
}

fn runtime_request(method: &str, params: Value) -> Option<Value> {
    let socket = env::var("CODEX_PLUGIN_RUNTIME_SOCKET").ok()?;
    runtime_request_impl(&socket, method, params).ok()
}

#[cfg(unix)]
fn runtime_request_impl(socket: &str, method: &str, params: Value) -> Result<Value> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket)?;
    let request = serde_json::json!({
        "id": 1,
        "method": method,
        "params": params
    });
    writeln!(stream, "{request}")?;
    let mut response = String::new();
    io::BufReader::new(stream).read_line(&mut response)?;
    let value: Value = serde_json::from_str(&response)?;
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("runtime request failed: {value}"))
}

#[cfg(not(unix))]
fn runtime_request_impl(_socket: &str, _method: &str, _params: Value) -> Result<Value> {
    bail!("plugin runtime socket is not supported on this platform yet")
}

fn command_success(command: &mut Command) -> bool {
    command.status().is_ok_and(|status| status.success())
}

fn local_enabled(automation: AutomationMode) -> bool {
    matches!(
        automation,
        AutomationMode::Local | AutomationMode::LocalAndRemote
    )
}

fn remote_enabled(automation: AutomationMode) -> bool {
    matches!(
        automation,
        AutomationMode::Remote | AutomationMode::LocalAndRemote
    )
}

fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

fn status_path(line: &str) -> Option<String> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    path.rsplit(" -> ").next().map(str::to_string)
}

fn format_steps(steps: &[codex_repo_ci::CapturedStep]) -> String {
    steps
        .iter()
        .map(|step| format!("{} {:?} {:?}", step.id, step.event, step.exit_code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn truncate_middle(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let half = max_bytes / 2;
    format!(
        "{}\n\n... truncated ...\n\n{}",
        &value[..floor_char_boundary(value, half)],
        &value[ceil_char_boundary(value, value.len() - half)..]
    )
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn exit_code(code: Option<i32>) -> ExitCode {
    match code {
        Some(code) => ExitCode::from(code.clamp(0, u8::MAX as i32) as u8),
        None => ExitCode::from(1),
    }
}

fn print_usage() {
    eprintln!(
        "Usage: codex-repo-ci <enable|disable|trust|learn|prepare|status|run|watch-pr|models|mode>"
    );
}
