use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::fs_wait;
use core_test_support::hooks::trust_discovered_hooks;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;
use std::time::Duration;
use test_case::test_case;
use tokio::time::timeout;

#[test_case(true; "checkpoint_first")]
#[test_case(false; "checkpoint_last")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_retains_only_current_sibling_success_failure_and_custom(
    checkpoint_first: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let checkpoint = ev_function_call(
        "checkpoint",
        "handoff_context",
        r#"{"summary":"Continue after completed sibling operations."}"#,
    );
    let mut calls = vec![
        ev_function_call(
            "success",
            "update_plan",
            r#"{"plan":[{"step":"SIBLING_PLAN","status":"completed"}]}"#,
        ),
        ev_function_call("failure", "unknown_operation", "{}"),
        ev_custom_tool_call("custom", "exec", "text('CUSTOM_UNSEEN_RESULT');"),
    ];
    calls.insert(if checkpoint_first { 0 } else { calls.len() }, checkpoint);
    calls.push(ev_completed("siblings"));
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call("previous", "unknown_operation", "{}"),
                ev_completed("previous"),
            ]),
            sse(calls),
            sse(vec![
                ev_assistant_message("done", "Done."),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::HandoffContext).unwrap();
            config.features.enable(Feature::CodeMode).unwrap();
            config.update_plan_enabled = true;
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "OLD_HISTORY only belongs before the checkpoint.".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let mut plan_updates = 0;
    wait_for_event(&test.codex, |event| {
        if matches!(event, EventMsg::PlanUpdate(_)) {
            plan_updates += 1;
        }
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(plan_updates, 1);
    let requests = responses.requests();
    assert_eq!(requests.len(), 3);
    assert!(!requests[2].body_contains_text("OLD_HISTORY"));
    assert!(!requests[2].body_contains_text("previous"));
    assert!(!requests[2].body_contains_text("checkpoint"));
    assert!(requests[2].body_contains_text("CUSTOM_UNSEEN_RESULT"));
    assert!(
        requests[2].function_call_output("success")["output"]
            .as_str()
            .unwrap()
            .contains("Plan updated")
    );
    assert!(
        requests[2].function_call_output("failure")["output"]
            .as_str()
            .unwrap()
            .contains("unknown_operation")
    );
    let input = requests[2].input();
    let pairs: Vec<_> = input
        .iter()
        .filter(|item| item["call_id"].is_string())
        .map(|item| (item["type"].clone(), item["call_id"].clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (json!("function_call"), json!("success")),
            (json!("function_call_output"), json!("success")),
            (json!("function_call"), json!("failure")),
            (json!("function_call_output"), json!("failure")),
            (json!("custom_tool_call"), json!("custom")),
            (json!("custom_tool_call_output"), json!("custom")),
        ]
    );
    let summary = input
        .iter()
        .position(|item| {
            item.to_string()
                .contains("Continue after completed sibling operations.")
        })
        .unwrap();
    let first_call = input
        .iter()
        .position(|item| item["call_id"] == "success")
        .unwrap();
    assert!(summary < first_call);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_overflow_preserves_history_then_allows_sole_retry() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mut calls = vec![ev_function_call(
        "checkpoint",
        "handoff_context",
        r#"{"summary":"REJECTED_SUMMARY"}"#,
    )];
    for index in 0..32 {
        calls.push(ev_function_call(
            &format!("failure-{index}"),
            "unknown_operation",
            "{}",
        ));
    }
    calls.push(ev_completed("overflow"));
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(calls),
            sse(vec![
                ev_function_call(
                    "retry",
                    "handoff_context",
                    r#"{"summary":"ACCEPTED_SUMMARY"}"#,
                ),
                ev_completed("retry"),
            ]),
            sse(vec![
                ev_assistant_message("done", "Done."),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::HandoffContext).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("OLD_HISTORY must survive rejected checkpoint.")
        .await?;
    let requests = responses.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].body_contains_text("OLD_HISTORY"));
    assert!(requests[1].body_contains_text("Checkpoint not installed"));
    assert!(requests[1].body_contains_text("do not rerun operations"));
    for index in 0..32 {
        assert!(
            requests[1].function_call_output(&format!("failure-{index}"))["output"]
                .as_str()
                .unwrap()
                .contains("unknown_operation")
        );
    }
    assert!(requests[2].body_contains_text("ACCEPTED_SUMMARY"));
    assert!(!requests[2].body_contains_text("REJECTED_SUMMARY"));
    assert!(!requests[2].body_contains_text("OLD_HISTORY"));
    Ok(())
}

#[tokio::test]
async fn context_handoff_interrupt_during_pre_compact_hook_preserves_history() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(
        Ok(()),
        "command hooks require a host-native working directory"
    );
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call(
                    "checkpoint",
                    "handoff_context",
                    r#"{"summary":"CANCELLED_CHECKPOINT"}"#,
                ),
                ev_completed("checkpoint"),
            ]),
            sse(vec![
                ev_assistant_message("recovery", "Recovered."),
                ev_completed("recovery"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_pre_build_hook(|home| {
            let script_path = home.join("gated_pre_compact.py");
            let ready_path = serde_json::to_string(&home.join("pre_compact_ready")).unwrap();
            let release_path = serde_json::to_string(&home.join("pre_compact_release")).unwrap();
            let script = format!(r#"import json
import sys
import time
from pathlib import Path
json.load(sys.stdin)
Path({ready_path}).touch()
deadline = time.monotonic() + 10
while not Path({release_path}).exists():
    if time.monotonic() > deadline:
        raise TimeoutError("pre-compaction hook was not released")
    time.sleep(0.001)
print(json.dumps({{"continue": True}}))
"#);
            fs::write(&script_path, script).unwrap();
            fs::write(home.join("hooks.json"), json!({"hooks":{"PreCompact":[{
                "matcher":"auto", "hooks":[{"type":"command", "command":format!("python3 \"{}\"", script_path.display())}]
            }]}}).to_string()).unwrap();
        })
        .with_config(|config| {
            trust_discovered_hooks(config);
            config.features.enable(Feature::HandoffContext).unwrap();
            config.agent_interrupt_message_enabled = false;
        })
        .build_with_auto_env(&server).await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "ORIGINAL_HISTORY must survive interrupted installation.".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let started = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::HookStarted(started) if started.run.event_name == HookEventName::PreCompact => {
            Some(started.clone())
        }
        _ => None,
    })
    .await;
    let turn_id = started.turn_id.context("pre-compaction hook turn ID")?;
    fs_wait::wait_for_path_exists(
        test.codex_home_path().join("pre_compact_ready"),
        Duration::from_secs(5),
    )
    .await?;
    assert!(test.codex.current_turn_model(&turn_id).await.is_some());
    test.codex.submit(Op::Interrupt).await?;
    timeout(Duration::from_secs(5), async {
        while test.codex.current_turn_model(&turn_id).await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("interrupt must cancel the running turn before releasing its hook")?;
    fs::write(
        test.codex_home_path().join("pre_compact_release"),
        "release",
    )?;
    let mut completed_hook = None;
    wait_for_event(&test.codex, |event| {
        if let EventMsg::HookCompleted(completed) = event
            && completed.run.event_name == HookEventName::PreCompact
        {
            completed_hook = Some(completed.run.status);
        }
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert_eq!(completed_hook, Some(HookRunStatus::Completed));
    assert_eq!(responses.requests().len(), 1);
    test.submit_turn("Continue after interruption.").await?;
    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body_contains_text("ORIGINAL_HISTORY"));
    assert!(
        !requests[1]
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains("CANCELLED_CHECKPOINT"))
    );
    let identities: Vec<_> = requests.iter().map(|request| {
        let metadata: serde_json::Value = serde_json::from_str(
            &request.header("x-codex-turn-metadata").expect("turn metadata header"),
        ).expect("valid turn metadata");
        json!({"windowNumber": metadata["window_number"], "windowId": metadata["context_window_id"]})
    }).collect();
    assert!(identities[0]["windowId"].is_string());
    assert_eq!(identities[0], identities[1]);
    let rollout_path = test.codex.rollout_path().context("rollout path")?;
    test.codex.shutdown_and_wait().await?;
    let items = fs::read_to_string(rollout_path)?
        .lines()
        .filter_map(|line| codex_rollout::parse_rollout_line(line).ok())
        .map(|line| line.item)
        .collect::<Vec<_>>();
    assert!(
        !items
            .iter()
            .any(|item| matches!(item, RolloutItem::Compacted(_)))
    );
    Ok(())
}
