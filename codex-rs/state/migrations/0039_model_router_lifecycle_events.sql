CREATE TABLE model_router_lifecycle_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    source TEXT NOT NULL,
    task_key TEXT NOT NULL,
    candidate_identity TEXT NOT NULL,
    base_candidate_identity TEXT NOT NULL,
    previous_status TEXT,
    next_status TEXT,
    rule_id TEXT,
    reason TEXT,
    production_model_provider TEXT,
    production_model TEXT,
    base_model_provider TEXT,
    base_model TEXT,
    lifecycle_window TEXT,
    shadow_phase TEXT,
    shadow_evaluated_count INTEGER,
    shadow_success_count INTEGER,
    shadow_success_rate REAL,
    shadow_average_score REAL,
    shadow_average_confidence REAL,
    shadow_cost_used_usd_micros INTEGER,
    shadow_tokens_used INTEGER,
    shadow_latest_evaluation_id INTEGER,
    shadow_latest_evaluation_at_ms INTEGER,
    failed_gates_json TEXT
);

CREATE INDEX model_router_lifecycle_events_task_idx
    ON model_router_lifecycle_events(task_key, created_at_ms DESC, id DESC);

CREATE INDEX model_router_lifecycle_events_candidate_idx
    ON model_router_lifecycle_events(candidate_identity, created_at_ms DESC, id DESC);

CREATE INDEX model_router_lifecycle_events_type_idx
    ON model_router_lifecycle_events(event_type, created_at_ms DESC, id DESC);

CREATE UNIQUE INDEX model_router_lifecycle_events_blocked_hwm_idx
    ON model_router_lifecycle_events(
        task_key,
        candidate_identity,
        base_candidate_identity,
        shadow_phase,
        shadow_latest_evaluation_id
    )
    WHERE event_type = 'promotion_blocked'
      AND shadow_latest_evaluation_id IS NOT NULL;
