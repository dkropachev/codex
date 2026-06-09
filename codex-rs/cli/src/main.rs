use chrono::Utc;
use clap::Args;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::Shell;
use clap_complete::generate;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_chatgpt::apply_command::ApplyCommand;
use codex_chatgpt::apply_command::run_apply_command;
use codex_cli::LandlockCommand;
use codex_cli::SeatbeltCommand;
use codex_cli::WindowsCommand;
use codex_cli::read_agent_identity_from_stdin;
use codex_cli::read_api_key_from_stdin;
use codex_cli::run_list_accounts;
use codex_cli::run_login_status;
use codex_cli::run_login_with_account_refresh;
use codex_cli::run_login_with_agent_identity;
use codex_cli::run_login_with_api_key;
use codex_cli::run_login_with_chatgpt;
use codex_cli::run_login_with_device_code;
use codex_cli::run_logout;
use codex_cloud_tasks::Cli as CloudTasksCli;
use codex_exec::Cli as ExecCli;
use codex_exec::Command as ExecCommand;
use codex_exec::ReviewArgs;
use codex_execpolicy::ExecPolicyCheckCommand;
use codex_responses_api_proxy::Args as ResponsesApiProxyArgs;
use codex_rollout_trace::REDUCED_STATE_FILE_NAME;
use codex_rollout_trace::replay_bundle;
use codex_state::StateRuntime;
use codex_state::state_db_path;
use codex_tui::AppExitInfo;
use codex_tui::Cli as TuiCli;
use codex_tui::ExitReason;
use codex_tui::UpdateAction;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cli::CliConfigOverrides;
use owo_colors::OwoColorize;
use serde::Serialize;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use supports_color::Stream;

mod account_usage;
mod api_catalog_cmd;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod app_cmd;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod desktop_app;
mod marketplace_cmd;
mod mcp_cmd;
mod native_workflow_cmd;
mod workflow_cmd;
mod workflow_quality_hook_cmd;
#[cfg(not(windows))]
mod wsl_paths;

use crate::api_catalog_cmd::ApiCatalogCli;
use crate::api_catalog_cmd::run_api_catalog_command;
use crate::marketplace_cmd::MarketplaceCli;
use crate::mcp_cmd::McpCli;
use crate::native_workflow_cmd::NativeWorkflowCli;
use crate::native_workflow_cmd::run_native_workflow_command;
use crate::workflow_cmd::WorkflowCli;
use crate::workflow_cmd::load_workflow_command_context;
use crate::workflow_cmd::run_workflow_command;
use crate::workflow_quality_hook_cmd::run_workflow_quality_hook;

use codex_config::LoaderOverrides;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_core::build_models_manager;
use codex_core::clear_memory_roots_contents;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config::find_codex_home;
use codex_core::model_router_candidate_pool_for_config;
use codex_features::FEATURES;
use codex_features::Stage;
use codex_features::is_known_feature_key;
use codex_login::AuthManager;
use codex_models_manager::bundled_models_response;
use codex_models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::user_input::UserInput;
use codex_terminal_detection::TerminalName;

fn promote_workflow_alias_from_prompt(
    prompt: &mut Vec<String>,
    workflows: &[codex_workflows::WorkflowSummary],
) -> Option<Subcommand> {
    let first_token = prompt.first()?;
    codex_workflows::find_workflow_by_command(workflows, first_token).map(|_| {
        let args = std::mem::take(prompt);
        Subcommand::Workflow(WorkflowCli {
            stage_session_id: None,
            args,
        })
    })
}

/// Codex CLI
///
/// If no subcommand is specified, options will be forwarded to the interactive CLI.
#[derive(Debug, Parser)]
#[clap(
    author,
    version,
    // If a sub‑command is given, ignore requirements of the default args.
    subcommand_negates_reqs = true,
    // The executable is sometimes invoked via a platform‑specific name like
    // `codex-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `codex` command name that users run.
    bin_name = "codex",
    override_usage = "codex [OPTIONS] [PROMPT]\n       codex [OPTIONS] <COMMAND> [ARGS]"
)]
struct MultitoolCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    pub feature_toggles: FeatureToggles,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    interactive: TuiCli,

    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

async fn cli_main(arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    let (cli, mut deferred_parse_error) = match parse_multitool_cli_from(std::env::args_os()) {
        Ok(parsed) => parsed,
        Err(err) => err.exit(),
    };
    let MultitoolCli {
        config_overrides: mut root_config_overrides,
        feature_toggles,
        remote,
        mut interactive,
        mut subcommand,
    } = cli;

    // Fold --enable/--disable into config overrides so they flow to all subcommands.
    let toggle_overrides = feature_toggles.to_overrides()?;
    root_config_overrides.raw_overrides.extend(toggle_overrides);
    let root_remote = remote.remote;
    let root_remote_auth_token_env = remote.remote_auth_token_env;

    if subcommand.is_none() && !interactive.prompt.is_empty() {
        let (_config, workflows) = load_workflow_command_context(
            root_config_overrides.clone(),
            interactive.config_profile.clone(),
            interactive.shared.cwd.clone(),
            arg0_paths.clone(),
            /*stage_session_id*/ None,
        )
        .await?;
        if let Some(workflow_subcommand) =
            promote_workflow_alias_from_prompt(&mut interactive.prompt, &workflows)
        {
            subcommand = Some(workflow_subcommand);
            deferred_parse_error = None;
        }
    }
    if let Some(err) = deferred_parse_error {
        err.exit();
    }

    match subcommand {
        None => {
            prepend_config_flags(
                &mut interactive.config_overrides,
                root_config_overrides.clone(),
            );
            let exit_info = run_interactive_tui(
                interactive,
                root_remote.clone(),
                root_remote_auth_token_env.clone(),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Exec(mut exec_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "exec",
            )?;
            exec_cli
                .shared
                .inherit_exec_root_options(&interactive.shared);
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            codex_exec::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::Review(review_args)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "review",
            )?;
            let mut exec_cli = ExecCli::try_parse_from(["codex", "exec"])?;
            exec_cli.command = Some(ExecCommand::Review(review_args));
            prepend_config_flags(
                &mut exec_cli.config_overrides,
                root_config_overrides.clone(),
            );
            codex_exec::run_main(exec_cli, arg0_paths.clone()).await?;
        }
        Some(Subcommand::McpServer) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "mcp-server",
            )?;
            codex_mcp_server::run_main(arg0_paths.clone(), root_config_overrides).await?;
        }
        Some(Subcommand::Mcp(mut mcp_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "mcp",
            )?;
            // Propagate any root-level config overrides (e.g. `-c key=value`).
            prepend_config_flags(&mut mcp_cli.config_overrides, root_config_overrides.clone());
            mcp_cli.run().await?;
        }
        Some(Subcommand::Plugin(plugin_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "plugin",
            )?;
            let PluginCli {
                mut config_overrides,
                subcommand,
            } = plugin_cli;
            prepend_config_flags(&mut config_overrides, root_config_overrides.clone());
            match subcommand {
                PluginSubcommand::Marketplace(mut marketplace_cli) => {
                    prepend_config_flags(&mut marketplace_cli.config_overrides, config_overrides);
                    marketplace_cli.run().await?;
                }
            }
        }
        Some(Subcommand::Api(api_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "api",
            )?;
            run_api_catalog_command(
                api_cli,
                root_config_overrides,
                interactive.config_profile.clone(),
                arg0_paths.clone(),
            )
            .await?;
        }
        Some(Subcommand::Workflow(workflow_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "workflow",
            )?;
            run_workflow_command(
                workflow_cli,
                root_config_overrides,
                interactive.config_profile.clone(),
                interactive.shared.cwd.clone(),
                arg0_paths.clone(),
            )
            .await?;
        }
        Some(Subcommand::NativeWorkflow(native_workflow_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "native-workflow",
            )?;
            run_native_workflow_command(
                native_workflow_cli,
                root_config_overrides,
                interactive.config_profile.clone(),
                interactive.shared.cwd.clone(),
                arg0_paths.clone(),
            )
            .await?;
        }
        Some(Subcommand::WorkflowQualityHook) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "workflow-quality-hook",
            )?;
            run_workflow_quality_hook()?;
        }
        Some(Subcommand::ToolRouter(tool_router_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "tool-router",
            )?;
            match tool_router_cli.subcommand {
                ToolRouterSubcommand::Tune(cmd) => {
                    run_tool_router_tune_command(cmd, root_config_overrides).await?;
                }
            }
        }
        Some(Subcommand::ModelRouter(model_router_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "model-router",
            )?;
            run_model_router_command(model_router_cli, root_config_overrides).await?;
        }
        Some(Subcommand::AppServer(app_server_cli)) => {
            let AppServerCommand {
                subcommand,
                listen,
                analytics_default_enabled,
                auth,
            } = app_server_cli;
            reject_remote_mode_for_app_server_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                subcommand.as_ref(),
            )?;
            match subcommand {
                None => {
                    let transport = listen;
                    let auth = auth.try_into_settings()?;
                    codex_app_server::run_main_with_transport(
                        arg0_paths.clone(),
                        root_config_overrides,
                        LoaderOverrides::default(),
                        analytics_default_enabled,
                        transport,
                        codex_protocol::protocol::SessionSource::VSCode,
                        auth,
                    )
                    .await?;
                }
                Some(AppServerSubcommand::Proxy(proxy_cli)) => {
                    let socket_path = match proxy_cli.socket_path {
                        Some(socket_path) => socket_path,
                        None => {
                            let codex_home = find_codex_home()?;
                            codex_app_server::app_server_control_socket_path(&codex_home)?
                        }
                    };
                    codex_stdio_to_uds::run(socket_path.as_path()).await?;
                }
                Some(AppServerSubcommand::GenerateTs(gen_cli)) => {
                    let options = codex_app_server_protocol::GenerateTsOptions {
                        experimental_api: gen_cli.experimental,
                        ..Default::default()
                    };
                    codex_app_server_protocol::generate_ts_with_options(
                        &gen_cli.out_dir,
                        gen_cli.prettier.as_deref(),
                        options,
                    )?;
                }
                Some(AppServerSubcommand::GenerateJsonSchema(gen_cli)) => {
                    codex_app_server_protocol::generate_json_with_experimental(
                        &gen_cli.out_dir,
                        gen_cli.experimental,
                    )?;
                }
                Some(AppServerSubcommand::GenerateInternalJsonSchema(gen_cli)) => {
                    codex_app_server_protocol::generate_internal_json_schema(&gen_cli.out_dir)?;
                }
            }
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Some(Subcommand::App(app_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "app",
            )?;
            app_cmd::run_app(app_cli).await?;
        }
        Some(Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            include_non_interactive,
            remote,
            config_overrides,
        })) => {
            interactive = finalize_resume_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                include_non_interactive,
                config_overrides,
            );
            let exit_info = run_interactive_tui(
                interactive,
                remote.remote.or(root_remote.clone()),
                remote
                    .remote_auth_token_env
                    .or(root_remote_auth_token_env.clone()),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            remote,
            config_overrides,
        })) => {
            interactive = finalize_fork_interactive(
                interactive,
                root_config_overrides.clone(),
                session_id,
                last,
                all,
                config_overrides,
            );
            let exit_info = run_interactive_tui(
                interactive,
                remote.remote.or(root_remote.clone()),
                remote
                    .remote_auth_token_env
                    .or(root_remote_auth_token_env.clone()),
                arg0_paths.clone(),
            )
            .await?;
            handle_app_exit(exit_info)?;
        }
        Some(Subcommand::Login(mut login_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "login",
            )?;
            prepend_config_flags(
                &mut login_cli.config_overrides,
                root_config_overrides.clone(),
            );
            match login_cli.action {
                Some(LoginSubcommand::Status) => {
                    run_login_status(login_cli.config_overrides).await;
                }
                None => {
                    if login_cli.with_api_key && login_cli.with_agent_identity {
                        eprintln!(
                            "Choose one login credential source: --with-api-key or --with-agent-identity."
                        );
                        std::process::exit(1);
                    } else if login_cli.use_device_code {
                        run_login_with_device_code(
                            login_cli.config_overrides,
                            login_cli.account,
                            login_cli.issuer_base_url,
                            login_cli.client_id,
                        )
                        .await;
                    } else if login_cli.api_key.is_some() {
                        eprintln!(
                            "The --api-key flag is no longer supported. Pipe the key instead, e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`."
                        );
                        std::process::exit(1);
                    } else if login_cli.with_api_key {
                        if login_cli.account.is_some() {
                            eprintln!("--account cannot be used with --with-api-key");
                            std::process::exit(1);
                        }
                        let api_key = read_api_key_from_stdin();
                        run_login_with_api_key(login_cli.config_overrides, api_key).await;
                    } else if login_cli.with_agent_identity {
                        let agent_identity = read_agent_identity_from_stdin();
                        run_login_with_agent_identity(login_cli.config_overrides, agent_identity)
                            .await;
                    } else {
                        run_login_with_chatgpt(login_cli.config_overrides, login_cli.account).await;
                    }
                }
            }
        }
        Some(Subcommand::Logout(mut logout_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "logout",
            )?;
            prepend_config_flags(
                &mut logout_cli.config_overrides,
                root_config_overrides.clone(),
            );
            run_logout(
                logout_cli.config_overrides,
                logout_cli.account,
                logout_cli.all,
            )
            .await;
        }
        Some(Subcommand::Account(mut account_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "account",
            )?;
            prepend_config_flags(
                &mut account_cli.config_overrides,
                root_config_overrides.clone(),
            );
            match account_cli.subcommand {
                AccountSubcommand::List(list) => {
                    run_list_accounts(account_cli.config_overrides, list.json).await;
                }
                AccountSubcommand::Limits => {
                    account_usage::run_account_limits(account_cli.config_overrides).await?;
                }
                AccountSubcommand::Refresh(refresh) => {
                    run_login_with_account_refresh(
                        account_cli.config_overrides,
                        refresh.id,
                        refresh.pool,
                    )
                    .await;
                }
            }
        }
        Some(Subcommand::Completion(completion_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "completion",
            )?;
            print_completion(completion_cli);
        }
        Some(Subcommand::Update) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "update",
            )?;
            run_update_command()?;
        }
        Some(Subcommand::Cloud(mut cloud_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "cloud",
            )?;
            prepend_config_flags(
                &mut cloud_cli.config_overrides,
                root_config_overrides.clone(),
            );
            codex_cloud_tasks::run_main(cloud_cli, arg0_paths.codex_linux_sandbox_exe.clone())
                .await?;
        }
        Some(Subcommand::Sandbox(sandbox_args)) => match sandbox_args.cmd {
            SandboxCommand::Macos(mut seatbelt_cli) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "sandbox macos",
                )?;
                prepend_config_flags(
                    &mut seatbelt_cli.config_overrides,
                    root_config_overrides.clone(),
                );
                codex_cli::run_command_under_seatbelt(
                    seatbelt_cli,
                    arg0_paths.codex_linux_sandbox_exe.clone(),
                )
                .await?;
            }
            SandboxCommand::Linux(mut landlock_cli) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "sandbox linux",
                )?;
                prepend_config_flags(
                    &mut landlock_cli.config_overrides,
                    root_config_overrides.clone(),
                );
                codex_cli::run_command_under_landlock(
                    landlock_cli,
                    arg0_paths.codex_linux_sandbox_exe.clone(),
                )
                .await?;
            }
            SandboxCommand::Windows(mut windows_cli) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "sandbox windows",
                )?;
                prepend_config_flags(
                    &mut windows_cli.config_overrides,
                    root_config_overrides.clone(),
                );
                codex_cli::run_command_under_windows(
                    windows_cli,
                    arg0_paths.codex_linux_sandbox_exe.clone(),
                )
                .await?;
            }
        },
        Some(Subcommand::Debug(DebugCommand { subcommand })) => match subcommand {
            DebugSubcommand::Models(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug models",
                )?;
                run_debug_models_command(cmd, root_config_overrides).await?;
            }
            DebugSubcommand::AppServer(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug app-server",
                )?;
                run_debug_app_server_command(cmd).await?;
            }
            DebugSubcommand::PromptInput(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug prompt-input",
                )?;
                run_debug_prompt_input_command(
                    cmd,
                    root_config_overrides,
                    interactive,
                    arg0_paths.clone(),
                )
                .await?;
            }
            DebugSubcommand::TraceReduce(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug trace-reduce",
                )?;
                run_debug_trace_reduce_command(cmd).await?;
            }
            DebugSubcommand::ClearMemories => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "debug clear-memories",
                )?;
                run_debug_clear_memories_command(&root_config_overrides, &interactive).await?;
            }
        },
        Some(Subcommand::Execpolicy(ExecpolicyCommand { sub })) => match sub {
            ExecpolicySubcommand::Check(cmd) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "execpolicy check",
                )?;
                run_execpolicycheck(cmd)?
            }
        },
        Some(Subcommand::Apply(mut apply_cli)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "apply",
            )?;
            prepend_config_flags(
                &mut apply_cli.config_overrides,
                root_config_overrides.clone(),
            );
            run_apply_command(apply_cli, /*cwd*/ None).await?;
        }
        Some(Subcommand::ResponsesApiProxy(args)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "responses-api-proxy",
            )?;
            tokio::task::spawn_blocking(move || codex_responses_api_proxy::run_main(args))
                .await??;
        }
        Some(Subcommand::StdioToUds(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "stdio-to-uds",
            )?;
            let socket_path = cmd.socket_path;
            codex_stdio_to_uds::run(socket_path.as_path()).await?;
        }
        Some(Subcommand::McpBroker(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "mcp-broker",
            )?;
            codex_mcp::run_mcp_broker(cmd.socket).await?;
        }
        Some(Subcommand::ExecServer(cmd)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "exec-server",
            )?;
            run_exec_server_command(cmd, &arg0_paths).await?;
        }
        Some(Subcommand::Features(FeaturesCli { sub })) => match sub {
            FeaturesSubcommand::List => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features list",
                )?;
                let mut cli_kv_overrides = root_config_overrides
                    .parse_overrides()
                    .map_err(anyhow::Error::msg)?;

                if interactive.web_search {
                    cli_kv_overrides.push((
                        "web_search".to_string(),
                        toml::Value::String("live".to_string()),
                    ));
                }

                let overrides = ConfigOverrides {
                    config_profile: interactive.config_profile.clone(),
                    ..Default::default()
                };

                let config = Config::load_with_cli_overrides_and_harness_overrides(
                    cli_kv_overrides,
                    overrides,
                )
                .await?;
                let mut rows = Vec::with_capacity(FEATURES.len());
                let mut name_width = 0;
                let mut stage_width = 0;
                for def in FEATURES {
                    let name = def.key;
                    let stage = stage_str(def.stage);
                    let enabled = config.features.enabled(def.id);
                    name_width = name_width.max(name.len());
                    stage_width = stage_width.max(stage.len());
                    rows.push((name, stage, enabled));
                }
                rows.sort_unstable_by_key(|(name, _, _)| *name);

                for (name, stage, enabled) in rows {
                    println!("{name:<name_width$}  {stage:<stage_width$}  {enabled}");
                }
            }
            FeaturesSubcommand::Enable(FeatureSetArgs { feature }) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features enable",
                )?;
                enable_feature_in_config(&interactive, &feature).await?;
            }
            FeaturesSubcommand::Disable(FeatureSetArgs { feature }) => {
                reject_remote_mode_for_subcommand(
                    root_remote.as_deref(),
                    root_remote_auth_token_env.as_deref(),
                    "features disable",
                )?;
                disable_feature_in_config(&interactive, &feature).await?;
            }
        },
        Some(Subcommand::Implement(args)) => {
            reject_remote_mode_for_subcommand(
                root_remote.as_deref(),
                root_remote_auth_token_env.as_deref(),
                "implement",
            )?;
            run_implement_command(args).await?;
        }
    }

    Ok(())
}

