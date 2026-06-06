use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentMessageEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::UserMessageEvent;
use tempfile::TempDir;
use tempfile::tempdir;
use tokio::select;
use tokio::time::sleep;
use tokio::time::timeout;
use uuid::Uuid;

use super::workflow_test_support::ensure_codex_binary;

const CLI_PREVIEW: &str = "CLI visible resume preview";
const WORKFLOW_PREVIEW: &str = "Workflow hidden resume preview";

struct SeededResumeHome {
    _codex_home: TempDir,
    workspace: TempDir,
    codex_home_path: PathBuf,
    cli_rollout: PathBuf,
    workflow_rollout: PathBuf,
}

#[tokio::test]
async fn resume_picker_hides_workflow_sessions_by_default() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let seeded = seed_resume_home()?;

    let output = run_codex_resume_until(ResumeInvocation {
        codex: &codex,
        repo_root: &repo_root,
        codex_home: seeded.codex_home_path.as_path(),
        workspace: seeded.workspace.path(),
        resume_args: &[],
        required_snippets: &[CLI_PREVIEW],
        forbidden_snippets: &[WORKFLOW_PREVIEW],
        prompt: None,
    })
    .await?;

    assert!(
        output.contains(CLI_PREVIEW),
        "default picker did not show CLI session: {output}"
    );
    assert!(
        !output.contains(WORKFLOW_PREVIEW),
        "default picker showed workflow session: {output}"
    );

    Ok(())
}

#[tokio::test]
async fn resume_picker_can_include_workflow_sessions() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let seeded = seed_resume_home()?;

    let output = run_codex_resume_until(ResumeInvocation {
        codex: &codex,
        repo_root: &repo_root,
        codex_home: seeded.codex_home_path.as_path(),
        workspace: seeded.workspace.path(),
        resume_args: &["--include-non-interactive"],
        required_snippets: &[WORKFLOW_PREVIEW],
        forbidden_snippets: &[],
        prompt: None,
    })
    .await?;

    assert!(
        output.contains(WORKFLOW_PREVIEW),
        "include-non-interactive picker did not show workflow session: {output}"
    );

    Ok(())
}

#[tokio::test]
async fn resume_last_ignores_newer_workflow_session_by_default() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let fixture = codex_utils_cargo_bin::find_resource!("../core/tests/cli_responses_fixture.sse")?;
    let seeded = seed_resume_home()?;
    let marker = format!("default-last-marker-{}", Uuid::new_v4());

    let output = run_codex_resume_until(ResumeInvocation {
        codex: &codex,
        repo_root: &repo_root,
        codex_home: seeded.codex_home_path.as_path(),
        workspace: seeded.workspace.path(),
        resume_args: &["--last"],
        required_snippets: &[CLI_PREVIEW],
        forbidden_snippets: &[WORKFLOW_PREVIEW],
        prompt: Some(ResumePrompt {
            text: marker.clone(),
            fixture_path: fixture.as_path(),
        }),
    })
    .await?;

    assert!(
        output.contains(CLI_PREVIEW),
        "default --last did not resume CLI session: {output}"
    );
    assert_rollout_contains(&seeded.cli_rollout, &marker)?;
    assert_rollout_not_contains(&seeded.workflow_rollout, &marker)?;

    Ok(())
}

#[tokio::test]
async fn resume_last_can_include_newer_workflow_session() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = ensure_codex_binary(&repo_root)?;
    let fixture = codex_utils_cargo_bin::find_resource!("../core/tests/cli_responses_fixture.sse")?;
    let seeded = seed_resume_home()?;
    let marker = format!("include-last-marker-{}", Uuid::new_v4());

    let output = run_codex_resume_until(ResumeInvocation {
        codex: &codex,
        repo_root: &repo_root,
        codex_home: seeded.codex_home_path.as_path(),
        workspace: seeded.workspace.path(),
        resume_args: &["--include-non-interactive", "--last"],
        required_snippets: &[WORKFLOW_PREVIEW],
        forbidden_snippets: &[],
        prompt: Some(ResumePrompt {
            text: marker.clone(),
            fixture_path: fixture.as_path(),
        }),
    })
    .await?;

    assert!(
        output.contains(WORKFLOW_PREVIEW),
        "include-non-interactive --last did not resume workflow session: {output}"
    );
    assert_rollout_contains(&seeded.workflow_rollout, &marker)?;
    assert_rollout_not_contains(&seeded.cli_rollout, &marker)?;

    Ok(())
}

