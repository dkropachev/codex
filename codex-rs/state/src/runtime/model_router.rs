use crate::runtime::StateRuntime;
use chrono::Utc;
use codex_model_router::RouterRequestKind;
use codex_model_router::RouterSavings;
use codex_model_router::summarize_savings;
use codex_protocol::protocol::TokenUsage;
use sqlx::Row;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRouterLedgerEntry {
    pub task_key: String,
    pub request_kind: RouterRequestKind,
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub account_id: Option<String>,
    pub token_usage: TokenUsage,
    pub actual_cost_usd_micros: i64,
    pub counterfactual_cost_usd_micros: i64,
    pub price_confidence: f64,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterSavingsSummary {
    pub task_key: Option<String>,
    pub savings: RouterSavings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterTuneRunRecord {
    pub run_id: String,
    pub schema_version: i64,
    pub generated_at_ms: i64,
    pub window_start_ms: Option<i64>,
    pub window_end_ms: i64,
    pub config_fingerprint: String,
    pub evaluated_count: i64,
    pub skipped_count: i64,
    pub cost_budget_usd_micros: i64,
    pub token_budget: i64,
    pub cost_used_usd_micros: i64,
    pub tokens_used: i64,
    pub report_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRouterTuneResultRecord {
    pub run_id: String,
    pub candidate_identity: String,
    pub task_key: String,
    pub status: String,
    pub score: Option<f64>,
    pub confidence: f64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd_micros: i64,
    pub output_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRouterMetricOverlay {
    pub candidate_identity: String,
    pub intelligence_score: Option<f64>,
    pub success_rate: Option<f64>,
    pub median_latency_ms: Option<u64>,
    pub estimated_cost_usd_micros: Option<i64>,
    pub source_report_id: String,
    pub config_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterTuneRollout {
    pub thread_id: String,
    pub rollout_path: PathBuf,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub source: String,
    pub model_provider: String,
    pub model: Option<String>,
    pub archived: bool,
}

impl StateRuntime {
    pub async fn record_model_router_ledger_entry(
        &self,
        entry: ModelRouterLedgerEntry,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO model_router_ledger (
                created_at_ms,
                task_key,
                request_kind,
                model_provider,
                model,
                account_id,
                input_tokens,
                cached_input_tokens,
                output_tokens,
                reasoning_output_tokens,
                total_tokens,
                actual_cost_usd_micros,
                counterfactual_cost_usd_micros,
                price_confidence,
                outcome
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(now_ms)
        .bind(entry.task_key)
        .bind(entry.request_kind.as_str())
        .bind(entry.model_provider)
        .bind(entry.model)
        .bind(entry.account_id)
        .bind(entry.token_usage.input_tokens)
        .bind(entry.token_usage.cached_input_tokens)
        .bind(entry.token_usage.output_tokens)
        .bind(entry.token_usage.reasoning_output_tokens)
        .bind(entry.token_usage.total_tokens)
        .bind(entry.actual_cost_usd_micros)
        .bind(entry.counterfactual_cost_usd_micros)
        .bind(entry.price_confidence)
        .bind(entry.outcome)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn model_router_savings_summary(
        &self,
        task_key: Option<&str>,
    ) -> anyhow::Result<ModelRouterSavingsSummary> {
        let row = if let Some(task_key) = task_key {
            sqlx::query(
                r#"
                SELECT
                    COALESCE(SUM(CASE WHEN request_kind = 'production' THEN actual_cost_usd_micros ELSE 0 END), 0) AS actual_production_cost_usd_micros,
                    COALESCE(SUM(CASE WHEN request_kind != 'production' THEN actual_cost_usd_micros ELSE 0 END), 0) AS router_overhead_cost_usd_micros,
                    COALESCE(SUM(CASE WHEN request_kind = 'production' THEN counterfactual_cost_usd_micros ELSE 0 END), 0) AS counterfactual_cost_usd_micros
                FROM model_router_ledger
                WHERE task_key = ?
                "#,
            )
            .bind(task_key)
            .fetch_one(self.pool.as_ref())
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT
                    COALESCE(SUM(CASE WHEN request_kind = 'production' THEN actual_cost_usd_micros ELSE 0 END), 0) AS actual_production_cost_usd_micros,
                    COALESCE(SUM(CASE WHEN request_kind != 'production' THEN actual_cost_usd_micros ELSE 0 END), 0) AS router_overhead_cost_usd_micros,
                    COALESCE(SUM(CASE WHEN request_kind = 'production' THEN counterfactual_cost_usd_micros ELSE 0 END), 0) AS counterfactual_cost_usd_micros
                FROM model_router_ledger
                "#,
            )
            .fetch_one(self.pool.as_ref())
            .await?
        };

        Ok(ModelRouterSavingsSummary {
            task_key: task_key.map(str::to_string),
            savings: summarize_savings(
                row.try_get("actual_production_cost_usd_micros")?,
                row.try_get("router_overhead_cost_usd_micros")?,
                row.try_get("counterfactual_cost_usd_micros")?,
            ),
        })
    }

    pub async fn record_model_router_tune_run(
        &self,
        record: ModelRouterTuneRunRecord,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO model_router_tune_runs (
                run_id,
                schema_version,
                generated_at_ms,
                window_start_ms,
                window_end_ms,
                config_fingerprint,
                evaluated_count,
                skipped_count,
                cost_budget_usd_micros,
                token_budget,
                cost_used_usd_micros,
                tokens_used,
                report_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(run_id) DO UPDATE SET
                schema_version = excluded.schema_version,
                generated_at_ms = excluded.generated_at_ms,
                window_start_ms = excluded.window_start_ms,
                window_end_ms = excluded.window_end_ms,
                config_fingerprint = excluded.config_fingerprint,
                evaluated_count = excluded.evaluated_count,
                skipped_count = excluded.skipped_count,
                cost_budget_usd_micros = excluded.cost_budget_usd_micros,
                token_budget = excluded.token_budget,
                cost_used_usd_micros = excluded.cost_used_usd_micros,
                tokens_used = excluded.tokens_used,
                report_json = excluded.report_json
            "#,
        )
        .bind(record.run_id)
        .bind(record.schema_version)
        .bind(record.generated_at_ms)
        .bind(record.window_start_ms)
        .bind(record.window_end_ms)
        .bind(record.config_fingerprint)
        .bind(record.evaluated_count)
        .bind(record.skipped_count)
        .bind(record.cost_budget_usd_micros)
        .bind(record.token_budget)
        .bind(record.cost_used_usd_micros)
        .bind(record.tokens_used)
        .bind(record.report_json)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn record_model_router_tune_result(
        &self,
        record: ModelRouterTuneResultRecord,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO model_router_tune_results (
                run_id,
                candidate_identity,
                task_key,
                status,
                score,
                confidence,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cost_usd_micros,
                output_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(record.run_id)
        .bind(record.candidate_identity)
        .bind(record.task_key)
        .bind(record.status)
        .bind(record.score)
        .bind(record.confidence)
        .bind(record.prompt_tokens)
        .bind(record.completion_tokens)
        .bind(record.total_tokens)
        .bind(record.cost_usd_micros)
        .bind(record.output_json)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn lookup_model_router_metric_overlay(
        &self,
        candidate_identity: &str,
    ) -> anyhow::Result<Option<ModelRouterMetricOverlay>> {
        let row = sqlx::query(
            r#"
            SELECT
                candidate_identity,
                intelligence_score,
                success_rate,
                median_latency_ms,
                estimated_cost_usd_micros,
                source_report_id,
                config_fingerprint
            FROM model_router_metric_overlays
            WHERE candidate_identity = ?
            "#,
        )
        .bind(candidate_identity)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(model_router_metric_overlay_from_row).transpose()
    }

    pub async fn upsert_model_router_metric_overlay(
        &self,
        overlay: ModelRouterMetricOverlay,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        let median_latency_ms = overlay.median_latency_ms.map(i64::try_from).transpose()?;
        sqlx::query(
            r#"
            INSERT INTO model_router_metric_overlays (
                candidate_identity,
                created_at_ms,
                updated_at_ms,
                intelligence_score,
                success_rate,
                median_latency_ms,
                estimated_cost_usd_micros,
                source_report_id,
                config_fingerprint
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(candidate_identity) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                intelligence_score = excluded.intelligence_score,
                success_rate = excluded.success_rate,
                median_latency_ms = excluded.median_latency_ms,
                estimated_cost_usd_micros = excluded.estimated_cost_usd_micros,
                source_report_id = excluded.source_report_id,
                config_fingerprint = excluded.config_fingerprint
            "#,
        )
        .bind(overlay.candidate_identity)
        .bind(now_ms)
        .bind(now_ms)
        .bind(overlay.intelligence_score)
        .bind(overlay.success_rate)
        .bind(median_latency_ms)
        .bind(overlay.estimated_cost_usd_micros)
        .bind(overlay.source_report_id)
        .bind(overlay.config_fingerprint)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn model_router_tune_rollouts(
        &self,
        window_start_ms: Option<i64>,
    ) -> anyhow::Result<Vec<ModelRouterTuneRollout>> {
        let rows = if let Some(window_start_ms) = window_start_ms {
            sqlx::query(
                r#"
                SELECT
                    id,
                    rollout_path,
                    created_at_ms,
                    updated_at_ms,
                    source,
                    model_provider,
                    model,
                    archived
                FROM threads
                WHERE rollout_path != ''
                  AND updated_at_ms >= ?
                ORDER BY updated_at_ms DESC, id DESC
                "#,
            )
            .bind(window_start_ms)
            .fetch_all(self.pool.as_ref())
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT
                    id,
                    rollout_path,
                    created_at_ms,
                    updated_at_ms,
                    source,
                    model_provider,
                    model,
                    archived
                FROM threads
                WHERE rollout_path != ''
                ORDER BY updated_at_ms DESC, id DESC
                "#,
            )
            .fetch_all(self.pool.as_ref())
            .await?
        };

        rows.into_iter()
            .map(|row| {
                Ok(ModelRouterTuneRollout {
                    thread_id: row.try_get("id")?,
                    rollout_path: PathBuf::from(row.try_get::<String, _>("rollout_path")?),
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                    source: row.try_get("source")?,
                    model_provider: row.try_get("model_provider")?,
                    model: row.try_get("model")?,
                    archived: row.try_get::<bool, _>("archived")?,
                })
            })
            .collect()
    }
}

fn model_router_metric_overlay_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterMetricOverlay> {
    let median_latency_ms = row
        .try_get::<Option<i64>, _>("median_latency_ms")?
        .map(u64::try_from)
        .transpose()?;
    Ok(ModelRouterMetricOverlay {
        candidate_identity: row.try_get("candidate_identity")?,
        intelligence_score: row.try_get("intelligence_score")?,
        success_rate: row.try_get("success_rate")?,
        median_latency_ms,
        estimated_cost_usd_micros: row.try_get("estimated_cost_usd_micros")?,
        source_report_id: row.try_get("source_report_id")?,
        config_fingerprint: row.try_get("config_fingerprint")?,
    })
}

#[cfg(test)]
mod tests {
    use codex_model_router::RouterSavings;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn savings_summary_subtracts_router_overhead() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .record_model_router_ledger_entry(ModelRouterLedgerEntry {
                task_key: "module.repo_ci.review".to_string(),
                request_kind: RouterRequestKind::Production,
                model_provider: Some("openai".to_string()),
                model: Some("routed-model".to_string()),
                account_id: None,
                token_usage: TokenUsage {
                    input_tokens: 100,
                    cached_input_tokens: 20,
                    output_tokens: 30,
                    reasoning_output_tokens: 0,
                    total_tokens: 130,
                },
                actual_cost_usd_micros: 100,
                counterfactual_cost_usd_micros: 175,
                price_confidence: 0.8,
                outcome: Some("ok".to_string()),
            })
            .await
            .expect("record production");
        runtime
            .record_model_router_ledger_entry(ModelRouterLedgerEntry {
                task_key: "module.repo_ci.review".to_string(),
                request_kind: RouterRequestKind::Judge,
                model_provider: Some("openai".to_string()),
                model: Some("judge-model".to_string()),
                account_id: None,
                token_usage: TokenUsage::default(),
                actual_cost_usd_micros: 50,
                counterfactual_cost_usd_micros: 0,
                price_confidence: 0.8,
                outcome: Some("ok".to_string()),
            })
            .await
            .expect("record overhead");

        assert_eq!(
            runtime
                .model_router_savings_summary(Some("module.repo_ci.review"))
                .await
                .expect("summary"),
            ModelRouterSavingsSummary {
                task_key: Some("module.repo_ci.review".to_string()),
                savings: RouterSavings {
                    actual_production_cost_usd_micros: 100,
                    router_overhead_cost_usd_micros: 50,
                    counterfactual_cost_usd_micros: 175,
                    gross_savings_usd_micros: 75,
                    net_savings_usd_micros: 25,
                },
            }
        );
    }

    #[tokio::test]
    async fn persists_tune_runs_results_and_metric_overlays() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .record_model_router_tune_run(ModelRouterTuneRunRecord {
                run_id: "run-1".to_string(),
                schema_version: 1,
                generated_at_ms: 10,
                window_start_ms: Some(1),
                window_end_ms: 10,
                config_fingerprint: "fingerprint".to_string(),
                evaluated_count: 2,
                skipped_count: 1,
                cost_budget_usd_micros: 100,
                token_budget: 1_000,
                cost_used_usd_micros: 25,
                tokens_used: 300,
                report_json: Some("{}".to_string()),
            })
            .await
            .expect("record run");
        runtime
            .record_model_router_tune_result(ModelRouterTuneResultRecord {
                run_id: "run-1".to_string(),
                candidate_identity: "candidate".to_string(),
                task_key: "module.test".to_string(),
                status: "passing".to_string(),
                score: Some(0.9),
                confidence: 0.8,
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                cost_usd_micros: 5,
                output_json: None,
            })
            .await
            .expect("record result");
        runtime
            .upsert_model_router_metric_overlay(ModelRouterMetricOverlay {
                candidate_identity: "candidate".to_string(),
                intelligence_score: Some(0.9),
                success_rate: Some(0.95),
                median_latency_ms: Some(1_500),
                estimated_cost_usd_micros: Some(42),
                source_report_id: "run-1".to_string(),
                config_fingerprint: "fingerprint".to_string(),
            })
            .await
            .expect("upsert overlay");

        assert_eq!(
            runtime
                .lookup_model_router_metric_overlay("candidate")
                .await
                .expect("lookup overlay"),
            Some(ModelRouterMetricOverlay {
                candidate_identity: "candidate".to_string(),
                intelligence_score: Some(0.9),
                success_rate: Some(0.95),
                median_latency_ms: Some(1_500),
                estimated_cost_usd_micros: Some(42),
                source_report_id: "run-1".to_string(),
                config_fingerprint: "fingerprint".to_string(),
            })
        );
    }
}