fn parse_multitool_cli_from<I, T>(
    args: I,
) -> Result<(MultitoolCli, Option<clap::Error>), clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    match MultitoolCli::try_parse_from(args.clone()) {
        Ok(cli) => Ok((cli, None)),
        Err(err) if err.kind() == clap::error::ErrorKind::UnknownArgument => {
            match try_parse_prompt_passthrough_args(&args) {
                Some(cli) => Ok((cli, Some(err))),
                None => Err(err),
            }
        }
        Err(err) => Err(err),
    }
}

fn try_parse_prompt_passthrough_args(args: &[OsString]) -> Option<MultitoolCli> {
    let passthrough_index = first_prompt_passthrough_arg(args)?;
    let mut passthrough_args = Vec::with_capacity(args.len() + 1);
    passthrough_args.extend_from_slice(&args[..passthrough_index]);
    passthrough_args.push(OsString::from("--"));
    passthrough_args.extend_from_slice(&args[passthrough_index..]);
    MultitoolCli::try_parse_from(passthrough_args).ok()
}

fn first_prompt_passthrough_arg(args: &[OsString]) -> Option<usize> {
    let mut index = 1;
    while index < args.len() {
        let arg = args[index].to_str()?;
        if arg == "--" {
            return args.get(index + 1).map(|_| index + 1);
        }
        if arg.starts_with('-') {
            index += root_option_arg_count(arg);
            continue;
        }
        return Some(index);
    }
    None
}

fn root_option_arg_count(arg: &str) -> usize {
    if arg.contains('=') {
        return 1;
    }
    match arg {
        "-c"
        | "--config"
        | "--enable"
        | "--disable"
        | "-m"
        | "--model"
        | "--local-provider"
        | "-p"
        | "--profile"
        | "-s"
        | "--sandbox"
        | "-a"
        | "--ask-for-approval"
        | "-C"
        | "--cd"
        | "-i"
        | "--image"
        | "--add-dir"
        | "--remote"
        | "--remote-auth-token-env" => 2,
        _ if arg.starts_with("-c")
            || arg.starts_with("-m")
            || arg.starts_with("-p")
            || arg.starts_with("-s")
            || arg.starts_with("-a")
            || arg.starts_with("-C")
            || arg.starts_with("-i") =>
        {
            1
        }
        _ => 1,
    }
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    /// Run Codex non-interactively.
    #[clap(visible_alias = "e")]
    Exec(ExecCli),

    /// Run a code review non-interactively.
    Review(ReviewArgs),

    /// Manage login.
    Login(LoginCommand),

    /// Remove stored authentication credentials.
    Logout(LogoutCommand),

    /// Manage configured accounts and account pools.
    Account(AccountCommand),

    /// Manage external MCP servers for Codex.
    Mcp(McpCli),

    /// Manage Codex plugins.
    Plugin(PluginCli),

    /// Print a machine-readable catalog of Codex APIs, MCP tools, and workflow helpers.
    Api(ApiCatalogCli),

    /// Manage Codex workflows.
    Workflow(WorkflowCli),

    /// Internal: run compiled-in native workflows.
    #[clap(hide = true, name = "native-workflow")]
    NativeWorkflow(NativeWorkflowCli),

    /// Internal: run workflow quality validation.
    #[clap(hide = true, name = "workflow-quality-hook")]
    WorkflowQualityHook,

    /// Tune tool-router guidance.
    #[clap(name = "tool-router")]
    ToolRouter(ToolRouterCli),

    /// Inspect, tune, and manage model-router policy.
    #[clap(name = "model-router")]
    ModelRouter(ModelRouterCli),

    /// Start Codex as an MCP server (stdio).
    McpServer,

    /// [experimental] Run the app server or related tooling.
    AppServer(AppServerCommand),

    /// Launch the Codex desktop app (opens the app installer if missing).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    App(app_cmd::AppCommand),

    /// Generate shell completion scripts.
    Completion(CompletionCommand),

    /// Update Codex to the latest version.
    Update,

    /// Run commands within a Codex-provided sandbox.
    Sandbox(SandboxArgs),

    /// Debugging tools.
    Debug(DebugCommand),

    /// Execpolicy tooling.
    #[clap(hide = true)]
    Execpolicy(ExecpolicyCommand),

    /// Apply the latest diff produced by Codex agent as a `git apply` to your local working tree.
    #[clap(visible_alias = "a")]
    Apply(ApplyCommand),

    /// Resume a previous interactive session (picker by default; use --last to continue the most recent).
    Resume(ResumeCommand),

    /// Fork a previous interactive session (picker by default; use --last to fork the most recent).
    Fork(ForkCommand),

    /// [EXPERIMENTAL] Browse tasks from Codex Cloud and apply changes locally.
    #[clap(name = "cloud", alias = "cloud-tasks")]
    Cloud(CloudTasksCli),

    /// Internal: run the responses API proxy.
    #[clap(hide = true)]
    ResponsesApiProxy(ResponsesApiProxyArgs),

    /// Internal: relay stdio to a Unix domain socket.
    #[clap(hide = true, name = "stdio-to-uds")]
    StdioToUds(StdioToUdsCommand),

    /// Internal: run the MCP stdio process reuse broker.
    #[clap(hide = true, name = "mcp-broker")]
    McpBroker(McpBrokerCommand),

    /// [EXPERIMENTAL] Run the standalone exec-server service.
    ExecServer(ExecServerCommand),

    /// Inspect feature flags.
    Features(FeaturesCli),

    /// Configure implement review/fix cycles.
    Implement(ImplementCli),
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex plugin")]
struct PluginCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    subcommand: PluginSubcommand,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex implement")]
struct ImplementCli {
    #[command(subcommand)]
    command: ImplementCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ImplementCommand {
    /// Enable automatic implement review/fix cycles.
    Enable(ImplementActionArgs),

    /// Disable implement review/fix cycles.
    Disable(ImplementActionArgs),

    /// Enable implement review/fix cycles only for explicit /implement turns.
    Implicit(ImplementActionArgs),
}

#[derive(Debug, Args)]
struct ImplementActionArgs {
    /// Maximum number of review/fix cycles before surfacing remaining findings.
    #[arg(long = "max-cycles")]
    max_cycles: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
enum ImplementConfigMode {
    Auto,
    Disabled,
    Implicit,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex tool-router")]
struct ToolRouterCli {
    #[command(subcommand)]
    subcommand: ToolRouterSubcommand,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex model-router")]
struct ModelRouterCli {
    #[command(subcommand)]
    subcommand: ModelRouterSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ModelRouterSubcommand {
    /// Show the effective routing policy for a task.
    Policy(ModelRouterPolicyCommand),

    /// Replay historical completed turns and tune router metrics.
    Tune(ModelRouterTuneCommand),

    /// Show or apply a stored model-router tune report.
    Report(ModelRouterReportCli),

    /// Show lifecycle promotion state.
    Lifecycle(ModelRouterLifecycleCommand),

    /// Show recorded shadow evaluation results.
    Shadows(ModelRouterShadowsCommand),

    /// Show model-router production savings and overhead usage.
    Usage(ModelRouterUsageCommand),

    /// Mark a candidate as promoted for a task.
    Promote(ModelRouterPromoteCommand),

    /// Mark a promoted candidate as demoted for a task.
    Demote(ModelRouterDemoteCommand),
}

#[derive(Debug, Args)]
struct ModelRouterPolicyCommand {
    /// Task key to evaluate, such as module.review.triage or subagent.review.
    #[arg(long = "task-key")]
    task_key: Option<String>,

    /// Emit the policy report as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex model-router report")]
struct ModelRouterReportCli {
    #[command(subcommand)]
    subcommand: ModelRouterReportSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ModelRouterReportSubcommand {
    /// Print a stored report and show deltas from current state.
    Show(ModelRouterReportShowCommand),

    /// Apply passing recommendations from a stored report.
    Apply(ModelRouterReportApplyCommand),
}

#[derive(Debug, Args)]
struct ModelRouterTuneCommand {
    /// Historical rollout window to inspect, such as 30d, 24h, 30m, or all.
    #[arg(long = "window")]
    window: Option<String>,

    /// Maximum evaluation cost budget in USD.
    #[arg(long = "cost-budget-usd")]
    cost_budget_usd: Option<f64>,

    /// Maximum replay and judge token budget.
    #[arg(long = "token-budget")]
    token_budget: Option<i64>,

    /// Preview recommendations without writing metric overlays.
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,

    /// Write the JSON report to this path.
    #[arg(long = "report-out", value_name = "PATH")]
    report_out: Option<PathBuf>,

    /// Emit the report as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelRouterLifecycleCommand {
    /// Limit lifecycle state to a task key.
    #[arg(long = "task-key")]
    task_key: Option<String>,

    /// Limit lifecycle state to one candidate identity key.
    #[arg(long = "candidate-identity")]
    candidate_identity: Option<String>,

    /// Lifecycle event window to inspect, such as 30d, 24h, 30m, or all.
    #[arg(long = "window", default_value = "all")]
    window: String,

    /// Include compact lifecycle event timeline rows in text output.
    #[arg(long = "events", default_value_t = false)]
    events: bool,

    /// Maximum lifecycle event rows to include.
    #[arg(long = "limit", default_value_t = 50)]
    limit: i64,

    /// Emit lifecycle state as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelRouterShadowsCommand {
    /// Limit shadow evaluations to a task key.
    #[arg(long = "task-key")]
    task_key: Option<String>,

    /// Maximum raw shadow evaluation rows to print.
    #[arg(long = "limit", default_value_t = 50)]
    limit: i64,

    /// Emit shadow evaluations as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelRouterUsageCommand {
    /// Ledger window to inspect, such as 30d, 24h, 30m, or all.
    #[arg(long = "window", default_value = "30d")]
    window: String,

    /// Limit usage to a task key.
    #[arg(long = "task-key")]
    task_key: Option<String>,

    /// Group usage rows by task, model, day, or request-kind.
    #[arg(long = "group-by", value_enum, default_value = "task")]
    group_by: ModelRouterUsageGroupByArg,

    /// Emit usage as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ModelRouterUsageGroupByArg {
    Task,
    Model,
    Day,
    RequestKind,
}

#[derive(Debug, Args)]
struct ModelRouterPromoteCommand {
    /// Task key whose production route should use this candidate.
    #[arg(long = "task-key")]
    task_key: String,

    /// Candidate identity key from `codex model-router policy --json`.
    #[arg(long = "candidate-identity")]
    candidate_identity: String,

    /// Base route identity key that the candidate shadowed.
    #[arg(long = "base-candidate-identity")]
    base_candidate_identity: String,

    /// Lifecycle rule id that authorized the promotion.
    #[arg(long = "rule-id")]
    rule_id: Option<String>,

    /// Human-readable promotion reason.
    #[arg(long = "reason")]
    reason: Option<String>,

    /// Emit the updated promotion record as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelRouterDemoteCommand {
    /// Task key whose promoted candidate should be demoted.
    #[arg(long = "task-key")]
    task_key: String,

    /// Candidate identity key to demote.
    #[arg(long = "candidate-identity")]
    candidate_identity: String,

    /// Human-readable demotion reason.
    #[arg(long = "reason")]
    reason: Option<String>,

    /// Emit the updated lifecycle state as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelRouterReportShowCommand {
    /// Stored report path.
    path: PathBuf,

    /// Emit the report as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, Args)]
struct ModelRouterReportApplyCommand {
    /// Stored report path.
    path: PathBuf,

    /// Preview applicable recommendations without writing metric overlays.
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,

    /// Emit the apply outcome as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, clap::Subcommand)]
enum ToolRouterSubcommand {
    /// Analyze recent router telemetry and optionally persist passing guidance.
    Tune(ToolRouterTuneCommand),
}

#[derive(Debug, Args)]
struct ToolRouterTuneCommand {
    /// Telemetry window to inspect, such as 7d, 24h, 30m, or all.
    #[arg(long = "window", default_value = "7d")]
    window: String,

    /// Tune one model slug. Defaults to the configured model when --all-models is not set.
    #[arg(long = "model", value_name = "SLUG", conflicts_with = "all_models")]
    model: Option<String>,

    /// Tune all models present in router telemetry.
    #[arg(long = "all-models", default_value_t = false)]
    all_models: bool,

    /// Maximum total guidance tokens. The hard maximum is 1200.
    #[arg(long = "max-guidance-tokens", default_value_t = 600)]
    max_guidance_tokens: usize,

    /// Run an introspection pass with model-router-selected model when no override is set.
    #[arg(long = "introspect", default_value_t = false)]
    introspect: bool,

    /// Persist only passing dynamic guidance with positive estimated net savings.
    #[arg(long = "apply", default_value_t = false)]
    apply: bool,

    /// Emit the report as JSON.
    #[arg(long = "json", default_value_t = false)]
    json: bool,
}

#[derive(Debug, clap::Subcommand)]
enum PluginSubcommand {
    /// Manage plugin marketplaces for Codex.
    Marketplace(MarketplaceCli),
}

#[derive(Debug, Parser)]
struct CompletionCommand {
    /// Shell to generate completions for
    #[clap(value_enum, default_value_t = Shell::Bash)]
    shell: Shell,
}

#[derive(Debug, Parser)]
struct DebugCommand {
    #[command(subcommand)]
    subcommand: DebugSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugSubcommand {
    /// Render the raw model catalog as JSON.
    Models(DebugModelsCommand),

    /// Tooling: helps debug the app server.
    AppServer(DebugAppServerCommand),

    /// Render the model-visible prompt input list as JSON.
    PromptInput(DebugPromptInputCommand),

    /// Replay a rollout trace bundle and write reduced state JSON.
    #[clap(hide = true)]
    TraceReduce(DebugTraceReduceCommand),

    /// Internal: reset local memory state for a fresh start.
    #[clap(hide = true)]
    ClearMemories,
}

#[derive(Debug, Parser)]
struct DebugAppServerCommand {
    #[command(subcommand)]
    subcommand: DebugAppServerSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugAppServerSubcommand {
    // Send message to app server V2.
    SendMessageV2(DebugAppServerSendMessageV2Command),
}

#[derive(Debug, Parser)]
struct DebugAppServerSendMessageV2Command {
    #[arg(value_name = "USER_MESSAGE", required = true)]
    user_message: String,
}

#[derive(Debug, Parser)]
struct McpBrokerCommand {
    #[arg(long)]
    socket: PathBuf,
}

#[derive(Debug, Parser)]
struct DebugPromptInputCommand {
    /// Optional user prompt to append after session context.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// Optional image(s) to attach to the user prompt.
    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    images: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
struct DebugModelsCommand {
    /// Skip refresh and dump only the bundled catalog shipped with this binary.
    #[arg(long = "bundled", default_value_t = false)]
    bundled: bool,
}

#[derive(Debug, Parser)]
struct DebugTraceReduceCommand {
    /// Trace bundle directory containing manifest.json and trace.jsonl.
    #[arg(value_name = "TRACE_BUNDLE")]
    trace_bundle: PathBuf,

    /// Output path for reduced RolloutTrace JSON. Defaults to TRACE_BUNDLE/state.json.
    #[arg(long = "output", short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct ResumeCommand {
    /// Conversation/session id (UUID) or thread name. UUIDs take precedence if it parses.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Continue the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    /// Include non-interactive sessions in the resume picker and --last selection.
    #[arg(long = "include-non-interactive", default_value_t = false)]
    include_non_interactive: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: TuiCli,
}

#[derive(Debug, Parser)]
struct ForkCommand {
    /// Conversation/session id (UUID). When provided, forks this session.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Fork the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false, conflicts_with = "session_id")]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: TuiCli,
}

#[derive(Debug, Parser)]
struct SandboxArgs {
    #[command(subcommand)]
    cmd: SandboxCommand,
}

#[derive(Debug, clap::Subcommand)]
enum SandboxCommand {
    /// Run a command under Seatbelt (macOS only).
    #[clap(visible_alias = "seatbelt")]
    Macos(SeatbeltCommand),

    /// Run a command under the Linux sandbox (bubblewrap by default).
    #[clap(visible_alias = "landlock")]
    Linux(LandlockCommand),

    /// Run a command under Windows restricted token (Windows only).
    Windows(WindowsCommand),
}

#[derive(Debug, Parser)]
struct ExecpolicyCommand {
    #[command(subcommand)]
    sub: ExecpolicySubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ExecpolicySubcommand {
    /// Check execpolicy files against a command.
    #[clap(name = "check")]
    Check(ExecPolicyCheckCommand),
}

#[derive(Debug, Parser)]
struct LoginCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[arg(
        long = "with-api-key",
        help = "Read the API key from stdin (e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`)"
    )]
    with_api_key: bool,

    #[arg(
        long = "with-agent-identity",
        help = "Read the experimental Agent Identity token from stdin (e.g. `printenv CODEX_AGENT_IDENTITY | codex login --with-agent-identity`)"
    )]
    with_agent_identity: bool,

    #[arg(
        long = "api-key",
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "API_KEY",
        help = "(deprecated) Previously accepted the API key directly; now exits with guidance to use --with-api-key",
        hide = true
    )]
    api_key: Option<String>,

    #[arg(long = "device-auth")]
    use_device_code: bool,

    /// Store ChatGPT credentials under a named account.
    #[arg(long = "account", value_name = "ID")]
    account: Option<String>,

    /// EXPERIMENTAL: Use custom OAuth issuer base URL (advanced)
    /// Override the OAuth issuer base URL (advanced)
    #[arg(long = "experimental_issuer", value_name = "URL", hide = true)]
    issuer_base_url: Option<String>,

    /// EXPERIMENTAL: Use custom OAuth client ID (advanced)
    #[arg(long = "experimental_client-id", value_name = "CLIENT_ID", hide = true)]
    client_id: Option<String>,

    #[command(subcommand)]
    action: Option<LoginSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum LoginSubcommand {
    /// Show login status.
    Status,
}

#[derive(Debug, Parser)]
struct LogoutCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    /// Remove credentials for a named account.
    #[arg(long = "account", value_name = "ID", conflicts_with = "all")]
    account: Option<String>,

    /// Remove default credentials and all named-account credentials.
    #[arg(long = "all")]
    all: bool,
}

#[derive(Debug, Parser)]
struct AccountCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    subcommand: AccountSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum AccountSubcommand {
    /// List default, named, and configured logical pool accounts.
    List(AccountListCommand),

    /// Show ChatGPT Codex usage limits for default, named, and pool accounts.
    Limits,

    /// Refresh ChatGPT tokens and usage snapshots for an account or pool.
    Refresh(AccountRefreshCommand),
}

#[derive(Debug, Parser)]
struct AccountListCommand {
    #[arg(long = "json")]
    json: bool,
}

#[derive(Debug, Parser)]
struct AccountRefreshCommand {
    #[arg(value_name = "ID", conflicts_with = "pool")]
    id: Option<String>,

    #[arg(long = "pool", value_name = "POOL_ID")]
    pool: Option<String>,
}

#[derive(Debug, Parser)]
struct AppServerCommand {
    /// Omit to run the app server; specify a subcommand for tooling.
    #[command(subcommand)]
    subcommand: Option<AppServerSubcommand>,

    /// Transport endpoint URL. Supported values: `stdio://` (default),
    /// `unix://`, `unix://PATH`, `ws://IP:PORT`, `off`.
    #[arg(
        long = "listen",
        value_name = "URL",
        default_value = codex_app_server::AppServerTransport::DEFAULT_LISTEN_URL
    )]
    listen: codex_app_server::AppServerTransport,

    /// Controls whether analytics are enabled by default.
    ///
    /// Analytics are disabled by default for app-server. Users have to explicitly opt in
    /// via the `analytics` section in the config.toml file.
    ///
    /// However, for first-party use cases like the VSCode IDE extension, we default analytics
    /// to be enabled by default by setting this flag. Users can still opt out by setting this
    /// in their config.toml:
    ///
    /// ```toml
    /// [analytics]
    /// enabled = false
    /// ```
    ///
    /// See https://developers.openai.com/codex/config-advanced/#metrics for more details.
    #[arg(long = "analytics-default-enabled")]
    analytics_default_enabled: bool,

    #[command(flatten)]
    auth: codex_app_server::AppServerWebsocketAuthArgs,
}

#[derive(Debug, Parser)]
struct ExecServerCommand {
    /// Transport endpoint URL. Supported values: `ws://IP:PORT` (default).
    #[arg(
        long = "listen",
        value_name = "URL",
        default_value = "ws://127.0.0.1:0"
    )]
    listen: String,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::enum_variant_names)]
enum AppServerSubcommand {
    /// Proxy stdio bytes to the running app-server control socket.
    Proxy(AppServerProxyCommand),

    /// [experimental] Generate TypeScript bindings for the app server protocol.
    GenerateTs(GenerateTsCommand),

    /// [experimental] Generate JSON Schema for the app server protocol.
    GenerateJsonSchema(GenerateJsonSchemaCommand),

    /// [internal] Generate internal JSON Schema artifacts for Codex tooling.
    #[clap(hide = true)]
    GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand),
}

#[derive(Debug, Args)]
struct AppServerProxyCommand {
    /// Path to the app-server Unix domain socket to connect to.
    #[arg(long = "sock", value_name = "SOCKET_PATH", value_parser = parse_socket_path)]
    socket_path: Option<AbsolutePathBuf>,
}

#[derive(Debug, Args)]
struct GenerateTsCommand {
    /// Output directory where .ts files will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Optional path to the Prettier executable to format generated files
    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateJsonSchemaCommand {
    /// Output directory where the schema bundle will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateInternalJsonSchemaCommand {
    /// Output directory where internal JSON Schema artifacts will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,
}

#[derive(Debug, Parser)]
struct StdioToUdsCommand {
    /// Path to the Unix domain socket to connect to.
    #[arg(value_name = "SOCKET_PATH", value_parser = parse_socket_path)]
    socket_path: AbsolutePathBuf,
}

fn parse_socket_path(raw: &str) -> Result<AbsolutePathBuf, String> {
    AbsolutePathBuf::relative_to_current_dir(raw)
        .map_err(|err| format!("failed to resolve socket path `{raw}`: {err}"))
}

fn format_exit_messages(exit_info: AppExitInfo, color_enabled: bool) -> Vec<String> {
    let AppExitInfo {
        token_usage,
        thread_id: conversation_id,
        ..
    } = exit_info;

    let mut lines = Vec::new();
    if !token_usage.is_zero() {
        lines.push(token_usage.to_string());
    }

    if let Some(resume_cmd) =
        codex_core::util::resume_command(/*thread_name*/ None, conversation_id)
    {
        let command = if color_enabled {
            resume_cmd.cyan().to_string()
        } else {
            resume_cmd
        };
        lines.push(format!("To continue this session, run {command}"));
    }

    lines
}

/// Handle the app exit and print the results. Optionally run the update action.
fn handle_app_exit(exit_info: AppExitInfo) -> anyhow::Result<()> {
    match exit_info.exit_reason {
        ExitReason::Fatal(message) => {
            eprintln!("ERROR: {message}");
            std::process::exit(1);
        }
        ExitReason::UserRequested => { /* normal exit */ }
    }

    let update_action = exit_info.update_action;
    let color_enabled = supports_color::on(Stream::Stdout).is_some();
    for line in format_exit_messages(exit_info, color_enabled) {
        println!("{line}");
    }
    if let Some(action) = update_action {
        run_update_action(action)?;
    }
    Ok(())
}

/// Run the update action and print the result.
fn run_update_action(action: UpdateAction) -> anyhow::Result<()> {
    println!();
    let cmd_str = action.command_str();
    println!("Updating Codex via `{cmd_str}`...");

    let status = {
        #[cfg(windows)]
        {
            if action == UpdateAction::StandaloneWindows {
                let (cmd, args) = action.command_args();
                // Run the standalone PowerShell installer with PowerShell
                // itself. Routing this through `cmd.exe /C` would parse
                // PowerShell metacharacters like `|` before PowerShell sees
                // the installer command.
                std::process::Command::new(cmd).args(args).status()?
            } else {
                // On Windows, run via cmd.exe so .CMD/.BAT are correctly resolved (PATHEXT semantics).
                std::process::Command::new("cmd")
                    .args(["/C", &cmd_str])
                    .status()?
            }
        }
        #[cfg(not(windows))]
        {
            let (cmd, args) = action.command_args();
            let command_path = crate::wsl_paths::normalize_for_wsl(cmd);
            let normalized_args: Vec<String> = args
                .iter()
                .map(crate::wsl_paths::normalize_for_wsl)
                .collect();
            std::process::Command::new(&command_path)
                .args(&normalized_args)
                .status()?
        }
    };
    if !status.success() {
        anyhow::bail!("`{cmd_str}` failed with status {status}");
    }
    println!("\n🎉 Update ran successfully! Please restart Codex.");
    Ok(())
}

fn run_update_command() -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    {
        anyhow::bail!(
            "`codex update` is not available in debug builds. Install a release build of Codex to use this command."
        );
    }

    #[cfg(not(debug_assertions))]
    {
        let Some(action) = codex_tui::get_update_action() else {
            anyhow::bail!(
                "Could not detect the Codex installation method. Please update manually: https://developers.openai.com/codex/cli/"
            );
        };
        run_update_action(action)
    }
}

fn run_execpolicycheck(cmd: ExecPolicyCheckCommand) -> anyhow::Result<()> {
    cmd.run()
}

async fn run_debug_app_server_command(cmd: DebugAppServerCommand) -> anyhow::Result<()> {
    match cmd.subcommand {
        DebugAppServerSubcommand::SendMessageV2(cmd) => {
            let codex_bin = std::env::current_exe()?;
            codex_app_server_test_client::send_message_v2(&codex_bin, &[], cmd.user_message, &None)
                .await
        }
    }
}

#[derive(Debug, Default, Parser, Clone)]
struct FeatureToggles {
    /// Enable a feature (repeatable). Equivalent to `-c features.<name>=true`.
    #[arg(long = "enable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    enable: Vec<String>,

    /// Disable a feature (repeatable). Equivalent to `-c features.<name>=false`.
    #[arg(long = "disable", value_name = "FEATURE", action = clap::ArgAction::Append, global = true)]
    disable: Vec<String>,
}

#[derive(Debug, Default, Parser, Clone)]
struct InteractiveRemoteOptions {
    /// Connect the TUI to a remote app server websocket endpoint.
    ///
    /// Accepted forms: `ws://host:port` or `wss://host:port`.
    #[arg(long = "remote", value_name = "ADDR")]
    remote: Option<String>,

    /// Name of the environment variable containing the bearer token to send to
    /// a remote app server websocket.
    #[arg(long = "remote-auth-token-env", value_name = "ENV_VAR")]
    remote_auth_token_env: Option<String>,
}

impl FeatureToggles {
    fn to_overrides(&self) -> anyhow::Result<Vec<String>> {
        let mut v = Vec::new();
        for feature in &self.enable {
            Self::validate_feature(feature)?;
            v.push(format!("features.{feature}=true"));
        }
        for feature in &self.disable {
            Self::validate_feature(feature)?;
            v.push(format!("features.{feature}=false"));
        }
        Ok(v)
    }

    fn validate_feature(feature: &str) -> anyhow::Result<()> {
        if is_known_feature_key(feature) {
            Ok(())
        } else {
            anyhow::bail!("Unknown feature flag: {feature}")
        }
    }
}

#[derive(Debug, Parser)]
struct FeaturesCli {
    #[command(subcommand)]
    sub: FeaturesSubcommand,
}

#[derive(Debug, Parser)]
enum FeaturesSubcommand {
    /// List known features with their stage and effective state.
    List,
    /// Enable a feature in config.toml.
    Enable(FeatureSetArgs),
    /// Disable a feature in config.toml.
    Disable(FeatureSetArgs),
}

#[derive(Debug, Parser)]
struct FeatureSetArgs {
    /// Feature key to update (for example: unified_exec).
    feature: String,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        cli_main(arg0_paths).await?;
        Ok(())
    })
}

fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::UnderDevelopment => "under development",
        Stage::Experimental { .. } => "experimental",
        Stage::Stable => "stable",
        Stage::Deprecated => "deprecated",
        Stage::Removed => "removed",
    }
}

