use crate::runtime::StateRuntime;
use chrono::Utc;
use sqlx::Row;
use std::collections::BTreeSet;

const TOOL_ROUTER_RULE_MATCH_KEY_MAX_LEN: usize = 1024;
const TOOL_ROUTER_TOOL_NAME: &str = "tool_router";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterLedgerEntry {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
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
                outcome
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                COALESCE(SUM(CASE WHEN route_kind IN ('spark', 'spark_script', 'model_router', 'model_router_script') THEN 1 ELSE 0 END), 0) AS spark_routes,
                COALESCE(SUM(CASE WHEN route_kind IN ('spark_script', 'model_router_script') THEN 1 ELSE 0 END), 0) AS spark_script_fallbacks,
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
    use pretty_assertions::assert_eq;
    use sqlx::Row;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn records_tool_router_ledger_entry() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .record_tool_router_ledger_entry(ToolRouterLedgerEntry {
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                call_id: "call".to_string(),
                model_slug: "gpt-test".to_string(),
                model_provider: "openai".to_string(),
                toolset_hash: "abc123".to_string(),
                router_schema_version: 1,
                guidance_version: 1,
                guidance_tokens: 9,
                format_description_tokens: 20,
                route_kind: "deterministic".to_string(),
                selected_tools: vec!["exec_command".to_string()],
                visible_router_schema_tokens: 10,
                hidden_tool_schema_tokens: 100,
                spark_prompt_tokens: 11,
                spark_completion_tokens: 3,
                fanout_call_count: 1,
                returned_output_tokens: 7,
                original_output_tokens: 9,
                truncated_output_tokens: 7,
                outcome: Some("ok".to_string()),
            })
            .await
            .expect("record ledger entry");

        let row = sqlx::query(
            r#"
            SELECT
                selected_tools_json,
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                guidance_version,
                guidance_tokens,
                format_description_tokens,
                visible_router_schema_tokens,
                hidden_tool_schema_tokens,
                spark_prompt_tokens,
                spark_completion_tokens
            FROM tool_router_ledger
            WHERE call_id = ?
            "#,
        )
        .bind("call")
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("ledger row");

        assert_eq!(
            (
                row.try_get::<String, _>("selected_tools_json")
                    .expect("selected tools"),
                row.try_get::<String, _>("model_slug").expect("model slug"),
                row.try_get::<String, _>("model_provider")
                    .expect("model provider"),
                row.try_get::<String, _>("toolset_hash")
                    .expect("toolset hash"),
                row.try_get::<i64, _>("router_schema_version")
                    .expect("router schema version"),
                row.try_get::<i64, _>("guidance_version")
                    .expect("guidance version"),
                row.try_get::<i64, _>("guidance_tokens")
                    .expect("guidance tokens"),
                row.try_get::<i64, _>("format_description_tokens")
                    .expect("format description tokens"),
                row.try_get::<i64, _>("visible_router_schema_tokens")
                    .expect("visible schema tokens"),
                row.try_get::<i64, _>("hidden_tool_schema_tokens")
                    .expect("hidden schema tokens"),
                row.try_get::<i64, _>("spark_prompt_tokens")
                    .expect("spark prompt tokens"),
                row.try_get::<i64, _>("spark_completion_tokens")
                    .expect("spark completion tokens"),
            ),
            (
                r#"["exec_command"]"#.to_string(),
                "gpt-test".to_string(),
                "openai".to_string(),
                "abc123".to_string(),
                1,
                1,
                9,
                20,
                10,
                100,
                11,
                3,
            )
        );
    }

    #[tokio::test]
    async fn tool_router_ledger_schema_omits_derived_savings_columns() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        let columns = sqlx::query("PRAGMA table_info(tool_router_ledger)")
            .fetch_all(runtime.pool.as_ref())
            .await
            .expect("table info")
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").expect("column name"))
            .collect::<BTreeSet<_>>();

        assert!(columns.contains("visible_router_schema_tokens"));
        assert!(columns.contains("hidden_tool_schema_tokens"));
        assert!(columns.contains("spark_prompt_tokens"));
        assert!(columns.contains("spark_completion_tokens"));
        assert!(columns.contains("model_slug"));
        assert!(columns.contains("toolset_hash"));
        assert!(columns.contains("guidance_tokens"));
        assert!(columns.contains("format_description_tokens"));
        assert!(!columns.contains("estimated_schema_tokens_saved"));
        assert!(!columns.contains("net_tokens_saved"));
    }

    #[tokio::test]
    async fn lookup_does_not_record_tool_router_rule_hit() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#,
                "spark",
            )
            .await
            .expect("upsert rule");

        let rule = runtime
            .lookup_tool_router_rule("v1|where=workspace|action=list")
            .await
            .expect("lookup rule")
            .expect("rule");

        assert_eq!(
            rule,
            ToolRouterLearnedRule {
                match_key: "v1|where=workspace|action=list".to_string(),
                route_json: r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#
                    .to_string(),
                source: "spark".to_string(),
                hit_count: 0,
            }
        );

        let row = sqlx::query("SELECT hit_count FROM tool_router_rules WHERE match_key = ?")
            .bind("v1|where=workspace|action=list")
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("rule row");

        assert_eq!(row.try_get::<i64, _>("hit_count").expect("hits"), 0);
    }

    #[tokio::test]
    async fn records_tool_router_rule_hit_explicitly() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#,
                "spark",
            )
            .await
            .expect("upsert rule");
        runtime
            .record_tool_router_rule_hit("v1|where=workspace|action=list")
            .await
            .expect("record hit");

        let row = sqlx::query("SELECT hit_count FROM tool_router_rules WHERE match_key = ?")
            .bind("v1|where=workspace|action=list")
            .fetch_one(runtime.pool.as_ref())
            .await
            .expect("rule row");

        assert_eq!(row.try_get::<i64, _>("hit_count").expect("hits"), 1);
    }

    #[tokio::test]
    async fn summarizes_tool_router_diagnostics() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        for entry in [
            ledger_entry("call-1", "deterministic", 1, 0, 0, Some("ok")),
            ledger_entry("call-2", "spark_script", 1, 11, 4, Some("failed")),
            ledger_entry("call-3", "learned_rule", 3, 0, 0, Some("ok")),
            ledger_entry("call-4", "error", 0, 0, 0, Some("route_error")),
        ] {
            runtime
                .record_tool_router_ledger_entry(entry)
                .await
                .expect("record ledger entry");
        }
        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#,
                "spark",
            )
            .await
            .expect("upsert rule");
        runtime
            .record_tool_router_rule_hit("v1|where=workspace|action=list")
            .await
            .expect("record hit");

        let summary = runtime
            .tool_router_diagnostics_summary(ToolRouterDiagnosticsWindow::AllTime)
            .await
            .expect("summary");

        assert_eq!(
            summary,
            ToolRouterDiagnosticsSummary {
                total_calls: 4,
                successful_calls: 2,
                failed_calls: 1,
                invalid_route_errors: 1,
                deterministic_routes: 1,
                learned_rule_routes: 1,
                spark_routes: 1,
                spark_script_fallbacks: 1,
                fanout_routes: 1,
                total_fanout_calls: 5,
                visible_router_schema_tokens: 40,
                hidden_tool_schema_tokens: 400,
                spark_prompt_tokens: 11,
                spark_completion_tokens: 4,
                returned_output_tokens: 28,
                success_rate_basis_points: 5000,
                learned_rule_count: 1,
                learned_rule_hits: 1,
                learned_rule_hit_rate_basis_points: 2500,
            }
        );
    }

    #[tokio::test]
    async fn prunes_stale_and_invalid_tool_router_rules() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list|target=keep",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#,
                "spark",
            )
            .await
            .expect("upsert keep rule");
        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list|target=stale",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#,
                "spark",
            )
            .await
            .expect("upsert stale rule");
        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list|target=missing",
                r#"{"type":"route","tool":"missing_tool","arguments":{}}"#,
                "spark",
            )
            .await
            .expect("upsert invalid tool rule");
        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=shell",
                r#"{"type":"script","script":"echo hi"}"#,
                "spark",
            )
            .await
            .expect("upsert script rule");

        sqlx::query("UPDATE tool_router_rules SET updated_at_ms = ? WHERE match_key = ?")
            .bind(Utc::now().timestamp_millis() - 10_000)
            .bind("v1|where=workspace|action=list|target=stale")
            .execute(runtime.pool.as_ref())
            .await
            .expect("age stale rule");

        let result = runtime
            .prune_tool_router_rules(ToolRouterRulePruneOptions {
                valid_tools: BTreeSet::from(["list_dir".to_string()]),
                max_rule_age_ms: 1_000,
                max_rule_count: 10,
            })
            .await
            .expect("prune rules");

        assert_eq!(
            result,
            ToolRouterRulePruneResult {
                stale_rules_pruned: 1,
                invalid_rules_pruned: 2,
                over_limit_rules_pruned: 0,
            }
        );
        let remaining = sqlx::query_scalar::<_, String>("SELECT match_key FROM tool_router_rules")
            .fetch_all(runtime.pool.as_ref())
            .await
            .expect("remaining rules");
        assert_eq!(
            remaining,
            vec!["v1|where=workspace|action=list|target=keep".to_string()]
        );
    }

    #[tokio::test]
    async fn prune_tool_router_rules_respects_count_limit() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list|target=low-hit",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"."}}"#,
                "spark",
            )
            .await
            .expect("upsert low hit rule");
        runtime
            .upsert_tool_router_rule(
                "v1|where=workspace|action=list|target=high-hit",
                r#"{"type":"route","tool":"list_dir","arguments":{"dir_path":"src"}}"#,
                "spark",
            )
            .await
            .expect("upsert high hit rule");
        runtime
            .record_tool_router_rule_hit("v1|where=workspace|action=list|target=high-hit")
            .await
            .expect("record hit");

        let result = runtime
            .prune_tool_router_rules(ToolRouterRulePruneOptions {
                valid_tools: BTreeSet::from(["list_dir".to_string()]),
                max_rule_age_ms: 60_000,
                max_rule_count: 1,
            })
            .await
            .expect("prune rules");

        assert_eq!(
            result,
            ToolRouterRulePruneResult {
                stale_rules_pruned: 0,
                invalid_rules_pruned: 0,
                over_limit_rules_pruned: 1,
            }
        );
        let remaining = sqlx::query_scalar::<_, String>("SELECT match_key FROM tool_router_rules")
            .fetch_all(runtime.pool.as_ref())
            .await
            .expect("remaining rules");
        assert_eq!(
            remaining,
            vec!["v1|where=workspace|action=list|target=high-hit".to_string()]
        );
    }

    fn ledger_entry(
        call_id: &str,
        route_kind: &str,
        fanout_call_count: i64,
        spark_prompt_tokens: i64,
        spark_completion_tokens: i64,
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
            guidance_version: 1,
            guidance_tokens: 9,
            format_description_tokens: 20,
            route_kind: route_kind.to_string(),
            selected_tools: Vec::new(),
            visible_router_schema_tokens: 10,
            hidden_tool_schema_tokens: 100,
            spark_prompt_tokens,
            spark_completion_tokens,
            fanout_call_count,
            returned_output_tokens: 7,
            original_output_tokens: 7,
            truncated_output_tokens: 7,
            outcome: outcome.map(str::to_string),
        }
    }
}
