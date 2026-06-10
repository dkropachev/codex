use rusqlite::OptionalExtension;
use rusqlite::params;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::persistence::DevCycleState;
use crate::persistence::digest;
use crate::persistence::now_unix_seconds;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SplitStrategyRecord {
    pub strategy_id: String,
    pub groups_json: String,
    pub rationale: String,
    pub expected_reviewer_count_savings: f64,
    pub risk_notes_json: String,
}

impl DevCycleState {
    pub(crate) fn ensure_split_schema(&self) -> anyhow::Result<()> {
        self.conn()?.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS split_strategy_proposals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL,
                item_set_key TEXT NOT NULL,
                strategy_id TEXT NOT NULL,
                groups_json TEXT NOT NULL,
                rationale TEXT NOT NULL,
                expected_reviewer_count_savings REAL NOT NULL,
                risk_notes_json TEXT NOT NULL,
                prompt_digest TEXT NOT NULL,
                raw_output_json TEXT,
                status TEXT NOT NULL,
                rejection_code TEXT,
                rejection_reason TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS split_strategy_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL,
                item_set_key TEXT NOT NULL,
                strategy_id TEXT NOT NULL,
                groups_json TEXT NOT NULL,
                reviewer_group_count INTEGER NOT NULL,
                baseline_strategy_id TEXT,
                baseline_group_count INTEGER,
                status TEXT NOT NULL,
                reviewer_count_savings INTEGER NOT NULL,
                lost_evidence_count INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS split_lost_evidence (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                model_key TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL,
                item_set_key TEXT NOT NULL,
                strategy_id TEXT NOT NULL,
                review_type_id TEXT NOT NULL,
                finding_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                title TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS split_scores (
                model_key TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL,
                item_set_key TEXT NOT NULL,
                strategy_id TEXT NOT NULL,
                groups_json TEXT NOT NULL,
                rationale TEXT NOT NULL,
                expected_reviewer_count_savings REAL NOT NULL,
                risk_notes_json TEXT NOT NULL,
                runs INTEGER NOT NULL,
                accepted INTEGER NOT NULL,
                rejected INTEGER NOT NULL,
                lost_evidence_count INTEGER NOT NULL,
                reviewer_count_savings INTEGER NOT NULL,
                score REAL NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (model_key, repo_tshirt_bucket, item_set_key, strategy_id)
            );
            CREATE TABLE IF NOT EXISTS split_suppressions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_key TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL,
                item_set_key TEXT NOT NULL,
                strategy_id TEXT NOT NULL,
                item_neighborhood_key TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                UNIQUE (model_key, repo_tshirt_bucket, item_set_key, item_neighborhood_key)
            );
            CREATE TABLE IF NOT EXISTS split_promotions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_key TEXT NOT NULL,
                repo_tshirt_bucket TEXT NOT NULL,
                item_set_key TEXT NOT NULL,
                strategy_id TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_split_strategy_proposals_scope_status
                ON split_strategy_proposals (
                    model_key, repo_tshirt_bucket, item_set_key, status, strategy_id
                );
            CREATE INDEX IF NOT EXISTS idx_split_strategy_attempts_scope_status
                ON split_strategy_attempts (
                    model_key, repo_tshirt_bucket, item_set_key, status, strategy_id
                );
            CREATE INDEX IF NOT EXISTS idx_split_strategy_attempts_model
                ON split_strategy_attempts (model_key, created_at);
            CREATE INDEX IF NOT EXISTS idx_split_scores_model_updated
                ON split_scores (model_key, updated_at);
            CREATE INDEX IF NOT EXISTS idx_split_suppressions_scope_strategy
                ON split_suppressions (
                    model_key, repo_tshirt_bucket, item_set_key, strategy_id
                );
            ",
        )?;
        Ok(())
    }

    pub(crate) fn record_split_proposal(
        &self,
        record: SplitProposalRecord<'_>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO split_strategy_proposals (
                run_id, model_key, repo_tshirt_bucket, item_set_key, strategy_id, groups_json,
                rationale, expected_reviewer_count_savings, risk_notes_json, prompt_digest,
                raw_output_json, status, rejection_code, rejection_reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                record.run_id,
                record.model_key,
                record.repo_tshirt_bucket,
                record.item_set_key,
                record.strategy_id,
                record.groups_json,
                record.rationale,
                record.expected_reviewer_count_savings,
                record.risk_notes_json,
                digest(record.prompt),
                record.raw_output_json,
                record.status,
                record.rejection_code,
                record.rejection_reason,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_split_attempt(
        &self,
        record: SplitAttemptRecord<'_>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO split_strategy_attempts (
                run_id, model_key, repo_tshirt_bucket, item_set_key, strategy_id, groups_json,
                reviewer_group_count, baseline_strategy_id, baseline_group_count, status,
                reviewer_count_savings, lost_evidence_count, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                record.run_id,
                record.model_key,
                record.repo_tshirt_bucket,
                record.item_set_key,
                record.strategy_id,
                record.groups_json,
                record.reviewer_group_count,
                record.baseline_strategy_id,
                record.baseline_group_count,
                record.status,
                record.reviewer_count_savings,
                record.lost_evidence_count,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_split_score(&self, record: SplitScoreRecord<'_>) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO split_scores (
                model_key, repo_tshirt_bucket, item_set_key, strategy_id, groups_json,
                rationale, expected_reviewer_count_savings, risk_notes_json, runs,
                accepted, rejected, lost_evidence_count, reviewer_count_savings, score,
                updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(model_key, repo_tshirt_bucket, item_set_key, strategy_id) DO UPDATE SET
                groups_json = excluded.groups_json,
                rationale = excluded.rationale,
                expected_reviewer_count_savings = excluded.expected_reviewer_count_savings,
                risk_notes_json = excluded.risk_notes_json,
                runs = runs + 1,
                accepted = accepted + excluded.accepted,
                rejected = rejected + excluded.rejected,
                lost_evidence_count = lost_evidence_count + excluded.lost_evidence_count,
                reviewer_count_savings = reviewer_count_savings + excluded.reviewer_count_savings,
                score = CASE
                    WHEN lost_evidence_count + excluded.lost_evidence_count > 0 THEN 0.0
                    ELSE CAST(accepted + excluded.accepted AS REAL) /
                        CAST(accepted + rejected + excluded.accepted + excluded.rejected AS REAL)
                END,
                updated_at = excluded.updated_at",
            params![
                record.model_key,
                record.repo_tshirt_bucket,
                record.item_set_key,
                record.strategy_id,
                record.groups_json,
                record.rationale,
                record.expected_reviewer_count_savings,
                record.risk_notes_json,
                i64::from(record.accepted),
                i64::from(!record.accepted),
                record.lost_evidence_count,
                record.reviewer_count_savings,
                if record.accepted { 1.0 } else { 0.0 },
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_split_lost_evidence(
        &self,
        record: SplitLostEvidenceRecord<'_>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO split_lost_evidence (
                run_id, model_key, repo_tshirt_bucket, item_set_key, strategy_id,
                review_type_id, finding_id, fingerprint, title, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.run_id,
                record.model_key,
                record.repo_tshirt_bucket,
                record.item_set_key,
                record.strategy_id,
                record.review_type_id,
                record.finding_id,
                record.fingerprint,
                record.title,
                record.reason,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_split_suppression(
        &self,
        record: SplitSuppressionRecord<'_>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT OR IGNORE INTO split_suppressions (
                model_key, repo_tshirt_bucket, item_set_key, strategy_id, item_neighborhood_key,
                reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.model_key,
                record.repo_tshirt_bucket,
                record.item_set_key,
                record.strategy_id,
                record.item_neighborhood_key,
                record.reason,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_split_promotion(
        &self,
        record: SplitPromotionRecord<'_>,
    ) -> anyhow::Result<()> {
        self.conn()?.execute(
            "INSERT INTO split_promotions (
                model_key, repo_tshirt_bucket, item_set_key, strategy_id, reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.model_key,
                record.repo_tshirt_bucket,
                record.item_set_key,
                record.strategy_id,
                record.reason,
                now_unix_seconds(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn best_split_strategy_record(
        &self,
        model_key: &str,
        repo_tshirt_bucket: &str,
        item_set_key: &str,
    ) -> anyhow::Result<Option<SplitStrategyRecord>> {
        self.conn()?
            .query_row(
                "SELECT strategy_id, groups_json, rationale, expected_reviewer_count_savings,
                    risk_notes_json
                 FROM split_scores AS scores
                 WHERE model_key = ?1
                   AND repo_tshirt_bucket = ?2
                   AND item_set_key = ?3
                   AND accepted > 0
                   AND lost_evidence_count = 0
                   AND strategy_id NOT IN (
                       SELECT strategy_id
                       FROM split_suppressions
                       WHERE model_key = ?1
                         AND repo_tshirt_bucket = ?2
                         AND item_set_key = ?3
                   )
                 ORDER BY score DESC, reviewer_count_savings DESC, updated_at DESC
                 LIMIT 1",
                params![model_key, repo_tshirt_bucket, item_set_key],
                |row| {
                    Ok(SplitStrategyRecord {
                        strategy_id: row.get(0)?,
                        groups_json: row.get(1)?,
                        rationale: row.get(2)?,
                        expected_reviewer_count_savings: row.get(3)?,
                        risk_notes_json: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn rejected_grouping_experiment_count(
        &self,
        model_key: &str,
        repo_tshirt_bucket: &str,
        item_set_key: &str,
    ) -> anyhow::Result<u32> {
        self.conn()?
            .query_row(
                "SELECT COUNT(*) FROM (
                    SELECT DISTINCT strategy_id
                    FROM split_strategy_proposals
                    WHERE model_key = ?1
                      AND repo_tshirt_bucket = ?2
                      AND item_set_key = ?3
                      AND status = 'rejected'
                    UNION
                    SELECT DISTINCT strategy_id
                    FROM split_strategy_attempts
                    WHERE model_key = ?1
                      AND repo_tshirt_bucket = ?2
                      AND item_set_key = ?3
                      AND status IN ('failed', 'suppressed', 'rejected')
                )",
                params![model_key, repo_tshirt_bucket, item_set_key],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub(crate) fn has_split_evidence_for_model(&self, model_key: &str) -> anyhow::Result<bool> {
        let count: u32 = self.conn()?.query_row(
            "SELECT
                (SELECT COUNT(*) FROM split_strategy_attempts WHERE model_key = ?1) +
                (SELECT COUNT(*) FROM split_scores WHERE model_key = ?1)",
            params![model_key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub(crate) fn split_strategy_seen(
        &self,
        model_key: &str,
        repo_tshirt_bucket: &str,
        item_set_key: &str,
        strategy_id: &str,
    ) -> anyhow::Result<bool> {
        let count: u32 = self.conn()?.query_row(
            "SELECT
                (SELECT COUNT(*) FROM split_strategy_proposals
                 WHERE model_key = ?1 AND repo_tshirt_bucket = ?2 AND item_set_key = ?3
                   AND strategy_id = ?4) +
                (SELECT COUNT(*) FROM split_strategy_attempts
                 WHERE model_key = ?1 AND repo_tshirt_bucket = ?2 AND item_set_key = ?3
                   AND strategy_id = ?4)",
            params![model_key, repo_tshirt_bucket, item_set_key, strategy_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub(crate) fn suppressed_split_neighborhoods(
        &self,
        model_key: &str,
        repo_tshirt_bucket: &str,
        item_set_key: &str,
    ) -> anyhow::Result<Vec<Vec<String>>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT item_neighborhood_key
             FROM split_suppressions
             WHERE model_key = ?1 AND repo_tshirt_bucket = ?2 AND item_set_key = ?3",
        )?;
        let rows = stmt.query_map(
            params![model_key, repo_tshirt_bucket, item_set_key],
            |row| {
                let key: String = row.get(0)?;
                Ok(key
                    .split('+')
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>())
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn split_evidence_for_prompt(
        &self,
        model_key: &str,
        repo_tshirt_bucket: &str,
        item_set_key: &str,
    ) -> anyhow::Result<JsonValue> {
        Ok(json!({
            "globalSameModel": self.split_score_rows(model_key, None, None)?,
            "globalSameModelAttempts": self.split_attempt_rows(model_key, None, None)?,
            "repoSizeBucket": repo_tshirt_bucket,
            "repoSizeSpecific": self.split_score_rows(model_key, Some(repo_tshirt_bucket), None)?,
            "repoSizeSpecificAttempts": self.split_attempt_rows(model_key, Some(repo_tshirt_bucket), None)?,
            "sameItemSetAndBucket": self.split_score_rows(model_key, Some(repo_tshirt_bucket), Some(item_set_key))?,
            "sameItemSetAndBucketAttempts": self.split_attempt_rows(model_key, Some(repo_tshirt_bucket), Some(item_set_key))?,
        }))
    }

    fn split_score_rows(
        &self,
        model_key: &str,
        repo_tshirt_bucket: Option<&str>,
        item_set_key: Option<&str>,
    ) -> anyhow::Result<Vec<JsonValue>> {
        let conn = self.conn()?;
        let mut sql = "SELECT repo_tshirt_bucket, item_set_key, strategy_id, runs, accepted,
                rejected, lost_evidence_count, reviewer_count_savings, score
             FROM split_scores
             WHERE model_key = ?1"
            .to_string();
        if repo_tshirt_bucket.is_some() {
            sql.push_str(" AND repo_tshirt_bucket = ?2");
        }
        if item_set_key.is_some() {
            sql.push_str(if repo_tshirt_bucket.is_some() {
                " AND item_set_key = ?3"
            } else {
                " AND item_set_key = ?2"
            });
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT 30");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match (repo_tshirt_bucket, item_set_key) {
            (Some(bucket), Some(item_set_key)) => {
                stmt.query(params![model_key, bucket, item_set_key])?
            }
            (Some(bucket), None) => stmt.query(params![model_key, bucket])?,
            (None, Some(item_set_key)) => stmt.query(params![model_key, item_set_key])?,
            (None, None) => stmt.query(params![model_key])?,
        };
        let mut values = Vec::new();
        while let Some(row) = rows.next()? {
            values.push(json!({
                "repoTshirtBucket": row.get::<_, String>(0)?,
                "itemSetKey": row.get::<_, String>(1)?,
                "strategyId": row.get::<_, String>(2)?,
                "runs": row.get::<_, u32>(3)?,
                "accepted": row.get::<_, u32>(4)?,
                "rejected": row.get::<_, u32>(5)?,
                "lostEvidenceCount": row.get::<_, u32>(6)?,
                "reviewerCountSavings": row.get::<_, i64>(7)?,
                "score": row.get::<_, f64>(8)?,
            }));
        }
        Ok(values)
    }

    fn split_attempt_rows(
        &self,
        model_key: &str,
        repo_tshirt_bucket: Option<&str>,
        item_set_key: Option<&str>,
    ) -> anyhow::Result<Vec<JsonValue>> {
        let conn = self.conn()?;
        let mut sql = "SELECT repo_tshirt_bucket, item_set_key, strategy_id,
                reviewer_group_count, baseline_strategy_id, baseline_group_count, status,
                reviewer_count_savings, lost_evidence_count, created_at
             FROM split_strategy_attempts
             WHERE model_key = ?1"
            .to_string();
        if repo_tshirt_bucket.is_some() {
            sql.push_str(" AND repo_tshirt_bucket = ?2");
        }
        if item_set_key.is_some() {
            sql.push_str(if repo_tshirt_bucket.is_some() {
                " AND item_set_key = ?3"
            } else {
                " AND item_set_key = ?2"
            });
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT 30");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match (repo_tshirt_bucket, item_set_key) {
            (Some(bucket), Some(item_set_key)) => {
                stmt.query(params![model_key, bucket, item_set_key])?
            }
            (Some(bucket), None) => stmt.query(params![model_key, bucket])?,
            (None, Some(item_set_key)) => stmt.query(params![model_key, item_set_key])?,
            (None, None) => stmt.query(params![model_key])?,
        };
        let mut values = Vec::new();
        while let Some(row) = rows.next()? {
            values.push(json!({
                "repoTshirtBucket": row.get::<_, String>(0)?,
                "itemSetKey": row.get::<_, String>(1)?,
                "strategyId": row.get::<_, String>(2)?,
                "reviewerGroupCount": row.get::<_, u32>(3)?,
                "baselineStrategyId": row.get::<_, Option<String>>(4)?,
                "baselineGroupCount": row.get::<_, Option<u32>>(5)?,
                "status": row.get::<_, String>(6)?,
                "reviewerCountSavings": row.get::<_, i64>(7)?,
                "lostEvidenceCount": row.get::<_, u32>(8)?,
                "createdAt": row.get::<_, i64>(9)?,
            }));
        }
        Ok(values)
    }
}

pub(crate) struct SplitProposalRecord<'a> {
    pub run_id: &'a str,
    pub model_key: &'a str,
    pub repo_tshirt_bucket: &'a str,
    pub item_set_key: &'a str,
    pub strategy_id: &'a str,
    pub groups_json: &'a str,
    pub rationale: &'a str,
    pub expected_reviewer_count_savings: f64,
    pub risk_notes_json: &'a str,
    pub prompt: &'a str,
    pub raw_output_json: Option<&'a str>,
    pub status: &'a str,
    pub rejection_code: Option<&'a str>,
    pub rejection_reason: Option<&'a str>,
}

pub(crate) struct SplitAttemptRecord<'a> {
    pub run_id: &'a str,
    pub model_key: &'a str,
    pub repo_tshirt_bucket: &'a str,
    pub item_set_key: &'a str,
    pub strategy_id: &'a str,
    pub groups_json: &'a str,
    pub reviewer_group_count: u32,
    pub baseline_strategy_id: Option<&'a str>,
    pub baseline_group_count: Option<u32>,
    pub status: &'a str,
    pub reviewer_count_savings: i64,
    pub lost_evidence_count: u32,
}

pub(crate) struct SplitScoreRecord<'a> {
    pub model_key: &'a str,
    pub repo_tshirt_bucket: &'a str,
    pub item_set_key: &'a str,
    pub strategy_id: &'a str,
    pub groups_json: &'a str,
    pub rationale: &'a str,
    pub expected_reviewer_count_savings: f64,
    pub risk_notes_json: &'a str,
    pub accepted: bool,
    pub lost_evidence_count: u32,
    pub reviewer_count_savings: i64,
}

pub(crate) struct SplitLostEvidenceRecord<'a> {
    pub run_id: &'a str,
    pub model_key: &'a str,
    pub repo_tshirt_bucket: &'a str,
    pub item_set_key: &'a str,
    pub strategy_id: &'a str,
    pub review_type_id: &'a str,
    pub finding_id: &'a str,
    pub fingerprint: &'a str,
    pub title: &'a str,
    pub reason: &'a str,
}

pub(crate) struct SplitSuppressionRecord<'a> {
    pub model_key: &'a str,
    pub repo_tshirt_bucket: &'a str,
    pub item_set_key: &'a str,
    pub strategy_id: &'a str,
    pub item_neighborhood_key: &'a str,
    pub reason: &'a str,
}

pub(crate) struct SplitPromotionRecord<'a> {
    pub model_key: &'a str,
    pub repo_tshirt_bucket: &'a str,
    pub item_set_key: &'a str,
    pub strategy_id: &'a str,
    pub reason: &'a str,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn split_scores_are_model_and_bucket_scoped() {
        let tempdir = tempfile::tempdir().unwrap();
        let state = DevCycleState::open(tempdir.path()).unwrap();
        state
            .record_split_score(SplitScoreRecord {
                model_key: "openai:gpt-5.5:xhigh",
                repo_tshirt_bucket: "S",
                item_set_key: "items",
                strategy_id: "grouping:v1",
                groups_json: "[{\"groupId\":\"core\",\"reviewTypeIds\":[\"correctness\",\"tests\"]}]",
                rationale: "same failure surface",
                expected_reviewer_count_savings: 1.0,
                risk_notes_json: "[]",
                accepted: true,
                lost_evidence_count: 0,
                reviewer_count_savings: 1,
            })
            .unwrap();

        assert_eq!(
            state
                .best_split_strategy_record("openai:gpt-5.5:xhigh", "S", "items")
                .unwrap()
                .map(|record| record.strategy_id),
            Some("grouping:v1".to_string())
        );
        assert_eq!(
            state
                .best_split_strategy_record("openai:gpt-5.5:high", "S", "items")
                .unwrap(),
            None
        );
        assert_eq!(
            state
                .best_split_strategy_record("openai:gpt-5.5:xhigh", "XL", "items")
                .unwrap(),
            None
        );
    }

    #[test]
    fn split_evidence_for_prompt_includes_attempts_before_scores_exist() {
        let tempdir = tempfile::tempdir().unwrap();
        let state = DevCycleState::open(tempdir.path()).unwrap();
        state
            .record_split_attempt(SplitAttemptRecord {
                run_id: "run-1",
                model_key: "openai:gpt-5.5:xhigh",
                repo_tshirt_bucket: "S",
                item_set_key: "items",
                strategy_id: "separate:v1",
                groups_json: "[]",
                reviewer_group_count: 2,
                baseline_strategy_id: None,
                baseline_group_count: None,
                status: "separate",
                reviewer_count_savings: 0,
                lost_evidence_count: 0,
            })
            .unwrap();

        let evidence = state
            .split_evidence_for_prompt("openai:gpt-5.5:xhigh", "S", "items")
            .unwrap();

        assert_eq!(evidence["globalSameModel"], json!([]));
        assert_eq!(
            evidence["globalSameModelAttempts"],
            json!([{
                "repoTshirtBucket": "S",
                "itemSetKey": "items",
                "strategyId": "separate:v1",
                "reviewerGroupCount": 2,
                "baselineStrategyId": null,
                "baselineGroupCount": null,
                "status": "separate",
                "reviewerCountSavings": 0,
                "lostEvidenceCount": 0,
                "createdAt": evidence["globalSameModelAttempts"][0]["createdAt"],
            }])
        );
        assert_eq!(
            evidence["sameItemSetAndBucketAttempts"],
            evidence["globalSameModelAttempts"]
        );
    }
}