async fn run_exec_server_command(
    cmd: ExecServerCommand,
    arg0_paths: &Arg0DispatchPaths,
) -> anyhow::Result<()> {
    let codex_self_exe = arg0_paths
        .codex_self_exe
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Codex executable path is not configured"))?;
    let runtime_paths = codex_exec_server::ExecServerRuntimePaths::new(
        codex_self_exe,
        arg0_paths.codex_linux_sandbox_exe.clone(),
    )?;
    codex_exec_server::run_main(&cmd.listen, runtime_paths)
        .await
        .map_err(anyhow::Error::from_boxed)
}

async fn enable_feature_in_config(interactive: &TuiCli, feature: &str) -> anyhow::Result<()> {
    FeatureToggles::validate_feature(feature)?;
    let codex_home = find_codex_home()?;
    ConfigEditsBuilder::new(&codex_home)
        .with_profile(interactive.config_profile.as_deref())
        .set_feature_enabled(feature, /*enabled*/ true)
        .apply()
        .await?;
    println!("Enabled feature `{feature}` in config.toml.");
    maybe_print_under_development_feature_warning(&codex_home, interactive, feature);
    Ok(())
}

async fn disable_feature_in_config(interactive: &TuiCli, feature: &str) -> anyhow::Result<()> {
    FeatureToggles::validate_feature(feature)?;
    let codex_home = find_codex_home()?;
    ConfigEditsBuilder::new(&codex_home)
        .with_profile(interactive.config_profile.as_deref())
        .set_feature_enabled(feature, /*enabled*/ false)
        .apply()
        .await?;
    println!("Disabled feature `{feature}` in config.toml.");
    Ok(())
}

async fn run_implement_command(args: ImplementCli) -> anyhow::Result<()> {
    let codex_home = find_codex_home()?;
    let mut edits = ConfigEditsBuilder::new(&codex_home);
    let (mode, max_cycles) = match args.command {
        ImplementCommand::Enable(action_args) => {
            (ImplementConfigMode::Auto, action_args.max_cycles)
        }
        ImplementCommand::Disable(action_args) => {
            (ImplementConfigMode::Disabled, action_args.max_cycles)
        }
        ImplementCommand::Implicit(action_args) => {
            (ImplementConfigMode::Implicit, action_args.max_cycles)
        }
    };
    match mode {
        ImplementConfigMode::Auto => {
            edits = edits.set_path_value(
                vec!["implement".to_string(), "enabled".to_string()],
                toml_edit::value(true),
            );
            edits = edits.set_path_value(
                vec!["implement".to_string(), "mode".to_string()],
                toml_edit::value("auto"),
            );
        }
        ImplementConfigMode::Disabled => {
            edits = edits.set_path_value(
                vec!["implement".to_string(), "enabled".to_string()],
                toml_edit::value(false),
            );
        }
        ImplementConfigMode::Implicit => {
            edits = edits.set_path_value(
                vec!["implement".to_string(), "enabled".to_string()],
                toml_edit::value(true),
            );
            edits = edits.set_path_value(
                vec!["implement".to_string(), "mode".to_string()],
                toml_edit::value("implicit"),
            );
        }
    }
    if let Some(max_cycles) = max_cycles {
        edits = edits.set_path_value(
            vec!["implement".to_string(), "max_cycles".to_string()],
            toml_edit::Item::Value(i64::from(max_cycles).into()),
        );
    }
    edits.apply().await?;

    match (mode, max_cycles) {
        (ImplementConfigMode::Disabled, Some(max_cycles)) => {
            println!("Disabled implement review/fix cycles and set max_cycles={max_cycles}.");
        }
        (ImplementConfigMode::Disabled, None) => println!("Disabled implement review/fix cycles."),
        (ImplementConfigMode::Implicit, Some(max_cycles)) => {
            println!("Enabled implicit implement review/fix cycles with max_cycles={max_cycles}.");
        }
        (ImplementConfigMode::Implicit, None) => {
            println!("Enabled implicit implement review/fix cycles.");
        }
        (ImplementConfigMode::Auto, Some(max_cycles)) => {
            println!("Enabled implement review/fix cycles with max_cycles={max_cycles}.");
        }
        (ImplementConfigMode::Auto, None) => println!("Enabled implement review/fix cycles."),
    }
    Ok(())
}

fn maybe_print_under_development_feature_warning(
    codex_home: &std::path::Path,
    interactive: &TuiCli,
    feature: &str,
) {
    if interactive.config_profile.is_some() {
        return;
    }

    let Some(spec) = FEATURES.iter().find(|spec| spec.key == feature) else {
        return;
    };
    if !matches!(spec.stage, Stage::UnderDevelopment) {
        return;
    }

    let config_path = codex_home.join(codex_config::CONFIG_TOML_FILE);
    eprintln!(
        "Under-development features enabled: {feature}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {}.",
        config_path.display()
    );
}

async fn run_debug_trace_reduce_command(cmd: DebugTraceReduceCommand) -> anyhow::Result<()> {
    let output = cmd
        .output
        .unwrap_or_else(|| cmd.trace_bundle.join(REDUCED_STATE_FILE_NAME));

    let trace = replay_bundle(&cmd.trace_bundle)?;
    let reduced_json = serde_json::to_vec_pretty(&trace)?;
    tokio::fs::write(&output, reduced_json).await?;
    println!("{}", output.display());

    Ok(())
}

async fn run_debug_prompt_input_command(
    cmd: DebugPromptInputCommand,
    root_config_overrides: CliConfigOverrides,
    interactive: TuiCli,
    arg0_paths: Arg0DispatchPaths,
) -> anyhow::Result<()> {
    let shared = interactive.shared.into_inner();
    let mut cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    if interactive.web_search {
        cli_kv_overrides.push((
            "web_search".to_string(),
            toml::Value::String("live".to_string()),
        ));
    }

    let approval_policy = if shared.dangerously_bypass_approvals_and_sandbox {
        Some(AskForApproval::Never)
    } else {
        interactive.approval_policy.map(Into::into)
    };
    let sandbox_mode = if shared.dangerously_bypass_approvals_and_sandbox {
        Some(codex_protocol::config_types::SandboxMode::DangerFullAccess)
    } else {
        shared.sandbox_mode.map(Into::into)
    };
    let overrides = ConfigOverrides {
        model: shared.model,
        config_profile: shared.config_profile,
        approval_policy,
        sandbox_mode,
        cwd: shared.cwd,
        codex_self_exe: arg0_paths.codex_self_exe,
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe,
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe,
        show_raw_agent_reasoning: shared.oss.then_some(true),
        ephemeral: Some(true),
        additional_writable_roots: shared.add_dir,
        ..Default::default()
    };
    let config =
        Config::load_with_cli_overrides_and_harness_overrides(cli_kv_overrides, overrides).await?;

    let mut input = shared
        .images
        .into_iter()
        .chain(cmd.images)
        .map(|path| UserInput::LocalImage { path })
        .collect::<Vec<_>>();
    let interactive_prompt = (!interactive.prompt.is_empty()).then(|| interactive.prompt.join(" "));
    if let Some(prompt) = cmd.prompt.or(interactive_prompt) {
        input.push(UserInput::Text {
            text: prompt.replace("\r\n", "\n").replace('\r', "\n"),
            text_elements: Vec::new(),
        });
    }

    let prompt_input = codex_core::build_prompt_input(config, input, None).await?;
    println!("{}", serde_json::to_string_pretty(&prompt_input)?);

    Ok(())
}

async fn run_debug_models_command(
    cmd: DebugModelsCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let catalog = if cmd.bundled {
        bundled_models_response()?
    } else {
        let cli_overrides = root_config_overrides
            .parse_overrides()
            .map_err(anyhow::Error::msg)?;
        let config = Config::load_with_cli_overrides(cli_overrides).await?;
        let auth_manager =
            AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ true);
        let models_manager =
            build_models_manager(&config, auth_manager, CollaborationModesConfig::default());
        models_manager
            .raw_model_catalog(RefreshStrategy::OnlineIfUncached)
            .await
    };

    serde_json::to_writer(std::io::stdout(), &catalog)?;
    println!();
    Ok(())
}

async fn run_tool_router_tune_command(
    cmd: ToolRouterTuneCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let cli_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = Config::load_with_cli_overrides(cli_overrides).await?;
    let model_slug = if cmd.all_models {
        None
    } else {
        cmd.model.clone().or_else(|| config.model.clone())
    };
    let state_db =
        StateRuntime::init(config.sqlite_home.clone(), config.model_provider_id.clone()).await?;
    let introspection_provider = if cmd.introspect {
        let auth_manager =
            AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ true);
        let models_manager = build_models_manager(
            &config,
            Arc::clone(&auth_manager),
            CollaborationModesConfig::default(),
        );
        Some(Arc::new(
            codex_core::tool_router_tune::ToolRouterModelIntrospectionProvider::new(
                config.clone(),
                auth_manager,
                models_manager,
            )
            .await?,
        )
            as Arc<
                dyn codex_core::tool_router_tune::ToolRouterIntrospectionProvider,
            >)
    } else {
        None
    };
    let report = codex_core::tool_router_tune::tune_tool_router(
        state_db.as_ref(),
        codex_core::tool_router_tune::ToolRouterTuneOptions {
            window: cmd.window,
            model_slug,
            max_guidance_tokens: cmd.max_guidance_tokens,
            introspection_provider,
            apply: cmd.apply,
        },
    )
    .await?;

    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_tool_router_tune_report(&report);
    }

    Ok(())
}

