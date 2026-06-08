CREATE TABLE model_router_tune_runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    schema_version INTEGER NOT NULL,
    generated_at_ms INTEGER NOT NULL,
    window_start_ms INTEGER,
    window_end_ms INTEGER NOT NULL,
    config_fingerprint TEXT NOT NULL,
    evaluated_count INTEGER NOT NULL DEFAULT 0,
    skipped_count INTEGER NOT NULL DEFAULT 0,
    cost_budget_usd_micros INTEGER NOT NULL DEFAULT 0,
    token_budget INTEGER NOT NULL DEFAULT 0,
    cost_used_usd_micros INTEGER NOT NULL DEFAULT 0,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    report_json TEXT
);

CREATE INDEX model_router_tune_runs_generated_at_idx
    ON model_router_tune_runs(generated_at_ms DESC);

CREATE TABLE model_router_tune_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    candidate_identity TEXT NOT NULL,
    task_key TEXT NOT NULL,
    status TEXT NOT NULL,
    score REAL,
    confidence REAL NOT NULL DEFAULT 0.0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd_micros INTEGER NOT NULL DEFAULT 0,
    output_json TEXT,
    FOREIGN KEY(run_id) REFERENCES model_router_tune_runs(run_id) ON DELETE CASCADE
);

CREATE INDEX model_router_tune_results_run_idx
    ON model_router_tune_results(run_id, candidate_identity);

CREATE TABLE model_router_metric_overlays (
    candidate_identity TEXT PRIMARY KEY NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    intelligence_score REAL,
    success_rate REAL,
    median_latency_ms INTEGER,
    estimated_cost_usd_micros INTEGER,
    source_report_id TEXT NOT NULL,
    config_fingerprint TEXT NOT NULL
);
