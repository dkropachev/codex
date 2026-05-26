use anyhow::Result;
use rune::Any;
use rune::Module;
use rune::runtime::Ref;
use rune::runtime::Value;
use rune::runtime::Vec as RuneVec;
use rune::runtime::VmResult;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::rune_app_server::json_value_to_rune_result;
use crate::rune_app_server::rune_value_to_json;
use crate::rune_app_server::vm_result_from_result;

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneInputHelpers;

impl WorkflowRuneInputHelpers {
    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::text__meta)?;
        module.function_meta(Self::image__meta)?;
        module.function_meta(Self::local_image__meta)?;
        module.function_meta(Self::skill__meta)?;
        module.function_meta(Self::mention__meta)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::text)]
    fn text(_: Ref<Self>, text: Ref<str>) -> VmResult<Value> {
        vm_result_from_result(json_value_to_rune_result(json!({
            "type": "text",
            "text": &*text,
            "textElements": [],
        })))
    }

    #[rune::function(keep, instance, path = Self::image)]
    fn image(_: Ref<Self>, url: Ref<str>) -> VmResult<Value> {
        vm_result_from_result(json_value_to_rune_result(json!({
            "type": "image",
            "url": &*url,
        })))
    }

    #[rune::function(keep, instance, path = Self::localImage)]
    fn local_image(_: Ref<Self>, path: Ref<str>) -> VmResult<Value> {
        vm_result_from_result(json_value_to_rune_result(json!({
            "type": "localImage",
            "path": &*path,
        })))
    }

    #[rune::function(keep, instance, path = Self::skill)]
    fn skill(_: Ref<Self>, name: Ref<str>, path: Ref<str>) -> VmResult<Value> {
        vm_result_from_result(json_value_to_rune_result(json!({
            "type": "skill",
            "name": &*name,
            "path": &*path,
        })))
    }

    #[rune::function(keep, instance, path = Self::mention)]
    fn mention(_: Ref<Self>, name: Ref<str>, path: Ref<str>) -> VmResult<Value> {
        vm_result_from_result(json_value_to_rune_result(json!({
            "type": "mention",
            "name": &*name,
            "path": &*path,
        })))
    }
}

pub(crate) fn normalize_user_input(input: Value) -> Result<Vec<JsonValue>> {
    if let Ok(text) = rune::from_value::<String>(input.clone()) {
        return Ok(vec![text_input(text)]);
    }
    if let Ok(items) = input.borrow_ref::<RuneVec>() {
        return items
            .iter()
            .map(|value| normalize_single_input(value.clone()))
            .collect();
    }
    Ok(vec![normalize_single_input(input)?])
}

fn normalize_single_input(input: Value) -> Result<JsonValue> {
    let value = rune_value_to_json(input)?;
    match value {
        JsonValue::String(text) => Ok(text_input(text)),
        JsonValue::Object(mut object) => {
            let Some(kind) = object.get("type").and_then(JsonValue::as_str) else {
                return Ok(text_input(serde_json::to_string(&JsonValue::Object(
                    object,
                ))?));
            };
            match kind {
                "text" => {
                    object
                        .entry("textElements".to_string())
                        .or_insert(JsonValue::Array(Vec::new()));
                    Ok(JsonValue::Object(object))
                }
                "image" | "localImage" | "skill" | "mention" => Ok(JsonValue::Object(object)),
                other => anyhow::bail!("unsupported user input type `{other}`"),
            }
        }
        JsonValue::Array(values) => {
            anyhow::bail!("nested user input arrays are not supported: {values:?}")
        }
        other => Ok(text_input(serde_json::to_string(&other)?)),
    }
}

fn text_input(text: String) -> JsonValue {
    json!({
        "type": "text",
        "text": text,
        "textElements": [],
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::normalize_user_input;
    use crate::rune_app_server::json_value_to_rune_result;

    #[test]
    fn normalizes_plain_string_to_text_input() {
        let input = json_value_to_rune_result(json!("hello")).expect("rune value");

        assert_eq!(
            normalize_user_input(input).expect("input"),
            vec![json!({ "type": "text", "text": "hello", "textElements": [] })]
        );
    }

    #[test]
    fn normalizes_structured_input_array() {
        let input = json_value_to_rune_result(json!([
            { "type": "text", "text": "hello" },
            { "type": "image", "url": "https://example.com/image.png" },
            { "type": "localImage", "path": "diagram.png" },
            { "type": "skill", "name": "review", "path": "/skills/review" },
            { "type": "mention", "name": "file", "path": "/tmp/file.txt" },
        ]))
        .expect("rune value");

        assert_eq!(
            normalize_user_input(input).expect("input"),
            vec![
                json!({ "type": "text", "text": "hello", "textElements": [] }),
                json!({ "type": "image", "url": "https://example.com/image.png" }),
                json!({ "type": "localImage", "path": "diagram.png" }),
                json!({ "type": "skill", "name": "review", "path": "/skills/review" }),
                json!({ "type": "mention", "name": "file", "path": "/tmp/file.txt" }),
            ]
        );
    }

    #[test]
    fn rejects_unknown_structured_input_type() {
        let input = json_value_to_rune_result(json!({ "type": "unknown" })).expect("rune value");

        let err = normalize_user_input(input).expect_err("unsupported type");

        assert_eq!(err.to_string(), "unsupported user input type `unknown`");
    }
}
