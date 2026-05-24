use super::CoreShellActionProvider;
use super::InterceptedExecPolicyContext;
use super::ParsedShellCommand;
use super::commands_for_intercepted_exec_policy;
use super::evaluate_intercepted_exec_policy;
use super::extract_shell_script;
use super::join_program_and_argv;
use super::map_exec_result;
use crate::config::Constrained;
use crate::sandboxing::SandboxPermissions;
use crate::session::tests::make_session_and_context;
use anyhow::Context;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_config::HookEventsToml;
use codex_config::HookHandlerConfig;
use codex_config::MatcherGroup;
use codex_execpolicy::Decision;
use codex_execpolicy::Evaluation;
use codex_execpolicy::PolicyParser;
use codex_execpolicy::RuleMatch;
use codex_hooks::Hooks;
use codex_hooks::HooksConfig;
use codex_plugin::PluginHookSource;
use codex_plugin::PluginId;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::FileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_protocol::protocol::GuardianCommandSource;
use codex_protocol::protocol::SandboxPolicy;
use codex_sandboxing::SandboxType;
use codex_shell_escalation::EscalationExecution;
use codex_shell_escalation::EscalationPermissions;
use codex_shell_escalation::ExecResult;
use codex_shell_escalation::ResolvedPermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::RwLock;

fn host_absolute_path(segments: &[&str]) -> String {
    let mut path = if cfg!(windows) {
        PathBuf::from(r"C:\")
    } else {
        PathBuf::from("/")
    };
    for segment in segments {
        path.push(segment);
    }
    path.to_string_lossy().into_owned()
}

fn starlark_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn read_only_file_system_sandbox_policy() -> FileSystemSandboxPolicy {
    FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Read,
    }])
}

fn permission_profile_from_sandbox_policy(sandbox_policy: &SandboxPolicy) -> PermissionProfile {
    PermissionProfile::from_legacy_sandbox_policy(sandbox_policy)
}

fn test_sandbox_cwd() -> AbsolutePathBuf {
    AbsolutePathBuf::try_from(host_absolute_path(&["workspace"])).unwrap()
}

#[test]
fn execve_prompt_rejection_keeps_prefix_rules_on_rules_flag() {
    assert_eq!(
        super::execve_prompt_is_rejected_by_policy(
            AskForApproval::Granular(GranularApprovalConfig {
                sandbox_approval: true,
                rules: false,
                skill_approval: true,
                request_permissions: true,
                mcp_elicitations: true,
            }),
            &super::DecisionSource::PrefixRule,
        ),
        Some("approval required by policy rule, but AskForApproval::Granular.rules is false"),
    );
}

#[test]
fn execve_prompt_rejection_keeps_unmatched_commands_on_sandbox_flag() {
    assert_eq!(
        super::execve_prompt_is_rejected_by_policy(
            AskForApproval::Granular(GranularApprovalConfig {
                sandbox_approval: false,
                rules: true,
                skill_approval: true,
                request_permissions: true,
                mcp_elicitations: true,
            }),
            &super::DecisionSource::UnmatchedCommandFallback,
        ),
        Some("approval required by policy, but AskForApproval::Granular.sandbox_approval is false"),
    );
}

#[test]
fn approval_sandbox_permissions_only_downgrades_preapproved_additional_permissions() {
    assert_eq!(
        super::approval_sandbox_permissions(
            SandboxPermissions::WithAdditionalPermissions,
            /*additional_permissions_preapproved*/ true
        ),
        SandboxPermissions::UseDefault,
    );
    assert_eq!(
        super::approval_sandbox_permissions(
            SandboxPermissions::WithAdditionalPermissions,
            /*additional_permissions_preapproved*/ false
        ),
        SandboxPermissions::WithAdditionalPermissions,
    );
    assert_eq!(
        super::approval_sandbox_permissions(
            SandboxPermissions::RequireEscalated,
            /*additional_permissions_preapproved*/ true
        ),
        SandboxPermissions::RequireEscalated,
    );
}