struct ResumePrompt<'a> {
    text: String,
    fixture_path: &'a Path,
}

struct ResumeInvocation<'a> {
    codex: &'a Path,
    repo_root: &'a Path,
    codex_home: &'a Path,
    workspace: &'a Path,
    resume_args: &'a [&'a str],
    required_snippets: &'a [&'a str],
    forbidden_snippets: &'a [&'a str],
    prompt: Option<ResumePrompt<'a>>,
}

async fn run_codex_resume_until(invocation: ResumeInvocation<'_>) -> Result<String> {
    let ResumeInvocation {
        codex,
        repo_root,
        codex_home,
        workspace,
        resume_args,
        required_snippets,
        forbidden_snippets,
        prompt,
    } = invocation;
    let mut env = HashMap::new();
    env.insert("CODEX_HOME".to_string(), codex_home.display().to_string());
    env.insert("OPENAI_API_KEY".to_string(), "dummy".to_string());
    if let Some(prompt) = prompt.as_ref() {
        env.insert(
            "CODEX_RS_SSE_FIXTURE".to_string(),
            prompt.fixture_path.display().to_string(),
        );
    }

    let mut args = vec!["resume".to_string()];
    args.extend(resume_args.iter().map(|arg| (*arg).to_string()));
    args.extend([
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ]);

    let spawned = codex_utils_pty::spawn_pty_process(
        codex.to_string_lossy().as_ref(),
        &args,
        repo_root,
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
    )
    .await?;

    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let mut output = Vec::new();
    let mut prompt_sent = false;
    let mut interrupt_sent = false;

    let exit_code_result = timeout(Duration::from_secs(30), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                        }
                        output.extend_from_slice(&chunk);
                        let output_text = String::from_utf8_lossy(&output);

                        if let Some(forbidden) = forbidden_snippets
                            .iter()
                            .find(|snippet| output_text.contains(**snippet))
                        {
                            anyhow::bail!("forbidden snippet {forbidden:?} appeared in output: {output_text}");
                        }

                        let saw_required = required_snippets
                            .iter()
                            .all(|snippet| output_text.contains(*snippet));
                        if saw_required && prompt.is_none() && !interrupt_sent {
                            interrupt_sent = true;
                            send_interrupts(writer_tx.clone()).await;
                        }

                        if saw_required
                            && let Some(prompt) = prompt.as_ref()
                            && !prompt_sent
                        {
                            prompt_sent = true;
                            let prompt_text = prompt.text.clone();
                            let prompt_writer = writer_tx.clone();
                            tokio::spawn(async move {
                                sleep(Duration::from_millis(300)).await;
                                let _ = prompt_writer.send(prompt_text.into_bytes()).await;
                                sleep(Duration::from_millis(100)).await;
                                let _ = prompt_writer.send(b"\r".to_vec()).await;
                            });
                        }

                        if prompt_sent
                            && !interrupt_sent
                            && let Some(prompt) = prompt.as_ref()
                            && rollout_tree_contains(codex_home, &prompt.text)?
                        {
                            interrupt_sent = true;
                            send_interrupts(writer_tx.clone()).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break Ok(exit_rx.await),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                result = &mut exit_rx => break Ok(result),
            }
        }
    })
    .await;

    let exit_result = match exit_code_result {
        Ok(result) => result?,
        Err(_) => {
            session.terminate();
            anyhow::bail!(
                "timed out waiting for codex resume; output: {}",
                String::from_utf8_lossy(&output)
            );
        }
    };
    if let Err(err) = exit_result {
        return Err(err.into());
    }
    while let Ok(chunk) = output_rx.try_recv() {
        output.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&output).to_string())
}

async fn send_interrupts(writer_tx: tokio::sync::mpsc::Sender<Vec<u8>>) {
    for _ in 0..4 {
        let _ = writer_tx.send(vec![3]).await;
        sleep(Duration::from_millis(150)).await;
    }
}

