use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;

use crate::function_tool::FunctionCallError;
use crate::tools::registry::ToolRegistry;
use codex_tools::ConfiguredToolSpec;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::TOOL_ROUTER_TOOL_NAME;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolIndexSource {
    Spec,
    Registry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolIndexEntry {
    pub(crate) name: ToolName,
    pub(crate) source: ToolIndexSource,
    pub(crate) has_handler: bool,
    pub(crate) freeform: bool,
    pub(crate) fanout_safe: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolRouterIndex {
    entries: Vec<ToolIndexEntry>,
}

impl ToolRouterIndex {
    pub(crate) fn build(
        specs: &[ConfiguredToolSpec],
        registry: &ToolRegistry,
        parallel_mcp_server_names: &HashSet<String>,
    ) -> Self {
        let mut entries = BTreeMap::<String, ToolIndexEntry>::new();
        for configured in specs {
            for (name, freeform) in spec_tool_names(&configured.spec) {
                let display = name.display();
                let has_handler = registry.has_handler(&name);
                entries.insert(
                    display,
                    ToolIndexEntry {
                        fanout_safe: fanout_safe_tool_name(&name, parallel_mcp_server_names),
                        name,
                        source: ToolIndexSource::Spec,
                        has_handler,
                        freeform,
                    },
                );
            }
        }

        for name in registry.tool_names() {
            let display = name.display();
            entries.entry(display).or_insert_with(|| ToolIndexEntry {
                fanout_safe: fanout_safe_tool_name(&name, parallel_mcp_server_names),
                name,
                source: ToolIndexSource::Registry,
                has_handler: true,
                freeform: false,
            });
        }

        Self {
            entries: entries.into_values().collect(),
        }
    }

    pub(crate) fn has_handler(&self, name: &ToolName) -> bool {
        self.entries
            .iter()
            .any(|entry| &entry.name == name && entry.has_handler)
    }

    pub(crate) fn is_freeform(&self, name: &ToolName) -> bool {
        self.entries
            .iter()
            .any(|entry| &entry.name == name && entry.freeform)
    }

    pub(crate) fn fanout_safe(&self, name: &ToolName) -> bool {
        self.entries
            .iter()
            .any(|entry| &entry.name == name && entry.fanout_safe && entry.has_handler)
    }

    pub(crate) fn find_exact(
        &self,
        candidate: &str,
        namespace: Option<&str>,
    ) -> Result<Option<ToolName>, FunctionCallError> {
        let candidate = candidate.trim();
        if candidate.is_empty() || candidate == TOOL_ROUTER_TOOL_NAME {
            return Ok(None);
        }

        let mut matches = self
            .entries
            .iter()
            .filter(|entry| {
                entry.has_handler
                    && entry.name.name != TOOL_ROUTER_TOOL_NAME
                    && matches_candidate(&entry.name, candidate, namespace)
            })
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        matches.sort_by_key(ToolName::display);
        let mut seen = HashSet::new();
        matches.retain(|name| seen.insert(name.display()));

        match matches.as_slice() {
            [] => Ok(None),
            [tool_name] => Ok(Some(tool_name.clone())),
            _ => Err(FunctionCallError::RespondToModel(format!(
                "tool_router route `{candidate}` is ambiguous; provide action.tool with an exact namespace-qualified tool name"
            ))),
        }
    }

    pub(crate) fn prompt_catalog(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.has_handler && entry.name.name != TOOL_ROUTER_TOOL_NAME)
            .map(|entry| {
                let fanout = if entry.fanout_safe {
                    " fanout_safe"
                } else {
                    ""
                };
                format!("{} ({:?}{fanout})", entry.name.display(), entry.source)
            })
            .collect()
    }

    pub(crate) fn learned_rule_tool_names(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .filter(|entry| entry.has_handler && entry.name.name != TOOL_ROUTER_TOOL_NAME)
            .map(|entry| entry.name.display())
            .collect()
    }
}

fn spec_tool_names(spec: &ToolSpec) -> Vec<(ToolName, bool)> {
    match spec {
        ToolSpec::Function(tool) => vec![(ToolName::plain(tool.name.as_str()), false)],
        ToolSpec::Freeform(tool) => vec![(ToolName::plain(tool.name.as_str()), true)],
        ToolSpec::Namespace(namespace) => namespace
            .tools
            .iter()
            .map(|tool| match tool {
                ResponsesApiNamespaceTool::Function(tool) => (
                    ToolName::namespaced(namespace.name.as_str(), tool.name.as_str()),
                    false,
                ),
            })
            .collect(),
        ToolSpec::ToolSearch { .. } => vec![(ToolName::plain("tool_search"), false)],
        ToolSpec::LocalShell {} => vec![(ToolName::plain("local_shell"), false)],
        ToolSpec::ImageGeneration { .. } => vec![(ToolName::plain("image_generation"), false)],
        ToolSpec::WebSearch { .. } => vec![(ToolName::plain("web_search"), false)],
    }
}

fn matches_candidate(tool_name: &ToolName, candidate: &str, namespace: Option<&str>) -> bool {
    if let Some(namespace) = namespace {
        return tool_name.namespace.as_deref() == Some(namespace)
            && (tool_name.name == candidate
                || tool_name.display() == candidate
                || namespaced_dot_candidate(tool_name, candidate));
    }

    tool_name.display() == candidate
        || tool_name.name == candidate
        || namespaced_dot_candidate(tool_name, candidate)
}

fn namespaced_dot_candidate(tool_name: &ToolName, candidate: &str) -> bool {
    tool_name
        .namespace
        .as_ref()
        .is_some_and(|namespace| format!("{namespace}.{}", tool_name.name) == candidate)
}

fn fanout_safe_tool_name(name: &ToolName, parallel_mcp_server_names: &HashSet<String>) -> bool {
    if name.namespace.is_none() {
        return matches!(
            name.name.as_str(),
            "tool_search" | "list_dir" | "view_image"
        );
    }

    name.namespace
        .as_deref()
        .and_then(mcp_server_from_namespace)
        .is_some_and(|server| parallel_mcp_server_names.contains(server))
}

fn mcp_server_from_namespace(namespace: &str) -> Option<&str> {
    namespace
        .strip_prefix("mcp__")
        .and_then(|suffix| suffix.strip_suffix("__"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::tools::context::FunctionToolOutput;
    use crate::tools::context::ToolInvocation;
    use crate::tools::registry::ToolHandler;
    use crate::tools::registry::ToolKind;
    use crate::tools::registry::ToolRegistry;
    use codex_tools::JsonSchema;
    use codex_tools::ResponsesApiTool;
    use pretty_assertions::assert_eq;

    use super::*;

    struct TestHandler;

    impl ToolHandler for TestHandler {
        type Output = FunctionToolOutput;

        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> Result<FunctionToolOutput, FunctionCallError> {
            Ok(FunctionToolOutput::from_text("ok".to_string(), Some(true)))
        }
    }

    #[test]
    fn exact_namespace_matching_rejects_ambiguous_bare_names() {
        let index = ToolRouterIndex {
            entries: vec![
                ToolIndexEntry {
                    name: ToolName::namespaced("mcp__calendar__", "list"),
                    source: ToolIndexSource::Spec,
                    has_handler: true,
                    freeform: false,
                    fanout_safe: false,
                },
                ToolIndexEntry {
                    name: ToolName::namespaced("mcp__files__", "list"),
                    source: ToolIndexSource::Spec,
                    has_handler: true,
                    freeform: false,
                    fanout_safe: false,
                },
            ],
        };

        assert_eq!(
            index
                .find_exact("list", Some("mcp__calendar__"))
                .expect("match"),
            Some(ToolName::namespaced("mcp__calendar__", "list"))
        );
        assert!(
            index
                .find_exact("list", None)
                .expect_err("ambiguous bare name")
                .to_string()
                .contains("ambiguous")
        );
    }

    #[test]
    fn built_in_read_only_tools_are_fanout_safe() {
        let registry =
            ToolRegistry::with_handler_for_test(ToolName::plain("list_dir"), Arc::new(TestHandler));
        let specs = vec![ConfiguredToolSpec::new(function_tool("list_dir"), false)];
        let index = ToolRouterIndex::build(&specs, &registry, &HashSet::new());

        assert!(index.fanout_safe(&ToolName::plain("list_dir")));
        assert!(!index.fanout_safe(&ToolName::plain("apply_patch")));
    }

    #[test]
    fn spec_without_handler_is_not_exact_match() {
        let index = ToolRouterIndex::build(
            &[ConfiguredToolSpec::new(
                function_tool("missing_tool"),
                false,
            )],
            &ToolRegistry::empty_for_test(),
            &HashSet::new(),
        );

        assert_eq!(
            index.find_exact("missing_tool", None).expect("lookup"),
            None
        );
    }

    #[test]
    fn exact_lookup_accepts_dot_qualified_namespace_names() {
        let tool_name = ToolName::namespaced("repo_ci", "run");
        let registry =
            ToolRegistry::with_handler_for_test(tool_name.clone(), Arc::new(TestHandler));
        let index = ToolRouterIndex::build(&[], &registry, &HashSet::new());

        assert_eq!(
            index.find_exact("repo_ci.run", None).expect("lookup"),
            Some(tool_name)
        );
    }

    #[test]
    fn parallel_mcp_namespace_is_fanout_safe() {
        let tool_name = ToolName::namespaced("mcp__echo__", "query");
        let registry =
            ToolRegistry::with_handler_for_test(tool_name.clone(), Arc::new(TestHandler));
        let index = ToolRouterIndex::build(&[], &registry, &HashSet::from(["echo".to_string()]));

        assert!(index.fanout_safe(&tool_name));
        assert!(!index.fanout_safe(&ToolName::namespaced("mcp__other__", "query")));
    }

    #[test]
    fn learned_rule_tool_names_excludes_router_and_missing_handlers() {
        let registry =
            ToolRegistry::with_handler_for_test(ToolName::plain("list_dir"), Arc::new(TestHandler));
        let specs = vec![
            ConfiguredToolSpec::new(function_tool("list_dir"), false),
            ConfiguredToolSpec::new(function_tool("missing_tool"), false),
            ConfiguredToolSpec::new(function_tool(TOOL_ROUTER_TOOL_NAME), false),
        ];
        let index = ToolRouterIndex::build(&specs, &registry, &HashSet::new());

        assert_eq!(
            index.learned_rule_tool_names(),
            BTreeSet::from(["list_dir".to_string()])
        );
    }

    fn function_tool(name: &str) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: name.to_string(),
            description: String::new(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(Default::default(), None, Some(false.into())),
            output_schema: None,
        })
    }
}
