use codex_native_workflow::NativeWorkflowModelCandidate;
use codex_native_workflow::NativeWorkflowModelSelection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewModelChoice {
    pub model: NativeWorkflowModelSelection,
    pub score_key: String,
}

pub(crate) fn ordered_model_candidates(
    candidates: Vec<NativeWorkflowModelCandidate>,
) -> Vec<NativeWorkflowModelCandidate> {
    let mut candidates = candidates
        .into_iter()
        .filter(|candidate| !candidate.provider_id.is_empty() && !candidate.model.is_empty())
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .intelligence_score
            .partial_cmp(&left.intelligence_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                effort_rank(right.reasoning_effort.as_deref())
                    .cmp(&effort_rank(left.reasoning_effort.as_deref()))
            })
            .then_with(|| left.provider_id.cmp(&right.provider_id))
            .then_with(|| left.model.cmp(&right.model))
    });
    candidates
}

pub(crate) fn primary_review_model(
    candidates: Vec<NativeWorkflowModelCandidate>,
) -> Option<ReviewModelChoice> {
    ordered_model_candidates(candidates)
        .into_iter()
        .next()
        .map(|candidate| {
            let model = NativeWorkflowModelSelection {
                provider_id: candidate.provider_id,
                model: candidate.model,
                reasoning_effort: candidate.reasoning_effort,
            };
            let score_key = model_score_key(&model);
            ReviewModelChoice { model, score_key }
        })
}

pub(crate) fn lower_effort_candidate(
    model: &NativeWorkflowModelSelection,
    high_effort_proven_good: bool,
    effort_lowering_enabled: bool,
) -> Option<NativeWorkflowModelSelection> {
    if !effort_lowering_enabled || !high_effort_proven_good {
        return None;
    }
    let lower = match model.reasoning_effort.as_deref() {
        Some("xhigh") => Some("high"),
        Some("high") => Some("medium"),
        Some("medium") => Some("low"),
        Some("low") => Some("minimal"),
        Some("minimal" | "none") | None => None,
        Some(_) => None,
    }?;
    Some(NativeWorkflowModelSelection {
        provider_id: model.provider_id.clone(),
        model: model.model.clone(),
        reasoning_effort: Some(lower.to_string()),
    })
}

pub(crate) fn model_score_key(model: &NativeWorkflowModelSelection) -> String {
    format!(
        "{}:{}:{}",
        model.provider_id,
        model.model,
        model.reasoning_effort.as_deref().unwrap_or("inherit")
    )
}

fn effort_rank(effort: Option<&str>) -> u8 {
    match effort {
        Some("xhigh") => 5,
        Some("high") => 4,
        Some("medium") => 3,
        Some("low") => 2,
        Some("minimal") => 1,
        Some("none") => 0,
        Some(_) | None => 3,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn candidates_order_by_intelligence_then_highest_effort() {
        let ordered = ordered_model_candidates(vec![
            candidate("provider-b", "model-b", Some("medium"), Some(0.9)),
            candidate("provider-a", "model-a", Some("xhigh"), Some(0.9)),
            candidate("provider-c", "model-c", Some("high"), Some(0.95)),
        ]);

        assert_eq!(
            ordered
                .iter()
                .map(|candidate| candidate.model.as_str())
                .collect::<Vec<_>>(),
            vec!["model-c", "model-a", "model-b"]
        );
    }

    #[test]
    fn lower_effort_waits_for_proven_high_effort() {
        let model = NativeWorkflowModelSelection {
            provider_id: "openai".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: Some("xhigh".to_string()),
        };

        assert_eq!(lower_effort_candidate(&model, false, true), None);
        assert_eq!(
            lower_effort_candidate(&model, true, true),
            Some(NativeWorkflowModelSelection {
                provider_id: "openai".to_string(),
                model: "gpt-5.5".to_string(),
                reasoning_effort: Some("high".to_string()),
            })
        );
    }

    fn candidate(
        provider_id: &str,
        model: &str,
        effort: Option<&str>,
        intelligence_score: Option<f64>,
    ) -> NativeWorkflowModelCandidate {
        NativeWorkflowModelCandidate {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
            reasoning_effort: effort.map(str::to_string),
            intelligence_score,
        }
    }
}
