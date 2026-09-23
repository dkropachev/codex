#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use anyhow::Result;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::ExecutorFileSystem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_shell_command_call;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::local_selections;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::Arc;

async fn write_repo_skill(
    cwd: AbsolutePathBuf,
    fs: Arc<dyn ExecutorFileSystem>,
    name: &str,
    description: &str,
    body: &str,
) -> Result<()> {
    let skill_dir = cwd.join(".agents").join("skills").join(name);
    let skill_dir_uri = PathUri::from_host_native_path(&skill_dir)?;
    fs.create_directory(
        &skill_dir_uri,
        CreateDirectoryOptions { recursive: true },
        /*sandbox*/ None,
    )
    .await?;
    let contents = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    let path = skill_dir.join("SKILL.md");
    let path_uri = PathUri::from_host_native_path(&path)?;
    fs.write_file(&path_uri, contents.into_bytes(), /*sandbox*/ None)
        .await?;
    Ok(())
}

async fn submit_skill_turn(
    test: &TestCodex,
    prompt: &str,
    name: &str,
    skill_path: PathBuf,
) -> Result<()> {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    test.codex
        .submit(Op::UserInput {
            items: vec![
                UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Skill {
                    name: name.to_string(),
                    path: skill_path,
                },
            ],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                environments: Some(local_selections(test.config.cwd.clone())),
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                    mode: codex_protocol::config_types::ModeKind::Default,
                    settings: codex_protocol::config_types::Settings {
                        model: test.session_configured.model.clone(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            },
        })
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_includes_skill_instructions() -> Result<()> {
    // TODO(anp): Remove after skill-path helpers use target-native paths.
    skip_if_wine_exec!(Ok(()), "requires native cross-OS skill paths");
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let skill_body = "skill body";
    let mut builder = test_codex().with_workspace_setup(move |cwd, fs| async move {
        write_repo_skill(cwd, fs, "demo", "demo skill", skill_body).await
    });
    let test = builder.build_with_auto_env(&server).await?;

    let skill_path = test
        .config
        .cwd
        .join(".agents/skills/demo/SKILL.md")
        .canonicalize()
        .unwrap_or_else(|_| test.config.cwd.join(".agents/skills/demo/SKILL.md"))
        .to_path_buf();

    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    submit_skill_turn(&test, "please use $demo", "demo", skill_path.clone()).await?;

    core_test_support::wait_for_event(test.codex.as_ref(), |event| {
        matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
    })
    .await;

    let request = mock.single_request();
    let user_texts = request.message_input_texts("user");
    let skill_path_str = skill_path.to_string_lossy();
    assert!(
        user_texts.iter().any(|text| {
            text.contains("<skill>\n<name>demo</name>")
                && text.contains("<path>")
                && text.contains(skill_body)
                && text.contains(skill_path_str.as_ref())
        }),
        "expected skill instructions in user input, got {user_texts:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_babysitting_skill_drives_monitoring_lifecycle() -> Result<()> {
    skip_if_wine_exec!(Ok(()), "requires native cross-OS skill paths");
    skip_if_no_network!(Ok(()));

    let skill_source_path =
        codex_utils_cargo_bin::find_resource!("../../.codex/skills/babysit-pr/SKILL.md")?;
    let skill_source = std::fs::read_to_string(skill_source_path)?;
    let (_, skill_body) = skill_source
        .split_once("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("babysit-pr skill frontmatter terminator missing"))?;
    let skill_body = skill_body.trim_start().to_string();
    let workspace_skill_body = skill_body.clone();

    let server = start_mock_server().await;
    let mut builder = test_codex().with_workspace_setup(move |cwd, fs| {
        let workspace_skill_body = workspace_skill_body;
        async move {
            write_repo_skill(
                cwd,
                fs,
                "babysit-pr",
                "Watch PR review comments, CI, and merge conflicts",
                &workspace_skill_body,
            )
            .await
        }
    });
    let test = builder.build_with_auto_env(&server).await?;
    let skill_path = test
        .config
        .cwd
        .join(".agents/skills/babysit-pr/SKILL.md")
        .canonicalize()
        .unwrap_or_else(|_| test.config.cwd.join(".agents/skills/babysit-pr/SKILL.md"))
        .to_path_buf();

    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_shell_command_call(
                    "wait-generation",
                    r#"printf '%s' '{"reason":"generation_changed"}'"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_shell_command_call(
                    "watch-green",
                    r#"printf '%s' '{"actions":["ready_to_merge"],"state":"OPEN"}'"#,
                ),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_shell_command_call("wait-closed", r#"printf '%s' '{"reason":"pr_closed"}'"#),
                ev_completed("resp-3"),
            ]),
            sse(vec![
                ev_response_created("resp-4"),
                ev_assistant_message("msg-4", "merged"),
                ev_completed("resp-4"),
            ]),
        ],
    )
    .await;

    submit_skill_turn(
        &test,
        "use $babysit-pr until the pull request reaches a terminal state",
        "babysit-pr",
        skill_path,
    )
    .await?;
    core_test_support::wait_for_event(test.codex.as_ref(), |event| {
        matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
    })
    .await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 4);
    let first_user_texts = requests[0].message_input_texts("user");
    assert!(first_user_texts.iter().any(|text| {
        text.contains("<skill>\n<name>babysit-pr</name>") && text.contains(&skill_body)
    }));
    assert!(
        requests[1]
            .function_call_output_text("wait-generation")
            .is_some_and(|output| output.contains("generation_changed"))
    );
    assert!(
        requests[2]
            .function_call_output_text("watch-green")
            .is_some_and(|output| { output.contains("ready_to_merge") && output.contains("OPEN") })
    );
    assert!(
        requests[3]
            .function_call_output_text("wait-closed")
            .is_some_and(|output| output.contains("pr_closed"))
    );

    Ok(())
}
