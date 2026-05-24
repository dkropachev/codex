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
