use crate::runtime::StateRuntime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::query::Query;
use sqlx::sqlite::SqliteArguments;
use std::collections::BTreeMap;

use super::ModelRouterLifecyclePromotionRecord;
use super::ModelRouterShadowEvaluationSummary;
use super::model_router_lifecycle_promotion_from_row;

pub const MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED: &str = "promoted";
pub const MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED: &str = "demoted";
pub const MODEL_ROUTER_LIFECYCLE_EVENT_EVALUATING: &str = "evaluating";
pub const MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED: &str = "promotion_blocked";
pub const MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED: &str = "rejected";
pub const MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO: &str = "auto";
pub const MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL: &str = "manual";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterLifecycleEventRecord {
    pub id: Option<i64>,
    pub created_at_ms: i64,
    pub event_type: String,
    pub source: String,
    pub task_key: String,
    pub candidate_identity: String,
    pub base_candidate_identity: String,
    pub previous_status: Option<String>,
    pub next_status: Option<String>,
    pub rule_id: Option<String>,
    pub reason: Option<String>,
    pub production_model_provider: Option<String>,
    pub production_model: Option<String>,
    pub base_model_provider: Option<String>,
    pub base_model: Option<String>,
    pub lifecycle_window: Option<String>,
    pub shadow_phase: Option<String>,
    pub shadow_evaluated_count: Option<i64>,
    pub shadow_success_count: Option<i64>,
    pub shadow_success_rate: Option<f64>,
    pub shadow_average_score: Option<f64>,
    pub shadow_average_confidence: Option<f64>,
    pub shadow_cost_used_usd_micros: Option<i64>,
    pub shadow_tokens_used: Option<i64>,
    pub shadow_latest_evaluation_id: Option<i64>,
    pub shadow_latest_evaluation_at_ms: Option<i64>,
    pub failed_gates_json: Option<String>,
}

