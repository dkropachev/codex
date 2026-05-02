use crate::runtime::StateRuntime;
use crate::runtime::tool_router::ToolRouterDiagnosticsWindow;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Row;
use std::collections::BTreeMap;

const TOP_TUNE_COUNT_LIMIT: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterRequestShape {
    pub where_kind: String,
    pub action_kind: String,
    pub target_kinds: Vec<String>,
    pub payload_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTuneCount {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterRequestShapeCluster {
    pub shape: ToolRouterRequestShape,
    pub route_kind: String,
    pub outcome: Option<String>,
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
    pub error_outcome_breakdown: Vec<ToolRouterTuneCount>,
    pub learned_rule_hits: i64,
    pub request_shape_clusters: Vec<ToolRouterRequestShapeCluster>,
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
        let learned_rule_hits = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(hit_count), 0) FROM tool_router_rules",
        )
        .fetch_one(self.pool.as_ref())
        .await?;
        let rows = sqlx::query(
            r#"
            SELECT
                model_slug,
                model_provider,
                toolset_hash,
                router_schema_version,
                route_kind,
                selected_tools_json,
                spark_prompt_tokens,
                spark_completion_tokens,
                guidance_tokens,
                format_description_tokens,
                visible_router_schema_tokens,
                hidden_tool_schema_tokens,
                outcome,
                request_shape_json
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

        let mut observations =
            BTreeMap::<ToolRouterTuneObservationKey, ToolRouterTuneObservationBuilder>::new();
        for row in rows {
            let key = ToolRouterTuneObservationKey {
                model_slug: row.try_get("model_slug")?,
                model_provider: row.try_get("model_provider")?,
                toolset_hash: row.try_get("toolset_hash")?,
                router_schema_version: row.try_get("router_schema_version")?,
            };
            let ledger_row = ToolRouterTuneLedgerRow {
                route_kind: row.try_get("route_kind")?,
                selected_tools_json: row.try_get("selected_tools_json")?,
                spark_prompt_tokens: row.try_get("spark_prompt_tokens")?,
                spark_completion_tokens: row.try_get("spark_completion_tokens")?,
                guidance_tokens: row.try_get("guidance_tokens")?,
                format_description_tokens: row.try_get("format_description_tokens")?,
                visible_router_schema_tokens: row.try_get("visible_router_schema_tokens")?,
                hidden_tool_schema_tokens: row.try_get("hidden_tool_schema_tokens")?,
                outcome: row.try_get("outcome")?,
                request_shape_json: row.try_get("request_shape_json")?,
            };
            observations
                .entry(key.clone())
                .or_insert_with(|| ToolRouterTuneObservationBuilder::new(key))
                .record_row(ledger_row);
        }

        let mut observations = observations
            .into_values()
            .map(|builder| builder.build(learned_rule_hits))
            .collect::<Vec<_>>();
        observations.sort_by(|left, right| {
            right
                .affected_call_count
                .cmp(&left.affected_call_count)
                .then_with(|| right.fallback_call_count.cmp(&left.fallback_call_count))
                .then_with(|| left.model_slug.cmp(&right.model_slug))
                .then_with(|| left.model_provider.cmp(&right.model_provider))
                .then_with(|| left.toolset_hash.cmp(&right.toolset_hash))
                .then_with(|| left.router_schema_version.cmp(&right.router_schema_version))
        });
        Ok(observations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolRouterTuneObservationKey {
    model_slug: String,
    model_provider: String,
    toolset_hash: String,
    router_schema_version: i64,
}

struct ToolRouterTuneLedgerRow {
    route_kind: String,
    selected_tools_json: String,
    spark_prompt_tokens: i64,
    spark_completion_tokens: i64,
    guidance_tokens: i64,
    format_description_tokens: i64,
    visible_router_schema_tokens: i64,
    hidden_tool_schema_tokens: i64,
    outcome: Option<String>,
    request_shape_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolRouterRequestShapeClusterKey {
    shape: ToolRouterRequestShape,
    route_kind: String,
    outcome: Option<String>,
}

struct ToolRouterTuneObservationBuilder {
    key: ToolRouterTuneObservationKey,
    affected_call_count: i64,
    fallback_call_count: i64,
    fallback_prompt_tokens: i64,
    fallback_completion_tokens: i64,
    invalid_route_errors: i64,
    guidance_tokens: i64,
    format_description_tokens: i64,
    visible_router_schema_tokens: i64,
    hidden_tool_schema_tokens: i64,
    route_kind_counts: BTreeMap<String, i64>,
    selected_tool_counts: BTreeMap<String, i64>,
    fallback_tool_counts: BTreeMap<String, i64>,
    outcome_counts: BTreeMap<String, i64>,
    error_outcome_counts: BTreeMap<String, i64>,
    request_shape_cluster_counts: BTreeMap<ToolRouterRequestShapeClusterKey, i64>,
}

impl ToolRouterTuneObservationBuilder {
    fn new(key: ToolRouterTuneObservationKey) -> Self {
        Self {
            key,
            affected_call_count: 0,
            fallback_call_count: 0,
            fallback_prompt_tokens: 0,
            fallback_completion_tokens: 0,
            invalid_route_errors: 0,
            guidance_tokens: 0,
            format_description_tokens: 0,
            visible_router_schema_tokens: 0,
            hidden_tool_schema_tokens: 0,
            route_kind_counts: BTreeMap::new(),
            selected_tool_counts: BTreeMap::new(),
            fallback_tool_counts: BTreeMap::new(),
            outcome_counts: BTreeMap::new(),
            error_outcome_counts: BTreeMap::new(),
            request_shape_cluster_counts: BTreeMap::new(),
        }
    }

    fn record_row(&mut self, row: ToolRouterTuneLedgerRow) {
        self.affected_call_count = self.affected_call_count.saturating_add(1);
        *self
            .route_kind_counts
            .entry(row.route_kind.clone())
            .or_default() += 1;
        let is_fallback = tool_router_route_kind_is_fallback(row.route_kind.as_str());
        if is_fallback {
            self.fallback_call_count = self.fallback_call_count.saturating_add(1);
            self.fallback_prompt_tokens = self
                .fallback_prompt_tokens
                .saturating_add(row.spark_prompt_tokens);
            self.fallback_completion_tokens = self
                .fallback_completion_tokens
                .saturating_add(row.spark_completion_tokens);
        }

        let outcome_name = row.outcome.clone().unwrap_or_else(|| "unknown".to_string());
        *self.outcome_counts.entry(outcome_name.clone()).or_default() += 1;
        if row.route_kind == "error" {
            self.invalid_route_errors = self.invalid_route_errors.saturating_add(1);
            *self.error_outcome_counts.entry(outcome_name).or_default() += 1;
        }

        self.guidance_tokens = self.guidance_tokens.max(row.guidance_tokens);
        self.format_description_tokens = self
            .format_description_tokens
            .max(row.format_description_tokens);
        self.visible_router_schema_tokens = self
            .visible_router_schema_tokens
            .max(row.visible_router_schema_tokens);
        self.hidden_tool_schema_tokens = self
            .hidden_tool_schema_tokens
            .max(row.hidden_tool_schema_tokens);

        if let Ok(selected_tools) = serde_json::from_str::<Vec<String>>(&row.selected_tools_json) {
            for selected_tool in selected_tools
                .into_iter()
                .filter(|selected_tool| !selected_tool.is_empty())
            {
                *self
                    .selected_tool_counts
                    .entry(selected_tool.clone())
                    .or_default() += 1;
                if is_fallback {
                    *self.fallback_tool_counts.entry(selected_tool).or_default() += 1;
                }
            }
        }

        if let Some(request_shape_json) = row.request_shape_json.as_deref()
            && let Ok(shape) = serde_json::from_str::<ToolRouterRequestShape>(request_shape_json)
        {
            let key = ToolRouterRequestShapeClusterKey {
                shape,
                route_kind: row.route_kind,
                outcome: row.outcome,
            };
            *self.request_shape_cluster_counts.entry(key).or_default() += 1;
        }
    }

    fn build(self, learned_rule_hits: i64) -> ToolRouterTuneObservation {
        ToolRouterTuneObservation {
            model_slug: self.key.model_slug,
            model_provider: self.key.model_provider,
            toolset_hash: self.key.toolset_hash,
            router_schema_version: self.key.router_schema_version,
            affected_call_count: self.affected_call_count,
            fallback_call_count: self.fallback_call_count,
            fallback_prompt_tokens: self.fallback_prompt_tokens,
            fallback_completion_tokens: self.fallback_completion_tokens,
            invalid_route_errors: self.invalid_route_errors,
            guidance_tokens: self.guidance_tokens,
            format_description_tokens: self.format_description_tokens,
            visible_router_schema_tokens: self.visible_router_schema_tokens,
            hidden_tool_schema_tokens: self.hidden_tool_schema_tokens,
            route_kind_breakdown: top_counts(self.route_kind_counts, TOP_TUNE_COUNT_LIMIT),
            selected_tool_breakdown: top_counts(self.selected_tool_counts, TOP_TUNE_COUNT_LIMIT),
            fallback_tool_breakdown: top_counts(self.fallback_tool_counts, TOP_TUNE_COUNT_LIMIT),
            outcome_breakdown: top_counts(self.outcome_counts, TOP_TUNE_COUNT_LIMIT),
            error_outcome_breakdown: top_counts(self.error_outcome_counts, TOP_TUNE_COUNT_LIMIT),
            learned_rule_hits,
            request_shape_clusters: top_request_shape_clusters(
                self.request_shape_cluster_counts,
                TOP_TUNE_COUNT_LIMIT,
            ),
        }
    }
}

fn tool_router_route_kind_is_fallback(route_kind: &str) -> bool {
    matches!(route_kind, "spark" | "spark_script")
}

fn top_counts(counts: BTreeMap<String, i64>, limit: usize) -> Vec<ToolRouterTuneCount> {
    let mut counts = counts
        .into_iter()
        .map(|(name, count)| ToolRouterTuneCount { name, count })
        .collect::<Vec<_>>();
    counts.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    counts.truncate(limit);
    counts
}

fn top_request_shape_clusters(
    counts: BTreeMap<ToolRouterRequestShapeClusterKey, i64>,
    limit: usize,
) -> Vec<ToolRouterRequestShapeCluster> {
    let mut clusters = counts
        .into_iter()
        .map(|(key, count)| ToolRouterRequestShapeCluster {
            shape: key.shape,
            route_kind: key.route_kind,
            outcome: key.outcome,
            count,
        })
        .collect::<Vec<_>>();
    clusters.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.route_kind.cmp(&right.route_kind))
            .then_with(|| left.outcome.cmp(&right.outcome))
            .then_with(|| left.shape.cmp(&right.shape))
    });
    clusters.truncate(limit);
    clusters
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

        let mut fallback = ledger_entry("call-1", "spark_script", 1, 11, 4, Some("failed"));
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
                        name: "spark_script".to_string(),
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
                error_outcome_breakdown: Vec::new(),
                learned_rule_hits: 0,
                request_shape_clusters: Vec::new(),
            }]
        );
    }

    #[tokio::test]
    async fn tune_observations_include_request_shape_clusters_and_rule_hits() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");
        let shape = ToolRouterRequestShape {
            where_kind: "shell".to_string(),
            action_kind: "exec".to_string(),
            target_kinds: vec!["path".to_string()],
            payload_fields: vec!["cmd".to_string()],
        };
        let shape_json = serde_json::to_string(&shape).expect("shape json");

        let mut fallback = ledger_entry("call-fallback", "spark", 1, 12, 3, Some("ok"));
        fallback.selected_tools = vec!["exec_command".to_string()];
        fallback.request_shape_json = Some(shape_json.clone());
        runtime
            .record_tool_router_ledger_entry(fallback)
            .await
            .expect("record fallback");
        let mut error = ledger_entry("call-error", "error", 0, 0, 0, Some("route_error"));
        error.request_shape_json = Some(shape_json);
        runtime
            .record_tool_router_ledger_entry(error)
            .await
            .expect("record error");
        runtime
            .upsert_tool_router_rule(
                "v1|where=shell|action=exec",
                r#"{"type":"route","tool":"exec_command","arguments":{}}"#,
                "tool_router_tune",
            )
            .await
            .expect("upsert rule");
        runtime
            .record_tool_router_rule_hit("v1|where=shell|action=exec")
            .await
            .expect("record hit");

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
                fallback_prompt_tokens: 12,
                fallback_completion_tokens: 3,
                invalid_route_errors: 1,
                guidance_tokens: 9,
                format_description_tokens: 20,
                visible_router_schema_tokens: 10,
                hidden_tool_schema_tokens: 100,
                route_kind_breakdown: vec![
                    ToolRouterTuneCount {
                        name: "error".to_string(),
                        count: 1,
                    },
                    ToolRouterTuneCount {
                        name: "spark".to_string(),
                        count: 1,
                    },
                ],
                selected_tool_breakdown: vec![ToolRouterTuneCount {
                    name: "exec_command".to_string(),
                    count: 1,
                }],
                fallback_tool_breakdown: vec![ToolRouterTuneCount {
                    name: "exec_command".to_string(),
                    count: 1,
                }],
                outcome_breakdown: vec![
                    ToolRouterTuneCount {
                        name: "ok".to_string(),
                        count: 1,
                    },
                    ToolRouterTuneCount {
                        name: "route_error".to_string(),
                        count: 1,
                    },
                ],
                error_outcome_breakdown: vec![ToolRouterTuneCount {
                    name: "route_error".to_string(),
                    count: 1,
                }],
                learned_rule_hits: 1,
                request_shape_clusters: vec![
                    ToolRouterRequestShapeCluster {
                        shape: shape.clone(),
                        route_kind: "error".to_string(),
                        outcome: Some("route_error".to_string()),
                        count: 1,
                    },
                    ToolRouterRequestShapeCluster {
                        shape,
                        route_kind: "spark".to_string(),
                        outcome: Some("ok".to_string()),
                        count: 1,
                    },
                ],
            }]
        );
    }

    #[tokio::test]
    async fn tune_observations_include_deterministic_noop_and_error_request_shapes() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "test".to_string())
            .await
            .expect("state runtime");
        let shape = ToolRouterRequestShape {
            where_kind: "none".to_string(),
            action_kind: "update_plan".to_string(),
            target_kinds: Vec::new(),
            payload_fields: vec!["input".to_string()],
        };
        let shape_json = serde_json::to_string(&shape).expect("shape json");

        for (call_id, route_kind, outcome) in [
            ("call-deterministic", "deterministic", Some("ok")),
            ("call-noop", "none", Some("noop")),
            ("call-error", "error", Some("route_error")),
        ] {
            let mut entry = ledger_entry(call_id, route_kind, 0, 0, 0, outcome);
            entry.request_shape_json = Some(shape_json.clone());
            runtime
                .record_tool_router_ledger_entry(entry)
                .await
                .expect("record entry");
        }

        let observations = runtime
            .tool_router_tune_observations(ToolRouterDiagnosticsWindow::AllTime, Some("gpt-test"))
            .await
            .expect("observations");

        assert_eq!(
            observations[0].request_shape_clusters,
            vec![
                ToolRouterRequestShapeCluster {
                    shape: shape.clone(),
                    route_kind: "deterministic".to_string(),
                    outcome: Some("ok".to_string()),
                    count: 1,
                },
                ToolRouterRequestShapeCluster {
                    shape: shape.clone(),
                    route_kind: "error".to_string(),
                    outcome: Some("route_error".to_string()),
                    count: 1,
                },
                ToolRouterRequestShapeCluster {
                    shape,
                    route_kind: "none".to_string(),
                    outcome: Some("noop".to_string()),
                    count: 1,
                },
            ]
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
            model_response_ordinal: 2,
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
            request_shape_json: None,
        }
    }
}