fn seed_resume_home() -> Result<SeededResumeHome> {
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    std::fs::create_dir_all(workspace.path().join(".git"))?;
    write_config(codex_home.path(), workspace.path())?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;

    let cli_id = Uuid::from_u128(0x11111111111111111111111111111111);
    let workflow_id = Uuid::from_u128(0x22222222222222222222222222222222);
    let cli_rollout = write_rollout(
        codex_home.path(),
        workspace.path(),
        cli_id,
        "2026-06-05T10:00:00Z",
        "2026-06-05T10-00-00",
        SessionSource::Cli,
        CLI_PREVIEW,
    )?;
    std::thread::sleep(Duration::from_millis(50));
    let workflow_rollout = write_rollout(
        codex_home.path(),
        workspace.path(),
        workflow_id,
        "2026-06-05T10:00:01Z",
        "2026-06-05T10-00-01",
        SessionSource::SubAgent(SubAgentSource::Other("workflow".to_string())),
        WORKFLOW_PREVIEW,
    )?;

    let codex_home_path = codex_home.path().to_path_buf();
    Ok(SeededResumeHome {
        _codex_home: codex_home,
        workspace,
        codex_home_path,
        cli_rollout,
        workflow_rollout,
    })
}

fn write_config(codex_home: &Path, workspace: &Path) -> Result<()> {
    let config_contents = format!(
        r#"model = "gpt-5.1"
model_provider = "openai"
check_for_update_on_startup = false
suppress_unstable_features_warning = true

[analytics]
enabled = false

[tui]
show_tooltips = false

[projects."{workspace}"]
trust_level = "trusted"
"#,
        workspace = workspace.display(),
    );
    std::fs::write(codex_home.join("config.toml"), config_contents)?;
    Ok(())
}

fn write_rollout(
    codex_home: &Path,
    workspace: &Path,
    id: Uuid,
    timestamp: &str,
    filename_timestamp: &str,
    source: SessionSource,
    preview: &str,
) -> Result<PathBuf> {
    let sessions_dir = codex_home.join("sessions/2026/06/05");
    std::fs::create_dir_all(&sessions_dir)?;
    let path = sessions_dir.join(format!("rollout-{filename_timestamp}-{id}.jsonl"));
    let thread_id = ThreadId::from_string(&id.to_string()).context("valid thread id")?;
    let lines = [
        RolloutLine {
            timestamp: timestamp.to_string(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    id: thread_id,
                    timestamp: timestamp.to_string(),
                    cwd: workspace.to_path_buf(),
                    originator: "codex-test".to_string(),
                    cli_version: "0.0.0-test".to_string(),
                    source,
                    model_provider: Some("openai".to_string()),
                    ..SessionMeta::default()
                },
                git: None,
            }),
        },
        RolloutLine {
            timestamp: timestamp.to_string(),
            item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: preview.to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            })),
        },
        RolloutLine {
            timestamp: timestamp.to_string(),
            item: RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
                message: format!("Completed {preview}"),
                phase: None,
                memory_citation: None,
            })),
        },
    ];
    let mut contents = String::new();
    for line in lines {
        contents.push_str(&serde_json::to_string(&line)?);
        contents.push('\n');
    }
    std::fs::write(&path, contents)?;
    Ok(path)
}

fn rollout_tree_contains(codex_home: &Path, needle: &str) -> Result<bool> {
    let sessions_dir = codex_home.join("sessions");
    if !sessions_dir.exists() {
        return Ok(false);
    }
    directory_contains_jsonl_text(sessions_dir.as_path(), needle)
}

fn directory_contains_jsonl_text(dir: &Path, needle: &str) -> Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if directory_contains_jsonl_text(path.as_path(), needle)? {
                return Ok(true);
            }
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            && std::fs::read_to_string(path)?.contains(needle)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn assert_rollout_contains(path: &Path, needle: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    anyhow::ensure!(
        content.contains(needle),
        "rollout {} did not contain {needle:?}",
        path.display()
    );
    Ok(())
}

fn assert_rollout_not_contains(path: &Path, needle: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    anyhow::ensure!(
        !content.contains(needle),
        "rollout {} unexpectedly contained {needle:?}",
        path.display()
    );
    Ok(())
}
