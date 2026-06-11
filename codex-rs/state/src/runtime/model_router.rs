use crate::runtime::StateRuntime;
use chrono::Utc;
use codex_model_router::RouterRequestKind;
use codex_model_router::RouterSavings;
use codex_model_router::summarize_savings;
use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;
use serde::Serialize;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use std::path::PathBuf;

mod lifecycle;

pub use lifecycle::MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED;
pub use lifecycle::MODEL_ROUTER_LIFECYCLE_EVENT_EVALUATING;
pub use lifecycle::MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED;
pub use lifecycle::MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED;
pub use lifecycle::MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED;
pub use lifecycle::MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO;
pub use lifecycle::MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL;
pub use lifecycle::ModelRouterLifecycleCandidateStats;
pub use lifecycle::ModelRouterLifecycleEventCounts;
pub use lifecycle::ModelRouterLifecycleEventRecord;
pub use lifecycle::ModelRouterLifecycleStatsQuery;
pub use lifecycle::ModelRouterLifecycleStatsSummary;
pub use lifecycle::ModelRouterLifecycleTransitionContext;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterUsageGroupBy {
    Task,
    Model,
    Day,
    RequestKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterUsageQuery {
    pub window_start_ms: Option<i64>,
    pub window_end_ms: i64,
    pub task_key: Option<String>,
    pub group_by: ModelRouterUsageGroupBy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterUsageSummary {
    pub window_start_ms: Option<i64>,
    pub window_end_ms: i64,
    pub task_key: Option<String>,
    pub group_by: ModelRouterUsageGroupBy,
    pub totals: ModelRouterUsageTotals,
    pub groups: Vec<ModelRouterUsageGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterUsageTotals {
    pub request_count: i64,
    pub production_request_count: i64,
    pub overhead_request_count: i64,
    pub token_usage: TokenUsage,
    pub savings: RouterSavings,
    pub average_price_confidence: f64,
    pub minimum_price_confidence: f64,
    pub coverage: ModelRouterUsageCoverage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterUsageGroup {
    pub key: String,
    pub totals: ModelRouterUsageTotals,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterUsageCoverage {
    pub missing_price_rows: i64,
    pub low_confidence_price_rows: i64,
    pub zero_token_rows: i64,
    pub production_rows_missing_actual_cost: i64,
    pub production_rows_missing_counterfactual: i64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterLifecyclePromotionRecord {
    pub task_key: String,
    pub candidate_identity: String,
    pub base_candidate_identity: String,
    pub status: String,
    pub rule_id: Option<String>,
    pub production_model_provider: Option<String>,
    pub production_model: Option<String>,
    pub base_model_provider: Option<String>,
    pub base_model: Option<String>,
    pub promoted_at_ms: i64,
    pub updated_at_ms: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterShadowEvaluationRecord {
    pub id: Option<i64>,
    pub created_at_ms: i64,
    pub task_key: String,
    pub phase: String,
    pub candidate_identity: String,
    pub base_candidate_identity: String,
    pub success: bool,
    pub score: Option<f64>,
    pub confidence: f64,
    pub cost_usd_micros: i64,
    pub total_tokens: i64,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterShadowEvaluationSummary {
    pub task_key: String,
    pub phase: String,
    pub candidate_identity: String,
    pub base_candidate_identity: String,
    pub evaluated_count: i64,
    pub success_count: i64,
    pub success_rate: f64,
    pub average_score: Option<f64>,
    pub average_confidence: f64,
    pub cost_used_usd_micros: i64,
    pub tokens_used: i64,
    pub latest_evaluation_id: Option<i64>,
    pub latest_evaluation_at_ms: Option<i64>,
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

    pub async fn model_router_route_max_observed_total_tokens(
        &self,
        task_key: &str,
    ) -> anyhow::Result<Option<i64>> {
        let row = sqlx::query(
            r#"
            SELECT MAX(total_tokens) AS max_total_tokens
            FROM model_router_ledger
            WHERE task_key = ?
              AND request_kind IN ('production', 'shadow')
            "#,
        )
        .bind(task_key)
        .fetch_one(self.pool.as_ref())
        .await?;
        Ok(row.try_get("max_total_tokens")?)
    }

    pub async fn model_router_usage_summary(
        &self,
        query: ModelRouterUsageQuery,
    ) -> anyhow::Result<ModelRouterUsageSummary> {
        let group_expression = match query.group_by {
            ModelRouterUsageGroupBy::Task => "task_key",
            ModelRouterUsageGroupBy::Model => {
                "COALESCE(model_provider, '<unknown>') || '/' || COALESCE(model, '<inherit>')"
            }
            ModelRouterUsageGroupBy::Day => "date(created_at_ms / 1000, 'unixepoch')",
            ModelRouterUsageGroupBy::RequestKind => "request_kind",
        };
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT ");
        builder.push(group_expression);
        builder.push(
            r#" AS group_key,
                COUNT(*) AS request_count,
                COALESCE(SUM(CASE WHEN request_kind = 'production' THEN 1 ELSE 0 END), 0) AS production_request_count,
                COALESCE(SUM(CASE WHEN request_kind != 'production' THEN 1 ELSE 0 END), 0) AS overhead_request_count,
                COALESCE(SUM(input_tokens), 0) AS input_tokens,
                COALESCE(SUM(cached_input_tokens), 0) AS cached_input_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(reasoning_output_tokens), 0) AS reasoning_output_tokens,
                COALESCE(SUM(total_tokens), 0) AS total_tokens,
                COALESCE(SUM(CASE WHEN request_kind = 'production' THEN actual_cost_usd_micros ELSE 0 END), 0) AS actual_production_cost_usd_micros,
                COALESCE(SUM(CASE WHEN request_kind != 'production' THEN actual_cost_usd_micros ELSE 0 END), 0) AS router_overhead_cost_usd_micros,
                COALESCE(SUM(CASE WHEN request_kind = 'production' THEN counterfactual_cost_usd_micros ELSE 0 END), 0) AS counterfactual_cost_usd_micros,
                COALESCE(AVG(price_confidence), 0.0) AS average_price_confidence,
                COALESCE(MIN(price_confidence), 0.0) AS minimum_price_confidence,
                COALESCE(SUM(CASE WHEN price_confidence <= 0.0 THEN 1 ELSE 0 END), 0) AS missing_price_rows,
                COALESCE(SUM(CASE WHEN price_confidence > 0.0 AND price_confidence < 0.75 THEN 1 ELSE 0 END), 0) AS low_confidence_price_rows,
                COALESCE(SUM(CASE WHEN total_tokens = 0 THEN 1 ELSE 0 END), 0) AS zero_token_rows,
                COALESCE(SUM(CASE WHEN request_kind = 'production' AND total_tokens > 0 AND actual_cost_usd_micros = 0 THEN 1 ELSE 0 END), 0) AS production_rows_missing_actual_cost,
                COALESCE(SUM(CASE WHEN request_kind = 'production' AND total_tokens > 0 AND counterfactual_cost_usd_micros = 0 THEN 1 ELSE 0 END), 0) AS production_rows_missing_counterfactual
            FROM model_router_ledger
            WHERE created_at_ms <= "#,
        );
        builder.push_bind(query.window_end_ms);
        if let Some(window_start_ms) = query.window_start_ms {
            builder.push(" AND created_at_ms >= ");
            builder.push_bind(window_start_ms);
        }
        if let Some(task_key) = query.task_key.as_deref() {
            builder.push(" AND task_key = ");
            builder.push_bind(task_key);
        }
        builder.push(" GROUP BY group_key ORDER BY group_key ASC");

        let rows = builder.build().fetch_all(self.pool.as_ref()).await?;
        let groups = rows
            .into_iter()
            .map(model_router_usage_group_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let totals = model_router_usage_totals_from_groups(&groups);
        Ok(ModelRouterUsageSummary {
            window_start_ms: query.window_start_ms,
            window_end_ms: query.window_end_ms,
            task_key: query.task_key,
            group_by: query.group_by,
            totals,
            groups,
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

    pub async fn upsert_model_router_lifecycle_promotion(
        &self,
        record: ModelRouterLifecyclePromotionRecord,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO model_router_lifecycle_promotions (
                task_key,
                candidate_identity,
                base_candidate_identity,
                status,
                rule_id,
                production_model_provider,
                production_model,
                base_model_provider,
                base_model,
                promoted_at_ms,
                updated_at_ms,
                reason
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(task_key, candidate_identity) DO UPDATE SET
                base_candidate_identity = excluded.base_candidate_identity,
                status = excluded.status,
                rule_id = excluded.rule_id,
                production_model_provider = excluded.production_model_provider,
                production_model = excluded.production_model,
                base_model_provider = excluded.base_model_provider,
                base_model = excluded.base_model,
                promoted_at_ms = excluded.promoted_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                reason = excluded.reason
            "#,
        )
        .bind(record.task_key)
        .bind(record.candidate_identity)
        .bind(record.base_candidate_identity)
        .bind(record.status)
        .bind(record.rule_id)
        .bind(record.production_model_provider)
        .bind(record.production_model)
        .bind(record.base_model_provider)
        .bind(record.base_model)
        .bind(record.promoted_at_ms)
        .bind(record.updated_at_ms)
        .bind(record.reason)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn model_router_lifecycle_promotions(
        &self,
        task_key: Option<&str>,
    ) -> anyhow::Result<Vec<ModelRouterLifecyclePromotionRecord>> {
        let rows = if let Some(task_key) = task_key {
            sqlx::query(
                r#"
                SELECT
                    task_key,
                    candidate_identity,
                    base_candidate_identity,
                    status,
                    rule_id,
                    production_model_provider,
                    production_model,
                    base_model_provider,
                    base_model,
                    promoted_at_ms,
                    updated_at_ms,
                    reason
                FROM model_router_lifecycle_promotions
                WHERE task_key = ?
                ORDER BY updated_at_ms DESC, candidate_identity ASC
                "#,
            )
            .bind(task_key)
            .fetch_all(self.pool.as_ref())
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT
                    task_key,
                    candidate_identity,
                    base_candidate_identity,
                    status,
                    rule_id,
                    production_model_provider,
                    production_model,
                    base_model_provider,
                    base_model,
                    promoted_at_ms,
                    updated_at_ms,
                    reason
                FROM model_router_lifecycle_promotions
                ORDER BY updated_at_ms DESC, task_key ASC, candidate_identity ASC
                "#,
            )
            .fetch_all(self.pool.as_ref())
            .await?
        };

        rows.into_iter()
            .map(model_router_lifecycle_promotion_from_row)
            .collect()
    }

    pub async fn demote_model_router_lifecycle_promotion(
        &self,
        task_key: &str,
        candidate_identity: &str,
        reason: Option<&str>,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE model_router_lifecycle_promotions
            SET status = 'demoted', updated_at_ms = ?, reason = ?
            WHERE task_key = ? AND candidate_identity = ?
            "#,
        )
        .bind(Utc::now().timestamp_millis())
        .bind(reason)
        .bind(task_key)
        .bind(candidate_identity)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn record_model_router_shadow_evaluation(
        &self,
        record: ModelRouterShadowEvaluationRecord,
    ) -> anyhow::Result<i64> {
        let result = sqlx::query(
            r#"
            INSERT INTO model_router_shadow_evaluations (
                created_at_ms,
                task_key,
                phase,
                candidate_identity,
                base_candidate_identity,
                success,
                score,
                confidence,
                cost_usd_micros,
                total_tokens,
                outcome,
                metadata_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(record.created_at_ms)
        .bind(record.task_key)
        .bind(record.phase)
        .bind(record.candidate_identity)
        .bind(record.base_candidate_identity)
        .bind(if record.success { 1_i64 } else { 0_i64 })
        .bind(record.score)
        .bind(record.confidence)
        .bind(record.cost_usd_micros)
        .bind(record.total_tokens)
        .bind(record.outcome)
        .bind(record.metadata_json)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn model_router_shadow_evaluations(
        &self,
        task_key: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ModelRouterShadowEvaluationRecord>> {
        let limit = limit.clamp(1, 1_000);
        let rows = if let Some(task_key) = task_key {
            sqlx::query(
                r#"
                SELECT
                    id,
                    created_at_ms,
                    task_key,
                    phase,
                    candidate_identity,
                    base_candidate_identity,
                    success,
                    score,
                    confidence,
                    cost_usd_micros,
                    total_tokens,
                    outcome,
                    metadata_json
                FROM model_router_shadow_evaluations
                WHERE task_key = ?
                ORDER BY created_at_ms DESC, id DESC
                LIMIT ?
                "#,
            )
            .bind(task_key)
            .bind(limit)
            .fetch_all(self.pool.as_ref())
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT
                    id,
                    created_at_ms,
                    task_key,
                    phase,
                    candidate_identity,
                    base_candidate_identity,
                    success,
                    score,
                    confidence,
                    cost_usd_micros,
                    total_tokens,
                    outcome,
                    metadata_json
                FROM model_router_shadow_evaluations
                ORDER BY created_at_ms DESC, id DESC
                LIMIT ?
                "#,
            )
            .bind(limit)
            .fetch_all(self.pool.as_ref())
            .await?
        };

        rows.into_iter()
            .map(model_router_shadow_evaluation_from_row)
            .collect()
    }

    pub async fn model_router_shadow_evaluation_summaries(
        &self,
        task_key: Option<&str>,
    ) -> anyhow::Result<Vec<ModelRouterShadowEvaluationSummary>> {
        self.model_router_shadow_evaluation_summaries_since(task_key, /*created_at_ms*/ None)
            .await
    }

    pub async fn model_router_shadow_evaluation_summaries_since(
        &self,
        task_key: Option<&str>,
        created_at_ms: Option<i64>,
    ) -> anyhow::Result<Vec<ModelRouterShadowEvaluationSummary>> {
        let rows = if let Some(task_key) = task_key {
            if let Some(created_at_ms) = created_at_ms {
                sqlx::query(
                    r#"
                    SELECT
                        task_key,
                        phase,
                        candidate_identity,
                        base_candidate_identity,
                        COUNT(*) AS evaluated_count,
                        COALESCE(SUM(success), 0) AS success_count,
                        AVG(score) AS average_score,
                        COALESCE(AVG(confidence), 0.0) AS average_confidence,
                        COALESCE(SUM(cost_usd_micros), 0) AS cost_used_usd_micros,
                        COALESCE(SUM(total_tokens), 0) AS tokens_used,
                        MAX(id) AS latest_evaluation_id,
                        MAX(created_at_ms) AS latest_evaluation_at_ms
                    FROM model_router_shadow_evaluations
                    WHERE task_key = ? AND created_at_ms >= ?
                    GROUP BY task_key, phase, candidate_identity, base_candidate_identity
                    ORDER BY task_key ASC, phase ASC, candidate_identity ASC, base_candidate_identity ASC
                    "#,
                )
                .bind(task_key)
                .bind(created_at_ms)
                .fetch_all(self.pool.as_ref())
                .await?
            } else {
                sqlx::query(
                    r#"
                    SELECT
                        task_key,
                        phase,
                        candidate_identity,
                        base_candidate_identity,
                        COUNT(*) AS evaluated_count,
                        COALESCE(SUM(success), 0) AS success_count,
                        AVG(score) AS average_score,
                        COALESCE(AVG(confidence), 0.0) AS average_confidence,
                        COALESCE(SUM(cost_usd_micros), 0) AS cost_used_usd_micros,
                        COALESCE(SUM(total_tokens), 0) AS tokens_used,
                        MAX(id) AS latest_evaluation_id,
                        MAX(created_at_ms) AS latest_evaluation_at_ms
                    FROM model_router_shadow_evaluations
                    WHERE task_key = ?
                    GROUP BY task_key, phase, candidate_identity, base_candidate_identity
                    ORDER BY task_key ASC, phase ASC, candidate_identity ASC, base_candidate_identity ASC
                    "#,
                )
                .bind(task_key)
                .fetch_all(self.pool.as_ref())
                .await?
            }
        } else if let Some(created_at_ms) = created_at_ms {
            sqlx::query(
                r#"
                    SELECT
                        task_key,
                        phase,
                        candidate_identity,
                        base_candidate_identity,
                        COUNT(*) AS evaluated_count,
                        COALESCE(SUM(success), 0) AS success_count,
                        AVG(score) AS average_score,
                        COALESCE(AVG(confidence), 0.0) AS average_confidence,
                        COALESCE(SUM(cost_usd_micros), 0) AS cost_used_usd_micros,
                        COALESCE(SUM(total_tokens), 0) AS tokens_used,
                        MAX(id) AS latest_evaluation_id,
                        MAX(created_at_ms) AS latest_evaluation_at_ms
                    FROM model_router_shadow_evaluations
                    WHERE created_at_ms >= ?
                    GROUP BY task_key, phase, candidate_identity, base_candidate_identity
                    ORDER BY task_key ASC, phase ASC, candidate_identity ASC, base_candidate_identity ASC
                    "#,
            )
            .bind(created_at_ms)
            .fetch_all(self.pool.as_ref())
            .await?
        } else {
            sqlx::query(
                r#"
                    SELECT
                        task_key,
                        phase,
                        candidate_identity,
                        base_candidate_identity,
                        COUNT(*) AS evaluated_count,
                        COALESCE(SUM(success), 0) AS success_count,
                        AVG(score) AS average_score,
                        COALESCE(AVG(confidence), 0.0) AS average_confidence,
                        COALESCE(SUM(cost_usd_micros), 0) AS cost_used_usd_micros,
                        COALESCE(SUM(total_tokens), 0) AS tokens_used,
                        MAX(id) AS latest_evaluation_id,
                        MAX(created_at_ms) AS latest_evaluation_at_ms
                    FROM model_router_shadow_evaluations
                    GROUP BY task_key, phase, candidate_identity, base_candidate_identity
                    ORDER BY task_key ASC, phase ASC, candidate_identity ASC, base_candidate_identity ASC
                    "#,
            )
            .fetch_all(self.pool.as_ref())
            .await?
        };

        rows.into_iter()
            .map(model_router_shadow_evaluation_summary_from_row)
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

fn model_router_lifecycle_promotion_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterLifecyclePromotionRecord> {
    Ok(ModelRouterLifecyclePromotionRecord {
        task_key: row.try_get("task_key")?,
        candidate_identity: row.try_get("candidate_identity")?,
        base_candidate_identity: row.try_get("base_candidate_identity")?,
        status: row.try_get("status")?,
        rule_id: row.try_get("rule_id")?,
        production_model_provider: row.try_get("production_model_provider")?,
        production_model: row.try_get("production_model")?,
        base_model_provider: row.try_get("base_model_provider")?,
        base_model: row.try_get("base_model")?,
        promoted_at_ms: row.try_get("promoted_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        reason: row.try_get("reason")?,
    })
}

fn model_router_shadow_evaluation_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterShadowEvaluationRecord> {
    Ok(ModelRouterShadowEvaluationRecord {
        id: row.try_get("id")?,
        created_at_ms: row.try_get("created_at_ms")?,
        task_key: row.try_get("task_key")?,
        phase: row.try_get("phase")?,
        candidate_identity: row.try_get("candidate_identity")?,
        base_candidate_identity: row.try_get("base_candidate_identity")?,
        success: row.try_get::<i64, _>("success")? != 0,
        score: row.try_get("score")?,
        confidence: row.try_get("confidence")?,
        cost_usd_micros: row.try_get("cost_usd_micros")?,
        total_tokens: row.try_get("total_tokens")?,
        outcome: row.try_get("outcome")?,
        metadata_json: row.try_get("metadata_json")?,
    })
}

fn model_router_shadow_evaluation_summary_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterShadowEvaluationSummary> {
    let evaluated_count = row.try_get::<i64, _>("evaluated_count")?;
    let success_count = row.try_get::<i64, _>("success_count")?;
    let success_rate = if evaluated_count > 0 {
        success_count as f64 / evaluated_count as f64
    } else {
        0.0
    };
    Ok(ModelRouterShadowEvaluationSummary {
        task_key: row.try_get("task_key")?,
        phase: row.try_get("phase")?,
        candidate_identity: row.try_get("candidate_identity")?,
        base_candidate_identity: row.try_get("base_candidate_identity")?,
        evaluated_count,
        success_count,
        success_rate,
        average_score: row.try_get("average_score")?,
        average_confidence: row.try_get("average_confidence")?,
        cost_used_usd_micros: row.try_get("cost_used_usd_micros")?,
        tokens_used: row.try_get("tokens_used")?,
        latest_evaluation_id: row.try_get("latest_evaluation_id")?,
        latest_evaluation_at_ms: row.try_get("latest_evaluation_at_ms")?,
    })
}

fn model_router_usage_group_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterUsageGroup> {
    Ok(ModelRouterUsageGroup {
        key: row.try_get("group_key")?,
        totals: model_router_usage_totals_from_row(&row)?,
    })
}

fn model_router_usage_totals_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterUsageTotals> {
    let actual_production_cost_usd_micros = row.try_get("actual_production_cost_usd_micros")?;
    let router_overhead_cost_usd_micros = row.try_get("router_overhead_cost_usd_micros")?;
    let counterfactual_cost_usd_micros = row.try_get("counterfactual_cost_usd_micros")?;
    Ok(ModelRouterUsageTotals {
        request_count: row.try_get("request_count")?,
        production_request_count: row.try_get("production_request_count")?,
        overhead_request_count: row.try_get("overhead_request_count")?,
        token_usage: TokenUsage {
            input_tokens: row.try_get("input_tokens")?,
            cached_input_tokens: row.try_get("cached_input_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            reasoning_output_tokens: row.try_get("reasoning_output_tokens")?,
            total_tokens: row.try_get("total_tokens")?,
        },
        savings: summarize_savings(
            actual_production_cost_usd_micros,
            router_overhead_cost_usd_micros,
            counterfactual_cost_usd_micros,
        ),
        average_price_confidence: row.try_get("average_price_confidence")?,
        minimum_price_confidence: row.try_get("minimum_price_confidence")?,
        coverage: ModelRouterUsageCoverage {
            missing_price_rows: row.try_get("missing_price_rows")?,
            low_confidence_price_rows: row.try_get("low_confidence_price_rows")?,
            zero_token_rows: row.try_get("zero_token_rows")?,
            production_rows_missing_actual_cost: row
                .try_get("production_rows_missing_actual_cost")?,
            production_rows_missing_counterfactual: row
                .try_get("production_rows_missing_counterfactual")?,
        },
    })
}

fn model_router_usage_totals_from_groups(
    groups: &[ModelRouterUsageGroup],
) -> ModelRouterUsageTotals {
    let mut token_usage = TokenUsage::default();
    let mut request_count = 0_i64;
    let mut production_request_count = 0_i64;
    let mut overhead_request_count = 0_i64;
    let mut actual_production_cost_usd_micros = 0_i64;
    let mut router_overhead_cost_usd_micros = 0_i64;
    let mut counterfactual_cost_usd_micros = 0_i64;
    let mut weighted_price_confidence = 0.0;
    let mut minimum_price_confidence: Option<f64> = None;
    let mut coverage = ModelRouterUsageCoverage::default();

    for group in groups {
        let totals = &group.totals;
        request_count = request_count.saturating_add(totals.request_count);
        production_request_count =
            production_request_count.saturating_add(totals.production_request_count);
        overhead_request_count =
            overhead_request_count.saturating_add(totals.overhead_request_count);
        token_usage.add_assign(&totals.token_usage);
        actual_production_cost_usd_micros = actual_production_cost_usd_micros
            .saturating_add(totals.savings.actual_production_cost_usd_micros);
        router_overhead_cost_usd_micros = router_overhead_cost_usd_micros
            .saturating_add(totals.savings.router_overhead_cost_usd_micros);
        counterfactual_cost_usd_micros = counterfactual_cost_usd_micros
            .saturating_add(totals.savings.counterfactual_cost_usd_micros);
        weighted_price_confidence +=
            totals.average_price_confidence * totals.request_count.max(0) as f64;
        minimum_price_confidence = Some(
            minimum_price_confidence
                .map(|current| current.min(totals.minimum_price_confidence))
                .unwrap_or(totals.minimum_price_confidence),
        );
        coverage.missing_price_rows = coverage
            .missing_price_rows
            .saturating_add(totals.coverage.missing_price_rows);
        coverage.low_confidence_price_rows = coverage
            .low_confidence_price_rows
            .saturating_add(totals.coverage.low_confidence_price_rows);
        coverage.zero_token_rows = coverage
            .zero_token_rows
            .saturating_add(totals.coverage.zero_token_rows);
        coverage.production_rows_missing_actual_cost = coverage
            .production_rows_missing_actual_cost
            .saturating_add(totals.coverage.production_rows_missing_actual_cost);
        coverage.production_rows_missing_counterfactual = coverage
            .production_rows_missing_counterfactual
            .saturating_add(totals.coverage.production_rows_missing_counterfactual);
    }

    ModelRouterUsageTotals {
        request_count,
        production_request_count,
        overhead_request_count,
        token_usage,
        savings: summarize_savings(
            actual_production_cost_usd_micros,
            router_overhead_cost_usd_micros,
            counterfactual_cost_usd_micros,
        ),
        average_price_confidence: if request_count > 0 {
            weighted_price_confidence / request_count as f64
        } else {
            0.0
        },
        minimum_price_confidence: minimum_price_confidence.unwrap_or(0.0),
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use codex_model_router::RouterSavings;
    use pretty_assertions::assert_eq;
    use sqlx::Row;
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
    async fn route_max_observed_total_tokens_uses_production_and_shadow_only() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        for (task_key, request_kind, total_tokens) in [
            ("module.repo_ci.review", RouterRequestKind::Production, 100),
            ("module.repo_ci.review", RouterRequestKind::Shadow, 250),
            ("module.repo_ci.review", RouterRequestKind::Judge, 1_000),
            ("module.other", RouterRequestKind::Production, 900),
        ] {
            runtime
                .record_model_router_ledger_entry(ModelRouterLedgerEntry {
                    task_key: task_key.to_string(),
                    request_kind,
                    model_provider: Some("openai".to_string()),
                    model: Some("model".to_string()),
                    account_id: None,
                    token_usage: TokenUsage {
                        input_tokens: total_tokens,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_output_tokens: 0,
                        total_tokens,
                    },
                    actual_cost_usd_micros: 0,
                    counterfactual_cost_usd_micros: 0,
                    price_confidence: 0.0,
                    outcome: None,
                })
                .await
                .expect("record route");
        }

        assert_eq!(
            runtime
                .model_router_route_max_observed_total_tokens("module.repo_ci.review")
                .await
                .expect("max observed tokens"),
            Some(250)
        );
        assert_eq!(
            runtime
                .model_router_route_max_observed_total_tokens("module.missing")
                .await
                .expect("missing max observed tokens"),
            None
        );
    }

    #[tokio::test]
    async fn usage_summary_groups_costs_tokens_and_coverage_gaps() {
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
                price_confidence: 1.0,
                outcome: Some("ok".to_string()),
            })
            .await
            .expect("record production");
        runtime
            .record_model_router_ledger_entry(ModelRouterLedgerEntry {
                task_key: "module.repo_ci.review".to_string(),
                request_kind: RouterRequestKind::Production,
                model_provider: Some("openai".to_string()),
                model: Some("routed-model".to_string()),
                account_id: None,
                token_usage: TokenUsage {
                    input_tokens: 8,
                    cached_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 10,
                },
                actual_cost_usd_micros: 20,
                counterfactual_cost_usd_micros: 0,
                price_confidence: 0.5,
                outcome: Some("missing counterfactual".to_string()),
            })
            .await
            .expect("record low confidence production");
        runtime
            .record_model_router_ledger_entry(ModelRouterLedgerEntry {
                task_key: "module.repo_ci.review".to_string(),
                request_kind: RouterRequestKind::Judge,
                model_provider: Some("openai".to_string()),
                model: Some("judge-model".to_string()),
                account_id: None,
                token_usage: TokenUsage::default(),
                actual_cost_usd_micros: 0,
                counterfactual_cost_usd_micros: 0,
                price_confidence: 0.0,
                outcome: Some("missing price".to_string()),
            })
            .await
            .expect("record overhead");

        let summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms: Utc::now().timestamp_millis(),
                task_key: Some("module.repo_ci.review".to_string()),
                group_by: ModelRouterUsageGroupBy::RequestKind,
            })
            .await
            .expect("usage summary");

        assert_eq!(summary.task_key, Some("module.repo_ci.review".to_string()));
        assert_eq!(summary.group_by, ModelRouterUsageGroupBy::RequestKind);
        assert_eq!(summary.groups.len(), 2);
        assert_eq!(
            summary.totals,
            ModelRouterUsageTotals {
                request_count: 3,
                production_request_count: 2,
                overhead_request_count: 1,
                token_usage: TokenUsage {
                    input_tokens: 108,
                    cached_input_tokens: 20,
                    output_tokens: 32,
                    reasoning_output_tokens: 0,
                    total_tokens: 140,
                },
                savings: RouterSavings {
                    actual_production_cost_usd_micros: 120,
                    router_overhead_cost_usd_micros: 0,
                    counterfactual_cost_usd_micros: 175,
                    gross_savings_usd_micros: 55,
                    net_savings_usd_micros: 55,
                },
                average_price_confidence: 0.5,
                minimum_price_confidence: 0.0,
                coverage: ModelRouterUsageCoverage {
                    missing_price_rows: 1,
                    low_confidence_price_rows: 1,
                    zero_token_rows: 1,
                    production_rows_missing_actual_cost: 0,
                    production_rows_missing_counterfactual: 1,
                },
            }
        );
    }

    #[tokio::test]
    async fn usage_summary_aggregates_by_task_model_day_and_request_kind() {
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
                price_confidence: 1.0,
                outcome: Some("completed".to_string()),
            })
            .await
            .expect("record review production");
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
                price_confidence: 1.0,
                outcome: Some("judge".to_string()),
            })
            .await
            .expect("record review overhead");
        runtime
            .record_model_router_ledger_entry(ModelRouterLedgerEntry {
                task_key: "module.repo_ci.triage".to_string(),
                request_kind: RouterRequestKind::Production,
                model_provider: Some("anthropic".to_string()),
                model: Some("claude".to_string()),
                account_id: None,
                token_usage: TokenUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 5,
                    reasoning_output_tokens: 0,
                    total_tokens: 15,
                },
                actual_cost_usd_micros: 60,
                counterfactual_cost_usd_micros: 90,
                price_confidence: 0.5,
                outcome: Some("completed".to_string()),
            })
            .await
            .expect("record triage production");

        let window_end_ms = Utc::now().timestamp_millis();
        let task_summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms,
                task_key: None,
                group_by: ModelRouterUsageGroupBy::Task,
            })
            .await
            .expect("task summary");
        assert_eq!(
            usage_group_projection(&task_summary),
            vec![
                ("module.repo_ci.review".to_string(), 2, 1, 1, 130, 25),
                ("module.repo_ci.triage".to_string(), 1, 1, 0, 15, 30),
            ]
        );

        let model_summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms,
                task_key: None,
                group_by: ModelRouterUsageGroupBy::Model,
            })
            .await
            .expect("model summary");
        assert_eq!(
            usage_group_projection(&model_summary),
            vec![
                ("anthropic/claude".to_string(), 1, 1, 0, 15, 30),
                ("openai/judge-model".to_string(), 1, 0, 1, 0, -50),
                ("openai/routed-model".to_string(), 1, 1, 0, 130, 75),
            ]
        );

        let request_kind_summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms,
                task_key: None,
                group_by: ModelRouterUsageGroupBy::RequestKind,
            })
            .await
            .expect("request-kind summary");
        assert_eq!(
            usage_group_projection(&request_kind_summary),
            vec![
                ("judge".to_string(), 1, 0, 1, 0, -50),
                ("production".to_string(), 2, 2, 0, 145, 105),
            ]
        );
        assert_eq!(
            request_kind_summary.groups[0]
                .totals
                .average_price_confidence,
            1.0
        );
        assert_eq!(
            request_kind_summary.groups[0]
                .totals
                .coverage
                .missing_price_rows,
            0
        );

        let day_summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms,
                task_key: None,
                group_by: ModelRouterUsageGroupBy::Day,
            })
            .await
            .expect("day summary");
        assert_eq!(
            usage_group_projection(&day_summary),
            vec![(Utc::now().format("%Y-%m-%d").to_string(), 3, 2, 1, 145, 55)]
        );
        assert_eq!(day_summary.totals, task_summary.totals);
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

    #[tokio::test]
    async fn persists_lifecycle_promotions_and_shadow_summaries() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .upsert_model_router_lifecycle_promotion(ModelRouterLifecyclePromotionRecord {
                task_key: "module.repo_ci.review".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                status: "promoted".to_string(),
                rule_id: Some("review".to_string()),
                production_model_provider: Some("openai".to_string()),
                production_model: Some("gpt-5.5".to_string()),
                base_model_provider: Some("openai".to_string()),
                base_model: Some("gpt-5.4".to_string()),
                promoted_at_ms: 10,
                updated_at_ms: 10,
                reason: Some("passed gates".to_string()),
            })
            .await
            .expect("upsert promotion");

        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some("module.repo_ci.review"))
                .await
                .expect("promotions"),
            vec![ModelRouterLifecyclePromotionRecord {
                task_key: "module.repo_ci.review".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                status: "promoted".to_string(),
                rule_id: Some("review".to_string()),
                production_model_provider: Some("openai".to_string()),
                production_model: Some("gpt-5.5".to_string()),
                base_model_provider: Some("openai".to_string()),
                base_model: Some("gpt-5.4".to_string()),
                promoted_at_ms: 10,
                updated_at_ms: 10,
                reason: Some("passed gates".to_string()),
            }]
        );

        runtime
            .record_model_router_shadow_evaluation(ModelRouterShadowEvaluationRecord {
                id: None,
                created_at_ms: 11,
                task_key: "module.repo_ci.review".to_string(),
                phase: "promotion".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                success: true,
                score: Some(1.0),
                confidence: 1.0,
                cost_usd_micros: 100,
                total_tokens: 200,
                outcome: Some("ok".to_string()),
                metadata_json: Some("{}".to_string()),
            })
            .await
            .expect("record shadow");
        runtime
            .record_model_router_shadow_evaluation(ModelRouterShadowEvaluationRecord {
                id: None,
                created_at_ms: 12,
                task_key: "module.repo_ci.review".to_string(),
                phase: "promotion".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                success: false,
                score: Some(0.0),
                confidence: 0.0,
                cost_usd_micros: 50,
                total_tokens: 100,
                outcome: Some("failed".to_string()),
                metadata_json: None,
            })
            .await
            .expect("record shadow");

        assert_eq!(
            runtime
                .model_router_shadow_evaluation_summaries(Some("module.repo_ci.review"))
                .await
                .expect("summaries"),
            vec![ModelRouterShadowEvaluationSummary {
                task_key: "module.repo_ci.review".to_string(),
                phase: "promotion".to_string(),
                candidate_identity: "candidate".to_string(),
                base_candidate_identity: "base".to_string(),
                evaluated_count: 2,
                success_count: 1,
                success_rate: 0.5,
                average_score: Some(0.5),
                average_confidence: 0.5,
                cost_used_usd_micros: 150,
                tokens_used: 300,
                latest_evaluation_id: Some(2),
                latest_evaluation_at_ms: Some(12),
            }]
        );

        let rows = runtime
            .demote_model_router_lifecycle_promotion(
                "module.repo_ci.review",
                "candidate",
                Some("monitoring failed"),
            )
            .await
            .expect("demote");
        assert_eq!(rows, 1);
        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some("module.repo_ci.review"))
                .await
                .expect("promotions")
                .first()
                .map(|record| record.status.as_str()),
            Some("demoted")
        );
    }

    #[tokio::test]
    async fn lifecycle_events_stats_filters_and_deduplicates_blocked_high_water() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        let columns = sqlx::query("PRAGMA table_info(model_router_lifecycle_events)")
            .fetch_all(runtime.pool.as_ref())
            .await
            .expect("table info")
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").expect("column name"))
            .collect::<Vec<_>>();
        assert!(columns.contains(&"event_type".to_string()));
        assert!(columns.contains(&"source".to_string()));
        assert!(columns.contains(&"shadow_latest_evaluation_id".to_string()));
        assert!(columns.contains(&"failed_gates_json".to_string()));

        let task_key = "module.repo_ci.review";
        let candidate_identity = "candidate";
        let base_candidate_identity = "base";
        runtime
            .promote_model_router_lifecycle_promotion(
                ModelRouterLifecyclePromotionRecord {
                    task_key: task_key.to_string(),
                    candidate_identity: candidate_identity.to_string(),
                    base_candidate_identity: base_candidate_identity.to_string(),
                    status: "promoted".to_string(),
                    rule_id: Some("review".to_string()),
                    production_model_provider: Some("openai".to_string()),
                    production_model: Some("gpt-5.5".to_string()),
                    base_model_provider: Some("openai".to_string()),
                    base_model: Some("gpt-5.4".to_string()),
                    promoted_at_ms: 10,
                    updated_at_ms: 10,
                    reason: Some("manual promote".to_string()),
                },
                ModelRouterLifecycleTransitionContext {
                    source: MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL.to_string(),
                    lifecycle_window: Some("all".to_string()),
                    shadow_phase: None,
                    shadow_summary: None,
                    failed_gates_json: None,
                },
            )
            .await
            .expect("manual promote");

        let demoted = runtime
            .demote_model_router_lifecycle_promotion_with_event(
                task_key,
                candidate_identity,
                Some("manual demote"),
                ModelRouterLifecycleTransitionContext {
                    source: MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL.to_string(),
                    lifecycle_window: Some("all".to_string()),
                    shadow_phase: None,
                    shadow_summary: None,
                    failed_gates_json: None,
                },
            )
            .await
            .expect("manual demote");
        assert_eq!(demoted, 1);

        let blocked = ModelRouterLifecycleEventRecord {
            id: None,
            created_at_ms: 30,
            event_type: MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED.to_string(),
            source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
            task_key: task_key.to_string(),
            candidate_identity: candidate_identity.to_string(),
            base_candidate_identity: base_candidate_identity.to_string(),
            previous_status: Some("demoted".to_string()),
            next_status: Some("demoted".to_string()),
            rule_id: Some("review".to_string()),
            reason: Some("promotion shadow gates failed".to_string()),
            production_model_provider: Some("openai".to_string()),
            production_model: Some("gpt-5.5".to_string()),
            base_model_provider: Some("openai".to_string()),
            base_model: Some("gpt-5.4".to_string()),
            lifecycle_window: Some("all".to_string()),
            shadow_phase: Some("promotion".to_string()),
            shadow_evaluated_count: Some(2),
            shadow_success_count: Some(1),
            shadow_success_rate: Some(0.5),
            shadow_average_score: Some(0.5),
            shadow_average_confidence: Some(1.0),
            shadow_cost_used_usd_micros: Some(100),
            shadow_tokens_used: Some(200),
            shadow_latest_evaluation_id: Some(99),
            shadow_latest_evaluation_at_ms: Some(29),
            failed_gates_json: Some(
                r#"[{"gate":"min_success_rate","actual":0.5,"threshold":0.9}]"#.to_string(),
            ),
        };
        assert!(
            runtime
                .record_model_router_lifecycle_event_once(blocked.clone())
                .await
                .expect("blocked event")
        );
        assert!(
            !runtime
                .record_model_router_lifecycle_event_once(blocked)
                .await
                .expect("duplicate blocked event")
        );

        let repromoted_at_ms = Utc::now().timestamp_millis().saturating_add(10_000);
        runtime
            .promote_model_router_lifecycle_promotion(
                ModelRouterLifecyclePromotionRecord {
                    task_key: task_key.to_string(),
                    candidate_identity: candidate_identity.to_string(),
                    base_candidate_identity: base_candidate_identity.to_string(),
                    status: "promoted".to_string(),
                    rule_id: Some("review".to_string()),
                    production_model_provider: Some("openai".to_string()),
                    production_model: Some("gpt-5.5".to_string()),
                    base_model_provider: Some("openai".to_string()),
                    base_model: Some("gpt-5.4".to_string()),
                    promoted_at_ms: repromoted_at_ms,
                    updated_at_ms: repromoted_at_ms,
                    reason: Some("auto repromote".to_string()),
                },
                ModelRouterLifecycleTransitionContext {
                    source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
                    lifecycle_window: Some("all".to_string()),
                    shadow_phase: None,
                    shadow_summary: None,
                    failed_gates_json: None,
                },
            )
            .await
            .expect("auto repromote");

        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some(task_key))
                .await
                .expect("promotions")
                .first()
                .map(|promotion| (promotion.status.as_str(), promotion.promoted_at_ms)),
            Some(("promoted", repromoted_at_ms))
        );

        let all_stats = runtime
            .model_router_lifecycle_stats(ModelRouterLifecycleStatsQuery {
                window_start_ms: None,
                window_end_ms: i64::MAX,
                task_key: Some(task_key.to_string()),
                candidate_identity: Some(candidate_identity.to_string()),
                event_limit: 50,
            })
            .await
            .expect("all stats");
        let expected_counts = ModelRouterLifecycleEventCounts {
            promoted: 2,
            demoted: 1,
            evaluating: 0,
            promotion_blocked: 1,
            rejected: 0,
            auto: 2,
            manual: 2,
        };
        assert_eq!(all_stats.totals, expected_counts);
        assert_eq!(all_stats.candidates.len(), 1);
        assert_eq!(all_stats.candidates[0].counts, expected_counts);
        assert_eq!(
            all_stats.candidates[0].current_status.as_deref(),
            Some("promoted")
        );
        assert_eq!(
            all_stats.candidates[0].last_reason.as_deref(),
            Some("auto repromote")
        );

        let blocked_window = runtime
            .model_router_lifecycle_stats(ModelRouterLifecycleStatsQuery {
                window_start_ms: Some(25),
                window_end_ms: 35,
                task_key: Some(task_key.to_string()),
                candidate_identity: Some(candidate_identity.to_string()),
                event_limit: 50,
            })
            .await
            .expect("blocked window stats");
        assert_eq!(
            blocked_window.totals,
            ModelRouterLifecycleEventCounts {
                promoted: 0,
                demoted: 0,
                evaluating: 0,
                promotion_blocked: 1,
                rejected: 0,
                auto: 1,
                manual: 0,
            }
        );

        assert!(
            runtime
                .model_router_lifecycle_stats(ModelRouterLifecycleStatsQuery {
                    window_start_ms: None,
                    window_end_ms: i64::MAX,
                    task_key: Some(task_key.to_string()),
                    candidate_identity: Some("missing".to_string()),
                    event_limit: 50,
                })
                .await
                .expect("missing stats")
                .candidates
                .is_empty()
        );
    }

    #[tokio::test]
    async fn lifecycle_persists_evaluating_and_rejected_statuses() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");
        let task_key = "module.repo_ci.review";
        let candidate_identity = "candidate";
        let base_candidate_identity = "base";
        let summary = ModelRouterShadowEvaluationSummary {
            task_key: task_key.to_string(),
            phase: "promotion".to_string(),
            candidate_identity: candidate_identity.to_string(),
            base_candidate_identity: base_candidate_identity.to_string(),
            evaluated_count: 1,
            success_count: 0,
            success_rate: 0.0,
            average_score: Some(0.0),
            average_confidence: 1.0,
            cost_used_usd_micros: 10,
            tokens_used: 20,
            latest_evaluation_id: Some(7),
            latest_evaluation_at_ms: Some(9),
        };

        runtime
            .mark_model_router_lifecycle_candidate_evaluating(
                ModelRouterLifecyclePromotionRecord {
                    task_key: task_key.to_string(),
                    candidate_identity: candidate_identity.to_string(),
                    base_candidate_identity: base_candidate_identity.to_string(),
                    status: MODEL_ROUTER_LIFECYCLE_EVENT_EVALUATING.to_string(),
                    rule_id: Some("review".to_string()),
                    production_model_provider: Some("openai".to_string()),
                    production_model: Some("gpt-5.5".to_string()),
                    base_model_provider: Some("openai".to_string()),
                    base_model: Some("gpt-5.4".to_string()),
                    promoted_at_ms: 10,
                    updated_at_ms: 10,
                    reason: Some("started".to_string()),
                },
                ModelRouterLifecycleTransitionContext {
                    source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
                    lifecycle_window: Some("all".to_string()),
                    shadow_phase: Some("promotion".to_string()),
                    shadow_summary: Some(summary.clone()),
                    failed_gates_json: None,
                },
            )
            .await
            .expect("mark evaluating");
        runtime
            .reject_model_router_lifecycle_candidate(
                ModelRouterLifecyclePromotionRecord {
                    task_key: task_key.to_string(),
                    candidate_identity: candidate_identity.to_string(),
                    base_candidate_identity: base_candidate_identity.to_string(),
                    status: MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED.to_string(),
                    rule_id: Some("review".to_string()),
                    production_model_provider: Some("openai".to_string()),
                    production_model: Some("gpt-5.5".to_string()),
                    base_model_provider: Some("openai".to_string()),
                    base_model: Some("gpt-5.4".to_string()),
                    promoted_at_ms: 20,
                    updated_at_ms: 20,
                    reason: Some("failed gates".to_string()),
                },
                ModelRouterLifecycleTransitionContext {
                    source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
                    lifecycle_window: Some("all".to_string()),
                    shadow_phase: Some("promotion".to_string()),
                    shadow_summary: Some(summary),
                    failed_gates_json: Some(r#"[{"gate":"min_success_rate"}]"#.to_string()),
                },
            )
            .await
            .expect("reject");

        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some(task_key))
                .await
                .expect("promotions")
                .first()
                .map(|promotion| promotion.status.as_str()),
            Some(MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED)
        );
        let stats = runtime
            .model_router_lifecycle_stats(ModelRouterLifecycleStatsQuery {
                window_start_ms: None,
                window_end_ms: i64::MAX,
                task_key: Some(task_key.to_string()),
                candidate_identity: Some(candidate_identity.to_string()),
                event_limit: 50,
            })
            .await
            .expect("stats");
        assert_eq!(
            stats.totals,
            ModelRouterLifecycleEventCounts {
                promoted: 0,
                demoted: 0,
                evaluating: 1,
                promotion_blocked: 0,
                rejected: 1,
                auto: 2,
                manual: 0,
            }
        );
        assert_eq!(
            stats.candidates[0].current_status.as_deref(),
            Some(MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED)
        );
    }

    fn usage_group_projection(
        summary: &ModelRouterUsageSummary,
    ) -> Vec<(String, i64, i64, i64, i64, i64)> {
        summary
            .groups
            .iter()
            .map(|group| {
                (
                    group.key.clone(),
                    group.totals.request_count,
                    group.totals.production_request_count,
                    group.totals.overhead_request_count,
                    group.totals.token_usage.total_tokens,
                    group.totals.savings.net_savings_usd_micros,
                )
            })
            .collect()
    }
}
