use super::*;
use crate::runtime::test_support::unique_temp_dir;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn accepts_high_savings_rg_summary_after_observations() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");
    let output = rg_output(/*lines*/ 80);

    for idx in 0..3 {
        let entry = ledger_entry(
            format!("call-{idx}").as_str(),
            r#"{"cmd":"rg -n foo src"}"#,
            output.as_str(),
        );
        runtime
            .record_tool_router_output_optimization_observation(entry)
            .await
            .expect("record observation");
    }

    let records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Accepted))
        .await
        .expect("list optimizations");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].suggestion_key, "exec.rg-summary-v1");
    assert_eq!(
        records[0].status,
        ToolRouterOutputOptimizationStatus::Accepted
    );
    assert_eq!(records[0].observation_count, 3);
    assert!(records[0].saved_output_tokens > 0);

    let accepted_keys = runtime
        .list_accepted_tool_router_output_optimization_keys_for_tool(
            "gpt-test",
            "openai",
            "",
            "exec_command",
        )
        .await
        .expect("list accepted keys");
    assert_eq!(accepted_keys, vec!["exec.rg-summary-v1".to_string()]);
}

#[tokio::test]
async fn accepts_high_savings_nextest_filter_after_observations() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");
    let output = nextest_output(/*pass_lines*/ 900);

    for idx in 0..3 {
        runtime
            .record_tool_router_output_optimization_observation(ledger_entry(
                format!("call-nextest-{idx}").as_str(),
                r#"{"cmd":"just test"}"#,
                output.as_str(),
            ))
            .await
            .expect("record observation");
    }

    let records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Accepted))
        .await
        .expect("list optimizations");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].suggestion_key, "exec.test-output-filter-v1");
    assert_eq!(
        records[0].status,
        ToolRouterOutputOptimizationStatus::Accepted
    );
    assert_eq!(records[0].observation_count, 3);
    assert!(records[0].saved_output_tokens > 0);
}

#[tokio::test]
async fn ignores_builtin_compacted_outputs_for_output_optimization() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");
    let mut entry = ledger_entry(
        "call-builtin",
        r#"{"cmd":"just test"}"#,
        nextest_output(/*pass_lines*/ 900).as_str(),
    );
    entry.output_compaction_filter = Some("nextest-v1".to_string());
    entry.returned_output_tokens = 80;

    runtime
        .record_tool_router_output_optimization_observation(entry)
        .await
        .expect("record builtin compacted output");

    let records = runtime
        .list_tool_router_output_optimizations(/*status*/ None)
        .await
        .expect("list optimizations");
    assert_eq!(records, Vec::new());
}

#[tokio::test]
async fn raw_recovery_for_builtin_compacted_chunk_does_not_decline_learned_candidate() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");
    let output = rg_output(/*lines*/ 80);
    runtime
        .record_tool_router_output_optimization_observation(ledger_entry(
            "call-candidate",
            r#"{"cmd":"rg -n foo src"}"#,
            output.as_str(),
        ))
        .await
        .expect("record candidate");

    let mut builtin_entry = ledger_entry(
        "call-builtin",
        r#"{"cmd":"just test"}"#,
        "Chunk ID: chunk-builtin\nCompaction: nextest-v1\nOutput:\nsummary\n",
    );
    builtin_entry.output_compaction_filter = Some("nextest-v1".to_string());
    runtime
        .record_tool_router_ledger_entry(builtin_entry)
        .await
        .expect("record builtin ledger entry");

    runtime
        .record_tool_router_output_optimization_observation(ToolRouterLedgerEntry {
            tool_name: Some("read_exec_output".to_string()),
            tool_input_json: Some(r#"{"chunk_id":"chunk-builtin"}"#.to_string()),
            tool_output_json: Some(serde_json::json!({ "output": "raw output" }).to_string()),
            returned_output_tokens: 3,
            original_output_tokens: 3,
            call_id: "call-recovery".to_string(),
            ..ledger_entry("call-base", r#"{"cmd":"pwd"}"#, "raw output")
        })
        .await
        .expect("record builtin recovery");

    let declined_records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Declined))
        .await
        .expect("list declined optimizations");
    assert_eq!(declined_records, Vec::new());

    let candidate_records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Candidate))
        .await
        .expect("list candidate optimizations");
    assert_eq!(candidate_records.len(), 1);
    assert_eq!(candidate_records[0].suggestion_key, "exec.rg-summary-v1");
    assert_eq!(candidate_records[0].recovery_count, 0);
}

#[tokio::test]
async fn declines_candidate_after_raw_output_recovery() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");
    let output = rg_output(/*lines*/ 80);
    runtime
        .record_tool_router_output_optimization_observation(ledger_entry(
            "call-candidate",
            r#"{"cmd":"rg -n foo src"}"#,
            output.as_str(),
        ))
        .await
        .expect("record candidate");

    runtime
        .record_tool_router_output_optimization_observation(recovery_entry())
        .await
        .expect("record recovery");

    let records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Declined))
        .await
        .expect("list optimizations");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].suggestion_key, "exec.rg-summary-v1");
    assert_eq!(records[0].recovery_count, 1);

    runtime
        .record_tool_router_output_optimization_observation(ledger_entry(
            "call-after-decline",
            r#"{"cmd":"rg -n foo src"}"#,
            output.as_str(),
        ))
        .await
        .expect("record skipped candidate");
    let records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Declined))
        .await
        .expect("list optimizations after skip");
    assert_eq!(records[0].observation_count, 1);
}