#[test]
fn extract_shell_script_preserves_login_flag() {
    assert_eq!(
        extract_shell_script(&["/bin/zsh".into(), "-lc".into(), "echo hi".into()]).unwrap(),
        ParsedShellCommand {
            program: "/bin/zsh".to_string(),
            script: "echo hi".to_string(),
            login: true,
        }
    );
    assert_eq!(
        extract_shell_script(&["/bin/zsh".into(), "-c".into(), "echo hi".into()]).unwrap(),
        ParsedShellCommand {
            program: "/bin/zsh".to_string(),
            script: "echo hi".to_string(),
            login: false,
        }
    );
}

#[test]
fn extract_shell_script_supports_wrapped_command_prefixes() {
    assert_eq!(
        extract_shell_script(&[
            "/usr/bin/env".into(),
            "CODEX_EXECVE_WRAPPER=1".into(),
            "/bin/zsh".into(),
            "-lc".into(),
            "echo hello".into()
        ])
        .unwrap(),
        ParsedShellCommand {
            program: "/bin/zsh".to_string(),
            script: "echo hello".to_string(),
            login: true,
        }
    );

    assert_eq!(
        extract_shell_script(&[
            "sandbox-exec".into(),
            "-p".into(),
            "sandbox_policy".into(),
            "/bin/zsh".into(),
            "-c".into(),
            "pwd".into(),
        ])
        .unwrap(),
        ParsedShellCommand {
            program: "/bin/zsh".to_string(),
            script: "pwd".to_string(),
            login: false,
        }
    );
}

#[test]
fn extract_shell_script_rejects_unsupported_shell_invocation() {
    let err = extract_shell_script(&[
        "sandbox-exec".into(),
        "-fc".into(),
        "echo not supported".into(),
    ])
    .unwrap_err();
    assert!(matches!(err, super::ToolError::Rejected(_)));
    assert_eq!(
        match err {
            super::ToolError::Rejected(reason) => reason,
            _ => "".to_string(),
        },
        "unexpected shell command format for zsh-fork execution"
    );
}

#[test]
fn join_program_and_argv_replaces_original_argv_zero() {
    assert_eq!(
        join_program_and_argv(
            &AbsolutePathBuf::from_absolute_path("/tmp/tool").unwrap(),
            &["./tool".into(), "--flag".into(), "value".into()],
        ),
        vec!["/tmp/tool", "--flag", "value"]
    );
    assert_eq!(
        join_program_and_argv(
            &AbsolutePathBuf::from_absolute_path("/tmp/tool").unwrap(),
            &["./tool".into()]
        ),
        vec!["/tmp/tool"]
    );
}

#[test]
fn commands_for_intercepted_exec_policy_parses_plain_shell_wrappers() {
    let program = AbsolutePathBuf::try_from(host_absolute_path(&["bin", "bash"])).unwrap();
    let candidate_commands = commands_for_intercepted_exec_policy(
        &program,
        &["not-bash".into(), "-lc".into(), "git status && pwd".into()],
    );

    assert_eq!(
        candidate_commands.commands,
        vec![
            vec!["git".to_string(), "status".to_string()],
            vec!["pwd".to_string()],
        ]
    );
    assert!(!candidate_commands.used_complex_parsing);
}

#[test]
fn map_exec_result_preserves_stdout_and_stderr() {
    let out = map_exec_result(
        SandboxType::None,
        ExecResult {
            exit_code: 0,
            stdout: "out".to_string(),
            stderr: "err".to_string(),
            output: "outerr".to_string(),
            duration: Duration::from_millis(1),
            timed_out: false,
        },
    )
    .unwrap();

    assert_eq!(out.stdout.text, "out");
    assert_eq!(out.stderr.text, "err");
    assert_eq!(out.aggregated_output.text, "outerr");
}

