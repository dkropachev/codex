use crate::runtime::StateRuntime;
use chrono::Utc;
use sqlx::Row;
use std::collections::BTreeSet;

const TOOL_ROUTER_RULE_MATCH_KEY_MAX_LEN: usize = 1024;
const TOOL_ROUTER_TOOL_NAME: &str = "tool_router";
pub const TOOL_ROUTER_REMEMBERED_TOOL_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub const TOOL_ROUTER_REMEMBERED_TOOL_NAMESPACE_SENTINEL: &str = "";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterRememberedToolKey {
    pub repo_key: String,
    pub task_key: String,
    pub tool_namespace: String,
    pub tool_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterRememberedToolRecord {
    pub key: ToolRouterRememberedToolKey,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub request_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolRouterRememberedToolSelector {
    pub tool_namespace: String,
    pub tool_name: String,
}

impl ToolRouterRememberedToolRecord {
    pub fn selector(&self) -> ToolRouterRememberedToolSelector {
        ToolRouterRememberedToolSelector {
            tool_namespace: self.key.tool_namespace.clone(),
            tool_name: self.key.tool_name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterLedgerEntry {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
    pub model_response_ordinal: i64,
    pub guidance_version: i64,
    pub guidance_tokens: i64,
    pub format_description_tokens: i64,
    pub route_kind: String,
    pub selected_tools: Vec<String>,
    pub visible_router_schema_tokens: i64,
    pub hidden_tool_schema_tokens: i64,
    pub spark_prompt_tokens: i64,
    pub spark_completion_tokens: i64,
    pub fanout_call_count: i64,
    pub returned_output_tokens: i64,
    pub original_output_tokens: i64,
    pub truncated_output_tokens: i64,
    pub outcome: Option<String>,
    pub request_shape_json: Option<String>,
    pub tool_call_source: Option<String>,
    pub tool_name: Option<String>,
    pub tool_namespace: Option<String>,
    pub tool_input_json: Option<String>,
    pub tool_output_json: Option<String>,
    pub tool_success: Option<bool>,
    pub prompt_json: Option<String>,
    pub previous_prompt_json: Option<String>,
    pub dialog_locator_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterLearnedRule {
    pub match_key: String,
    pub route_json: String,
    pub source: String,
    pub hit_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterGuidanceKey {
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterGuidanceRecord {
    pub key: ToolRouterGuidanceKey,
    pub guidance_version: i64,
    pub guidance_text: String,
    pub guidance_tokens: i64,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRouterDiagnosticsWindow {
    AllTime,
    SinceCreatedAtMs(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterDiagnosticsSummary {
    pub total_calls: i64,
    pub successful_calls: i64,
    pub failed_calls: i64,
    pub invalid_route_errors: i64,
    pub deterministic_routes: i64,
    pub learned_rule_routes: i64,
    pub spark_routes: i64,
    pub spark_script_fallbacks: i64,
    pub fanout_routes: i64,
    pub total_fanout_calls: i64,
    pub visible_router_schema_tokens: i64,
    pub hidden_tool_schema_tokens: i64,
    pub spark_prompt_tokens: i64,
    pub spark_completion_tokens: i64,
    pub returned_output_tokens: i64,
    pub success_rate_basis_points: i64,
    pub learned_rule_count: i64,
    pub learned_rule_hits: i64,
    pub learned_rule_hit_rate_basis_points: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterRulePruneOptions {
    pub valid_tools: BTreeSet<String>,
    pub max_rule_age_ms: i64,
    pub max_rule_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterRulePruneResult {
    pub stale_rules_pruned: i64,
    pub invalid_rules_pruned: i64,
    pub over_limit_rules_pruned: i64,
}

impl StateRuntime {
    pub async fn upsert_tool_router_remembered_tool(
        &self,
        key: ToolRouterRememberedToolKey,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO tool_router_remembered_tools (
                created_at_ms,
                updated_at_ms,
                repo_key,
                task_key,
                tool_namespace,
                tool_name,
                request_count
            )
            VALUES (?, ?, ?, ?, ?, ?, 1)
            ON CONFLICT(repo_key, task_key, tool_namespace, tool_name) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                request_count = tool_router_remembered_tools.request_count + 1
            "#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(key.repo_key)
        .bind(key.task_key)
        .bind(key.tool_namespace)
        .bind(key.tool_name)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn list_tool_router_remembered_tools(
        &self,
        repo_key: &str,
        task_key: &str,
        updated_at_ms_cutoff: i64,
    ) -> anyhow::Result<Vec<ToolRouterRememberedToolRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT repo_key, task_key, tool_namespace, tool_name, created_at_ms, updated_at_ms, request_count
            FROM tool_router_remembered_tools
            WHERE repo_key = ?
              AND task_key = ?
              AND updated_at_ms >= ?
            ORDER BY updated_at_ms DESC, request_count DESC, tool_namespace, tool_name
            LIMIT 8
            "#,
        )
        .bind(repo_key)
        .bind(task_key)
        .bind(updated_at_ms_cutoff)
        .fetch_all(self.pool.as_ref())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(ToolRouterRememberedToolRecord {
                    key: ToolRouterRememberedToolKey {
                        repo_key: row.try_get("repo_key")?,
                        task_key: row.try_get("task_key")?,
                        tool_namespace: row.try_get("tool_namespace")?,
                        tool_name: row.try_get("tool_name")?,
                    },
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                    request_count: row.try_get("request_count")?,
                })
            })
            .collect()
    }

    pub async fn record_tool_router_ledger_entry(
        &self,
        entry: ToolRouterLedgerEntry,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        let selected_tools_json = serde_json::to_string(&entry.selected_tools)?;
        sqlx::query(
            r#"
            INSERT INTO tool_router_ledger (
                created_at_ms,
                thread_id,
                turn_id,
                call_id,
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                model_response_ordinal,
                guidance_version,
                guidance_tokens,
                format_description_tokens,
                route_kind,
                selected_tools_json,
                visible_router_schema_tokens,
                hidden_tool_schema_tokens,
                spark_prompt_tokens,
                spark_completion_tokens,
                fanout_call_count,
                returned_output_tokens,
                original_output_tokens,
                truncated_output_tokens,
                outcome,
                request_shape_json,
                tool_call_source,
                tool_name,
                tool_namespace,
                tool_input_json,
                tool_output_json,
                tool_success,
                prompt_json,
                previous_prompt_json,
                dialog_locator_json
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(now_ms)
        .bind(entry.thread_id)
        .bind(entry.turn_id)
        .bind(entry.call_id)
        .bind(entry.model_slug)
        .bind(entry.model_provider)
        .bind(entry.toolset_hash)
        .bind(entry.router_schema_version)
        .bind(entry.model_response_ordinal)
        .bind(entry.guidance_version)
        .bind(entry.guidance_tokens)
        .bind(entry.format_description_tokens)
        .bind(entry.route_kind)
        .bind(selected_tools_json)
        .bind(entry.visible_router_schema_tokens)
        .bind(entry.hidden_tool_schema_tokens)
        .bind(entry.spark_prompt_tokens)
        .bind(entry.spark_completion_tokens)
        .bind(entry.fanout_call_count)
        .bind(entry.returned_output_tokens)
        .bind(entry.original_output_tokens)
        .bind(entry.truncated_output_tokens)
        .bind(entry.outcome)
        .bind(entry.request_shape_json)
        .bind(entry.tool_call_source)
        .bind(entry.tool_name)
        .bind(entry.tool_namespace)
        .bind(entry.tool_input_json)
        .bind(entry.tool_output_json)
        .bind(entry.tool_success)
        .bind(entry.prompt_json)
        .bind(entry.previous_prompt_json)
        .bind(entry.dialog_locator_json)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn lookup_tool_router_guidance(
        &self,
        key: &ToolRouterGuidanceKey,
    ) -> anyhow::Result<Option<ToolRouterGuidanceRecord>> {
        let Some(row) = sqlx::query(
            r#"
            SELECT guidance_version, guidance_text, guidance_tokens, source
            FROM tool_router_guidance
            WHERE model_slug = ?
              AND model_provider = ?
              AND toolset_hash = ?
              AND router_schema_version = ?
            "#,
        )
        .bind(&key.model_slug)
        .bind(&key.model_provider)
        .bind(&key.toolset_hash)
        .bind(key.router_schema_version)
        .fetch_optional(self.pool.as_ref())
        .await?
        else {
            return Ok(None);
        };

        Ok(Some(ToolRouterGuidanceRecord {
            key: key.clone(),
            guidance_version: row.try_get("guidance_version")?,
            guidance_text: row.try_get("guidance_text")?,
            guidance_tokens: row.try_get("guidance_tokens")?,
            source: row.try_get("source")?,
        }))
    }

    pub async fn upsert_tool_router_guidance(
        &self,
        record: ToolRouterGuidanceRecord,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO tool_router_guidance (
                created_at_ms,
                updated_at_ms,
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                guidance_version,
                guidance_text,
                guidance_tokens,
                source
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(model_slug, model_provider, toolset_hash, router_schema_version) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                guidance_version = excluded.guidance_version,
                guidance_text = excluded.guidance_text,
                guidance_tokens = excluded.guidance_tokens,
                source = excluded.source
            "#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(record.key.model_slug)
        .bind(record.key.model_provider)
        .bind(record.key.toolset_hash)
        .bind(record.key.router_schema_version)
        .bind(record.guidance_version)
        .bind(record.guidance_text)
        .bind(record.guidance_tokens)
        .bind(record.source)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn lookup_tool_router_rule(
        &self,
        match_key: &str,
    ) -> anyhow::Result<Option<ToolRouterLearnedRule>> {
        let Some(row) = sqlx::query(
            r#"
            SELECT match_key, route_json, source, hit_count
            FROM tool_router_rules
            WHERE match_key = ?
            "#,
        )
        .bind(match_key)
        .fetch_optional(self.pool.as_ref())
        .await?
        else {
            return Ok(None);
        };

        Ok(Some(ToolRouterLearnedRule {
            match_key: row.try_get("match_key")?,
            route_json: row.try_get("route_json")?,
            source: row.try_get("source")?,
            hit_count: row.try_get("hit_count")?,
        }))
    }

    pub async fn record_tool_router_rule_hit(&self, match_key: &str) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            UPDATE tool_router_rules
            SET hit_count = hit_count + 1,
                last_hit_at_ms = ?,
                updated_at_ms = ?
            WHERE match_key = ?
            "#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(match_key)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn upsert_tool_router_rule(
        &self,
        match_key: &str,
        route_json: &str,
        source: &str,
    ) -> anyhow::Result<()> {
        let now_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO tool_router_rules (
                created_at_ms,
                updated_at_ms,
                match_key,
                route_json,
                source
            )
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(match_key) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                route_json = excluded.route_json,
                source = excluded.source
            "#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(match_key)
        .bind(route_json)
        .bind(source)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn tool_router_diagnostics_summary(
        &self,
        window: ToolRouterDiagnosticsWindow,
    ) -> anyhow::Result<ToolRouterDiagnosticsSummary> {
        let since_created_at_ms = match window {
            ToolRouterDiagnosticsWindow::AllTime => i64::MIN,
            ToolRouterDiagnosticsWindow::SinceCreatedAtMs(value) => value,
        };
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*) AS total_calls,
                COALESCE(SUM(CASE WHEN outcome IN ('ok', 'noop') THEN 1 ELSE 0 END), 0) AS successful_calls,
                COALESCE(SUM(CASE WHEN outcome = 'failed' THEN 1 ELSE 0 END), 0) AS failed_calls,
                COALESCE(SUM(CASE WHEN route_kind = 'error' THEN 1 ELSE 0 END), 0) AS invalid_route_errors,
                COALESCE(SUM(CASE WHEN route_kind = 'deterministic' THEN 1 ELSE 0 END), 0) AS deterministic_routes,
                COALESCE(SUM(CASE WHEN route_kind = 'learned_rule' THEN 1 ELSE 0 END), 0) AS learned_rule_routes,
                COALESCE(SUM(CASE WHEN route_kind IN ('spark', 'spark_script') THEN 1 ELSE 0 END), 0) AS spark_routes,
                COALESCE(SUM(CASE WHEN route_kind = 'spark_script' THEN 1 ELSE 0 END), 0) AS spark_script_fallbacks,
                COALESCE(SUM(CASE WHEN fanout_call_count > 1 THEN 1 ELSE 0 END), 0) AS fanout_routes,
                COALESCE(SUM(fanout_call_count), 0) AS total_fanout_calls,
                COALESCE(SUM(visible_router_schema_tokens), 0) AS visible_router_schema_tokens,
                COALESCE(SUM(hidden_tool_schema_tokens), 0) AS hidden_tool_schema_tokens,
                COALESCE(SUM(spark_prompt_tokens), 0) AS spark_prompt_tokens,
                COALESCE(SUM(spark_completion_tokens), 0) AS spark_completion_tokens,
                COALESCE(SUM(returned_output_tokens), 0) AS returned_output_tokens
            FROM tool_router_ledger
            WHERE created_at_ms >= ?
            "#,
        )
        .bind(since_created_at_ms)
        .fetch_one(self.pool.as_ref())
        .await?;
        let rules_row = sqlx::query(
            r#"
            SELECT
                COUNT(*) AS learned_rule_count,
                COALESCE(SUM(hit_count), 0) AS learned_rule_hits
            FROM tool_router_rules
            "#,
        )
        .fetch_one(self.pool.as_ref())
        .await?;
        let total_calls = row.try_get("total_calls")?;
        let successful_calls = row.try_get("successful_calls")?;
        let learned_rule_routes = row.try_get("learned_rule_routes")?;

        Ok(ToolRouterDiagnosticsSummary {
            total_calls,
            successful_calls,
            failed_calls: row.try_get("failed_calls")?,
            invalid_route_errors: row.try_get("invalid_route_errors")?,
            deterministic_routes: row.try_get("deterministic_routes")?,
            learned_rule_routes,
            spark_routes: row.try_get("spark_routes")?,
            spark_script_fallbacks: row.try_get("spark_script_fallbacks")?,
            fanout_routes: row.try_get("fanout_routes")?,
            total_fanout_calls: row.try_get("total_fanout_calls")?,
            visible_router_schema_tokens: row.try_get("visible_router_schema_tokens")?,
            hidden_tool_schema_tokens: row.try_get("hidden_tool_schema_tokens")?,
            spark_prompt_tokens: row.try_get("spark_prompt_tokens")?,
            spark_completion_tokens: row.try_get("spark_completion_tokens")?,
            returned_output_tokens: row.try_get("returned_output_tokens")?,
            success_rate_basis_points: basis_points(successful_calls, total_calls),
            learned_rule_count: rules_row.try_get("learned_rule_count")?,
            learned_rule_hits: rules_row.try_get("learned_rule_hits")?,
            learned_rule_hit_rate_basis_points: basis_points(learned_rule_routes, total_calls),
        })
    }

    pub async fn prune_tool_router_rules(
        &self,
        options: ToolRouterRulePruneOptions,
    ) -> anyhow::Result<ToolRouterRulePruneResult> {
        let stale_rules_pruned = self
            .prune_stale_tool_router_rules(options.max_rule_age_ms)
            .await?;
        let invalid_rules_pruned = self
            .prune_invalid_tool_router_rules(&options.valid_tools)
            .await?;
        let over_limit_rules_pruned = self
            .prune_excess_tool_router_rules(options.max_rule_count)
            .await?;

        Ok(ToolRouterRulePruneResult {
            stale_rules_pruned,
            invalid_rules_pruned,
            over_limit_rules_pruned,
        })
    }

    async fn prune_stale_tool_router_rules(&self, max_rule_age_ms: i64) -> anyhow::Result<i64> {
        let cutoff_ms = Utc::now()
            .timestamp_millis()
            .saturating_sub(max_rule_age_ms);
        let result = sqlx::query(
            r#"
            DELETE FROM tool_router_rules
            WHERE updated_at_ms < ?
            "#,
        )
        .bind(cutoff_ms)
        .execute(self.pool.as_ref())
        .await?;
        Ok(rows_affected_i64(result.rows_affected()))
    }

    async fn prune_invalid_tool_router_rules(
        &self,
        valid_tools: &BTreeSet<String>,
    ) -> anyhow::Result<i64> {
        let rows = sqlx::query(
            r#"
            SELECT id, match_key, route_json
            FROM tool_router_rules
            "#,
        )
        .fetch_all(self.pool.as_ref())
        .await?;
        let mut pruned = 0;
        for row in rows {
            let id: i64 = row.try_get("id")?;
            let match_key: String = row.try_get("match_key")?;
            let route_json: String = row.try_get("route_json")?;
            if learned_rule_is_valid(match_key.as_str(), route_json.as_str(), valid_tools) {
                continue;
            }
            let result = sqlx::query("DELETE FROM tool_router_rules WHERE id = ?")
                .bind(id)
                .execute(self.pool.as_ref())
                .await?;
            pruned += rows_affected_i64(result.rows_affected());
        }
        Ok(pruned)
    }

    async fn prune_excess_tool_router_rules(&self, max_rule_count: i64) -> anyhow::Result<i64> {
        let rule_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tool_router_rules")
            .fetch_one(self.pool.as_ref())
            .await?;
        let max_rule_count = max_rule_count.max(0);
        if rule_count <= max_rule_count {
            return Ok(0);
        }
        let excess_count = rule_count - max_rule_count;

        let result = sqlx::query(
            r#"
            DELETE FROM tool_router_rules
            WHERE id IN (
                SELECT id
                FROM tool_router_rules
                ORDER BY hit_count ASC, COALESCE(last_hit_at_ms, 0) ASC, updated_at_ms ASC
                LIMIT ?
            )
            "#,
        )
        .bind(excess_count)
        .execute(self.pool.as_ref())
        .await?;
        Ok(rows_affected_i64(result.rows_affected()))
    }
}

fn learned_rule_is_valid(
    match_key: &str,
    route_json: &str,
    valid_tools: &BTreeSet<String>,
) -> bool {
    if !learned_rule_match_key_is_bounded(match_key) {
        return false;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(route_json) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("route") => object
            .get("tool")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|tool| learned_rule_tool_is_valid(tool, valid_tools)),
        Some("fanout") => {
            let Some(calls) = object.get("calls").and_then(serde_json::Value::as_array) else {
                return false;
            };
            !calls.is_empty()
                && calls.iter().all(|call| {
                    call.get("tool")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|tool| learned_rule_tool_is_valid(tool, valid_tools))
                })
        }
        Some("script" | "no_route" | "rule") | None | Some(_) => false,
    }
}

fn learned_rule_match_key_is_bounded(match_key: &str) -> bool {
    match_key.starts_with("v1|")
        && match_key.len() <= TOOL_ROUTER_RULE_MATCH_KEY_MAX_LEN
        && !match_key.chars().any(|ch| matches!(ch, '\n' | '\r' | '\t'))
}

fn learned_rule_tool_is_valid(tool: &str, valid_tools: &BTreeSet<String>) -> bool {
    tool != TOOL_ROUTER_TOOL_NAME && valid_tools.contains(tool)
}

fn basis_points(numerator: i64, denominator: i64) -> i64 {
    if denominator == 0 {
        0
    } else {
        numerator.saturating_mul(10_000) / denominator
    }
}

fn rows_affected_i64(rows: u64) -> i64 {
    i64::try_from(rows).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_support::unique_temp_dir;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn upsert_remembered_tool_increments_request_count_and_keeps_created_at() {
        let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
            .await
            .expect("state runtime");

        let key = ToolRouterRememberedToolKey {
            repo_key: "/repo".to_string(),
            task_key: "chat.default".to_string(),
            tool_namespace: TOOL_ROUTER_REMEMBERED_TOOL_NAMESPACE_SENTINEL.to_string(),
            tool_name: "apply_patch".to_string(),
        };

        runtime
            .upsert_tool_router_remembered_tool(key.clone())
            .await
            .expect("first upsert");
        let first = runtime
            .list_tool_router_remembered_tools("/repo", "chat.default", i64::MIN)
            .await
            .expect("first lookup");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].request_count, 1);

        runtime
            .upsert_tool_router_remembered_tool(key)
            .await
            .expect("second upsert");
        let second = runtime
            .list_tool_router_remembered_tools("/repo", "chat.default", i64::MIN)
            .await
            .expect("second lookup");

        assert_eq!(second.len(), 1);
        assert_eq!(second[0].created_at_ms, first[0].created_at_ms);
        assert_eq!(second[0].request_count, 2);
        assert!(second[0].updated_at_ms >= first[0].updated_at_ms);
    }

    #[tokio::test]
    async fn records_tool_router_ledger_entry_and_summarizes() {
        let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .record_tool_router_ledger_entry(ledger_entry("call", "deterministic", Some("ok")))
            .await
            .expect("record ledger entry");

        let summary = runtime
            .tool_router_diagnostics_summary(ToolRouterDiagnosticsWindow::AllTime)
            .await
            .expect("summary");

        assert_eq!(
            summary,
            ToolRouterDiagnosticsSummary {
                total_calls: 1,
                successful_calls: 1,
                failed_calls: 0,
                invalid_route_errors: 0,
                deterministic_routes: 1,
                learned_rule_routes: 0,
                spark_routes: 0,
                spark_script_fallbacks: 0,
                fanout_routes: 0,
                total_fanout_calls: 1,
                visible_router_schema_tokens: 10,
                hidden_tool_schema_tokens: 0,
                spark_prompt_tokens: 0,
                spark_completion_tokens: 0,
                returned_output_tokens: 7,
                success_rate_basis_points: 10_000,
                learned_rule_count: 0,
                learned_rule_hits: 0,
                learned_rule_hit_rate_basis_points: 0,
            }
        );

        let future_summary = runtime
            .tool_router_diagnostics_summary(ToolRouterDiagnosticsWindow::SinceCreatedAtMs(
                i64::MAX,
            ))
            .await
            .expect("future summary");
        assert_eq!(
            future_summary,
            ToolRouterDiagnosticsSummary {
                total_calls: 0,
                successful_calls: 0,
                failed_calls: 0,
                invalid_route_errors: 0,
                deterministic_routes: 0,
                learned_rule_routes: 0,
                spark_routes: 0,
                spark_script_fallbacks: 0,
                fanout_routes: 0,
                total_fanout_calls: 0,
                visible_router_schema_tokens: 0,
                hidden_tool_schema_tokens: 0,
                spark_prompt_tokens: 0,
                spark_completion_tokens: 0,
                returned_output_tokens: 0,
                success_rate_basis_points: 0,
                learned_rule_count: 0,
                learned_rule_hits: 0,
                learned_rule_hit_rate_basis_points: 0,
            }
        );
    }

    #[tokio::test]
    async fn guidance_upsert_replaces_existing_record_for_same_toolset() {
        let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
            .await
            .expect("state runtime");
        let key = ToolRouterGuidanceKey {
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
        };
        let first = ToolRouterGuidanceRecord {
            key: key.clone(),
            guidance_version: 1,
            guidance_text: "Prefer shell for shell tasks.".to_string(),
            guidance_tokens: 7,
            source: "test".to_string(),
        };
        runtime
            .upsert_tool_router_guidance(first.clone())
            .await
            .expect("first guidance upsert");
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&key)
                .await
                .expect("first guidance lookup"),
            Some(first)
        );

        let second = ToolRouterGuidanceRecord {
            key: key.clone(),
            guidance_version: 2,
            guidance_text: "Prefer apply_patch for code edits.".to_string(),
            guidance_tokens: 8,
            source: "tune".to_string(),
        };
        runtime
            .upsert_tool_router_guidance(second.clone())
            .await
            .expect("second guidance upsert");
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&key)
                .await
                .expect("second guidance lookup"),
            Some(second)
        );

        let missing_key = ToolRouterGuidanceKey {
            toolset_hash: "different".to_string(),
            ..key
        };
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&missing_key)
                .await
                .expect("missing guidance lookup"),
            None
        );
    }

    #[tokio::test]
    async fn prune_rules_removes_invalid_and_keeps_valid_routes() {
        let runtime = StateRuntime::init(unique_temp_dir(), "test".to_string())
            .await
            .expect("state runtime");
        runtime
            .upsert_tool_router_rule(
                "v1|where=shell|action=exec",
                r#"{"type":"route","tool":"exec_command"}"#,
                "test",
            )
            .await
            .expect("valid rule");
        runtime
            .upsert_tool_router_rule(
                "v1|where=shell|action=bad",
                r#"{"type":"route","tool":"missing"}"#,
                "test",
            )
            .await
            .expect("invalid rule");

        let result = runtime
            .prune_tool_router_rules(ToolRouterRulePruneOptions {
                valid_tools: BTreeSet::from(["exec_command".to_string()]),
                max_rule_age_ms: i64::MAX,
                max_rule_count: 100,
            })
            .await
            .expect("prune");

        assert_eq!(
            result,
            ToolRouterRulePruneResult {
                stale_rules_pruned: 0,
                invalid_rules_pruned: 1,
                over_limit_rules_pruned: 0,
            }
        );
        assert!(
            runtime
                .lookup_tool_router_rule("v1|where=shell|action=exec")
                .await
                .expect("lookup valid")
                .is_some()
        );
        assert!(
            runtime
                .lookup_tool_router_rule("v1|where=shell|action=bad")
                .await
                .expect("lookup invalid")
                .is_none()
        );
    }

    fn ledger_entry(
        call_id: &str,
        route_kind: &str,
        outcome: Option<&str>,
    ) -> ToolRouterLedgerEntry {
        ToolRouterLedgerEntry {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: call_id.to_string(),
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
            model_response_ordinal: 0,
            guidance_version: 0,
            guidance_tokens: 0,
            format_description_tokens: 0,
            route_kind: route_kind.to_string(),
            selected_tools: vec!["exec_command".to_string()],
            visible_router_schema_tokens: 10,
            hidden_tool_schema_tokens: 0,
            spark_prompt_tokens: 0,
            spark_completion_tokens: 0,
            fanout_call_count: 1,
            returned_output_tokens: 7,
            original_output_tokens: 7,
            truncated_output_tokens: 0,
            outcome: outcome.map(str::to_string),
            request_shape_json: None,
            tool_call_source: Some("direct".to_string()),
            tool_name: Some("exec_command".to_string()),
            tool_namespace: None,
            tool_input_json: Some(r#"{"cmd":"pwd"}"#.to_string()),
            tool_output_json: Some(r#"{"type":"function_call_output"}"#.to_string()),
            tool_success: Some(true),
            prompt_json: None,
            previous_prompt_json: None,
            dialog_locator_json: None,
        }
    }
}
