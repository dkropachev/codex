use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn websocket_safety_buffering_uses_event_before_header_fallback() {
    let metadata: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "codex.response.metadata",
        "headers": {
            "x-codex-safety-buffering-enabled": "true",
            "x-codex-safety-buffering-faster-model": "gpt-fast-header"
        }
    }))
    .expect("deserialize treatment metadata");
    let event: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.output_text.delta",
        "safety_buffering": {
            "use_cases": ["cyber"],
            "reasons": ["user_risk"],
            "retry_model": "gpt-fast-wire"
        }
    }))
    .expect("deserialize safety buffering event");
    let mut treatment = SafetyBufferingTreatment::default();

    assert!(safety_buffering_for_event(&metadata, &mut treatment).is_none());
    let buffering = safety_buffering_for_event(&event, &mut treatment)
        .expect("expected safety buffering payload");

    assert_eq!(
        buffering,
        crate::common::SafetyBuffering {
            use_cases: vec!["cyber".to_string()],
            reasons: vec!["user_risk".to_string()],
            show_buffering_ui: true,
            faster_model: Some("gpt-fast-wire".to_string()),
        }
    );
}

#[test]
fn websocket_safety_buffering_event_controls_visibility_when_header_disables_it() {
    let metadata: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "codex.response.metadata",
        "headers": {
            "x-codex-safety-buffering-enabled": "false",
            "x-codex-safety-buffering-faster-model": "gpt-fast-header"
        }
    }))
    .expect("deserialize treatment metadata");
    let event: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.output_text.delta",
        "safety_buffering": {
            "use_cases": ["cyber"],
            "reasons": ["user_risk"]
        }
    }))
    .expect("deserialize safety buffering event");
    let mut treatment = SafetyBufferingTreatment::default();

    assert!(safety_buffering_for_event(&metadata, &mut treatment).is_none());
    let buffering = safety_buffering_for_event(&event, &mut treatment)
        .expect("expected safety buffering payload");

    assert_eq!(
        buffering,
        crate::common::SafetyBuffering {
            use_cases: vec!["cyber".to_string()],
            reasons: vec!["user_risk".to_string()],
            show_buffering_ui: true,
            faster_model: Some("gpt-fast-header".to_string()),
        }
    );
}
