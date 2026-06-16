use crate::runtime::StateRuntime;
use crate::runtime::tool_router::ToolRouterLedgerEntry;
use chrono::Utc;
use sqlx::Row;

mod detect;
mod lookup;
mod types;

use detect::already_optimized_suggestion;
use detect::basis_points;
use detect::recovery_reason;
use types::OutputOptimizationKey;
use types::OutputOptimizationSuggestion;
pub use types::ToolRouterOutputOptimizationRecord;
pub use types::ToolRouterOutputOptimizationStatus;

const MIN_SUGGESTION_ORIGINAL_TOKENS: i64 = 400;
const ACCEPT_MIN_OBSERVATIONS: i64 = 3;
const ACCEPT_MIN_SAVINGS_BASIS_POINTS: i64 = 3_500;
const DECLINE_MIN_OBSERVATIONS: i64 = 3;
const DECLINE_MAX_SAVINGS_BASIS_POINTS: i64 = 1_000;
const OPTIMIZED_MIN_OBSERVATIONS: i64 = 3;
const OPTIMIZED_MAX_AVERAGE_TOKENS: i64 = 400;
const RECOVERY_LOOKBACK_MS: i64 = 10 * 60 * 1000;
const LEARNED_OUTPUT_COMPACTION_FILTER_PREFIX: &str = "exec.";

impl StateRuntime {
    pub async fn record_tool_router_output_optimization_observation(
        &self,
        entry: ToolRouterLedgerEntry,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        self.record_output_optimization_recovery_signal(&entry, now_ms)
            .await?;
        if entry_has_builtin_output_compaction(&entry) {
            return Ok(());
        }

        let key = output_optimization_key(&entry);
        let suggestions = self
            .output_optimization_suggestions(&entry, now_ms, MIN_SUGGESTION_ORIGINAL_TOKENS)
            .await?;
        if suggestions.is_empty() {
            if let Some(suggestion) =
                already_optimized_suggestion(&entry, OPTIMIZED_MAX_AVERAGE_TOKENS)
            {
                self.record_output_optimization_suggestion(&key, &entry, suggestion, now_ms)
                    .await?;
            }
            return Ok(());
        }

        for suggestion in suggestions {
            self.record_output_optimization_suggestion(&key, &entry, suggestion, now_ms)
                .await?;
        }
        Ok(())
    }

    pub async fn list_tool_router_output_optimizations(
        &self,
        status: Option<ToolRouterOutputOptimizationStatus>,
    ) -> anyhow::Result<Vec<ToolRouterOutputOptimizationRecord>> {
        let rows = match status {
            Some(status) => {
                sqlx::query(
                    r#"
                    SELECT
                        model_slug,
                        model_provider,
                        toolset_hash,
                        router_schema_version,
                        tool_namespace,
                        tool_name,
                        suggestion_key,
                        suggestion_label,
                        status,
                        observation_count,
                        recovery_count,
                        original_output_tokens,
                        returned_output_tokens,
                        candidate_output_tokens,
                        saved_output_tokens,
                        last_decision_reason
                    FROM tool_router_output_optimizations
                    WHERE status = ?
                    ORDER BY saved_output_tokens DESC, observation_count DESC, suggestion_key
                    "#,
                )
                .bind(status.as_str())
                .fetch_all(self.pool.as_ref())
                .await?
            }
            None => {
                sqlx::query(
                    r#"
                    SELECT
                        model_slug,
                        model_provider,
                        toolset_hash,
                        router_schema_version,
                        tool_namespace,
                        tool_name,
                        suggestion_key,
                        suggestion_label,
                        status,
                        observation_count,
                        recovery_count,
                        original_output_tokens,
                        returned_output_tokens,
                        candidate_output_tokens,
                        saved_output_tokens,
                        last_decision_reason
                    FROM tool_router_output_optimizations
                    ORDER BY saved_output_tokens DESC, observation_count DESC, suggestion_key
                    "#,
                )
                .fetch_all(self.pool.as_ref())
                .await?
            }
        };

        rows.into_iter()
            .map(|row| {
                let status_text: String = row.try_get("status")?;
                Ok(ToolRouterOutputOptimizationRecord {
                    model_slug: row.try_get("model_slug")?,
                    model_provider: row.try_get("model_provider")?,
                    toolset_hash: row.try_get("toolset_hash")?,
                    router_schema_version: row.try_get("router_schema_version")?,
                    tool_namespace: row.try_get("tool_namespace")?,
                    tool_name: row.try_get("tool_name")?,
                    suggestion_key: row.try_get("suggestion_key")?,
                    suggestion_label: row.try_get("suggestion_label")?,
                    status: ToolRouterOutputOptimizationStatus::from_str(status_text.as_str()),
                    observation_count: row.try_get("observation_count")?,
                    recovery_count: row.try_get("recovery_count")?,
                    original_output_tokens: row.try_get("original_output_tokens")?,
                    returned_output_tokens: row.try_get("returned_output_tokens")?,
                    candidate_output_tokens: row.try_get("candidate_output_tokens")?,
                    saved_output_tokens: row.try_get("saved_output_tokens")?,
                    last_decision_reason: row.try_get("last_decision_reason")?,
                })
            })
            .collect()
    }

