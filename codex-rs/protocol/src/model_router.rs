//! Shared model-router request classification and savings accounting types.

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RouterSavings {
    pub actual_production_cost_usd_micros: i64,
    pub router_overhead_cost_usd_micros: i64,
    pub counterfactual_cost_usd_micros: i64,
    pub gross_savings_usd_micros: i64,
    pub net_savings_usd_micros: i64,
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