#[test]
fn shell_request_escalation_execution_is_explicit() {
    let requested_permissions = AdditionalPermissionProfile {
        file_system: Some(FileSystemPermissions::from_read_write_roots(
            /*read*/ None,
            Some(vec![
                AbsolutePathBuf::from_absolute_path("/tmp/output").unwrap(),
            ]),
        )),
        ..Default::default()
    };
    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path("/tmp/original/output").unwrap(),
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path("/tmp/secret").unwrap(),
            },
            access: FileSystemAccessMode::None,
        },
    ]);
    let network_sandbox_policy = NetworkSandboxPolicy::Restricted;
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        network_sandbox_policy,
    );

    assert_eq!(
        CoreShellActionProvider::shell_request_escalation_execution(
            crate::sandboxing::SandboxPermissions::UseDefault,
            &permission_profile,
            /*additional_permissions*/ None,
        ),
        EscalationExecution::TurnDefault,
    );
    assert_eq!(
        CoreShellActionProvider::shell_request_escalation_execution(
            crate::sandboxing::SandboxPermissions::RequireEscalated,
            &permission_profile,
            /*additional_permissions*/ None,
        ),
        EscalationExecution::Unsandboxed,
    );
    assert_eq!(
        CoreShellActionProvider::shell_request_escalation_execution(
            crate::sandboxing::SandboxPermissions::WithAdditionalPermissions,
            &permission_profile,
            Some(&requested_permissions),
        ),
        EscalationExecution::Permissions(EscalationPermissions::ResolvedPermissionProfile(
            ResolvedPermissionProfile { permission_profile },
        )),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn execve_permission_request_hook_returns_allow_decision() -> anyhow::Result<()> {
    let (session, mut turn_context) = make_session_and_context().await;
    std::fs::create_dir_all(&turn_context.config.codex_home)
        .context("recreate codex home for hook fixtures")?;
    let script_path = turn_context
        .config
        .codex_home
        .join("permission_request_hook.py");
    let log_path = turn_context
        .config
        .codex_home
        .join("permission_request_hook_log.jsonl");
    std::fs::write(
        &script_path,
        format!(
            r#"import pathlib
import sys

pathlib.Path({log_path:?}).write_text(sys.stdin.read(), encoding="utf-8")
print({response:?})
"#,
            log_path = log_path.display().to_string(),
            response = "{\"hookSpecificOutput\":{\"hookEventName\":\"PermissionRequest\",\"decision\":{\"behavior\":\"allow\"}}}",
        ),
    )
    .with_context(|| format!("write hook script to {}", script_path.display()))?;
    let python = if cfg!(windows) { "python" } else { "python3" };
    let script_path_arg = if cfg!(windows) {
        script_path.display().to_string()
    } else {
        format!(
            "'{}'",
            script_path.display().to_string().replace('\'', "'\\''")
        )
    };
    let plugin_root = turn_context.config.codex_home.clone();
    let source_path = plugin_root.join("hooks/permission_request_hook.json");
    let plugin_hook_sources = vec![PluginHookSource {
        plugin_id: PluginId::parse("test-hooks@test-marketplace")?,
        plugin_root: plugin_root.clone(),
        plugin_data_root: plugin_root.clone(),
        source_path,
        source_relative_path: "hooks/permission_request_hook.json".to_string(),
        hooks: HookEventsToml {
            permission_request: vec![MatcherGroup {
                matcher: Some("*".to_string()),
                hooks: vec![HookHandlerConfig::Command {
                    command: format!("{python} {script_path_arg}"),
                    timeout_sec: Some(5),
                    r#async: false,
                    status_message: None,
                }],
            }],
            ..Default::default()
        },
    }];
    let discovered = codex_hooks::list_hooks(HooksConfig {
        feature_enabled: true,
        plugin_hook_sources: plugin_hook_sources.clone(),
        ..HooksConfig::default()
    });
    let state = discovered
        .hooks
        .into_iter()
        .map(|entry| {
            (
                entry.key,
                serde_json::json!({
                    "trusted_hash": entry.current_hash,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let hook_state_config: toml::Value = serde_json::from_value(serde_json::json!({
        "hooks": {
            "state": state,
        },
    }))?;
    let config_path = turn_context.config.codex_home.join("config.toml");
    let config_layer_stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::User { file: config_path },
            hook_state_config,
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )?;

    session
        .services
        .hooks
        .store(std::sync::Arc::new(Hooks::new(HooksConfig {
            feature_enabled: true,
            config_layer_stack: Some(config_layer_stack),
            plugin_hook_sources,
            ..HooksConfig::default()
        })));

    turn_context.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    turn_context.permission_profile = PermissionProfile::from_runtime_permissions(
        &read_only_file_system_sandbox_policy(),
        NetworkSandboxPolicy::Restricted,
    );
    let workdir = AbsolutePathBuf::try_from(std::env::current_dir()?)?;
    let target = std::env::temp_dir().join("execve-hook-short-circuit.txt");
    let target_str = target.display().to_string();
    let expected_hook_command =
        codex_shell_command::parse_command::shlex_join(&["/usr/bin/touch".to_string(), target_str]);
    let provider = CoreShellActionProvider {
        policy: std::sync::Arc::new(RwLock::new(codex_execpolicy::Policy::empty())),
        session: std::sync::Arc::new(session),
        turn: std::sync::Arc::new(turn_context),
        call_id: "execve-hook-call".to_string(),
        tool_name: GuardianCommandSource::Shell,
        approval_policy: AskForApproval::OnRequest,
        permission_profile: permission_profile_from_sandbox_policy(
            &SandboxPolicy::new_read_only_policy(),
        ),
        file_system_sandbox_policy: read_only_file_system_sandbox_policy(),
        sandbox_policy_cwd: workdir.clone(),
        sandbox_permissions: SandboxPermissions::RequireEscalated,
        approval_sandbox_permissions: SandboxPermissions::RequireEscalated,
        prompt_permissions: None,
        stopwatch: codex_shell_escalation::Stopwatch::new(Duration::from_secs(1)),
    };

    let decision = tokio::time::timeout(
        Duration::from_secs(5),
        crate::hook_runtime::run_permission_request_hooks(
            &provider.session,
            &provider.turn,
            "execve-hook-call",
            crate::tools::sandboxing::PermissionRequestPayload::bash(
                expected_hook_command.clone(),
                /*description*/ None,
            ),
        ),
    )
    .await
    .context("timed out waiting for execve permission hook decision")?;
    assert_eq!(
        decision,
        Some(codex_hooks::PermissionRequestDecision::Allow)
    );

    let hook_inputs: Vec<Value> = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read hook log at {}", log_path.display()))?
        .lines()
        .map(serde_json::from_str)
        .collect::<serde_json::Result<_>>()
        .context("parse hook log")?;
    assert_eq!(hook_inputs.len(), 1);
    assert_eq!(
        hook_inputs[0]["tool_input"]["command"],
        expected_hook_command
    );
    assert_eq!(
        hook_inputs[0]["tool_input"]["description"],
        serde_json::Value::Null
    );

    Ok(())
}

#[test]
fn evaluate_intercepted_exec_policy_uses_wrapper_command_when_shell_wrapper_parsing_disabled() {
    let policy_src = r#"prefix_rule(pattern = ["npm", "publish"], decision = "prompt")"#;
    let mut parser = PolicyParser::new();
    parser.parse("test.rules", policy_src).unwrap();
    let policy = parser.build();
    let program = AbsolutePathBuf::try_from(host_absolute_path(&["bin", "zsh"])).unwrap();
    let sandbox_cwd = test_sandbox_cwd();

    let enable_intercepted_exec_policy_shell_wrapper_parsing = false;
    let evaluation = evaluate_intercepted_exec_policy(
        &policy,
        &program,
        &[
            "zsh".to_string(),
            "-lc".to_string(),
            "npm publish".to_string(),
        ],
        InterceptedExecPolicyContext {
            approval_policy: AskForApproval::OnRequest,
            permission_profile: permission_profile_from_sandbox_policy(
                &SandboxPolicy::new_read_only_policy(),
            ),
            file_system_sandbox_policy: &read_only_file_system_sandbox_policy(),
            sandbox_cwd: sandbox_cwd.as_path(),
            sandbox_permissions: SandboxPermissions::UseDefault,
            enable_shell_wrapper_parsing: enable_intercepted_exec_policy_shell_wrapper_parsing,
        },
    );

    assert!(
        matches!(
            evaluation.matched_rules.as_slice(),
            [RuleMatch::HeuristicsRuleMatch { command, decision: Decision::Allow }]
                if command == &vec![
                    program.to_string_lossy().to_string(),
                    "-lc".to_string(),
                    "npm publish".to_string(),
                ]
        ),
        r#"This is allowed because when shell wrapper parsing is disabled,
the policy evaluation does not try to parse the shell command and instead
matches the whole command line with the resolved program path, which in this
case is `/bin/zsh` followed by some arguments.

Because there is no policy rule for `/bin/zsh` or `zsh`, the decision is to
allow the command and let the sandbox be responsible for enforcing any
restrictions.

That said, if /bin/zsh is the zsh-fork, then the execve wrapper should
ultimately intercept the `npm publish` command and apply the policy rules to it.
"#
    );
}

#[test]
fn evaluate_intercepted_exec_policy_matches_inner_shell_commands_when_enabled() {
    let policy_src = r#"prefix_rule(pattern = ["npm", "publish"], decision = "prompt")"#;
    let mut parser = PolicyParser::new();
    parser.parse("test.rules", policy_src).unwrap();
    let policy = parser.build();
    let program = AbsolutePathBuf::try_from(host_absolute_path(&["bin", "bash"])).unwrap();
    let sandbox_cwd = test_sandbox_cwd();

    let enable_intercepted_exec_policy_shell_wrapper_parsing = true;
    let evaluation = evaluate_intercepted_exec_policy(
        &policy,
        &program,
        &[
            "bash".to_string(),
            "-lc".to_string(),
            "npm publish".to_string(),
        ],
        InterceptedExecPolicyContext {
            approval_policy: AskForApproval::OnRequest,
            permission_profile: permission_profile_from_sandbox_policy(
                &SandboxPolicy::new_read_only_policy(),
            ),
            file_system_sandbox_policy: &read_only_file_system_sandbox_policy(),
            sandbox_cwd: sandbox_cwd.as_path(),
            sandbox_permissions: SandboxPermissions::UseDefault,
            enable_shell_wrapper_parsing: enable_intercepted_exec_policy_shell_wrapper_parsing,
        },
    );

    assert_eq!(
        evaluation,
        Evaluation {
            decision: Decision::Prompt,
            matched_rules: vec![RuleMatch::PrefixRuleMatch {
                matched_prefix: vec!["npm".to_string(), "publish".to_string()],
                decision: Decision::Prompt,
                resolved_program: None,
                justification: None,
            }],
        }
    );
}

#[test]
fn intercepted_exec_policy_uses_host_executable_mappings() {
    let git_path = host_absolute_path(&["usr", "bin", "git"]);
    let git_path_literal = starlark_string(&git_path);
    let policy_src = format!(
        r#"
prefix_rule(pattern = ["git", "status"], decision = "prompt")
host_executable(name = "git", paths = ["{git_path_literal}"])
"#
    );
    let mut parser = PolicyParser::new();
    parser.parse("test.rules", &policy_src).unwrap();
    let policy = parser.build();
    let program = AbsolutePathBuf::try_from(git_path).unwrap();
    let sandbox_cwd = test_sandbox_cwd();

    let evaluation = evaluate_intercepted_exec_policy(
        &policy,
        &program,
        &["git".to_string(), "status".to_string()],
        InterceptedExecPolicyContext {
            approval_policy: AskForApproval::OnRequest,
            permission_profile: permission_profile_from_sandbox_policy(
                &SandboxPolicy::new_read_only_policy(),
            ),
            file_system_sandbox_policy: &read_only_file_system_sandbox_policy(),
            sandbox_cwd: sandbox_cwd.as_path(),
            sandbox_permissions: SandboxPermissions::UseDefault,
            enable_shell_wrapper_parsing: false,
        },
    );

    assert_eq!(
        evaluation,
        Evaluation {
            decision: Decision::Prompt,
            matched_rules: vec![RuleMatch::PrefixRuleMatch {
                matched_prefix: vec!["git".to_string(), "status".to_string()],
                decision: Decision::Prompt,
                resolved_program: Some(program),
                justification: None,
            }],
        }
    );
    assert!(CoreShellActionProvider::decision_driven_by_policy(
        &evaluation.matched_rules,
        evaluation.decision
    ));
}

#[test]
fn intercepted_exec_policy_treats_preapproved_additional_permissions_as_default() {
    let policy = PolicyParser::new().build();
    let program = AbsolutePathBuf::try_from(host_absolute_path(&["usr", "bin", "printf"])).unwrap();
    let argv = ["printf".to_string(), "hello".to_string()];
    let approval_policy = AskForApproval::OnRequest;
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let file_system_sandbox_policy = read_only_file_system_sandbox_policy();
    let sandbox_cwd = test_sandbox_cwd();

    let preapproved = evaluate_intercepted_exec_policy(
        &policy,
        &program,
        &argv,
        InterceptedExecPolicyContext {
            approval_policy,
            permission_profile: permission_profile_from_sandbox_policy(&sandbox_policy),
            file_system_sandbox_policy: &file_system_sandbox_policy,
            sandbox_cwd: sandbox_cwd.as_path(),
            sandbox_permissions: super::approval_sandbox_permissions(
                SandboxPermissions::WithAdditionalPermissions,
                /*additional_permissions_preapproved*/ true,
            ),
            enable_shell_wrapper_parsing: false,
        },
    );
    let fresh_request = evaluate_intercepted_exec_policy(
        &policy,
        &program,
        &argv,
        InterceptedExecPolicyContext {
            approval_policy,
            permission_profile: permission_profile_from_sandbox_policy(&sandbox_policy),
            file_system_sandbox_policy: &file_system_sandbox_policy,
            sandbox_cwd: sandbox_cwd.as_path(),
            sandbox_permissions: SandboxPermissions::WithAdditionalPermissions,
            enable_shell_wrapper_parsing: false,
        },
    );

    assert_eq!(preapproved.decision, Decision::Allow);
    assert_eq!(fresh_request.decision, Decision::Prompt);
}

#[test]
fn intercepted_exec_policy_rejects_disallowed_host_executable_mapping() {
    let allowed_git = host_absolute_path(&["usr", "bin", "git"]);
    let other_git = host_absolute_path(&["opt", "homebrew", "bin", "git"]);
    let allowed_git_literal = starlark_string(&allowed_git);
    let policy_src = format!(
        r#"
prefix_rule(pattern = ["git", "status"], decision = "prompt")
host_executable(name = "git", paths = ["{allowed_git_literal}"])
"#
    );
    let mut parser = PolicyParser::new();
    parser.parse("test.rules", &policy_src).unwrap();
    let policy = parser.build();
    let program = AbsolutePathBuf::try_from(other_git.clone()).unwrap();
    let sandbox_cwd = test_sandbox_cwd();

    let evaluation = evaluate_intercepted_exec_policy(
        &policy,
        &program,
        &["git".to_string(), "status".to_string()],
        InterceptedExecPolicyContext {
            approval_policy: AskForApproval::OnRequest,
            permission_profile: permission_profile_from_sandbox_policy(
                &SandboxPolicy::new_read_only_policy(),
            ),
            file_system_sandbox_policy: &read_only_file_system_sandbox_policy(),
            sandbox_cwd: sandbox_cwd.as_path(),
            sandbox_permissions: SandboxPermissions::UseDefault,
            enable_shell_wrapper_parsing: false,
        },
    );

    assert!(matches!(
        evaluation.matched_rules.as_slice(),
        [RuleMatch::HeuristicsRuleMatch { command, .. }]
            if command == &vec![other_git, "status".to_string()]
    ));
    assert!(!CoreShellActionProvider::decision_driven_by_policy(
        &evaluation.matched_rules,
        evaluation.decision
    ));
}
