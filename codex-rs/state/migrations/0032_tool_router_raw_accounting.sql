CREATE TABLE tool_router_ledger_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    route_kind TEXT NOT NULL,
    selected_tools_json TEXT NOT NULL,
    visible_router_schema_tokens INTEGER NOT NULL DEFAULT 0,
    hidden_tool_schema_tokens INTEGER NOT NULL DEFAULT 0,
    spark_prompt_tokens INTEGER NOT NULL DEFAULT 0,
    spark_completion_tokens INTEGER NOT NULL DEFAULT 0,
    fanout_call_count INTEGER NOT NULL DEFAULT 0,
    returned_output_tokens INTEGER NOT NULL DEFAULT 0,
    original_output_tokens INTEGER NOT NULL DEFAULT 0,
    truncated_output_tokens INTEGER NOT NULL DEFAULT 0,
    outcome TEXT
);

INSERT INTO tool_router_ledger_new (
    id,
    created_at_ms,
    thread_id,
    turn_id,
    call_id,
    route_kind,
    selected_tools_json,
    visible_router_schema_tokens,
    hidden_tool_schema_tokens,
    spark_prompt_tokens,
    spark_completion_tokens,
    fanout_call_count,
    returned_output_tokens,
    original_output_tokens,
    truncated_output_tokens,
    outcome
)
SELECT
    id,
    created_at_ms,
    thread_id,
    turn_id,
    call_id,
    route_kind,
    selected_tools_json,
    visible_router_schema_tokens,
    hidden_tool_schema_tokens,
    spark_prompt_tokens,
    spark_completion_tokens,
    fanout_call_count,
    returned_output_tokens,
    original_output_tokens,
    truncated_output_tokens,
    outcome
FROM tool_router_ledger;

DROP TABLE tool_router_ledger;
ALTER TABLE tool_router_ledger_new RENAME TO tool_router_ledger;

CREATE INDEX tool_router_ledger_thread_turn_idx
    ON tool_router_ledger(thread_id, turn_id, created_at_ms);

CREATE INDEX tool_router_ledger_route_kind_idx
    ON tool_router_ledger(route_kind, created_at_ms);
