CREATE TABLE tool_router_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    route_kind TEXT NOT NULL,
    selected_tools_json TEXT NOT NULL,
    visible_router_schema_tokens INTEGER NOT NULL DEFAULT 0,
    hidden_tool_schema_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_schema_tokens_saved INTEGER NOT NULL DEFAULT 0,
    spark_prompt_tokens INTEGER NOT NULL DEFAULT 0,
    spark_completion_tokens INTEGER NOT NULL DEFAULT 0,
    net_tokens_saved INTEGER NOT NULL DEFAULT 0,
    fanout_call_count INTEGER NOT NULL DEFAULT 0,
    returned_output_tokens INTEGER NOT NULL DEFAULT 0,
    original_output_tokens INTEGER NOT NULL DEFAULT 0,
    truncated_output_tokens INTEGER NOT NULL DEFAULT 0,
    outcome TEXT
);

CREATE INDEX tool_router_ledger_thread_turn_idx
    ON tool_router_ledger(thread_id, turn_id, created_at_ms);

CREATE INDEX tool_router_ledger_route_kind_idx
    ON tool_router_ledger(route_kind, created_at_ms);

CREATE TABLE tool_router_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    last_hit_at_ms INTEGER,
    match_key TEXT NOT NULL UNIQUE,
    route_json TEXT NOT NULL,
    source TEXT NOT NULL,
    hit_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX tool_router_rules_source_idx
    ON tool_router_rules(source, updated_at_ms);
