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
use super::slash_commands::BuiltinCommandFlags;
use super::slash_commands::ServiceTierCommand;
use super::slash_commands::SlashCommandItem;
use super::slash_commands::commands_for_input;
use crate::render::Insets;
use crate::render::RectExt;
use crate::slash_command::SlashCommand;
use crate::workflow_commands::WorkflowCommand;
use crate::workflow_commands::WorkflowCommandOptionHint;

// Hide alias commands in the default popup list so each unique action appears once.
// `quit` is an alias of `exit`, and `btw` is an alias of `side`, so we skip
// those aliases here.
const ALIAS_COMMANDS: &[SlashCommand] = &[SlashCommand::Quit, SlashCommand::Btw];
const COMMAND_COLUMN_WIDTH: ColumnWidthConfig = ColumnWidthConfig::new(
    ColumnWidthMode::AutoAllRows,
    /*name_column_width*/ None,
);

/// A selectable item in the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandItem {
    Builtin(SlashCommand),
    ServiceTier(ServiceTierCommand),
    Workflow(WorkflowCommand),
    WorkflowOption(WorkflowCommandOptionHint),
}

pub(crate) struct CommandPopup {
    command_filter: String,
    workflow_argument_filter: String,
    commands: Vec<CommandItem>,
    state: ScrollState,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CommandPopupFlags {
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) connectors_enabled: bool,
    pub(crate) plugins_command_enabled: bool,
    pub(crate) service_tier_commands_enabled: bool,
    pub(crate) workflow_commands_enabled: bool,
    pub(crate) goal_command_enabled: bool,
    pub(crate) personality_command_enabled: bool,
    pub(crate) realtime_conversation_enabled: bool,
    pub(crate) audio_device_selection_enabled: bool,
    pub(crate) windows_degraded_sandbox_active: bool,
    pub(crate) side_conversation_active: bool,
}

impl From<CommandPopupFlags> for BuiltinCommandFlags {
    fn from(value: CommandPopupFlags) -> Self {
        Self {
            collaboration_modes_enabled: value.collaboration_modes_enabled,
            connectors_enabled: value.connectors_enabled,
            plugins_command_enabled: value.plugins_command_enabled,
            service_tier_commands_enabled: value.service_tier_commands_enabled,
            workflow_commands_enabled: value.workflow_commands_enabled,
            goal_command_enabled: value.goal_command_enabled,
            personality_command_enabled: value.personality_command_enabled,
            realtime_conversation_enabled: value.realtime_conversation_enabled,
            audio_device_selection_enabled: value.audio_device_selection_enabled,
            allow_elevate_sandbox: value.windows_degraded_sandbox_active,
            side_conversation_active: value.side_conversation_active,
        }
    }
}

impl CommandPopup {
    #[cfg(test)]
    pub(crate) fn new(
        flags: CommandPopupFlags,
        service_tier_commands: Vec<ServiceTierCommand>,
    ) -> Self {
        Self::new_with_workflows(flags, service_tier_commands, Vec::new())
    }

    pub(crate) fn new_with_workflows(
        flags: CommandPopupFlags,
        service_tier_commands: Vec<ServiceTierCommand>,
        workflow_commands: Vec<WorkflowCommand>,
    ) -> Self {
        // Keep built-in availability in sync with the composer.
        let commands = commands_for_input(flags.into(), &service_tier_commands, &workflow_commands)
            .into_iter()
            .filter_map(|command| match command {
                SlashCommandItem::Builtin(cmd) => (!cmd.command().starts_with("debug")
                    && cmd != SlashCommand::Apps)
                    .then_some(CommandItem::Builtin(cmd)),
                SlashCommandItem::ServiceTier(command) => Some(CommandItem::ServiceTier(command)),
                SlashCommandItem::Workflow(command) => Some(CommandItem::Workflow(command)),
            })
            .collect();
        Self {
            command_filter: String::new(),
            workflow_argument_filter: String::new(),
            commands,
            state: ScrollState::new(),
        }
    }

