use super::*;

#[test]
fn truncates_workflow_output_at_byte_cap() {
    let run_config = WorkflowRunConfig::default();
    let output_max_bytes = run_config.output_max_bytes();
    let text = "a".repeat(output_max_bytes + 32);

    let truncated = truncate_workflow_output(text, run_config);

    assert_eq!(truncated.len(), output_max_bytes);
    assert!(truncated.ends_with(&format!(
        "[Workflow output truncated to {output_max_bytes} bytes.]"
    )));
}

#[test]
fn truncates_workflow_output_on_char_boundary() {
    let output_max_bytes = 128;
    let run_config =
        WorkflowRunConfig::new(output_max_bytes).expect("custom output limit should be valid");
    let mut text = "a".repeat(output_max_bytes - 1);
    text.push('é');

    let truncated = truncate_workflow_output(text, run_config);

    assert_eq!(truncated.len(), output_max_bytes);
    assert!(truncated.is_char_boundary(truncated.len()));
    assert!(truncated.ends_with(&format!(
        "[Workflow output truncated to {output_max_bytes} bytes.]"
    )));
}
