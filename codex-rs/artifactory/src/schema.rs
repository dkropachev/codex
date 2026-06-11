use rusqlite::Connection;

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS artifact_states (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            namespace TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            source_key TEXT NOT NULL,
            state_dir TEXT NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at_unix_sec INTEGER NOT NULL,
            updated_at_unix_sec INTEGER NOT NULL,
            last_hit_at_unix_sec INTEGER,
            UNIQUE(namespace, scope_key, source_key),
            UNIQUE(namespace, state_dir)
        );
        CREATE INDEX IF NOT EXISTS artifact_states_scope_idx
            ON artifact_states(namespace, scope_key);
        CREATE INDEX IF NOT EXISTS artifact_states_last_hit_idx
            ON artifact_states(namespace, last_hit_at_unix_sec);
        CREATE TABLE IF NOT EXISTS artifact_sources (
            state_id INTEGER NOT NULL REFERENCES artifact_states(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            kind TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            PRIMARY KEY(state_id, path, kind)
        );
        CREATE TABLE IF NOT EXISTS artifact_files (
            state_id INTEGER NOT NULL REFERENCES artifact_states(id) ON DELETE CASCADE,
            relative_path TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            sha256 TEXT NOT NULL,
            updated_at_unix_sec INTEGER NOT NULL,
            PRIMARY KEY(state_id, relative_path)
        );
        CREATE INDEX IF NOT EXISTS artifact_files_relative_path_idx
            ON artifact_files(relative_path);
        CREATE TABLE IF NOT EXISTS cache_entries (
            namespace TEXT NOT NULL,
            key TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            status TEXT NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at_unix_sec INTEGER NOT NULL,
            updated_at_unix_sec INTEGER NOT NULL,
            last_hit_at_unix_sec INTEGER,
            PRIMARY KEY(namespace, key)
        );
        CREATE INDEX IF NOT EXISTS cache_entries_artifact_idx
            ON cache_entries(namespace, artifact_id);
        CREATE TABLE IF NOT EXISTS maintenance_state (
            namespace TEXT PRIMARY KEY,
            last_prune_at_unix_sec INTEGER NOT NULL
        );",
    )?;
    Ok(())
}
