use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::sync::Arc;

use anyhow::Context;
use anyhow::bail;
use clap::Parser;
use codex_core_api::AbsolutePathBuf;
use codex_core_api::Arg0DispatchPaths;
use codex_core_api::AskForApproval;
use codex_core_api::AuthManager;
use codex_core_api::CodexThread;
use codex_core_api::CollaborationModesConfig;
use codex_core_api::Config;
use codex_core_api::ConfigBuilder;
use codex_core_api::ConfigOverrides;
use codex_core_api::EnvironmentManager;
use codex_core_api::EnvironmentManagerArgs;
use codex_core_api::EventMsg;
use codex_core_api::ExecServerRuntimePaths;
use codex_core_api::Features;
use codex_core_api::NewThread;
use codex_core_api::Op;
use codex_core_api::PermissionProfile;
use codex_core_api::SessionSource;
use codex_core_api::ThreadManager;
use codex_core_api::UserInput;
use codex_core_api::arg0_dispatch_or_else;
use codex_core_api::find_codex_home;
use codex_core_api::item_event_to_server_notification;
use codex_core_api::set_default_originator;

#[derive(Debug, Parser)]
#[command(
    name = "codex-thread-manager-sample",
    about = "Run one Codex turn through ThreadManager and print mapped notifications as newline-delimited JSON."
)]
struct Args {
    /// Override the model for this run.
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,

    /// Prompt text. If omitted, the prompt is read from piped stdin.
    #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
    prompt: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(run_main)
}

async fn run_main(arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    if let Err(err) = set_default_originator("codex_thread_manager_sample".to_string()) {
        tracing::warn!("failed to set originator: {err:?}");
    }

    let args = Args::parse();
    let prompt = if args.prompt.is_empty() {
        if std::io::stdin().is_terminal() {
            bail!("no prompt provided; pass a prompt argument or pipe one into stdin");
        }

        let mut prompt = String::new();
        std::io::stdin()
            .read_to_string(&mut prompt)
            .context("read prompt from stdin")?;
        let prompt = prompt.replace("\r\n", "\n").replace('\r', "\n");
        if prompt.trim().is_empty() {
            bail!("no prompt provided via stdin");
        }
        prompt
    } else {
        args.prompt.join(" ")
    };

    let config = new_config(args.model, arg0_paths).await?;

    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false);
    let local_runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        config.codex_self_exe.clone(),
        config.codex_linux_sandbox_exe.clone(),
    )?;
    let environment_manager =
        Arc::new(EnvironmentManager::new(EnvironmentManagerArgs::new(local_runtime_paths)).await);
    let thread_manager = ThreadManager::new(
        &config,
        auth_manager,
        SessionSource::Exec,
        CollaborationModesConfig::default(),
        environment_manager,
        /*analytics_events_client*/ None,
    );

    let NewThread {
        thread_id, thread, ..
    } = thread_manager
        .start_thread(config)
        .await
        .context("start Codex thread")?;

    let thread_id_string = thread_id.to_string();
    let turn_output = run_turn(&thread, &thread_id_string, prompt).await;
    let shutdown_result = thread.shutdown_and_wait().await;
    let _ = thread_manager.remove_thread(&thread_id).await;

    turn_output?;
    shutdown_result.context("shut down Codex thread")?;

    Ok(())
}

async fn new_config(
    model: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> anyhow::Result<Config> {
    let codex_home = find_codex_home().context("find Codex home")?;
    let cwd = AbsolutePathBuf::current_dir().context("resolve current directory")?;
    let mut config = ConfigBuilder::default()
        .codex_home(codex_home.to_path_buf())
        .fallback_cwd(Some(cwd.to_path_buf()))
        .harness_overrides(ConfigOverrides {
            model,
            approval_policy: Some(AskForApproval::Never),
            permission_profile: Some(PermissionProfile::read_only()),
            codex_self_exe: arg0_paths.codex_self_exe,
            codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe,
            main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe,
            ephemeral: Some(true),
            ..Default::default()
        })
        .build()
        .await
        .context("load Codex config")?;
    config
        .features
        .set(Features::with_defaults())
        .context("configure default features")?;
    config.analytics_enabled = Some(false);
    config.feedback_enabled = false;
    config.check_for_update_on_startup = false;
    Ok(config)
}

async fn run_turn(thread: &CodexThread, thread_id: &str, prompt: String) -> anyhow::Result<()> {
    thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt,
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await
        .context("submit user input")?;

    let mut current_turn_id: Option<String> = None;
    let mut stdout = std::io::stdout().lock();
    loop {
        let event = thread.next_event().await.context("read Codex event")?;
        let notification = match &event.msg {
            EventMsg::TurnStarted(event) => {
                current_turn_id = Some(event.turn_id.clone());
                None
            }
            EventMsg::DynamicToolCallResponse(_)
            | EventMsg::McpToolCallBegin(_)
            | EventMsg::McpToolCallEnd(_)
            | EventMsg::CollabAgentSpawnBegin(_)
            | EventMsg::CollabAgentSpawnEnd(_)
            | EventMsg::CollabAgentInteractionBegin(_)
            | EventMsg::CollabAgentInteractionEnd(_)
            | EventMsg::CollabWaitingBegin(_)
            | EventMsg::CollabWaitingEnd(_)
            | EventMsg::CollabCloseBegin(_)
            | EventMsg::CollabCloseEnd(_)
            | EventMsg::CollabResumeBegin(_)
            | EventMsg::CollabResumeEnd(_)
            | EventMsg::AgentMessageContentDelta(_)
            | EventMsg::PlanDelta(_)
            | EventMsg::ReasoningContentDelta(_)
            | EventMsg::ReasoningRawContentDelta(_)
            | EventMsg::AgentReasoningSectionBreak(_)
            | EventMsg::ItemStarted(_)
            | EventMsg::ItemCompleted(_)
            | EventMsg::PatchApplyBegin(_)
            | EventMsg::PatchApplyUpdated(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandBegin(_)
            | EventMsg::ExecCommandOutputDelta(_)
            | EventMsg::ExecCommandEnd(_) => Some(item_event_to_server_notification(
                event.msg.clone(),
                thread_id,
                current_turn_id
                    .as_deref()
                    .context("mapped notification arrived before turn started")?,
            )),
            _ => None,
        };
        if let Some(notification) = notification {
            serde_json::to_writer(&mut stdout, &notification)
                .context("serialize mapped notification")?;
            stdout
                .write_all(b"\n")
                .context("write notification newline")?;
            stdout.flush().context("flush notification output")?;
        }

        match event.msg {
            EventMsg::TurnComplete(_) => {
                return Ok(());
            }
            EventMsg::Error(event) => {
                bail!(event.message);
            }
            EventMsg::TurnAborted(_) => {
                bail!("turn aborted");
            }
            EventMsg::ExecApprovalRequest(_) => {
                bail!("turn requested exec approval");
            }
            EventMsg::ApplyPatchApprovalRequest(_) => {
                bail!("turn requested patch approval");
            }
            EventMsg::RequestPermissions(_) => {
                bail!("turn requested permissions");
            }
            EventMsg::RequestUserInput(_) => {
                bail!("turn requested user input");
            }
            EventMsg::DynamicToolCallRequest(_) => {
                bail!("turn requested a dynamic tool call");
            }
            _ => {}
        }
    }
}
