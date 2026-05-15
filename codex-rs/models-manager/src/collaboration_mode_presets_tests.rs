use super::*;
use pretty_assertions::assert_eq;

#[test]
fn preset_names_use_mode_display_names() {
    assert_eq!(plan_preset().name, ModeKind::Plan.display_name());
    assert_eq!(workflow_preset().name, ModeKind::Workflow.display_name());
    assert_eq!(
        default_preset(CollaborationModesConfig::default()).name,
        ModeKind::Default.display_name()
    );
    assert_eq!(plan_preset().model, None);
    assert_eq!(
        plan_preset().reasoning_effort,
        Some(Some(ReasoningEffort::Medium))
    );
    assert_eq!(workflow_preset().model, None);
    assert_eq!(
        workflow_preset().reasoning_effort,
        Some(Some(ReasoningEffort::Medium))
    );
    assert_eq!(
        default_preset(CollaborationModesConfig::default()).model,
        None
    );
    assert_eq!(
        default_preset(CollaborationModesConfig::default()).reasoning_effort,
        None
    );
}

#[test]
fn workflow_preset_is_feature_gated() {
    assert_eq!(
        builtin_collaboration_mode_presets(CollaborationModesConfig::default())
            .into_iter()
            .map(|preset| preset.mode)
            .collect::<Vec<_>>(),
        vec![Some(ModeKind::Plan), Some(ModeKind::Default)]
    );

    assert_eq!(
        builtin_collaboration_mode_presets(CollaborationModesConfig {
            workflows_enabled: true,
            ..Default::default()
        })
        .into_iter()
        .map(|preset| preset.mode)
        .collect::<Vec<_>>(),
        vec![
            Some(ModeKind::Plan),
            Some(ModeKind::Workflow),
            Some(ModeKind::Default)
        ]
    );
}

#[test]
fn default_mode_instructions_replace_mode_names_placeholder() {
    let default_instructions = default_preset(CollaborationModesConfig::default())
        .developer_instructions
        .expect("default preset should include instructions")
        .expect("default instructions should be set");

    assert!(!default_instructions.contains("{{KNOWN_MODE_NAMES}}"));

    let visible_modes = visible_modes_for_config(CollaborationModesConfig::default());
    let known_mode_names = format_mode_names(&visible_modes);
    let expected_snippet = format!("Known mode names are {known_mode_names}.");
    assert!(default_instructions.contains(&expected_snippet));

    assert!(
        default_instructions
            .contains("The `request_user_input` tool is unavailable in Default mode.")
    );
    assert!(
        default_instructions.contains("ask the user directly with a concise plain-text question")
    );
}

#[test]
fn default_mode_instructions_include_workflow_name_when_enabled() {
    let config = CollaborationModesConfig {
        workflows_enabled: true,
        ..Default::default()
    };
    let default_instructions = default_preset(config)
        .developer_instructions
        .expect("default preset should include instructions")
        .expect("default instructions should be set");

    let visible_modes = visible_modes_for_config(config);
    let known_mode_names = format_mode_names(&visible_modes);
    let expected_snippet = format!("Known mode names are {known_mode_names}.");
    assert!(default_instructions.contains(&expected_snippet));
    assert!(default_instructions.contains("Workflow"));
}

#[test]
fn workflow_mode_instructions_are_workflow_specialist_guidance() {
    let workflow_instructions = workflow_preset()
        .developer_instructions
        .expect("workflow preset should include instructions")
        .expect("workflow instructions should be set");

    assert!(workflow_instructions.contains(
        "Workflow mode exists to design, inspect, tune, validate, repair, and explain Codex workflows."
    ));
    assert!(workflow_instructions.contains(
        "Do not bounce the request back with a meta question like \"can you develop a workflow for me\"."
    ));
    assert!(workflow_instructions.contains("/workflow list"));
    assert!(
        workflow_instructions
            .contains("Do not use broad file search, web search, or unrelated repo spelunking")
    );
}
