use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::ColumnWidthConfig;
use super::selection_popup_common::ColumnWidthMode;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height_with_col_width_mode;
use super::selection_popup_common::render_rows_with_col_width_mode;
use super::slash_commands;
use super::workflow_command_options::WorkflowCommandInfo;
use super::workflow_command_options::load_workflow_command_info;
use crate::render::Insets;
use crate::render::RectExt;
use crate::slash_command::SlashCommand;
use codex_workflows::WorkflowCommandCompletionSuggestion;
use codex_workflows::WorkflowSummary;

// Hide alias commands in the default popup list so each unique action appears once.
// `quit` is an alias of `exit`, so we skip `quit` here.
const ALIAS_COMMANDS: &[SlashCommand] = &[SlashCommand::Quit];
const COMMAND_COLUMN_WIDTH: ColumnWidthConfig = ColumnWidthConfig::new(
    ColumnWidthMode::AutoAllRows,
    /*name_column_width*/ None,
);

/// A selectable item in the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandItem {
    Builtin(SlashCommand),
    Workflow(Box<WorkflowCommandInfo>),
    WorkflowOption(codex_workflows::WorkflowCommandOptionHint),
    WorkflowSuggestion {
        command: String,
        suggestion: WorkflowCommandCompletionSuggestion,
    },
}

