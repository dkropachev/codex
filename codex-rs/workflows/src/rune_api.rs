use anyhow::Result;
use rune::Any;
use rune::Module;
use rune::runtime::Protocol;
use rune::runtime::Ref;
use rune::runtime::Value;
use rune::runtime::VmResult;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::rune_app_server::WorkflowRuneAppServer;
use crate::rune_app_server::json_value_to_rune_result;
use crate::rune_app_server::rune_value_to_json;
use crate::rune_app_server::vm_result_from_future;

macro_rules! request_method {
    ($name:ident, $rune_name:ident, $method:literal) => {
        #[rune::function(keep, instance, path = Self::$rune_name)]
        async fn $name(this: Ref<Self>, params: Value) -> VmResult<Value> {
            request_value(&this.app_server, $method, params).await
        }
    };
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneApiNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneArtifactsNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneWorkflowsNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneWorkflowRegistryNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneWorkflowConfigNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneWorkflowCommandNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneMcpNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneToolsNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneFsNamespace {
    app_server: WorkflowRuneAppServer,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneProcessNamespace {
    app_server: WorkflowRuneAppServer,
}

impl WorkflowRuneApiNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::read__meta)?;
        Ok(())
    }

    request_method!(read, read, "apiCatalog/read");
}

impl WorkflowRuneArtifactsNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::register_state__meta)?;
        module.function_meta(Self::read_state__meta)?;
        module.function_meta(Self::list_states__meta)?;
        module.function_meta(Self::record_state_hit__meta)?;
        module.function_meta(Self::prune_states__meta)?;
        module.function_meta(Self::index_file__meta)?;
        module.function_meta(Self::find_file__meta)?;
        module.function_meta(Self::read_cache_entry__meta)?;
        module.function_meta(Self::write_cache_entry__meta)?;
        module.function_meta(Self::delete_cache_entry__meta)?;
        Ok(())
    }

    request_method!(register_state, registerState, "artifact/state/register");
    request_method!(read_state, readState, "artifact/state/read");
    request_method!(list_states, listStates, "artifact/state/list");
    request_method!(record_state_hit, recordStateHit, "artifact/state/hit");
    request_method!(prune_states, pruneStates, "artifact/state/prune");
    request_method!(index_file, indexFile, "artifact/file/index");
    request_method!(find_file, findFile, "artifact/file/find");
    request_method!(read_cache_entry, readCacheEntry, "artifact/cache/read");
    request_method!(write_cache_entry, writeCacheEntry, "artifact/cache/write");
    request_method!(
        delete_cache_entry,
        deleteCacheEntry,
        "artifact/cache/delete"
    );
}

impl WorkflowRuneWorkflowsNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::run__meta)?;
        module.field_function(&Protocol::GET, "registry", Self::registry)?;
        module.field_function(&Protocol::GET, "config", Self::config)?;
        module.field_function(&Protocol::GET, "command", Self::command)?;
        WorkflowRuneWorkflowRegistryNamespace::install(module)?;
        WorkflowRuneWorkflowConfigNamespace::install(module)?;
        WorkflowRuneWorkflowCommandNamespace::install(module)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::run)]
    async fn run(this: Ref<Self>, id: Ref<str>, input: Value) -> VmResult<Value> {
        vm_result_from_future(async {
            let input = rune_value_to_json(input)?;
            let response = this
                .app_server
                .request_json("workflow/run", json!({ "id": &*id, "input": input }))
                .await?;
            json_value_to_rune_result(response)
        })
        .await
    }

    fn registry(&self) -> WorkflowRuneWorkflowRegistryNamespace {
        WorkflowRuneWorkflowRegistryNamespace::new(self.app_server.clone())
    }

    fn config(&self) -> WorkflowRuneWorkflowConfigNamespace {
        WorkflowRuneWorkflowConfigNamespace::new(self.app_server.clone())
    }

    fn command(&self) -> WorkflowRuneWorkflowCommandNamespace {
        WorkflowRuneWorkflowCommandNamespace::new(self.app_server.clone())
    }
}

impl WorkflowRuneWorkflowRegistryNamespace {
    fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    request_method!(list, list, "workflow/list");
    request_method!(read, read, "workflow/read");
    request_method!(impact, impact, "workflow/impact");
    request_method!(develop, develop, "workflow/develop");
    request_method!(edit, edit, "workflow/edit");
    request_method!(validate, validate, "workflow/validate");
    request_method!(repair, repair, "workflow/repair");
    request_method!(
        authoring_context_prepare,
        authoringContextPrepare,
        "workflow/authoringContext/prepare"
    );

    fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::list__meta)?;
        module.function_meta(Self::read__meta)?;
        module.function_meta(Self::impact__meta)?;
        module.function_meta(Self::develop__meta)?;
        module.function_meta(Self::edit__meta)?;
        module.function_meta(Self::validate__meta)?;
        module.function_meta(Self::repair__meta)?;
        module.function_meta(Self::authoring_context_prepare__meta)?;
        Ok(())
    }
}

