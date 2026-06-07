CREATE TABLE tool_router_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    model_slug TEXT NOT NULL DEFAULT '',
    model_provider TEXT NOT NULL DEFAULT '',
    toolset_hash TEXT NOT NULL DEFAULT '',
    router_schema_version INTEGER NOT NULL DEFAULT 0,
    model_response_ordinal INTEGER NOT NULL DEFAULT 0,
    guidance_version INTEGER NOT NULL DEFAULT 0,
    guidance_tokens INTEGER NOT NULL DEFAULT 0,
    format_description_tokens INTEGER NOT NULL DEFAULT 0,
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
    outcome TEXT,
    request_shape_json TEXT,
    tool_call_source TEXT,
    tool_name TEXT,
    tool_namespace TEXT,
    tool_input_json TEXT,
    tool_output_json TEXT,
    tool_success INTEGER,
    prompt_json TEXT,
    previous_prompt_json TEXT,
    dialog_locator_json TEXT
);

CREATE INDEX tool_router_ledger_thread_turn_idx
    ON tool_router_ledger(thread_id, turn_id, created_at_ms);

CREATE INDEX tool_router_ledger_route_kind_idx
    ON tool_router_ledger(route_kind, created_at_ms);

CREATE INDEX tool_router_ledger_model_toolset_idx
    ON tool_router_ledger(model_slug, model_provider, toolset_hash, router_schema_version, created_at_ms);

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

CREATE TABLE tool_router_guidance (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    model_slug TEXT NOT NULL,
    model_provider TEXT NOT NULL,
    toolset_hash TEXT NOT NULL,
    router_schema_version INTEGER NOT NULL,
    guidance_version INTEGER NOT NULL,
    guidance_text TEXT NOT NULL,
    guidance_tokens INTEGER NOT NULL,
    source TEXT NOT NULL,
    UNIQUE(model_slug, model_provider, toolset_hash, router_schema_version)
);

CREATE TABLE tool_router_remembered_tools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    repo_key TEXT NOT NULL,
    task_key TEXT NOT NULL,
    tool_namespace TEXT NOT NULL DEFAULT '',
    tool_name TEXT NOT NULL,
    request_count INTEGER NOT NULL DEFAULT 1,
    UNIQUE(repo_key, task_key, tool_namespace, tool_name)
);

CREATE INDEX tool_router_remembered_tools_repo_task_updated_idx
    ON tool_router_remembered_tools(repo_key, task_key, updated_at_ms DESC, request_count DESC, tool_namespace, tool_name);
