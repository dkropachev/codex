use super::*;

#[test]
fn truncates_workflow_output_at_byte_cap() {
    let text = "a".repeat(WORKFLOW_OUTPUT_MAX_BYTES + 32);

    let truncated = truncate_workflow_output(text);

    assert_eq!(truncated.len(), WORKFLOW_OUTPUT_MAX_BYTES);
    assert!(truncated.ends_with("[Workflow output truncated to 40960 bytes.]"));
}

#[test]
fn truncates_workflow_output_on_char_boundary() {
    let mut text = "a".repeat(WORKFLOW_OUTPUT_MAX_BYTES - 1);
    text.push('é');

    let truncated = truncate_workflow_output(text);

    assert_eq!(truncated.len(), WORKFLOW_OUTPUT_MAX_BYTES);
    assert!(truncated.is_char_boundary(truncated.len()));
    assert!(truncated.ends_with("[Workflow output truncated to 40960 bytes.]"));
}
