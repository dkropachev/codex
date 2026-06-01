use codex_app_server_protocol as api;
use codex_core::config::ConfigOverrides;
use codex_core::prompt_context as core;

pub(crate) fn prompt_context_policy_to_core(
    policy: api::PromptContextPolicy,
    allow_system_instructions: bool,
) -> Result<core::PromptContextPolicy, String> {
    if !allow_system_instructions && policy.system_instructions.is_some() {
        return Err(
            "promptContext.systemInstructions is only supported at thread start, thread resume, or thread/promptContext/update"
                .to_string(),
        );
    }
    let system_instructions = policy.system_instructions.map(instruction_policy_to_core);
    if matches!(system_instructions, Some(core::InstructionPolicy::Omit)) {
        return Err("promptContext.systemInstructions cannot be omitted".to_string());
    }

    Ok(core::PromptContextPolicy {
        preset: policy.preset.map(prompt_context_preset_to_core),
        system_instructions,
        developer: policy.developer.map(developer_prompt_policy_to_core),
        user_context: policy.user_context.map(user_context_prompt_policy_to_core),
        strict: policy.strict,
    })
}

pub(crate) fn tool_policy_to_core(policy: api::ToolPolicy) -> Result<core::ToolPolicy, String> {
    let policy = core::ToolPolicy {
        builtins: policy.builtins.map(tool_set_policy_to_core),
        mcp: policy.mcp.map(mcp_tool_policy_to_core),
        dynamic: policy.dynamic.map(tool_set_policy_to_core),
        tool_router: policy.tool_router.map(tool_router_policy_to_core),
    };
    policy.validate_static()?;
    Ok(policy)
}

pub(crate) fn apply_system_instruction_override(
    overrides: &mut ConfigOverrides,
    policy: &core::PromptContextPolicy,
) {
    if let Some(core::InstructionPolicy::Set(text)) = &policy.system_instructions {
        overrides.base_instructions = Some(text.clone());
    }
}

fn prompt_context_preset_to_core(preset: api::PromptContextPreset) -> core::PromptContextPreset {
    match preset {
        api::PromptContextPreset::Current => core::PromptContextPreset::Current,
        api::PromptContextPreset::Workflow => core::PromptContextPreset::Workflow,
        api::PromptContextPreset::Minimal => core::PromptContextPreset::Minimal,
        api::PromptContextPreset::Isolated => core::PromptContextPreset::Isolated,
    }
}

fn prompt_block_mode_to_core(mode: api::PromptBlockMode) -> core::PromptBlockMode {
    match mode {
        api::PromptBlockMode::Inherit => core::PromptBlockMode::Inherit,
        api::PromptBlockMode::Include => core::PromptBlockMode::Include,
        api::PromptBlockMode::Omit => core::PromptBlockMode::Omit,
    }
}

fn instruction_policy_to_core(policy: api::InstructionPolicy) -> core::InstructionPolicy {
    match policy {
        api::InstructionPolicy::Inherit => core::InstructionPolicy::Inherit,
        api::InstructionPolicy::Omit => core::InstructionPolicy::Omit,
        api::InstructionPolicy::Set { text } => core::InstructionPolicy::Set(text),
    }
}

fn developer_prompt_policy_to_core(
    policy: api::DeveloperPromptPolicy,
) -> core::DeveloperPromptPolicy {
    core::DeveloperPromptPolicy {
        instructions: policy.instructions.map(instruction_policy_to_core),
        blocks: policy
            .blocks
            .map(developer_blocks_to_core)
            .unwrap_or_default(),
    }
}

fn user_context_prompt_policy_to_core(
    policy: api::UserContextPromptPolicy,
) -> core::UserContextPromptPolicy {
    core::UserContextPromptPolicy {
        instructions: policy.instructions.map(instruction_policy_to_core),
        blocks: policy
            .blocks
            .map(user_context_blocks_to_core)
            .unwrap_or_default(),
    }
}

fn developer_blocks_to_core(blocks: api::DeveloperPromptBlocks) -> core::DeveloperPromptBlocks {
    core::DeveloperPromptBlocks {
        permissions: blocks.permissions.map(prompt_block_mode_to_core),
        collaboration_mode: blocks.collaboration_mode.map(prompt_block_mode_to_core),
        memories: blocks.memories.map(prompt_block_mode_to_core),
        apps: blocks.apps.map(prompt_block_mode_to_core),
        skills: blocks.skills.map(prompt_block_mode_to_core),
        plugins: blocks.plugins.map(prompt_block_mode_to_core),
        commit_attribution: blocks.commit_attribution.map(prompt_block_mode_to_core),
        personality: blocks.personality.map(prompt_block_mode_to_core),
        realtime: blocks.realtime.map(prompt_block_mode_to_core),
    }
}

fn user_context_blocks_to_core(blocks: api::UserContextBlocks) -> core::UserContextBlocks {
    core::UserContextBlocks {
        agents_md: blocks.agents_md.map(prompt_block_mode_to_core),
        environment: blocks.environment.map(prompt_block_mode_to_core),
        subagents: blocks.subagents.map(prompt_block_mode_to_core),
    }
}

fn tool_set_policy_to_core(policy: api::ToolSetPolicy) -> core::ToolSetPolicy {
    match policy {
        api::ToolSetPolicy::Inherit => core::ToolSetPolicy::Inherit,
        api::ToolSetPolicy::None => core::ToolSetPolicy::None,
        api::ToolSetPolicy::AllowOnly { tools } => core::ToolSetPolicy::AllowOnly(tools),
        api::ToolSetPolicy::Deny { tools } => core::ToolSetPolicy::Deny(tools),
    }
}

fn mcp_tool_policy_to_core(policy: api::McpToolPolicy) -> core::McpToolPolicy {
    match policy {
        api::McpToolPolicy::Inherit => core::McpToolPolicy::Inherit,
        api::McpToolPolicy::None => core::McpToolPolicy::None,
        api::McpToolPolicy::AllowOnly { servers, tools } => {
            core::McpToolPolicy::AllowOnly { servers, tools }
        }
        api::McpToolPolicy::Deny { servers, tools } => core::McpToolPolicy::Deny { servers, tools },
    }
}

fn tool_router_policy_to_core(policy: api::ToolRouterPolicy) -> core::ToolRouterPolicy {
    match policy {
        api::ToolRouterPolicy::Inherit => core::ToolRouterPolicy::Inherit,
        api::ToolRouterPolicy::Off => core::ToolRouterPolicy::Off,
    }
}
