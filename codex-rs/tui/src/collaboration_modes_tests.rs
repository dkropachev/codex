use super::*;
use codex_models_manager::collaboration_mode_presets::CollaborationModesConfig;
use pretty_assertions::assert_eq;

#[test]
fn next_mask_hides_workflow_when_feature_is_disabled() {
    let model_catalog = ModelCatalog::new(Vec::new());

    let first = next_mask_with_config(
        &model_catalog,
        /*current*/ None,
        CollaborationModesConfig::default(),
    )
    .expect("default presets should include plan");
    let second = next_mask_with_config(
        &model_catalog,
        Some(&first),
        CollaborationModesConfig::default(),
    )
    .expect("default presets should include default");

    assert_eq!(
        vec![Some(ModeKind::Plan), Some(ModeKind::Default)],
        vec![first.mode, second.mode]
    );
}

#[test]
fn next_mask_includes_workflow_when_feature_is_enabled() {
    let model_catalog = ModelCatalog::new(Vec::new());
    let config = CollaborationModesConfig {
        workflows_enabled: true,
        ..Default::default()
    };

    let first = next_mask_with_config(&model_catalog, /*current*/ None, config)
        .expect("workflow-enabled presets should include plan");
    let second = next_mask_with_config(&model_catalog, Some(&first), config)
        .expect("workflow-enabled presets should include workflow");
    let third = next_mask_with_config(&model_catalog, Some(&second), config)
        .expect("workflow-enabled presets should include default");

    assert_eq!(
        vec![
            Some(ModeKind::Plan),
            Some(ModeKind::Workflow),
            Some(ModeKind::Default)
        ],
        vec![first.mode, second.mode, third.mode]
    );
}
