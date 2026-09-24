use std::collections::HashMap;

use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputQuestion;
use codex_protocol::request_user_input::RequestUserInputQuestionOption;
use codex_protocol::request_user_input::RequestUserInputResponse;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::tasks::workflow_command::runtime::WORKFLOW_CONTROL_VERSION;

fn option(label: &str) -> RequestUserInputQuestionOption {
    RequestUserInputQuestionOption {
        label: label.to_string(),
        description: format!("Use {label}."),
    }
}

fn valid_request() -> RequestUserInputArgs {
    RequestUserInputArgs {
        questions: vec![
            RequestUserInputQuestion {
                id: "deploy_target".to_string(),
                header: "Target".to_string(),
                question: "Where should the workflow deploy?".to_string(),
                is_other: true,
                is_secret: false,
                options: Some(vec![option("Staging"), option("Production")]),
            },
            RequestUserInputQuestion {
                id: "release_note".to_string(),
                header: "Note".to_string(),
                question: "What should the release note say?".to_string(),
                is_other: false,
                is_secret: true,
                options: None,
            },
        ],
        is_blocking: true,
        auto_resolution_ms: None,
    }
}

fn rejected(args: RequestUserInputArgs, expected: &str) {
    assert_eq!(
        validate_user_input_request(args).expect_err("request should be rejected"),
        expected
    );
}

fn rejected_containing(args: RequestUserInputArgs, expected: &str) {
    let error = validate_user_input_request(args).expect_err("request should be rejected");
    assert!(
        error.contains(expected),
        "expected {error:?} to contain {expected:?}"
    );
}

fn frame_rejected(payload: &str, expected_id: u64, expected: &str) {
    let error = parse_control_request(payload, expected_id).expect_err("frame should be rejected");
    assert!(
        error.contains(expected),
        "expected {error:?} to contain {expected:?}"
    );
}

fn frame(version: u8, id: u64, method: &str) -> String {
    json!({
        "v": version,
        "id": id,
        "method": method,
        "params": valid_request(),
    })
    .to_string()
}

#[test]
fn valid_choice_and_freeform_request_is_preserved_exactly() {
    let expected = valid_request();
    let request = parse_control_request(
        &frame(WORKFLOW_CONTROL_VERSION, /*id*/ 1, "requestUserInput"),
        /*expected_request_id*/ 1,
    )
    .expect("control frame should parse");
    let args = serde_json::from_value::<RequestUserInputArgs>(request.params)
        .expect("request params should decode");

    assert_eq!(validate_user_input_request(args), Ok(expected));
}

#[test]
fn duplicate_question_ids_and_choice_labels_are_rejected() {
    let mut duplicate_id = valid_request();
    duplicate_id.questions[1].id = duplicate_id.questions[0].id.clone();
    let mut duplicate_label = valid_request();
    duplicate_label.questions[0]
        .options
        .as_mut()
        .expect("choice question should have options")[1]
        .label = " Staging ".to_string();
    let mut reserved_other = valid_request();
    reserved_other.questions[0]
        .options
        .as_mut()
        .expect("choice question should have options")[1]
        .label = OTHER_OPTION_LABEL.to_string();
    let mut reserved_note_prefix = valid_request();
    reserved_note_prefix.questions[0]
        .options
        .as_mut()
        .expect("choice question should have options")[1]
        .label = "user_note: authored label".to_string();

    let cases = [
        (
            duplicate_id,
            "duplicate requestUserInput question id `deploy_target`",
        ),
        (
            duplicate_label,
            "requestUserInput question `deploy_target` has duplicate option label ` Staging `",
        ),
        (
            reserved_other,
            "requestUserInput question `deploy_target` uses reserved option label `None of the above`",
        ),
        (
            reserved_note_prefix,
            "requestUserInput question `deploy_target` uses reserved option-label prefix `user_note: `",
        ),
    ];
    for (args, expected) in cases {
        rejected(args, expected);
    }
}

#[test]
fn response_is_validated_and_missing_questions_are_canonicalized() {
    let response = RequestUserInputResponse {
        answers: HashMap::from([(
            "deploy_target".to_string(),
            RequestUserInputAnswer {
                answers: vec!["Staging".to_string()],
            },
        )]),
    };
    let validated =
        validate_user_input_response(&valid_request(), response).expect("response should be valid");
    assert_eq!(
        validated,
        RequestUserInputResponse {
            answers: HashMap::from([
                (
                    "deploy_target".to_string(),
                    RequestUserInputAnswer {
                        answers: vec!["Staging".to_string()],
                    },
                ),
                (
                    "release_note".to_string(),
                    RequestUserInputAnswer {
                        answers: Vec::new(),
                    },
                ),
            ]),
        }
    );

    let invalid = RequestUserInputResponse {
        answers: HashMap::from([(
            "deploy_target".to_string(),
            RequestUserInputAnswer {
                answers: vec!["Unknown".to_string()],
            },
        )]),
    };
    assert_eq!(
        validate_user_input_response(&valid_request(), invalid)
            .expect_err("unknown option should be rejected"),
        "requestUserInput response for `deploy_target` does not match its question"
    );

    let unknown_id = RequestUserInputResponse {
        answers: HashMap::from([
            (
                "unknown_z".to_string(),
                RequestUserInputAnswer {
                    answers: Vec::new(),
                },
            ),
            (
                "unknown_a".to_string(),
                RequestUserInputAnswer {
                    answers: Vec::new(),
                },
            ),
        ]),
    };
    assert_eq!(
        validate_user_input_response(&valid_request(), unknown_id)
            .expect_err("unknown question should be rejected"),
        "requestUserInput response contains unknown question id `unknown_a`"
    );

    let other = RequestUserInputResponse {
        answers: HashMap::from([(
            "deploy_target".to_string(),
            RequestUserInputAnswer {
                answers: vec!["user_note: another target".to_string()],
            },
        )]),
    };
    assert_eq!(
        validate_user_input_response(&valid_request(), other)
            .expect("note-only Other should be normalized")
            .answers["deploy_target"]
            .answers,
        vec![
            OTHER_OPTION_LABEL.to_string(),
            "user_note: another target".to_string()
        ]
    );
}

