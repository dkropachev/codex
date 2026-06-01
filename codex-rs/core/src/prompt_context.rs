use codex_features::Feature;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_tools::ConfiguredToolSpec;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PromptContextPreset {
    #[default]
    Current,
    Workflow,
    Minimal,
    Isolated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptBlockMode {
    Inherit,
    Include,
    Omit,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum InstructionPolicy {
    #[default]
    Inherit,
    Omit,
    Set(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptContextPolicy {
    pub preset: Option<PromptContextPreset>,
    pub system_instructions: Option<InstructionPolicy>,
    pub developer: Option<DeveloperPromptPolicy>,
    pub user_context: Option<UserContextPromptPolicy>,
    pub strict: bool,
}

impl Default for PromptContextPolicy {
    fn default() -> Self {
        Self {
            preset: None,
            system_instructions: None,
            developer: None,
            user_context: None,
            strict: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeveloperPromptPolicy {
    pub instructions: Option<InstructionPolicy>,
    pub blocks: DeveloperPromptBlocks,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserContextPromptPolicy {
    pub instructions: Option<InstructionPolicy>,
    pub blocks: UserContextBlocks,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeveloperPromptBlocks {
    pub permissions: Option<PromptBlockMode>,
    pub collaboration_mode: Option<PromptBlockMode>,
    pub memories: Option<PromptBlockMode>,
    pub apps: Option<PromptBlockMode>,
    pub skills: Option<PromptBlockMode>,
    pub plugins: Option<PromptBlockMode>,
    pub commit_attribution: Option<PromptBlockMode>,
    pub personality: Option<PromptBlockMode>,
    pub realtime: Option<PromptBlockMode>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserContextBlocks {
    pub agents_md: Option<PromptBlockMode>,
    pub environment: Option<PromptBlockMode>,
    pub subagents: Option<PromptBlockMode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPromptContextPolicy {
    pub developer_instructions: bool,
    pub developer_instruction_policy: InstructionPolicy,
    pub user_instruction_policy: InstructionPolicy,
    pub permissions: bool,
    pub collaboration_mode: bool,
    pub memories: bool,
    pub apps: bool,
    pub skills: bool,
    pub plugins: bool,
    pub commit_attribution: bool,
    pub agents_md: bool,
    pub environment: bool,
    pub subagents: bool,
    pub personality: bool,
    pub realtime: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptInstructions {
    pub system_instructions: String,
    pub developer_instructions: String,
    pub user_instructions: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresetBlocks {
    developer_instructions: bool,
    user_instructions: bool,
    permissions: bool,
    collaboration_mode: bool,
    memories: bool,
    apps: bool,
    skills: bool,
    plugins: bool,
    commit_attribution: bool,
    agents_md: bool,
    environment: bool,
    subagents: bool,
    personality: bool,
    realtime: bool,
}

impl PromptContextPolicy {
    pub(crate) fn resolve(&self, defaults: PromptContextDefaults) -> ResolvedPromptContextPolicy {
        let preset = self.preset.unwrap_or_default();
        let base = preset_blocks(preset, defaults);
        let developer = self.developer.as_ref();
        let user_context = self.user_context.as_ref();

        let developer_instruction_policy = developer
            .and_then(|policy| policy.instructions.clone())
            .unwrap_or_default();
        let user_instruction_policy = user_context
            .and_then(|policy| policy.instructions.clone())
            .unwrap_or_default();

        ResolvedPromptContextPolicy {
            developer_instructions: instruction_enabled(
                base.developer_instructions,
                &developer_instruction_policy,
            ),
            developer_instruction_policy,
            user_instruction_policy: user_instruction_policy.clone(),
            permissions: decide_block(
                base.permissions,
                developer.and_then(|p| p.blocks.permissions),
            ),
            collaboration_mode: decide_block(
                base.collaboration_mode,
                developer.and_then(|p| p.blocks.collaboration_mode),
            ),
            memories: decide_block(base.memories, developer.and_then(|p| p.blocks.memories)),
            apps: decide_block(base.apps, developer.and_then(|p| p.blocks.apps)),
            skills: decide_block(base.skills, developer.and_then(|p| p.blocks.skills)),
            plugins: decide_block(base.plugins, developer.and_then(|p| p.blocks.plugins)),
            commit_attribution: decide_block(
                base.commit_attribution,
                developer.and_then(|p| p.blocks.commit_attribution),
            ),
            agents_md: decide_block(
                base.agents_md,
                user_context.and_then(|p| p.blocks.agents_md),
            ) && instruction_enabled(base.user_instructions, &user_instruction_policy),
            environment: decide_block(
                base.environment,
                user_context.and_then(|p| p.blocks.environment),
            ),
            subagents: decide_block(
                base.subagents,
                user_context.and_then(|p| p.blocks.subagents),
            ),
            personality: decide_block(
                base.personality,
                developer.and_then(|p| p.blocks.personality),
            ),
            realtime: decide_block(base.realtime, developer.and_then(|p| p.blocks.realtime)),
        }
    }

    pub fn validate_strict_for_config(&self, config: &crate::config::Config) -> Result<(), String> {
        if !self.strict {
            return Ok(());
        }

        let developer_blocks = self.developer.as_ref().map(|policy| &policy.blocks);
        let user_context = self.user_context.as_ref();
        let user_blocks = user_context.map(|policy| &policy.blocks);

        if explicitly_includes(developer_blocks.and_then(|blocks| blocks.memories))
            && (!config.features.enabled(Feature::MemoryTool) || !config.memories.use_memories)
        {
            return Err(
                "promptContext.developer.blocks.memories=include cannot be honored because memories are disabled"
                    .to_string(),
            );
        }

        if explicitly_includes(developer_blocks.and_then(|blocks| blocks.apps))
            && !config.features.enabled(Feature::Apps)
        {
            return Err(
                "promptContext.developer.blocks.apps=include cannot be honored because apps are disabled"
                    .to_string(),
            );
        }

        if explicitly_includes(developer_blocks.and_then(|blocks| blocks.commit_attribution))
            && !config.features.enabled(Feature::CodexGitCommit)
        {
            return Err(
                "promptContext.developer.blocks.commitAttribution=include cannot be honored because commit attribution is disabled"
                    .to_string(),
            );
        }

        if explicitly_includes(developer_blocks.and_then(|blocks| blocks.personality))
            && !config.features.enabled(Feature::Personality)
        {
            return Err(
                "promptContext.developer.blocks.personality=include cannot be honored because personality prompts are disabled"
                    .to_string(),
            );
        }

        if explicitly_includes(user_blocks.and_then(|blocks| blocks.agents_md))
            && user_context
                .and_then(|policy| policy.instructions.as_ref())
                .is_some_and(|policy| matches!(policy, InstructionPolicy::Omit))
        {
            return Err(
                "promptContext.userContext.blocks.agentsMd=include cannot be honored while userContext.instructions=omit"
                    .to_string(),
            );
        }

        if explicitly_includes(user_blocks.and_then(|blocks| blocks.subagents))
            && user_blocks
                .and_then(|blocks| blocks.environment)
                .is_some_and(|mode| matches!(mode, PromptBlockMode::Omit))
        {
            return Err(
                "promptContext.userContext.blocks.subagents=include cannot be honored while userContext.blocks.environment=omit"
                    .to_string(),
            );
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PromptContextDefaults {
    pub(crate) permissions: bool,
    pub(crate) apps: bool,
    pub(crate) skills: bool,
    pub(crate) environment: bool,
}

fn instruction_enabled(default: bool, policy: &InstructionPolicy) -> bool {
    match policy {
        InstructionPolicy::Inherit => default,
        InstructionPolicy::Omit => false,
        InstructionPolicy::Set(_) => true,
    }
}

fn decide_block(default: bool, mode: Option<PromptBlockMode>) -> bool {
    match mode.unwrap_or(PromptBlockMode::Inherit) {
        PromptBlockMode::Inherit => default,
        PromptBlockMode::Include => true,
        PromptBlockMode::Omit => false,
    }
}

fn explicitly_includes(mode: Option<PromptBlockMode>) -> bool {
    matches!(mode, Some(PromptBlockMode::Include))
}

fn preset_blocks(preset: PromptContextPreset, defaults: PromptContextDefaults) -> PresetBlocks {
    match preset {
        PromptContextPreset::Current => PresetBlocks {
            developer_instructions: true,
            user_instructions: true,
            permissions: defaults.permissions,
            collaboration_mode: true,
            memories: true,
            apps: defaults.apps,
            skills: defaults.skills,
            plugins: true,
            commit_attribution: true,
            agents_md: true,
            environment: defaults.environment,
            subagents: defaults.environment,
            personality: true,
            realtime: true,
        },
        PromptContextPreset::Workflow => PresetBlocks {
            developer_instructions: true,
            user_instructions: true,
            permissions: true,
            collaboration_mode: false,
            memories: false,
            apps: false,
            skills: false,
            plugins: false,
            commit_attribution: false,
            agents_md: true,
            environment: true,
            subagents: false,
            personality: false,
            realtime: false,
        },
        PromptContextPreset::Minimal => PresetBlocks {
            developer_instructions: false,
            user_instructions: false,
            permissions: true,
            collaboration_mode: false,
            memories: false,
            apps: false,
            skills: false,
            plugins: false,
            commit_attribution: false,
            agents_md: false,
            environment: true,
            subagents: false,
            personality: false,
            realtime: false,
        },
        PromptContextPreset::Isolated => PresetBlocks {
            developer_instructions: false,
            user_instructions: false,
            permissions: true,
            collaboration_mode: false,
            memories: false,
            apps: false,
            skills: false,
            plugins: false,
            commit_attribution: false,
            agents_md: false,
            environment: false,
            subagents: false,
            personality: false,
            realtime: false,
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPolicy {
    pub builtins: Option<ToolSetPolicy>,
    pub mcp: Option<McpToolPolicy>,
    pub dynamic: Option<ToolSetPolicy>,
    pub tool_router: Option<ToolRouterPolicy>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolSetPolicy {
    Inherit,
    None,
    AllowOnly(Vec<String>),
    Deny(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpToolPolicy {
    Inherit,
    None,
    AllowOnly {
        servers: Vec<String>,
        tools: Vec<String>,
    },
    Deny {
        servers: Vec<String>,
        tools: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRouterPolicy {
    Inherit,
    Off,
}

impl ToolPolicy {
    pub fn validate_static(&self) -> Result<(), String> {
        validate_named_tool_set(
            "toolPolicy.builtins",
            self.builtins.as_ref(),
            is_builtin_tool_name,
        )
    }

    pub fn validate_dynamic_tools(&self, dynamic_tools: &[DynamicToolSpec]) -> Result<(), String> {
        let available_tools = dynamic_tools
            .iter()
            .map(|tool| ToolName::new(tool.namespace.clone(), tool.name.clone()).display())
            .collect::<HashSet<_>>();
        validate_named_tool_set("toolPolicy.dynamic", self.dynamic.as_ref(), |name| {
            available_tools.contains(name)
        })
    }

    pub(crate) fn tool_router_enabled(&self, default: bool) -> bool {
        match self.tool_router.unwrap_or(ToolRouterPolicy::Inherit) {
            ToolRouterPolicy::Inherit => default,
            ToolRouterPolicy::Off => false,
        }
    }

    pub(crate) fn filter_dynamic_tools<T>(
        &self,
        tools: Vec<T>,
        tool_name: impl Fn(&T) -> ToolName,
    ) -> Vec<T> {
        filter_tool_set(tools, self.dynamic.as_ref(), tool_name)
    }

    pub(crate) fn filter_mcp_tools<T>(
        &self,
        tools: Vec<T>,
        server_name: impl Fn(&T) -> &str,
        tool_name: impl Fn(&T) -> ToolName,
    ) -> Vec<T> {
        match self.mcp.as_ref().unwrap_or(&McpToolPolicy::Inherit) {
            McpToolPolicy::Inherit => tools,
            McpToolPolicy::None => Vec::new(),
            McpToolPolicy::AllowOnly {
                servers,
                tools: names,
            } => {
                let servers = servers.iter().map(String::as_str).collect::<HashSet<_>>();
                let names = names.iter().map(String::as_str).collect::<HashSet<_>>();
                tools
                    .into_iter()
                    .filter(|tool| {
                        let server_allowed =
                            servers.is_empty() || servers.contains(server_name(tool));
                        let name = tool_name(tool).display();
                        let tool_allowed = names.is_empty() || names.contains(name.as_str());
                        server_allowed && tool_allowed
                    })
                    .collect()
            }
            McpToolPolicy::Deny {
                servers,
                tools: names,
            } => {
                let servers = servers.iter().map(String::as_str).collect::<HashSet<_>>();
                let names = names.iter().map(String::as_str).collect::<HashSet<_>>();
                tools
                    .into_iter()
                    .filter(|tool| {
                        let server_denied = servers.contains(server_name(tool));
                        let name = tool_name(tool).display();
                        let tool_denied = names.contains(name.as_str());
                        !(server_denied || tool_denied)
                    })
                    .collect()
            }
        }
    }

    pub(crate) fn filter_tool_specs(&self, specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
        specs
            .into_iter()
            .filter_map(|spec| filter_tool_spec(spec, self.builtins.as_ref()))
            .collect()
    }

    pub(crate) fn filter_configured_tool_specs(
        &self,
        specs: Vec<ConfiguredToolSpec>,
    ) -> Vec<ConfiguredToolSpec> {
        specs
            .into_iter()
            .filter_map(|configured| {
                filter_tool_spec(configured.spec, self.builtins.as_ref()).map(|spec| {
                    ConfiguredToolSpec::new(spec, configured.supports_parallel_tool_calls)
                })
            })
            .collect()
    }
}

fn filter_tool_set<T>(
    tools: Vec<T>,
    policy: Option<&ToolSetPolicy>,
    tool_name: impl Fn(&T) -> ToolName,
) -> Vec<T> {
    match policy.unwrap_or(&ToolSetPolicy::Inherit) {
        ToolSetPolicy::Inherit => tools,
        ToolSetPolicy::None => Vec::new(),
        ToolSetPolicy::AllowOnly(names) => {
            let names = names.iter().map(String::as_str).collect::<HashSet<_>>();
            tools
                .into_iter()
                .filter(|tool| names.contains(tool_name(tool).display().as_str()))
                .collect()
        }
        ToolSetPolicy::Deny(names) => {
            let names = names.iter().map(String::as_str).collect::<HashSet<_>>();
            tools
                .into_iter()
                .filter(|tool| !names.contains(tool_name(tool).display().as_str()))
                .collect()
        }
    }
}

fn filter_tool_spec(spec: ToolSpec, policy: Option<&ToolSetPolicy>) -> Option<ToolSpec> {
    let policy = policy.unwrap_or(&ToolSetPolicy::Inherit);
    match spec {
        ToolSpec::Namespace(namespace) => Some(ToolSpec::Namespace(namespace)),
        spec => {
            let name = spec.name().to_string();
            let affected = is_builtin_tool_name(&name);
            if !affected {
                return Some(spec);
            }
            let keep = match policy {
                ToolSetPolicy::Inherit => true,
                ToolSetPolicy::None => false,
                ToolSetPolicy::AllowOnly(names) => names.iter().any(|allowed| allowed == &name),
                ToolSetPolicy::Deny(names) => !names.iter().any(|denied| denied == &name),
            };
            keep.then_some(spec)
        }
    }
}

fn validate_named_tool_set(
    field_name: &str,
    policy: Option<&ToolSetPolicy>,
    is_known: impl Fn(&str) -> bool,
) -> Result<(), String> {
    let names = match policy {
        Some(ToolSetPolicy::AllowOnly(names) | ToolSetPolicy::Deny(names)) => names,
        Some(ToolSetPolicy::Inherit | ToolSetPolicy::None) | None => return Ok(()),
    };
    let unknown = names
        .iter()
        .filter(|name| !is_known(name))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{field_name} references unknown tool(s): {}",
            unknown.join(", ")
        ))
    }
}

pub fn is_builtin_tool_name(name: &str) -> bool {
    matches!(
        name,
        "exec"
            | "wait"
            | "exec_command"
            | "write_stdin"
            | "apply_patch"
            | "view_image"
            | "update_plan"
            | "get_goal"
            | "create_goal"
            | "update_goal"
            | "request_user_input"
            | "request_permissions"
            | "request_plugin_install"
            | "spawn_agent"
            | "send_input"
            | "send_message"
            | "followup_task"
            | "resume_agent"
            | "wait_agent"
            | "close_agent"
            | "list_agents"
            | "list_mcp_resources"
            | "list_mcp_resource_templates"
            | "read_mcp_resource"
            | "tool_search"
            | "tool_suggest"
            | "tool_router"
            | "web_search"
            | "image_generation"
            | "local_shell"
            | "shell"
            | "shell_command"
            | "container.exec"
            | "list_dir"
            | "test_sync_tool"
            | "spawn_agents_on_csv"
            | "report_agent_job_result"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_tools::JsonSchema;
    use codex_tools::ResponsesApiTool;
    use pretty_assertions::assert_eq;

    fn defaults() -> PromptContextDefaults {
        PromptContextDefaults {
            permissions: false,
            apps: true,
            skills: false,
            environment: true,
        }
    }

    #[test]
    fn current_preset_preserves_runtime_defaults() {
        let resolved = PromptContextPolicy::default().resolve(defaults());

        assert_eq!(
            resolved,
            ResolvedPromptContextPolicy {
                developer_instructions: true,
                developer_instruction_policy: InstructionPolicy::Inherit,
                user_instruction_policy: InstructionPolicy::Inherit,
                permissions: false,
                collaboration_mode: true,
                memories: true,
                apps: true,
                skills: false,
                plugins: true,
                commit_attribution: true,
                agents_md: true,
                environment: true,
                subagents: true,
                personality: true,
                realtime: true,
            }
        );
    }

    #[test]
    fn prompt_context_policy_defaults_to_strict() {
        assert_eq!(
            PromptContextPolicy::default(),
            PromptContextPolicy {
                preset: None,
                system_instructions: None,
                developer: None,
                user_context: None,
                strict: true,
            }
        );
    }

    #[test]
    fn workflow_preset_can_be_overridden_by_blocks_and_instruction_policies() {
        let resolved = PromptContextPolicy {
            preset: Some(PromptContextPreset::Workflow),
            system_instructions: Some(InstructionPolicy::Set("system".to_string())),
            developer: Some(DeveloperPromptPolicy {
                instructions: Some(InstructionPolicy::Set("developer".to_string())),
                blocks: DeveloperPromptBlocks {
                    permissions: Some(PromptBlockMode::Omit),
                    skills: Some(PromptBlockMode::Include),
                    ..Default::default()
                },
            }),
            user_context: Some(UserContextPromptPolicy {
                instructions: Some(InstructionPolicy::Set("user".to_string())),
                blocks: UserContextBlocks {
                    agents_md: Some(PromptBlockMode::Include),
                    environment: Some(PromptBlockMode::Omit),
                    ..Default::default()
                },
            }),
            strict: true,
        }
        .resolve(defaults());

        assert_eq!(
            resolved,
            ResolvedPromptContextPolicy {
                developer_instructions: true,
                developer_instruction_policy: InstructionPolicy::Set("developer".to_string()),
                user_instruction_policy: InstructionPolicy::Set("user".to_string()),
                permissions: false,
                collaboration_mode: false,
                memories: false,
                apps: false,
                skills: true,
                plugins: false,
                commit_attribution: false,
                agents_md: true,
                environment: false,
                subagents: false,
                personality: false,
                realtime: false,
            }
        );
    }

    #[test]
    fn tool_policy_filters_builtin_and_dynamic_tools() {
        let custom_tool = ToolSpec::Function(ResponsesApiTool {
            name: "custom_lookup".to_string(),
            description: "custom".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::default(),
            output_schema: None,
        });
        let policy = ToolPolicy {
            builtins: Some(ToolSetPolicy::None),
            dynamic: Some(ToolSetPolicy::AllowOnly(vec!["workflowsearch".to_string()])),
            ..Default::default()
        };

        let specs = policy.filter_tool_specs(vec![ToolSpec::LocalShell {}, custom_tool.clone()]);
        let dynamic_tools = policy.filter_dynamic_tools(
            vec![
                ("workflow", "search"),
                ("workflow", "write"),
                ("other", "search"),
            ],
            |(namespace, name)| ToolName::namespaced(*namespace, *name),
        );

        assert_eq!(specs, vec![custom_tool]);
        assert_eq!(dynamic_tools, vec![("workflow", "search")]);
    }

    #[test]
    fn tool_policy_validation_rejects_unknown_tools() {
        let known_builtin_policy = ToolPolicy {
            builtins: Some(ToolSetPolicy::AllowOnly(vec![
                "exec".to_string(),
                "wait".to_string(),
                "exec_command".to_string(),
                "request_plugin_install".to_string(),
                "tool_router".to_string(),
                "get_goal".to_string(),
                "create_goal".to_string(),
                "update_goal".to_string(),
            ])),
            ..Default::default()
        };
        assert_eq!(known_builtin_policy.validate_static(), Ok(()));

        let static_policy = ToolPolicy {
            builtins: Some(ToolSetPolicy::AllowOnly(vec![
                "exec_command".to_string(),
                "unknown_builtin".to_string(),
            ])),
            ..Default::default()
        };
        assert_eq!(
            static_policy.validate_static(),
            Err("toolPolicy.builtins references unknown tool(s): unknown_builtin".to_string())
        );

        let dynamic_policy = ToolPolicy {
            dynamic: Some(ToolSetPolicy::Deny(vec!["missing.tool".to_string()])),
            ..Default::default()
        };
        let dynamic_tools = vec![DynamicToolSpec {
            namespace: Some("workflow".to_string()),
            name: "search".to_string(),
            description: "Search workflow state".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            defer_loading: false,
        }];

        assert_eq!(
            dynamic_policy.validate_dynamic_tools(&dynamic_tools),
            Err("toolPolicy.dynamic references unknown tool(s): missing.tool".to_string())
        );
    }

    #[test]
    fn mcp_tool_policy_filters_by_server_and_tool_name() {
        let policy = ToolPolicy {
            mcp: Some(McpToolPolicy::Deny {
                servers: vec!["blocked".to_string()],
                tools: vec!["other.read".to_string()],
            }),
            ..Default::default()
        };
        let tools = vec![
            ("allowed", "allowed.read"),
            ("blocked", "blocked.read"),
            ("other", "other.read"),
        ];

        let filtered = policy.filter_mcp_tools(
            tools,
            |(server, _name)| *server,
            |(_server, name)| ToolName::plain(*name),
        );

        assert_eq!(filtered, vec![("allowed", "allowed.read")]);
    }
}