async fn run_model_router_command(
    cli: ModelRouterCli,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    match cli.subcommand {
        ModelRouterSubcommand::Policy(cmd) => {
            run_model_router_policy_command(cmd, root_config_overrides).await
        }
        ModelRouterSubcommand::Tune(cmd) => {
            run_model_router_tune_command(cmd, root_config_overrides).await
        }
        ModelRouterSubcommand::Report(report_cli) => match report_cli.subcommand {
            ModelRouterReportSubcommand::Show(cmd) => {
                run_model_router_report_show_command(cmd, root_config_overrides).await
            }
            ModelRouterReportSubcommand::Apply(cmd) => {
                run_model_router_report_apply_command(cmd, root_config_overrides).await
            }
        },
        ModelRouterSubcommand::Lifecycle(cmd) => {
            run_model_router_lifecycle_command(cmd, root_config_overrides).await
        }
        ModelRouterSubcommand::Shadows(cmd) => {
            run_model_router_shadows_command(cmd, root_config_overrides).await
        }
        ModelRouterSubcommand::Usage(cmd) => {
            run_model_router_usage_command(cmd, root_config_overrides).await
        }
        ModelRouterSubcommand::Promote(cmd) => {
            run_model_router_promote_command(cmd, root_config_overrides).await
        }
        ModelRouterSubcommand::Demote(cmd) => {
            run_model_router_demote_command(cmd, root_config_overrides).await
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRouterPolicyReport {
    enabled: bool,
    discovery: String,
    task_key: Option<String>,
    candidates: Vec<ModelRouterPolicyCandidateReport>,
    policy: Option<codex_model_router::policy::PolicyApplication>,
    policy_error: Option<String>,
    lifecycle: codex_model_router::policy::EffectiveLifecycle,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRouterPolicyCandidateReport {
    route_index: usize,
    identity_key: String,
    id: Option<String>,
    model_provider: String,
    model: Option<String>,
    incumbent: bool,
    eligible: bool,
    score_bias: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRouterLifecycleStateReport {
    task_key: Option<String>,
    candidate_identity: Option<String>,
    window: String,
    effective_lifecycle: codex_model_router::policy::EffectiveLifecycle,
    promotions: Vec<codex_state::ModelRouterLifecyclePromotionRecord>,
    stats: codex_state::ModelRouterLifecycleStatsSummary,
    events: Vec<codex_state::ModelRouterLifecycleEventRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelRouterLifecycleTimelineDisplay {
    Hidden,
    Shown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRouterShadowReport {
    task_key: Option<String>,
    summaries: Vec<codex_state::ModelRouterShadowEvaluationSummary>,
    recent: Vec<codex_state::ModelRouterShadowEvaluationRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRouterUsageReport {
    window: String,
    summary: codex_state::ModelRouterUsageSummary,
}

async fn run_model_router_policy_command(
    cmd: ModelRouterPolicyCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (config, _state_db) = model_router_config_and_state(root_config_overrides).await?;
    let report = model_router_policy_report(&config, cmd.task_key.as_deref()).await;
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_model_router_policy_report(&report);
    }
    Ok(())
}

async fn run_model_router_lifecycle_command(
    cmd: ModelRouterLifecycleCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let effective_lifecycle = codex_model_router::policy::effective_lifecycle_for_route(
        config.model_router.as_ref(),
        cmd.task_key.as_deref().unwrap_or("model_router.lifecycle"),
        /*route*/ None,
    )?;
    let now_ms = Utc::now().timestamp_millis();
    let query = codex_state::ModelRouterLifecycleStatsQuery {
        window_start_ms: model_router_usage_window_start_ms(&cmd.window, now_ms)?,
        window_end_ms: now_ms,
        task_key: cmd.task_key.clone(),
        candidate_identity: cmd.candidate_identity.clone(),
        event_limit: cmd.limit,
    };
    let mut promotions = state_db
        .model_router_lifecycle_promotions(cmd.task_key.as_deref())
        .await?;
    if let Some(candidate_identity) = cmd.candidate_identity.as_deref() {
        promotions.retain(|promotion| promotion.candidate_identity == candidate_identity);
    }
    let report = ModelRouterLifecycleStateReport {
        task_key: cmd.task_key.clone(),
        candidate_identity: cmd.candidate_identity.clone(),
        window: cmd.window,
        effective_lifecycle,
        promotions,
        stats: state_db.model_router_lifecycle_stats(query.clone()).await?,
        events: state_db.model_router_lifecycle_events(query).await?,
    };
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        let timeline = if cmd.events {
            ModelRouterLifecycleTimelineDisplay::Shown
        } else {
            ModelRouterLifecycleTimelineDisplay::Hidden
        };
        print!(
            "{}",
            format_model_router_lifecycle_report(&report, timeline)
        );
    }
    Ok(())
}

async fn run_model_router_shadows_command(
    cmd: ModelRouterShadowsCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (_config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let report = ModelRouterShadowReport {
        task_key: cmd.task_key.clone(),
        summaries: state_db
            .model_router_shadow_evaluation_summaries(cmd.task_key.as_deref())
            .await?,
        recent: state_db
            .model_router_shadow_evaluations(cmd.task_key.as_deref(), cmd.limit)
            .await?,
    };
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_model_router_shadow_report(&report);
    }
    Ok(())
}

async fn run_model_router_usage_command(
    cmd: ModelRouterUsageCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (_config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let now_ms = Utc::now().timestamp_millis();
    let summary = state_db
        .model_router_usage_summary(codex_state::ModelRouterUsageQuery {
            window_start_ms: model_router_usage_window_start_ms(&cmd.window, now_ms)?,
            window_end_ms: now_ms,
            task_key: cmd.task_key.clone(),
            group_by: cmd.group_by.into(),
        })
        .await?;
    let report = ModelRouterUsageReport {
        window: cmd.window,
        summary,
    };
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print!("{}", format_model_router_usage_report(&report));
    }
    Ok(())
}

async fn run_model_router_promote_command(
    cmd: ModelRouterPromoteCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let report = model_router_policy_report(&config, Some(&cmd.task_key)).await;
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.identity_key == cmd.candidate_identity)
        .ok_or_else(|| anyhow::anyhow!("candidate identity is not present in effective policy"))?;
    let base = report
        .candidates
        .iter()
        .find(|candidate| candidate.identity_key == cmd.base_candidate_identity);
    let now_ms = Utc::now().timestamp_millis();
    let record = codex_state::ModelRouterLifecyclePromotionRecord {
        task_key: cmd.task_key,
        candidate_identity: cmd.candidate_identity,
        base_candidate_identity: cmd.base_candidate_identity,
        status: "promoted".to_string(),
        rule_id: cmd.rule_id,
        production_model_provider: Some(candidate.model_provider.clone()),
        production_model: candidate.model.clone(),
        base_model_provider: base.map(|base| base.model_provider.clone()),
        base_model: base.and_then(|base| base.model.clone()),
        promoted_at_ms: now_ms,
        updated_at_ms: now_ms,
        reason: cmd.reason,
    };
    state_db
        .promote_model_router_lifecycle_promotion(
            record.clone(),
            codex_state::ModelRouterLifecycleTransitionContext {
                source: codex_state::MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL.to_string(),
                lifecycle_window: Some(report.lifecycle.window.clone()),
                shadow_phase: None,
                shadow_summary: None,
                failed_gates_json: None,
            },
        )
        .await?;
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &record)?;
        println!();
    } else {
        println!(
            "Promoted {} for {}",
            record.candidate_identity, record.task_key
        );
    }
    Ok(())
}

async fn run_model_router_demote_command(
    cmd: ModelRouterDemoteCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let lifecycle_window = codex_model_router::policy::effective_lifecycle_for_route(
        config.model_router.as_ref(),
        &cmd.task_key,
        /*route*/ None,
    )
    .ok()
    .map(|lifecycle| lifecycle.window);
    let rows = state_db
        .demote_model_router_lifecycle_promotion_with_event(
            &cmd.task_key,
            &cmd.candidate_identity,
            cmd.reason.as_deref(),
            codex_state::ModelRouterLifecycleTransitionContext {
                source: codex_state::MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL.to_string(),
                lifecycle_window,
                shadow_phase: None,
                shadow_summary: None,
                failed_gates_json: None,
            },
        )
        .await?;
    let promotions = state_db
        .model_router_lifecycle_promotions(Some(&cmd.task_key))
        .await?;
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &promotions)?;
        println!();
    } else if rows == 0 {
        println!(
            "No promoted model-router candidate found for {} on {}",
            cmd.candidate_identity, cmd.task_key
        );
    } else {
        println!("Demoted {} for {}", cmd.candidate_identity, cmd.task_key);
    }
    Ok(())
}

async fn model_router_config_and_state(
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<(Config, std::sync::Arc<StateRuntime>)> {
    let cli_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = Config::load_with_cli_overrides(cli_overrides).await?;
    let state_db =
        StateRuntime::init(config.sqlite_home.clone(), config.model_provider_id.clone()).await?;
    Ok((config, state_db))
}

async fn model_router_policy_report(
    config: &Config,
    task_key: Option<&str>,
) -> ModelRouterPolicyReport {
    let Some(model_router) = config.model_router.as_ref() else {
        return ModelRouterPolicyReport {
            enabled: false,
            discovery: "curated".to_string(),
            task_key: task_key.map(str::to_string),
            candidates: Vec::new(),
            policy: None,
            policy_error: None,
            lifecycle: codex_model_router::policy::EffectiveLifecycle::default(),
        };
    };
    let auth_manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ true);
    let models_manager =
        build_models_manager(config, auth_manager, CollaborationModesConfig::default());
    let (candidate_pool, mut policy_error) =
        match model_router_candidate_pool_for_config(config, &models_manager).await {
            Ok(candidates) => (candidates, None),
            Err(err) => (model_router.candidates.clone(), Some(err)),
        };

    let mut routes = Vec::with_capacity(candidate_pool.len() + 1);
    let mut candidates = Vec::with_capacity(candidate_pool.len() + 1);
    let incumbent = codex_config::config_toml::ModelRouterCandidateToml {
        id: Some("incumbent".to_string()),
        model: config.model.clone(),
        model_provider: Some(config.model_provider_id.clone()),
        ..Default::default()
    };
    push_policy_candidate_report(
        config,
        &mut routes,
        &mut candidates,
        &incumbent,
        /*incumbent*/ true,
    );
    for candidate in &candidate_pool {
        push_policy_candidate_report(
            config,
            &mut routes,
            &mut candidates,
            candidate,
            /*incumbent*/ false,
        );
    }

    let policy = if let Some(task_key) = task_key {
        match codex_model_router::policy::apply_model_router_policy(model_router, task_key, &routes)
        {
            Ok(policy) => Some(policy),
            Err(err) => {
                policy_error = Some(err.to_string());
                None
            }
        }
    } else {
        None
    };
    if let Some(policy) = policy.as_ref() {
        for candidate in &mut candidates {
            if let Some(decision) = policy
                .routes
                .iter()
                .find(|decision| decision.route_index == candidate.route_index)
            {
                candidate.eligible = true;
                candidate.score_bias = decision.score_bias;
            } else {
                candidate.eligible = false;
                candidate.score_bias = 0.0;
            }
        }
    }

    let lifecycle = codex_model_router::policy::effective_lifecycle_for_route(
        Some(model_router),
        task_key.unwrap_or("model_router.policy"),
        /*route*/ None,
    )
    .unwrap_or_default();

    ModelRouterPolicyReport {
        enabled: model_router.enabled,
        discovery: format!("{:?}", model_router.discovery.unwrap_or_default()),
        task_key: task_key.map(str::to_string),
        candidates,
        policy,
        policy_error,
        lifecycle,
    }
}

fn push_policy_candidate_report(
    config: &Config,
    routes: &mut Vec<codex_model_router::policy::PolicyRoute>,
    candidates: &mut Vec<ModelRouterPolicyCandidateReport>,
    candidate: &codex_config::config_toml::ModelRouterCandidateToml,
    incumbent: bool,
) {
    let route_index = routes.len();
    let model_provider = candidate
        .model_provider
        .clone()
        .unwrap_or_else(|| config.model_provider_id.clone());
    let model = candidate.model.clone().or_else(|| config.model.clone());
    routes.push(codex_model_router::policy::PolicyRoute {
        index: route_index,
        model_provider: model_provider.clone(),
        model: model.clone(),
    });
    candidates.push(ModelRouterPolicyCandidateReport {
        route_index,
        identity_key: codex_model_router::policy::candidate_identity_key(candidate),
        id: candidate.id.clone(),
        model_provider,
        model,
        incumbent,
        eligible: true,
        score_bias: 0.0,
    });
}

fn print_model_router_policy_report(report: &ModelRouterPolicyReport) {
    println!("Model router policy");
    println!("Enabled: {}", report.enabled);
    println!("Discovery: {}", report.discovery);
    if let Some(task_key) = &report.task_key {
        println!("Task: {task_key}");
    }
    if let Some(error) = &report.policy_error {
        println!("Policy error: {error}");
    }
    println!(
        "Lifecycle: window {}, min evaluated {}, confidence {:.2}, success {:.2}",
        report.lifecycle.window,
        report.lifecycle.min_evaluated,
        report.lifecycle.min_confidence,
        report.lifecycle.min_success_rate
    );
    println!("Candidates:");
    for candidate in &report.candidates {
        println!(
            "- #{} {}{} model={} provider={} eligible={} bias={:.3}",
            candidate.route_index,
            candidate.id.as_deref().unwrap_or("<unnamed>"),
            if candidate.incumbent {
                " (incumbent)"
            } else {
                ""
            },
            candidate.model.as_deref().unwrap_or("<inherit>"),
            candidate.model_provider,
            candidate.eligible,
            candidate.score_bias
        );
    }
}

fn format_model_router_lifecycle_report(
    report: &ModelRouterLifecycleStateReport,
    timeline: ModelRouterLifecycleTimelineDisplay,
) -> String {
    let mut lines = vec![
        "Model router lifecycle".to_string(),
        format!("Window: {}", report.window),
    ];
    if let Some(task_key) = &report.task_key {
        lines.push(format!("Task: {task_key}"));
    }
    if let Some(candidate_identity) = &report.candidate_identity {
        lines.push(format!("Candidate: {candidate_identity}"));
    }
    lines.push(format!(
        "Defaults: window {}, budget {} tokens/${:.6}, gates {} evals/{:.2} confidence/{:.2} success",
        report.effective_lifecycle.window,
        report.effective_lifecycle.token_budget,
        report.effective_lifecycle.cost_budget_usd,
        report.effective_lifecycle.min_evaluated,
        report.effective_lifecycle.min_confidence,
        report.effective_lifecycle.min_success_rate
    ));

    let totals = &report.stats.totals;
    lines.push(format!(
        "Events: {} promoted, {} demoted, {} evaluating, {} rejected, {} blocked (auto {}, manual {})",
        totals.promoted,
        totals.demoted,
        totals.evaluating,
        totals.rejected,
        totals.promotion_blocked,
        totals.auto,
        totals.manual
    ));
    if report.stats.candidates.is_empty() {
        lines.push("No lifecycle state found.".to_string());
    } else {
        lines.push("Candidates:".to_string());
        for candidate in &report.stats.candidates {
            let status = candidate.current_status.as_deref().unwrap_or("none");
            let last_event = candidate.last_event_at_ms.map_or_else(
                || "never".to_string(),
                |at_ms| {
                    format!(
                        "{} at {}",
                        candidate.last_event_type.as_deref().unwrap_or("event"),
                        at_ms
                    )
                },
            );
            let reason = candidate.last_reason.as_deref().unwrap_or("<none>");
            lines.push(format!(
                "- {} {}: status {}, promoted {}, demoted {}, evaluating {}, rejected {}, blocked {}, auto/manual {}/{}, last {}, reason {}",
                candidate.task_key,
                candidate.candidate_identity,
                status,
                candidate.counts.promoted,
                candidate.counts.demoted,
                candidate.counts.evaluating,
                candidate.counts.rejected,
                candidate.counts.promotion_blocked,
                candidate.counts.auto,
                candidate.counts.manual,
                last_event,
                reason
            ));
        }
    }

    if timeline == ModelRouterLifecycleTimelineDisplay::Shown {
        if report.events.is_empty() {
            lines.push("No lifecycle events found.".to_string());
        } else {
            lines.push("Timeline:".to_string());
            for event in &report.events {
                lines.push(format_model_router_lifecycle_event(event));
            }
        }
    }

    format!("{}\n", lines.join("\n"))
}

fn format_model_router_lifecycle_event(
    event: &codex_state::ModelRouterLifecycleEventRecord,
) -> String {
    let status = match (&event.previous_status, &event.next_status) {
        (Some(previous), Some(next)) => format!(" {previous}->{next}"),
        (Some(previous), None) => format!(" {previous}->none"),
        (None, Some(next)) => format!(" none->{next}"),
        (None, None) => String::new(),
    };
    let mut line = format!(
        "- {} {} {} {} {}{}",
        event.created_at_ms,
        event.source,
        event.event_type,
        event.task_key,
        event.candidate_identity,
        status
    );
    if let Some(reason) = &event.reason {
        line.push_str(&format!(" reason={reason}"));
    }
    if let Some(phase) = &event.shadow_phase {
        line.push_str(&format!(
            " shadow={} evals={} success={:.2} confidence={:.2} cost={} tokens={} latest={}@{}",
            phase,
            event.shadow_evaluated_count.unwrap_or_default(),
            event.shadow_success_rate.unwrap_or_default(),
            event.shadow_average_confidence.unwrap_or_default(),
            model_router_usd(event.shadow_cost_used_usd_micros.unwrap_or_default()),
            event.shadow_tokens_used.unwrap_or_default(),
            event
                .shadow_latest_evaluation_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "?".to_string()),
            event
                .shadow_latest_evaluation_at_ms
                .map(|at_ms| at_ms.to_string())
                .unwrap_or_else(|| "?".to_string())
        ));
    }
    if let Some(gates) = format_failed_lifecycle_gates(event.failed_gates_json.as_deref()) {
        line.push_str(&format!(" failed_gates={gates}"));
    }
    line
}

