use std::path::Path;

use codex_protocol::config_types::ModeKind;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::*;

#[test]
fn config_plan_mask_uses_plan_wire_mode_with_config_name() {
    let codex_home =
        AbsolutePathBuf::from_absolute_path("/codex-home").expect("absolute codex home");
    let mask = config_plan_mask(Path::new("/target"), &codex_home);

    assert_eq!(mask.name, CONFIG_MODE_NAME);
    assert_eq!(mask.mode, Some(ModeKind::Plan));
    assert!(is_config_mask(Some(&mask)));
    assert!(
        mask.developer_instructions
            .as_ref()
            .and_then(|value| value.as_ref())
            .is_some_and(|instructions| instructions.contains("Bare /config enters Config mode"))
    );
    let instructions = mask
        .developer_instructions
        .as_ref()
        .and_then(|value| value.as_ref())
        .expect("config mode instructions");
    assert!(instructions.contains("only when the config change is decision-complete"));
    assert!(instructions.contains("Proposed diffs"));
    assert!(instructions.contains("one unified diff code block for each config file"));
    assert!(instructions.contains("Prefer bounded config inspection"));
}

#[test]
fn config_context_stays_bounded() {
    let codex_home =
        AbsolutePathBuf::from_absolute_path("/codex-home").expect("absolute codex home");
    let mask = config_plan_mask(Path::new("/target"), &codex_home);
    let instructions = mask
        .developer_instructions
        .as_ref()
        .and_then(|value| value.as_ref())
        .expect("config mode instructions");

    assert!(instructions.len() <= MAX_CONFIG_CONTEXT_BYTES + 128);
}
