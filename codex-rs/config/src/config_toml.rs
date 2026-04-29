//! Schema-heavy configuration TOML types used by Codex.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

use crate::HookEventsToml;
use crate::permissions_toml::PermissionsToml;
use crate::profile_toml::ConfigProfile;
use crate::types::AnalyticsConfigToml;
use crate::types::ApprovalsReviewer;
use crate::types::AppsConfigToml;
use crate::types::AuthCredentialsStoreMode;
use crate::types::FeedbackConfigToml;
use crate::types::History;
use crate::types::MarketplaceConfig;
use crate::types::McpServerConfig;
use crate::types::MemoriesToml;
use crate::types::Notice;
use crate::types::OAuthCredentialsStoreMode;
use crate::types::OtelConfigToml;
use crate::types::PluginConfig;
use crate::types::SandboxWorkspaceWrite;
use crate::types::ShellEnvironmentPolicyToml;
use crate::types::SkillsConfig;
use crate::types::ToolSuggestConfig;
use crate::types::Tui;
use crate::types::UriBasedFileOpener;
use crate::types::WindowsToml;
use codex_app_server_protocol::Tools;
use codex_app_server_protocol::UserSavedConfig;
use codex_features::FeaturesToml;
use codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
use codex_model_provider_info::LEGACY_OLLAMA_CHAT_PROVIDER_ID;
use codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OLLAMA_CHAT_PROVIDER_REMOVED_ERROR;
use codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::config_types::Verbosity;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::config_types::WebSearchToolConfig;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::RepoCiIssueType;
use codex_protocol::protocol::SandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path::normalize_for_path_comparison;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

const RESERVED_MODEL_PROVIDER_IDS: [&str; 4] = [
    AMAZON_BEDROCK_PROVIDER_ID,
    OPENAI_PROVIDER_ID,
    OLLAMA_OSS_PROVIDER_ID,
    LMSTUDIO_OSS_PROVIDER_ID,
];