fn format_failed_lifecycle_gates(failed_gates_json: Option<&str>) -> Option<String> {
    let failed_gates_json = failed_gates_json?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(failed_gates_json) else {
        return Some(failed_gates_json.to_string());
    };
    let Some(gates) = value.as_array() else {
        return Some(failed_gates_json.to_string());
    };
    let names = gates
        .iter()
        .filter_map(|gate| gate.get("gate").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    if names.is_empty() {
        Some(failed_gates_json.to_string())
    } else {
        Some(names.join(","))
    }
}

fn print_model_router_shadow_report(report: &ModelRouterShadowReport) {
    println!("Model router shadows");
    if let Some(task_key) = &report.task_key {
        println!("Task: {task_key}");
    }
    if report.summaries.is_empty() {
        println!("No shadow evaluations found.");
    } else {
        println!("Summaries:");
        for summary in &report.summaries {
            println!(
                "- {} {} {} vs {}: {} evals, success {:.2}, confidence {:.2}, cost ${:.6}, tokens {}",
                summary.task_key,
                summary.phase,
                summary.candidate_identity,
                summary.base_candidate_identity,
                summary.evaluated_count,
                summary.success_rate,
                summary.average_confidence,
                summary.cost_used_usd_micros as f64 / 1_000_000.0,
                summary.tokens_used
            );
        }
    }
    if !report.recent.is_empty() {
        println!("Recent:");
        for row in &report.recent {
            println!(
                "- {} {} {} {} vs {} success={} confidence={:.2}",
                row.created_at_ms,
                row.task_key,
                row.phase,
                row.candidate_identity,
                row.base_candidate_identity,
                row.success,
                row.confidence
            );
        }
    }
}

fn format_model_router_usage_report(report: &ModelRouterUsageReport) -> String {
    let totals = &report.summary.totals;
    let mut lines = vec![
        "Model router usage".to_string(),
        format!("Window: {}", report.window),
    ];
    if let Some(task_key) = &report.summary.task_key {
        lines.push(format!("Task: {task_key}"));
    }
    lines.push(format!(
        "Requests: {} production, {} overhead, {} total",
        totals.production_request_count, totals.overhead_request_count, totals.request_count
    ));
    lines.push(format!(
        "Tokens: {} total (input {}, cached {}, output {}, reasoning {})",
        totals.token_usage.total_tokens,
        totals.token_usage.input_tokens,
        totals.token_usage.cached_input_tokens,
        totals.token_usage.output_tokens,
        totals.token_usage.reasoning_output_tokens
    ));
    lines.push(format!(
        "Costs: production {}, counterfactual {}, overhead {}",
        model_router_usd(totals.savings.actual_production_cost_usd_micros),
        model_router_usd(totals.savings.counterfactual_cost_usd_micros),
        model_router_usd(totals.savings.router_overhead_cost_usd_micros)
    ));
    lines.push(format!(
        "Savings: gross {}, net {}",
        model_router_usd(totals.savings.gross_savings_usd_micros),
        model_router_usd(totals.savings.net_savings_usd_micros)
    ));
    lines.push(format!(
        "Price confidence: avg {:.2}, min {:.2}",
        totals.average_price_confidence, totals.minimum_price_confidence
    ));
    let coverage = &totals.coverage;
    if coverage.missing_price_rows > 0
        || coverage.low_confidence_price_rows > 0
        || coverage.zero_token_rows > 0
        || coverage.production_rows_missing_actual_cost > 0
        || coverage.production_rows_missing_counterfactual > 0
    {
        lines.push(format!(
            "Coverage gaps: missing price {}, low confidence {}, zero-token {}, missing production actual {}, missing production counterfactual {}",
            coverage.missing_price_rows,
            coverage.low_confidence_price_rows,
            coverage.zero_token_rows,
            coverage.production_rows_missing_actual_cost,
            coverage.production_rows_missing_counterfactual
        ));
    }
    if report.summary.groups.is_empty() {
        lines.push("No model-router ledger rows found.".to_string());
    } else {
        let group_by = match report.summary.group_by {
            codex_state::ModelRouterUsageGroupBy::Task => "task",
            codex_state::ModelRouterUsageGroupBy::Model => "model",
            codex_state::ModelRouterUsageGroupBy::Day => "day",
            codex_state::ModelRouterUsageGroupBy::RequestKind => "request-kind",
        };
        lines.push(format!("Groups by {group_by}:"));
        for group in &report.summary.groups {
            let totals = &group.totals;
            lines.push(format!(
                "- {}: requests {}, tokens {}, production {}, counterfactual {}, overhead {}, net {}, confidence {:.2}",
                group.key,
                totals.request_count,
                totals.token_usage.total_tokens,
                model_router_usd(totals.savings.actual_production_cost_usd_micros),
                model_router_usd(totals.savings.counterfactual_cost_usd_micros),
                model_router_usd(totals.savings.router_overhead_cost_usd_micros),
                model_router_usd(totals.savings.net_savings_usd_micros),
                totals.average_price_confidence
            ));
        }
    }
    format!("{}\n", lines.join("\n"))
}

fn model_router_usd(usd_micros: i64) -> String {
    format!("${:.6}", usd_micros as f64 / 1_000_000.0)
}

fn model_router_usage_window_start_ms(window: &str, now_ms: i64) -> anyhow::Result<Option<i64>> {
    let window = window.trim();
    if window.eq_ignore_ascii_case("all") || window.eq_ignore_ascii_case("all-time") {
        return Ok(None);
    }
    let (number, unit) = window.split_at(window.len().saturating_sub(1));
    let value = number.parse::<i64>()?.max(0);
    let multiplier = match unit {
        "d" => 24 * 60 * 60 * 1000,
        "h" => 60 * 60 * 1000,
        "m" => 60 * 1000,
        _ => anyhow::bail!("window must be a duration like 30d, 24h, 30m, or all"),
    };
    Ok(Some(
        now_ms.saturating_sub(value.saturating_mul(multiplier)),
    ))
}

impl From<ModelRouterUsageGroupByArg> for codex_state::ModelRouterUsageGroupBy {
    fn from(value: ModelRouterUsageGroupByArg) -> Self {
        match value {
            ModelRouterUsageGroupByArg::Task => Self::Task,
            ModelRouterUsageGroupByArg::Model => Self::Model,
            ModelRouterUsageGroupByArg::Day => Self::Day,
            ModelRouterUsageGroupByArg::RequestKind => Self::RequestKind,
        }
    }
}

async fn run_model_router_tune_command(
    cmd: ModelRouterTuneCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let ModelRouterTuneCommand {
        window,
        cost_budget_usd,
        token_budget,
        dry_run,
        report_out,
        json,
    } = cmd;
    let (config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let lifecycle_defaults = codex_model_router::policy::effective_lifecycle_for_route(
        config.model_router.as_ref(),
        "model_router.tune",
        /*route*/ None,
    )?;
    let window = window.unwrap_or(lifecycle_defaults.window);
    let cost_budget_usd = cost_budget_usd.unwrap_or(lifecycle_defaults.cost_budget_usd);
    let token_budget = token_budget
        .unwrap_or_else(|| i64::try_from(lifecycle_defaults.token_budget).unwrap_or(i64::MAX));
    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ true);
    let models_manager = build_models_manager(
        &config,
        auth_manager.clone(),
        CollaborationModesConfig::default(),
    );
    let candidate_preview = model_router_candidate_pool_for_config(&config, &models_manager)
        .await
        .map_err(anyhow::Error::msg)?;
    let options = codex_core::model_router_tune::ModelRouterTuneOptions {
        window,
        cost_budget_usd,
        token_budget,
        dry_run,
    };
    print_model_router_tune_progress_start(&options, &candidate_preview);
    let started_at = Instant::now();
    let tune_runtime =
        codex_core::model_router_tune::ModelRouterTuneRuntime::new(auth_manager, models_manager);
    let report = codex_core::model_router_tune::tune_model_router(
        state_db.as_ref(),
        &config,
        options,
        Some(tune_runtime),
    )
    .await?;
    if let Some(report_out) = report_out {
        let json = serde_json::to_string_pretty(&report)?;
        tokio::fs::write(report_out, format!("{json}\n")).await?;
    }
    print_model_router_tune_progress_complete(&report, started_at.elapsed());
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_model_router_tune_report(&report);
    }
    Ok(())
}

async fn run_model_router_report_show_command(
    cmd: ModelRouterReportShowCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let mut report = read_model_router_report(&cmd.path).await?;
    codex_core::model_router_tune::refresh_model_router_report_deltas(
        state_db.as_ref(),
        &config,
        &mut report,
    )
    .await?;
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_model_router_tune_report(&report);
    }
    Ok(())
}

async fn run_model_router_report_apply_command(
    cmd: ModelRouterReportApplyCommand,
    root_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let (config, state_db) = model_router_config_and_state(root_config_overrides).await?;
    let report = read_model_router_report(&cmd.path).await?;
    let outcome = codex_core::model_router_tune::apply_model_router_tune_report(
        state_db.as_ref(),
        &config,
        report,
        cmd.dry_run,
    )
    .await?;
    if cmd.json {
        serde_json::to_writer_pretty(std::io::stdout(), &outcome)?;
        println!();
    } else {
        let mode = if outcome.dry_run { "dry-run" } else { "apply" };
        println!(
            "Model router report {mode}: {} recommendation(s)",
            outcome.applied_recommendations
        );
        print_model_router_tune_report(&outcome.report);
    }
    Ok(())
}

async fn read_model_router_report(
    path: &PathBuf,
) -> anyhow::Result<codex_core::model_router_tune::ModelRouterTuneReport> {
    let text = tokio::fs::read_to_string(path).await?;
    Ok(serde_json::from_str(&text)?)
}

fn print_model_router_tune_report(report: &codex_core::model_router_tune::ModelRouterTuneReport) {
    println!("Model router tune report");
    println!("Run: {}", report.run_id);
    println!("Generated: {}", report.generated_at);
    println!("Window: {}", report.window);
    println!(
        "Budget: {} tokens, ${:.6} cost; used {} tokens, ${:.6}",
        report.budget.token_budget,
        report.budget.cost_budget_usd_micros as f64 / 1_000_000.0,
        report.budget_used.tokens_used,
        report.budget_used.cost_used_usd_micros as f64 / 1_000_000.0
    );
    println!(
        "Cases: evaluated {}, skipped {}",
        report.evaluated_count, report.skipped_count
    );
    if !report.apply_eligibility.eligible {
        println!(
            "Apply eligibility: refused ({})",
            report
                .apply_eligibility
                .reason
                .as_deref()
                .unwrap_or("unknown reason")
        );
    }
    if report.recommendations.is_empty() {
        println!("No recommendations found.");
        return;
    }
    println!("Recommendations:");
    for recommendation in &report.recommendations {
        println!(
            "- {}: confidence {:.2}, passing {}, eligible {}, applied {}",
            recommendation.candidate_identity_key,
            recommendation.confidence,
            recommendation.passing,
            recommendation.apply_eligible,
            recommendation.applied
        );
        for change in &recommendation.changes {
            println!(
                "  {}: {} {:?} -> {} ({:?}, eligible {})",
                model_router_metric_name(change.metric),
                model_router_metric_source(change.current_source),
                change.current_value,
                model_router_metric_value(change.proposed_value),
                change.action,
                change.apply_eligible
            );
        }
    }
}

fn print_model_router_tune_progress_start(
    options: &codex_core::model_router_tune::ModelRouterTuneOptions,
    candidates: &[ModelRouterCandidateToml],
) {
    eprintln!(
        "model-router tune: window={}, token_budget={}, cost_budget=${:.6}, dry_run={}",
        options.window, options.token_budget, options.cost_budget_usd, options.dry_run
    );
    eprintln!(
        "model-router tune: discovered {} candidate(s): {}",
        candidates.len(),
        summarize_model_router_candidates(candidates)
    );
}

fn print_model_router_tune_progress_complete(
    report: &codex_core::model_router_tune::ModelRouterTuneReport,
    elapsed: std::time::Duration,
) {
    eprintln!(
        "model-router tune: completed in {:.1}s; evaluated {}, skipped {}, used {} tokens and ${:.6}",
        elapsed.as_secs_f64(),
        report.evaluated_count,
        report.skipped_count,
        report.budget_used.tokens_used,
        report.budget_used.cost_used_usd_micros as f64 / 1_000_000.0
    );
}

fn summarize_model_router_candidates(candidates: &[ModelRouterCandidateToml]) -> String {
    if candidates.is_empty() {
        return "none".to_string();
    }

    let mut labels = candidates
        .iter()
        .take(8)
        .map(model_router_candidate_label)
        .collect::<Vec<_>>();
    if candidates.len() > labels.len() {
        labels.push(format!("+{} more", candidates.len() - labels.len()));
    }
    labels.join(", ")
}

fn model_router_candidate_label(candidate: &ModelRouterCandidateToml) -> String {
    let provider = candidate
        .model_provider
        .as_deref()
        .unwrap_or("current-provider");
    let model = candidate.model.as_deref().unwrap_or("current-model");
    match candidate.id.as_deref() {
        Some(id) => format!("{id} ({provider}/{model})"),
        None => format!("{provider}/{model}"),
    }
}

fn model_router_metric_name(
    metric: codex_core::model_router_tune::ModelRouterMetricName,
) -> &'static str {
    match metric {
        codex_core::model_router_tune::ModelRouterMetricName::IntelligenceScore => {
            "intelligence_score"
        }
        codex_core::model_router_tune::ModelRouterMetricName::SuccessRate => "success_rate",
        codex_core::model_router_tune::ModelRouterMetricName::MedianLatencyMs => {
            "median_latency_ms"
        }
        codex_core::model_router_tune::ModelRouterMetricName::EstimatedCostUsdMicros => {
            "estimated_cost_usd_micros"
        }
    }
}

fn model_router_metric_source(
    source: codex_core::model_router_tune::ModelRouterMetricSource,
) -> &'static str {
    match source {
        codex_core::model_router_tune::ModelRouterMetricSource::ExplicitToml => "explicit",
        codex_core::model_router_tune::ModelRouterMetricSource::AppliedOverlay => "overlay",
        codex_core::model_router_tune::ModelRouterMetricSource::Missing => "missing",
    }
}

fn model_router_metric_value(
    value: Option<codex_core::model_router_tune::ModelRouterMetricValue>,
) -> String {
    match value {
        Some(codex_core::model_router_tune::ModelRouterMetricValue::Score(value)) => {
            format!("{value:.3}")
        }
        Some(codex_core::model_router_tune::ModelRouterMetricValue::Millis(value)) => {
            format!("{value}ms")
        }
        Some(codex_core::model_router_tune::ModelRouterMetricValue::UsdMicros(value)) => {
            format!("${:.6}", value as f64 / 1_000_000.0)
        }
        None => "none".to_string(),
    }
}

fn print_tool_router_tune_report(report: &codex_core::tool_router_tune::ToolRouterTuneReport) {
    print!("{}", format_tool_router_tune_report(report));
}

fn format_tool_router_tune_report(
    report: &codex_core::tool_router_tune::ToolRouterTuneReport,
) -> String {
    let mode = if report.apply { "apply" } else { "dry-run" };
    let mut output = String::new();
    output.push_str(&format!("Tool router tune ({mode})\n"));
    output.push_str(&format!("Window: {}\n", report.window));
    if let Some(model) = &report.introspection_model {
        output.push_str(&format!(
            "Introspection: {model} (prompt {}, completion {}, total {})\n",
            report.introspection_tokens.prompt_tokens,
            report.introspection_tokens.completion_tokens,
            report.introspection_tokens.total_tokens
        ));
    } else {
        output.push_str("Introspection: disabled\n");
    }
    output.push_str(&format!(
        "Schema/format tokens: visible router {}, hidden tools {}, format {}",
        report.schema_format_tokens.visible_router_schema_tokens,
        report.schema_format_tokens.hidden_tool_schema_tokens,
        report.schema_format_tokens.format_description_tokens
    ));
    output.push('\n');
    if report.optimizations.is_empty() {
        output.push_str("No optimizations found.\n");
        return output;
    }
    output.push_str("Optimizations:\n");
    for optimization in &report.optimizations {
        output.push_str(&format!(
            "- {} [{}]: model {} via {}, toolset {}, calls {} (fallbacks {}, errors {}), guidance {} -> {}, gross {}, guidance-cost {}, net {}, persisted {}\n",
            tool_router_optimization_name(optimization.optimization_type),
            tool_router_test_status_name(optimization.test_status),
            optimization.model_slug,
            optimization.model_provider,
            optimization.toolset_hash,
            optimization.affected_call_count,
            optimization.fallback_call_count,
            optimization.invalid_route_errors,
            optimization.guidance_tokens_before,
            optimization.guidance_tokens_after,
            optimization.gross_savings_tokens,
            optimization.guidance_delta_cost_tokens,
            optimization.net_savings_tokens,
            optimization.persisted
        ));
        output.push_str(&format!(
            "  route kinds: {}\n",
            format_tool_router_counts(&optimization.route_kind_breakdown)
        ));
        output.push_str(&format!(
            "  selected tools: {}\n",
            format_tool_router_counts(&optimization.selected_tool_breakdown)
        ));
        output.push_str(&format!(
            "  fallback tools: {}\n",
            format_tool_router_counts(&optimization.fallback_tool_breakdown)
        ));
        output.push_str(&format!(
            "  outcomes: {}\n",
            format_tool_router_counts(&optimization.outcome_breakdown)
        ));
        output.push_str(&format!(
            "  learned rule hits: {}\n",
            optimization.learned_rule_hits
        ));
        output.push_str(&format!(
            "  error outcomes: {}\n",
            format_tool_router_counts(&optimization.error_outcome_breakdown)
        ));
        output.push_str(&format!("  guidance: {}\n", optimization.message));
    }
    output
}

fn format_tool_router_counts(counts: &[codex_state::ToolRouterTuneCount]) -> String {
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .iter()
        .map(|count| format!("{}={}", count.name, count.count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn tool_router_optimization_name(
    optimization_type: codex_core::tool_router_tune::ToolRouterOptimizationType,
) -> &'static str {
    match optimization_type {
        codex_core::tool_router_tune::ToolRouterOptimizationType::DropStaticGuidance => {
            "drop-static-guidance"
        }
        codex_core::tool_router_tune::ToolRouterOptimizationType::DynamicGuidance => {
            "dynamic-guidance"
        }
        codex_core::tool_router_tune::ToolRouterOptimizationType::FormatDescriptionRefresh => {
            "format-description-refresh"
        }
    }
}

fn tool_router_test_status_name(
    status: codex_core::tool_router_tune::ToolRouterOptimizationTestStatus,
) -> &'static str {
    match status {
        codex_core::tool_router_tune::ToolRouterOptimizationTestStatus::Passing => "passing",
        codex_core::tool_router_tune::ToolRouterOptimizationTestStatus::Failing => "failing",
    }
}

async fn run_debug_clear_memories_command(
    root_config_overrides: &CliConfigOverrides,
    interactive: &TuiCli,
) -> anyhow::Result<()> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let overrides = ConfigOverrides {
        config_profile: interactive.config_profile.clone(),
        ..Default::default()
    };
    let config =
        Config::load_with_cli_overrides_and_harness_overrides(cli_kv_overrides, overrides).await?;

    let state_path = state_db_path(config.sqlite_home.as_path());
    let mut cleared_state_db = false;
    if tokio::fs::try_exists(&state_path).await? {
        let state_db =
            StateRuntime::init(config.sqlite_home.clone(), config.model_provider_id.clone())
                .await?;
        state_db.clear_memory_data().await?;
        cleared_state_db = true;
    }

    clear_memory_roots_contents(&config.codex_home).await?;

    let mut message = if cleared_state_db {
        format!("Cleared memory state from {}.", state_path.display())
    } else {
        format!("No state db found at {}.", state_path.display())
    };
    message.push_str(&format!(
        " Cleared memory directories under {}.",
        config.codex_home.display()
    ));

    println!("{message}");

    Ok(())
}

/// Prepend root-level overrides so they have lower precedence than
/// CLI-specific ones specified after the subcommand (if any).
fn prepend_config_flags(
    subcommand_config_overrides: &mut CliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides
        .raw_overrides
        .splice(0..0, cli_config_overrides.raw_overrides);
}

fn reject_remote_mode_for_subcommand(
    remote: Option<&str>,
    remote_auth_token_env: Option<&str>,
    subcommand: &str,
) -> anyhow::Result<()> {
    if let Some(remote) = remote {
        anyhow::bail!(
            "`--remote {remote}` is only supported for interactive TUI commands, not `codex {subcommand}`"
        );
    }
    if remote_auth_token_env.is_some() {
        anyhow::bail!(
            "`--remote-auth-token-env` is only supported for interactive TUI commands, not `codex {subcommand}`"
        );
    }
    Ok(())
}

