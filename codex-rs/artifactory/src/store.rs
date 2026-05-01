use crate::DB_FILENAME;
use crate::keys::ArtifactSource;
use crate::keys::file_sha256;
use crate::keys::normalized_relative_path;
use crate::schema::ensure_schema;
use anyhow::Context;
use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::params;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactState {
    pub id: i64,
    pub namespace: String,
    pub scope_key: String,
    pub source_key: String,
    pub state_dir: PathBuf,
    pub metadata_json: String,
    pub created_at_unix_sec: i64,
    pub updated_at_unix_sec: i64,
    pub last_hit_at_unix_sec: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactFile {
    pub state_id: i64,
    pub relative_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub updated_at_unix_sec: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub namespace: String,
    pub key: String,
    pub artifact_id: String,
    pub status: String,
    pub metadata_json: String,
    pub created_at_unix_sec: i64,
    pub updated_at_unix_sec: i64,
    pub last_hit_at_unix_sec: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRegistration {
    pub namespace: String,
    pub scope_key: String,
    pub source_key: String,
    pub state_dir: PathBuf,
    pub sources: Vec<ArtifactSource>,
    pub metadata_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneOptions {
    pub retention_secs: i64,
    pub throttle_secs: i64,
    pub now_unix_sec: i64,
}

impl PruneOptions {
    pub fn new(retention_secs: i64, throttle_secs: i64) -> Self {
        Self {
            retention_secs,
            throttle_secs,
            now_unix_sec: unix_now(),
        }
    }

    pub fn with_now(retention_secs: i64, throttle_secs: i64, now_unix_sec: i64) -> Self {
        Self {
            retention_secs,
            throttle_secs,
            now_unix_sec,
        }
    }
}

pub struct Artifactory {
    conn: Connection,
}

impl Artifactory {
    pub fn open(codex_home: &Path) -> Result<Self> {
        if !codex_home.exists() {
            fs::create_dir_all(codex_home)
                .with_context(|| format!("failed to create {}", codex_home.display()))?;
        }
        Self::open_at(codex_home.join(DB_FILENAME))
    }

    pub fn open_at(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open artifactory DB {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        ensure_schema(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { conn })
    }

    pub fn db_path(codex_home: &Path) -> PathBuf {
        codex_home.join(DB_FILENAME)
    }

    pub fn register_state(&mut self, registration: &StateRegistration) -> Result<ArtifactState> {
        let now = unix_now();
        let tx = self.conn.transaction()?;
        upsert_state(&tx, registration, now)?;
        let id = tx.query_row(
            "SELECT id FROM artifact_states
             WHERE namespace = ?1 AND scope_key = ?2 AND source_key = ?3",
            params![
                registration.namespace,
                registration.scope_key,
                registration.source_key
            ],
            |row| row.get::<_, i64>(0),
        )?;
        tx.execute(
            "DELETE FROM artifact_sources WHERE state_id = ?1",
            params![id],
        )?;
        {
            let mut statement = tx.prepare(
                "INSERT INTO artifact_sources (state_id, path, kind, sha256)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for source in &registration.sources {
                statement.execute(params![
                    id,
                    normalized_relative_path(&source.path),
                    source.kind,
                    source.sha256,
                ])?;
            }
        }
        tx.commit()?;
        self.state_by_id(id)
    }

    pub fn state_by_keys(
        &self,
        namespace: &str,
        scope_key: &str,
        source_key: &str,
    ) -> Result<Option<ArtifactState>> {
        let sql = STATE_SELECT_SQL.to_string()
            + " WHERE namespace = ?1 AND scope_key = ?2 AND source_key = ?3";
        self.conn
            .query_row(
                &sql,
                params![namespace, scope_key, source_key],
                state_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn state_by_dir(&self, namespace: &str, state_dir: &Path) -> Result<Option<ArtifactState>> {
        let sql = STATE_SELECT_SQL.to_string() + " WHERE namespace = ?1 AND state_dir = ?2";
        self.conn
            .query_row(
                &sql,
                params![namespace, path_string(state_dir)],
                state_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn states_for_scope(&self, namespace: &str, scope_key: &str) -> Result<Vec<ArtifactState>> {
        let mut statement = self.conn.prepare(
            &(STATE_SELECT_SQL.to_string()
                + " WHERE namespace = ?1 AND scope_key = ?2
                    ORDER BY COALESCE(last_hit_at_unix_sec, updated_at_unix_sec, created_at_unix_sec) DESC,
                             state_dir ASC"),
        )?;
        let states = statement
            .query_map(params![namespace, scope_key], state_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(states)
    }

    pub fn record_state_hit_by_dir(&self, namespace: &str, state_dir: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE artifact_states
             SET last_hit_at_unix_sec = ?1, updated_at_unix_sec = ?1
             WHERE namespace = ?2 AND state_dir = ?3",
            params![unix_now(), namespace, path_string(state_dir)],
        )?;
        Ok(())
    }

    pub fn index_file(
        &self,
        namespace: &str,
        state_dir: &Path,
        relative_path: &Path,
    ) -> Result<()> {
        let Some(state) = self.state_by_dir(namespace, state_dir)? else {
            anyhow::bail!(
                "artifact state {} is not registered for namespace {namespace}",
                state_dir.display()
            );
        };
        let absolute_path = state_dir.join(relative_path);
        let metadata = fs::metadata(&absolute_path)
            .with_context(|| format!("failed to stat {}", absolute_path.display()))?;
        let size_bytes = i64::try_from(metadata.len()).context("artifact file too large")?;
        let updated_at_unix_sec = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or_else(unix_now, |duration| {
                i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
            });
        let sha256 = file_sha256(&absolute_path)
            .with_context(|| format!("failed to hash {}", absolute_path.display()))?;
        self.conn.execute(
            "INSERT INTO artifact_files (state_id, relative_path, size_bytes, sha256, updated_at_unix_sec)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(state_id, relative_path) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                sha256 = excluded.sha256,
                updated_at_unix_sec = excluded.updated_at_unix_sec",
            params![
                state.id,
                normalized_relative_path(relative_path),
                size_bytes,
                sha256,
                updated_at_unix_sec,
            ],
        )?;
        Ok(())
    }

    pub fn find_file(
        &self,
        namespace: &str,
        relative_path: &Path,
    ) -> Result<Option<(ArtifactState, ArtifactFile)>> {
        let relative_path = normalized_relative_path(relative_path);
        self.conn
            .query_row(
                "SELECT s.id, s.namespace, s.scope_key, s.source_key, s.state_dir,
                        s.metadata_json, s.created_at_unix_sec, s.updated_at_unix_sec,
                        s.last_hit_at_unix_sec,
                        f.relative_path, f.size_bytes, f.sha256, f.updated_at_unix_sec
                 FROM artifact_files f
                 JOIN artifact_states s ON s.id = f.state_id
                 WHERE s.namespace = ?1 AND f.relative_path = ?2
                 ORDER BY f.updated_at_unix_sec DESC, s.state_dir ASC
                 LIMIT 1",
                params![namespace, relative_path],
                |row| {
                    let state = state_from_row(row)?;
                    let size_bytes = u64::try_from(row.get::<_, i64>(10)?).unwrap_or_default();
                    Ok((
                        state,
                        ArtifactFile {
                            state_id: row.get(0)?,
                            relative_path: PathBuf::from(row.get::<_, String>(9)?),
                            size_bytes,
                            sha256: row.get(11)?,
                            updated_at_unix_sec: row.get(12)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn put_cache_entry(
        &self,
        namespace: &str,
        key: &str,
        artifact_id: &str,
        status: &str,
        metadata_json: &str,
    ) -> Result<()> {
        let now = unix_now();
        self.conn.execute(
            "INSERT INTO cache_entries
                (namespace, key, artifact_id, status, metadata_json, created_at_unix_sec,
                 updated_at_unix_sec, last_hit_at_unix_sec)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, NULL)
             ON CONFLICT(namespace, key) DO UPDATE SET
                artifact_id = excluded.artifact_id,
                status = excluded.status,
                metadata_json = excluded.metadata_json,
                updated_at_unix_sec = excluded.updated_at_unix_sec",
            params![namespace, key, artifact_id, status, metadata_json, now],
        )?;
        Ok(())
    }

    pub fn cache_entry(&self, namespace: &str, key: &str) -> Result<Option<CacheEntry>> {
        let entry = self
            .conn
            .query_row(
                "SELECT namespace, key, artifact_id, status, metadata_json,
                        created_at_unix_sec, updated_at_unix_sec, last_hit_at_unix_sec
                 FROM cache_entries WHERE namespace = ?1 AND key = ?2",
                params![namespace, key],
                cache_entry_from_row,
            )
            .optional()?;
        if entry.is_some() {
            self.conn.execute(
                "UPDATE cache_entries
                 SET last_hit_at_unix_sec = ?1, updated_at_unix_sec = ?1
                 WHERE namespace = ?2 AND key = ?3",
                params![unix_now(), namespace, key],
            )?;
        }
        Ok(entry)
    }

    pub fn delete_cache_entry(&self, namespace: &str, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM cache_entries WHERE namespace = ?1 AND key = ?2",
            params![namespace, key],
        )?;
        Ok(())
    }

    pub fn prune_stale_states(&self, namespace: &str, options: PruneOptions) -> Result<usize> {
        if let Some(last_prune_at) = self.last_prune_at(namespace)?
            && options.now_unix_sec.saturating_sub(last_prune_at) < options.throttle_secs
        {
            return Ok(0);
        }
        let cutoff = options.now_unix_sec.saturating_sub(options.retention_secs);
        let mut statement = self.conn.prepare(
            "SELECT id, state_dir FROM artifact_states
             WHERE namespace = ?1
               AND COALESCE(last_hit_at_unix_sec, updated_at_unix_sec, created_at_unix_sec) < ?2",
        )?;
        let stale = statement
            .query_map(params![namespace, cutoff], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);

        for (_, state_dir) in &stale {
            let _ = fs::remove_dir_all(state_dir);
        }
        for (id, _) in &stale {
            self.conn
                .execute("DELETE FROM artifact_states WHERE id = ?1", params![id])?;
        }
        self.conn.execute(
            "INSERT INTO maintenance_state (namespace, last_prune_at_unix_sec)
             VALUES (?1, ?2)
             ON CONFLICT(namespace) DO UPDATE SET
                last_prune_at_unix_sec = excluded.last_prune_at_unix_sec",
            params![namespace, options.now_unix_sec],
        )?;
        Ok(stale.len())
    }

    fn state_by_id(&self, id: i64) -> Result<ArtifactState> {
        let sql = STATE_SELECT_SQL.to_string() + " WHERE id = ?1";
        self.conn
            .query_row(&sql, params![id], state_from_row)
            .map_err(Into::into)
    }

    fn last_prune_at(&self, namespace: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT last_prune_at_unix_sec FROM maintenance_state WHERE namespace = ?1",
                params![namespace],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}

const STATE_SELECT_SQL: &str = "SELECT id, namespace, scope_key, source_key, state_dir,
    metadata_json, created_at_unix_sec, updated_at_unix_sec, last_hit_at_unix_sec
    FROM artifact_states";

fn upsert_state(tx: &Transaction<'_>, registration: &StateRegistration, now: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO artifact_states
            (namespace, scope_key, source_key, state_dir, metadata_json,
             created_at_unix_sec, updated_at_unix_sec, last_hit_at_unix_sec)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, NULL)
         ON CONFLICT(namespace, scope_key, source_key) DO UPDATE SET
            state_dir = excluded.state_dir,
            metadata_json = excluded.metadata_json,
            updated_at_unix_sec = excluded.updated_at_unix_sec",
        params![
            registration.namespace,
            registration.scope_key,
            registration.source_key,
            path_string(&registration.state_dir),
            registration.metadata_json,
            now,
        ],
    )?;
    Ok(())
}

fn state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactState> {
    Ok(ArtifactState {
        id: row.get(0)?,
        namespace: row.get(1)?,
        scope_key: row.get(2)?,
        source_key: row.get(3)?,
        state_dir: PathBuf::from(row.get::<_, String>(4)?),
        metadata_json: row.get(5)?,
        created_at_unix_sec: row.get(6)?,
        updated_at_unix_sec: row.get(7)?,
        last_hit_at_unix_sec: row.get(8)?,
    })
}

fn cache_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheEntry> {
    Ok(CacheEntry {
        namespace: row.get(0)?,
        key: row.get(1)?,
        artifact_id: row.get(2)?,
        status: row.get(3)?,
        metadata_json: row.get(4)?,
        created_at_unix_sec: row.get(5)?,
        updated_at_unix_sec: row.get(6)?,
        last_hit_at_unix_sec: row.get(7)?,
    })
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rusqlite::params;

    #[test]
    fn open_creates_database_schema() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db = temp.path().join("artifactory.sqlite");

        let store = Artifactory::open_at(&db).expect("open");

        assert!(db.exists());
        let table_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'artifact_states'",
                [],
                |row| row.get(0),
            )
            .expect("artifact_states table");
        assert_eq!(table_count, 1);
    }

    #[test]
    fn open_configures_pragmas() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Artifactory::open_at(temp.path().join("db.sqlite")).expect("open");

        let foreign_keys: i64 = store
            .conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign_keys");
        let busy_timeout_ms: i64 = store
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("busy_timeout");
        let journal_mode: String = store
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal_mode");

        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout_ms, 5_000);
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn registers_state_and_sources() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut store = Artifactory::open_at(temp.path().join("db.sqlite")).expect("open");
        let registration = registration(temp.path().join("state"));

        let state = store.register_state(&registration).expect("register");

        assert_eq!(
            store
                .state_by_keys("repo-ci", "scope", "source")
                .expect("lookup"),
            Some(state.clone())
        );
        let sources = store
            .conn
            .prepare("SELECT path, kind, sha256 FROM artifact_sources WHERE state_id = ?1")
            .expect("prepare")
            .query_map(params![state.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        assert_eq!(
            sources,
            vec![(
                "Cargo.toml".to_string(),
                "build_manifest".to_string(),
                "abc".to_string()
            )]
        );
    }

    #[test]
    fn indexes_and_finds_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(state_dir.join("run-artifacts")).expect("state dir");
        let file = state_dir.join("run-artifacts/artifact.json");
        fs::write(&file, b"payload").expect("write file");
        let mut store = Artifactory::open_at(temp.path().join("db.sqlite")).expect("open");
        store
            .register_state(&registration(state_dir.clone()))
            .expect("register");

        store
            .index_file(
                "repo-ci",
                &state_dir,
                Path::new("run-artifacts/artifact.json"),
            )
            .expect("index");
        let (state, file) = store
            .find_file("repo-ci", Path::new("run-artifacts/artifact.json"))
            .expect("find")
            .expect("file");

        assert_eq!(state.state_dir, state_dir);
        assert_eq!(
            file.relative_path,
            PathBuf::from("run-artifacts/artifact.json")
        );
        assert_eq!(file.size_bytes, 7);
    }

    #[test]
    fn cache_lookup_records_hits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Artifactory::open_at(temp.path().join("db.sqlite")).expect("open");

        store
            .put_cache_entry("repo-ci", "key", "artifact", "passed", "{}")
            .expect("put");
        let entry = store
            .cache_entry("repo-ci", "key")
            .expect("cache")
            .expect("entry");
        let updated = store
            .cache_entry("repo-ci", "key")
            .expect("cache")
            .expect("entry");

        assert_eq!(entry.artifact_id, "artifact");
        assert!(updated.last_hit_at_unix_sec.is_some());
    }

    #[test]
    fn delete_cache_entry_removes_entry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Artifactory::open_at(temp.path().join("db.sqlite")).expect("open");
        store
            .put_cache_entry("repo-ci", "key", "artifact", "passed", "{}")
            .expect("put");

        store.delete_cache_entry("repo-ci", "key").expect("delete");

        assert_eq!(store.cache_entry("repo-ci", "key").expect("cache"), None);
    }

    #[test]
    fn prune_removes_stale_states_and_throttles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let old_dir = temp.path().join("old");
        let fresh_dir = temp.path().join("fresh");
        fs::create_dir_all(&old_dir).expect("old dir");
        fs::create_dir_all(&fresh_dir).expect("fresh dir");
        let mut store = Artifactory::open_at(temp.path().join("db.sqlite")).expect("open");
        store
            .register_state(&registration_for(&old_dir, "old-source"))
            .expect("old register");
        store
            .register_state(&registration_for(&fresh_dir, "fresh-source"))
            .expect("fresh register");
        store
            .conn
            .execute(
                "UPDATE artifact_states SET last_hit_at_unix_sec = CASE source_key
                    WHEN 'old-source' THEN 10 ELSE 100 END",
                [],
            )
            .expect("set hits");

        let removed = store
            .prune_stale_states(
                "repo-ci",
                PruneOptions::with_now(
                    /*retention_secs*/ 50, /*throttle_secs*/ 100,
                    /*now_unix_sec*/ 80,
                ),
            )
            .expect("prune");
        let throttled = store
            .prune_stale_states(
                "repo-ci",
                PruneOptions::with_now(
                    /*retention_secs*/ 50, /*throttle_secs*/ 100,
                    /*now_unix_sec*/ 81,
                ),
            )
            .expect("prune throttled");

        assert_eq!(removed, 1);
        assert_eq!(throttled, 0);
        assert!(!old_dir.exists());
        assert!(fresh_dir.exists());
        assert!(
            store
                .state_by_keys("repo-ci", "scope", "old-source")
                .expect("old lookup")
                .is_none()
        );
    }

    fn registration(state_dir: PathBuf) -> StateRegistration {
        registration_for(&state_dir, "source")
    }

    fn registration_for(state_dir: &Path, source_key: &str) -> StateRegistration {
        StateRegistration {
            namespace: "repo-ci".to_string(),
            scope_key: "scope".to_string(),
            source_key: source_key.to_string(),
            state_dir: state_dir.to_path_buf(),
            sources: vec![ArtifactSource::new(
                PathBuf::from("Cargo.toml"),
                "build_manifest",
                "abc".to_string(),
            )],
            metadata_json: "{}".to_string(),
        }
    }
}