#[tokio::test]
async fn marks_small_output_family_optimized() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");

    for idx in 0..3 {
        runtime
            .record_tool_router_output_optimization_observation(ledger_entry(
                format!("small-{idx}").as_str(),
                r#"{"cmd":"pwd"}"#,
                "Chunk ID: 1\nOutput:\n/tmp/repo\n",
            ))
            .await
            .expect("record small output");
    }

    let records = runtime
        .list_tool_router_output_optimizations(Some(ToolRouterOutputOptimizationStatus::Optimized))
        .await
        .expect("list optimized outputs");

    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].suggestion_key,
        "exec.generic.already-optimized-v1"
    );
    assert_eq!(records[0].observation_count, 3);
}

#[tokio::test]
async fn detects_recent_duplicate_source_read_for_command() {
    let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
        .await
        .expect("state runtime");
    runtime
        .record_tool_router_ledger_entry(ledger_entry(
            "call-source-read",
            r#"{"cmd":"sed -n '10,120p' src/lib.rs"}"#,
            source_read_output(/*start_line*/ 10, /*line_count*/ 111).as_str(),
        ))
        .await
        .expect("record source read");

    assert!(
        runtime
            .tool_router_recent_duplicate_source_read_for_command(
                "thread",
                "call-current",
                "sed -n '10,120p' src/lib.rs",
            )
            .await
            .expect("detect duplicate source read")
    );
    assert!(
        !runtime
            .tool_router_recent_duplicate_source_read_for_command(
                "thread",
                "call-current",
                "sed -n '10,120p' src/other.rs",
            )
            .await
            .expect("detect non-duplicate source read")
    );
}

fn rg_output(lines: usize) -> String {
    (0..lines)
        .map(|idx| format!("src/file{}.rs:{idx}:fn foo_{idx}() {{}}\n", idx % 8))
        .collect()
}

fn nextest_output(pass_lines: usize) -> String {
    let mut output =
        String::from("Nextest run ID abc with nextest profile: default\nStarting tests\n");
    for idx in 0..pass_lines {
        output.push_str(&format!(
            "        PASS [   0.004s] ({idx:5}/{pass_lines}) codex-core suite::passes_{idx}\n"
        ));
    }
    output.push_str(&format!(
        "     Summary [ 10.000s] {pass_lines} tests run: {pass_lines} passed, 0 skipped\n"
    ));
    output
}

fn source_read_output(start_line: usize, line_count: usize) -> String {
    (start_line..start_line + line_count)
        .map(|line| format!("{line}:     let value_{line} = compute_value({line});\n"))
        .collect()
}

fn ledger_entry(call_id: &str, tool_input_json: &str, output: &str) -> ToolRouterLedgerEntry {
    let output_tokens = estimate_text_tokens(output);
    ToolRouterLedgerEntry {
        thread_id: "thread".to_string(),
        turn_id: "turn".to_string(),
        call_id: call_id.to_string(),
        model_slug: "gpt-test".to_string(),
        model_provider: "openai".to_string(),
        toolset_hash: "abc123".to_string(),
        router_schema_version: 1,
        model_response_ordinal: 0,
        guidance_version: 0,
        guidance_tokens: 0,
        format_description_tokens: 0,
        route_kind: "deterministic".to_string(),
        selected_tools: vec!["exec_command".to_string()],
        visible_router_schema_tokens: 10,
        hidden_tool_schema_tokens: 0,
        spark_prompt_tokens: 0,
        spark_completion_tokens: 0,
        fanout_call_count: 1,
        returned_output_tokens: output_tokens,
        original_output_tokens: output_tokens,
        truncated_output_tokens: 0,
        output_compaction_filter: None,
        outcome: Some("ok".to_string()),
        request_shape_json: None,
        tool_call_source: Some("direct".to_string()),
        tool_name: Some("exec_command".to_string()),
        tool_namespace: None,
        tool_input_json: Some(tool_input_json.to_string()),
        tool_output_json: Some(serde_json::json!({ "output": output }).to_string()),
        tool_success: Some(true),
        prompt_json: None,
        previous_prompt_json: None,
        dialog_locator_json: None,
    }
}

fn recovery_entry() -> ToolRouterLedgerEntry {
    ToolRouterLedgerEntry {
        tool_name: Some("read_exec_output".to_string()),
        tool_input_json: Some(r#"{"chunk_id":"chunk-1"}"#.to_string()),
        tool_output_json: Some(serde_json::json!({ "output": "raw output" }).to_string()),
        returned_output_tokens: 3,
        original_output_tokens: 3,
        call_id: "call-recovery".to_string(),
        ..ledger_entry("call-base", r#"{"cmd":"pwd"}"#, "raw output")
    }
}

fn estimate_text_tokens(text: &str) -> i64 {
    i64::try_from(text.len().div_ceil(4)).unwrap_or(i64::MAX)
}