fn reject_remote_mode_for_app_server_subcommand(
    remote: Option<&str>,
    remote_auth_token_env: Option<&str>,
    subcommand: Option<&AppServerSubcommand>,
) -> anyhow::Result<()> {
    let subcommand_name = match subcommand {
        None => "app-server",
        Some(AppServerSubcommand::Proxy(_)) => "app-server proxy",
        Some(AppServerSubcommand::GenerateTs(_)) => "app-server generate-ts",
        Some(AppServerSubcommand::GenerateJsonSchema(_)) => "app-server generate-json-schema",
        Some(AppServerSubcommand::GenerateInternalJsonSchema(_)) => {
            "app-server generate-internal-json-schema"
        }
    };
    reject_remote_mode_for_subcommand(remote, remote_auth_token_env, subcommand_name)
}

fn read_remote_auth_token_from_env_var_with<F>(
    env_var_name: &str,
    get_var: F,
) -> anyhow::Result<String>
where
    F: FnOnce(&str) -> Result<String, std::env::VarError>,
{
    let auth_token = get_var(env_var_name)
        .map_err(|_| anyhow::anyhow!("environment variable `{env_var_name}` is not set"))?;
    let auth_token = auth_token.trim().to_string();
    if auth_token.is_empty() {
        anyhow::bail!("environment variable `{env_var_name}` is empty");
    }
    Ok(auth_token)
}

fn read_remote_auth_token_from_env_var(env_var_name: &str) -> anyhow::Result<String> {
    read_remote_auth_token_from_env_var_with(env_var_name, |name| std::env::var(name))
}

async fn run_interactive_tui(
    interactive: TuiCli,
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> std::io::Result<AppExitInfo> {
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Ok(AppExitInfo::fatal(
            "stdin is not a terminal. Run `codex exec` for non-interactive use.",
        ));
    }

    let terminal_info = codex_terminal_detection::terminal_info();
    if terminal_info.name == TerminalName::Dumb {
        eprintln!(
            "WARNING: TERM is set to \"dumb\". Codex's interactive TUI may not work in this terminal."
        );
        if !confirm("Continue anyway? [y/N]: ")? {
            return Ok(AppExitInfo::fatal(
                "Refusing to start the interactive TUI because TERM is set to \"dumb\". Run in a supported terminal or unset TERM.",
            ));
        }
    }

    let normalized_remote = remote
        .as_deref()
        .map(codex_tui::normalize_remote_addr)
        .transpose()
        .map_err(std::io::Error::other)?;
    if remote_auth_token_env.is_some() && normalized_remote.is_none() {
        return Ok(AppExitInfo::fatal(
            "`--remote-auth-token-env` requires `--remote`.",
        ));
    }
    let remote_auth_token = remote_auth_token_env
        .as_deref()
        .map(read_remote_auth_token_from_env_var)
        .transpose()
        .map_err(std::io::Error::other)?;
    const INTERACTIVE_TUI_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

    std::thread::Builder::new()
        .name("codex-interactive-tui".to_string())
        .stack_size(INTERACTIVE_TUI_STACK_SIZE_BYTES)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(INTERACTIVE_TUI_STACK_SIZE_BYTES)
                .build()
                .map_err(std::io::Error::other)?;

            runtime.block_on(codex_tui::run_main(
                interactive,
                arg0_paths,
                LoaderOverrides::default(),
                normalized_remote,
                remote_auth_token,
            ))
        })?
        .join()
        .map_err(|_| std::io::Error::other("interactive TUI thread panicked"))?
}

fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprintln!("{prompt}");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Build the final `TuiCli` for a `codex resume` invocation.
fn finalize_resume_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    include_non_interactive: bool,
    resume_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so resume shares the same
    // configuration surface area as `codex` without additional flags.
    let resume_session_id = session_id;
    interactive.resume_picker = resume_session_id.is_none() && !last;
    interactive.resume_last = last;
    interactive.resume_session_id = resume_session_id;
    interactive.resume_show_all = show_all;
    interactive.resume_include_non_interactive = include_non_interactive;

    // Merge resume-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, resume_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Build the final `TuiCli` for a `codex fork` invocation.
fn finalize_fork_interactive(
    mut interactive: TuiCli,
    root_config_overrides: CliConfigOverrides,
    session_id: Option<String>,
    last: bool,
    show_all: bool,
    fork_cli: TuiCli,
) -> TuiCli {
    // Start with the parsed interactive CLI so fork shares the same
    // configuration surface area as `codex` without additional flags.
    let fork_session_id = session_id;
    interactive.fork_picker = fork_session_id.is_none() && !last;
    interactive.fork_last = last;
    interactive.fork_session_id = fork_session_id;
    interactive.fork_show_all = show_all;

    // Merge fork-scoped flags and overrides with highest precedence.
    merge_interactive_cli_flags(&mut interactive, fork_cli);

    // Propagate any root-level config overrides (e.g. `-c key=value`).
    prepend_config_flags(&mut interactive.config_overrides, root_config_overrides);

    interactive
}

/// Merge flags provided to `codex resume`/`codex fork` so they take precedence over any
/// root-level flags. Only overrides fields explicitly set on the subcommand-scoped
/// CLI. Also appends `-c key=value` overrides with highest precedence.
fn merge_interactive_cli_flags(interactive: &mut TuiCli, subcommand_cli: TuiCli) {
    let TuiCli {
        shared,
        approval_policy,
        web_search,
        prompt,
        config_overrides,
        ..
    } = subcommand_cli;
    interactive
        .shared
        .apply_subcommand_overrides(shared.into_inner());
    if let Some(approval) = approval_policy {
        interactive.approval_policy = Some(approval);
    }
    if web_search {
        interactive.web_search = true;
    }
    if !prompt.is_empty() {
        interactive.prompt = prompt;
    }

    interactive
        .config_overrides
        .raw_overrides
        .extend(config_overrides.raw_overrides);
}

