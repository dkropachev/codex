use super::ToolRouterOutputOptimizationStatus;
use crate::runtime::StateRuntime;
use sqlx::Row;

impl StateRuntime {
    pub async fn list_accepted_tool_router_output_optimization_keys_for_tool(
        &self,
        model_slug: &str,
        model_provider: &str,
        tool_namespace: &str,
        tool_name: &str,
    ) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT
                suggestion_key,
                MAX(saved_output_tokens) AS max_saved_output_tokens,
                MAX(observation_count) AS max_observation_count
            FROM tool_router_output_optimizations
            WHERE status = ?
              AND model_slug = ?
              AND model_provider = ?
              AND tool_namespace = ?
              AND tool_name = ?
            GROUP BY suggestion_key
            ORDER BY max_saved_output_tokens DESC, max_observation_count DESC, suggestion_key
            "#,
        )
        .bind(ToolRouterOutputOptimizationStatus::Accepted.as_str())
        .bind(model_slug)
        .bind(model_provider)
        .bind(tool_namespace)
        .bind(tool_name)
        .fetch_all(self.pool.as_ref())
        .await?;

        rows.into_iter()
            .map(|row| row.try_get("suggestion_key"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}
