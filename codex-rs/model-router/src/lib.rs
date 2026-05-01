use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouterRequestKind {
    Production,
    Shadow,
    CanaryExtra,
    BenchmarkProbe,
    ModelSelfAssessment,
    Judge,
    Verifier,
}

impl RouterRequestKind {
    pub const fn is_router_overhead(self) -> bool {
        match self {
            Self::Production => false,
            Self::Shadow
            | Self::CanaryExtra
            | Self::BenchmarkProbe
            | Self::ModelSelfAssessment
            | Self::Judge
            | Self::Verifier => true,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Shadow => "shadow",
            Self::CanaryExtra => "canary_extra",
            Self::BenchmarkProbe => "benchmark_probe",
            Self::ModelSelfAssessment => "model_self_assessment",
            Self::Judge => "judge",
            Self::Verifier => "verifier",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
pub struct TokenPrice {
    /// Price in USD per one million non-cached input tokens.
    pub input_per_million: f64,
    /// Price in USD per one million cached input tokens.
    pub cached_input_per_million: f64,
    /// Price in USD per one million output tokens.
    pub output_per_million: f64,
    /// Price in USD per one million reasoning output tokens when billed separately.
    ///
    /// When unset, reasoning tokens are treated as regular output tokens.
    pub reasoning_output_per_million: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
pub struct CostEstimate {
    /// Estimated cost in millionths of a US dollar.
    pub usd_micros: i64,
    /// Confidence in the estimate, from 0.0 to 1.0.
    pub confidence: f64,
}

impl CostEstimate {
    pub fn zero_with_confidence(confidence: f64) -> Self {
        Self {
            usd_micros: 0,
            confidence: clamp_confidence(confidence),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RouterSavings {
    pub actual_production_cost_usd_micros: i64,
    pub router_overhead_cost_usd_micros: i64,
    pub counterfactual_cost_usd_micros: i64,
    pub gross_savings_usd_micros: i64,
    pub net_savings_usd_micros: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouterTaskClass {
    LatencySensitive,
    QualitySensitive,
    RareQualitySensitive,
    CostSensitive,
    Balanced,
}

impl RouterTaskClass {
    pub fn infer(task_key: &str, prompt_bytes: usize) -> Self {
        let key = task_key.to_ascii_lowercase();
        if key.contains("learn") {
            return Self::RareQualitySensitive;
        }
        if key.contains("review")
            || key.contains("fix")
            || key.contains("commit")
            || key.contains("memory_consolidation")
        {
            return Self::QualitySensitive;
        }
        if key.contains("tool") || key.contains("mcp") || key.contains("triage") {
            return Self::LatencySensitive;
        }
        if key.contains("compact") || prompt_bytes > 256_000 {
            return Self::CostSensitive;
        }
        if prompt_bytes <= 8_192 {
            return Self::LatencySensitive;
        }
        Self::Balanced
    }

    const fn profile(self) -> RouteProfile {
        match self {
            Self::LatencySensitive => RouteProfile {
                quality_weight: 0.20,
                cost_weight: 0.15,
                latency_weight: 0.55,
                reliability_weight: 0.10,
                quality_floor: 0.35,
                max_latency_ms: Some(30_000),
            },
            Self::QualitySensitive => RouteProfile {
                quality_weight: 0.65,
                cost_weight: 0.15,
                latency_weight: 0.05,
                reliability_weight: 0.15,
                quality_floor: 0.70,
                max_latency_ms: Some(180_000),
            },
            Self::RareQualitySensitive => RouteProfile {
                quality_weight: 0.80,
                cost_weight: 0.0,
                latency_weight: 0.0,
                reliability_weight: 0.20,
                quality_floor: 0.75,
                max_latency_ms: Some(300_000),
            },
            Self::CostSensitive => RouteProfile {
                quality_weight: 0.30,
                cost_weight: 0.45,
                latency_weight: 0.05,
                reliability_weight: 0.20,
                quality_floor: 0.55,
                max_latency_ms: Some(180_000),
            },
            Self::Balanced => RouteProfile {
                quality_weight: 0.40,
                cost_weight: 0.25,
                latency_weight: 0.15,
                reliability_weight: 0.20,
                quality_floor: 0.50,
                max_latency_ms: Some(120_000),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RouteProfile {
    quality_weight: f64,
    cost_weight: f64,
    latency_weight: f64,
    reliability_weight: f64,
    quality_floor: f64,
    max_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
pub struct CandidateMetrics {
    /// External benchmark or router-observed quality score, normalized from 0.0 to 1.0.
    pub intelligence_score: Option<f64>,
    /// Router-observed successful completion rate, normalized from 0.0 to 1.0.
    pub success_rate: Option<f64>,
    /// Router-observed or configured median end-to-end latency.
    pub median_latency_ms: Option<u64>,
    /// Estimated request cost for this task in millionths of a US dollar.
    pub estimated_cost_usd_micros: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CandidateRoute {
    pub id: Option<String>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    /// Effective context window available to this route, after reserving model-specific headroom.
    /// Candidates with an unknown limit remain eligible.
    pub usable_context_window_tokens: Option<i64>,
    pub is_incumbent: bool,
    pub metrics: CandidateMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct CandidateSelection {
    pub index: usize,
    pub score: f64,
    pub task_class: RouterTaskClass,
}

pub fn select_candidate(
    task_key: &str,
    prompt_bytes: usize,
    candidates: &[CandidateRoute],
) -> Option<CandidateSelection> {
    if candidates.is_empty() {
        return None;
    }
    let task_class = RouterTaskClass::infer(task_key, prompt_bytes);
    let profile = task_class.profile();
    let estimated_task_usage = estimate_task_usage(prompt_bytes, task_class);
    let mut enriched = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let metrics = infer_missing_metrics(candidate);
        if !fits_context_window(candidate, estimated_task_usage.total_tokens) {
            enriched.push(None);
            continue;
        }
        if let Some(max_latency_ms) = profile.max_latency_ms
            && metrics
                .median_latency_ms
                .is_some_and(|latency_ms| latency_ms > max_latency_ms)
        {
            enriched.push(None);
            continue;
        }
        if metrics
            .intelligence_score
            .is_some_and(|quality| quality < profile.quality_floor)
        {
            enriched.push(None);
            continue;
        }
        enriched.push(Some(metrics));
    }

    let cost_range = metric_range(enriched.iter().flatten().filter_map(|m| {
        m.estimated_cost_usd_micros
            .filter(|cost| *cost >= 0)
            .map(|cost| cost as f64)
    }));
    let latency_range = metric_range(
        enriched
            .iter()
            .flatten()
            .filter_map(|m| m.median_latency_ms.map(|latency| latency as f64)),
    );

    enriched
        .iter()
        .enumerate()
        .filter_map(|(index, metrics)| {
            let metrics = metrics.as_ref()?;
            let quality = metrics.intelligence_score.unwrap_or(0.5);
            let reliability = metrics.success_rate.unwrap_or(0.90);
            let cost = metrics
                .estimated_cost_usd_micros
                .filter(|cost| *cost >= 0)
                .map(|cost| normalized_inverse(cost as f64, cost_range))
                .unwrap_or(0.5);
            let latency = metrics
                .median_latency_ms
                .map(|latency| normalized_inverse(latency as f64, latency_range))
                .unwrap_or(0.5);
            let score = profile.quality_weight * quality
                + profile.cost_weight * cost
                + profile.latency_weight * latency
                + profile.reliability_weight * reliability;
            Some(CandidateSelection {
                index,
                score,
                task_class,
            })
        })
        .max_by(|left, right| {
            left.score.total_cmp(&right.score).then_with(|| {
                candidates[left.index]
                    .is_incumbent
                    .cmp(&candidates[right.index].is_incumbent)
            })
        })
}

fn fits_context_window(candidate: &CandidateRoute, estimated_total_tokens: i64) -> bool {
    let Some(usable_context_window_tokens) = candidate.usable_context_window_tokens else {
        return true;
    };
    usable_context_window_tokens > 0 && estimated_total_tokens <= usable_context_window_tokens
}

pub fn estimate_token_cost(
    usage: &TokenUsage,
    price: &TokenPrice,
    confidence: f64,
) -> CostEstimate {
    let non_cached_input_tokens = usage.non_cached_input().max(0) as f64;
    let cached_input_tokens = usage.cached_input().max(0) as f64;
    let output_tokens = usage.output_tokens.max(0) as f64;
    let reasoning_output_tokens = usage.reasoning_output_tokens.max(0) as f64;
    let regular_output_tokens = if price.reasoning_output_per_million.is_some() {
        (output_tokens - reasoning_output_tokens).max(0.0)
    } else {
        output_tokens
    };
    let reasoning_price = price
        .reasoning_output_per_million
        .unwrap_or(price.output_per_million);
    let usd = (non_cached_input_tokens * price.input_per_million
        + cached_input_tokens * price.cached_input_per_million
        + regular_output_tokens * price.output_per_million
        + reasoning_output_tokens * reasoning_price)
        / 1_000_000.0;
    CostEstimate {
        usd_micros: (usd * 1_000_000.0).round() as i64,
        confidence: clamp_confidence(confidence),
    }
}

pub fn summarize_savings(
    actual_production_cost_usd_micros: i64,
    router_overhead_cost_usd_micros: i64,
    counterfactual_cost_usd_micros: i64,
) -> RouterSavings {
    let gross_savings_usd_micros =
        counterfactual_cost_usd_micros - actual_production_cost_usd_micros;
    RouterSavings {
        actual_production_cost_usd_micros,
        router_overhead_cost_usd_micros,
        counterfactual_cost_usd_micros,
        gross_savings_usd_micros,
        net_savings_usd_micros: gross_savings_usd_micros - router_overhead_cost_usd_micros,
    }
}

pub fn estimate_task_usage(prompt_bytes: usize, task_class: RouterTaskClass) -> TokenUsage {
    let input_tokens = i64::try_from(prompt_bytes.div_ceil(4)).unwrap_or(i64::MAX);
    let output_tokens = match task_class {
        RouterTaskClass::LatencySensitive => 400,
        RouterTaskClass::QualitySensitive => 2_000,
        RouterTaskClass::RareQualitySensitive => 3_000,
        RouterTaskClass::CostSensitive => 1_000,
        RouterTaskClass::Balanced => 1_200,
    };
    TokenUsage {
        input_tokens,
        cached_input_tokens: 0,
        output_tokens,
        reasoning_output_tokens: 0,
        total_tokens: input_tokens.saturating_add(output_tokens),
    }
}

fn infer_missing_metrics(candidate: &CandidateRoute) -> CandidateMetrics {
    let model = candidate.model.as_deref().unwrap_or_default();
    CandidateMetrics {
        intelligence_score: candidate
            .metrics
            .intelligence_score
            .or_else(|| inferred_intelligence_score(model)),
        success_rate: candidate.metrics.success_rate,
        median_latency_ms: candidate
            .metrics
            .median_latency_ms
            .or_else(|| inferred_latency_ms(model)),
        estimated_cost_usd_micros: candidate.metrics.estimated_cost_usd_micros,
    }
}

fn inferred_intelligence_score(model: &str) -> Option<f64> {
    let model = model.to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    let score = if model.contains("gpt-5.5") {
        0.98
    } else if model.contains("gpt-5.4") {
        0.94
    } else if model.contains("gpt-5.3") {
        0.90
    } else if model.contains("gpt-5") {
        0.86
    } else if model.contains("gpt-4.1") || model.contains("gpt-4o") {
        0.78
    } else if model.contains("mini") {
        0.62
    } else if model.contains("spark") || model.contains("nano") {
        0.52
    } else {
        0.55
    };
    Some(score)
}

fn inferred_latency_ms(model: &str) -> Option<u64> {
    let model = model.to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    let latency = if model.contains("spark") || model.contains("nano") {
        2_000
    } else if model.contains("mini") {
        4_000
    } else if model.contains("gpt-5.5") {
        45_000
    } else if model.contains("gpt-5.4") {
        30_000
    } else if model.contains("gpt-5.3") {
        18_000
    } else {
        12_000
    };
    Some(latency)
}

fn metric_range(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for value in values {
        min = min.min(value);
        max = max.max(value);
    }
    min.is_finite().then_some((min, max))
}

fn normalized_inverse(value: f64, range: Option<(f64, f64)>) -> f64 {
    let Some((min, max)) = range else {
        return 0.5;
    };
    if (max - min).abs() < f64::EPSILON {
        return 0.5;
    }
    (1.0 - ((value - min) / (max - min))).clamp(0.0, 1.0)
}

fn clamp_confidence(confidence: f64) -> f64 {
    if confidence.is_nan() {
        return 0.0;
    }
    confidence.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use codex_protocol::protocol::TokenUsage;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn estimates_token_cost_with_cached_input_and_reasoning() {
        let usage = TokenUsage {
            input_tokens: 1_500_000,
            cached_input_tokens: 500_000,
            output_tokens: 200_000,
            reasoning_output_tokens: 50_000,
            total_tokens: 1_700_000,
        };
        let price = TokenPrice {
            input_per_million: 2.0,
            cached_input_per_million: 0.2,
            output_per_million: 10.0,
            reasoning_output_per_million: Some(20.0),
        };

        assert_eq!(
            estimate_token_cost(&usage, &price, 1.5),
            CostEstimate {
                usd_micros: 4_600_000,
                confidence: 1.0,
            }
        );
    }

    #[test]
    fn net_savings_subtracts_router_overhead_and_can_be_negative() {
        assert_eq!(
            summarize_savings(
                /*actual_production_cost_usd_micros*/ 100,
                /*router_overhead_cost_usd_micros*/ 50,
                /*counterfactual_cost_usd_micros*/ 125,
            ),
            RouterSavings {
                actual_production_cost_usd_micros: 100,
                router_overhead_cost_usd_micros: 50,
                counterfactual_cost_usd_micros: 125,
                gross_savings_usd_micros: 25,
                net_savings_usd_micros: -25,
            }
        );
    }

    #[test]
    fn latency_sensitive_tasks_prefer_fast_enough_candidate() {
        let candidates = vec![
            CandidateRoute {
                id: Some("incumbent".to_string()),
                model: Some("gpt-5.4".to_string()),
                model_provider: Some("openai".to_string()),
                usable_context_window_tokens: Some(100_000),
                is_incumbent: true,
                metrics: CandidateMetrics::default(),
            },
            CandidateRoute {
                id: Some("fast".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                model_provider: Some("openai".to_string()),
                usable_context_window_tokens: Some(100_000),
                is_incumbent: false,
                metrics: CandidateMetrics {
                    success_rate: Some(0.95),
                    ..Default::default()
                },
            },
        ];

        assert_eq!(
            select_candidate("module.repo_ci.triage", 1_000, &candidates)
                .map(|selection| (selection.index, selection.task_class)),
            Some((1, RouterTaskClass::LatencySensitive))
        );
    }

    #[test]
    fn rare_quality_tasks_ignore_cost_and_prefer_quality() {
        let candidates = vec![
            CandidateRoute {
                id: Some("cheap".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                model_provider: Some("openai".to_string()),
                usable_context_window_tokens: Some(100_000),
                is_incumbent: false,
                metrics: CandidateMetrics {
                    intelligence_score: Some(0.70),
                    estimated_cost_usd_micros: Some(10),
                    ..Default::default()
                },
            },
            CandidateRoute {
                id: Some("quality".to_string()),
                model: Some("gpt-5.5".to_string()),
                model_provider: Some("openai".to_string()),
                usable_context_window_tokens: Some(100_000),
                is_incumbent: false,
                metrics: CandidateMetrics {
                    estimated_cost_usd_micros: Some(10_000),
                    ..Default::default()
                },
            },
        ];

        assert_eq!(
            select_candidate("module.repo_ci.learn", 100_000, &candidates)
                .map(|selection| { (selection.index, selection.task_class) }),
            Some((1, RouterTaskClass::RareQualitySensitive))
        );
    }

    #[test]
    fn skips_candidate_that_cannot_fit_estimated_task_usage() {
        let candidates = vec![
            CandidateRoute {
                id: Some("incumbent".to_string()),
                model: Some("gpt-5.4".to_string()),
                model_provider: Some("openai".to_string()),
                usable_context_window_tokens: Some(100_000),
                is_incumbent: true,
                metrics: CandidateMetrics::default(),
            },
            CandidateRoute {
                id: Some("fast".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                model_provider: Some("openai".to_string()),
                usable_context_window_tokens: Some(1_000),
                is_incumbent: false,
                metrics: CandidateMetrics {
                    success_rate: Some(0.99),
                    ..Default::default()
                },
            },
        ];

        assert_eq!(
            select_candidate("module.repo_ci.triage", 8_000, &candidates)
                .map(|selection| { (selection.index, selection.task_class) }),
            Some((0, RouterTaskClass::LatencySensitive))
        );
    }
}
