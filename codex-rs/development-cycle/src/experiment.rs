use crate::models::ReviewModelChoice;
use crate::models::lower_effort_candidate;
use crate::models::model_score_key;
use crate::persistence::DevCycleState;
use crate::persistence::ExperimentDecision;
use crate::persistence::ScoreSummary;
use crate::review_types::ReviewTypeDefinition;

const PROMOTION_SCORE: f64 = 0.82;
const SUPPRESSION_SCORE: f64 = 0.35;
const MIN_WORK_SIZE_UNITS: u32 = 25;

pub(crate) fn experiment_decisions(
    state: &DevCycleState,
    repo_family: &str,
    review_types: &[ReviewTypeDefinition],
    model: Option<&ReviewModelChoice>,
    min_evidence_runs: u32,
    effort_lowering_enabled: bool,
) -> anyhow::Result<Vec<ExperimentDecision>> {
    let Some(model) = model else {
        return Ok(review_types
            .iter()
            .map(|review_type| ExperimentDecision {
                review_type_id: review_type.id.clone(),
                model_key: "unavailable".to_string(),
                split_strategy: "grouped".to_string(),
                decision: "collecting".to_string(),
                reason: "no model candidates available from host".to_string(),
            })
            .collect());
    };

    let mut decisions = Vec::new();
    for review_type in review_types {
        let summary = state.score_summary(repo_family, &review_type.id, &model.score_key)?;
        let evidence_ready = summary
            .as_ref()
            .is_some_and(|summary| evidence_ready(summary, min_evidence_runs));
        let (decision, reason) = match summary.as_ref() {
            Some(summary) if evidence_ready && summary.score >= PROMOTION_SCORE => (
                "promote",
                format!(
                    "verifier-only score {:.2} across {} runs / {} work units",
                    summary.score, summary.runs, summary.work_size_units
                ),
            ),
            Some(summary) if evidence_ready && summary.score <= SUPPRESSION_SCORE => (
                "suppress",
                format!(
                    "verifier-only score {:.2} after evidence gate",
                    summary.score
                ),
            ),
            Some(summary) => (
                "collecting",
                format!(
                    "evidence not ready: {} runs / {} work units, score {:.2}",
                    summary.runs, summary.work_size_units, summary.score
                ),
            ),
            None => (
                "collecting",
                format!("waiting for at least {min_evidence_runs} completed runs"),
            ),
        };
        decisions.push(ExperimentDecision {
            review_type_id: review_type.id.clone(),
            model_key: model.score_key.clone(),
            split_strategy: active_split_strategy(summary.as_ref(), min_evidence_runs).to_string(),
            decision: decision.to_string(),
            reason,
        });

        if let Some(lower) = lower_effort_candidate(
            &model.model,
            evidence_ready
                && summary
                    .as_ref()
                    .is_some_and(|summary| summary.score >= PROMOTION_SCORE),
            effort_lowering_enabled,
        ) {
            decisions.push(ExperimentDecision {
                review_type_id: review_type.id.clone(),
                model_key: model_score_key(&lower),
                split_strategy: "grouped".to_string(),
                decision: "challenge".to_string(),
                reason: "high effort is proven good; lower effort is eligible for sampling"
                    .to_string(),
            });
        } else if effort_lowering_enabled {
            decisions.push(ExperimentDecision {
                review_type_id: review_type.id.clone(),
                model_key: model.score_key.clone(),
                split_strategy: "grouped".to_string(),
                decision: "lower_effort_gated".to_string(),
                reason: "lower effort waits until the highest effort has passed the evidence gate"
                    .to_string(),
            });
        }
    }
    Ok(decisions)
}

fn evidence_ready(summary: &ScoreSummary, min_evidence_runs: u32) -> bool {
    summary.runs >= min_evidence_runs && summary.work_size_units >= MIN_WORK_SIZE_UNITS
}

fn active_split_strategy(summary: Option<&ScoreSummary>, min_evidence_runs: u32) -> &'static str {
    match summary {
        Some(summary) if evidence_ready(summary, min_evidence_runs) => "evidence-selected",
        _ => "grouped",
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::persistence::ScoreSummary;

    #[test]
    fn evidence_requires_minimum_runs_and_work_size() {
        assert!(!evidence_ready(
            &ScoreSummary {
                runs: 4,
                work_size_units: 100,
                score: 1.0,
            },
            5
        ));
        assert!(!evidence_ready(
            &ScoreSummary {
                runs: 5,
                work_size_units: 24,
                score: 1.0,
            },
            5
        ));
        assert!(evidence_ready(
            &ScoreSummary {
                runs: 5,
                work_size_units: 25,
                score: 1.0,
            },
            5
        ));
    }

    #[test]
    fn split_strategy_is_deterministic_before_and_after_gate() {
        assert_eq!(active_split_strategy(None, 5), "grouped");
        assert_eq!(
            active_split_strategy(
                Some(&ScoreSummary {
                    runs: 5,
                    work_size_units: 25,
                    score: 1.0,
                }),
                5
            ),
            "evidence-selected"
        );
    }
}