    /// Update the filter string based on the current composer text. The text
    /// passed in is expected to start with a leading '/'. Everything after the
    /// *first* '/' on the *first* line becomes the active filter that is used
    /// to narrow down the list of available commands.
    pub(crate) fn on_composer_text_change(&mut self, text: String) {
        let first_line = text.lines().next().unwrap_or("");
        let previous_filter = self.command_filter.clone();
        let previous_workflow_argument_filter = self.workflow_argument_filter.clone();

        if let Some(stripped) = first_line.strip_prefix('/') {
            // Extract the *first* token (sequence of non-whitespace
            // characters) after the slash so that `/clear something` still
            // shows the help for `/clear`, while the remainder narrows the
            // workflow option-hint surface.
            let token = stripped.trim_start();
            let cmd_token = token.split_whitespace().next().unwrap_or("");
            let workflow_argument_filter = token.strip_prefix(cmd_token).unwrap_or("").trim_start();

            // Update the filter keeping the original case (commands are all
            // lower-case for now but this may change in the future).
            self.command_filter = cmd_token.to_string();
            self.workflow_argument_filter = workflow_argument_filter.to_string();
        } else {
            // The composer no longer starts with '/'. Reset the filter so the
            // popup shows the *full* command list if it is still displayed
            // for some reason.
            self.command_filter.clear();
            self.workflow_argument_filter.clear();
        }

        if self.command_filter != previous_filter
            || self.workflow_argument_filter != previous_workflow_argument_filter
        {
            self.state.reset();
        }

        // Reset or clamp selected index based on new filtered list.
        let matches_len = self.filtered_items().len();
        self.state.clamp_selection(matches_len);
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

    /// Compute exact/prefix matches over built-in commands and user prompts,
    /// paired with optional highlight indices. Preserves the original
    /// presentation order for built-ins and prompts.
    fn filtered(&self) -> Vec<(CommandItem, Option<Vec<usize>>)> {
        let filter = self.command_filter.trim();
        let mut out: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        if filter.is_empty() {
            for command in self.commands.iter() {
                if matches!(command, CommandItem::Builtin(cmd) if ALIAS_COMMANDS.contains(cmd)) {
                    continue;
                }
                out.push((command.clone(), None));
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

        for command in self.commands.iter() {
            let display = command.command();
            push_match(command.clone(), display, None, 0);
        }

        let workflow_filter = self.workflow_argument_filter.trim_start();
        for idx in (0..exact.len()).rev() {
            let CommandItem::Workflow(command) = &exact[idx].0 else {
                continue;
            };
            if !command.command.eq_ignore_ascii_case(filter) {
                continue;
            }
            let option_rows = workflow_value_hint_rows(command, workflow_filter)
                .unwrap_or_else(|| {
                    command
                        .option_hints
                        .iter()
                        .filter(|option| option.display.starts_with(workflow_filter.trim()))
                        .cloned()
                        .collect()
                })
                .into_iter()
                .map(CommandItem::WorkflowOption)
                .map(|option| (option, None))
                .collect::<Vec<_>>();
            exact.splice(idx + 1..idx + 1, option_rows);
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
                let name = format!("/{}", item.command());
                let description = item.description().map(ToString::to_string);
                let (name, name_prefix_spans, is_disabled) = match &item {
                    CommandItem::WorkflowOption(option) => {
                        (option.display.clone(), vec!["  ".into()], true)
                    }
                    _ => (name, Vec::new(), false),
                };
                GenericDisplayRow {
                    name,
                    name_prefix_spans,
                    match_indices: indices.map(|v| v.into_iter().map(|i| i + 1).collect()),
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
        let len = self.filtered_items().len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    /// Move the selection cursor one step down.
    pub(crate) fn move_down(&mut self) {
        let matches_len = self.filtered_items().len();
        self.state.move_down_wrap(matches_len);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Return currently selected command, if any.
    pub(crate) fn selected_item(&self) -> Option<CommandItem> {
        let matches = self.filtered_items();
        self.state
            .selected_idx
            .and_then(|idx| matches.get(idx).cloned())
            .filter(|item| !matches!(item, CommandItem::WorkflowOption(_)))
    }

    pub(crate) fn unique_workflow_option_name_completion(&self) -> Option<String> {
        let current_token = self
            .workflow_argument_filter
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or("")
            .trim();
        if current_token.is_empty() || !current_token.starts_with('-') {
            return None;
        }

        let mut candidates = self
            .commands
            .iter()
            .filter_map(|command| match command {
                CommandItem::Workflow(command)
                    if command.command.eq_ignore_ascii_case(&self.command_filter) =>
                {
                    Some(command)
                }
                _ => None,
            })
            .flat_map(|command| command.option_hints.iter())
            .filter_map(|option| option.display.split_whitespace().next())
            .filter(|option_name| option_name.starts_with(current_token))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();

        let [candidate] = candidates.as_slice() else {
            return None;
        };
        Some(candidate.clone())
    }

    pub(crate) fn unique_workflow_option_value_completion(&self) -> Option<String> {
        let mut candidates = self
            .commands
            .iter()
            .filter_map(|command| match command {
                CommandItem::Workflow(command)
                    if command.command.eq_ignore_ascii_case(&self.command_filter) =>
                {
                    Some(command)
                }
                _ => None,
            })
            .flat_map(|command| {
                workflow_value_candidates(command, self.workflow_argument_filter.trim_start())
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();

        let [candidate] = candidates.as_slice() else {
            return None;
        };
        Some(candidate.clone())
    }
}

fn workflow_value_hint_rows(
    command: &WorkflowCommand,
    workflow_filter: &str,
) -> Option<Vec<WorkflowCommandOptionHint>> {
    let (option_name, value_prefix) = workflow_value_context(workflow_filter)?;
    Some(
        command
            .option_hints
            .iter()
            .filter(|option| option_name_from_display(&option.display) == Some(option_name))
            .flat_map(|option| {
                value_hint_values(&option.display)
                    .into_iter()
                    .filter(move |value| value.starts_with(value_prefix))
                    .map(|value| WorkflowCommandOptionHint {
                        display: format!("{option_name} {value}"),
                        description: option.description.clone(),
                    })
            })
            .collect(),
    )
}

fn workflow_value_candidates(command: &WorkflowCommand, workflow_filter: &str) -> Vec<String> {
    let Some((option_name, value_prefix)) = workflow_value_context(workflow_filter) else {
        return Vec::new();
    };
    command
        .option_hints
        .iter()
        .filter(|option| option_name_from_display(&option.display) == Some(option_name))
        .flat_map(|option| value_hint_values(&option.display))
        .filter(|value| value.starts_with(value_prefix))
        .collect()
}

fn workflow_value_context(workflow_filter: &str) -> Option<(&str, &str)> {
    let tokens = workflow_filter.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 2
        || workflow_filter
            .chars()
            .last()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let value_prefix = tokens.last()?;
    let option_name = tokens.get(tokens.len().checked_sub(2)?)?;
    option_name
        .starts_with("--")
        .then_some((*option_name, *value_prefix))
}

fn option_name_from_display(display: &str) -> Option<&str> {
    display.split_whitespace().next()
}

fn value_hint_values(display: &str) -> Vec<String> {
    let Some(value_hint) = display.split_whitespace().nth(1) else {
        return Vec::new();
    };
    let Some(values) = value_hint
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    else {
        return Vec::new();
    };
    values
        .split('|')
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

impl CommandItem {
    pub(crate) fn command(&self) -> &str {
        match self {
            Self::Builtin(cmd) => cmd.command(),
            Self::ServiceTier(command) => &command.name,
            Self::Workflow(command) => &command.command,
            Self::WorkflowOption(option) => &option.display,
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            Self::Builtin(cmd) => Some(cmd.description()),
            Self::ServiceTier(command) => Some(&command.description),
            Self::Workflow(command) => Some(&command.description),
            Self::WorkflowOption(option) => option.description.as_deref(),
        }
    }
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

    #[test]
    fn filter_includes_init_when_typing_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        // Simulate the composer line starting with '/in' so the popup filters
        // matching commands by prefix.
        popup.on_composer_text_change("/in".to_string());

        // Access the filtered list via the selected command and ensure that
        // one of the matches is the new "init" command.
        let matches = popup.filtered_items();
        let has_init = matches.iter().any(|item| match item {
            CommandItem::Builtin(cmd) => cmd.command() == "init",
            CommandItem::ServiceTier(_) => false,
            CommandItem::Workflow(_) => false,
            CommandItem::WorkflowOption(_) => false,
        });
        assert!(
            has_init,
            "expected '/init' to appear among filtered commands"
        );
    }

    #[test]
    fn selecting_init_by_exact_match() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/init".to_string());

        // When an exact match exists, the selected command should be that
        // command by default.
        let selected = popup.selected_item();
        match selected {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "init"),
            Some(CommandItem::ServiceTier(command)) => {
                panic!("expected init command, got service tier {command:?}")
            }
            Some(CommandItem::Workflow(command)) => {
                panic!("expected init command, got workflow {command:?}")
            }
            Some(CommandItem::WorkflowOption(option)) => {
                panic!("expected init command, got workflow option {option:?}")
            }
            None => panic!("expected a selected command for exact match"),
        }
    }

    #[test]
    fn model_is_first_suggestion_for_mo() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/mo".to_string());
        let matches = popup.filtered_items();
        match matches.first() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "model"),
            Some(CommandItem::ServiceTier(command)) => {
                panic!("expected model command, got service tier {command:?}")
            }
            Some(CommandItem::Workflow(command)) => {
                panic!("expected model command, got workflow {command:?}")
            }
            Some(CommandItem::WorkflowOption(option)) => {
                panic!("expected model command, got workflow option {option:?}")
            }
            None => panic!("expected at least one match for '/mo'"),
        }
    }

    #[test]
    fn service_tier_command_uses_catalog_name_and_description() {
        let mut popup = CommandPopup::new(
            CommandPopupFlags {
                service_tier_commands_enabled: true,
                ..CommandPopupFlags::default()
            },
            vec![ServiceTierCommand {
                id: "priority".to_string(),
                name: "fast".to_string(),
                description: "Fastest inference with increased plan usage".to_string(),
            }],
        );
        popup.on_composer_text_change("/fa".to_string());

        match popup.selected_item() {
            Some(CommandItem::ServiceTier(command)) => assert_eq!(
                command,
                ServiceTierCommand {
                    id: "priority".to_string(),
                    name: "fast".to_string(),
                    description: "Fastest inference with increased plan usage".to_string(),
                }
            ),
            other => panic!("expected fast service tier to be selected, got {other:?}"),
        }
        let rows = popup.rows_from_matches(popup.filtered());
        assert_eq!(
            rows.first().and_then(|row| row.description.as_deref()),
            Some("Fastest inference with increased plan usage")
        );
    }

    #[test]
    fn workflow_command_uses_manifest_name_and_description() {
        let workflow = WorkflowCommand {
            id: "code-review".to_string(),
            command: "code-review".to_string(),
            description: "Run a code review workflow.".to_string(),
            option_hints: Vec::new(),
            workflow_dir: PathBuf::from("/tmp/code-review"),
        };
        let mut popup = CommandPopup::new_with_workflows(
            CommandPopupFlags {
                workflow_commands_enabled: true,
                ..CommandPopupFlags::default()
            },
            Vec::new(),
            vec![workflow.clone()],
        );
        popup.on_composer_text_change("/code".to_string());

        assert_eq!(popup.selected_item(), Some(CommandItem::Workflow(workflow)));
        let rows = popup.rows_from_matches(popup.filtered());
        assert_eq!(
            rows.first().and_then(|row| row.description.as_deref()),
            Some("Run a code review workflow.")
        );

        let width = 72;
        let area = Rect::new(
            /*x*/ 0,
            /*y*/ 0,
            width,
            popup.calculate_required_height(width),
        );
        let mut buf = Buffer::empty(area);
        popup.render_ref(area, &mut buf);
        insta::assert_snapshot!("command_popup_workflow", format!("{buf:?}"));
    }

    #[test]
    fn workflow_exact_command_shows_static_option_hints() {
        let workflow = WorkflowCommand {
            id: "code-review".to_string(),
            command: "code-review".to_string(),
            description: "Run a code review workflow.".to_string(),
            option_hints: vec![
                WorkflowCommandOptionHint {
                    display: "--action <review|list-reports>".to_string(),
                    description: Some("Run mode.".to_string()),
                },
                WorkflowCommandOptionHint {
                    display: "--include-preexisting".to_string(),
                    description: Some("Keep preexisting findings.".to_string()),
                },
            ],
            workflow_dir: PathBuf::from("/tmp/code-review"),
        };
        let mut popup = CommandPopup::new_with_workflows(
            CommandPopupFlags {
                workflow_commands_enabled: true,
                ..CommandPopupFlags::default()
            },
            Vec::new(),
            vec![workflow],
        );
        popup.on_composer_text_change("/code-review --a".to_string());

        let rows = popup.rows_from_matches(popup.filtered());
        assert_eq!(
            rows.into_iter()
                .map(|row| (row.name, row.description, row.is_disabled))
                .collect::<Vec<_>>(),
            vec![
                (
                    "/code-review".to_string(),
                    Some("Run a code review workflow.".to_string()),
                    false,
                ),
                (
                    "--action <review|list-reports>".to_string(),
                    Some("Run mode.".to_string()),
                    true,
                ),
            ]
        );
    }

    #[test]
    fn workflow_unique_option_completion_returns_flag_name_only() {
        let workflow = WorkflowCommand {
            id: "code-review".to_string(),
            command: "code-review".to_string(),
            description: "Run a code review workflow.".to_string(),
            option_hints: vec![
                WorkflowCommandOptionHint {
                    display: "--action <review|list-reports>".to_string(),
                    description: Some("Run mode.".to_string()),
                },
                WorkflowCommandOptionHint {
                    display: "--allowed-areas <Test|Code>".to_string(),
                    description: Some("Allowed areas.".to_string()),
                },
            ],
            workflow_dir: PathBuf::from("/tmp/code-review"),
        };
        let mut popup = CommandPopup::new_with_workflows(
            CommandPopupFlags {
                workflow_commands_enabled: true,
                ..CommandPopupFlags::default()
            },
            Vec::new(),
            vec![workflow],
        );

        popup.on_composer_text_change("/code-review --acti".to_string());
        assert_eq!(
            popup.unique_workflow_option_name_completion(),
            Some("--action".to_string())
        );

        popup.on_composer_text_change("/code-review --a".to_string());
        assert_eq!(popup.unique_workflow_option_name_completion(), None);
    }

    #[test]
    fn workflow_option_value_completion_uses_value_hint_enums() {
        let workflow = WorkflowCommand {
            id: "code-review".to_string(),
            command: "code-review".to_string(),
            description: "Run a code review workflow.".to_string(),
            option_hints: vec![WorkflowCommandOptionHint {
                display: "--action <review|list-reports>".to_string(),
                description: Some("Run mode.".to_string()),
            }],
            workflow_dir: PathBuf::from("/tmp/code-review"),
        };
        let mut popup = CommandPopup::new_with_workflows(
            CommandPopupFlags {
                workflow_commands_enabled: true,
                ..CommandPopupFlags::default()
            },
            Vec::new(),
            vec![workflow],
        );
        popup.on_composer_text_change("/code-review --action li".to_string());

        assert_eq!(
            popup.unique_workflow_option_value_completion(),
            Some("list-reports".to_string())
        );
        let rows = popup.rows_from_matches(popup.filtered());
        assert_eq!(
            rows.into_iter().map(|row| row.name).collect::<Vec<_>>(),
            vec![
                "/code-review".to_string(),
                "--action list-reports".to_string(),
            ]
        );
    }

    #[test]
    fn workflow_command_hidden_when_disabled() {
        let mut popup = CommandPopup::new_with_workflows(
            CommandPopupFlags::default(),
            Vec::new(),
            vec![WorkflowCommand {
                id: "code-review".to_string(),
                command: "code-review".to_string(),
                description: "Run a code review workflow.".to_string(),
                option_hints: Vec::new(),
                workflow_dir: PathBuf::from("/tmp/code-review"),
            }],
        );
        popup.on_composer_text_change("/code".to_string());

        assert_eq!(popup.filtered_items(), Vec::new());
    }

    #[test]
    fn filtered_commands_keep_presentation_order_for_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/m".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| item.command().to_string())
            .collect();
        assert_eq!(
            cmds,
            vec![
                "model".to_string(),
                "memories".to_string(),
                "mention".to_string(),
                "mcp".to_string()
            ]
        );
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn app_command_popup_snapshot() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/app".to_string());

        let width = 72;
        let area = Rect::new(
            /*x*/ 0,
            /*y*/ 0,
            width,
            popup.calculate_required_height(width),
        );
        let mut buf = Buffer::empty(area);
        popup.render_ref(area, &mut buf);

        insta::assert_snapshot!("command_popup_app", format!("{buf:?}"));
    }

    #[test]
    fn prefix_filter_limits_matches_for_ac() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/ac".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| item.command().to_string())
            .collect();
        assert!(
            !cmds.iter().any(|cmd| cmd == "compact"),
            "expected prefix search for '/ac' to exclude 'compact', got {cmds:?}"
        );
    }

    #[test]
    fn changing_filter_resets_selection_after_scrolling() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/".to_string());

        for _ in 0..MAX_POPUP_ROWS {
            popup.move_down();
        }
        assert!(popup.state.scroll_top > 0);

        popup.on_composer_text_change("/st".to_string());

        assert_eq!(
            popup.selected_item(),
            Some(CommandItem::Builtin(SlashCommand::Status))
        );
        assert_eq!(popup.state.scroll_top, 0);
        let width = 72;
        let area = Rect::new(
            /*x*/ 0,
            /*y*/ 0,
            width,
            popup.calculate_required_height(width),
        );
        let mut buf = Buffer::empty(area);
        popup.render_ref(area, &mut buf);
        insta::assert_snapshot!(
            "command_popup_filter_reset_after_scroll",
            format!("{buf:?}")
        );
    }

    #[test]
    fn quit_hidden_in_empty_filter_but_shown_for_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/".to_string());
        let items = popup.filtered_items();
        assert!(!items.contains(&CommandItem::Builtin(SlashCommand::Quit)));

        popup.on_composer_text_change("/qu".to_string());
        let items = popup.filtered_items();
        assert!(items.contains(&CommandItem::Builtin(SlashCommand::Quit)));
    }

