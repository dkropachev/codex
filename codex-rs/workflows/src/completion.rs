use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::WorkflowPackage;

const MAX_COMPLETION_ITEMS: usize = 128;
const MAX_COMPLETION_VALUE_BYTES: usize = 1_024;
const MAX_COMPLETION_DESCRIPTION_BYTES: usize = 2_048;
const MAX_COMPLETION_TOTAL_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionMode {
    Field,
    Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionRequest {
    pub input: Value,
    pub active_field: Option<String>,
    pub prefix: String,
    pub mode: CompletionMode,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletionItem {
    pub value: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionResult {
    pub items: Vec<CompletionItem>,
    pub error: Option<String>,
    insertions: BTreeMap<String, String>,
}

impl CompletionResult {
    pub fn new(items: Vec<CompletionItem>, error: Option<String>) -> Self {
        let candidates = items
            .into_iter()
            .map(|item| CompletionCandidate {
                insertion: string_argument(&item.value),
                item,
            })
            .collect();
        completion_result(candidates, error)
    }

    pub fn insertion_for<'a>(&'a self, value: &'a str) -> &'a str {
        self.insertions.get(value).map_or(value, String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletionCandidate {
    item: CompletionItem,
    insertion: String,
}

pub fn complete_workflow(root: &Path, request: &CompletionRequest) -> CompletionResult {
    complete_workflow_cancellable(root, request, Arc::new(AtomicBool::new(false)))
}

pub fn complete_workflow_cancellable(
    root: &Path,
    request: &CompletionRequest,
    cancelled: Arc<AtomicBool>,
) -> CompletionResult {
    let deadline = crate::runner::CommandDeadline::after(crate::runner::COMPLETION_TIMEOUT);
    let package = match deadline
        .check(Some(&cancelled))
        .and_then(|()| WorkflowPackage::load(root))
        .and_then(|package| {
            crate::validation::validate_executable_package_cancellable(
                &package, deadline, &cancelled,
            )?;
            Ok(package)
        }) {
        Ok(package) => package,
        Err(err) => {
            return CompletionResult {
                items: Vec::new(),
                error: Some(format!("{err:#}")),
                insertions: BTreeMap::new(),
            };
        }
    };
    let operation = match crate::runner::run_completion_operation_cancellable_until(
        root,
        &package.manifest,
        request,
        &cancelled,
        deadline,
    ) {
        Ok(operation) => operation,
        Err(err) => {
            return CompletionResult {
                items: Vec::new(),
                error: Some(format!("{err:#}")),
                insertions: BTreeMap::new(),
            };
        }
    };
    let contract = match crate::schema::contract_from_inspection(&operation.inspection) {
        Ok(contract) => contract,
        Err(err) => {
            return CompletionResult {
                items: Vec::new(),
                error: Some(format!("{err:#}")),
                insertions: BTreeMap::new(),
            };
        }
    };
    let static_items = static_completions(contract.input_schema(), request);
    let items = merge_completions(static_items, operation.items);
    if let Err(err) = deadline.check(Some(&cancelled)) {
        return CompletionResult {
            items: Vec::new(),
            error: Some(format!("{err:#}")),
            insertions: BTreeMap::new(),
        };
    }
    completion_result(items, operation.error)
}

fn static_completions(
    input_schema: &Value,
    request: &CompletionRequest,
) -> Vec<CompletionCandidate> {
    let properties = crate::completion_schema::top_level_properties(input_schema);
    match request.mode {
        CompletionMode::Field => properties
            .iter()
            .filter_map(|(name, schema)| {
                let flag = format!("--{}", camel_to_kebab(name));
                flag.starts_with(&request.prefix)
                    .then(|| CompletionCandidate {
                        insertion: flag.clone(),
                        item: CompletionItem {
                            value: flag,
                            description: schema.description(input_schema).map(str::to_string),
                        },
                    })
            })
            .collect(),
        CompletionMode::Value => {
            let Some(active_field) = request.active_field.as_deref() else {
                return Vec::new();
            };
            let Some(property) = properties.get(active_field) else {
                return Vec::new();
            };
            let description = property.description(input_schema).map(str::to_string);
            let values = property.values(input_schema);
            values
                .into_iter()
                .filter_map(completion_value)
                .filter(|(value, _)| value.starts_with(&request.prefix))
                .map(|(value, insertion)| CompletionCandidate {
                    insertion,
                    item: CompletionItem {
                        value,
                        description: description.clone(),
                    },
                })
                .collect()
        }
    }
}

fn merge_completions(
    static_items: Vec<CompletionCandidate>,
    dynamic_items: Vec<CompletionItem>,
) -> Vec<CompletionCandidate> {
    let mut merged = BTreeMap::new();
    let dynamic_items = dynamic_items.into_iter().map(|item| CompletionCandidate {
        insertion: string_argument(&item.value),
        item,
    });
    for candidate in static_items.into_iter().chain(dynamic_items) {
        merged
            .entry(candidate.item.value.clone())
            .and_modify(|existing: &mut CompletionCandidate| {
                if existing.item.description.is_none() {
                    existing
                        .item
                        .description
                        .clone_from(&candidate.item.description);
                }
            })
            .or_insert(candidate);
    }
    merged.into_values().collect()
}

fn bound_items(items: Vec<CompletionCandidate>) -> Vec<CompletionCandidate> {
    let mut retained = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0;
    for mut candidate in items {
        let item = &mut candidate.item;
        if retained.len() >= MAX_COMPLETION_ITEMS
            || item.value.is_empty()
            || item.value.len() > MAX_COMPLETION_VALUE_BYTES
            || !seen.insert(item.value.clone())
        {
            continue;
        }
        if item
            .description
            .as_ref()
            .is_some_and(|description| description.len() > MAX_COMPLETION_DESCRIPTION_BYTES)
        {
            item.description = None;
        }
        let item_bytes = item.value.len()
            + item
                .description
                .as_ref()
                .map_or(0, std::string::String::len);
        if total_bytes + item_bytes > MAX_COMPLETION_TOTAL_BYTES {
            break;
        }
        total_bytes += item_bytes;
        retained.push(candidate);
    }
    retained
}

fn completion_value(value: &Value) -> Option<(String, String)> {
    match value {
        Value::String(value) => Some((value.clone(), string_argument(value))),
        value => serde_json::to_string(value)
            .ok()
            .map(|value| (value.clone(), value)),
    }
}

fn string_argument(value: &str) -> String {
    match serde_json::from_str::<Value>(value) {
        Ok(_) => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        Err(_) => value.to_string(),
    }
}

fn completion_result(
    candidates: Vec<CompletionCandidate>,
    error: Option<String>,
) -> CompletionResult {
    let candidates = bound_items(candidates);
    let insertions = candidates
        .iter()
        .map(|candidate| (candidate.item.value.clone(), candidate.insertion.clone()))
        .collect();
    CompletionResult {
        items: candidates
            .into_iter()
            .map(|candidate| candidate.item)
            .collect(),
        error,
        insertions,
    }
}

fn camel_to_kebab(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        if ch.is_ascii_uppercase() {
            if !output.is_empty() {
                output.push('-');
            }
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
