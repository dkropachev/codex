use std::collections::BTreeSet;

use codex_config::config_toml::ModelRouterCandidateToml;
use codex_protocol::openai_models::ModelPreset;

use crate::config::Config;

const SPARK_CANDIDATE_ID: &str = "spark";

pub(crate) fn candidates_from_available_models(
    config: &Config,
    available_models: &[ModelPreset],
) -> Vec<ModelRouterCandidateToml> {
    let mut seen_models = configured_model_slugs(config);
    let mut seen_ids = configured_candidate_ids(config);
    let mut candidates = Vec::new();

    for model in available_models.iter().filter_map(|preset| {
        let model = preset.model.trim();
        (!model.is_empty()).then_some(model)
    }) {
        if !seen_models.insert(model.to_string()) {
            continue;
        }
        candidates.push(candidate_for_model(
            config,
            model,
            Some(unique_candidate_id(model, &mut seen_ids)),
        ));
    }

    candidates
}

pub(crate) fn candidate_from_available_model_by_id(
    config: &Config,
    available_models: &[ModelPreset],
    candidate_id: &str,
) -> Option<ModelRouterCandidateToml> {
    if let Some(model) = config.model.as_deref()
        && (model == candidate_id || candidate_id == SPARK_CANDIDATE_ID && is_spark_model(model))
    {
        return Some(candidate_for_model(
            config,
            model,
            Some(candidate_id.to_string()),
        ));
    }

    candidates_from_available_models(config, available_models)
        .into_iter()
        .find(|candidate| {
            candidate.id.as_deref() == Some(candidate_id)
                || candidate.model.as_deref() == Some(candidate_id)
        })
}

fn candidate_for_model(
    config: &Config,
    model: &str,
    id: Option<String>,
) -> ModelRouterCandidateToml {
    ModelRouterCandidateToml {
        id,
        model: Some(model.to_string()),
        model_provider: Some(config.model_provider_id.clone()),
        ..Default::default()
    }
}

fn configured_model_slugs(config: &Config) -> BTreeSet<String> {
    let mut models = BTreeSet::new();
    if let Some(model) = config.model.as_deref()
        && !model.trim().is_empty()
    {
        models.insert(model.to_string());
    }
    if let Some(model_router) = config.model_router.as_ref() {
        models.extend(
            model_router
                .candidates
                .iter()
                .filter_map(|candidate| candidate.model.as_deref())
                .filter(|model| !model.trim().is_empty())
                .map(ToString::to_string),
        );
    }
    models
}

fn configured_candidate_ids(config: &Config) -> BTreeSet<String> {
    config
        .model_router
        .as_ref()
        .map(|model_router| {
            model_router
                .candidates
                .iter()
                .filter_map(|candidate| candidate.id.as_deref())
                .filter(|id| !id.trim().is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn unique_candidate_id(model: &str, seen_ids: &mut BTreeSet<String>) -> String {
    let base_id = if is_spark_model(model) && !seen_ids.contains(SPARK_CANDIDATE_ID) {
        SPARK_CANDIDATE_ID.to_string()
    } else {
        format!("auto:{model}")
    };

    if seen_ids.insert(base_id.clone()) {
        return base_id;
    }

    let mut suffix = 2;
    loop {
        let candidate_id = format!("{base_id}:{suffix}");
        if seen_ids.insert(candidate_id.clone()) {
            return candidate_id;
        }
        suffix += 1;
    }
}

fn is_spark_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("spark")
}
