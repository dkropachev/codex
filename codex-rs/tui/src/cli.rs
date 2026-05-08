use clap::Args;
use clap::FromArgMatches;
use clap::Parser;
use clap::ValueEnum;
use codex_protocol::protocol::RepoCiIssueType;
use codex_utils_cli::ApprovalModeCliArg;
use codex_utils_cli::CliConfigOverrides;
use codex_utils_cli::SharedCliOptions;

#[derive(Parser, Debug)]
#[command(version)]
pub struct Cli {
    /// Optional user prompt to start the session.
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,

    // Internal controls set by the top-level `codex resume` subcommand.
    // These are not exposed as user flags on the base `codex` command.
    #[clap(skip)]
    pub resume_picker: bool,

    #[clap(skip)]
    pub resume_last: bool,

    /// Internal: resume a specific recorded session by id (UUID). Set by the
    /// top-level `codex resume <SESSION_ID>` wrapper; not exposed as a public flag.
    #[clap(skip)]
    pub resume_session_id: Option<String>,

    /// Internal: show all sessions (disables cwd filtering and shows CWD column).
    #[clap(skip)]
    pub resume_show_all: bool,

    /// Internal: include non-interactive sessions in resume listings.
    #[clap(skip)]
    pub resume_include_non_interactive: bool,

    // Internal controls set by the top-level `codex fork` subcommand.
    // These are not exposed as user flags on the base `codex` command.
    #[clap(skip)]
    pub fork_picker: bool,

    #[clap(skip)]
    pub fork_last: bool,

    /// Internal: fork a specific recorded session by id (UUID). Set by the
    /// top-level `codex fork <SESSION_ID>` wrapper; not exposed as a public flag.
    #[clap(skip)]
    pub fork_session_id: Option<String>,

    /// Internal: show all sessions (disables cwd filtering and shows CWD column).
    #[clap(skip)]
    pub fork_show_all: bool,

    #[clap(flatten)]
    pub shared: TuiSharedCliOptions,

    /// Configure when the model requires human approval before executing a command.
    #[arg(long = "ask-for-approval", short = 'a')]
    pub approval_policy: Option<ApprovalModeCliArg>,

    /// Enable live web search. When enabled, the native Responses `web_search` tool is available to the model (no per‑call approval).
    #[arg(long = "search", default_value_t = false)]
    pub web_search: bool,

    /// Override repo CI automation for this session.
    #[arg(long = "repo-ci", value_enum)]
    pub repo_ci: Option<RepoCiCliMode>,

    /// Override targeted repo CI review issue types for this session.
    /// Use `none` to disable the review phase.
    #[arg(long = "repo-ci-issue-types", value_delimiter = ',', value_enum)]
    pub repo_ci_issue_types: Option<Vec<RepoCiIssueTypeCliArg>>,

    /// Override repo CI review/fix round limit for this session.
    #[arg(long = "repo-ci-review-rounds")]
    pub repo_ci_review_rounds: Option<u8>,

