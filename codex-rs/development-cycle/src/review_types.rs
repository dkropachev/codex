use std::collections::BTreeMap;
use std::collections::BTreeSet;

use codex_native_workflow::NativeWorkflowCompletionSuggestion;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewTypeDefinition {
    pub id: String,
    pub short_name: String,
    pub description: String,
    pub prompt: Option<String>,
    pub exclude_prompt: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewTypeDefinitionInput {
    pub short_name: String,
    pub description: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub exclude_prompt: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub(crate) fn built_in_review_types() -> Vec<ReviewTypeDefinition> {
    [
        (
            "correctness",
            "Correctness",
            "behavioral bugs, regressions, edge cases",
        ),
        (
            "security",
            "Security",
            "auth, injection, secrets, unsafe permissions, data exposure",
        ),
        (
            "tests",
            "Tests",
            "missing, weak, flaky, or mis-scoped tests",
        ),
        (
            "architecture",
            "Architecture",
            "API boundaries, coupling, ownership, long-term design risk",
        ),
        (
            "api",
            "API",
            "public interfaces, wire contracts, schemas, compatibility",
        ),
        (
            "maintainability",
            "Maintainability",
            "clarity, complexity, duplication, future change cost",
        ),
        (
            "performance",
            "Performance",
            "avoidable latency, memory, I/O, scaling risks",
        ),
        (
            "ux",
            "UX",
            "user workflow, wording, error states, ergonomics",
        ),
        (
            "ui",
            "UI",
            "layout, visual consistency, responsive rendering",
        ),
        (
            "accessibility",
            "Accessibility",
            "keyboard, contrast, semantics, assistive UX",
        ),
        ("docs", "Docs", "stale or missing docs for changed behavior"),
    ]
    .into_iter()
    .map(|(id, short_name, description)| ReviewTypeDefinition {
        id: id.to_string(),
        short_name: short_name.to_string(),
        description: description.to_string(),
        prompt: None,
        exclude_prompt: None,
        enabled: true,
    })
    .collect()
}

pub(crate) fn merge_review_types(
    custom: Option<BTreeMap<String, ReviewTypeDefinitionInput>>,
) -> anyhow::Result<Vec<ReviewTypeDefinition>> {
    let mut merged = built_in_review_types()
        .into_iter()
        .map(|definition| (definition.id.clone(), definition))
        .collect::<BTreeMap<_, _>>();

    for (id, definition) in custom.unwrap_or_default() {
        validate_custom_review_type(&id, &definition)?;
        merged.insert(
            id.clone(),
            ReviewTypeDefinition {
                id,
                short_name: definition.short_name,
                description: definition.description,
                prompt: definition.prompt,
                exclude_prompt: definition.exclude_prompt,
                enabled: definition.enabled.unwrap_or(true),
            },
        );
    }

    Ok(merged.into_values().collect())
}

pub(crate) fn select_review_types(
    definitions: &[ReviewTypeDefinition],
    requested: Option<&[String]>,
) -> anyhow::Result<(Vec<ReviewTypeDefinition>, Vec<ReviewTypeDefinition>)> {
    let enabled = definitions
        .iter()
        .filter(|definition| definition.enabled)
        .cloned()
        .collect::<Vec<_>>();
    let selected_ids = match requested {
        Some(ids) => {
            let mut unique = BTreeSet::new();
            ids.iter()
                .filter(|id| unique.insert(id.to_string()))
                .cloned()
                .collect::<Vec<_>>()
        }
        None => enabled
            .iter()
            .map(|definition| definition.id.clone())
            .collect(),
    };

    let mut selected = Vec::new();
    for id in selected_ids {
        let Some(definition) = definitions.iter().find(|definition| definition.id == id) else {
            anyhow::bail!("unknown dev-cycle review type '{id}'");
        };
        if !definition.enabled {
            anyhow::bail!("dev-cycle review type '{id}' is disabled");
        }
        selected.push(definition.clone());
    }

    let selected_id_set = selected
        .iter()
        .map(|definition| definition.id.as_str())
        .collect::<BTreeSet<_>>();
    let excluded = definitions
        .iter()
        .filter(|definition| !selected_id_set.contains(definition.id.as_str()))
        .cloned()
        .collect();
    Ok((selected, excluded))
}

pub(crate) fn review_type_suggestions(
    input: &JsonValue,
    prefix: &str,
) -> anyhow::Result<Vec<NativeWorkflowCompletionSuggestion>> {
    let custom = input
        .get("reviewTypeDefinitions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let definitions = merge_review_types(custom)?;
    let prefix = prefix.trim_start_matches(['"', '\'']);
    Ok(definitions
        .into_iter()
        .filter(|definition| definition.enabled && definition.id.starts_with(prefix))
        .map(|definition| NativeWorkflowCompletionSuggestion {
            display: definition.id.clone(),
            insert_text: definition.id,
            description: Some(format!(
                "{}: {}",
                definition.short_name, definition.description
            )),
        })
        .collect())
}

pub(crate) fn review_type_definitions_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": {
            "type": "object",
            "required": ["shortName", "description"],
            "additionalProperties": false,
            "properties": {
                "shortName": { "type": "string" },
                "description": { "type": "string" },
                "prompt": { "type": "string" },
                "excludePrompt": { "type": "string" },
                "enabled": { "type": "boolean" }
            }
        },
        "description": "Custom dev-cycle review type definitions keyed by review type id."
    })
}

fn validate_custom_review_type(
    id: &str,
    definition: &ReviewTypeDefinitionInput,
) -> anyhow::Result<()> {
    if id.trim().is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        anyhow::bail!("custom dev-cycle review type id '{id}' must be lowercase kebab/snake case");
    }
    if definition.short_name.trim().is_empty() {
        anyhow::bail!("custom dev-cycle review type '{id}' requires shortName");
    }
    if definition.description.trim().is_empty() {
        anyhow::bail!("custom dev-cycle review type '{id}' requires description");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn custom_review_types_merge_and_selection_respects_enabled() {
        let definitions = merge_review_types(Some(BTreeMap::from([(
            "domain".to_string(),
            ReviewTypeDefinitionInput {
                short_name: "Domain".to_string(),
                description: "domain-specific invariants".to_string(),
                prompt: Some("Check domain rules.".to_string()),
                exclude_prompt: None,
                enabled: Some(true),
            },
        )])))
        .unwrap();
        let (selected, excluded) = select_review_types(
            &definitions,
            Some(&["domain".to_string(), "tests".to_string()]),
        )
        .unwrap();

        assert_eq!(
            selected
                .iter()
                .map(|definition| definition.id.as_str())
                .collect::<Vec<_>>(),
            vec!["domain", "tests"]
        );
        assert!(
            excluded
                .iter()
                .any(|definition| definition.id == "security")
        );
    }

    #[test]
    fn completion_suggests_builtin_and_custom_review_types() {
        let suggestions = review_type_suggestions(
            &json!({
                "reviewTypeDefinitions": {
                    "domain": {
                        "shortName": "Domain",
                        "description": "domain-specific invariants"
                    }
                }
            }),
            "do",
        )
        .unwrap();

        assert_eq!(
            suggestions,
            vec![
                NativeWorkflowCompletionSuggestion {
                    display: "docs".to_string(),
                    insert_text: "docs".to_string(),
                    description: Some(
                        "Docs: stale or missing docs for changed behavior".to_string()
                    ),
                },
                NativeWorkflowCompletionSuggestion {
                    display: "domain".to_string(),
                    insert_text: "domain".to_string(),
                    description: Some("Domain: domain-specific invariants".to_string()),
                }
            ]
        );
    }
}
