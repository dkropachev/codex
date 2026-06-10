use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::review_types::ReviewTypeDefinition;
use crate::work_size::RepoSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisregardedFinding {
    pub review_type_id: String,
    pub title: String,
    pub reason: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExperimentDecision {
    pub review_type_id: String,
    pub model_key: String,
    pub split_strategy: String,
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScoreSummary {
    pub runs: u32,
    pub work_size_units: u32,
    pub score: f64,
}

pub(crate) struct DevCycleState {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl DevCycleState {
    pub(crate) fn open(state_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create {}", state_dir.display()))?;
        let path = state_dir.join("dev_cycle.sqlite3");
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let state = Self {
            path,
            conn: Mutex::new(conn),
        };
        state.ensure_schema()?;
        Ok(state)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn start_run(
        &self,
        run_id: &str,
        repo: &RepoSnapshot,
        task_description: Option<&str>,
        selected_review_types: &[ReviewTypeDefinition],
    ) -> anyhow::Result<()> {
        let selected_json = serde_json::to_string(selected_review_types)?;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO runs (
                id, repo_identity, repo_family, repo_tshirt_bucket, task_description,
                work_size_units, status, started_at, selected_review_types_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, ?8)",
            params![
                run_id,
                repo.repo_identity,
                repo.repo_family,
                repo.work_size.repo_tshirt_bucket,
                task_description,
                repo.work_size.work_size_units,
                now_unix_seconds(),
                selected_json,
            ],
        )?;
        for definition in selected_review_types {
            conn.execute(
                "INSERT INTO review_type_definitions (
                    run_id, review_type_id, short_name, description, prompt, exclude_prompt, enabled
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run_id,
                    definition.id,
                    definition.short_name,
                    definition.description,
                    definition.prompt,
                    definition.exclude_prompt,
                    definition.enabled,
                ],
            )?;
        }
        Ok(())
    }

    pub(crate) fn finish_run(&self, run_id: &str, status: &str) -> anyhow::Result<()> {
        self.conn()?.execute(
            "UPDATE runs SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status, now_unix_seconds(), run_id],
        )?;
        Ok(())
    }

    pub(crate) fn record_agent_attempt(
        &self,
        record: AgentAttemptRecord<'_>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO agent_attempts (
                run_id, role, name, agent_id, model_key, status, prompt_digest, output_json,
                created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.run_id,
                record.role,
                record.name,
                record.agent_id,
                record.model_key,
                record.status,
                digest(record.prompt),
                record.output_json,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_finding(&self, record: FindingRecord<'_>) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO findings (
                id, run_id, review_type_id, producer_agent_id, writer_agent_id, title, details,
                file_path, line, severity, status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.id,
                record.run_id,
                record.review_type_id,
                record.producer_agent_id,
                record.writer_agent_id,
                record.title,
                record.details,
                record.file_path,
                record.line,
                record.severity,
                record.status,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_verifier_decision(
        &self,
        record: VerifierDecisionRecord<'_>,
    ) -> anyhow::Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO verifier_decisions (
                finding_id, verifier_agent_id, model_key, accepted, reason, work_size_units,
                created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.finding_id,
                record.verifier_agent_id,
                record.model_key,
                record.accepted,
                record.reason,
                record.work_size_units,
                now_unix_seconds(),
            ],
        )?;
        conn.execute(
            "UPDATE findings SET status = ?1 WHERE id = ?2",
            params![
                if record.accepted {
                    "verified"
                } else {
                    "disregarded"
                },
                record.finding_id,
            ],
        )?;
        conn.execute(
            "INSERT INTO model_effort_scores (
                repo_family, review_type_id, model_key, runs, work_size_units, accepted, rejected,
                score
             ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7)
             ON CONFLICT(repo_family, review_type_id, model_key) DO UPDATE SET
                runs = runs + 1,
                work_size_units = work_size_units + excluded.work_size_units,
                accepted = accepted + excluded.accepted,
                rejected = rejected + excluded.rejected,
                score = CAST(accepted + excluded.accepted AS REAL) /
                    CAST(accepted + rejected + excluded.accepted + excluded.rejected AS REAL)",
            params![
                record.repo_family,
                record.review_type_id,
                record.model_key,
                record.work_size_units,
                i64::from(record.accepted),
                i64::from(!record.accepted),
                if record.accepted { 1.0 } else { 0.0 },
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_disregarded_finding(
        &self,
        run_id: &str,
        review_type_id: &str,
        title: &str,
        reason: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO disregarded_findings (
                run_id, review_type_id, title, reason, status, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id,
                review_type_id,
                title,
                reason,
                status,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn recent_disregarded_findings(
        &self,
        review_type_id: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<DisregardedFinding>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT review_type_id, title, reason, status
             FROM disregarded_findings
             WHERE review_type_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![review_type_id, limit], |row| {
            Ok(DisregardedFinding {
                review_type_id: row.get(0)?,
                title: row.get(1)?,
                reason: row.get(2)?,
                status: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn score_summary(
        &self,
        repo_family: &str,
        review_type_id: &str,
        model_key: &str,
    ) -> anyhow::Result<Option<ScoreSummary>> {
        self.conn()?
            .query_row(
                "SELECT runs, work_size_units, score
                 FROM model_effort_scores
                 WHERE repo_family = ?1 AND review_type_id = ?2 AND model_key = ?3",
                params![repo_family, review_type_id, model_key],
                |row| {
                    Ok(ScoreSummary {
                        runs: row.get::<_, u32>(0)?,
                        work_size_units: row.get::<_, u32>(1)?,
                        score: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn record_experiment_decision(
        &self,
        run_id: &str,
        decision: &ExperimentDecision,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO experiment_decisions (
                run_id, review_type_id, model_key, split_strategy, decision, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                decision.review_type_id,
                decision.model_key,
                decision.split_strategy,
                decision.decision,
                decision.reason,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    fn ensure_schema(&self) -> anyhow::Result<()> {
        self.conn()?.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                repo_identity TEXT NOT NULL,
                repo_family TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL DEFAULT 'M',
                task_description TEXT,
                work_size_units INTEGER NOT NULL,
                status TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                selected_review_types_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS review_type_definitions (
                run_id TEXT NOT NULL,
                review_type_id TEXT NOT NULL,
                short_name TEXT NOT NULL,
                description TEXT NOT NULL,
                prompt TEXT,
                exclude_prompt TEXT,
                enabled INTEGER NOT NULL,
                PRIMARY KEY (run_id, review_type_id)
            );
            CREATE TABLE IF NOT EXISTS agent_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                role TEXT NOT NULL,
                name TEXT NOT NULL,
                agent_id TEXT,
                model_key TEXT,
                status TEXT NOT NULL,
                prompt_digest TEXT NOT NULL,
                output_json TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS findings (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                review_type_id TEXT NOT NULL,
                producer_agent_id TEXT NOT NULL,
                writer_agent_id TEXT,
                title TEXT NOT NULL,
                details TEXT NOT NULL,
                file_path TEXT,
                line INTEGER,
                severity TEXT NOT NULL,
                status TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS verifier_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                finding_id TEXT NOT NULL,
                verifier_agent_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                accepted INTEGER NOT NULL,
                reason TEXT NOT NULL,
                work_size_units INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS disregarded_findings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                review_type_id TEXT NOT NULL,
                title TEXT NOT NULL,
                reason TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS model_effort_scores (
                repo_family TEXT NOT NULL,
                review_type_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                runs INTEGER NOT NULL,
                work_size_units INTEGER NOT NULL,
                accepted INTEGER NOT NULL,
                rejected INTEGER NOT NULL,
                score REAL NOT NULL,
                PRIMARY KEY (repo_family, review_type_id, model_key)
            );
            CREATE TABLE IF NOT EXISTS experiment_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                review_type_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                split_strategy TEXT NOT NULL,
                decision TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            ",
        )?;
        self.ensure_column(
            "runs",
            "repo_tshirt_bucket",
            "ALTER TABLE runs ADD COLUMN repo_tshirt_bucket TEXT NOT NULL DEFAULT 'M'",
        )?;
        self.ensure_split_schema()?;
        Ok(())
    }

    pub(crate) fn ensure_column(
        &self,
        table: &str,
        column: &str,
        alter_sql: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn()?;
        let found = {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for row in rows {
                if row? == column {
                    found = true;
                    break;
                }
            }
            found
        };
        if !found {
            conn.execute(alter_sql, [])?;
        }
        Ok(())
    }

    pub(crate) fn conn(&self) -> anyhow::Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("dev-cycle sqlite connection mutex poisoned"))
    }
}

pub(crate) struct FindingRecord<'a> {
    pub id: &'a str,
    pub run_id: &'a str,
    pub review_type_id: &'a str,
    pub producer_agent_id: &'a str,
    pub writer_agent_id: Option<&'a str>,
    pub title: &'a str,
    pub details: &'a str,
    pub file_path: Option<&'a str>,
    pub line: Option<u32>,
    pub severity: &'a str,
    pub status: &'a str,
}

pub(crate) struct AgentAttemptRecord<'a> {
    pub run_id: &'a str,
    pub role: &'a str,
    pub name: &'a str,
    pub agent_id: Option<&'a str>,
    pub model_key: Option<&'a str>,
    pub status: &'a str,
    pub prompt: &'a str,
    pub output_json: Option<&'a str>,
}

pub(crate) struct VerifierDecisionRecord<'a> {
    pub finding_id: &'a str,
    pub verifier_agent_id: &'a str,
    pub model_key: &'a str,
    pub accepted: bool,
    pub reason: &'a str,
    pub work_size_units: u32,
    pub repo_family: &'a str,
    pub review_type_id: &'a str,
}

pub(crate) fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::review_types::built_in_review_types;
    use crate::work_size::WorkSize;

    #[test]
    fn persistence_roundtrips_run_finding_score_and_disregarded_records() {
        let tempdir = tempfile::tempdir().unwrap();
        let state = DevCycleState::open(tempdir.path()).unwrap();
        let repo = RepoSnapshot {
            repo_identity: "repo".to_string(),
            repo_family: "family".to_string(),
            remote_hash: None,
            path_hash: "path".to_string(),
            work_size: WorkSize {
                tracked_files: 20,
                changed_files: 1,
                changed_lines: 10,
                touched_modules: 1,
                language_mix: Default::default(),
                shared_api_changed: false,
                ui_changed: false,
                tests_changed: false,
                work_size_units: 3,
                repo_tshirt_bucket: "XS".to_string(),
            },
        };
        let review_types = built_in_review_types();
        state
            .start_run("run-1", &repo, Some("task"), &review_types[..1])
            .unwrap();
        state
            .record_finding(FindingRecord {
                id: "finding-1",
                run_id: "run-1",
                review_type_id: "correctness",
                producer_agent_id: "reviewer-1",
                writer_agent_id: Some("writer-1"),
                title: "Bug",
                details: "details",
                file_path: Some("src/lib.rs"),
                line: Some(7),
                severity: "high",
                status: "candidate",
            })
            .unwrap();
        state
            .record_verifier_decision(VerifierDecisionRecord {
                finding_id: "finding-1",
                verifier_agent_id: "verifier-1",
                model_key: "openai:gpt-5.5:xhigh",
                accepted: true,
                reason: "confirmed",
                work_size_units: 3,
                repo_family: "family",
                review_type_id: "correctness",
            })
            .unwrap();
        state
            .record_disregarded_finding(
                "run-1",
                "correctness",
                "Old false positive",
                "duplicate",
                "duplicate",
            )
            .unwrap();

        assert_eq!(
            state
                .score_summary("family", "correctness", "openai:gpt-5.5:xhigh")
                .unwrap(),
            Some(ScoreSummary {
                runs: 1,
                work_size_units: 3,
                score: 1.0,
            })
        );
        assert_eq!(
            state.recent_disregarded_findings("correctness", 5).unwrap(),
            vec![DisregardedFinding {
                review_type_id: "correctness".to_string(),
                title: "Old false positive".to_string(),
                reason: "duplicate".to_string(),
                status: "duplicate".to_string(),
            }]
        );
        state.finish_run("run-1", "succeeded").unwrap();
    }
}