pub(crate) struct CommandPopup {
    command_filter: String,
    builtins: Vec<(&'static str, SlashCommand)>,
    workflows: Vec<WorkflowCommandInfo>,
    state: ScrollState,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CommandPopupFlags {
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) connectors_enabled: bool,
    pub(crate) plugins_command_enabled: bool,
    pub(crate) fast_command_enabled: bool,
    pub(crate) goal_command_enabled: bool,
    pub(crate) personality_command_enabled: bool,
    pub(crate) realtime_conversation_enabled: bool,
    pub(crate) audio_device_selection_enabled: bool,
    pub(crate) workflows_enabled: bool,
    pub(crate) windows_degraded_sandbox_active: bool,
    pub(crate) side_conversation_active: bool,
}

impl From<CommandPopupFlags> for slash_commands::BuiltinCommandFlags {
    fn from(value: CommandPopupFlags) -> Self {
        Self {
            collaboration_modes_enabled: value.collaboration_modes_enabled,
            connectors_enabled: value.connectors_enabled,
            plugins_command_enabled: value.plugins_command_enabled,
            fast_command_enabled: value.fast_command_enabled,
            goal_command_enabled: value.goal_command_enabled,
            personality_command_enabled: value.personality_command_enabled,
            realtime_conversation_enabled: value.realtime_conversation_enabled,
            audio_device_selection_enabled: value.audio_device_selection_enabled,
            workflows_enabled: value.workflows_enabled,
            allow_elevate_sandbox: value.windows_degraded_sandbox_active,
            side_conversation_active: value.side_conversation_active,
        }
    }
}

impl CommandPopup {
    pub(crate) fn new(flags: CommandPopupFlags) -> Self {
        // Keep built-in availability in sync with the composer.
        let builtins: Vec<(&'static str, SlashCommand)> =
            slash_commands::builtins_for_input(flags.into())
                .into_iter()
                .filter(|(name, _)| !name.starts_with("debug"))
                .filter(|(_, cmd)| *cmd != SlashCommand::Apps)
                .collect();
        Self {
            command_filter: String::new(),
            builtins,
            workflows: Vec::new(),
            state: ScrollState::new(),
        }
    }

    pub(crate) fn set_workflows(&mut self, workflows: Option<&[WorkflowSummary]>) {
        self.workflows = workflows.map_or_else(Vec::new, |workflows| {
            workflows.iter().map(load_workflow_command_info).collect()
        });
        self.sync_selection();
    }

    pub(crate) fn set_workflow_suggestions(
        &mut self,
        command: &str,
        suggestions: Vec<WorkflowCommandCompletionSuggestion>,
    ) {
        if let Some(workflow) = self.workflows.iter_mut().find(|workflow| {
            workflow
                .workflow
                .command
                .as_deref()
                .is_some_and(|workflow_command| workflow_command == command)
        }) {
            workflow.dynamic_suggestions = suggestions;
        }
        self.sync_selection();
    }

    /// Update the filter string based on the current composer text. The text
    /// passed in is expected to start with a leading '/'. Everything after the
    /// *first* '/' on the *first* line becomes the active filter that is used
    /// to narrow down the list of available commands.
    pub(crate) fn on_composer_text_change(&mut self, text: String) {
        let first_line = text.lines().next().unwrap_or("");

        if let Some(stripped) = first_line.strip_prefix('/') {
            // Extract the *first* token (sequence of non-whitespace
            // characters) after the slash so that `/clear something` still
            // shows the help for `/clear`.
            let token = stripped.trim_start();
            let cmd_token = token.split_whitespace().next().unwrap_or("");

            // Update the filter keeping the original case (commands are all
            // lower-case for now but this may change in the future).
            self.command_filter = cmd_token.to_string();
        } else {
            // The composer no longer starts with '/'. Reset the filter so the
            // popup shows the *full* command list if it is still displayed
            // for some reason.
            self.command_filter.clear();
        }

        self.sync_selection();
    }

    fn sync_selection(&mut self) {
        // Reset or clamp selected index based on new filtered list.
        let matches = self.filtered_items();
        let matches_len = matches.len();
        self.state.clamp_selection(matches_len);
        if self
            .state
            .selected_idx
            .and_then(|idx| matches.get(idx))
            .is_some_and(Self::item_is_disabled)
        {
            self.state.selected_idx = Self::first_selectable_index(&matches);
        }
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Determine the preferred height of the popup for a given width.
    /// Accounts for wrapped descriptions so that long tooltips don't overflow.
    pub(crate) fn calculate_required_height(&self, width: u16) -> u16 {
        let rows = self.rows_from_matches(self.filtered());

        measure_rows_height_with_col_width_mode(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            width,
            COMMAND_COLUMN_WIDTH,
        )
    }

    /// Compute exact/prefix matches over built-in commands and registered
    /// workflows, paired with optional highlight indices. Preserves the
    /// original presentation order for built-ins and workflows.
    fn filtered(&self) -> Vec<(CommandItem, Option<Vec<usize>>)> {
        let filter = self.command_filter.trim();
        let mut out: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        if filter.is_empty() {
            for (_, cmd) in self.builtins.iter() {
                if ALIAS_COMMANDS.contains(cmd) {
                    continue;
                }
                out.push((CommandItem::Builtin(*cmd), None));
            }
            for workflow in self.workflows.iter() {
                if workflow.workflow.command.is_some() {
                    out.push((CommandItem::Workflow(Box::new(workflow.clone())), None));
                }
            }
            return out;
        }

        let filter_lower = filter.to_lowercase();
        let filter_chars = filter.chars().count();
        let mut exact: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        let mut prefix: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        let indices_for = |offset| Some((offset..offset + filter_chars).collect());

        let mut push_match =
            |item: CommandItem, display: &str, name: Option<&str>, name_offset: usize| {
                let display_lower = display.to_lowercase();
                let name_lower = name.map(str::to_lowercase);
                let display_exact = display_lower == filter_lower;
                let name_exact = name_lower.as_deref() == Some(filter_lower.as_str());
                if display_exact || name_exact {
                    let offset = if display_exact { 0 } else { name_offset };
                    exact.push((item, indices_for(offset)));
                    return;
                }
                let display_prefix = display_lower.starts_with(&filter_lower);
                let name_prefix = name_lower
                    .as_ref()
                    .is_some_and(|name| name.starts_with(&filter_lower));
                if display_prefix || name_prefix {
                    let offset = if display_prefix { 0 } else { name_offset };
                    prefix.push((item, indices_for(offset)));
                }
            };

        for (_, cmd) in self.builtins.iter() {
            push_match(CommandItem::Builtin(*cmd), cmd.command(), None, 0);
        }

        for workflow in self.workflows.iter() {
            let Some(match_kind) = slash_commands::workflow_match_kind(&workflow.workflow, filter)
            else {
                continue;
            };

            let indices = workflow.workflow.command.as_deref().and_then(|command| {
                let command_lower = command.to_lowercase();
                if command_lower == filter_lower || command_lower.starts_with(&filter_lower) {
                    Some((0..filter_chars).collect())
                } else {
                    None
                }
            });
            let item = CommandItem::Workflow(Box::new(workflow.clone()));
            match match_kind {
                slash_commands::WorkflowMatchKind::Exact => {
                    let show_option_hints = workflow
                        .workflow
                        .command
                        .as_deref()
                        .is_some_and(|command| command.eq_ignore_ascii_case(filter));
                    exact.push((item, indices));
                    if show_option_hints {
                        for option in &workflow.option_hints {
                            exact.push((CommandItem::WorkflowOption(option.clone()), None));
                        }
                        for suggestion in &workflow.dynamic_suggestions {
                            exact.push((
                                CommandItem::WorkflowSuggestion {
                                    command: filter.to_string(),
                                    suggestion: suggestion.clone(),
                                },
                                None,
                            ));
                        }
                    }
                }
                slash_commands::WorkflowMatchKind::Prefix => prefix.push((item, indices)),
            }
        }

        out.extend(exact);
        out.extend(prefix);
        out
    }

    fn filtered_items(&self) -> Vec<CommandItem> {
        self.filtered().into_iter().map(|(c, _)| c).collect()
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(CommandItem, Option<Vec<usize>>)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(item, indices)| {
                let (name, description, name_prefix_spans, match_indices, is_disabled) = match &item
                {
                    CommandItem::Builtin(cmd) => (
                        format!("/{}", cmd.command()),
                        Some(cmd.description().to_string()),
                        Vec::new(),
                        indices.map(|v| v.into_iter().map(|i| i + 1).collect()),
                        false,
                    ),
                    CommandItem::Workflow(workflow) => {
                        let command = workflow
                            .workflow
                            .command
                            .as_deref()
                            .unwrap_or(workflow.workflow.id.as_str());
                        let description = workflow
                            .workflow
                            .title
                            .as_deref()
                            .or(workflow.workflow.user_description.as_deref())
                            .unwrap_or(workflow.workflow.id.as_str())
                            .to_string();
                        (
                            format!("/{command}"),
                            Some(description),
                            Vec::new(),
                            indices.map(|v| v.into_iter().map(|i| i + 1).collect()),
                            false,
                        )
                    }
                    CommandItem::WorkflowOption(option) => (
                        option.display.clone(),
                        option.description.clone(),
                        vec!["  ".into()],
                        None,
                        true,
                    ),
                    CommandItem::WorkflowSuggestion { suggestion, .. } => (
                        suggestion.display.clone(),
                        suggestion.description.clone(),
                        vec!["  ".into()],
                        None,
                        false,
                    ),
                };
                GenericDisplayRow {
                    name,
                    name_prefix_spans,
                    match_indices,
                    display_shortcut: None,
                    description,
                    category_tag: None,
                    wrap_indent: None,
                    is_disabled,
                    disabled_reason: None,
                }
            })
            .collect()
    }

    /// Move the selection cursor one step up.
    pub(crate) fn move_up(&mut self) {
        let items = self.filtered_items();
        let len = items.len();
        self.state.selected_idx =
            Self::next_selectable_index(&items, self.state.selected_idx, NavigationDirection::Up);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    /// Move the selection cursor one step down.
    pub(crate) fn move_down(&mut self) {
        let items = self.filtered_items();
        let matches_len = items.len();
        self.state.selected_idx =
            Self::next_selectable_index(&items, self.state.selected_idx, NavigationDirection::Down);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Return currently selected command, if any.
    pub(crate) fn selected_item(&self) -> Option<CommandItem> {
        let matches = self.filtered_items();
        self.state
            .selected_idx
            .and_then(|idx| matches.get(idx).cloned())
            .filter(|item| !Self::item_is_disabled(item))
    }

    fn item_is_disabled(item: &CommandItem) -> bool {
        matches!(item, CommandItem::WorkflowOption(_))
    }

    fn first_selectable_index(items: &[CommandItem]) -> Option<usize> {
        items.iter().position(|item| !Self::item_is_disabled(item))
    }

    fn next_selectable_index(
        items: &[CommandItem],
        current: Option<usize>,
        direction: NavigationDirection,
    ) -> Option<usize> {
        let first_selectable = Self::first_selectable_index(items)?;
        let len = items.len();
        let start = current.unwrap_or(first_selectable);
        let mut idx = start;

        loop {
            idx = match direction {
                NavigationDirection::Up => idx.checked_sub(1).unwrap_or(len - 1),
                NavigationDirection::Down => (idx + 1) % len,
            };
            if !Self::item_is_disabled(&items[idx]) {
                return Some(idx);
            }
            if idx == start {
                return Some(first_selectable);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum NavigationDirection {
    Up,
    Down,
}

impl WidgetRef for CommandPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.rows_from_matches(self.filtered());
        render_rows_with_col_width_mode(
            area.inset(Insets::tlbr(
                /*top*/ 0, /*left*/ 2, /*bottom*/ 0, /*right*/ 0,
            )),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no matches",
            COMMAND_COLUMN_WIDTH,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn render_popup(popup: &CommandPopup, width: u16) -> String {
        let area = Rect::new(0, 0, width, popup.calculate_required_height(width));
        let mut buf = Buffer::empty(area);
        popup.render_ref(area, &mut buf);
        format!("{buf:?}")
    }

    fn command_name(item: &CommandItem) -> String {
        match item {
            CommandItem::Builtin(cmd) => cmd.command().to_string(),
            CommandItem::Workflow(workflow) => workflow
                .workflow
                .command
                .clone()
                .unwrap_or_else(|| workflow.workflow.id.clone()),
            CommandItem::WorkflowOption(option) => option.display.clone(),
            CommandItem::WorkflowSuggestion { suggestion, .. } => suggestion.display.clone(),
        }
    }

    #[test]
    fn filter_includes_init_when_typing_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        // Simulate the composer line starting with '/in' so the popup filters
        // matching commands by prefix.
        popup.on_composer_text_change("/in".to_string());

        // Access the filtered list via the selected command and ensure that
        // one of the matches is the new "init" command.
        let matches = popup.filtered_items();
        let has_init = matches.iter().any(|item| command_name(item) == "init");
        assert!(
            has_init,
            "expected '/init' to appear among filtered commands"
        );
    }

    #[test]
    fn selecting_init_by_exact_match() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        popup.on_composer_text_change("/init".to_string());

        // When an exact match exists, the selected command should be that
        // command by default.
        let selected = popup.selected_item();
        match selected {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "init"),
            Some(CommandItem::Workflow(workflow)) => {
                panic!("expected builtin /init to be selected, got workflow {workflow:?}")
            }
            Some(CommandItem::WorkflowOption(option)) => {
                panic!("expected builtin /init to be selected, got option hint {option:?}")
            }
            Some(CommandItem::WorkflowSuggestion { suggestion, .. }) => {
                panic!(
                    "expected builtin /init to be selected, got dynamic suggestion {suggestion:?}"
                )
            }
            None => panic!("expected a selected command for exact match"),
        }
    }

    #[test]
    fn selecting_workflow_alias_by_exact_match() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        let root = PathBuf::from("/tmp/workflows");
        let path = root.join("reports").join("jira-summary");
        let workflow = WorkflowSummary {
            id: "reports/jira-summary".to_string(),
            command: Some("jira-summary".to_string()),
            title: Some("Jira Summary".to_string()),
            user_description: Some("Prepare a focused workflow report".to_string()),
            search_terms: vec!["report".to_string()],
            command_option_hints: Vec::new(),
            root_label: "global".to_string(),
            root_kind: codex_workflows::WorkflowRootKind::Global,
            root_path: root.clone(),
            path: path.clone(),
            workflow_yaml_path: path.join("workflow.yaml"),
            mention_target: codex_workflows::mention_target(&root, "reports/jira-summary").unwrap(),
            validation: codex_workflows::WorkflowValidation {
                status: codex_workflows::WorkflowValidationStatus::Valid,
                messages: Vec::new(),
            },
            repair_mode: "threshold:3".to_string(),
        };
        popup.set_workflows(Some(std::slice::from_ref(&workflow)));
        popup.on_composer_text_change("/jira-summary".to_string());

        match popup.selected_item() {
            Some(CommandItem::Workflow(selected)) => assert_eq!(&selected.workflow, &workflow),
            other => {
                panic!("expected workflow alias to be selected for exact match, got {other:?}")
            }
        }
    }

    #[test]
    fn selecting_workflow_by_title_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags {
            workflows_enabled: true,
            ..CommandPopupFlags::default()
        });
        let root = PathBuf::from("/tmp/workflows");
        let path = root.join("reports").join("jira-summary");
        let workflow = WorkflowSummary {
            id: "reports/jira-summary".to_string(),
            command: Some("summary".to_string()),
            title: Some("Jira Summary".to_string()),
            user_description: Some("Prepare a focused workflow report".to_string()),
            search_terms: vec!["report".to_string()],
            command_option_hints: Vec::new(),
            root_label: "global".to_string(),
            root_kind: codex_workflows::WorkflowRootKind::Global,
            root_path: root.clone(),
            path: path.clone(),
            workflow_yaml_path: path.join("workflow.yaml"),
            mention_target: codex_workflows::mention_target(&root, "reports/jira-summary").unwrap(),
            validation: codex_workflows::WorkflowValidation {
                status: codex_workflows::WorkflowValidationStatus::Valid,
                messages: Vec::new(),
            },
            repair_mode: "threshold:3".to_string(),
        };
        popup.set_workflows(Some(std::slice::from_ref(&workflow)));
        popup.on_composer_text_change("/jira".to_string());

        match popup.selected_item() {
            Some(CommandItem::Workflow(selected)) => assert_eq!(&selected.workflow, &workflow),
            other => {
                panic!("expected workflow to be selected for title prefix match, got {other:?}")
            }
        }

        insta::assert_snapshot!("workflow_title_prefix", render_popup(&popup, /*width*/ 72));
    }

    #[test]
    fn exact_workflow_command_shows_dimmed_option_hints() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("workflows");
        let path = root.join("code-review");
        std::fs::create_dir_all(&path).expect("workflow dir");
        let workflow_yaml_path = path.join("workflow.yaml");
        std::fs::write(
            &workflow_yaml_path,
            r#"id: code-review
command: code-review
title: Code Review
userDescription: Review an existing submission.
api:
  inputSchema:
    type: object
    required:
      - reviewId
    properties:
      reviewId:
        type: string
        description: Review identifier
      format:
        type: string
        enum:
          - summary
          - full
        description: Output format
      includeComments:
        type: boolean
        description: Include comment bodies
"#,
        )
        .expect("workflow spec");

        let workflow = WorkflowSummary {
            id: "code-review".to_string(),
            command: Some("code-review".to_string()),
            title: Some("Code Review".to_string()),
            user_description: Some("Review an existing submission.".to_string()),
            search_terms: vec!["review".to_string()],
            command_option_hints: vec![
                codex_workflows::WorkflowCommandOptionHint {
                    display: "--review-id <string>".to_string(),
                    description: Some("required · Review identifier".to_string()),
                },
                codex_workflows::WorkflowCommandOptionHint {
                    display: "--format <summary|full>".to_string(),
                    description: Some("Output format".to_string()),
                },
                codex_workflows::WorkflowCommandOptionHint {
                    display: "--include-comments".to_string(),
                    description: Some("Include comment bodies".to_string()),
                },
            ],
            root_label: "global".to_string(),
            root_kind: codex_workflows::WorkflowRootKind::Global,
            root_path: root.clone(),
            path,
            workflow_yaml_path,
            mention_target: codex_workflows::mention_target(&root, "code-review").unwrap(),
            validation: codex_workflows::WorkflowValidation {
                status: codex_workflows::WorkflowValidationStatus::Valid,
                messages: Vec::new(),
            },
            repair_mode: "threshold:3".to_string(),
        };

        let mut popup = CommandPopup::new(CommandPopupFlags {
            workflows_enabled: true,
            ..CommandPopupFlags::default()
        });
        popup.set_workflows(Some(std::slice::from_ref(&workflow)));
        popup.on_composer_text_change("/code-review".to_string());

        assert!(popup.filtered_items().iter().any(|item| {
            matches!(
                item,
                CommandItem::WorkflowOption(option)
                    if option.display == "--review-id <string>"
            )
        }));
        assert_eq!(
            popup.selected_item(),
            Some(CommandItem::Workflow(Box::new(
                super::load_workflow_command_info(&workflow)
            )))
        );
        insta::assert_snapshot!(
            "workflow_exact_command_options",
            render_popup(&popup, /*width*/ 88)
        );
    }