impl ModelRouterLifecycleEventRecord {
    pub fn apply_shadow_summary(
        &mut self,
        phase: impl Into<String>,
        summary: &ModelRouterShadowEvaluationSummary,
    ) {
        self.shadow_phase = Some(phase.into());
        self.shadow_evaluated_count = Some(summary.evaluated_count);
        self.shadow_success_count = Some(summary.success_count);
        self.shadow_success_rate = Some(summary.success_rate);
        self.shadow_average_score = summary.average_score;
        self.shadow_average_confidence = Some(summary.average_confidence);
        self.shadow_cost_used_usd_micros = Some(summary.cost_used_usd_micros);
        self.shadow_tokens_used = Some(summary.tokens_used);
        self.shadow_latest_evaluation_id = summary.latest_evaluation_id;
        self.shadow_latest_evaluation_at_ms = summary.latest_evaluation_at_ms;
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelRouterLifecycleTransitionContext {
    pub source: String,
    pub lifecycle_window: Option<String>,
    pub shadow_phase: Option<String>,
    pub shadow_summary: Option<ModelRouterShadowEvaluationSummary>,
    pub failed_gates_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterLifecycleStatsQuery {
    pub window_start_ms: Option<i64>,
    pub window_end_ms: i64,
    pub task_key: Option<String>,
    pub candidate_identity: Option<String>,
    pub event_limit: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterLifecycleEventCounts {
    pub promoted: i64,
    pub demoted: i64,
    pub evaluating: i64,
    pub promotion_blocked: i64,
    pub rejected: i64,
    pub auto: i64,
    pub manual: i64,
}

impl ModelRouterLifecycleEventCounts {
    fn record(&mut self, event: &ModelRouterLifecycleEventRecord) {
        match event.event_type.as_str() {
            MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED => self.promoted += 1,
            MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED => self.demoted += 1,
            MODEL_ROUTER_LIFECYCLE_EVENT_EVALUATING => self.evaluating += 1,
            MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED => self.promotion_blocked += 1,
            MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED => self.rejected += 1,
            _ => {}
        }
        match event.source.as_str() {
            MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO => self.auto += 1,
            MODEL_ROUTER_LIFECYCLE_SOURCE_MANUAL => self.manual += 1,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterLifecycleCandidateStats {
    pub task_key: String,
    pub candidate_identity: String,
    pub current_status: Option<String>,
    pub base_candidate_identity: Option<String>,
    pub rule_id: Option<String>,
    pub production_model_provider: Option<String>,
    pub production_model: Option<String>,
    pub base_model_provider: Option<String>,
    pub base_model: Option<String>,
    pub promoted_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub counts: ModelRouterLifecycleEventCounts,
    pub last_event_at_ms: Option<i64>,
    pub last_event_type: Option<String>,
    pub last_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterLifecycleStatsSummary {
    pub window_start_ms: Option<i64>,
    pub window_end_ms: i64,
    pub task_key: Option<String>,
    pub candidate_identity: Option<String>,
    pub totals: ModelRouterLifecycleEventCounts,
    pub candidates: Vec<ModelRouterLifecycleCandidateStats>,
}

impl StateRuntime {
    pub async fn mark_model_router_lifecycle_candidate_evaluating(
        &self,
        record: ModelRouterLifecyclePromotionRecord,
        context: ModelRouterLifecycleTransitionContext,
    ) -> anyhow::Result<()> {
        self.upsert_model_router_lifecycle_status_with_event(
            record,
            MODEL_ROUTER_LIFECYCLE_EVENT_EVALUATING,
            context,
        )
        .await
    }

    pub async fn promote_model_router_lifecycle_promotion(
        &self,
        record: ModelRouterLifecyclePromotionRecord,
        context: ModelRouterLifecycleTransitionContext,
    ) -> anyhow::Result<()> {
        self.upsert_model_router_lifecycle_status_with_event(
            record,
            MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED,
            context,
        )
        .await
    }

    pub async fn reject_model_router_lifecycle_candidate(
        &self,
        record: ModelRouterLifecyclePromotionRecord,
        context: ModelRouterLifecycleTransitionContext,
    ) -> anyhow::Result<()> {
        self.upsert_model_router_lifecycle_status_with_event(
            record,
            MODEL_ROUTER_LIFECYCLE_EVENT_REJECTED,
            context,
        )
        .await
    }

    async fn upsert_model_router_lifecycle_status_with_event(
        &self,
        record: ModelRouterLifecyclePromotionRecord,
        event_type: &str,
        context: ModelRouterLifecycleTransitionContext,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let previous = select_model_router_lifecycle_promotion_tx(
            &mut tx,
            &record.task_key,
            &record.candidate_identity,
        )
        .await?;
        upsert_model_router_lifecycle_promotion_tx(&mut tx, &record).await?;

        let mut event = lifecycle_event_from_status_transition(
            &record,
            event_type,
            previous.as_ref().map(|record| record.status.clone()),
            context,
        );
        event.created_at_ms = record.updated_at_ms;
        insert_model_router_lifecycle_event_tx(&mut tx, &event).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn demote_model_router_lifecycle_promotion_with_event(
        &self,
        task_key: &str,
        candidate_identity: &str,
        reason: Option<&str>,
        context: ModelRouterLifecycleTransitionContext,
    ) -> anyhow::Result<u64> {
        let mut tx = self.pool.begin().await?;
        let Some(previous) =
            select_model_router_lifecycle_promotion_tx(&mut tx, task_key, candidate_identity)
                .await?
        else {
            tx.commit().await?;
            return Ok(0);
        };
        if previous
            .status
            .eq_ignore_ascii_case(MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED)
        {
            tx.commit().await?;
            return Ok(0);
        }

        let now_ms = Utc::now().timestamp_millis();
        let result = sqlx::query(
            r#"
            UPDATE model_router_lifecycle_promotions
            SET status = 'demoted', updated_at_ms = ?, reason = ?
            WHERE task_key = ? AND candidate_identity = ?
            "#,
        )
        .bind(now_ms)
        .bind(reason)
        .bind(task_key)
        .bind(candidate_identity)
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() > 0 {
            let mut event = lifecycle_event_from_demotion_transition(
                &previous,
                reason.map(str::to_string),
                now_ms,
                context,
            );
            event.created_at_ms = now_ms;
            insert_model_router_lifecycle_event_tx(&mut tx, &event).await?;
        }

        tx.commit().await?;
        Ok(result.rows_affected())
    }

    pub async fn record_model_router_lifecycle_event_once(
        &self,
        event: ModelRouterLifecycleEventRecord,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let rows = insert_or_ignore_model_router_lifecycle_event_tx(&mut tx, &event).await?;
        tx.commit().await?;
        Ok(rows > 0)
    }

    pub async fn model_router_lifecycle_events(
        &self,
        query: ModelRouterLifecycleStatsQuery,
    ) -> anyhow::Result<Vec<ModelRouterLifecycleEventRecord>> {
        let rows = model_router_lifecycle_events_query(&query, Some(query.event_limit))
            .build()
            .fetch_all(self.pool.as_ref())
            .await?;
        rows.into_iter()
            .map(model_router_lifecycle_event_from_row)
            .collect()
    }

    pub async fn model_router_lifecycle_stats(
        &self,
        query: ModelRouterLifecycleStatsQuery,
    ) -> anyhow::Result<ModelRouterLifecycleStatsSummary> {
        let event_rows = model_router_lifecycle_events_query(&query, /*limit*/ None)
            .build()
            .fetch_all(self.pool.as_ref())
            .await?;
        let events = event_rows
            .into_iter()
            .map(model_router_lifecycle_event_from_row)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut promotions = self
            .model_router_lifecycle_promotions(query.task_key.as_deref())
            .await?;
        if let Some(candidate_identity) = query.candidate_identity.as_deref() {
            promotions.retain(|promotion| promotion.candidate_identity == candidate_identity);
        }

        let mut totals = ModelRouterLifecycleEventCounts::default();
        let mut candidates =
            BTreeMap::<(String, String), ModelRouterLifecycleCandidateStats>::new();
        for promotion in promotions {
            let key = (
                promotion.task_key.clone(),
                promotion.candidate_identity.clone(),
            );
            let stats = candidates
                .entry(key)
                .or_insert_with(|| lifecycle_stats_from_promotion(&promotion));
            apply_promotion_to_stats(stats, &promotion);
        }

        for event in &events {
            totals.record(event);
            let key = (event.task_key.clone(), event.candidate_identity.clone());
            let stats = candidates
                .entry(key)
                .or_insert_with(|| lifecycle_stats_from_event(event));
            stats.counts.record(event);
            if stats.last_event_at_ms.is_none() {
                stats.last_event_at_ms = Some(event.created_at_ms);
                stats.last_event_type = Some(event.event_type.clone());
                stats.last_reason = event.reason.clone();
            }
            fill_stats_identity_from_event(stats, event);
        }

        Ok(ModelRouterLifecycleStatsSummary {
            window_start_ms: query.window_start_ms,
            window_end_ms: query.window_end_ms,
            task_key: query.task_key,
            candidate_identity: query.candidate_identity,
            totals,
            candidates: candidates.into_values().collect(),
        })
    }
}

async fn select_model_router_lifecycle_promotion_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    task_key: &str,
    candidate_identity: &str,
) -> anyhow::Result<Option<ModelRouterLifecyclePromotionRecord>> {
    let row = sqlx::query(
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
        WHERE task_key = ? AND candidate_identity = ?
        "#,
    )
    .bind(task_key)
    .bind(candidate_identity)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(model_router_lifecycle_promotion_from_row)
        .transpose()
}

async fn upsert_model_router_lifecycle_promotion_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    record: &ModelRouterLifecyclePromotionRecord,
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
    .bind(&record.task_key)
    .bind(&record.candidate_identity)
    .bind(&record.base_candidate_identity)
    .bind(&record.status)
    .bind(record.rule_id.as_deref())
    .bind(record.production_model_provider.as_deref())
    .bind(record.production_model.as_deref())
    .bind(record.base_model_provider.as_deref())
    .bind(record.base_model.as_deref())
    .bind(record.promoted_at_ms)
    .bind(record.updated_at_ms)
    .bind(&record.reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_model_router_lifecycle_event_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    event: &ModelRouterLifecycleEventRecord,
) -> anyhow::Result<u64> {
    let result = bind_model_router_lifecycle_event(sqlx::query(LIFECYCLE_EVENT_INSERT_SQL), event)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

async fn insert_or_ignore_model_router_lifecycle_event_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    event: &ModelRouterLifecycleEventRecord,
) -> anyhow::Result<u64> {
    let result =
        bind_model_router_lifecycle_event(sqlx::query(LIFECYCLE_EVENT_INSERT_OR_IGNORE_SQL), event)
            .execute(&mut **tx)
            .await?;
    Ok(result.rows_affected())
}

const LIFECYCLE_EVENT_INSERT_SQL: &str = r#"
    INSERT INTO model_router_lifecycle_events (
        created_at_ms,
        event_type,
        source,
        task_key,
        candidate_identity,
        base_candidate_identity,
        previous_status,
        next_status,
        rule_id,
        reason,
        production_model_provider,
        production_model,
        base_model_provider,
        base_model,
        lifecycle_window,
        shadow_phase,
        shadow_evaluated_count,
        shadow_success_count,
        shadow_success_rate,
        shadow_average_score,
        shadow_average_confidence,
        shadow_cost_used_usd_micros,
        shadow_tokens_used,
        shadow_latest_evaluation_id,
        shadow_latest_evaluation_at_ms,
        failed_gates_json
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    "#;

const LIFECYCLE_EVENT_INSERT_OR_IGNORE_SQL: &str = r#"
    INSERT OR IGNORE INTO model_router_lifecycle_events (
        created_at_ms,
        event_type,
        source,
        task_key,
        candidate_identity,
        base_candidate_identity,
        previous_status,
        next_status,
        rule_id,
        reason,
        production_model_provider,
        production_model,
        base_model_provider,
        base_model,
        lifecycle_window,
        shadow_phase,
        shadow_evaluated_count,
        shadow_success_count,
        shadow_success_rate,
        shadow_average_score,
        shadow_average_confidence,
        shadow_cost_used_usd_micros,
        shadow_tokens_used,
        shadow_latest_evaluation_id,
        shadow_latest_evaluation_at_ms,
        failed_gates_json
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    "#;

fn bind_model_router_lifecycle_event<'q>(
    query: Query<'q, Sqlite, SqliteArguments>,
    event: &'q ModelRouterLifecycleEventRecord,
) -> Query<'q, Sqlite, SqliteArguments> {
    query
        .bind(event.created_at_ms)
        .bind(&event.event_type)
        .bind(&event.source)
        .bind(&event.task_key)
        .bind(&event.candidate_identity)
        .bind(&event.base_candidate_identity)
        .bind(event.previous_status.as_deref())
        .bind(event.next_status.as_deref())
        .bind(event.rule_id.as_deref())
        .bind(event.reason.as_deref())
        .bind(event.production_model_provider.as_deref())
        .bind(event.production_model.as_deref())
        .bind(event.base_model_provider.as_deref())
        .bind(event.base_model.as_deref())
        .bind(event.lifecycle_window.as_deref())
        .bind(event.shadow_phase.as_deref())
        .bind(event.shadow_evaluated_count)
        .bind(event.shadow_success_count)
        .bind(event.shadow_success_rate)
        .bind(event.shadow_average_score)
        .bind(event.shadow_average_confidence)
        .bind(event.shadow_cost_used_usd_micros)
        .bind(event.shadow_tokens_used)
        .bind(event.shadow_latest_evaluation_id)
        .bind(event.shadow_latest_evaluation_at_ms)
        .bind(event.failed_gates_json.as_deref())
}

fn model_router_lifecycle_events_query(
    query: &ModelRouterLifecycleStatsQuery,
    limit: Option<i64>,
) -> QueryBuilder<Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT
            id,
            created_at_ms,
            event_type,
            source,
            task_key,
            candidate_identity,
            base_candidate_identity,
            previous_status,
            next_status,
            rule_id,
            reason,
            production_model_provider,
            production_model,
            base_model_provider,
            base_model,
            lifecycle_window,
            shadow_phase,
            shadow_evaluated_count,
            shadow_success_count,
            shadow_success_rate,
            shadow_average_score,
            shadow_average_confidence,
            shadow_cost_used_usd_micros,
            shadow_tokens_used,
            shadow_latest_evaluation_id,
            shadow_latest_evaluation_at_ms,
            failed_gates_json
        FROM model_router_lifecycle_events
        WHERE created_at_ms <=
        "#,
    );
    builder.push_bind(query.window_end_ms);
    if let Some(window_start_ms) = query.window_start_ms {
        builder.push(" AND created_at_ms >= ");
        builder.push_bind(window_start_ms);
    }
    if let Some(task_key) = query.task_key.as_deref() {
        builder.push(" AND task_key = ");
        builder.push_bind(task_key.to_string());
    }
    if let Some(candidate_identity) = query.candidate_identity.as_deref() {
        builder.push(" AND candidate_identity = ");
        builder.push_bind(candidate_identity.to_string());
    }
    builder.push(" ORDER BY created_at_ms DESC, id DESC");
    if let Some(limit) = limit {
        builder.push(" LIMIT ");
        builder.push_bind(limit.clamp(1, 1_000));
    }
    builder
}

fn model_router_lifecycle_event_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<ModelRouterLifecycleEventRecord> {
    Ok(ModelRouterLifecycleEventRecord {
        id: row.try_get("id")?,
        created_at_ms: row.try_get("created_at_ms")?,
        event_type: row.try_get("event_type")?,
        source: row.try_get("source")?,
        task_key: row.try_get("task_key")?,
        candidate_identity: row.try_get("candidate_identity")?,
        base_candidate_identity: row.try_get("base_candidate_identity")?,
        previous_status: row.try_get("previous_status")?,
        next_status: row.try_get("next_status")?,
        rule_id: row.try_get("rule_id")?,
        reason: row.try_get("reason")?,
        production_model_provider: row.try_get("production_model_provider")?,
        production_model: row.try_get("production_model")?,
        base_model_provider: row.try_get("base_model_provider")?,
        base_model: row.try_get("base_model")?,
        lifecycle_window: row.try_get("lifecycle_window")?,
        shadow_phase: row.try_get("shadow_phase")?,
        shadow_evaluated_count: row.try_get("shadow_evaluated_count")?,
        shadow_success_count: row.try_get("shadow_success_count")?,
        shadow_success_rate: row.try_get("shadow_success_rate")?,
        shadow_average_score: row.try_get("shadow_average_score")?,
        shadow_average_confidence: row.try_get("shadow_average_confidence")?,
        shadow_cost_used_usd_micros: row.try_get("shadow_cost_used_usd_micros")?,
        shadow_tokens_used: row.try_get("shadow_tokens_used")?,
        shadow_latest_evaluation_id: row.try_get("shadow_latest_evaluation_id")?,
        shadow_latest_evaluation_at_ms: row.try_get("shadow_latest_evaluation_at_ms")?,
        failed_gates_json: row.try_get("failed_gates_json")?,
    })
}

fn lifecycle_event_from_status_transition(
    record: &ModelRouterLifecyclePromotionRecord,
    event_type: &str,
    previous_status: Option<String>,
    context: ModelRouterLifecycleTransitionContext,
) -> ModelRouterLifecycleEventRecord {
    let mut event = ModelRouterLifecycleEventRecord {
        id: None,
        created_at_ms: record.updated_at_ms,
        event_type: event_type.to_string(),
        source: context.source,
        task_key: record.task_key.clone(),
        candidate_identity: record.candidate_identity.clone(),
        base_candidate_identity: record.base_candidate_identity.clone(),
        previous_status,
        next_status: Some(record.status.clone()),
        rule_id: record.rule_id.clone(),
        reason: record.reason.clone(),
        production_model_provider: record.production_model_provider.clone(),
        production_model: record.production_model.clone(),
        base_model_provider: record.base_model_provider.clone(),
        base_model: record.base_model.clone(),
        lifecycle_window: context.lifecycle_window,
        shadow_phase: None,
        shadow_evaluated_count: None,
        shadow_success_count: None,
        shadow_success_rate: None,
        shadow_average_score: None,
        shadow_average_confidence: None,
        shadow_cost_used_usd_micros: None,
        shadow_tokens_used: None,
        shadow_latest_evaluation_id: None,
        shadow_latest_evaluation_at_ms: None,
        failed_gates_json: context.failed_gates_json,
    };
    if let (Some(phase), Some(summary)) = (context.shadow_phase, context.shadow_summary.as_ref()) {
        event.apply_shadow_summary(phase, summary);
    }
    event
}

fn lifecycle_event_from_demotion_transition(
    record: &ModelRouterLifecyclePromotionRecord,
    reason: Option<String>,
    now_ms: i64,
    context: ModelRouterLifecycleTransitionContext,
) -> ModelRouterLifecycleEventRecord {
    let mut event = ModelRouterLifecycleEventRecord {
        id: None,
        created_at_ms: now_ms,
        event_type: MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED.to_string(),
        source: context.source,
        task_key: record.task_key.clone(),
        candidate_identity: record.candidate_identity.clone(),
        base_candidate_identity: record.base_candidate_identity.clone(),
        previous_status: Some(record.status.clone()),
        next_status: Some(MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED.to_string()),
        rule_id: record.rule_id.clone(),
        reason,
        production_model_provider: record.production_model_provider.clone(),
        production_model: record.production_model.clone(),
        base_model_provider: record.base_model_provider.clone(),
        base_model: record.base_model.clone(),
        lifecycle_window: context.lifecycle_window,
        shadow_phase: None,
        shadow_evaluated_count: None,
        shadow_success_count: None,
        shadow_success_rate: None,
        shadow_average_score: None,
        shadow_average_confidence: None,
        shadow_cost_used_usd_micros: None,
        shadow_tokens_used: None,
        shadow_latest_evaluation_id: None,
        shadow_latest_evaluation_at_ms: None,
        failed_gates_json: context.failed_gates_json,
    };
    if let (Some(phase), Some(summary)) = (context.shadow_phase, context.shadow_summary.as_ref()) {
        event.apply_shadow_summary(phase, summary);
    }
    event
}

fn lifecycle_stats_from_promotion(
    promotion: &ModelRouterLifecyclePromotionRecord,
) -> ModelRouterLifecycleCandidateStats {
    ModelRouterLifecycleCandidateStats {
        task_key: promotion.task_key.clone(),
        candidate_identity: promotion.candidate_identity.clone(),
        current_status: Some(promotion.status.clone()),
        base_candidate_identity: Some(promotion.base_candidate_identity.clone()),
        rule_id: promotion.rule_id.clone(),
        production_model_provider: promotion.production_model_provider.clone(),
        production_model: promotion.production_model.clone(),
        base_model_provider: promotion.base_model_provider.clone(),
        base_model: promotion.base_model.clone(),
        promoted_at_ms: Some(promotion.promoted_at_ms),
        updated_at_ms: Some(promotion.updated_at_ms),
        counts: ModelRouterLifecycleEventCounts::default(),
        last_event_at_ms: None,
        last_event_type: None,
        last_reason: None,
    }
}

fn lifecycle_stats_from_event(
    event: &ModelRouterLifecycleEventRecord,
) -> ModelRouterLifecycleCandidateStats {
    ModelRouterLifecycleCandidateStats {
        task_key: event.task_key.clone(),
        candidate_identity: event.candidate_identity.clone(),
        current_status: event.next_status.clone(),
        base_candidate_identity: Some(event.base_candidate_identity.clone()),
        rule_id: event.rule_id.clone(),
        production_model_provider: event.production_model_provider.clone(),
        production_model: event.production_model.clone(),
        base_model_provider: event.base_model_provider.clone(),
        base_model: event.base_model.clone(),
        promoted_at_ms: None,
        updated_at_ms: None,
        counts: ModelRouterLifecycleEventCounts::default(),
        last_event_at_ms: None,
        last_event_type: None,
        last_reason: None,
    }
}

fn apply_promotion_to_stats(
    stats: &mut ModelRouterLifecycleCandidateStats,
    promotion: &ModelRouterLifecyclePromotionRecord,
) {
    stats.current_status = Some(promotion.status.clone());
    stats.base_candidate_identity = Some(promotion.base_candidate_identity.clone());
    stats.rule_id = promotion.rule_id.clone();
    stats.production_model_provider = promotion.production_model_provider.clone();
    stats.production_model = promotion.production_model.clone();
    stats.base_model_provider = promotion.base_model_provider.clone();
    stats.base_model = promotion.base_model.clone();
    stats.promoted_at_ms = Some(promotion.promoted_at_ms);
    stats.updated_at_ms = Some(promotion.updated_at_ms);
}

fn fill_stats_identity_from_event(
    stats: &mut ModelRouterLifecycleCandidateStats,
    event: &ModelRouterLifecycleEventRecord,
) {
    if stats.base_candidate_identity.is_none() {
        stats.base_candidate_identity = Some(event.base_candidate_identity.clone());
    }
    if stats.rule_id.is_none() {
        stats.rule_id = event.rule_id.clone();
    }
    if stats.production_model_provider.is_none() {
        stats.production_model_provider = event.production_model_provider.clone();
    }
    if stats.production_model.is_none() {
        stats.production_model = event.production_model.clone();
    }
    if stats.base_model_provider.is_none() {
        stats.base_model_provider = event.base_model_provider.clone();
    }
    if stats.base_model.is_none() {
        stats.base_model = event.base_model.clone();
    }
}