#[test]
fn auto_resolution_is_rejected() {
    let mut args = valid_request();
    args.auto_resolution_ms = Some(/*auto_resolution_ms*/ 60_000);

    rejected(args, "requestUserInput does not support autoResolutionMs");
    let mut params = serde_json::to_value(valid_request()).expect("request should serialize");
    params["autoResolutionMs"] = Value::Null;
    assert_eq!(
        decode_user_input_request(params).expect_err("auto resolution should be rejected"),
        "requestUserInput does not support autoResolutionMs"
    );
}

#[test]
fn non_blocking_requests_are_rejected() {
    let mut args = valid_request();
    args.is_blocking = false;

    rejected(args, "requestUserInput requires isBlocking to be true");
}

#[test]
fn question_and_option_bounds_are_enforced() {
    let empty = RequestUserInputArgs {
        questions: Vec::new(),
        is_blocking: true,
        auto_resolution_ms: None,
    };
    let mut invalid_id = valid_request();
    invalid_id.questions[0].id = "Not-Snake-Case".to_string();
    let mut long_header = valid_request();
    long_header.questions[0].header = "h".repeat(MAX_WORKFLOW_QUESTION_HEADER_CHARS + 1);
    let mut too_many_options = valid_request();
    too_many_options.questions[0].options = Some(
        (0..=MAX_WORKFLOW_OPTIONS)
            .map(|index| option(&format!("Option {index}")))
            .collect(),
    );
    rejected_containing(empty, "one to three questions");
    rejected_containing(invalid_id, "must be snake_case");
    rejected_containing(long_header, "question header exceeds");
    rejected_containing(too_many_options, "exceeds the limit");
}

#[test]
fn invalid_control_frames_are_rejected() {
    frame_rejected(
        "{",
        /*expected_id*/ 1,
        "invalid workflow control frame",
    );
    frame_rejected(
        &frame(
            WORKFLOW_CONTROL_VERSION + 1,
            /*id*/ 1,
            "requestUserInput",
        ),
        /*expected_id*/ 1,
        "unsupported workflow control version",
    );
    frame_rejected(
        &frame(WORKFLOW_CONTROL_VERSION, /*id*/ 1, "unknown"),
        /*expected_id*/ 1,
        "unsupported workflow control method",
    );
    frame_rejected(
        &frame(WORKFLOW_CONTROL_VERSION, /*id*/ 1, "requestUserInput"),
        /*expected_id*/ 2,
        "was out of order",
    );

    let oversized = "x".repeat(MAX_WORKFLOW_CONTROL_FRAME_BYTES + 1);
    assert_eq!(
        parse_control_request(&oversized, /*expected_request_id*/ 1)
            .expect_err("oversized frame should be rejected"),
        format!("workflow control frame exceeded {MAX_WORKFLOW_CONTROL_FRAME_BYTES} bytes")
    );
}

#[test]
fn request_count_boundary_is_enforced() {
    let last_allowed = parse_control_request(
        &frame(WORKFLOW_CONTROL_VERSION, /*id*/ 64, "requestUserInput"),
        /*expected_request_id*/ 64,
    )
    .expect("request 64 should be accepted");
    assert_eq!(last_allowed.id, 64);

    frame_rejected(
        &frame(WORKFLOW_CONTROL_VERSION, /*id*/ 65, "requestUserInput"),
        /*expected_id*/ 65,
        "exceeded the limit of 64 user input requests",
    );
}

#[test]
fn completion_frame_preserves_escaped_markdown_within_output_cap() {
    let markdown = "\"\n".repeat(20_000);
    let payload = json!({
        "v": WORKFLOW_CONTROL_VERSION,
        "id": 0,
        "method": "complete",
        "params": { "markdown": markdown },
    })
    .to_string();

    assert!(payload.len() > crate::tasks::workflow_command::WORKFLOW_OUTPUT_MAX_BYTES);
    assert!(markdown.len() <= crate::tasks::workflow_command::WORKFLOW_OUTPUT_MAX_BYTES);
    assert_eq!(
        parse_completion(&payload).expect("completion frame should parse"),
        markdown
    );
}

#[test]
fn unsupported_interaction_fields_are_rejected() {
    let mut params = serde_json::to_value(valid_request()).expect("request should serialize");
    params["questions"][0]["multiSelect"] = Value::Bool(true);

    assert_eq!(
        decode_user_input_request(params).expect_err("multi-select should be rejected"),
        "requestUserInput question 0 contains unsupported field `multiSelect`"
    );
}
