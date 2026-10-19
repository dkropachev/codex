//! Client-marked function outputs survive compaction and a cold resume in their original role.

use super::*;
use codex_protocol::turn_input::AnnotatedResponseItem;
use codex_protocol::turn_input::ResponseItemAnnotations;
use codex_protocol::turn_input::TurnInput;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_tool_output_survives_compaction_and_resume() -> Result<()> {
    skip_if_no_network!(Ok(()));
    for enabled in [false, true] {
        let server = start_mock_server().await;
        // Each detail fits the normal item cap. Together they exceed the shared
        // 64K-token retention budget, so compaction must evict whole older outputs.
        let details = (0..8).map(|index| {
            format!(
                "Task detail {index}: {}",
                "Investigate the failing tests carefully. ".repeat(/*n*/ 875)
            )
        });
        let latest_instruction = "Report your findings without changing files.";
        let mut marked_outputs = vec![(
            "send_message_to_thread",
            "Investigate the failing tests; report your findings without changing files."
                .to_owned(),
        )];
        marked_outputs.extend(details.map(|detail| ("task_details", detail)));
        marked_outputs.push(("send_message_to_thread", latest_instruction.to_owned()));
        let input_count = marked_outputs.len();
        let mut events = vec![sse(vec![
            ev_assistant_message("accepted", "I will investigate the tests."),
            ev_completed("accepted-response"),
        ])];
        events.extend((0..input_count - 2).map(|index| {
            sse(vec![
                ev_assistant_message(
                    &format!("details-accepted-{index}"),
                    "I will use those task details.",
                ),
                ev_completed(&format!("details-response-{index}")),
            ])
        }));
        events.extend([
            sse(vec![ev_completed("instruction-response")]),
            sse(vec![
                json!({"type":"response.output_item.done", "item":{"type":"compaction", "encrypted_content":"RETAINED_TOOL_OUTPUT_CHECKPOINT"}}),
                ev_completed("compact-response"),
            ]),
            sse(vec![
                ev_assistant_message("resumed", "The request was to investigate the tests."),
                ev_completed("resumed-response"),
            ]),
        ]);
        let mock = mount_sse_sequence(&server, events).await;
        let configure = move |config: &mut Config| {
            configure_scenario_catalog(config);
            config
                .features
                .set_enabled(Feature::RetainClientToolOutputs, enabled)
                .unwrap();
            config.model_context_window = Some(1_000);
            config.model_auto_compact_token_limit = Some(i64::MAX);
        };
        let test = test_codex()
            .with_model("gpt-6-astra")
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(configure)
            .build_with_auto_env(&server)
            .await?;
        for (name, output) in &marked_outputs {
            let request = TurnInputRequest::new(TurnInput::AnnotatedResponseItem(
                AnnotatedResponseItem {
                    item: serde_json::from_value(json!({
                        "type": "function_call_output", "name": name, "namespace": "codex_app", "output": output,
                    }))?,
                    annotations: ResponseItemAnnotations { retain: true },
                },
            ));
            test.codex.start_or_steer_turn(request).await?;
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnComplete(_))
            })
            .await;
        }
        test.codex
            .inject_response_items(vec![serde_json::from_value(json!({
                "type": "function_call_output", "name": "progress", "namespace": "example",
                "output": "Transient progress update. ".repeat(1000),
            }))?])
            .await?;
        test.codex.submit(Op::Compact).await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        let resumed = test_codex()
            .with_model("gpt-6-astra")
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(move |config| {
                configure(config);
                config.model_context_window = Some(200_000);
            })
            .restart_with_auto_env(&server, &test)
            .await?;
        resumed
            .submit_text_turn("What request are you working on?")
            .await?;
        let requests = mock.requests();
        assert_eq!(requests.len(), input_count + 2);
        let mut admitted_tokens = 0;
        for (index, (request, (_, output))) in requests[..input_count]
            .iter()
            .zip(&marked_outputs)
            .enumerate()
        {
            assert!(request.inputs_of_type("compaction_trigger").is_empty());
            let outputs = request.inputs_of_type("function_call_output");
            let admitted = outputs.last().expect("newly admitted marked output");
            let text = admitted["output"].as_str().expect("marked output text");
            // Compare large payloads without constructing a quadratic character diff.
            assert!(
                text == output.as_str(),
                "marked output {index} changed at admission"
            );
            admitted_tokens += approx_token_count(text);
        }
        assert!(admitted_tokens > 64_000);
        for request in &requests {
            for item in request.input() {
                let bytes = serde_json::to_vec(&item)?.len();
                assert!(bytes <= 40_000, "{} item has {bytes} bytes", item["type"]);
            }
        }
        let compact_outputs = requests[input_count].inputs_of_type("function_call_output");
        let mut expected_names = marked_outputs
            .iter()
            .map(|(name, _)| Some(*name))
            .collect::<Vec<_>>();
        expected_names.push(Some("progress"));
        assert_eq!(
            compact_outputs
                .iter()
                .map(|output| output["name"].as_str())
                .collect::<Vec<_>>(),
            expected_names
        );
        // Emergency trimming removes the trailing log, then stops at the marked instruction.
        for (index, (_, output)) in marked_outputs[..input_count - 1].iter().enumerate() {
            assert!(
                compact_outputs[index]["output"].as_str() == Some(output.as_str()),
                "older marked output {index} changed before compaction"
            );
        }
        if enabled {
            assert_eq!(
                compact_outputs[input_count - 1]["output"],
                latest_instruction
            );
        } else {
            assert_eq!(
                compact_outputs[input_count - 1]["output"],
                "Output exceeded the available model context and was truncated"
            );
        }
        assert_eq!(
            compact_outputs[input_count]["output"],
            "Output exceeded the available model context and was truncated"
        );
        let last = requests.last().unwrap();
        assert!(last.inputs_of_type("compaction_trigger").is_empty());
        assert!(last.body_contains_text("What request are you working on?"));
        let outputs = last.inputs_of_type("function_call_output");
        if enabled {
            // Seven newest details fit; the oldest detail and instruction are
            // evicted whole. The latest instruction stays unchanged on cold resume.
            let expected = &compact_outputs[2..input_count];
            assert_eq!(outputs.len(), expected.len());
            for (index, (output, expected)) in outputs.iter().zip(expected).enumerate() {
                assert!(
                    output == expected,
                    "retained output {index} changed on resume"
                );
                assert!(output.get("retain").is_none());
                assert!(output.get("client_authored").is_none());
            }
            let retained_tokens = outputs
                .iter()
                .map(|output| approx_token_count(output["output"].as_str().unwrap()))
                .sum::<usize>();
            assert!(retained_tokens <= 64_000);
            assert!(
                retained_tokens
                    + approx_token_count(compact_outputs[1]["output"].as_str().unwrap())
                    > 64_000
            );
        } else {
            assert!(outputs.is_empty());
        }
        insta::assert_snapshot!(
            if enabled {
                "retained_tool_output_survives_compaction_and_resume"
            } else {
                "retained_tool_output_retention_disabled"
            },
            context_snapshot::format_request_history_snapshot(
                if enabled {
                    "Individually bounded retained outputs exceed the shared 64K-token budget. Emergency trimming preserves them, compaction evicts the oldest whole outputs, and cold resume keeps the seven newest details and latest instruction unchanged."
                } else {
                    "With retention disabled, the client can still send retain=true. Tool outputs use ordinary emergency trimming and are not retained after compaction and cold resume."
                },
                &requests,
                &ContextSnapshotOptions::default().rewrite_known_segments(),
            )
        );
        resumed.codex.shutdown_and_wait().await?;
    }
    Ok(())
}