    #[test]
    fn btw_hidden_in_empty_filter_but_shown_for_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/".to_string());
        let items = popup.filtered_items();
        assert!(!items.contains(&CommandItem::Builtin(SlashCommand::Btw)));

        popup.on_composer_text_change("/bt".to_string());
        let items = popup.filtered_items();
        assert!(items.contains(&CommandItem::Builtin(SlashCommand::Btw)));
    }

    #[test]
    fn plan_command_hidden_when_collaboration_modes_disabled() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        popup.on_composer_text_change("/".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| item.command().to_string())
            .collect();
        assert!(
            !cmds.iter().any(|cmd| cmd == "plan"),
            "expected '/plan' to be hidden when collaboration modes are disabled, got {cmds:?}"
        );
    }

    #[test]
    fn plan_command_visible_when_collaboration_modes_enabled() {
        let mut popup = CommandPopup::new(
            CommandPopupFlags {
                collaboration_modes_enabled: true,
                connectors_enabled: false,
                plugins_command_enabled: false,
                service_tier_commands_enabled: false,
                workflow_commands_enabled: false,
                goal_command_enabled: false,
                personality_command_enabled: true,
                realtime_conversation_enabled: false,
                audio_device_selection_enabled: false,
                windows_degraded_sandbox_active: false,
                side_conversation_active: false,
            },
            Vec::new(),
        );
        popup.on_composer_text_change("/plan".to_string());

        match popup.selected_item() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "plan"),
            Some(CommandItem::ServiceTier(command)) => {
                panic!("expected plan command, got service tier {command:?}")
            }
            Some(CommandItem::Workflow(command)) => {
                panic!("expected plan command, got workflow {command:?}")
            }
            Some(CommandItem::WorkflowOption(option)) => {
                panic!("expected plan command, got workflow option {option:?}")
            }
            other => panic!("expected plan to be selected for exact match, got {other:?}"),
        }
    }

    #[test]
    fn personality_command_hidden_when_disabled() {
        let mut popup = CommandPopup::new(
            CommandPopupFlags {
                collaboration_modes_enabled: true,
                connectors_enabled: false,
                plugins_command_enabled: false,
                service_tier_commands_enabled: false,
                workflow_commands_enabled: false,
                goal_command_enabled: false,
                personality_command_enabled: false,
                realtime_conversation_enabled: false,
                audio_device_selection_enabled: false,
                windows_degraded_sandbox_active: false,
                side_conversation_active: false,
            },
            Vec::new(),
        );
        popup.on_composer_text_change("/pers".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| item.command().to_string())
            .collect();
        assert!(
            !cmds.iter().any(|cmd| cmd == "personality"),
            "expected '/personality' to be hidden when disabled, got {cmds:?}"
        );
    }

    #[test]
    fn personality_command_visible_when_enabled() {
        let mut popup = CommandPopup::new(
            CommandPopupFlags {
                collaboration_modes_enabled: true,
                connectors_enabled: false,
                plugins_command_enabled: false,
                service_tier_commands_enabled: false,
                workflow_commands_enabled: false,
                goal_command_enabled: false,
                personality_command_enabled: true,
                realtime_conversation_enabled: false,
                audio_device_selection_enabled: false,
                windows_degraded_sandbox_active: false,
                side_conversation_active: false,
            },
            Vec::new(),
        );
        popup.on_composer_text_change("/personality".to_string());

        match popup.selected_item() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "personality"),
            Some(CommandItem::ServiceTier(command)) => {
                panic!("expected personality command, got service tier {command:?}")
            }
            Some(CommandItem::Workflow(command)) => {
                panic!("expected personality command, got workflow {command:?}")
            }
            Some(CommandItem::WorkflowOption(option)) => {
                panic!("expected personality command, got workflow option {option:?}")
            }
            other => panic!("expected personality to be selected for exact match, got {other:?}"),
        }
    }

    #[test]
    fn settings_command_hidden_when_audio_device_selection_is_disabled() {
        let mut popup = CommandPopup::new(
            CommandPopupFlags {
                collaboration_modes_enabled: false,
                connectors_enabled: false,
                plugins_command_enabled: false,
                service_tier_commands_enabled: false,
                workflow_commands_enabled: false,
                goal_command_enabled: false,
                personality_command_enabled: true,
                realtime_conversation_enabled: true,
                audio_device_selection_enabled: false,
                windows_degraded_sandbox_active: false,
                side_conversation_active: false,
            },
            Vec::new(),
        );
        popup.on_composer_text_change("/aud".to_string());

        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| item.command().to_string())
            .collect();

        assert!(
            !cmds.iter().any(|cmd| cmd == "settings"),
            "expected '/settings' to be hidden when audio device selection is disabled, got {cmds:?}"
        );
    }

    #[test]
    fn debug_commands_are_hidden_from_popup() {
        let popup = CommandPopup::new(CommandPopupFlags::default(), Vec::new());
        let cmds: Vec<String> = popup
            .filtered_items()
            .into_iter()
            .map(|item| item.command().to_string())
            .collect();

        assert!(
            !cmds.iter().any(|name| name.starts_with("debug")),
            "expected no /debug* command in popup menu, got {cmds:?}"
        );
    }
}
