use anyhow::Result;
use anyhow::anyhow;
use rune::Any;
use rune::Module;
use rune::runtime::Function;
use rune::runtime::Object;
use rune::runtime::Protocol;
use rune::runtime::Ref;
use rune::runtime::Value;
use rune::runtime::VmResult;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::rune_app_server::RegisteredDynamicTool;
use crate::rune_app_server::WorkflowRuneAppServer;
use crate::rune_app_server::json_value_to_rune_result;
use crate::rune_app_server::rune_value_to_json;
use crate::rune_app_server::vm_result_from_result;

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneDynamicTool {
    namespace: Option<String>,
    name: String,
    spec: JsonValue,
}

impl WorkflowRuneDynamicTool {
    pub(crate) fn define(
        app_server: WorkflowRuneAppServer,
        spec: Value,
        handler: Function,
    ) -> Result<Self> {
        let spec = normalize_tool_spec(spec)?;
        let namespace = spec
            .get("namespace")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string);
        let name = spec
            .get("name")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("dynamic tool spec requires non-empty name"))?
            .to_string();
        let description = spec
            .get("description")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("dynamic tool spec requires non-empty description"))?;
        if description.is_empty() {
            anyhow::bail!("dynamic tool spec requires non-empty description");
        }
        app_server.register_dynamic_tool(RegisteredDynamicTool {
            namespace: namespace.clone(),
            name: name.clone(),
            handler,
        })?;
        Ok(Self {
            namespace,
            name,
            spec,
        })
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::to_spec__meta)?;
        module.field_function(&Protocol::GET, "namespace", Self::namespace)?;
        module.field_function(&Protocol::GET, "name", Self::name)?;
        Ok(())
    }

    pub(crate) fn spec(&self) -> JsonValue {
        self.spec.clone()
    }

    #[rune::function(keep, instance, path = Self::toSpec)]
    fn to_spec(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_result(json_value_to_rune_result(this.spec()))
    }

    fn namespace(&self) -> Option<String> {
        self.namespace.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        value.borrow_ref::<Self>().ok().map(|tool| tool.clone())
    }
}

fn normalize_tool_spec(value: Value) -> Result<JsonValue> {
    let object = value
        .borrow_ref::<Object>()
        .map_err(|_| anyhow!("dynamic tool spec must be an object"))?;
    let mut spec = serde_json::Map::new();
    for (key, value) in object.iter() {
        let normalized_key = match key.as_str() {
            "input_schema" => "inputSchema",
            "defer_loading" => "deferLoading",
            other => other,
        };
        spec.insert(
            normalized_key.to_string(),
            rune_value_to_json(value.clone())?,
        );
    }
    let input_schema = spec
        .remove("inputSchema")
        .or_else(|| spec.remove("parameters"))
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    spec.insert("inputSchema".to_string(), input_schema);
    Ok(JsonValue::Object(spec))
}
