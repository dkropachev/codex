use super::*;

use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn request_user_input_args_defaults_legacy_missing_is_blocking_to_true() {
    let args: RequestUserInputArgs = serde_json::from_value(json!({
        "questions": [{
            "id": "q1",
            "header": "Confirm",
            "question": "Continue?",
            "options": [{
                "label": "Yes",
                "description": "Continue."
            }]
        }]
    }))
    .expect("legacy request_user_input args should deserialize");

    assert!(args.is_blocking);
}

#[test]
fn request_user_input_args_defaults_legacy_timed_request_to_non_blocking() {
    let args: RequestUserInputArgs = serde_json::from_value(json!({
        "questions": [],
        "autoResolutionMs": 60_000
    }))
    .expect("legacy timed request_user_input args should deserialize");

    assert_eq!(
        args,
        RequestUserInputArgs {
            questions: Vec::new(),
            is_blocking: false,
            auto_resolution_ms: Some(60_000),
        }
    );
}

#[test]
fn request_user_input_event_defaults_legacy_timed_request_to_non_blocking() {
    let event: RequestUserInputEvent = serde_json::from_value(json!({
        "call_id": "call-1",
        "turn_id": "turn-1",
        "questions": [{
            "id": "q1",
            "header": "Confirm",
            "question": "Continue?",
            "options": [{
                "label": "Yes",
                "description": "Continue."
            }]
        }],
        "autoResolutionMs": 60_000
    }))
    .expect("legacy request_user_input event should deserialize");

    assert_eq!(
        event,
        RequestUserInputEvent {
            call_id: "call-1".to_string(),
            turn_id: "turn-1".to_string(),
            questions: vec![RequestUserInputQuestion {
                id: "q1".to_string(),
                header: "Confirm".to_string(),
                question: "Continue?".to_string(),
                is_other: false,
                is_secret: false,
                options: Some(vec![RequestUserInputQuestionOption {
                    label: "Yes".to_string(),
                    description: "Continue.".to_string(),
                }]),
            }],
            is_blocking: false,
            auto_resolution_ms: Some(60_000),
        }
    );
}

#[test]
fn request_user_input_event_preserves_explicit_is_blocking_with_timer() {
    let event: RequestUserInputEvent = serde_json::from_value(json!({
        "call_id": "call-1",
        "questions": [],
        "isBlocking": true,
        "autoResolutionMs": 60_000
    }))
    .expect("request_user_input event should deserialize");

    assert_eq!(
        event,
        RequestUserInputEvent {
            call_id: "call-1".to_string(),
            turn_id: String::new(),
            questions: Vec::new(),
            is_blocking: true,
            auto_resolution_ms: Some(60_000),
        }
    );
}
