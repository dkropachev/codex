CREATE TABLE tool_router_output_optimizations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    model_slug TEXT NOT NULL,
    model_provider TEXT NOT NULL,
    toolset_hash TEXT NOT NULL,
    router_schema_version INTEGER NOT NULL,
    tool_namespace TEXT NOT NULL DEFAULT '',
    tool_name TEXT NOT NULL,
    suggestion_key TEXT NOT NULL,
    suggestion_label TEXT NOT NULL,
    status TEXT NOT NULL,
    observation_count INTEGER NOT NULL DEFAULT 0,
    recovery_count INTEGER NOT NULL DEFAULT 0,
    original_output_tokens INTEGER NOT NULL DEFAULT 0,
    returned_output_tokens INTEGER NOT NULL DEFAULT 0,
    candidate_output_tokens INTEGER NOT NULL DEFAULT 0,
    saved_output_tokens INTEGER NOT NULL DEFAULT 0,
    last_decision_reason TEXT,
    last_observed_at_ms INTEGER,
    decided_at_ms INTEGER,
    UNIQUE(
        model_slug,
        model_provider,
        toolset_hash,
        router_schema_version,
        tool_namespace,
        tool_name,
        suggestion_key
    )
);

CREATE INDEX tool_router_output_optimizations_status_idx
    ON tool_router_output_optimizations(status, updated_at_ms);

CREATE TABLE tool_router_output_optimization_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    optimization_id INTEGER NOT NULL,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    tool_input_json TEXT,
    original_output_tokens INTEGER NOT NULL,
    returned_output_tokens INTEGER NOT NULL,
    candidate_output_tokens INTEGER NOT NULL,
    saved_output_tokens INTEGER NOT NULL,
    recovery_detected INTEGER NOT NULL DEFAULT 0,
    recovery_reason TEXT,
    UNIQUE(optimization_id, thread_id, turn_id, call_id)
);

CREATE INDEX tool_router_output_optimization_observations_thread_idx
    ON tool_router_output_optimization_observations(thread_id, created_at_ms DESC);