fn print_completion(cmd: CompletionCommand) {
    let mut app = MultitoolCli::command();
    let name = "codex";
    generate(cmd.shell, &mut app, name, &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use codex_protocol::ThreadId;
    use codex_tui::TokenUsage;
    use pretty_assertions::assert_eq;

    fn finalize_resume_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;

        let Subcommand::Resume(ResumeCommand {
            session_id,
            last,
            all,
            include_non_interactive,
            remote: _,
            config_overrides: resume_cli,
        }) = subcommand.expect("resume present")
        else {
            unreachable!()
        };

        finalize_resume_interactive(
            interactive,
            root_overrides,
            session_id,
            last,
            all,
            include_non_interactive,
            resume_cli,
        )
    }

    fn finalize_fork_from_args(args: &[&str]) -> TuiCli {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let MultitoolCli {
            interactive,
            config_overrides: root_overrides,
            subcommand,
            feature_toggles: _,
            remote: _,
        } = cli;

        let Subcommand::Fork(ForkCommand {
            session_id,
            last,
            all,
            remote: _,
            config_overrides: fork_cli,
        }) = subcommand.expect("fork present")
        else {
            unreachable!()
        };

        finalize_fork_interactive(interactive, root_overrides, session_id, last, all, fork_cli)
    }

    #[test]
    fn login_accepts_named_device_auth_account() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "login", "--account", "work", "--device-auth"])
                .expect("parse should succeed");

        let Some(Subcommand::Login(login)) = cli.subcommand else {
            panic!("expected login subcommand");
        };

        assert_eq!(login.account.as_deref(), Some("work"));
        assert!(login.use_device_code);
    }

    #[test]
    fn logout_account_conflicts_with_all_accounts() {
        let err = MultitoolCli::try_parse_from(["codex", "logout", "--account", "work", "--all"])
            .expect_err("conflicting account logout flags should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn account_list_json_parses() {
        let cli = MultitoolCli::try_parse_from(["codex", "account", "list", "--json"])
            .expect("parse should succeed");

        let Some(Subcommand::Account(AccountCommand {
            subcommand: AccountSubcommand::List(list),
            ..
        })) = cli.subcommand
        else {
            panic!("expected account list subcommand");
        };

        assert!(list.json);
    }

    #[test]
    fn account_refresh_accepts_account_or_pool_but_not_both() {
        let account_cli = MultitoolCli::try_parse_from(["codex", "account", "refresh", "work"])
            .expect("parse should succeed");
        let Some(Subcommand::Account(AccountCommand {
            subcommand: AccountSubcommand::Refresh(account_refresh),
            ..
        })) = account_cli.subcommand
        else {
            panic!("expected account refresh subcommand");
        };
        assert_eq!(account_refresh.id.as_deref(), Some("work"));
        assert_eq!(account_refresh.pool, None);

        let pool_cli =
            MultitoolCli::try_parse_from(["codex", "account", "refresh", "--pool", "codex-pro"])
                .expect("parse should succeed");
        let Some(Subcommand::Account(AccountCommand {
            subcommand: AccountSubcommand::Refresh(pool_refresh),
            ..
        })) = pool_cli.subcommand
        else {
            panic!("expected account refresh subcommand");
        };
        assert_eq!(pool_refresh.id, None);
        assert_eq!(pool_refresh.pool.as_deref(), Some("codex-pro"));

        let err = MultitoolCli::try_parse_from([
            "codex",
            "account",
            "refresh",
            "work",
            "--pool",
            "codex-pro",
        ])
        .expect_err("conflicting account refresh target flags should be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn account_limits_parses() {
        let cli = MultitoolCli::try_parse_from(["codex", "account", "limits"])
            .expect("parse should succeed");

        let Some(Subcommand::Account(AccountCommand {
            subcommand: AccountSubcommand::Limits,
            ..
        })) = cli.subcommand
        else {
            panic!("expected account limits subcommand");
        };
    }

    #[test]
    fn exec_resume_last_accepts_prompt_positional() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "exec", "--json", "resume", "--last", "2+2"])
                .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(codex_exec::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert!(args.last);
        assert_eq!(args.session_id, None);
        assert_eq!(args.prompt.as_deref(), Some("2+2"));
    }

    #[test]
    fn exec_resume_accepts_include_non_interactive() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "exec",
            "resume",
            "--last",
            "--include-non-interactive",
            "2+2",
        ])
        .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(codex_exec::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert!(args.last);
        assert!(args.include_non_interactive);
        assert_eq!(args.prompt.as_deref(), Some("2+2"));
    }

    #[test]
    fn exec_resume_accepts_output_last_message_flag_after_subcommand() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "exec",
            "resume",
            "session-123",
            "-o",
            "/tmp/resume-output.md",
            "re-review",
        ])
        .expect("parse should succeed");

        let Some(Subcommand::Exec(exec)) = cli.subcommand else {
            panic!("expected exec subcommand");
        };
        let Some(codex_exec::Command::Resume(args)) = exec.command else {
            panic!("expected exec resume");
        };

        assert_eq!(
            exec.last_message_file,
            Some(std::path::PathBuf::from("/tmp/resume-output.md"))
        );
        assert_eq!(args.session_id.as_deref(), Some("session-123"));
        assert_eq!(args.prompt.as_deref(), Some("re-review"));
    }

    #[test]
    fn dangerous_bypass_conflicts_with_approval_policy() {
        let err = MultitoolCli::try_parse_from([
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "--ask-for-approval",
            "on-request",
        ])
        .expect_err("conflicting permission flags should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    fn app_server_from_args(args: &[&str]) -> AppServerCommand {
        let cli = MultitoolCli::try_parse_from(args).expect("parse");
        let Subcommand::AppServer(app_server) = cli.subcommand.expect("app-server present") else {
            unreachable!()
        };
        app_server
    }

    fn default_app_server_socket_path() -> AbsolutePathBuf {
        let codex_home = find_codex_home().expect("codex home");
        codex_app_server::app_server_control_socket_path(&codex_home)
            .expect("default app-server socket path")
    }

    #[test]
    fn debug_prompt_input_parses_prompt_and_images() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "debug",
            "prompt-input",
            "hello",
            "--image",
            "/tmp/a.png,/tmp/b.png",
        ])
        .expect("parse");

        let Some(Subcommand::Debug(DebugCommand {
            subcommand: DebugSubcommand::PromptInput(cmd),
        })) = cli.subcommand
        else {
            panic!("expected debug prompt-input subcommand");
        };

        assert_eq!(cmd.prompt.as_deref(), Some("hello"));
        assert_eq!(
            cmd.images,
            vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")]
        );
    }

    #[test]
    fn debug_models_parses_bundled_flag() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "debug", "models", "--bundled"]).expect("parse");

        let Some(Subcommand::Debug(DebugCommand {
            subcommand: DebugSubcommand::Models(cmd),
        })) = cli.subcommand
        else {
            panic!("expected debug models subcommand");
        };

        assert!(cmd.bundled);
    }

    #[test]
    fn tool_router_tune_parses_flags() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "tool-router",
            "tune",
            "--window",
            "24h",
            "--model",
            "gpt-test",
            "--max-guidance-tokens",
            "500",
            "--introspect",
            "--apply",
            "--json",
        ])
        .expect("parse");

        let Some(Subcommand::ToolRouter(ToolRouterCli {
            subcommand: ToolRouterSubcommand::Tune(cmd),
        })) = cli.subcommand
        else {
            panic!("expected tool-router tune subcommand");
        };

        assert_eq!(cmd.window, "24h");
        assert_eq!(cmd.model.as_deref(), Some("gpt-test"));
        assert_eq!(cmd.max_guidance_tokens, 500);
        assert!(cmd.introspect);
        assert!(cmd.apply);
        assert!(cmd.json);
    }

    #[test]
    fn tool_router_tune_model_conflicts_with_all_models() {
        let err = MultitoolCli::try_parse_from([
            "codex",
            "tool-router",
            "tune",
            "--model",
            "gpt-test",
            "--all-models",
        ])
        .expect_err("conflicting model selection flags should be rejected");

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn tool_router_tune_text_report_includes_diagnostics() {
        use codex_core::tool_router_tune::ToolRouterOptimizationReport;
        use codex_core::tool_router_tune::ToolRouterOptimizationTestStatus;
        use codex_core::tool_router_tune::ToolRouterOptimizationType;
        use codex_core::tool_router_tune::ToolRouterSchemaFormatTokens;
        use codex_core::tool_router_tune::ToolRouterTuneReport;
        use codex_core::tool_router_tune::ToolRouterTuneTokenUsage;

        let report = ToolRouterTuneReport {
            window: "24h".to_string(),
            apply: false,
            introspection_model: None,
            introspection_tokens: ToolRouterTuneTokenUsage::default(),
            schema_format_tokens: ToolRouterSchemaFormatTokens {
                visible_router_schema_tokens: 10,
                hidden_tool_schema_tokens: 20,
                format_description_tokens: 30,
            },
            optimizations: vec![ToolRouterOptimizationReport {
                optimization_type: ToolRouterOptimizationType::DynamicGuidance,
                model_slug: "gpt-test".to_string(),
                model_provider: "openai".to_string(),
                toolset_hash: "abc123".to_string(),
                router_schema_version: 1,
                guidance_version: 2,
                guidance_tokens_before: 5,
                guidance_tokens_after: 8,
                fallback_call_count: 2,
                invalid_route_errors: 1,
                affected_call_count: 3,
                route_kind_breakdown: vec![codex_state::ToolRouterTuneCount {
                    name: "spark".to_string(),
                    count: 2,
                }],
                selected_tool_breakdown: vec![codex_state::ToolRouterTuneCount {
                    name: "exec_command".to_string(),
                    count: 2,
                }],
                fallback_tool_breakdown: vec![codex_state::ToolRouterTuneCount {
                    name: "exec_command".to_string(),
                    count: 2,
                }],
                outcome_breakdown: vec![codex_state::ToolRouterTuneCount {
                    name: "route_error".to_string(),
                    count: 1,
                }],
                error_outcome_breakdown: vec![codex_state::ToolRouterTuneCount {
                    name: "route_error".to_string(),
                    count: 1,
                }],
                learned_rule_hits: 0,
                request_shape_clusters: Vec::new(),
                per_call_estimated_savings_tokens: 12,
                gross_savings_tokens: 24,
                guidance_delta_cost_tokens: 9,
                allocated_introspection_tokens: 0,
                net_savings_tokens: 15,
                test_status: ToolRouterOptimizationTestStatus::Passing,
                persisted: false,
                message: "Prefer exact routes.".to_string(),
            }],
        };

        let output = format_tool_router_tune_report(&report);

        assert!(output.contains("Introspection: disabled"));
        assert!(output.contains("model gpt-test via openai, toolset abc123"));
        assert!(output.contains("fallbacks 2, errors 1"));
        assert!(output.contains("selected tools: exec_command=2"));
        assert!(output.contains("guidance: Prefer exact routes."));
    }

    #[test]
    fn model_router_tune_parses_flags_and_defaults_to_apply() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "tune",
            "--window",
            "24h",
            "--cost-budget-usd",
            "7.50",
            "--token-budget",
            "12345",
            "--report-out",
            "/tmp/model-router-report.json",
            "--json",
        ])
        .expect("parse");

        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Tune(cmd),
        })) = cli.subcommand
        else {
            panic!("expected model-router tune subcommand");
        };

        assert_eq!(cmd.window.as_deref(), Some("24h"));
        assert_eq!(cmd.cost_budget_usd, Some(7.50));
        assert_eq!(cmd.token_budget, Some(12345));
        assert_eq!(
            cmd.report_out,
            Some(PathBuf::from("/tmp/model-router-report.json"))
        );
        assert!(!cmd.dry_run);
        assert!(cmd.json);
    }

    #[test]
    fn model_router_tune_dry_run_parses() {
        let cli = MultitoolCli::try_parse_from(["codex", "model-router", "tune", "--dry-run"])
            .expect("parse");

        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Tune(cmd),
        })) = cli.subcommand
        else {
            panic!("expected model-router tune subcommand");
        };

        assert_eq!(cmd.window, None);
        assert_eq!(cmd.cost_budget_usd, None);
        assert_eq!(cmd.token_budget, None);
        assert!(cmd.dry_run);
    }

    #[test]
    fn model_router_policy_lifecycle_and_shadow_commands_parse() {
        let policy = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "policy",
            "--task-key",
            "module.review.triage",
            "--json",
        ])
        .expect("parse policy");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Policy(policy),
        })) = policy.subcommand
        else {
            panic!("expected model-router policy subcommand");
        };
        assert_eq!(policy.task_key.as_deref(), Some("module.review.triage"));
        assert!(policy.json);

        let lifecycle = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "lifecycle",
            "--task-key",
            "subagent.review",
            "--candidate-identity",
            "candidate",
            "--window",
            "7d",
            "--events",
            "--limit",
            "10",
            "--json",
        ])
        .expect("parse lifecycle");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Lifecycle(lifecycle),
        })) = lifecycle.subcommand
        else {
            panic!("expected model-router lifecycle subcommand");
        };
        assert_eq!(lifecycle.task_key.as_deref(), Some("subagent.review"));
        assert_eq!(lifecycle.candidate_identity.as_deref(), Some("candidate"));
        assert_eq!(lifecycle.window, "7d");
        assert!(lifecycle.events);
        assert_eq!(lifecycle.limit, 10);
        assert!(lifecycle.json);

        let shadows = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "shadows",
            "--task-key",
            "subagent.review",
            "--limit",
            "5",
            "--json",
        ])
        .expect("parse shadows");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Shadows(shadows),
        })) = shadows.subcommand
        else {
            panic!("expected model-router shadows subcommand");
        };
        assert_eq!(shadows.task_key.as_deref(), Some("subagent.review"));
        assert_eq!(shadows.limit, 5);
        assert!(shadows.json);
    }

    #[test]
    fn model_router_lifecycle_formats_stats_events_and_json() {
        let report = ModelRouterLifecycleStateReport {
            task_key: Some("subagent.review".to_string()),
            candidate_identity: Some("candidate".to_string()),
            window: "all".to_string(),
            effective_lifecycle: codex_model_router::policy::EffectiveLifecycle::default(),
            promotions: vec![codex_state::ModelRouterLifecyclePromotionRecord {
                task_key: "subagent.review".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                status: "promoted".to_string(),
                rule_id: Some("review".to_string()),
                production_model_provider: Some("openai".to_string()),
                production_model: Some("gpt-5.5".to_string()),
                base_model_provider: Some("openai".to_string()),
                base_model: Some("gpt-5.4".to_string()),
                promoted_at_ms: 20,
                updated_at_ms: 20,
                reason: Some("passed".to_string()),
            }],
            stats: codex_state::ModelRouterLifecycleStatsSummary {
                window_start_ms: None,
                window_end_ms: 30,
                task_key: Some("subagent.review".to_string()),
                candidate_identity: Some("candidate".to_string()),
                totals: codex_state::ModelRouterLifecycleEventCounts {
                    promoted: 1,
                    demoted: 1,
                    evaluating: 1,
                    promotion_blocked: 1,
                    rejected: 1,
                    auto: 2,
                    manual: 1,
                },
                candidates: vec![codex_state::ModelRouterLifecycleCandidateStats {
                    task_key: "subagent.review".to_string(),
                    candidate_identity: "candidate".to_string(),
                    current_status: Some("promoted".to_string()),
                    base_candidate_identity: Some("base".to_string()),
                    rule_id: Some("review".to_string()),
                    production_model_provider: Some("openai".to_string()),
                    production_model: Some("gpt-5.5".to_string()),
                    base_model_provider: Some("openai".to_string()),
                    base_model: Some("gpt-5.4".to_string()),
                    promoted_at_ms: Some(20),
                    updated_at_ms: Some(20),
                    counts: codex_state::ModelRouterLifecycleEventCounts {
                        promoted: 1,
                        demoted: 1,
                        evaluating: 1,
                        promotion_blocked: 1,
                        rejected: 1,
                        auto: 2,
                        manual: 1,
                    },
                    last_event_at_ms: Some(20),
                    last_event_type: Some("promoted".to_string()),
                    last_reason: Some("passed".to_string()),
                }],
            },
            events: vec![codex_state::ModelRouterLifecycleEventRecord {
                id: Some(3),
                created_at_ms: 30,
                event_type: "promotion_blocked".to_string(),
                source: "auto".to_string(),
                task_key: "subagent.review".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                previous_status: Some("demoted".to_string()),
                next_status: Some("demoted".to_string()),
                rule_id: Some("review".to_string()),
                reason: Some("promotion shadow gates failed".to_string()),
                production_model_provider: Some("openai".to_string()),
                production_model: Some("gpt-5.5".to_string()),
                base_model_provider: Some("openai".to_string()),
                base_model: Some("gpt-5.4".to_string()),
                lifecycle_window: Some("all".to_string()),
                shadow_phase: Some("promotion".to_string()),
                shadow_evaluated_count: Some(2),
                shadow_success_count: Some(1),
                shadow_success_rate: Some(0.5),
                shadow_average_score: Some(0.5),
                shadow_average_confidence: Some(1.0),
                shadow_cost_used_usd_micros: Some(100),
                shadow_tokens_used: Some(200),
                shadow_latest_evaluation_id: Some(9),
                shadow_latest_evaluation_at_ms: Some(29),
                failed_gates_json: Some(
                    r#"[{"gate":"min_success_rate","actual":0.5,"threshold":0.9}]"#.to_string(),
                ),
            }],
        };

        let output = format_model_router_lifecycle_report(
            &report,
            ModelRouterLifecycleTimelineDisplay::Shown,
        );
        assert!(output.contains(
            "Events: 1 promoted, 1 demoted, 1 evaluating, 1 rejected, 1 blocked (auto 2, manual 1)"
        ));
        assert!(output.contains("status promoted"));
        assert!(output.contains("failed_gates=min_success_rate"));

        let json = serde_json::to_value(&report).expect("json report");
        assert_eq!(
            json["stats"]["totals"]["promotionBlocked"],
            serde_json::json!(1)
        );
        assert_eq!(
            json["events"][0]["eventType"],
            serde_json::json!("promotion_blocked")
        );
    }

    #[test]
    fn model_router_usage_parses_and_formats() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "usage",
            "--window",
            "7d",
            "--task-key",
            "module.review.review",
            "--group-by",
            "request-kind",
            "--json",
        ])
        .expect("parse usage");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Usage(usage),
        })) = cli.subcommand
        else {
            panic!("expected model-router usage subcommand");
        };
        assert_eq!(usage.window, "7d");
        assert_eq!(usage.task_key.as_deref(), Some("module.review.review"));
        assert_eq!(usage.group_by, ModelRouterUsageGroupByArg::RequestKind);
        assert!(usage.json);

        let report = ModelRouterUsageReport {
            window: "7d".to_string(),
            summary: codex_state::ModelRouterUsageSummary {
                window_start_ms: Some(1),
                window_end_ms: 2,
                task_key: Some("module.review.review".to_string()),
                group_by: codex_state::ModelRouterUsageGroupBy::RequestKind,
                totals: codex_state::ModelRouterUsageTotals {
                    request_count: 3,
                    production_request_count: 2,
                    overhead_request_count: 1,
                    token_usage: codex_protocol::protocol::TokenUsage {
                        input_tokens: 10,
                        cached_input_tokens: 2,
                        output_tokens: 3,
                        reasoning_output_tokens: 1,
                        total_tokens: 13,
                    },
                    savings: codex_model_router::RouterSavings {
                        actual_production_cost_usd_micros: 100,
                        router_overhead_cost_usd_micros: 25,
                        counterfactual_cost_usd_micros: 175,
                        gross_savings_usd_micros: 75,
                        net_savings_usd_micros: 50,
                    },
                    average_price_confidence: 0.4,
                    minimum_price_confidence: 0.0,
                    coverage: codex_state::ModelRouterUsageCoverage {
                        missing_price_rows: 1,
                        low_confidence_price_rows: 1,
                        zero_token_rows: 1,
                        production_rows_missing_actual_cost: 1,
                        production_rows_missing_counterfactual: 1,
                    },
                },
                groups: vec![
                    codex_state::ModelRouterUsageGroup {
                        key: "judge".to_string(),
                        totals: codex_state::ModelRouterUsageTotals {
                            request_count: 1,
                            production_request_count: 0,
                            overhead_request_count: 1,
                            token_usage: codex_protocol::protocol::TokenUsage::default(),
                            savings: codex_model_router::RouterSavings {
                                actual_production_cost_usd_micros: 0,
                                router_overhead_cost_usd_micros: 25,
                                counterfactual_cost_usd_micros: 0,
                                gross_savings_usd_micros: 0,
                                net_savings_usd_micros: -25,
                            },
                            average_price_confidence: 0.0,
                            minimum_price_confidence: 0.0,
                            coverage: codex_state::ModelRouterUsageCoverage {
                                missing_price_rows: 1,
                                low_confidence_price_rows: 0,
                                zero_token_rows: 1,
                                production_rows_missing_actual_cost: 0,
                                production_rows_missing_counterfactual: 0,
                            },
                        },
                    },
                    codex_state::ModelRouterUsageGroup {
                        key: "production".to_string(),
                        totals: codex_state::ModelRouterUsageTotals {
                            request_count: 2,
                            production_request_count: 2,
                            overhead_request_count: 0,
                            token_usage: codex_protocol::protocol::TokenUsage {
                                input_tokens: 10,
                                cached_input_tokens: 2,
                                output_tokens: 3,
                                reasoning_output_tokens: 1,
                                total_tokens: 13,
                            },
                            savings: codex_model_router::RouterSavings {
                                actual_production_cost_usd_micros: 100,
                                router_overhead_cost_usd_micros: 0,
                                counterfactual_cost_usd_micros: 175,
                                gross_savings_usd_micros: 75,
                                net_savings_usd_micros: 75,
                            },
                            average_price_confidence: 0.6,
                            minimum_price_confidence: 0.2,
                            coverage: codex_state::ModelRouterUsageCoverage {
                                missing_price_rows: 0,
                                low_confidence_price_rows: 1,
                                zero_token_rows: 0,
                                production_rows_missing_actual_cost: 1,
                                production_rows_missing_counterfactual: 1,
                            },
                        },
                    },
                ],
            },
        };

        let output = format_model_router_usage_report(&report);
        assert!(output.contains("Model router usage"));
        assert!(output.contains("production $0.000100"));
        assert!(output.contains("net $0.000050"));
        assert!(output.contains(
            "Coverage gaps: missing price 1, low confidence 1, zero-token 1, missing production actual 1, missing production counterfactual 1"
        ));
        let json_output = serde_json::to_string_pretty(&report).expect("json report string");
        assert!(json_output.contains("\"missingPriceRows\": 1"));
        let json = serde_json::to_value(&report).expect("json report");
        assert_eq!(
            json["summary"]["totals"]["requestCount"],
            serde_json::json!(3)
        );
        assert_eq!(
            json["summary"]["totals"]["coverage"]["productionRowsMissingCounterfactual"],
            serde_json::json!(1)
        );
    }

    #[test]
    fn model_router_promote_and_demote_parse() {
        let promote = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "promote",
            "--task-key",
            "subagent.review",
            "--candidate-identity",
            "candidate",
            "--base-candidate-identity",
            "base",
            "--rule-id",
            "review",
            "--reason",
            "passed",
            "--json",
        ])
        .expect("parse promote");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Promote(promote),
        })) = promote.subcommand
        else {
            panic!("expected model-router promote subcommand");
        };
        assert_eq!(promote.task_key, "subagent.review");
        assert_eq!(promote.candidate_identity, "candidate");
        assert_eq!(promote.base_candidate_identity, "base");
        assert_eq!(promote.rule_id.as_deref(), Some("review"));
        assert_eq!(promote.reason.as_deref(), Some("passed"));
        assert!(promote.json);

        let demote = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "demote",
            "--task-key",
            "subagent.review",
            "--candidate-identity",
            "candidate",
            "--reason",
            "failed",
            "--json",
        ])
        .expect("parse demote");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand: ModelRouterSubcommand::Demote(demote),
        })) = demote.subcommand
        else {
            panic!("expected model-router demote subcommand");
        };
        assert_eq!(demote.task_key, "subagent.review");
        assert_eq!(demote.candidate_identity, "candidate");
        assert_eq!(demote.reason.as_deref(), Some("failed"));
        assert!(demote.json);
    }

    #[test]
    fn model_router_report_show_and_apply_parse() {
        let show = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "report",
            "show",
            "/tmp/report.json",
            "--json",
        ])
        .expect("parse show");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand:
                ModelRouterSubcommand::Report(ModelRouterReportCli {
                    subcommand: ModelRouterReportSubcommand::Show(show),
                }),
        })) = show.subcommand
        else {
            panic!("expected model-router report show subcommand");
        };
        assert_eq!(show.path, PathBuf::from("/tmp/report.json"));
        assert!(show.json);

        let apply = MultitoolCli::try_parse_from([
            "codex",
            "model-router",
            "report",
            "apply",
            "/tmp/report.json",
            "--dry-run",
            "--json",
        ])
        .expect("parse apply");
        let Some(Subcommand::ModelRouter(ModelRouterCli {
            subcommand:
                ModelRouterSubcommand::Report(ModelRouterReportCli {
                    subcommand: ModelRouterReportSubcommand::Apply(apply),
                }),
        })) = apply.subcommand
        else {
            panic!("expected model-router report apply subcommand");
        };
        assert_eq!(apply.path, PathBuf::from("/tmp/report.json"));
        assert!(apply.dry_run);
        assert!(apply.json);
    }

    #[test]
    fn responses_subcommand_is_not_registered() {
        let command = MultitoolCli::command();
        assert!(
            command
                .get_subcommands()
                .all(|subcommand| subcommand.get_name() != "responses")
        );
    }

    fn help_from_args(args: &[&str]) -> String {
        let err = MultitoolCli::try_parse_from(args).expect_err("help should short-circuit");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        err.to_string()
    }

    #[test]
    fn plugin_marketplace_help_uses_plugin_namespace() {
        let help = help_from_args(&["codex", "plugin", "marketplace", "--help"]);
        assert!(
            help.contains("Usage: codex plugin marketplace [OPTIONS] <COMMAND>"),
            "{help}"
        );

        for (subcommand, usage) in [
            ("add", "Usage: codex plugin marketplace add"),
            ("upgrade", "Usage: codex plugin marketplace upgrade"),
            ("remove", "Usage: codex plugin marketplace remove"),
        ] {
            let help = help_from_args(&["codex", "plugin", "marketplace", subcommand, "--help"]);
            assert!(help.contains(usage), "{help}");
        }
    }

    #[test]
    fn plugin_marketplace_add_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "add", "owner/repo"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn plugin_marketplace_upgrade_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "upgrade", "debug"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn plugin_marketplace_remove_parses_under_plugin() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "plugin", "marketplace", "remove", "debug"])
                .expect("parse");

        assert!(matches!(cli.subcommand, Some(Subcommand::Plugin(_))));
    }

    #[test]
    fn marketplace_no_longer_parses_at_top_level() {
        let add_cli = MultitoolCli::try_parse_from(["codex", "marketplace", "add", "owner/repo"])
            .expect("parse");
        assert_eq!(
            add_cli.interactive.prompt,
            vec![
                "marketplace".to_string(),
                "add".to_string(),
                "owner/repo".to_string(),
            ]
        );
        assert!(add_cli.subcommand.is_none());

        let upgrade_cli =
            MultitoolCli::try_parse_from(["codex", "marketplace", "upgrade", "debug"])
                .expect("parse");
        assert_eq!(
            upgrade_cli.interactive.prompt,
            vec![
                "marketplace".to_string(),
                "upgrade".to_string(),
                "debug".to_string(),
            ]
        );
        assert!(upgrade_cli.subcommand.is_none());

        let remove_cli = MultitoolCli::try_parse_from(["codex", "marketplace", "remove", "debug"])
            .expect("parse");
        assert_eq!(
            remove_cli.interactive.prompt,
            vec![
                "marketplace".to_string(),
                "remove".to_string(),
                "debug".to_string(),
            ]
        );
        assert!(remove_cli.subcommand.is_none());
    }

    fn sample_exit_info(conversation_id: Option<&str>, thread_name: Option<&str>) -> AppExitInfo {
        let token_usage = TokenUsage {
            output_tokens: 2,
            total_tokens: 2,
            ..Default::default()
        };
        AppExitInfo {
            token_usage,
            thread_id: conversation_id
                .map(ThreadId::from_string)
                .map(Result::unwrap),
            thread_name: thread_name.map(str::to_string),
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        }
    }

    #[test]
    fn format_exit_messages_skips_zero_usage() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            thread_id: None,
            thread_name: None,
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert!(lines.is_empty());
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_without_color() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            /*thread_name*/ None,
        );
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run codex resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_applies_color_when_enabled() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            /*thread_name*/ None,
        );
        let lines = format_exit_messages(exit_info, /*color_enabled*/ true);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\u{1b}[36m"));
    }

    #[test]
    fn format_exit_messages_uses_id_even_when_thread_has_name() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            Some("my-thread"),
        );
        let lines = format_exit_messages(exit_info, /*color_enabled*/ false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run codex resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn resume_model_flag_applies_when_no_root_flags() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "-m", "gpt-5.1-test"].as_ref());

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn resume_picker_logic_none_and_not_last() {
        let interactive = finalize_resume_from_args(["codex", "resume"].as_ref());
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_picker_logic_last() {
        let interactive = finalize_resume_from_args(["codex", "resume", "--last"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_picker_logic_with_session_id() {
        let interactive = finalize_resume_from_args(["codex", "resume", "1234"].as_ref());
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("1234"));
        assert!(!interactive.resume_show_all);
    }

    #[test]
    fn resume_all_flag_sets_show_all() {
        let interactive = finalize_resume_from_args(["codex", "resume", "--all"].as_ref());
        assert!(interactive.resume_picker);
        assert!(interactive.resume_show_all);
    }

    #[test]
    fn resume_include_non_interactive_flag_sets_source_filter_override() {
        let interactive =
            finalize_resume_from_args(["codex", "resume", "--include-non-interactive"].as_ref());

        assert!(interactive.resume_picker);
        assert!(interactive.resume_include_non_interactive);
    }

    #[test]
    fn resume_merges_option_flags() {
        let interactive = finalize_resume_from_args(
            [
                "codex",
                "resume",
                "sid",
                "--oss",
                "--search",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "-m",
                "gpt-5.1-test",
                "-p",
                "my-profile",
                "-C",
                "/tmp",
                "-i",
                "/tmp/a.png,/tmp/b.png",
            ]
            .as_ref(),
        );

        assert_eq!(interactive.model.as_deref(), Some("gpt-5.1-test"));
        assert!(interactive.oss);
        assert_eq!(interactive.config_profile.as_deref(), Some("my-profile"));
        assert_matches!(
            interactive.sandbox_mode,
            Some(codex_utils_cli::SandboxModeCliArg::WorkspaceWrite)
        );
        assert_matches!(
            interactive.approval_policy,
            Some(codex_utils_cli::ApprovalModeCliArg::OnRequest)
        );
        assert_eq!(
            interactive.cwd.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert!(interactive.web_search);
        let has_a = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/a.png"));
        let has_b = interactive
            .images
            .iter()
            .any(|p| p == std::path::Path::new("/tmp/b.png"));
        assert!(has_a && has_b);
        assert!(!interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id.as_deref(), Some("sid"));
    }

    #[test]
    fn resume_merges_dangerously_bypass_flag() {
        let interactive = finalize_resume_from_args(
            [
                "codex",
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
            .as_ref(),
        );
        assert!(interactive.dangerously_bypass_approvals_and_sandbox);
        assert!(interactive.resume_picker);
        assert!(!interactive.resume_last);
        assert_eq!(interactive.resume_session_id, None);
    }

    #[test]
    fn fork_picker_logic_none_and_not_last() {
        let interactive = finalize_fork_from_args(["codex", "fork"].as_ref());
        assert!(interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_picker_logic_last() {
        let interactive = finalize_fork_from_args(["codex", "fork", "--last"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(interactive.fork_last);
        assert_eq!(interactive.fork_session_id, None);
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_picker_logic_with_session_id() {
        let interactive = finalize_fork_from_args(["codex", "fork", "1234"].as_ref());
        assert!(!interactive.fork_picker);
        assert!(!interactive.fork_last);
        assert_eq!(interactive.fork_session_id.as_deref(), Some("1234"));
        assert!(!interactive.fork_show_all);
    }

    #[test]
    fn fork_all_flag_sets_show_all() {
        let interactive = finalize_fork_from_args(["codex", "fork", "--all"].as_ref());
        assert!(interactive.fork_picker);
        assert!(interactive.fork_show_all);
    }

    #[test]
    fn app_server_analytics_default_disabled_without_flag() {
        let app_server = app_server_from_args(["codex", "app-server"].as_ref());
        assert!(!app_server.analytics_default_enabled);
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::Stdio
        );
    }

    #[test]
    fn app_server_analytics_default_enabled_with_flag() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--analytics-default-enabled"].as_ref());
        assert!(app_server.analytics_default_enabled);
    }

    #[test]
    fn remote_flag_parses_for_interactive_root() {
        let cli = MultitoolCli::try_parse_from(["codex", "--remote", "ws://127.0.0.1:4500"])
            .expect("parse");
        assert_eq!(cli.remote.remote.as_deref(), Some("ws://127.0.0.1:4500"));
    }

    #[test]
    fn remote_auth_token_env_flag_parses_for_interactive_root() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "--remote-auth-token-env",
            "CODEX_REMOTE_AUTH_TOKEN",
            "--remote",
            "ws://127.0.0.1:4500",
        ])
        .expect("parse");
        assert_eq!(
            cli.remote.remote_auth_token_env.as_deref(),
            Some("CODEX_REMOTE_AUTH_TOKEN")
        );
    }

    #[test]
    fn remote_flag_parses_for_resume_subcommand() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "resume", "--remote", "ws://127.0.0.1:4500"])
                .expect("parse");
        let Subcommand::Resume(ResumeCommand { remote, .. }) =
            cli.subcommand.expect("resume present")
        else {
            panic!("expected resume subcommand");
        };
        assert_eq!(remote.remote.as_deref(), Some("ws://127.0.0.1:4500"));
    }

    #[test]
    fn reject_remote_mode_for_non_interactive_subcommands() {
        let err = reject_remote_mode_for_subcommand(
            Some("127.0.0.1:4500"),
            /*remote_auth_token_env*/ None,
            "exec",
        )
        .expect_err("non-interactive subcommands should reject --remote");
        assert!(
            err.to_string()
                .contains("only supported for interactive TUI commands")
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_non_interactive_subcommands() {
        let err = reject_remote_mode_for_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            "exec",
        )
        .expect_err("non-interactive subcommands should reject --remote-auth-token-env");
        assert!(
            err.to_string()
                .contains("only supported for interactive TUI commands")
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_generate_internal_json_schema() {
        let subcommand =
            AppServerSubcommand::GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand {
                out_dir: PathBuf::from("/tmp/out"),
            });
        let err = reject_remote_mode_for_app_server_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("non-interactive app-server subcommands should reject --remote-auth-token-env");
        assert!(err.to_string().contains("generate-internal-json-schema"));
    }

    #[test]
    fn read_remote_auth_token_from_env_var_reports_missing_values() {
        let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
            Err(std::env::VarError::NotPresent)
        })
        .expect_err("missing env vars should be rejected");
        assert!(err.to_string().contains("is not set"));
    }

    #[test]
    fn read_remote_auth_token_from_env_var_trims_values() {
        let auth_token =
            read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
                Ok("  bearer-token  ".to_string())
            })
            .expect("env var should parse");
        assert_eq!(auth_token, "bearer-token");
    }

    #[test]
    fn read_remote_auth_token_from_env_var_rejects_empty_values() {
        let err = read_remote_auth_token_from_env_var_with("CODEX_REMOTE_AUTH_TOKEN", |_| {
            Ok(" \n\t ".to_string())
        })
        .expect_err("empty env vars should be rejected");
        assert!(err.to_string().contains("is empty"));
    }

    #[test]
    fn app_server_listen_websocket_url_parses() {
        let app_server = app_server_from_args(
            ["codex", "app-server", "--listen", "ws://127.0.0.1:4500"].as_ref(),
        );
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::WebSocket {
                bind_address: "127.0.0.1:4500".parse().expect("valid socket address"),
            }
        );
    }

    #[test]
    fn app_server_listen_stdio_url_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--listen", "stdio://"].as_ref());
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::Stdio
        );
    }

    #[test]
    fn app_server_listen_unix_socket_url_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "--listen", "unix://"].as_ref());
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::UnixSocket {
                socket_path: default_app_server_socket_path()
            }
        );
    }

    #[test]
    fn app_server_listen_unix_socket_path_parses() {
        let app_server = app_server_from_args(
            ["codex", "app-server", "--listen", "unix:///tmp/codex.sock"].as_ref(),
        );
        assert_eq!(
            app_server.listen,
            codex_app_server::AppServerTransport::UnixSocket {
                socket_path: AbsolutePathBuf::from_absolute_path("/tmp/codex.sock")
                    .expect("absolute path should parse")
            }
        );
    }

    #[test]
    fn app_server_listen_off_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "--listen", "off"].as_ref());
        assert_eq!(app_server.listen, codex_app_server::AppServerTransport::Off);
    }

    #[test]
    fn app_server_listen_invalid_url_fails_to_parse() {
        let parse_result =
            MultitoolCli::try_parse_from(["codex", "app-server", "--listen", "http://foo"]);
        assert!(parse_result.is_err());
    }

    #[test]
    fn app_server_proxy_subcommand_parses() {
        let app_server = app_server_from_args(["codex", "app-server", "proxy"].as_ref());
        assert!(matches!(
            app_server.subcommand,
            Some(AppServerSubcommand::Proxy(AppServerProxyCommand {
                socket_path: None
            }))
        ));
    }

    #[test]
    fn app_server_proxy_sock_path_parses() {
        let app_server =
            app_server_from_args(["codex", "app-server", "proxy", "--sock", "codex.sock"].as_ref());
        let Some(AppServerSubcommand::Proxy(proxy)) = app_server.subcommand else {
            panic!("expected proxy subcommand");
        };
        assert_eq!(
            proxy.socket_path,
            Some(
                AbsolutePathBuf::relative_to_current_dir("codex.sock")
                    .expect("relative path should resolve")
            )
        );
    }

    #[test]
    fn reject_remote_auth_token_env_for_app_server_proxy() {
        let subcommand = AppServerSubcommand::Proxy(AppServerProxyCommand { socket_path: None });
        let err = reject_remote_mode_for_app_server_subcommand(
            /*remote*/ None,
            Some("CODEX_REMOTE_AUTH_TOKEN"),
            Some(&subcommand),
        )
        .expect_err("app-server proxy should reject --remote-auth-token-env");
        assert!(err.to_string().contains("app-server proxy"));
    }

    #[test]
    fn app_server_capability_token_flags_parse() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--ws-auth",
                "capability-token",
                "--ws-token-file",
                "/tmp/codex-token",
            ]
            .as_ref(),
        );
        assert_eq!(
            app_server.auth.ws_auth,
            Some(codex_app_server::WebsocketAuthCliMode::CapabilityToken)
        );
        assert_eq!(
            app_server.auth.ws_token_file,
            Some(PathBuf::from("/tmp/codex-token"))
        );
    }

    #[test]
    fn app_server_signed_bearer_flags_parse() {
        let app_server = app_server_from_args(
            [
                "codex",
                "app-server",
                "--ws-auth",
                "signed-bearer-token",
                "--ws-shared-secret-file",
                "/tmp/codex-secret",
                "--ws-issuer",
                "issuer",
                "--ws-audience",
                "audience",
                "--ws-max-clock-skew-seconds",
                "9",
            ]
            .as_ref(),
        );
        assert_eq!(
            app_server.auth.ws_auth,
            Some(codex_app_server::WebsocketAuthCliMode::SignedBearerToken)
        );
        assert_eq!(
            app_server.auth.ws_shared_secret_file,
            Some(PathBuf::from("/tmp/codex-secret"))
        );
        assert_eq!(app_server.auth.ws_issuer.as_deref(), Some("issuer"));
        assert_eq!(app_server.auth.ws_audience.as_deref(), Some("audience"));
        assert_eq!(app_server.auth.ws_max_clock_skew_seconds, Some(9));
    }

    #[test]
    fn app_server_rejects_removed_insecure_non_loopback_flag() {
        let parse_result = MultitoolCli::try_parse_from([
            "codex",
            "app-server",
            "--allow-unauthenticated-non-loopback-ws",
        ]);
        assert!(parse_result.is_err());
    }

    #[test]
    fn api_catalog_parses_mcp_detail() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "api", "--mcp-detail", "tools-and-auth-only"])
                .expect("parse should succeed");
        let Some(Subcommand::Api(api)) = cli.subcommand else {
            panic!("expected api subcommand");
        };
        assert_eq!(
            api.mcp_detail,
            api_catalog_cmd::ApiCatalogMcpDetail::ToolsAndAuthOnly
        );
    }

    #[test]
    fn features_enable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["codex", "features", "enable", "unified_exec"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Enable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features enable");
        };
        assert_eq!(feature, "unified_exec");
    }

    #[test]
    fn features_disable_parses_feature_name() {
        let cli = MultitoolCli::try_parse_from(["codex", "features", "disable", "shell_tool"])
            .expect("parse should succeed");
        let Some(Subcommand::Features(FeaturesCli { sub })) = cli.subcommand else {
            panic!("expected features subcommand");
        };
        let FeaturesSubcommand::Disable(FeatureSetArgs { feature }) = sub else {
            panic!("expected features disable");
        };
        assert_eq!(feature, "shell_tool");
    }

    #[test]
    fn implement_parses_enable_and_max_cycles() {
        let cli = MultitoolCli::try_parse_from(["codex", "implement", "enable", "--max-cycles=4"])
            .expect("parse should succeed");
        let Some(Subcommand::Implement(args)) = cli.subcommand else {
            panic!("expected implement subcommand");
        };
        let ImplementCommand::Enable(action_args) = args.command else {
            panic!("expected implement enable command");
        };
        assert_eq!(action_args.max_cycles, Some(4));
    }

    #[test]
    fn implement_parses_implicit_and_max_cycles() {
        let cli =
            MultitoolCli::try_parse_from(["codex", "implement", "implicit", "--max-cycles=2"])
                .expect("parse should succeed");
        let Some(Subcommand::Implement(args)) = cli.subcommand else {
            panic!("expected implement subcommand");
        };
        let ImplementCommand::Implicit(action_args) = args.command else {
            panic!("expected implement implicit command");
        };
        assert_eq!(action_args.max_cycles, Some(2));
    }

    #[test]
    fn implement_parses_disable() {
        let cli = MultitoolCli::try_parse_from(["codex", "implement", "disable"])
            .expect("parse should succeed");
        let Some(Subcommand::Implement(args)) = cli.subcommand else {
            panic!("expected implement subcommand");
        };
        let ImplementCommand::Disable(action_args) = args.command else {
            panic!("expected implement disable command");
        };
        assert_eq!(action_args.max_cycles, None);
    }

    #[test]
    fn unknown_command_is_treated_as_prompt() {
        let cli = MultitoolCli::try_parse_from(["codex", "no-such-command"]).expect("parse");
        assert_eq!(cli.interactive.prompt, vec!["no-such-command".to_string()]);
        assert!(cli.subcommand.is_none());
    }

    #[test]
    fn sandbox_linux_accepts_config_compatibility_flags() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "sandbox",
            "linux",
            "--permissions-profile",
            "workspace",
            "-C",
            "/tmp/work",
            "--include-managed-config",
            "echo",
            "hi",
        ])
        .expect("parse should succeed");
        let Some(Subcommand::Sandbox(sandbox_args)) = cli.subcommand else {
            panic!("expected sandbox subcommand");
        };
        let SandboxCommand::Linux(cmd) = sandbox_args.cmd else {
            panic!("expected linux sandbox command");
        };

        assert_eq!(
            cmd.sandbox_overrides.permissions_profile.as_deref(),
            Some("workspace")
        );
        assert_eq!(cmd.sandbox_overrides.cwd, Some(PathBuf::from("/tmp/work")));
        assert!(cmd.sandbox_overrides.include_managed_config);
        assert_eq!(cmd.command, vec!["echo".to_string(), "hi".to_string()]);
    }

    #[test]
    fn interactive_prompt_stops_before_following_flags() {
        let cli = MultitoolCli::try_parse_from(["codex", "hello", "--search"])
            .expect("parse should succeed");

        assert_eq!(cli.interactive.prompt, vec!["hello".to_string()]);
        assert!(cli.interactive.web_search);
        assert!(cli.subcommand.is_none());
    }

    #[test]
    fn workflow_alias_prompt_passthrough_preserves_unknown_flags_for_late_promotion() {
        let (cli, deferred_error) = parse_multitool_cli_from([
            "codex",
            "--enable",
            "workflows",
            "-C",
            "/tmp/repo",
            "patch-impact",
            "--base-ref",
            "HEAD",
            "--include-untracked",
            "--max-files",
            "20",
        ])
        .expect("parse should fall back to prompt passthrough");

        assert!(deferred_error.is_some());
        assert_eq!(
            cli.interactive.prompt,
            vec![
                "patch-impact".to_string(),
                "--base-ref".to_string(),
                "HEAD".to_string(),
                "--include-untracked".to_string(),
                "--max-files".to_string(),
                "20".to_string(),
            ]
        );
        assert!(cli.subcommand.is_none());
    }

    #[test]
    fn workflow_quality_hook_hidden_subcommand_parses() {
        let cli = MultitoolCli::try_parse_from(["codex", "workflow-quality-hook"])
            .expect("parse should succeed");
        assert!(matches!(
            cli.subcommand,
            Some(Subcommand::WorkflowQualityHook)
        ));
        assert!(cli.interactive.prompt.is_empty());
    }

    #[test]
    fn native_workflow_hidden_subcommand_parses() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "native-workflow",
            "run",
            "dev-cycle",
            "--input",
            "{}",
        ])
        .expect("parse should succeed");
        let Some(Subcommand::NativeWorkflow(native_workflow_cli)) = cli.subcommand else {
            panic!("expected native workflow subcommand");
        };

        assert!(matches!(
            native_workflow_cli.command,
            crate::native_workflow_cmd::NativeWorkflowSubcommand::Run { .. }
        ));
        assert!(cli.interactive.prompt.is_empty());
    }

    #[test]
    fn workflow_hidden_stage_session_id_parses() {
        let cli = MultitoolCli::try_parse_from([
            "codex",
            "workflow",
            "--stage-session-id",
            "session-123",
            "list",
        ])
        .expect("parse should succeed");
        let Some(Subcommand::Workflow(workflow_cli)) = cli.subcommand else {
            panic!("expected workflow subcommand");
        };

        assert_eq!(
            workflow_cli.stage_session_id,
            Some("session-123".to_string())
        );
        assert_eq!(workflow_cli.args, vec!["list".to_string()]);
    }

    #[test]
    fn workflow_alias_prompt_is_promoted_to_workflow_subcommand() {
        let root = PathBuf::from("/tmp/workflows");
        let workflow_id = "reports/jira-summary";
        let path = workflow_id
            .split('/')
            .fold(root.clone(), |path, component| path.join(component));
        let workflow = codex_workflows::WorkflowSummary {
            id: workflow_id.to_string(),
            engine: codex_workflows::WorkflowEngine::TypeScript,
            command: Some("jira-summary".to_string()),
            title: Some("Jira Summary".to_string()),
            user_description: Some("Prepare a focused workflow report".to_string()),
            search_terms: vec!["report".to_string()],
            command_option_hints: Vec::new(),
            input_schema: None,
            root_label: "global".to_string(),
            root_kind: codex_workflows::WorkflowRootKind::Global,
            root_path: root.clone(),
            path: path.clone(),
            workflow_yaml_path: path.join("workflow.yaml"),
            mention_target: codex_workflows::mention_target(&root, workflow_id).unwrap(),
            validation: codex_workflows::WorkflowValidation {
                status: codex_workflows::WorkflowValidationStatus::Valid,
                findings: Vec::new(),
            },
            repair_mode: "full".to_string(),
        };
        let mut prompt = vec![
            "jira-summary".to_string(),
            "--project".to_string(),
            "COD".to_string(),
        ];

        let subcommand = promote_workflow_alias_from_prompt(&mut prompt, &[workflow])
            .expect("expected workflow promotion");
        let Subcommand::Workflow(workflow_cli) = subcommand else {
            panic!("expected workflow subcommand");
        };

        assert_eq!(
            workflow_cli.args,
            vec![
                "jira-summary".to_string(),
                "--project".to_string(),
                "COD".to_string(),
            ]
        );
        assert!(prompt.is_empty());
    }

    #[test]
    fn rust_workflow_alias_prompt_is_promoted_to_workflow_subcommand() {
        let root = PathBuf::from("/tmp/workflows/.native-workflows");
        let path = root.join("dev-cycle");
        let workflow = codex_workflows::WorkflowSummary {
            id: "dev-cycle".to_string(),
            engine: codex_workflows::WorkflowEngine::Rust,
            command: Some("dev-cycle".to_string()),
            title: Some("Development Cycle Preview".to_string()),
            user_description: Some("Preview a development cycle".to_string()),
            search_terms: vec!["development".to_string()],
            command_option_hints: Vec::new(),
            input_schema: None,
            root_label: "native".to_string(),
            root_kind: codex_workflows::WorkflowRootKind::Global,
            root_path: root.clone(),
            path: path.clone(),
            workflow_yaml_path: path.join("workflow.yaml"),
            mention_target: codex_workflows::mention_target(&root, "dev-cycle").unwrap(),
            validation: codex_workflows::WorkflowValidation {
                status: codex_workflows::WorkflowValidationStatus::Valid,
                findings: Vec::new(),
            },
            repair_mode: "full".to_string(),
        };
        let mut prompt = vec![
            "dev-cycle".to_string(),
            "--stage-tests".to_string(),
            "off".to_string(),
        ];

        let subcommand = promote_workflow_alias_from_prompt(&mut prompt, &[workflow])
            .expect("expected workflow promotion");
        let Subcommand::Workflow(workflow_cli) = subcommand else {
            panic!("expected workflow subcommand");
        };

        assert_eq!(
            workflow_cli.args,
            vec![
                "dev-cycle".to_string(),
                "--stage-tests".to_string(),
                "off".to_string(),
            ]
        );
        assert!(prompt.is_empty());
    }

    #[test]
    fn feature_toggles_known_features_generate_overrides() {
        let toggles = FeatureToggles {
            enable: vec!["web_search_request".to_string()],
            disable: vec!["unified_exec".to_string()],
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec![
                "features.web_search_request=true".to_string(),
                "features.unified_exec=false".to_string(),
            ]
        );
    }

    #[test]
    fn feature_toggles_accept_legacy_linux_sandbox_flag() {
        let toggles = FeatureToggles {
            enable: vec!["use_linux_sandbox_bwrap".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec!["features.use_linux_sandbox_bwrap=true".to_string(),]
        );
    }

    #[test]
    fn feature_toggles_accept_removed_image_detail_original_flag() {
        let toggles = FeatureToggles {
            enable: vec!["image_detail_original".to_string()],
            disable: Vec::new(),
        };
        let overrides = toggles.to_overrides().expect("valid features");
        assert_eq!(
            overrides,
            vec!["features.image_detail_original=true".to_string(),]
        );
    }

    #[test]
    fn feature_toggles_unknown_feature_errors() {
        let toggles = FeatureToggles {
            enable: vec!["does_not_exist".to_string()],
            disable: Vec::new(),
        };
        let err = toggles
            .to_overrides()
            .expect_err("feature should be rejected");
        assert_eq!(err.to_string(), "Unknown feature flag: does_not_exist");
    }
}
