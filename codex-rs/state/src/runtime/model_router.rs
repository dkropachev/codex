use crate::runtime::StateRuntime;
use chrono::Utc;
use codex_model_router::RouterRequestKind;
use codex_model_router::RouterSavings;
use codex_model_router::summarize_savings;
use codex_protocol::protocol::TokenUsage;
use sqlx::Row;

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
}
