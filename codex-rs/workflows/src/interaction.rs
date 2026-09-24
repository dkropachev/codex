use std::collections::HashMap;
use std::collections::HashSet;

use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputResponse;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;

use crate::runner::MAX_WORKFLOW_CONTROL_FRAME_BYTES;
use crate::runner::WORKFLOW_CONTROL_VERSION;
use crate::runner::WORKFLOW_OUTPUT_MAX_BYTES;

pub(super) const MAX_WORKFLOW_QUESTION_HEADER_CHARS: usize = 12;
pub(super) const MAX_WORKFLOW_OPTIONS: usize = 10;
pub const MAX_WORKFLOW_USER_INPUT_REQUESTS: u64 = 64;
const MAX_WORKFLOW_QUESTION_ID_CHARS: usize = 64;
const MAX_WORKFLOW_QUESTION_CHARS: usize = 1_024;
const MAX_WORKFLOW_OPTION_LABEL_CHARS: usize = 80;
const MAX_WORKFLOW_OPTION_DESCRIPTION_CHARS: usize = 512;
const OTHER_OPTION_LABEL: &str = "None of the above";
const USER_NOTE_PREFIX: &str = "user_note: ";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowControlRequest {
    v: u8,
    pub id: u64,
    method: String,
    pub params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowCompletion {
    v: u8,
    id: u64,
    method: String,
    params: WorkflowCompletionParams,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowCompletionParams {
    markdown: String,
}

pub fn parse_completion(payload: &str) -> Result<String, String> {
    let completion = serde_json::from_str::<WorkflowCompletion>(payload)
        .map_err(|err| format!("invalid workflow completion frame: {err}"))?;
    if completion.v != WORKFLOW_CONTROL_VERSION
        || completion.id != 0
        || completion.method != "complete"
    {
        return Err("invalid workflow completion frame header".to_string());
    }
    if completion.params.markdown.len() > WORKFLOW_OUTPUT_MAX_BYTES {
        return Err(format!(
            "workflow markdown exceeded {WORKFLOW_OUTPUT_MAX_BYTES} bytes"
        ));
    }
    Ok(completion.params.markdown)
}

pub fn parse_control_request(
    payload: &str,
    expected_request_id: u64,
) -> Result<WorkflowControlRequest, String> {
    if payload.len() > MAX_WORKFLOW_CONTROL_FRAME_BYTES {
        return Err(format!(
            "workflow control frame exceeded {MAX_WORKFLOW_CONTROL_FRAME_BYTES} bytes"
        ));
    }
    let request = serde_json::from_str::<WorkflowControlRequest>(payload)
        .map_err(|err| format!("invalid workflow control frame: {err}"))?;
    if request.v != WORKFLOW_CONTROL_VERSION {
        return Err(format!(
            "unsupported workflow control version {}; expected {WORKFLOW_CONTROL_VERSION}",
            request.v
        ));
    }
    if request.method != "requestUserInput" {
        return Err(format!(
            "unsupported workflow control method `{}`",
            request.method
        ));
    }
    if request.id != expected_request_id {
        return Err(format!(
            "workflow control request id {} was out of order; expected {expected_request_id}",
            request.id
        ));
    }
    Ok(request)
}

pub(super) fn validate_user_input_request(
    args: RequestUserInputArgs,
) -> Result<RequestUserInputArgs, String> {
    if args.questions.is_empty() || args.questions.len() > 3 {
        return Err("requestUserInput requires one to three questions".to_string());
    }
    if args.auto_resolution_ms.is_some() {
        return Err("requestUserInput does not support autoResolutionMs".to_string());
    }

    let mut question_ids = HashSet::new();
    for question in &args.questions {
        validate_question_id(&question.id)?;
        if !question_ids.insert(question.id.as_str()) {
            return Err(format!(
                "duplicate requestUserInput question id `{}`",
                question.id
            ));
        }
        validate_required_text(
            &question.header,
            "question header",
            MAX_WORKFLOW_QUESTION_HEADER_CHARS,
        )?;
        validate_required_text(
            &question.question,
            "question prompt",
            MAX_WORKFLOW_QUESTION_CHARS,
        )?;

        match question.options.as_ref() {
            Some(options) if options.is_empty() => {
                return Err(format!(
                    "requestUserInput question `{}` must omit options for free-form input",
                    question.id
                ));
            }
            Some(options) => {
                if options.len() > MAX_WORKFLOW_OPTIONS {
                    return Err(format!(
                        "requestUserInput question `{}` exceeds the limit of {MAX_WORKFLOW_OPTIONS} options",
                        question.id
                    ));
                }
                let mut labels = HashSet::new();
                for option in options {
                    validate_required_text(
                        &option.label,
                        "option label",
                        MAX_WORKFLOW_OPTION_LABEL_CHARS,
                    )?;
                    if option.description.chars().count() > MAX_WORKFLOW_OPTION_DESCRIPTION_CHARS {
                        return Err(format!(
                            "requestUserInput option description exceeds {MAX_WORKFLOW_OPTION_DESCRIPTION_CHARS} characters"
                        ));
                    }
                    if !labels.insert(option.label.trim()) {
                        return Err(format!(
                            "requestUserInput question `{}` has duplicate option label `{}`",
                            question.id, option.label
                        ));
                    }
                    if option.label.starts_with(USER_NOTE_PREFIX) {
                        return Err(format!(
                            "requestUserInput question `{}` uses reserved option-label prefix `{USER_NOTE_PREFIX}`",
                            question.id
                        ));
                    }
                }
                if question.is_other && labels.contains(OTHER_OPTION_LABEL) {
                    return Err(format!(
                        "requestUserInput question `{}` uses reserved option label `{OTHER_OPTION_LABEL}`",
                        question.id
                    ));
                }
            }
            None if question.is_other => {
                return Err(format!(
                    "requestUserInput question `{}` cannot enable Other without options",
                    question.id
                ));
            }
            None => {}
        }
    }
    Ok(args)
}

pub fn decode_user_input_request(params: Value) -> Result<RequestUserInputArgs, String> {
    let params = require_object(&params, "requestUserInput params")?;
    reject_unknown_fields(
        params,
        "requestUserInput params",
        &["questions", "autoResolutionMs"],
    )?;
    if params.contains_key("autoResolutionMs") {
        return Err("requestUserInput does not support autoResolutionMs".to_string());
    }

    if let Some(questions) = params.get("questions").and_then(Value::as_array) {
        for (index, question) in questions.iter().enumerate() {
            let label = format!("requestUserInput question {index}");
            let question = require_object(question, &label)?;
            reject_unknown_fields(
                question,
                &label,
                &["id", "header", "question", "isOther", "isSecret", "options"],
            )?;
            if let Some(options) = question.get("options").and_then(Value::as_array) {
                for (option_index, option) in options.iter().enumerate() {
                    let label = format!("{label} option {option_index}");
                    reject_unknown_fields(
                        require_object(option, &label)?,
                        &label,
                        &["label", "description"],
                    )?;
                }
            }
        }
    }

    let args = serde_json::from_value::<RequestUserInputArgs>(Value::Object(params.clone()))
        .map_err(|err| format!("invalid requestUserInput params: {err}"))?;
    validate_user_input_request(args)
}

pub fn validate_user_input_response(
    request: &RequestUserInputArgs,
    response: RequestUserInputResponse,
) -> Result<RequestUserInputResponse, String> {
    if let Some(id) = response
        .answers
        .keys()
        .filter(|id| {
            !request
                .questions
                .iter()
                .any(|question| question.id.as_str() == id.as_str())
        })
        .min()
    {
        return Err(format!(
            "requestUserInput response contains unknown question id `{id}`"
        ));
    }

    let mut answers = HashMap::with_capacity(request.questions.len());
    for question in &request.questions {
        let answer = response
            .answers
            .get(&question.id)
            .cloned()
            .unwrap_or_else(|| RequestUserInputAnswer {
                answers: Vec::new(),
            });
        let answer = normalize_question_answer(question, answer)?;
        answers.insert(question.id.clone(), answer);
    }
    Ok(RequestUserInputResponse { answers })
}

fn normalize_question_answer(
    question: &codex_protocol::request_user_input::RequestUserInputQuestion,
    mut answer: RequestUserInputAnswer,
) -> Result<RequestUserInputAnswer, String> {
    if question.options.is_some()
        && question.is_other
        && matches!(answer.answers.as_slice(), [note] if note.starts_with(USER_NOTE_PREFIX))
        && !question.options.as_ref().is_some_and(|options| {
            options
                .iter()
                .any(|option| option.label == answer.answers[0])
        })
    {
        answer
            .answers
            .insert(/*index*/ 0, OTHER_OPTION_LABEL.to_string());
    }
    let values = answer.answers.as_slice();
    let valid = match question.options.as_ref() {
        Some(options) => match values {
            [] => true,
            [selection] => valid_selection(question.is_other, options, selection),
            [selection, note] => {
                valid_selection(question.is_other, options, selection)
                    && note.starts_with(USER_NOTE_PREFIX)
            }
            _ => false,
        },
        None => match values {
            [] => true,
            [note] => note.starts_with(USER_NOTE_PREFIX),
            _ => false,
        },
    };
    if valid {
        Ok(answer)
    } else {
        Err(format!(
            "requestUserInput response for `{}` does not match its question",
            question.id
        ))
    }
}

fn valid_selection(
    is_other: bool,
    options: &[codex_protocol::request_user_input::RequestUserInputQuestionOption],
    selection: &str,
) -> bool {
    options.iter().any(|option| option.label == selection)
        || (is_other && selection == OTHER_OPTION_LABEL)
}

fn require_object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}

fn reject_unknown_fields(
    object: &Map<String, Value>,
    label: &str,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("{label} contains unsupported field `{field}`"));
    }
    Ok(())
}

fn validate_question_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_lowercase())
        || !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        || id.chars().count() > MAX_WORKFLOW_QUESTION_ID_CHARS
    {
        return Err(format!(
            "requestUserInput question id `{id}` must be snake_case and at most {MAX_WORKFLOW_QUESTION_ID_CHARS} characters"
        ));
    }
    Ok(())
}

fn validate_required_text(value: &str, label: &str, max_chars: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("requestUserInput {label} must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "requestUserInput {label} exceeds {max_chars} characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod tests;
