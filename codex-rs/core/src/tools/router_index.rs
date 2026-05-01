use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;

use crate::function_tool::FunctionCallError;
use crate::tools::registry::ToolRegistry;
use codex_tools::ConfiguredToolSpec;
use codex_tools::JsonSchema;
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
    pub(crate) description: String,
    pub(crate) argument_hints: Vec<String>,
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
            for spec_entry in spec_tool_entries(&configured.spec) {
                let name = spec_entry.name;
                let display = name.display();
                let has_handler = registry.has_handler(&name);
                entries.insert(
                    display,
                    ToolIndexEntry {
                        fanout_safe: fanout_safe_tool_name(&name, parallel_mcp_server_names),
                        name,
                        source: ToolIndexSource::Spec,
                        has_handler,
                        freeform: spec_entry.freeform,
                        description: spec_entry.description,
                        argument_hints: spec_entry.argument_hints,
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
                description: String::new(),
                argument_hints: Vec::new(),
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
                let description = if entry.description.is_empty() {
                    "no description".to_string()
                } else {
                    compact_description(entry.description.as_str())
                };
                let arguments = if entry.argument_hints.is_empty() {
                    String::new()
                } else {
                    format!(" args: {}", entry.argument_hints.join(", "))
                };
                format!(
                    "- `{}` ({:?}{fanout}): {description}{arguments}",
                    entry.name.display(),
                    entry.source
                )
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

struct ToolSpecCatalogEntry {
    name: ToolName,
    freeform: bool,
    description: String,
    argument_hints: Vec<String>,
}

fn spec_tool_entries(spec: &ToolSpec) -> Vec<ToolSpecCatalogEntry> {
    match spec {
        ToolSpec::Function(tool) => vec![ToolSpecCatalogEntry {
            name: ToolName::plain(tool.name.as_str()),
            freeform: false,
            description: tool.description.clone(),
            argument_hints: argument_hints(&tool.parameters),
        }],
        ToolSpec::Freeform(tool) => vec![ToolSpecCatalogEntry {
            name: ToolName::plain(tool.name.as_str()),
            freeform: true,
            description: tool.description.clone(),
            argument_hints: vec!["freeform input".to_string()],
        }],
        ToolSpec::Namespace(namespace) => namespace
            .tools
            .iter()
            .map(|tool| match tool {
                ResponsesApiNamespaceTool::Function(tool) => ToolSpecCatalogEntry {
                    name: ToolName::namespaced(namespace.name.as_str(), tool.name.as_str()),
                    freeform: false,
                    description: tool.description.clone(),
                    argument_hints: argument_hints(&tool.parameters),
                },
            })
            .collect(),
        ToolSpec::ToolSearch {
            description,
            parameters,
            ..
        } => vec![ToolSpecCatalogEntry {
            name: ToolName::plain("tool_search"),
            freeform: false,
            description: description.clone(),
            argument_hints: argument_hints(parameters),
        }],
        ToolSpec::LocalShell {} => vec![ToolSpecCatalogEntry {
            name: ToolName::plain("local_shell"),
            freeform: false,
            description: "execute a local shell action".to_string(),
            argument_hints: vec!["cmd".to_string()],
        }],
        ToolSpec::ImageGeneration { .. } => vec![ToolSpecCatalogEntry {
            name: ToolName::plain("image_generation"),
            freeform: false,
            description: "generate or edit bitmap images".to_string(),
            argument_hints: vec!["prompt".to_string()],
        }],
        ToolSpec::WebSearch { .. } => vec![ToolSpecCatalogEntry {
            name: ToolName::plain("web_search"),
            freeform: false,
            description: "search the web".to_string(),
            argument_hints: vec!["query".to_string()],
        }],
    }
}

fn argument_hints(schema: &JsonSchema) -> Vec<String> {
    let Some(properties) = schema.properties.as_ref() else {
        return Vec::new();
    };
    properties.keys().take(8).cloned().collect()
}

fn compact_description(description: &str) -> String {
    let first_line = description.lines().next().unwrap_or_default().trim();
    if first_line.len() <= 160 {
        first_line.to_string()
    } else {
        let truncated = first_line.chars().take(160).collect::<String>();
        format!("{}...", truncated.trim_end())
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
    use std::collections::BTreeMap;
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
                    description: String::new(),
                    argument_hints: Vec::new(),
                },
                ToolIndexEntry {
                    name: ToolName::namespaced("mcp__files__", "list"),
                    source: ToolIndexSource::Spec,
                    has_handler: true,
                    freeform: false,
                    fanout_safe: false,
                    description: String::new(),
                    argument_hints: Vec::new(),
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
    fn prompt_catalog_includes_descriptions_and_argument_hints() {
        let tool_name = ToolName::plain("exec_command");
        let spec = ToolSpec::Function(ResponsesApiTool {
            name: "exec_command".to_string(),
            description: "Run a command in the workspace.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    ("cmd".to_string(), JsonSchema::string(None)),
                    ("workdir".to_string(), JsonSchema::string(None)),
                ]),
                None,
                Some(false.into()),
            ),
            output_schema: None,
        });
        let index = ToolRouterIndex::build(
            &[ConfiguredToolSpec::new(spec, false)],
            &ToolRegistry::with_handler_for_test(tool_name, Arc::new(TestHandler)),
            &HashSet::new(),
        );

        assert_eq!(
            index.prompt_catalog(),
            vec![
                "- `exec_command` (Spec): Run a command in the workspace. args: cmd, workdir"
                    .to_string()
            ]
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