impl WorkflowRuneWorkflowConfigNamespace {
    fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    request_method!(read, read, "workflow/config/read");
    request_method!(write, write, "workflow/config/write");

    fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::read__meta)?;
        module.function_meta(Self::write__meta)?;
        Ok(())
    }
}

impl WorkflowRuneWorkflowCommandNamespace {
    fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    request_method!(execute, execute, "workflow/command/execute");

    fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::execute__meta)?;
        Ok(())
    }
}

impl WorkflowRuneMcpNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::list_servers__meta)?;
        module.function_meta(Self::read_resource__meta)?;
        module.function_meta(Self::call_tool__meta)?;
        Ok(())
    }

    request_method!(list_servers, listServers, "mcpServerStatus/list");
    request_method!(read_resource, readResource, "mcpServer/resource/read");
    request_method!(call_tool, callTool, "mcpServer/tool/call");
}

impl WorkflowRuneToolsNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::exec__meta)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::exec)]
    async fn exec(this: Ref<Self>, command: Value, options: Value) -> VmResult<Value> {
        vm_result_from_future(async {
            let mut params = command_exec_params(command)?;
            merge_object(&mut params, rune_value_to_json(options)?);
            let response = this.app_server.request_json("command/exec", params).await?;
            json_value_to_rune_result(response)
        })
        .await
    }
}

impl WorkflowRuneFsNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::read_file__meta)?;
        module.function_meta(Self::write_file__meta)?;
        module.function_meta(Self::read_directory__meta)?;
        module.function_meta(Self::create_directory__meta)?;
        module.function_meta(Self::remove__meta)?;
        module.function_meta(Self::copy__meta)?;
        module.function_meta(Self::watch__meta)?;
        module.function_meta(Self::unwatch__meta)?;
        Ok(())
    }

    request_method!(read_file, readFile, "fs/readFile");
    request_method!(write_file, writeFile, "fs/writeFile");
    request_method!(read_directory, readDirectory, "fs/readDirectory");
    request_method!(create_directory, createDirectory, "fs/createDirectory");
    request_method!(remove, remove, "fs/remove");
    request_method!(copy, copy, "fs/copy");
    request_method!(watch, watch, "fs/watch");
    request_method!(unwatch, unwatch, "fs/unwatch");
}

impl WorkflowRuneProcessNamespace {
    pub(crate) fn new(app_server: WorkflowRuneAppServer) -> Self {
        Self { app_server }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::spawn__meta)?;
        module.function_meta(Self::write_stdin__meta)?;
        module.function_meta(Self::kill__meta)?;
        module.function_meta(Self::resize_pty__meta)?;
        Ok(())
    }

    request_method!(spawn, spawn, "process/spawn");
    request_method!(write_stdin, writeStdin, "process/writeStdin");
    request_method!(kill, kill, "process/kill");
    request_method!(resize_pty, resizePty, "process/resizePty");
}

async fn request_value(
    app_server: &WorkflowRuneAppServer,
    method: &str,
    params: Value,
) -> VmResult<Value> {
    vm_result_from_future(async {
        let response = app_server
            .request_json(method, rune_value_to_json(params)?)
            .await?;
        json_value_to_rune_result(response)
    })
    .await
}

fn command_exec_params(command: Value) -> Result<JsonValue> {
    if let Ok(command) = rune::from_value::<String>(command.clone()) {
        let command =
            shlex::split(&command).unwrap_or_else(|| vec!["sh".into(), "-lc".into(), command]);
        return Ok(json!({ "command": command }));
    }
    let value = rune_value_to_json(command)?;
    match value {
        JsonValue::Array(command) => Ok(json!({ "command": command })),
        JsonValue::Object(object) => Ok(JsonValue::Object(object)),
        _ => anyhow::bail!("ctx.tools.exec command must be a string, array, or params object"),
    }
}

fn merge_object(target: &mut JsonValue, overlay: JsonValue) {
    let JsonValue::Object(target) = target else {
        return;
    };
    let JsonValue::Object(overlay) = overlay else {
        return;
    };
    for (key, value) in overlay {
        target.insert(key, value);
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::command_exec_params;
    use crate::rune_app_server::json_value_to_rune_result;

    #[test]
    fn command_exec_params_accepts_string_array_and_object() {
        let string =
            command_exec_params(json_value_to_rune_result(json!("echo hello")).expect("string"))
                .expect("string command");
        let array = command_exec_params(
            json_value_to_rune_result(json!(["echo", "hello"])).expect("array"),
        )
        .expect("array command");
        let object = command_exec_params(
            json_value_to_rune_result(json!({ "command": ["echo"], "cwd": "/tmp" }))
                .expect("object"),
        )
        .expect("object command");

        assert_eq!(string, json!({ "command": ["echo", "hello"] }));
        assert_eq!(array, json!({ "command": ["echo", "hello"] }));
        assert_eq!(object, json!({ "command": ["echo"], "cwd": "/tmp" }));
    }
}
