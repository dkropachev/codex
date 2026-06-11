CREATE TABLE model_router_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_ms INTEGER NOT NULL,
    task_key TEXT NOT NULL,
    request_kind TEXT NOT NULL,
    model_provider TEXT,
    model TEXT,
    account_id TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    actual_cost_usd_micros INTEGER NOT NULL DEFAULT 0,
    counterfactual_cost_usd_micros INTEGER NOT NULL DEFAULT 0,
    price_confidence REAL NOT NULL DEFAULT 0.0,
    outcome TEXT
);

CREATE INDEX model_router_ledger_task_key_idx
    ON model_router_ledger(task_key, created_at_ms);

CREATE INDEX model_router_ledger_request_kind_idx
    ON model_router_ledger(request_kind, created_at_ms);

CREATE TABLE model_router_task_incumbents (
    task_key TEXT PRIMARY KEY NOT NULL,
    candidate_id TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
