//! Keeps per-step model specs separate from shared execution runtimes.

use super::CoreToolRuntime;
use super::ToolExposure;
use super::ToolRegistry;
use codex_tools::FunctionsNamespaceFunctionPrefixes;
use codex_tools::ToolName;
use codex_tools::ToolSearchInfo;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A tool runtime together with its effective exposure for the current step.
pub(crate) struct RegisteredTool {
    pub(crate) runtime: Arc<dyn CoreToolRuntime>,
    pub(crate) exposure: ToolExposure,
    /// Per-step description override; never written back to a shared runtime.
    pub(crate) model_spec: Option<Arc<ToolSpec>>,
}

impl RegisteredTool {
    /// Direct declarations and Code Mode read this spec; execution uses the runtime.
    pub(crate) fn spec(&self) -> Arc<ToolSpec> {
        self.model_spec
            .as_ref()
            .or_else(|| self.runtime.immutable_spec())
            .cloned()
            .unwrap_or_else(|| Arc::new(self.runtime.spec()))
    }

    /// Runtime caches describe the original spec, so cannot serve an overridden one.
    pub(crate) fn cached_runtime(&self) -> Option<&Arc<dyn CoreToolRuntime>> {
        (self.model_spec.is_none() && self.runtime.immutable_spec().is_some())
            .then_some(&self.runtime)
    }
}

impl ToolRegistry {
    pub(crate) fn search_info(&self, tool: &RegisteredTool) -> Option<ToolSearchInfo> {
        let info = tool.runtime.search_info()?;
        // A runtime may advertise a different search schema or description. Preserve it,
        // along with its ranking text and source, rather than substituting the normal spec.
        match self
            .functions_namespace_function_prefixes
            .prepare(&tool.runtime.tool_name(), || {
                info.entry.to_loadable_spec().into()
            }) {
            Some(spec) => ToolSearchInfo::from_spec(info.entry.search_text, spec, info.source_info),
            None => Some(info),
        }
    }

    /// Apply catalog prefixes to this step's specs, budgeting only reachable tools.
    /// The planner supplies the names; this does not change execution or exposure.
    pub(crate) fn apply_functions_namespace_function_prefixes(
        &mut self,
        prefixes: Option<&BTreeMap<String, String>>,
        available_tools: impl IntoIterator<Item = ToolName>,
    ) -> Result<(), &'static str> {
        let prefixes = FunctionsNamespaceFunctionPrefixes::new(prefixes, available_tools)?;
        for tool in self.tools.values_mut() {
            tool.model_spec = prefixes
                .prepare(&tool.runtime.tool_name(), || tool.runtime.spec())
                .map(Arc::new);
        }
        self.functions_namespace_function_prefixes = prefixes;
        Ok(())
    }

    pub(super) fn prepare_tool(
        &self,
        runtime: Arc<dyn CoreToolRuntime>,
        exposure: ToolExposure,
    ) -> RegisteredTool {
        let model_spec = self
            .functions_namespace_function_prefixes
            .prepare(&runtime.tool_name(), || runtime.spec())
            .map(Arc::new);
        RegisteredTool {
            runtime,
            exposure,
            model_spec,
        }
    }
}