/// Base config deserialized from ~/.codex/config.toml.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigToml {
    /// Optional override of model selection.
    pub model: Option<String>,
    /// Review model override used by the `/review` feature.
    pub review_model: Option<String>,

    /// Provider to use from the model_providers map.
    pub model_provider: Option<String>,

    /// Size of the context window for the model, in tokens.
    pub model_context_window: Option<i64>,

    /// Token usage threshold triggering auto-compaction of conversation history.
    pub model_auto_compact_token_limit: Option<i64>,

    /// Default approval policy for executing commands.
    pub approval_policy: Option<AskForApproval>,

    /// Configures who approval requests are routed to for review once they have
    /// been escalated. This does not disable separate safety checks such as
    /// ARC.
    pub approvals_reviewer: Option<ApprovalsReviewer>,

    /// Optional policy instructions for the guardian auto-reviewer.
    #[serde(default)]
    pub auto_review: Option<AutoReviewToml>,

    #[serde(default)]
    pub shell_environment_policy: ShellEnvironmentPolicyToml,

    /// Whether the model may request a login shell for shell-based tools.
    /// Default to `true`
    ///
    /// If `true`, the model may request a login shell (`login = true`), and
    /// omitting `login` defaults to using a login shell.
    /// If `false`, the model can never use a login shell: `login = true`
    /// requests are rejected, and omitting `login` defaults to a non-login
    /// shell.
    pub allow_login_shell: Option<bool>,

    /// Sandbox mode to use.
    pub sandbox_mode: Option<SandboxMode>,

    /// Sandbox configuration to apply if `sandbox` is `WorkspaceWrite`.
    pub sandbox_workspace_write: Option<SandboxWorkspaceWrite>,

    /// Default named permissions profile to apply from the `[permissions]`
    /// table.
    pub default_permissions: Option<String>,

    /// Named permissions profiles.
    #[serde(default)]
    pub permissions: Option<PermissionsToml>,

    /// Optional external command to spawn for end-user notifications.
    #[serde(default)]
    pub notify: Option<Vec<String>>,

    /// System instructions.
    pub instructions: Option<String>,

    /// Developer instructions inserted as a `developer` role message.
    #[serde(default)]
    pub developer_instructions: Option<String>,

    /// Whether to inject the `<permissions instructions>` developer block.
    pub include_permissions_instructions: Option<bool>,

    /// Whether to inject the `<apps_instructions>` developer block.
    pub include_apps_instructions: Option<bool>,

    /// Whether to inject the `<environment_context>` user block.
    pub include_environment_context: Option<bool>,

    /// Optional path to a file containing model instructions that will override
    /// the built-in instructions for the selected model. Users are STRONGLY
    /// DISCOURAGED from using this field, as deviating from the instructions
    /// sanctioned by Codex will likely degrade model performance.
    pub model_instructions_file: Option<AbsolutePathBuf>,

    /// Compact prompt used for history compaction.
    pub compact_prompt: Option<String>,

    /// Optional commit attribution text for commit message co-author trailers.
    ///
    /// Set to an empty string to disable automatic commit attribution.
    pub commit_attribution: Option<String>,

    /// When set, restricts ChatGPT login to a specific workspace identifier.
    #[serde(default)]
    pub forced_chatgpt_workspace_id: Option<String>,

    /// When set, restricts the login mechanism users may use.
    #[serde(default)]
    pub forced_login_method: Option<ForcedLoginMethod>,

    /// Preferred backend for storing CLI auth credentials.
    /// file (default): Use a file in the Codex home directory.
    /// keyring: Use an OS-specific keyring service.
    /// auto: Use the keyring if available, otherwise use a file.
    #[serde(default)]
    pub cli_auth_credentials_store: Option<AuthCredentialsStoreMode>,

    /// Definition for MCP servers that Codex can reach out to for tool calls.
    #[serde(default)]
    // Uses the raw MCP input shape (custom deserialization) rather than `McpServerConfig`.
    #[schemars(schema_with = "crate::schema::mcp_servers_schema")]
    pub mcp_servers: HashMap<String, McpServerConfig>,

    /// Preferred backend for storing MCP OAuth credentials.
    /// keyring: Use an OS-specific keyring service.
    ///          https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/src/oauth.rs#L2
    /// file: Use a file in the Codex home directory.
    /// auto (default): Use the OS-specific keyring service if available, otherwise use a file.
    #[serde(default)]
    pub mcp_oauth_credentials_store: Option<OAuthCredentialsStoreMode>,

    /// Optional fixed port for the local HTTP callback server used during MCP OAuth login.
    /// When unset, Codex will bind to an ephemeral port chosen by the OS.
    pub mcp_oauth_callback_port: Option<u16>,

    /// Optional redirect URI to use during MCP OAuth login.
    /// When set, this URI is used in the OAuth authorization request instead
    /// of the local listener address. The local callback listener still binds
    /// to 127.0.0.1 (using `mcp_oauth_callback_port` when provided).
    pub mcp_oauth_callback_url: Option<String>,

    /// User-defined provider entries that extend the built-in list. Built-in
    /// IDs cannot be overridden.
    #[serde(default, deserialize_with = "deserialize_model_providers")]
    pub model_providers: HashMap<String, ModelProviderInfo>,

    /// Optional logical pools of bounded Codex subscription accounts.
    #[serde(default, deserialize_with = "deserialize_account_pool")]
    pub account_pool: Option<AccountPoolToml>,

    /// Optional routing policy for internal model calls.
    #[serde(default, deserialize_with = "deserialize_model_policy")]
    pub model_policy: Option<ModelPolicyToml>,

    /// Maximum number of bytes to include from an AGENTS.md project doc file.
    pub project_doc_max_bytes: Option<usize>,

    /// Ordered list of fallback filenames to look for when AGENTS.md is missing.
    pub project_doc_fallback_filenames: Option<Vec<String>>,

    /// Token budget applied when storing tool/function outputs in the context manager.
    pub tool_output_token_limit: Option<usize>,

    /// Maximum poll window for background terminal output (`write_stdin`), in milliseconds.
    /// Default: `300000` (5 minutes).
    pub background_terminal_max_timeout: Option<u64>,

    /// Optional absolute path to the Node runtime used by `js_repl`.
    pub js_repl_node_path: Option<AbsolutePathBuf>,

    /// Ordered list of directories to search for Node modules in `js_repl`.
    pub js_repl_node_module_dirs: Option<Vec<AbsolutePathBuf>>,

    /// Optional absolute path to patched zsh used by zsh-exec-bridge-backed shell execution.
    pub zsh_path: Option<AbsolutePathBuf>,

    /// Profile to use from the `profiles` map.
    pub profile: Option<String>,

    /// Named profiles to facilitate switching between different configurations.
    #[serde(default)]
    pub profiles: HashMap<String, ConfigProfile>,

    /// Settings that govern if and what will be written to `~/.codex/history.jsonl`.
    #[serde(default)]
    pub history: Option<History>,

    /// Directory where Codex stores the SQLite state DB.
    /// Defaults to `$CODEX_SQLITE_HOME` when set. Otherwise uses `$CODEX_HOME`.
    pub sqlite_home: Option<AbsolutePathBuf>,

    /// Directory where Codex writes log files, for example `codex-tui.log`.
    /// Defaults to `$CODEX_HOME/log`.
    pub log_dir: Option<AbsolutePathBuf>,

    /// Optional URI-based file opener. If set, citations to files in the model
    /// output will be hyperlinked using the specified URI scheme.
    pub file_opener: Option<UriBasedFileOpener>,

    /// Collection of settings that are specific to the TUI.
    pub tui: Option<Tui>,

    /// When set to `true`, `AgentReasoning` events will be hidden from the
    /// UI/output. Defaults to `false`.
    pub hide_agent_reasoning: Option<bool>,

    /// When set to `true`, `AgentReasoningRawContentEvent` events will be shown in the UI/output.
    /// Defaults to `false`.
    pub show_raw_agent_reasoning: Option<bool>,

    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    /// Optional verbosity control for GPT-5 models (Responses API `text.verbosity`).
    pub model_verbosity: Option<Verbosity>,

    /// Override to force-enable reasoning summaries for the configured model.
    pub model_supports_reasoning_summaries: Option<bool>,

    /// Optional path to a JSON model catalog (applied on startup only).
    /// Per-thread `config` overrides are accepted but do not reapply this (no-ops).
    pub model_catalog_json: Option<AbsolutePathBuf>,

    /// Optionally specify a personality for the model
    pub personality: Option<Personality>,

    /// Optional explicit service tier preference for new turns (`fast` or `flex`).
    pub service_tier: Option<ServiceTier>,

    /// Base URL for requests to ChatGPT (as opposed to the OpenAI API).
    pub chatgpt_base_url: Option<String>,

    /// Base URL override for the built-in `openai` model provider.
    pub openai_base_url: Option<String>,

    /// Machine-local realtime audio device preferences used by realtime voice.
    #[serde(default)]
    pub audio: Option<RealtimeAudioToml>,

    /// Experimental / do not use. Overrides only the realtime conversation
    /// websocket transport base URL (the `Op::RealtimeConversation`
    /// `/v1/realtime`
    /// connection) without changing normal provider HTTP requests.
    pub experimental_realtime_ws_base_url: Option<String>,
    /// Experimental / do not use. Selects the realtime websocket model/snapshot
    /// used for the `Op::RealtimeConversation` connection.
    pub experimental_realtime_ws_model: Option<String>,
    /// Experimental / do not use. Realtime websocket session selection.
    /// `version` controls v1/v2 and `type` controls conversational/transcription.
    #[serde(default)]
    pub realtime: Option<RealtimeToml>,
    /// Experimental / do not use. Overrides only the realtime conversation
    /// websocket transport instructions (the `Op::RealtimeConversation`
    /// `/ws` session.update instructions) without changing normal prompts.
    pub experimental_realtime_ws_backend_prompt: Option<String>,
    /// Experimental / do not use. Replaces the synthesized realtime startup
    /// context appended to websocket session instructions. An empty string
    /// disables startup context injection entirely.
    pub experimental_realtime_ws_startup_context: Option<String>,
    /// Experimental / do not use. Replaces the built-in realtime start
    /// instructions inserted into developer messages when realtime becomes
    /// active.
    pub experimental_realtime_start_instructions: Option<String>,

    /// Experimental / do not use. When set, app-server uses a remote thread
    /// store at this endpoint instead of the local filesystem/SQLite store.
    pub experimental_thread_store_endpoint: Option<String>,

    /// Experimental / do not use. When set, app-server fetches thread-scoped
    /// config from a remote service at this endpoint.
    pub experimental_thread_config_endpoint: Option<String>,
    pub projects: Option<HashMap<String, ProjectConfig>>,

    /// Controls the web search tool mode: disabled, cached, or live.
    pub web_search: Option<WebSearchMode>,

    /// Nested tools section for feature toggles
    pub tools: Option<ToolsToml>,

    /// Additional discoverable tools that can be suggested for installation.
    pub tool_suggest: Option<ToolSuggestConfig>,

    /// Agent-related settings (thread limits, etc.).
    pub agents: Option<AgentsToml>,

    /// Memories subsystem settings.
    pub memories: Option<MemoriesToml>,

    /// User-level skill config entries keyed by SKILL.md path.
    pub skills: Option<SkillsConfig>,

    /// Lifecycle hooks configured inline in TOML.
    pub hooks: Option<HookEventsToml>,

    /// User-level plugin config entries keyed by plugin name.
    #[serde(default)]
    pub plugins: HashMap<String, PluginConfig>,

    /// User-level marketplace entries keyed by marketplace name.
    #[serde(default)]
    pub marketplaces: HashMap<String, MarketplaceConfig>,

    /// Centralized feature flags (new). Prefer this over individual toggles.
    #[serde(default)]
    // Injects known feature keys into the schema and forbids unknown keys.
    #[schemars(schema_with = "crate::schema::features_schema")]
    pub features: Option<FeaturesToml>,

    /// Repository CI learning and validation settings.
    #[serde(default)]
    pub repo_ci: Option<RepoCiToml>,

    /// Suppress warnings about unstable (under development) features.
    pub suppress_unstable_features_warning: Option<bool>,

    /// Settings for ghost snapshots (used for undo).
    #[serde(default)]
    pub ghost_snapshot: Option<GhostSnapshotToml>,

    /// Markers used to detect the project root when searching parent
    /// directories for `.codex` folders. Defaults to [".git"] when unset.
    #[serde(default)]
    pub project_root_markers: Option<Vec<String>>,

    /// When `true`, checks for Codex updates on startup and surfaces update prompts.
    /// Set to `false` only if your Codex updates are centrally managed.
    /// Defaults to `true`.
    pub check_for_update_on_startup: Option<bool>,

    /// When true, disables burst-paste detection for typed input entirely.
    /// All characters are inserted as they are received, and no buffering
    /// or placeholder replacement will occur for fast keypress bursts.
    pub disable_paste_burst: Option<bool>,

    /// When `false`, disables analytics across Codex product surfaces in this machine.
    /// Defaults to `true`.
    pub analytics: Option<AnalyticsConfigToml>,

    /// When `false`, disables feedback collection across Codex product surfaces.
    /// Defaults to `true`.
    pub feedback: Option<FeedbackConfigToml>,

    /// Settings for app-specific controls.
    #[serde(default)]
    pub apps: Option<AppsConfigToml>,

    /// OTEL configuration.
    pub otel: Option<OtelConfigToml>,

    /// Windows-specific configuration.
    #[serde(default)]
    pub windows: Option<WindowsToml>,

    /// Tracks whether the Windows onboarding screen has been acknowledged.
    pub windows_wsl_setup_acknowledged: Option<bool>,

    /// Collection of in-product notices (different from notifications)
    /// See [`crate::types::Notice`] for more details
    pub notice: Option<Notice>,

    /// Legacy, now use features
    /// Deprecated: ignored. Use `model_instructions_file`.
    #[schemars(skip)]
    pub experimental_instructions_file: Option<AbsolutePathBuf>,
    pub experimental_compact_prompt_file: Option<AbsolutePathBuf>,
    pub experimental_use_unified_exec_tool: Option<bool>,
    pub experimental_use_freeform_apply_patch: Option<bool>,
    /// Preferred OSS provider for local models, e.g. "lmstudio" or "ollama".
    pub oss_provider: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RepoCiToml {
    /// Default settings used when no narrower scope matches.
    #[serde(default)]
    pub defaults: Option<RepoCiScopeToml>,

    /// Per-directory settings keyed by absolute or user-relative path.
    #[serde(default)]
    pub directories: BTreeMap<String, RepoCiScopeToml>,

    /// Per-GitHub-repository settings keyed by `owner/name`.
    #[serde(default)]
    pub github_repos: BTreeMap<String, RepoCiScopeToml>,

    /// Per-GitHub-organization settings keyed by org name.
    #[serde(default)]
    pub github_orgs: BTreeMap<String, RepoCiScopeToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct RepoCiScopeToml {
    /// Whether repo CI should run automatically for this scope.
    pub enabled: Option<bool>,

    /// Where checks should run.
    pub automation: Option<RepoCiAutomationToml>,

    /// Local integration/e2e/ui tests above this budget are not selected for the fast runner.
    pub local_test_time_budget_sec: Option<u64>,

    /// Whether local repo CI should run the full runner, including long integration/e2e checks.
    pub long_ci: Option<bool>,

    /// Maximum number of local fix attempts before surfacing the failure.
    pub max_local_fix_rounds: Option<u8>,

    /// Maximum number of remote CI fix attempts before surfacing the failure.
    pub max_remote_fix_rounds: Option<u8>,

    /// Targeted review issue categories for repo CI preflight review.
    #[serde(default)]
    pub review_issue_types: Option<Vec<RepoCiIssueType>>,

    /// Maximum number of repo CI targeted review/fix rounds before surfacing the result.
    pub max_review_fix_rounds: Option<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RepoCiAutomationToml {
    Local,
    Remote,
    LocalAndRemote,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
pub struct AutoReviewToml {
    /// Additional policy instructions inserted into the guardian prompt.
    pub policy: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AccountPoolToml {
    #[serde(default)]
    pub enabled: bool,

    pub default_pool: Option<String>,

    #[serde(default)]
    pub pools: BTreeMap<String, AccountPoolDefinitionToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AccountPoolDefinitionToml {
    pub provider: String,
    pub policy: AccountPoolPolicyToml,
    pub accounts: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountPoolPolicyToml {
    Drain,
    LoadBalance,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelPolicyToml {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub rules: Vec<ModelPolicyRuleToml>,

    #[serde(default)]
    pub default_route: Option<ModelPolicyRouteToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelPolicyRuleToml {
    /// Source selector, for example `subagent`, `subagent.review`,
    /// `module.memory_consolidation`, or `*`. Accepts one selector or a list.
    #[serde(
        default,
        alias = "sources",
        deserialize_with = "deserialize_model_policy_sources"
    )]
    pub source: Option<Vec<String>>,

    /// Inclusive lower prompt-size bound, in UTF-8 bytes.
    pub min_prompt_bytes: Option<usize>,

    /// Inclusive upper prompt-size bound, in UTF-8 bytes.
    pub max_prompt_bytes: Option<usize>,

    #[serde(flatten)]
    pub route: ModelPolicyRouteToml,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelPolicyRouteToml {
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub service_tier: Option<ServiceTier>,
    pub reasoning_effort: Option<ModelPolicyReasoningEffortToml>,
    pub account_pool: Option<String>,
    pub account: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelPolicyReasoningEffortToml {
    /// Preserve the reasoning effort already selected by the parent/default config.
    Inherit,
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

impl ModelPolicyReasoningEffortToml {
    pub fn as_reasoning_effort(self) -> Option<ReasoningEffort> {
        match self {
            ModelPolicyReasoningEffortToml::Inherit => None,
            ModelPolicyReasoningEffortToml::None => Some(ReasoningEffort::None),
            ModelPolicyReasoningEffortToml::Minimal => Some(ReasoningEffort::Minimal),
            ModelPolicyReasoningEffortToml::Low => Some(ReasoningEffort::Low),
            ModelPolicyReasoningEffortToml::Medium => Some(ReasoningEffort::Medium),
            ModelPolicyReasoningEffortToml::High => Some(ReasoningEffort::High),
            ModelPolicyReasoningEffortToml::XHigh => Some(ReasoningEffort::XHigh),
        }
    }
}

impl From<ConfigToml> for UserSavedConfig {
    fn from(config_toml: ConfigToml) -> Self {
        let profiles = config_toml
            .profiles
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect();

        Self {
            approval_policy: config_toml.approval_policy,
            sandbox_mode: config_toml.sandbox_mode,
            sandbox_settings: config_toml.sandbox_workspace_write.map(From::from),
            forced_chatgpt_workspace_id: config_toml.forced_chatgpt_workspace_id,
            forced_login_method: config_toml.forced_login_method,
            model: config_toml.model,
            model_reasoning_effort: config_toml.model_reasoning_effort,
            model_reasoning_summary: config_toml.model_reasoning_summary,
            model_verbosity: config_toml.model_verbosity,
            tools: config_toml.tools.map(From::from),
            profile: config_toml.profile,
            profiles,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProjectConfig {
    pub trust_level: Option<TrustLevel>,
}

impl ProjectConfig {
    pub fn is_trusted(&self) -> bool {
        matches!(self.trust_level, Some(TrustLevel::Trusted))
    }

    pub fn is_untrusted(&self) -> bool {
        matches!(self.trust_level, Some(TrustLevel::Untrusted))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RealtimeAudioConfig {
    pub microphone: Option<String>,
    pub speaker: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeWsMode {
    #[default]
    Conversational,
    Transcription,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeTransport {
    #[default]
    #[serde(rename = "webrtc")]
    WebRtc,
    Websocket,
}

pub use codex_protocol::protocol::RealtimeConversationVersion as RealtimeWsVersion;
pub use codex_protocol::protocol::RealtimeVoice;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RealtimeConfig {
    pub version: RealtimeWsVersion,
    #[serde(rename = "type")]
    pub session_type: RealtimeWsMode,
    pub transport: RealtimeTransport,
    pub voice: Option<RealtimeVoice>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RealtimeToml {
    pub version: Option<RealtimeWsVersion>,
    #[serde(rename = "type")]
    pub session_type: Option<RealtimeWsMode>,
    pub transport: Option<RealtimeTransport>,
    pub voice: Option<RealtimeVoice>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RealtimeAudioToml {
    pub microphone: Option<String>,
    pub speaker: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolsToml {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_web_search_tool_config"
    )]
    pub web_search: Option<WebSearchToolConfig>,

    /// Enable the `view_image` tool that lets the agent attach local images.
    #[serde(default)]
    pub view_image: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WebSearchToolConfigInput {
    Enabled(bool),
    Config(WebSearchToolConfig),
}

fn deserialize_optional_web_search_tool_config<'de, D>(
    deserializer: D,
) -> Result<Option<WebSearchToolConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<WebSearchToolConfigInput>::deserialize(deserializer)?;

    Ok(match value {
        None => None,
        Some(WebSearchToolConfigInput::Enabled(enabled)) => {
            let _ = enabled;
            None
        }
        Some(WebSearchToolConfigInput::Config(config)) => Some(config),
    })
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentsToml {
    /// Maximum number of agent threads that can be open concurrently.
    /// When unset, no limit is enforced.
    #[schemars(range(min = 1))]
    pub max_threads: Option<usize>,
    /// Maximum nesting depth allowed for spawned agent threads.
    /// Root sessions start at depth 0.
    #[schemars(range(min = 1))]
    pub max_depth: Option<i32>,
    /// Default maximum runtime in seconds for agent job workers.
    #[schemars(range(min = 1))]
    pub job_max_runtime_seconds: Option<u64>,

    /// User-defined role declarations keyed by role name.
    ///
    /// Example:
    /// ```toml
    /// [agents.researcher]
    /// description = "Research-focused role."
    /// config_file = "./agents/researcher.toml"
    /// nickname_candidates = ["Herodotus", "Ibn Battuta"]
    /// ```
    #[serde(default, flatten)]
    pub roles: BTreeMap<String, AgentRoleToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentRoleToml {
    /// Human-facing role documentation used in spawn tool guidance.
    /// Required unless supplied by the referenced agent role file.
    pub description: Option<String>,

    /// Path to a role-specific config layer.
    /// Relative paths are resolved relative to the `config.toml` that defines them.
    pub config_file: Option<AbsolutePathBuf>,

    /// Candidate nicknames for agents spawned with this role.
    pub nickname_candidates: Option<Vec<String>>,
}

impl From<ToolsToml> for Tools {
    fn from(tools_toml: ToolsToml) -> Self {
        Self {
            web_search: tools_toml.web_search.is_some().then_some(true),
            view_image: tools_toml.view_image,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GhostSnapshotToml {
    /// Exclude untracked files larger than this many bytes from ghost snapshots.
    #[serde(alias = "ignore_untracked_files_over_bytes")]
    pub ignore_large_untracked_files: Option<i64>,
    /// Ignore untracked directories that contain this many files or more.
    /// (Still emits a warning unless warnings are disabled.)
    #[serde(alias = "large_untracked_dir_warning_threshold")]
    pub ignore_large_untracked_dirs: Option<i64>,
    /// Disable all ghost snapshot warning events.
    pub disable_warnings: Option<bool>,
}

impl ConfigToml {
    /// Derive the effective sandbox policy from the configuration.
    pub async fn derive_sandbox_policy(
        &self,
        sandbox_mode_override: Option<SandboxMode>,
        profile_sandbox_mode: Option<SandboxMode>,
        windows_sandbox_level: WindowsSandboxLevel,
        active_project: Option<&ProjectConfig>,
        sandbox_policy_constraint: Option<&crate::Constrained<SandboxPolicy>>,
    ) -> SandboxPolicy {
        let sandbox_mode_was_explicit = sandbox_mode_override.is_some()
            || profile_sandbox_mode.is_some()
            || self.sandbox_mode.is_some();
        let resolved_sandbox_mode = sandbox_mode_override
            .or(profile_sandbox_mode)
            .or(self.sandbox_mode)
            .or(if sandbox_mode_was_explicit {
                None
            } else {
                // If no sandbox_mode is set but this directory has a trust decision,
                // default to workspace-write except on unsandboxed Windows where we
                // default to read-only.
                active_project.and_then(|p| {
                    if p.is_trusted() || p.is_untrusted() {
                        if cfg!(target_os = "windows")
                            && windows_sandbox_level == WindowsSandboxLevel::Disabled
                        {
                            Some(SandboxMode::ReadOnly)
                        } else {
                            Some(SandboxMode::WorkspaceWrite)
                        }
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();
        let mut sandbox_policy = match resolved_sandbox_mode {
            SandboxMode::ReadOnly => SandboxPolicy::new_read_only_policy(),
            SandboxMode::WorkspaceWrite => match self.sandbox_workspace_write.as_ref() {
                Some(SandboxWorkspaceWrite {
                    writable_roots,
                    network_access,
                    exclude_tmpdir_env_var,
                    exclude_slash_tmp,
                }) => SandboxPolicy::WorkspaceWrite {
                    writable_roots: writable_roots.clone(),
                    network_access: *network_access,
                    exclude_tmpdir_env_var: *exclude_tmpdir_env_var,
                    exclude_slash_tmp: *exclude_slash_tmp,
                },
                None => SandboxPolicy::new_workspace_write_policy(),
            },
            SandboxMode::DangerFullAccess => SandboxPolicy::DangerFullAccess,
        };
        let downgrade_workspace_write_if_unsupported = |policy: &mut SandboxPolicy| {
            if cfg!(target_os = "windows")
                // If the experimental Windows sandbox is enabled, do not force a downgrade.
                && windows_sandbox_level == WindowsSandboxLevel::Disabled
                && matches!(&*policy, SandboxPolicy::WorkspaceWrite { .. })
            {
                *policy = SandboxPolicy::new_read_only_policy();
            }
        };
        if matches!(resolved_sandbox_mode, SandboxMode::WorkspaceWrite) {
            downgrade_workspace_write_if_unsupported(&mut sandbox_policy);
        }
        if !sandbox_mode_was_explicit
            && let Some(constraint) = sandbox_policy_constraint
            && let Err(err) = constraint.can_set(&sandbox_policy)
        {
            tracing::warn!(
                error = %err,
                "default sandbox policy is disallowed by requirements; falling back to required default"
            );
            sandbox_policy = constraint.get().clone();
            downgrade_workspace_write_if_unsupported(&mut sandbox_policy);
        }
        sandbox_policy
    }

    /// Resolves the cwd to an existing project, or returns None if ConfigToml
    /// does not contain a project corresponding to cwd or the resolved git repo
    /// root for cwd.
    pub fn get_active_project(
        &self,
        resolved_cwd: &Path,
        repo_root: Option<&Path>,
    ) -> Option<ProjectConfig> {
        let projects = self.projects.as_ref()?;

        for normalized_cwd in normalized_project_lookup_keys(resolved_cwd) {
            if let Some(project_config) = project_config_for_lookup_key(projects, &normalized_cwd) {
                return Some(project_config);
            }
        }

        if let Some(repo_root) = repo_root {
            for normalized_repo_root in normalized_project_lookup_keys(repo_root) {
                if let Some(project_config_for_root) =
                    project_config_for_lookup_key(projects, &normalized_repo_root)
                {
                    return Some(project_config_for_root);
                }
            }
        }

        None
    }

    pub fn get_config_profile(
        &self,
        override_profile: Option<String>,
    ) -> Result<ConfigProfile, std::io::Error> {
        let profile = override_profile.or_else(|| self.profile.clone());

        match profile {
            Some(key) => {
                if let Some(profile) = self.profiles.get(key.as_str()) {
                    return Ok(profile.clone());
                }

                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("config profile `{key}` not found"),
                ))
            }
            None => Ok(ConfigProfile::default()),
        }
    }
}

/// Canonicalize the path and convert it to a string to be used as a key in the
/// projects trust map. On Windows, strips UNC, when possible, to try to ensure
/// that different paths that point to the same location have the same key.
fn normalized_project_lookup_keys(path: &Path) -> Vec<String> {
    let normalized_path = normalize_project_lookup_key(path.to_string_lossy().to_string());
    let normalized_canonical_path = normalize_project_lookup_key(
        normalize_for_path_comparison(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string(),
    );
    if normalized_path == normalized_canonical_path {
        vec![normalized_canonical_path]
    } else {
        vec![normalized_canonical_path, normalized_path]
    }
}

fn normalize_project_lookup_key(key: String) -> String {
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn project_config_for_lookup_key(
    projects: &HashMap<String, ProjectConfig>,
    lookup_key: &str,
) -> Option<ProjectConfig> {
    if let Some(project_config) = projects.get(lookup_key) {
        return Some(project_config.clone());
    }

    let mut normalized_matches: Vec<_> = projects
        .iter()
        .filter(|(key, _)| normalize_project_lookup_key((*key).clone()) == lookup_key)
        .collect();
    normalized_matches.sort_by(|(left, _), (right, _)| left.cmp(right));
    normalized_matches
        .first()
        .map(|(_, project_config)| (**project_config).clone())
}

pub fn validate_reserved_model_provider_ids(
    model_providers: &HashMap<String, ModelProviderInfo>,
) -> Result<(), String> {
    let mut conflicts = model_providers
        .keys()
        .filter(|key| {
            key.as_str() != AMAZON_BEDROCK_PROVIDER_ID
                && RESERVED_MODEL_PROVIDER_IDS.contains(&key.as_str())
        })
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>();
    conflicts.sort_unstable();
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "model_providers contains reserved built-in provider IDs: {}. \
Built-in providers cannot be overridden. Rename your custom provider (for example, `openai-custom`).",
            conflicts.join(", ")
        ))
    }
}

pub fn validate_model_providers(
    model_providers: &HashMap<String, ModelProviderInfo>,
) -> Result<(), String> {
    validate_reserved_model_provider_ids(model_providers)?;
    for (key, provider) in model_providers {
        if key == AMAZON_BEDROCK_PROVIDER_ID {
            continue;
        }
        if provider.aws.is_some() {
            return Err(format!(
                "model_providers.{key}: provider aws is only supported for `{AMAZON_BEDROCK_PROVIDER_ID}`"
            ));
        }
        if provider.name.trim().is_empty() {
            return Err(format!(
                "model_providers.{key}: provider name must not be empty"
            ));
        }
        provider
            .validate()
            .map_err(|message| format!("model_providers.{key}: {message}"))?;
    }
    Ok(())
}

fn deserialize_model_providers<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, ModelProviderInfo>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let model_providers = HashMap::<String, ModelProviderInfo>::deserialize(deserializer)?;
    validate_model_providers(&model_providers).map_err(serde::de::Error::custom)?;
    Ok(model_providers)
}

pub fn validate_account_pool(account_pool: &AccountPoolToml) -> Result<(), String> {
    if !account_pool.enabled {
        return Ok(());
    }

    if account_pool.pools.is_empty() {
        return Err("account_pool: enabled account pool must define at least one pool".to_string());
    }

    if let Some(default_pool) = account_pool.default_pool.as_deref()
        && !account_pool.pools.contains_key(default_pool)
    {
        return Err(format!(
            "account_pool.default_pool `{default_pool}` does not reference a configured pool"
        ));
    }

    for (pool_id, pool) in &account_pool.pools {
        if pool.provider != OPENAI_PROVIDER_ID {
            return Err(format!(
                "account_pool.pools.{pool_id}: provider must be `{OPENAI_PROVIDER_ID}`"
            ));
        }
        if pool.accounts.is_empty() {
            return Err(format!(
                "account_pool.pools.{pool_id}: accounts must contain at least one account id"
            ));
        }
        for account_id in &pool.accounts {
            if account_id.trim().is_empty() {
                return Err(format!(
                    "account_pool.pools.{pool_id}: account ids must not be empty"
                ));
            }
            if !is_safe_account_id(account_id) {
                return Err(format!(
                    "account_pool.pools.{pool_id}: account id `{account_id}` must not contain path separators or parent directory components"
                ));
            }
        }
    }

    Ok(())
}

fn is_safe_account_id(account_id: &str) -> bool {
    account_id != "."
        && account_id != ".."
        && !account_id.contains('/')
        && !account_id.contains('\\')
}

fn deserialize_account_pool<'de, D>(deserializer: D) -> Result<Option<AccountPoolToml>, D::Error>
where
    D: Deserializer<'de>,
{
    let account_pool = Option::<AccountPoolToml>::deserialize(deserializer)?;
    if let Some(account_pool) = account_pool.as_ref() {
        validate_account_pool(account_pool).map_err(serde::de::Error::custom)?;
    }
    Ok(account_pool)
}

pub fn validate_model_policy(model_policy: &ModelPolicyToml) -> Result<(), String> {
    if !model_policy.enabled {
        return Ok(());
    }

    if model_policy.rules.is_empty() && model_policy.default_route.is_none() {
        return Err(
            "model_policy: enabled model policy must define rules or default_route".to_string(),
        );
    }
    if let Some(route) = &model_policy.default_route {
        validate_model_policy_route("model_policy.default_route", route)?;
    }
    for (index, rule) in model_policy.rules.iter().enumerate() {
        let label = format!("model_policy.rules[{index}]");
        if let Some(sources) = &rule.source
            && sources.is_empty()
        {
            return Err(format!("{label}: source must not be an empty list"));
        }
        if let Some(sources) = &rule.source
            && sources.iter().any(|source| source.trim().is_empty())
        {
            return Err(format!("{label}: source entries must not be empty"));
        }
        if let (Some(min), Some(max)) = (rule.min_prompt_bytes, rule.max_prompt_bytes)
            && min > max
        {
            return Err(format!(
                "{label}: min_prompt_bytes must be <= max_prompt_bytes"
            ));
        }
        if rule.source.is_none()
            && rule.min_prompt_bytes.is_none()
            && rule.max_prompt_bytes.is_none()
        {
            return Err(format!(
                "{label}: rule must specify source or a prompt-size bound"
            ));
        }
        validate_model_policy_route(&label, &rule.route)?;
    }

    Ok(())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrManyStrings {
    One(String),
    Many(Vec<String>),
}

fn deserialize_model_policy_sources<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let source = Option::<OneOrManyStrings>::deserialize(deserializer)?;
    Ok(source.map(|source| match source {
        OneOrManyStrings::One(source) => vec![source],
        OneOrManyStrings::Many(sources) => sources,
    }))
}

fn validate_model_policy_route(label: &str, route: &ModelPolicyRouteToml) -> Result<(), String> {
    if route.model.is_none()
        && route.model_provider.is_none()
        && route.service_tier.is_none()
        && route.reasoning_effort.is_none()
        && route.account_pool.is_none()
        && route.account.is_none()
    {
        return Err(format!("{label}: route must set at least one target field"));
    }
    if route.account_pool.is_some() && route.account.is_some() {
        return Err(format!(
            "{label}: account_pool and account are mutually exclusive"
        ));
    }
    if let Some(account) = &route.account
        && (account.trim().is_empty() || !is_safe_account_id(account))
    {
        return Err(format!(
            "{label}: account must not be empty or contain path separators or parent directory components"
        ));
    }
    if let Some(account_pool) = &route.account_pool
        && account_pool.trim().is_empty()
    {
        return Err(format!("{label}: account_pool must not be empty"));
    }
    Ok(())
}

fn deserialize_model_policy<'de, D>(deserializer: D) -> Result<Option<ModelPolicyToml>, D::Error>
where
    D: Deserializer<'de>,
{
    let model_policy = Option::<ModelPolicyToml>::deserialize(deserializer)?;
    if let Some(model_policy) = model_policy.as_ref() {
        validate_model_policy(model_policy).map_err(serde::de::Error::custom)?;
    }
    Ok(model_policy)
}

pub fn validate_oss_provider(provider: &str) -> std::io::Result<()> {
    match provider {
        LMSTUDIO_OSS_PROVIDER_ID | OLLAMA_OSS_PROVIDER_ID => Ok(()),
        LEGACY_OLLAMA_CHAT_PROVIDER_ID => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            OLLAMA_CHAT_PROVIDER_REMOVED_ERROR,
        )),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Invalid OSS provider '{provider}'. Must be one of: {LMSTUDIO_OSS_PROVIDER_ID}, {OLLAMA_OSS_PROVIDER_ID}"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parses_account_pool_config() {
        let config: ConfigToml = toml::from_str(
            r#"
            [account_pool]
            enabled = true
            default_pool = "codex-pro"

            [account_pool.pools.codex-pro]
            provider = "openai"
            policy = "load_balance"
            accounts = ["work-pro", "personal-pro"]
            "#,
        )
        .expect("config should parse");

        let account_pool = config.account_pool.expect("account pool");
        assert_eq!(account_pool.enabled, true);
        assert_eq!(account_pool.default_pool.as_deref(), Some("codex-pro"));
        assert_eq!(
            account_pool.pools.get("codex-pro").expect("pool").policy,
            AccountPoolPolicyToml::LoadBalance
        );
    }

    #[test]
    fn parses_model_policy_config() {
        let config: ConfigToml = toml::from_str(
            r#"
            [model_policy]
            enabled = true

            [[model_policy.rules]]
            source = ["subagent", "module.repo_ci"]
            max_prompt_bytes = 20000
            model = "gpt-5.3-codex-spark"
            service_tier = "flex"
            reasoning_effort = "inherit"
            account = "spark-account"

            [model_policy.default_route]
            account_pool = "codex-pro"
            "#,
        )
        .expect("config should parse");

        let model_policy = config.model_policy.expect("model policy");
        assert_eq!(model_policy.enabled, true);
        assert_eq!(model_policy.rules.len(), 1);
        let rule = model_policy.rules.first().expect("rule");
        assert_eq!(
            rule.source.as_deref(),
            Some(["subagent".to_string(), "module.repo_ci".to_string()].as_slice())
        );
        assert_eq!(rule.max_prompt_bytes, Some(20000));
        assert_eq!(rule.route.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert_eq!(rule.route.service_tier, Some(ServiceTier::Flex));
        assert_eq!(
            rule.route.reasoning_effort,
            Some(ModelPolicyReasoningEffortToml::Inherit)
        );
        assert_eq!(rule.route.account.as_deref(), Some("spark-account"));
        assert_eq!(
            model_policy
                .default_route
                .as_ref()
                .and_then(|route| route.account_pool.as_deref()),
            Some("codex-pro")
        );
    }

    #[test]
    fn rejects_model_policy_with_ambiguous_account_target() {
        let err = toml::from_str::<ConfigToml>(
            r#"
            [model_policy]
            enabled = true

            [[model_policy.rules]]
            source = "subagent"
            model = "gpt-5.3-codex-spark"
            account_pool = "codex-pro"
            account = "work-pro"
            "#,
        )
        .expect_err("ambiguous account target should be rejected");

        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn rejects_repo_ci_models_config() {
        let err = toml::from_str::<ConfigToml>(
            r#"
            [repo_ci.defaults]
            enabled = true
            models = [{ inherit = true }]
            "#,
        )
        .expect_err("repo_ci.models should be rejected");

        assert!(err.to_string().contains("unknown field `models`"));
    }

    #[test]
    fn rejects_account_pool_default_that_does_not_exist() {
        let err = toml::from_str::<ConfigToml>(
            r#"
            [account_pool]
            enabled = true
            default_pool = "missing"

            [account_pool.pools.codex-pro]
            provider = "openai"
            policy = "drain"
            accounts = ["work-pro"]
            "#,
        )
        .expect_err("missing default pool should be rejected");

        assert!(err.to_string().contains("account_pool.default_pool"));
    }

    #[test]
    fn rejects_non_openai_account_pool_provider() {
        let err = toml::from_str::<ConfigToml>(
            r#"
            [account_pool]
            enabled = true
            default_pool = "codex-pro"

            [account_pool.pools.codex-pro]
            provider = "openrouter"
            policy = "drain"
            accounts = ["work-pro"]
            "#,
        )
        .expect_err("non-openai provider should be rejected");

        assert!(err.to_string().contains("provider must be `openai`"));
    }

    #[test]
    fn rejects_unsafe_account_pool_account_ids() {
        for account_id in ["", " ", ".", "..", "../work", "team/work", "team\\work"] {
            let toml_account_id = account_id.replace('\\', "\\\\");
            let config = format!(
                r#"
                [account_pool]
                enabled = true

                [account_pool.pools.codex-pro]
                provider = "openai"
                policy = "drain"
                accounts = ["{toml_account_id}"]
                "#
            );
            let err = toml::from_str::<ConfigToml>(&config)
                .expect_err("unsafe account id should be rejected");
            let message = err.to_string();
            assert!(
                message.contains("account ids must not be empty")
                    || message.contains("path separators"),
                "{message}"
            );
        }
    }

    #[test]
    fn parses_repo_ci_github_repo_scope_and_empty_issue_types() {
        let config: ConfigToml = toml::from_str(
            r#"
            [repo_ci.github_repos."openai/codex"]
            enabled = true
            long_ci = true
            review_issue_types = []
            max_review_fix_rounds = 4
            "#,
        )
        .expect("config should parse");

        let repo_ci = config.repo_ci.expect("repo_ci");
        let scope = repo_ci
            .github_repos
            .get("openai/codex")
            .expect("github repo scope");
        assert_eq!(scope.enabled, Some(true));
        assert_eq!(scope.long_ci, Some(true));
        assert_eq!(scope.review_issue_types, Some(Vec::new()));
        assert_eq!(scope.max_review_fix_rounds, Some(4));
    }
}
