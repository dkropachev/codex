//! Applies sparse catalog overrides while preserving bundled tool transport requirements.
//! Invalid parameters retain the bundled schema; other tool fields remain harness-owned.

use crate::ResponsesApiTool;
use crate::parse_catalog_tool_parameters;
use codex_protocol::openai_models::ToolMessage;

const MAX_CATALOG_FIELD_BYTES: usize = 4_000;

/// Retain bundled guidance when a catalog override exceeds the serialized context budget.
pub fn catalog_tool_description<'a>(bundled: &'a str, description: Option<&'a str>) -> &'a str {
    match description {
        Some(description)
            if serde_json::to_vec(description)
                .is_ok_and(|value| value.len() <= MAX_CATALOG_FIELD_BYTES) =>
        {
            description
        }
        Some(_) => {
            tracing::warn!(
                "Catalog tool description exceeds context budget; using bundled description"
            );
            bundled
        }
        None => bundled,
    }
}

/// Apply catalog descriptions and parameters without changing the tool's identity or transport.
pub fn with_catalog_tool_message(
    mut tool: ResponsesApiTool,
    messages: Option<&ToolMessage>,
) -> ResponsesApiTool {
    let Some(messages) = messages else {
        return tool;
    };
    tool.description =
        catalog_tool_description(&tool.description, messages.description.as_deref()).to_string();
    if let Some(parameters) = &messages.parameters {
        let parameters = if parameters.len() > MAX_CATALOG_FIELD_BYTES {
            Err("schema exceeds catalog context budget")
        } else {
            parse_catalog_tool_parameters(parameters)
        }
        .and_then(|mut parameters| {
            if let Some(properties) = &tool.parameters.properties {
                for (name, bundled_property) in properties.iter() {
                    if bundled_property.encrypted == Some(true) {
                        let property = parameters
                            .properties
                            .as_mut()
                            .and_then(|properties| properties.get_mut(name))
                            .ok_or("schema omits an encrypted parameter")?;
                        property.encrypted = Some(true);
                    }
                }
            }
            if serde_json::to_vec(&parameters)
                .is_ok_and(|value| value.len() <= MAX_CATALOG_FIELD_BYTES)
            {
                Ok(parameters)
            } else {
                Err("schema exceeds catalog context budget")
            }
        });
        match parameters {
            Ok(parameters) => tool.parameters = parameters,
            Err(reason) => tracing::warn!(
                tool = %tool.name,
                reason,
                "Invalid catalog tool parameters; using bundled parameters"
            ),
        }
    }
    tool
}

#[cfg(test)]
#[path = "catalog_tool_message_tests.rs"]
mod tests;
