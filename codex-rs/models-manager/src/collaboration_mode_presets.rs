use codex_collaboration_mode_templates::WORKFLOW as COLLABORATION_MODE_WORKFLOW;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::TUI_VISIBLE_COLLABORATION_MODES;
use codex_protocol::openai_models::ReasoningEffort;
use codex_utils_template::Template;
use std::sync::LazyLock;

const KNOWN_MODE_NAMES_TEMPLATE_KEY: &str = "KNOWN_MODE_NAMES";
const REQUEST_USER_INPUT_AVAILABILITY_TEMPLATE_KEY: &str = "REQUEST_USER_INPUT_AVAILABILITY";
const ASKING_QUESTIONS_GUIDANCE_TEMPLATE_KEY: &str = "ASKING_QUESTIONS_GUIDANCE";
static COLLABORATION_MODE_DEFAULT_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    Template::parse(
        ResolvedModelMessages::bundled()
            .collaboration_modes()
            .default
            .text(),
    )
    .unwrap_or_else(|err| panic!("collaboration mode default template must parse: {err}"))
});

/// Feature flags that control collaboration-mode preset behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CollaborationModesConfig {
    /// Enables `request_user_input` availability in Default mode.
    pub default_mode_request_user_input: bool,
    /// Enables the Workflow collaboration mode preset.
    pub workflows_enabled: bool,
}

pub fn builtin_collaboration_mode_presets(
    collaboration_modes_config: CollaborationModesConfig,
) -> Vec<CollaborationModeMask> {
    let mut presets = vec![plan_preset()];
    if collaboration_modes_config.workflows_enabled {
        presets.push(workflow_preset());
    }
    presets.push(default_preset(collaboration_modes_config));
    presets
}

fn plan_preset() -> CollaborationModeMask {
    CollaborationModeMask {
        name: ModeKind::Plan.display_name().to_string(),
        mode: Some(ModeKind::Plan),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(
            ResolvedModelMessages::bundled()
                .collaboration_modes()
                .plan
                .text()
                .to_string(),
        )),
    }
}

fn workflow_preset() -> CollaborationModeMask {
    CollaborationModeMask {
        name: ModeKind::Workflow.display_name().to_string(),
        mode: Some(ModeKind::Workflow),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(COLLABORATION_MODE_WORKFLOW.to_string())),
    }
}

fn default_preset(collaboration_modes_config: CollaborationModesConfig) -> CollaborationModeMask {
    CollaborationModeMask {
        name: ModeKind::Default.display_name().to_string(),
        mode: Some(ModeKind::Default),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(Some(default_mode_instructions(collaboration_modes_config))),
    }
}

fn default_mode_instructions(collaboration_modes_config: CollaborationModesConfig) -> String {
    let visible_modes = visible_modes_for_config(collaboration_modes_config);
    let known_mode_names = format_mode_names(&visible_modes);
    let request_user_input_availability = request_user_input_availability_message(
        collaboration_modes_config.default_mode_request_user_input,
    );
    let asking_questions_guidance = asking_questions_guidance_message(
        collaboration_modes_config.default_mode_request_user_input,
    );
    COLLABORATION_MODE_DEFAULT_TEMPLATE
        .render([
            (KNOWN_MODE_NAMES_TEMPLATE_KEY, known_mode_names.as_str()),
            (
                REQUEST_USER_INPUT_AVAILABILITY_TEMPLATE_KEY,
                request_user_input_availability.as_str(),
            ),
            (
                ASKING_QUESTIONS_GUIDANCE_TEMPLATE_KEY,
                asking_questions_guidance.as_str(),
            ),
        ])
        .unwrap_or_else(|err| panic!("collaboration mode default template must render: {err}"))
}

fn visible_modes_for_config(collaboration_modes_config: CollaborationModesConfig) -> Vec<ModeKind> {
    TUI_VISIBLE_COLLABORATION_MODES
        .into_iter()
        .filter(|mode| *mode != ModeKind::Workflow || collaboration_modes_config.workflows_enabled)
        .collect()
}

fn format_mode_names(modes: &[ModeKind]) -> String {
    let mode_names: Vec<&str> = modes.iter().map(|mode| mode.display_name()).collect();
    match mode_names.as_slice() {
        [] => "none".to_string(),
        [mode_name] => (*mode_name).to_string(),
        [first, second] => format!("{first} and {second}"),
        [..] => mode_names.join(", "),
    }
}

fn request_user_input_availability_message(default_mode_request_user_input: bool) -> String {
    let availability = if default_mode_request_user_input {
        "The `request_user_input` tool is available in Default mode."
    } else {
        "The `request_user_input` tool is unavailable in Default mode. If you call it while in Default mode, it will return an error."
    };
    format!(
        "Use the `request_user_input` tool only when it is listed in the available tools for this turn.\n\n{availability}"
    )
}

fn asking_questions_guidance_message(default_mode_request_user_input: bool) -> String {
    if default_mode_request_user_input {
        "In Default mode, strongly prefer making reasonable assumptions and executing the user's request rather than stopping to ask questions.\n\nUse the `request_user_input` tool only for optional questions where the answer would materially improve the quality of the work.\n\nIf `request_user_input` returns no answers, continue with best judgment instead of asking again or treating the turn as blocked.\n\nNever use the `request_user_input` tool for permission requests or permission-related escalations.\n\nIf explicit user input is required for another reason before progress can safely continue, do not use the `request_user_input` tool. Ask the user directly with one concise plain-text question instead. Never write a multiple choice question as a textual assistant message.".to_string()
    } else {
        "In Default mode, strongly prefer making reasonable assumptions and executing the user's request rather than stopping to ask questions.\n\nIf explicit user input is required before progress can safely continue, pause. Ask the user directly with one concise plain-text question instead. Never write a multiple choice question as a textual assistant message.".to_string()
    }
}

#[cfg(test)]
#[path = "collaboration_mode_presets_tests.rs"]
mod tests;
