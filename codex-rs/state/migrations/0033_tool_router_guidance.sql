ALTER TABLE tool_router_ledger ADD COLUMN model_slug TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_router_ledger ADD COLUMN model_provider TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_router_ledger ADD COLUMN toolset_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_router_ledger ADD COLUMN router_schema_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tool_router_ledger ADD COLUMN guidance_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tool_router_ledger ADD COLUMN guidance_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tool_router_ledger ADD COLUMN format_description_tokens INTEGER NOT NULL DEFAULT 0;

CREATE INDEX tool_router_ledger_model_toolset_idx
    ON tool_router_ledger(model_slug, model_provider, toolset_hash, router_schema_version, created_at_ms);

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
