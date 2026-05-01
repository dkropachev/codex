use crate::runtime::StateRuntime;
use crate::runtime::tool_router::ToolRouterDiagnosticsWindow;
use serde::Serialize;
use sqlx::Row;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTuneCount {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterTuneObservation {
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
    pub affected_call_count: i64,
    pub fallback_call_count: i64,
    pub fallback_prompt_tokens: i64,
    pub fallback_completion_tokens: i64,
    pub invalid_route_errors: i64,
    pub guidance_tokens: i64,
    pub format_description_tokens: i64,
    pub visible_router_schema_tokens: i64,
    pub hidden_tool_schema_tokens: i64,
    pub route_kind_breakdown: Vec<ToolRouterTuneCount>,
    pub selected_tool_breakdown: Vec<ToolRouterTuneCount>,
    pub fallback_tool_breakdown: Vec<ToolRouterTuneCount>,
    pub outcome_breakdown: Vec<ToolRouterTuneCount>,
}

impl StateRuntime {
    pub async fn tool_router_tune_observations(
        &self,
        window: ToolRouterDiagnosticsWindow,
        model_slug: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRouterTuneObservation>> {
        let since_created_at_ms = match window {
            ToolRouterDiagnosticsWindow::AllTime => i64::MIN,
            ToolRouterDiagnosticsWindow::SinceCreatedAtMs(value) => value,
        };
        let rows = sqlx::query(
            r#"
            SELECT
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                COUNT(*) AS affected_call_count,
                COALESCE(SUM(CASE WHEN route_kind IN ('model_router', 'model_router_script', 'spark', 'spark_script') THEN 1 ELSE 0 END), 0) AS fallback_call_count,
                COALESCE(SUM(CASE WHEN route_kind IN ('model_router', 'model_router_script', 'spark', 'spark_script') THEN spark_prompt_tokens ELSE 0 END), 0) AS fallback_prompt_tokens,
                COALESCE(SUM(CASE WHEN route_kind IN ('model_router', 'model_router_script', 'spark', 'spark_script') THEN spark_completion_tokens ELSE 0 END), 0) AS fallback_completion_tokens,
                COALESCE(SUM(CASE WHEN route_kind = 'error' THEN 1 ELSE 0 END), 0) AS invalid_route_errors,
                COALESCE(MAX(guidance_tokens), 0) AS guidance_tokens,
                COALESCE(MAX(format_description_tokens), 0) AS format_description_tokens,
                COALESCE(MAX(visible_router_schema_tokens), 0) AS visible_router_schema_tokens,
                COALESCE(MAX(hidden_tool_schema_tokens), 0) AS hidden_tool_schema_tokens
            FROM tool_router_ledger
            WHERE created_at_ms >= ?
              AND model_slug != ''
              AND (? IS NULL OR model_slug = ?)
            GROUP BY model_slug, model_provider, toolset_hash, router_schema_version
            ORDER BY affected_call_count DESC, fallback_call_count DESC
            "#,
        )
        .bind(since_created_at_ms)
        .bind(model_slug)
        .bind(model_slug)
        .fetch_all(self.pool.as_ref())
        .await?;

        let mut observations = rows
            .into_iter()
            .map(|row| {
                Ok(ToolRouterTuneObservation {
                    model_slug: row.try_get("model_slug")?,
                    model_provider: row.try_get("model_provider")?,
                    toolset_hash: row.try_get("toolset_hash")?,
                    router_schema_version: row.try_get("router_schema_version")?,
                    affected_call_count: row.try_get("affected_call_count")?,
                    fallback_call_count: row.try_get("fallback_call_count")?,
                    fallback_prompt_tokens: row.try_get("fallback_prompt_tokens")?,
                    fallback_completion_tokens: row.try_get("fallback_completion_tokens")?,
                    invalid_route_errors: row.try_get("invalid_route_errors")?,
                    guidance_tokens: row.try_get("guidance_tokens")?,
                    format_description_tokens: row.try_get("format_description_tokens")?,
                    visible_router_schema_tokens: row.try_get("visible_router_schema_tokens")?,
                    hidden_tool_schema_tokens: row.try_get("hidden_tool_schema_tokens")?,
                    route_kind_breakdown: Vec::new(),
                    selected_tool_breakdown: Vec::new(),
                    fallback_tool_breakdown: Vec::new(),
                    outcome_breakdown: Vec::new(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let observation_indexes = observations
            .iter()
            .enumerate()
            .map(|(index, observation)| {
                (
                    (
                        observation.model_slug.clone(),
                        observation.model_provider.clone(),
                        observation.toolset_hash.clone(),
                        observation.router_schema_version,
                    ),
                    index,
                )
            })
            .collect::<BTreeMap<_, _>>();

        let route_rows = sqlx::query(
            r#"
            SELECT
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                route_kind AS name,
                COUNT(*) AS count
            FROM tool_router_ledger
            WHERE created_at_ms >= ?
              AND model_slug != ''
              AND (? IS NULL OR model_slug = ?)
            GROUP BY model_slug, model_provider, toolset_hash, router_schema_version, route_kind
            "#,
        )
        .bind(since_created_at_ms)
        .bind(model_slug)
        .bind(model_slug)
        .fetch_all(self.pool.as_ref())
        .await?;

        for row in route_rows {
            let key = (
                row.try_get("model_slug")?,
                row.try_get("model_provider")?,
                row.try_get("toolset_hash")?,
                row.try_get("router_schema_version")?,
            );
            if let Some(index) = observation_indexes.get(&key) {
                observations[*index]
                    .route_kind_breakdown
                    .push(ToolRouterTuneCount {
                        name: row.try_get("name")?,
                        count: row.try_get("count")?,
                    });
            }
        }

        let outcome_rows = sqlx::query(
            r#"
            SELECT
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                COALESCE(outcome, 'unknown') AS name,
                COUNT(*) AS count
            FROM tool_router_ledger
            WHERE created_at_ms >= ?
              AND model_slug != ''
              AND (? IS NULL OR model_slug = ?)
            GROUP BY model_slug, model_provider, toolset_hash, router_schema_version, COALESCE(outcome, 'unknown')
            "#,
        )
        .bind(since_created_at_ms)
        .bind(model_slug)
        .bind(model_slug)
        .fetch_all(self.pool.as_ref())
        .await?;

        for row in outcome_rows {
            let key = (
                row.try_get("model_slug")?,
                row.try_get("model_provider")?,
                row.try_get("toolset_hash")?,
                row.try_get("router_schema_version")?,
            );
            if let Some(index) = observation_indexes.get(&key) {
                observations[*index]
                    .outcome_breakdown
                    .push(ToolRouterTuneCount {
                        name: row.try_get("name")?,
                        count: row.try_get("count")?,
                    });
            }
        }

        let selected_tool_rows = sqlx::query(
            r#"
            SELECT
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                route_kind,
                selected_tools_json
            FROM tool_router_ledger
            WHERE created_at_ms >= ?
              AND model_slug != ''
              AND (? IS NULL OR model_slug = ?)
            "#,
        )
        .bind(since_created_at_ms)
        .bind(model_slug)
        .bind(model_slug)
        .fetch_all(self.pool.as_ref())
        .await?;
        let mut selected_tool_counts = BTreeMap::new();
        let mut fallback_tool_counts = BTreeMap::new();
        for row in selected_tool_rows {
            let model_slug: String = row.try_get("model_slug")?;
            let model_provider: String = row.try_get("model_provider")?;
            let toolset_hash: String = row.try_get("toolset_hash")?;
            let router_schema_version: i64 = row.try_get("router_schema_version")?;
            let route_kind: String = row.try_get("route_kind")?;
            let selected_tools_json: String = row.try_get("selected_tools_json")?;
            let selected_tools =
                serde_json::from_str::<Vec<String>>(&selected_tools_json).unwrap_or_default();
            for selected_tool in selected_tools
                .into_iter()
                .filter(|selected_tool| !selected_tool.is_empty())
            {
                let key = (
                    model_slug.clone(),
                    model_provider.clone(),
                    toolset_hash.clone(),
                    router_schema_version,
                    selected_tool,
                );
                *selected_tool_counts.entry(key.clone()).or_insert(0) += 1;
                if matches!(
                    route_kind.as_str(),
                    "model_router" | "model_router_script" | "spark" | "spark_script"
                ) {
                    *fallback_tool_counts.entry(key).or_insert(0) += 1;
                }
            }
        }

        for ((model_slug, model_provider, toolset_hash, router_schema_version, name), count) in
            selected_tool_counts
        {
            if let Some(index) = observation_indexes.get(&(
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
            )) {
                observations[*index]
                    .selected_tool_breakdown
                    .push(ToolRouterTuneCount { name, count });
            }
        }
        for ((model_slug, model_provider, toolset_hash, router_schema_version, name), count) in
            fallback_tool_counts
        {
            if let Some(index) = observation_indexes.get(&(
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
            )) {
                observations[*index]
                    .fallback_tool_breakdown
                    .push(ToolRouterTuneCount { name, count });
            }
        }

        for observation in &mut observations {
            sort_tool_router_tune_counts(&mut observation.route_kind_breakdown);
            sort_tool_router_tune_counts(&mut observation.selected_tool_breakdown);
            sort_tool_router_tune_counts(&mut observation.fallback_tool_breakdown);
            sort_tool_router_tune_counts(&mut observation.outcome_breakdown);
        }

        Ok(observations)
    }
}

fn sort_tool_router_tune_counts(counts: &mut [ToolRouterTuneCount]) {
    counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
}

#[cfg(test)]
mod tests {
    use crate::runtime::tool_router::ToolRouterLedgerEntry;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn tune_observations_include_route_tool_and_outcome_breakdowns() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");

        let mut fallback = ledger_entry("call-1", "model_router_script", 1, 11, 4, Some("failed"));
        fallback.selected_tools = vec!["exec_command".to_string()];
        runtime
            .record_tool_router_ledger_entry(fallback)
            .await
            .expect("record fallback");

        let mut deterministic = ledger_entry("call-2", "deterministic", 2, 0, 0, Some("ok"));
        deterministic.selected_tools = vec!["exec_command".to_string(), "tool_search".to_string()];
        runtime
            .record_tool_router_ledger_entry(deterministic)
            .await
            .expect("record deterministic");

        let observations = runtime
            .tool_router_tune_observations(ToolRouterDiagnosticsWindow::AllTime, Some("gpt-test"))
            .await
            .expect("observations");

        assert_eq!(
            observations,
            vec![ToolRouterTuneObservation {
                model_slug: "gpt-test".to_string(),
                model_provider: "openai".to_string(),
                toolset_hash: "abc123".to_string(),
                router_schema_version: 1,
                affected_call_count: 2,
                fallback_call_count: 1,
                fallback_prompt_tokens: 11,
                fallback_completion_tokens: 4,
                invalid_route_errors: 0,
                guidance_tokens: 9,
                format_description_tokens: 20,
                visible_router_schema_tokens: 10,
                hidden_tool_schema_tokens: 100,
                route_kind_breakdown: vec![
                    ToolRouterTuneCount {
                        name: "deterministic".to_string(),
                        count: 1,
                    },
                    ToolRouterTuneCount {
                        name: "model_router_script".to_string(),
                        count: 1,
                    },
                ],
                selected_tool_breakdown: vec![
                    ToolRouterTuneCount {
                        name: "exec_command".to_string(),
                        count: 2,
                    },
                    ToolRouterTuneCount {
                        name: "tool_search".to_string(),
                        count: 1,
                    },
                ],
                fallback_tool_breakdown: vec![ToolRouterTuneCount {
                    name: "exec_command".to_string(),
                    count: 1,
                }],
                outcome_breakdown: vec![
                    ToolRouterTuneCount {
                        name: "failed".to_string(),
                        count: 1,
                    },
                    ToolRouterTuneCount {
                        name: "ok".to_string(),
                        count: 1,
                    },
                ],
            }]
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
