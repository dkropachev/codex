use std::path::Path;
use std::path::PathBuf;

use codex_workflows::CompletionMode;
use codex_workflows::CompletionRequest;
use codex_workflows::CompletionResult;

use super::ActivePopup;
use super::AppEvent;
use super::ChatComposer;
use super::WorkflowCommand;
use super::parse_slash_name;
use crate::bottom_pane::slash_commands::find_unique_workflow_command;

impl ChatComposer {
    pub(crate) fn on_workflow_completion_result(
        &mut self,
        generation: u64,
        workflow_dir: PathBuf,
        request: CompletionRequest,
        result: CompletionResult,
    ) {
        if generation != self.workflow_completion_generation
            || self.workflow_completion_request.as_ref()
                != Some(&(workflow_dir.clone(), request.clone()))
        {
            return;
        }
        if let Some(error) = result.error.as_deref() {
            tracing::debug!(workflow_dir = %workflow_dir.display(), %error, "workflow completion failed");
        }
        let hints = workflow_completion_hints(&request, &result);
        if let Some(command) = self
            .workflow_commands
            .iter_mut()
            .find(|command| command.workflow_dir == workflow_dir)
        {
            command.option_hints = hints;
        }
        if matches!(self.popups.active, ActivePopup::Command(_)) {
            self.popups.active = ActivePopup::None;
        }
        self.sync_popups();
    }

    pub(super) fn sync_workflow_completion(&mut self, first_line: &str, cursor: usize) {
        let request = workflow_completion_request(
            first_line,
            cursor,
            &self.workflow_commands,
            &self.workflow_completion_cwd,
        );
        let Some((workflow_dir, request)) = request else {
            self.cancel_workflow_completion();
            return;
        };
        if self.workflow_completion_request.as_ref()
            == Some(&(workflow_dir.clone(), request.clone()))
        {
            return;
        }

        self.workflow_completion_generation = self.workflow_completion_generation.wrapping_add(1);
        self.workflow_completion_request = Some((workflow_dir.clone(), request.clone()));
        if let Some(command) = self
            .workflow_commands
            .iter_mut()
            .find(|command| command.workflow_dir == workflow_dir)
        {
            command.option_hints.clear();
        }
        self.app_event_tx.send(AppEvent::StartWorkflowCompletion {
            generation: self.workflow_completion_generation,
            workflow_dir,
            request,
        });
    }

    pub(super) fn cancel_workflow_completion(&mut self) {
        if self.workflow_completion_request.take().is_some() {
            self.workflow_completion_generation =
                self.workflow_completion_generation.wrapping_add(1);
            self.app_event_tx.send(AppEvent::CancelWorkflowCompletion);
        }
    }
}

fn workflow_completion_request(
    first_line: &str,
    cursor: usize,
    commands: &[WorkflowCommand],
    cwd: &Path,
) -> Option<(PathBuf, CompletionRequest)> {
    if cursor != first_line.len() {
        return None;
    }
    let (name, rest, _) = parse_slash_name(first_line)?;
    let command = find_unique_workflow_command(commands, name)?;
    let tokens = shlex::split(rest)?;
    let ends_with_whitespace = first_line
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace);
    let mut completed = tokens.clone();
    let mut active_field = None;
    let mut prefix = String::new();
    let mode = match tokens.last() {
        Some(last) if !ends_with_whitespace => {
            if let Some(flag) = last.strip_prefix("--")
                && let Some((name, value_prefix)) = flag.split_once('=')
            {
                completed.pop();
                active_field = kebab_flag_to_field(name);
                prefix = value_prefix.to_string();
                CompletionMode::Value
            } else if let Some(flag_prefix) = last.strip_prefix("--") {
                completed.pop();
                prefix = format!("--{flag_prefix}");
                CompletionMode::Field
            } else if let Some(flag) = tokens
                .get(tokens.len().saturating_sub(2))
                .and_then(|value| value.strip_prefix("--"))
            {
                completed.truncate(tokens.len().saturating_sub(2));
                active_field = kebab_flag_to_field(flag);
                prefix = last.clone();
                CompletionMode::Value
            } else {
                return None;
            }
        }
        Some(last) if last.starts_with("--") => {
            completed.pop();
            active_field = kebab_flag_to_field(last.trim_start_matches("--"));
            CompletionMode::Value
        }
        Some(_) | None => CompletionMode::Field,
    };
    if mode == CompletionMode::Value && active_field.is_none() {
        return None;
    }
    let input = codex_workflows::workflow_invocation_input_from_args(cwd, &completed)
        .unwrap_or_else(|_| {
            serde_json::json!({
                "workingDirectory": cwd.to_string_lossy(),
            })
        });
    Some((
        command.workflow_dir.clone(),
        CompletionRequest {
            input,
            active_field,
            prefix,
            mode,
        },
    ))
}

fn kebab_flag_to_field(flag: &str) -> Option<String> {
    let mut parts = flag.split('-');
    let first = parts.next()?;
    if first.is_empty()
        || !first
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    {
        return None;
    }
    let mut field = first.to_string();
    for part in parts {
        if part.is_empty()
            || !part
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        {
            return None;
        }
        let mut chars = part.chars();
        field.push(chars.next()?.to_ascii_uppercase());
        field.push_str(chars.as_str());
    }
    Some(field)
}

fn workflow_completion_hints(
    request: &CompletionRequest,
    result: &CompletionResult,
) -> Vec<crate::workflow_commands::WorkflowCommandOptionHint> {
    let option_name = request
        .active_field
        .as_deref()
        .map(|field| format!("--{}", workflow_field_to_kebab(field)));
    result
        .items
        .iter()
        .map(|item| {
            let raw_insertion = result.insertion_for(&item.value);
            let insertion = match request.mode {
                CompletionMode::Value => shlex::try_quote(raw_insertion)
                    .map_or_else(|_| raw_insertion.to_string(), std::borrow::Cow::into_owned),
                CompletionMode::Field => raw_insertion.to_string(),
            };
            let display = match (&request.mode, option_name.as_deref()) {
                (CompletionMode::Value, Some(option_name)) => {
                    format!("{option_name} {insertion}")
                }
                (CompletionMode::Field, _) | (CompletionMode::Value, None) => item.value.clone(),
            };
            crate::workflow_commands::WorkflowCommandOptionHint {
                display,
                description: item.description.clone(),
            }
        })
        .collect()
}

fn workflow_field_to_kebab(field: &str) -> String {
    let mut value = String::new();
    for ch in field.chars() {
        if ch.is_ascii_uppercase() {
            if !value.is_empty() {
                value.push('-');
            }
            value.push(ch.to_ascii_lowercase());
        } else {
            value.push(ch);
        }
    }
    value
}

#[cfg(test)]
#[path = "workflow_completion_tests.rs"]
mod tests;