    async fn record_output_optimization_suggestion(
        &self,
        key: &OutputOptimizationKey,
        entry: &ToolRouterLedgerEntry,
        suggestion: OutputOptimizationSuggestion,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let Some((optimization_id, status)) = self
            .lookup_or_create_output_optimization(key, &suggestion, now_ms)
            .await?
        else {
            return Ok(());
        };

        if matches!(
            status,
            ToolRouterOutputOptimizationStatus::Declined
                | ToolRouterOutputOptimizationStatus::Optimized
        ) {
            return Ok(());
        }

        let result = sqlx::query(
            r#"
            INSERT OR IGNORE INTO tool_router_output_optimization_observations (
                created_at_ms,
                updated_at_ms,
                optimization_id,
                thread_id,
                turn_id,
                call_id,
                tool_input_json,
                original_output_tokens,
                returned_output_tokens,
                candidate_output_tokens,
                saved_output_tokens
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(optimization_id)
        .bind(&entry.thread_id)
        .bind(&entry.turn_id)
        .bind(&entry.call_id)
        .bind(&entry.tool_input_json)
        .bind(suggestion.original_output_tokens)
        .bind(suggestion.returned_output_tokens)
        .bind(suggestion.candidate_output_tokens)
        .bind(suggestion.saved_output_tokens)
        .execute(self.pool.as_ref())
        .await?;
        if result.rows_affected() == 0 {
            return Ok(());
        }

        sqlx::query(
            r#"
            UPDATE tool_router_output_optimizations
            SET
                updated_at_ms = ?,
                observation_count = observation_count + 1,
                original_output_tokens = original_output_tokens + ?,
                returned_output_tokens = returned_output_tokens + ?,
                candidate_output_tokens = candidate_output_tokens + ?,
                saved_output_tokens = saved_output_tokens + ?,
                last_observed_at_ms = ?
            WHERE id = ?
            "#,
        )
        .bind(now_ms)
        .bind(suggestion.original_output_tokens)
        .bind(suggestion.returned_output_tokens)
        .bind(suggestion.candidate_output_tokens)
        .bind(suggestion.saved_output_tokens)
        .bind(now_ms)
        .bind(optimization_id)
        .execute(self.pool.as_ref())
        .await?;

        self.refresh_output_optimization_decision(optimization_id, now_ms)
            .await
    }

    async fn lookup_or_create_output_optimization(
        &self,
        key: &OutputOptimizationKey,
        suggestion: &OutputOptimizationSuggestion,
        now_ms: i64,
    ) -> anyhow::Result<Option<(i64, ToolRouterOutputOptimizationStatus)>> {
        if let Some(row) = sqlx::query(
            r#"
            SELECT id, status
            FROM tool_router_output_optimizations
            WHERE model_slug = ?
              AND model_provider = ?
              AND toolset_hash = ?
              AND router_schema_version = ?
              AND tool_namespace = ?
              AND tool_name = ?
              AND suggestion_key = ?
            "#,
        )
        .bind(&key.model_slug)
        .bind(&key.model_provider)
        .bind(&key.toolset_hash)
        .bind(key.router_schema_version)
        .bind(&key.tool_namespace)
        .bind(&key.tool_name)
        .bind(&suggestion.suggestion_key)
        .fetch_optional(self.pool.as_ref())
        .await?
        {
            let status: String = row.try_get("status")?;
            return Ok(Some((
                row.try_get("id")?,
                ToolRouterOutputOptimizationStatus::from_str(status.as_str()),
            )));
        }

        let result = sqlx::query(
            r#"
            INSERT INTO tool_router_output_optimizations (
                created_at_ms,
                updated_at_ms,
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                tool_namespace,
                tool_name,
                suggestion_key,
                suggestion_label,
                status
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(&key.model_slug)
        .bind(&key.model_provider)
        .bind(&key.toolset_hash)
        .bind(key.router_schema_version)
        .bind(&key.tool_namespace)
        .bind(&key.tool_name)
        .bind(&suggestion.suggestion_key)
        .bind(&suggestion.suggestion_label)
        .bind(ToolRouterOutputOptimizationStatus::Candidate.as_str())
        .execute(self.pool.as_ref())
        .await?;

        Ok(Some((
            result.last_insert_rowid(),
            ToolRouterOutputOptimizationStatus::Candidate,
        )))
    }

    async fn refresh_output_optimization_decision(
        &self,
        optimization_id: i64,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let Some(row) = sqlx::query(
            r#"
            SELECT
                status,
                observation_count,
                recovery_count,
                original_output_tokens,
                candidate_output_tokens,
                saved_output_tokens,
                suggestion_key
            FROM tool_router_output_optimizations
            WHERE id = ?
            "#,
        )
        .bind(optimization_id)
        .fetch_optional(self.pool.as_ref())
        .await?
        else {
            return Ok(());
        };

        let status_text: String = row.try_get("status")?;
        if matches!(
            ToolRouterOutputOptimizationStatus::from_str(status_text.as_str()),
            ToolRouterOutputOptimizationStatus::Accepted
                | ToolRouterOutputOptimizationStatus::Declined
                | ToolRouterOutputOptimizationStatus::Optimized
        ) {
            return Ok(());
        }

        let observation_count: i64 = row.try_get("observation_count")?;
        let recovery_count: i64 = row.try_get("recovery_count")?;
        let original_output_tokens: i64 = row.try_get("original_output_tokens")?;
        let saved_output_tokens: i64 = row.try_get("saved_output_tokens")?;
        let suggestion_key: String = row.try_get("suggestion_key")?;
        let savings_basis_points = basis_points(saved_output_tokens, original_output_tokens);

        let decision = if recovery_count > 0 {
            Some((
                ToolRouterOutputOptimizationStatus::Declined,
                "declined after follow-up tool call indicated missing output detail".to_string(),
            ))
        } else if suggestion_key.ends_with(".already-optimized-v1")
            && observation_count >= OPTIMIZED_MIN_OBSERVATIONS
            && average_tokens(original_output_tokens, observation_count)
                <= OPTIMIZED_MAX_AVERAGE_TOKENS
        {
            Some((
                ToolRouterOutputOptimizationStatus::Optimized,
                "marked optimized after repeated low-volume outputs".to_string(),
            ))
        } else if observation_count >= ACCEPT_MIN_OBSERVATIONS
            && savings_basis_points >= ACCEPT_MIN_SAVINGS_BASIS_POINTS
        {
            Some((
                ToolRouterOutputOptimizationStatus::Accepted,
                format!("accepted after {savings_basis_points} bps estimated savings"),
            ))
        } else if observation_count >= DECLINE_MIN_OBSERVATIONS
            && savings_basis_points <= DECLINE_MAX_SAVINGS_BASIS_POINTS
        {
            Some((
                ToolRouterOutputOptimizationStatus::Declined,
                format!("declined after only {savings_basis_points} bps estimated savings"),
            ))
        } else {
            None
        };

        let Some((status, reason)) = decision else {
            return Ok(());
        };

        sqlx::query(
            r#"
            UPDATE tool_router_output_optimizations
            SET status = ?,
                updated_at_ms = ?,
                decided_at_ms = ?,
                last_decision_reason = ?
            WHERE id = ?
            "#,
        )
        .bind(status.as_str())
        .bind(now_ms)
        .bind(now_ms)
        .bind(reason)
        .bind(optimization_id)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    async fn record_output_optimization_recovery_signal(
        &self,
        entry: &ToolRouterLedgerEntry,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let Some(reason) = recovery_reason(entry) else {
            return Ok(());
        };
        if self
            .recovery_targets_builtin_output_compaction(entry)
            .await?
        {
            return Ok(());
        }
        let cutoff_ms = now_ms.saturating_sub(RECOVERY_LOOKBACK_MS);
        let Some(row) = sqlx::query(
            r#"
            SELECT observation.id AS observation_id, optimization.id AS optimization_id
            FROM tool_router_output_optimization_observations AS observation
            JOIN tool_router_output_optimizations AS optimization
              ON optimization.id = observation.optimization_id
            WHERE observation.thread_id = ?
              AND observation.created_at_ms >= ?
              AND observation.recovery_detected = 0
              AND optimization.status IN ('candidate', 'accepted')
            ORDER BY observation.created_at_ms DESC
            LIMIT 1
            "#,
        )
        .bind(&entry.thread_id)
        .bind(cutoff_ms)
        .fetch_optional(self.pool.as_ref())
        .await?
        else {
            return Ok(());
        };

        let observation_id: i64 = row.try_get("observation_id")?;
        let optimization_id: i64 = row.try_get("optimization_id")?;
        sqlx::query(
            r#"
            UPDATE tool_router_output_optimization_observations
            SET recovery_detected = 1,
                recovery_reason = ?,
                updated_at_ms = ?
            WHERE id = ?
            "#,
        )
        .bind(&reason)
        .bind(now_ms)
        .bind(observation_id)
        .execute(self.pool.as_ref())
        .await?;
        sqlx::query(
            r#"
            UPDATE tool_router_output_optimizations
            SET recovery_count = recovery_count + 1,
                updated_at_ms = ?
            WHERE id = ?
            "#,
        )
        .bind(now_ms)
        .bind(optimization_id)
        .execute(self.pool.as_ref())
        .await?;
        self.refresh_output_optimization_decision(optimization_id, now_ms)
            .await
    }

    async fn recovery_targets_builtin_output_compaction(
        &self,
        entry: &ToolRouterLedgerEntry,
    ) -> anyhow::Result<bool> {
        let Some(chunk_id) = read_exec_output_chunk_id(entry) else {
            return Ok(false);
        };
        let like_pattern = format!("%{chunk_id}%");
        let Some(filter) = sqlx::query_scalar::<_, String>(
            r#"
            SELECT output_compaction_filter
            FROM tool_router_ledger
            WHERE thread_id = ?
              AND tool_name IN ('exec_command', 'write_stdin')
              AND output_compaction_filter IS NOT NULL
              AND tool_output_json LIKE ?
            ORDER BY created_at_ms DESC
            LIMIT 1
            "#,
        )
        .bind(&entry.thread_id)
        .bind(like_pattern)
        .fetch_optional(self.pool.as_ref())
        .await?
        else {
            return Ok(false);
        };

        Ok(compaction_filter_is_builtin(filter.as_str()))
    }
}

fn output_optimization_key(entry: &ToolRouterLedgerEntry) -> OutputOptimizationKey {
    OutputOptimizationKey {
        model_slug: entry.model_slug.clone(),
        model_provider: entry.model_provider.clone(),
        toolset_hash: entry.toolset_hash.clone(),
        router_schema_version: entry.router_schema_version,
        tool_namespace: entry.tool_namespace.clone().unwrap_or_default(),
        tool_name: entry
            .tool_name
            .clone()
            .or_else(|| entry.selected_tools.first().cloned())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

fn entry_has_builtin_output_compaction(entry: &ToolRouterLedgerEntry) -> bool {
    entry
        .output_compaction_filter
        .as_deref()
        .is_some_and(compaction_filter_is_builtin)
}

fn compaction_filter_is_builtin(filter: &str) -> bool {
    !filter.starts_with(LEARNED_OUTPUT_COMPACTION_FILTER_PREFIX)
}

fn read_exec_output_chunk_id(entry: &ToolRouterLedgerEntry) -> Option<String> {
    if entry.tool_name.as_deref() != Some("read_exec_output") {
        return None;
    }
    let value =
        serde_json::from_str::<serde_json::Value>(entry.tool_input_json.as_deref()?).ok()?;
    value
        .get("chunk_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn average_tokens(total: i64, count: i64) -> i64 {
    if count == 0 { 0 } else { total / count }
}

#[cfg(test)]
#[path = "tool_router_output_optimization_tests.rs"]
mod tests;
