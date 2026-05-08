use std::collections::BTreeSet;

use codex_config::config_toml::ModelRouterCandidateToml;

use crate::config::Config;

use super::AvailableRouterModel;

const SPARK_CANDIDATE_ID: &str = "spark";

pub(crate) fn candidates_from_available_models(
    config: &Config,
    available_models: &[AvailableRouterModel],
) -> Vec<ModelRouterCandidateToml> {
    let mut seen_models = configured_model_keys(config);
    let mut seen_ids = configured_candidate_ids(config);
    let mut candidates = Vec::new();

    for (model_provider_id, model) in available_models.iter().filter_map(|available_model| {
        let model = available_model.model.trim();
        (!model.is_empty()).then_some((available_model.model_provider_id.as_str(), model))
    }) {
        if !seen_models.insert(model_key(model_provider_id, model)) {
            continue;
        }
        candidates.push(candidate_for_model(
            model_provider_id,
            model,
            Some(unique_candidate_id(
                config,
                model_provider_id,
                model,
                &mut seen_ids,
            )),
        ));
    }

    candidates
}

fn candidate_for_model(
    model_provider_id: &str,
    model: &str,
    id: Option<String>,
) -> ModelRouterCandidateToml {
    ModelRouterCandidateToml {
        id,
        model: Some(model.to_string()),
        model_provider: Some(model_provider_id.to_string()),
        ..Default::default()
    }
}

fn configured_model_keys(config: &Config) -> BTreeSet<(String, String)> {
    let mut models = BTreeSet::new();
    if let Some(model) = config.model.as_deref()
        && !model.trim().is_empty()
    {
        models.insert(model_key(&config.model_provider_id, model));
    }
    if let Some(model_router) = config.model_router.as_ref() {
        models.extend(model_router.candidates.iter().filter_map(|candidate| {
            let model = candidate.model.as_deref()?;
            (!model.trim().is_empty()).then(|| {
                model_key(
                    candidate
                        .model_provider
                        .as_deref()
                        .unwrap_or(&config.model_provider_id),
                    model,
                )
            })
        }));
    }
    models
}

fn model_key(model_provider_id: &str, model: &str) -> (String, String) {
    (model_provider_id.to_string(), model.to_string())
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

fn unique_candidate_id(
    config: &Config,
    model_provider_id: &str,
    model: &str,
    seen_ids: &mut BTreeSet<String>,
) -> String {
    let base_id = if is_spark_model(model)
        && model_provider_id == config.model_provider_id
        && !seen_ids.contains(SPARK_CANDIDATE_ID)
    {
        SPARK_CANDIDATE_ID.to_string()
    } else if model_provider_id == config.model_provider_id {
        format!("auto:{model}")
    } else {
        format!("auto:{model_provider_id}:{model}")
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