    #[test]
    fn model_is_first_suggestion_for_mo() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        popup.on_composer_text_change("/mo".to_string());
        let matches = popup.filtered_items();
        match matches.first() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "model"),
            Some(CommandItem::Workflow(workflow)) => {
                panic!("expected builtin /model to be first, got workflow {workflow:?}")
            }
            Some(CommandItem::WorkflowOption(option)) => {
                panic!("expected builtin /model to be first, got option hint {option:?}")
            }
            Some(CommandItem::WorkflowSuggestion { suggestion, .. }) => {
                panic!("expected builtin /model to be first, got dynamic suggestion {suggestion:?}")
            }
            None => panic!("expected at least one match for '/mo'"),
        }
    }

    #[test]
    fn filtered_commands_keep_presentation_order_for_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        popup.on_composer_text_change("/m".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| command_name(&item))
            .collect();
        assert_eq!(
            cmds,
            vec![
                "model".to_string(),
                "memories".to_string(),
                "mention".to_string(),
                "mcp".to_string(),
            ]
        );
    }

    #[test]
    fn prefix_filter_limits_matches_for_ac() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        popup.on_composer_text_change("/ac".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| command_name(&item))
            .collect();
        assert!(
            !cmds.contains(&"compact".to_string()),
            "expected prefix search for '/ac' to exclude 'compact', got {cmds:?}"
        );
    }

    #[test]
    fn quit_hidden_in_empty_filter_but_shown_for_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        popup.on_composer_text_change("/".to_string());
        let items = popup.filtered_items();
        assert!(!items.contains(&CommandItem::Builtin(SlashCommand::Quit)));

        popup.on_composer_text_change("/qu".to_string());
        let items = popup.filtered_items();
        assert!(items.contains(&CommandItem::Builtin(SlashCommand::Quit)));
    }

    #[test]
    fn collab_command_hidden_when_collaboration_modes_disabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default());
        popup.on_composer_text_change("/".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| command_name(&item))
            .collect();
        assert!(
            !cmds.contains(&"collab".to_string()),
            "expected '/collab' to be hidden when collaboration modes are disabled, got {cmds:?}"
        );
        assert!(
            !cmds.contains(&"plan".to_string()),
            "expected '/plan' to be hidden when collaboration modes are disabled, got {cmds:?}"
        );
    }

    #[test]
    fn collab_command_visible_when_collaboration_modes_enabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags {
            collaboration_modes_enabled: true,
            connectors_enabled: false,
            plugins_command_enabled: false,
            fast_command_enabled: false,
            goal_command_enabled: false,
            personality_command_enabled: true,
            realtime_conversation_enabled: false,
            audio_device_selection_enabled: false,
            workflows_enabled: false,
            windows_degraded_sandbox_active: false,
            side_conversation_active: false,
        });
        popup.on_composer_text_change("/collab".to_string());

        match popup.selected_item() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "collab"),
            other => panic!("expected collab to be selected for exact match, got {other:?}"),
        }
    }

    #[test]
    fn plan_command_visible_when_collaboration_modes_enabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags {
            collaboration_modes_enabled: true,
            connectors_enabled: false,
            plugins_command_enabled: false,
            fast_command_enabled: false,
            goal_command_enabled: false,
            personality_command_enabled: true,
            realtime_conversation_enabled: false,
            audio_device_selection_enabled: false,
            workflows_enabled: false,
            windows_degraded_sandbox_active: false,
            side_conversation_active: false,
        });
        popup.on_composer_text_change("/plan".to_string());

        match popup.selected_item() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "plan"),
            other => panic!("expected plan to be selected for exact match, got {other:?}"),
        }
    }

    #[test]
    fn personality_command_hidden_when_disabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags {
            collaboration_modes_enabled: true,
            connectors_enabled: false,
            plugins_command_enabled: false,
            fast_command_enabled: false,
            goal_command_enabled: false,
            personality_command_enabled: false,
            realtime_conversation_enabled: false,
            audio_device_selection_enabled: false,
            workflows_enabled: false,
            windows_degraded_sandbox_active: false,
            side_conversation_active: false,
        });
        popup.on_composer_text_change("/pers".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| command_name(&item))
            .collect();
        assert!(
            !cmds.contains(&"personality".to_string()),
            "expected '/personality' to be hidden when disabled, got {cmds:?}"
        );
    }

    #[test]
    fn personality_command_visible_when_enabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags {
            collaboration_modes_enabled: true,
            connectors_enabled: false,
            plugins_command_enabled: false,
            fast_command_enabled: false,
            goal_command_enabled: false,
            personality_command_enabled: true,
            realtime_conversation_enabled: false,
            audio_device_selection_enabled: false,
            workflows_enabled: false,
            windows_degraded_sandbox_active: false,
            side_conversation_active: false,
        });
        popup.on_composer_text_change("/personality".to_string());

        match popup.selected_item() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "personality"),
            other => panic!("expected personality to be selected for exact match, got {other:?}"),
        }
    }

    #[test]
    fn settings_command_hidden_when_audio_device_selection_is_disabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags {
            collaboration_modes_enabled: false,
            connectors_enabled: false,
            plugins_command_enabled: false,
            fast_command_enabled: false,
            goal_command_enabled: false,
            personality_command_enabled: true,
            realtime_conversation_enabled: true,
            audio_device_selection_enabled: false,
            workflows_enabled: false,
            windows_degraded_sandbox_active: false,
            side_conversation_active: false,
        });
        popup.on_composer_text_change("/aud".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| command_name(&item))
            .collect();

        assert!(
            !cmds.contains(&"settings".to_string()),
            "expected '/settings' to be hidden when audio device selection is disabled, got {cmds:?}"
        );
    }

    #[test]
    fn debug_commands_are_hidden_from_popup() {
        let popup = CommandPopup::new(CommandPopupFlags::default());
        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| command_name(&item))
            .collect();

        assert!(
            !cmds.iter().any(|name| name.starts_with("debug")),
            "expected no /debug* command in popup menu, got {cmds:?}"
        );
    }
}