    /// Disable alternate screen mode
    ///
    /// Runs the TUI in inline mode, preserving terminal scrollback history. This is useful
    /// in terminal multiplexers like Zellij that follow the xterm spec strictly and disable
    /// scrollback in alternate screen buffers.
    #[arg(long = "no-alt-screen", default_value_t = false)]
    pub no_alt_screen: bool,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RepoCiCliMode {
    Off,
    Local,
    Remote,
    LocalAndRemote,
}

impl From<RepoCiCliMode> for codex_protocol::protocol::RepoCiSessionMode {
    fn from(value: RepoCiCliMode) -> Self {
        match value {
            RepoCiCliMode::Off => Self::Off,
            RepoCiCliMode::Local => Self::Local,
            RepoCiCliMode::Remote => Self::Remote,
            RepoCiCliMode::LocalAndRemote => Self::LocalAndRemote,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RepoCiIssueTypeCliArg {
    Correctness,
    Reliability,
    Performance,
    Scalability,
    Security,
    Maintainability,
    Testability,
    Observability,
    Compatibility,
    #[value(name = "ux-config-cli")]
    UxConfigCli,
    None,
}

impl RepoCiIssueTypeCliArg {
    pub fn normalize_list(
        values: Option<Vec<Self>>,
    ) -> Result<Option<Vec<RepoCiIssueType>>, String> {
        let Some(values) = values else {
            return Ok(None);
        };
        if values.contains(&Self::None) {
            if values.len() == 1 {
                return Ok(Some(Vec::new()));
            }
            return Err("`none` cannot be combined with other repo CI issue types".to_string());
        }
        Ok(Some(values.into_iter().map(Into::into).collect()))
    }
}

impl From<RepoCiIssueTypeCliArg> for RepoCiIssueType {
    fn from(value: RepoCiIssueTypeCliArg) -> Self {
        match value {
            RepoCiIssueTypeCliArg::Correctness => Self::Correctness,
            RepoCiIssueTypeCliArg::Reliability => Self::Reliability,
            RepoCiIssueTypeCliArg::Performance => Self::Performance,
            RepoCiIssueTypeCliArg::Scalability => Self::Scalability,
            RepoCiIssueTypeCliArg::Security => Self::Security,
            RepoCiIssueTypeCliArg::Maintainability => Self::Maintainability,
            RepoCiIssueTypeCliArg::Testability => Self::Testability,
            RepoCiIssueTypeCliArg::Observability => Self::Observability,
            RepoCiIssueTypeCliArg::Compatibility => Self::Compatibility,
            RepoCiIssueTypeCliArg::UxConfigCli => Self::UxConfigCli,
            RepoCiIssueTypeCliArg::None => unreachable!("normalized out before conversion"),
        }
    }
}

impl std::ops::Deref for Cli {
    type Target = SharedCliOptions;

    fn deref(&self) -> &Self::Target {
        &self.shared.0
    }
}

impl std::ops::DerefMut for Cli {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.shared.0
    }
}

#[derive(Debug, Default)]
pub struct TuiSharedCliOptions(SharedCliOptions);

impl TuiSharedCliOptions {
    pub fn into_inner(self) -> SharedCliOptions {
        self.0
    }
}

impl std::ops::Deref for TuiSharedCliOptions {
    type Target = SharedCliOptions;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for TuiSharedCliOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Args for TuiSharedCliOptions {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        mark_tui_args(SharedCliOptions::augment_args(cmd))
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        mark_tui_args(SharedCliOptions::augment_args_for_update(cmd))
    }
}

impl FromArgMatches for TuiSharedCliOptions {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        SharedCliOptions::from_arg_matches(matches).map(Self)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)
    }
}

fn mark_tui_args(cmd: clap::Command) -> clap::Command {
    cmd.mut_arg("dangerously_bypass_approvals_and_sandbox", |arg| {
        arg.conflicts_with("approval_policy")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_repo_ci_review_flags() {
        let cli = Cli::try_parse_from([
            "codex",
            "--repo-ci",
            "remote",
            "--repo-ci-issue-types",
            "correctness,security",
            "--repo-ci-review-rounds",
            "3",
        ])
        .expect("parse should succeed");

        assert!(matches!(cli.repo_ci, Some(RepoCiCliMode::Remote)));
        assert_eq!(
            RepoCiIssueTypeCliArg::normalize_list(cli.repo_ci_issue_types),
            Ok(Some(vec![
                RepoCiIssueType::Correctness,
                RepoCiIssueType::Security
            ]))
        );
        assert_eq!(cli.repo_ci_review_rounds, Some(3));
    }

    #[test]
    fn parses_repo_ci_issue_types_none() {
        let cli = Cli::try_parse_from(["codex", "--repo-ci-issue-types", "none"])
            .expect("parse should succeed");

        assert_eq!(
            RepoCiIssueTypeCliArg::normalize_list(cli.repo_ci_issue_types),
            Ok(Some(Vec::new()))
        );
    }

    #[test]
    fn rejects_mixing_none_with_other_issue_types() {
        assert_eq!(
            RepoCiIssueTypeCliArg::normalize_list(Some(vec![
                RepoCiIssueTypeCliArg::None,
                RepoCiIssueTypeCliArg::Security,
            ])),
            Err("`none` cannot be combined with other repo CI issue types".to_string())
        );
    }
}
