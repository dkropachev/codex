CREATE TABLE model_router_lifecycle_promotions (
    task_key TEXT NOT NULL,
    candidate_identity TEXT NOT NULL,
    base_candidate_identity TEXT NOT NULL,
    status TEXT NOT NULL,
    rule_id TEXT,
    production_model_provider TEXT,
    production_model TEXT,
    base_model_provider TEXT,
    base_model TEXT,
    promoted_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    reason TEXT,
    PRIMARY KEY(task_key, candidate_identity)
);

CREATE INDEX model_router_lifecycle_promotions_status_idx
    ON model_router_lifecycle_promotions(status, updated_at_ms DESC);

CREATE TABLE model_router_shadow_evaluations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    task_key TEXT NOT NULL,
    phase TEXT NOT NULL,
    candidate_identity TEXT NOT NULL,
    base_candidate_identity TEXT NOT NULL,
    success INTEGER NOT NULL DEFAULT 0,
    score REAL,
    confidence REAL NOT NULL DEFAULT 0.0,
    cost_usd_micros INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    outcome TEXT,
    metadata_json TEXT
);

CREATE INDEX model_router_shadow_evaluations_task_phase_idx
    ON model_router_shadow_evaluations(task_key, phase, created_at_ms DESC);

CREATE INDEX model_router_shadow_evaluations_candidate_idx
    ON model_router_shadow_evaluations(candidate_identity, created_at_ms DESC);
