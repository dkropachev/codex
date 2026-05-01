use chrono::Utc;
use codex_state::StateRuntime;
use codex_state::ToolRouterDiagnosticsWindow;
use codex_state::ToolRouterGuidanceKey;
use codex_state::ToolRouterGuidanceRecord;
use codex_state::ToolRouterTuneObservation;
use codex_tools::compose_tool_router_guidance;
use codex_tools::tool_router_static_guidelines_tokens;
use codex_tools::validate_tool_router_guidance_cap;
use serde::Serialize;

const DYNAMIC_GUIDANCE_VERSION: i64 = 2;

#[derive(Debug, Clone)]
pub struct ToolRouterTuneOptions {
    pub window: String,
    pub model_slug: Option<String>,
    pub max_guidance_tokens: usize,
    pub introspection_model: Option<String>,
    pub apply: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTuneReport {
    pub window: String,
    pub apply: bool,
    pub introspection_model: Option<String>,
    pub introspection_tokens: ToolRouterTuneTokenUsage,
    pub schema_format_tokens: ToolRouterSchemaFormatTokens,
    pub optimizations: Vec<ToolRouterOptimizationReport>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTuneTokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterSchemaFormatTokens {
    pub visible_router_schema_tokens: i64,
    pub hidden_tool_schema_tokens: i64,
    pub format_description_tokens: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterOptimizationReport {
    pub optimization_type: ToolRouterOptimizationType,
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
    pub guidance_version: i64,
    pub guidance_tokens_before: i64,
    pub guidance_tokens_after: i64,
    pub affected_call_count: i64,
    pub per_call_estimated_savings_tokens: i64,
    pub gross_savings_tokens: i64,
    pub allocated_introspection_tokens: i64,
    pub net_savings_tokens: i64,
    pub test_status: ToolRouterOptimizationTestStatus,
    pub persisted: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRouterOptimizationType {
    DropStaticGuidance,
    DynamicGuidance,
    FormatDescriptionRefresh,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRouterOptimizationTestStatus {
    Passing,
    Failing,
}

pub async fn tune_tool_router(
    state_db: &StateRuntime,
    options: ToolRouterTuneOptions,
) -> anyhow::Result<ToolRouterTuneReport> {
    validate_tool_router_guidance_cap(options.max_guidance_tokens).map_err(anyhow::Error::msg)?;
    let window = parse_window(&options.window)?;
    let observations = state_db
        .tool_router_tune_observations(window, options.model_slug.as_deref())
        .await?;
    let introspection_tokens = ToolRouterTuneTokenUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut optimizations = Vec::new();
    for observation in &observations {
        optimizations.extend(optimizations_for_observation(observation, &options));
    }
    allocate_introspection_tokens(&mut optimizations, introspection_tokens.total_tokens);
    if options.apply {
        persist_passing_dynamic_guidance(state_db, &mut optimizations).await?;
    }

    Ok(ToolRouterTuneReport {
        window: options.window,
        apply: options.apply,
        introspection_model: options.introspection_model,
        introspection_tokens,
        schema_format_tokens: schema_format_tokens(&observations),
        optimizations,
    })
}

fn optimizations_for_observation(
    observation: &ToolRouterTuneObservation,
    options: &ToolRouterTuneOptions,
) -> Vec<ToolRouterOptimizationReport> {
    let mut optimizations = Vec::new();
    let static_tokens = i64::try_from(tool_router_static_guidelines_tokens()).unwrap_or(i64::MAX);
    optimizations.push(report(OptimizationDraft {
        optimization_type: ToolRouterOptimizationType::DropStaticGuidance,
        observation,
        guidance_tokens_before: observation.guidance_tokens.saturating_add(static_tokens),
        guidance_tokens_after: observation.guidance_tokens,
        affected_call_count: observation.affected_call_count,
        per_call_estimated_savings_tokens: static_tokens,
        test_status: ToolRouterOptimizationTestStatus::Passing,
        message: "Static Tool Guidelines are removed from bundled base instructions only when tool_router is active.".to_string(),
    }));

    if observation.fallback_call_count > 0 || observation.invalid_route_errors > 0 {
        let dynamic_guidance = dynamic_guidance_for_observation(observation);
        let composed = compose_tool_router_guidance(
            Some(dynamic_guidance.as_str()),
            options.max_guidance_tokens,
        );
        let dynamic_tokens = i64::try_from(composed.tokens).unwrap_or(i64::MAX);
        let test_status = if composed.dynamic_guidance_accepted {
            ToolRouterOptimizationTestStatus::Passing
        } else {
            ToolRouterOptimizationTestStatus::Failing
        };
        let fallback_token_total = observation
            .fallback_prompt_tokens
            .saturating_add(observation.fallback_completion_tokens);
        let affected_calls = observation
            .fallback_call_count
            .saturating_add(observation.invalid_route_errors);
        let per_call_savings = if observation.fallback_call_count > 0 {
            fallback_token_total / observation.fallback_call_count
        } else {
            0
        };
        optimizations.push(report(OptimizationDraft {
            optimization_type: ToolRouterOptimizationType::DynamicGuidance,
            observation,
            guidance_tokens_before: observation.guidance_tokens,
            guidance_tokens_after: dynamic_tokens,
            affected_call_count: affected_calls,
            per_call_estimated_savings_tokens: per_call_savings,
            test_status,
            message: dynamic_guidance,
        }));
    }

    optimizations.push(report(OptimizationDraft {
        optimization_type: ToolRouterOptimizationType::FormatDescriptionRefresh,
        observation,
        guidance_tokens_before: observation.guidance_tokens,
        guidance_tokens_after: observation.guidance_tokens,
        affected_call_count: observation.affected_call_count,
        per_call_estimated_savings_tokens: 0,
        test_status: ToolRouterOptimizationTestStatus::Passing,
        message: "Router format description is regenerated from the current tool catalog and is not counted against the guidance cap.".to_string(),
    }));
    optimizations
}

struct OptimizationDraft<'a> {
    optimization_type: ToolRouterOptimizationType,
    observation: &'a ToolRouterTuneObservation,
    guidance_tokens_before: i64,
    guidance_tokens_after: i64,
    affected_call_count: i64,
    per_call_estimated_savings_tokens: i64,
    test_status: ToolRouterOptimizationTestStatus,
    message: String,
}

fn report(draft: OptimizationDraft<'_>) -> ToolRouterOptimizationReport {
    let OptimizationDraft {
        optimization_type,
        observation,
        guidance_tokens_before,
        guidance_tokens_after,
        affected_call_count,
        per_call_estimated_savings_tokens,
        test_status,
        message,
    } = draft;
    let gross_savings_tokens = affected_call_count
        .max(0)
        .saturating_mul(per_call_estimated_savings_tokens.max(0));
    ToolRouterOptimizationReport {
        optimization_type,
        model_slug: observation.model_slug.clone(),
        model_provider: observation.model_provider.clone(),
        toolset_hash: observation.toolset_hash.clone(),
        router_schema_version: observation.router_schema_version,
        guidance_version: match optimization_type {
            ToolRouterOptimizationType::DynamicGuidance => DYNAMIC_GUIDANCE_VERSION,
            ToolRouterOptimizationType::DropStaticGuidance
            | ToolRouterOptimizationType::FormatDescriptionRefresh => 1,
        },
        guidance_tokens_before,
        guidance_tokens_after,
        affected_call_count,
        per_call_estimated_savings_tokens,
        gross_savings_tokens,
        allocated_introspection_tokens: 0,
        net_savings_tokens: gross_savings_tokens,
        test_status,
        persisted: false,
        message,
    }
}

fn dynamic_guidance_for_observation(observation: &ToolRouterTuneObservation) -> String {
    if observation.invalid_route_errors > 0 {
        "Prefer deterministic router fields for this model/toolset: include `action.tool` for known tools, `action.cmd` for shell execution, and path/query targets when routing filesystem or search work.".to_string()
    } else {
        "For this model/toolset, avoid fallback routing by setting `action.tool` when the destination tool is known and by providing concrete `cmd`, `patch`, `query`, or `mcp_args` payloads.".to_string()
    }
}

fn allocate_introspection_tokens(
    optimizations: &mut [ToolRouterOptimizationReport],
    introspection_total_tokens: i64,
) {
    if introspection_total_tokens <= 0 {
        return;
    }
    let gross_total = optimizations
        .iter()
        .map(|optimization| optimization.gross_savings_tokens.max(0))
        .sum::<i64>();
    if gross_total <= 0 {
        return;
    }
    let mut allocated = 0;
    let last_index = optimizations.len().saturating_sub(1);
    for (index, optimization) in optimizations.iter_mut().enumerate() {
        let share = if index == last_index {
            introspection_total_tokens.saturating_sub(allocated)
        } else {
            optimization
                .gross_savings_tokens
                .saturating_mul(introspection_total_tokens)
                / gross_total
        };
        allocated = allocated.saturating_add(share);
        optimization.allocated_introspection_tokens = share;
        optimization.net_savings_tokens = optimization.gross_savings_tokens.saturating_sub(share);
    }
}

async fn persist_passing_dynamic_guidance(
    state_db: &StateRuntime,
    optimizations: &mut [ToolRouterOptimizationReport],
) -> anyhow::Result<()> {
    for optimization in optimizations.iter_mut() {
        if optimization.optimization_type != ToolRouterOptimizationType::DynamicGuidance
            || optimization.test_status != ToolRouterOptimizationTestStatus::Passing
        {
            continue;
        }
        let record = ToolRouterGuidanceRecord {
            key: ToolRouterGuidanceKey {
                model_slug: optimization.model_slug.clone(),
                model_provider: optimization.model_provider.clone(),
                toolset_hash: optimization.toolset_hash.clone(),
                router_schema_version: optimization.router_schema_version,
            },
            guidance_version: optimization.guidance_version,
            guidance_text: optimization.message.clone(),
            guidance_tokens: optimization.guidance_tokens_after,
            source: "codex tool-router tune".to_string(),
        };
        state_db.upsert_tool_router_guidance(record).await?;
        optimization.persisted = true;
    }
    Ok(())
}

fn schema_format_tokens(
    observations: &[ToolRouterTuneObservation],
) -> ToolRouterSchemaFormatTokens {
    let visible_router_schema_tokens = observations
        .iter()
        .map(|observation| observation.visible_router_schema_tokens)
        .max()
        .unwrap_or_default();
    let hidden_tool_schema_tokens = observations
        .iter()
        .map(|observation| observation.hidden_tool_schema_tokens)
        .max()
        .unwrap_or_default();
    ToolRouterSchemaFormatTokens {
        visible_router_schema_tokens,
        hidden_tool_schema_tokens,
        format_description_tokens: observations
            .iter()
            .map(|observation| observation.format_description_tokens)
            .max()
            .unwrap_or_default(),
    }
}

fn parse_window(window: &str) -> anyhow::Result<ToolRouterDiagnosticsWindow> {
    let window = window.trim();
    if window.eq_ignore_ascii_case("all") || window.eq_ignore_ascii_case("all-time") {
        return Ok(ToolRouterDiagnosticsWindow::AllTime);
    }
    let (number, unit) = window.split_at(window.len().saturating_sub(1));
    let value = number
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("window must be a duration like 7d, 24h, 30m, or all"))?;
    let multiplier = match unit {
        "d" => 24 * 60 * 60 * 1000,
        "h" => 60 * 60 * 1000,
        "m" => 60 * 1000,
        _ => {
            return Err(anyhow::anyhow!(
                "window must be a duration like 7d, 24h, 30m, or all"
            ));
        }
    };
    let window_ms = value.max(0).saturating_mul(multiplier);
    Ok(ToolRouterDiagnosticsWindow::SinceCreatedAtMs(
        Utc::now().timestamp_millis().saturating_sub(window_ms),
    ))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use codex_state::ToolRouterLedgerEntry;

    use super::*;

    #[tokio::test]
    async fn dry_run_reports_optimizations_without_persisting() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 25, 5))
            .await
            .expect("record ledger");

        let report = tune_tool_router(&runtime, options(/*apply*/ false, 600))
            .await
            .expect("tune report");

        assert!(report.optimizations.iter().any(|optimization| {
            optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
                && !optimization.persisted
                && optimization.net_savings_tokens >= 0
        }));
        let key = guidance_key();
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&key)
                .await
                .expect("lookup guidance"),
            None
        );
    }

    #[tokio::test]
    async fn apply_persists_only_passing_dynamic_guidance() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record ledger");

        let report = tune_tool_router(&runtime, options(/*apply*/ true, 600))
            .await
            .expect("tune report");

        assert!(report.optimizations.iter().any(|optimization| {
            optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
                && optimization.persisted
        }));
        let record = runtime
            .lookup_tool_router_guidance(&guidance_key())
            .await
            .expect("lookup guidance")
            .expect("guidance record");
        assert_eq!(record.guidance_version, DYNAMIC_GUIDANCE_VERSION);
    }

    #[tokio::test]
    async fn over_cap_dynamic_guidance_is_not_persisted() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record ledger");

        let report = tune_tool_router(&runtime, options(/*apply*/ true, 1))
            .await
            .expect("tune report");

        assert!(report.optimizations.iter().any(|optimization| {
            optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
                && optimization.test_status == ToolRouterOptimizationTestStatus::Failing
                && !optimization.persisted
        }));
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&guidance_key())
                .await
                .expect("lookup guidance"),
            None
        );
    }

    fn options(apply: bool, max_guidance_tokens: usize) -> ToolRouterTuneOptions {
        ToolRouterTuneOptions {
            window: "all".to_string(),
            model_slug: Some("gpt-test".to_string()),
            max_guidance_tokens,
            introspection_model: Some("gpt-introspect".to_string()),
            apply,
        }
    }

    async fn state_runtime() -> (TempDir, std::sync::Arc<StateRuntime>) {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");
        (codex_home, runtime)
    }

    fn guidance_key() -> ToolRouterGuidanceKey {
        ToolRouterGuidanceKey {
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
        }
    }

    fn ledger_entry(
        route_kind: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> ToolRouterLedgerEntry {
        ToolRouterLedgerEntry {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "call".to_string(),
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
            guidance_version: 1,
            guidance_tokens: 10,
            format_description_tokens: 40,
            route_kind: route_kind.to_string(),
            selected_tools: vec!["list_dir".to_string()],
            visible_router_schema_tokens: 12,
            hidden_tool_schema_tokens: 120,
            spark_prompt_tokens: prompt_tokens,
            spark_completion_tokens: completion_tokens,
            fanout_call_count: 1,
            returned_output_tokens: 0,
            original_output_tokens: 0,
            truncated_output_tokens: 0,
            outcome: Some("ok".to_string()),
        }
    }
}
