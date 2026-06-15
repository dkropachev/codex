//! Schema-heavy configuration TOML types used by Codex.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

use crate::HooksToml;
use crate::permissions_toml::PermissionsToml;
use crate::profile_toml::ConfigProfile;
use crate::types::AnalyticsConfigToml;
use crate::types::ApprovalsReviewer;
use crate::types::AppsConfigToml;
use crate::types::ArtifactStyle;
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
use crate::types::ResponseStyle;
use crate::types::SandboxWorkspaceWrite;
use crate::types::ShellEnvironmentPolicyToml;
use crate::types::SkillsConfig;
use crate::types::ToolSuggestConfig;
use crate::types::Tui;
use crate::types::UriBasedFileOpener;
use crate::types::WindowsToml;
use codex_app_server_protocol::ForcedChatgptWorkspaceIds as ApiForcedChatgptWorkspaceIds;
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
use codex_protocol::config_types::AutoCompactTokenLimitScope;
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
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path::normalize_for_path_comparison;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as SerdeError;
use serde_json::Value as JsonValue;

const RESERVED_MODEL_PROVIDER_IDS: [&str; 4] = [
    AMAZON_BEDROCK_PROVIDER_ID,
    OPENAI_PROVIDER_ID,
    OLLAMA_OSS_PROVIDER_ID,
    LMSTUDIO_OSS_PROVIDER_ID,
];

pub const DEFAULT_PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;

const fn default_allow_login_shell() -> Option<bool> {
    Some(true)
}

fn default_history() -> Option<History> {
    Some(History::default())
}

const fn default_project_doc_max_bytes() -> Option<usize> {
    Some(DEFAULT_PROJECT_DOC_MAX_BYTES)
}

fn default_project_doc_fallback_filenames() -> Option<Vec<String>> {
    Some(Vec::new())
}

const fn default_hide_agent_reasoning() -> Option<bool> {
    Some(false)
}

const fn default_true() -> bool {
    true
}

/// Backward-compatible shape for ChatGPT workspace login restrictions in config.toml.
#[derive(Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum ForcedChatgptWorkspaceIds {
    Single(String),
    Multiple(Vec<String>),
}

impl ForcedChatgptWorkspaceIds {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::Single(value) => vec![value],
            Self::Multiple(values) => values,
        }
    }

    pub fn into_api(self) -> ApiForcedChatgptWorkspaceIds {
        match self {
            Self::Single(value) => ApiForcedChatgptWorkspaceIds::Single(value),
            Self::Multiple(values) => ApiForcedChatgptWorkspaceIds::Multiple(values),
        }
    }
}

impl<'de> Deserialize<'de> for ForcedChatgptWorkspaceIds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Single(String),
            Multiple(Vec<String>),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Single(value) if value.contains(',') => Err(D::Error::custom(
                "forced_chatgpt_workspace_id must be a single workspace ID string or a TOML list \
of strings; comma-separated strings are not supported. Use \
`forced_chatgpt_workspace_id = [\"123e4567-e89b-42d3-a456-426614174000\", \
\"123e4567-e89b-42d3-a456-426614174001\"]` instead.",
            )),
            Repr::Single(value) => Ok(Self::Single(value)),
            Repr::Multiple(values) => Ok(Self::Multiple(values)),
        }
    }
}

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

    /// Controls whether the auto-compaction limit applies to the full context or
    /// only to tokens after the carried prefix in the current compaction window.
    pub model_auto_compact_token_limit_scope: Option<AutoCompactTokenLimitScope>,

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
    #[serde(default = "default_allow_login_shell")]
    pub allow_login_shell: Option<bool>,

    /// Sandbox mode to use.
    pub sandbox_mode: Option<SandboxMode>,

    /// Sandbox configuration to apply if `sandbox` is `WorkspaceWrite`.
    pub sandbox_workspace_write: Option<SandboxWorkspaceWrite>,

    /// Default permissions profile to apply. Names starting with `:` refer to
    /// built-in profiles; other names are resolved from the `[permissions]`
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

    /// Whether to inject the `<collaboration_mode>` developer block.
    pub include_collaboration_mode_instructions: Option<bool>,

    /// Whether to inject the `<environment_context>` user block.
    pub include_environment_context: Option<bool>,

    /// Optional path to a file containing model instructions that will override
    /// the built-in instructions for the selected model. Users are STRONGLY
    /// DISCOURAGED from using this field, as deviating from the instructions
    /// sanctioned by Codex will likely degrade model performance.
    pub model_instructions_file: Option<AbsolutePathBuf>,

    /// Compact prompt used for history compaction.
    pub compact_prompt: Option<String>,

    /// When set, restricts ChatGPT login to one or more workspace identifiers.
    #[serde(default)]
    pub forced_chatgpt_workspace_id: Option<ForcedChatgptWorkspaceIds>,

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

    /// Optional logical pools of ChatGPT accounts.
    #[serde(default, deserialize_with = "deserialize_account_pool")]
    pub account_pool: Option<AccountPoolToml>,

    /// Optional routing policy for internal model calls.
    #[serde(default, deserialize_with = "deserialize_model_policy")]
    pub model_policy: Option<ModelPolicyToml>,

    /// Optional adaptive router for internal model calls.
    #[serde(default, deserialize_with = "deserialize_model_router")]
    pub model_router: Option<ModelRouterToml>,

    /// Maximum number of bytes to include from an AGENTS.md project doc file.
    #[serde(default = "default_project_doc_max_bytes")]
    pub project_doc_max_bytes: Option<usize>,

    /// Ordered list of fallback filenames to look for when AGENTS.md is missing.
    #[serde(default = "default_project_doc_fallback_filenames")]
    pub project_doc_fallback_filenames: Option<Vec<String>>,

    /// Token budget applied when storing tool/function outputs in the context manager.
    pub tool_output_token_limit: Option<usize>,

    /// Maximum poll window for background terminal output (`write_stdin`), in milliseconds.
    /// Default: `300000` (5 minutes).
    pub background_terminal_max_timeout: Option<u64>,

    /// Deprecated: ignored.
    #[schemars(skip)]
    pub js_repl_node_path: Option<AbsolutePathBuf>,

    /// Deprecated: ignored.
    #[schemars(skip)]
    pub js_repl_node_module_dirs: Option<Vec<AbsolutePathBuf>>,

    /// Profile to use from the `profiles` map.
    pub profile: Option<String>,

    /// Named profiles to facilitate switching between different configurations.
    #[serde(default)]
    pub profiles: HashMap<String, ConfigProfile>,

    /// Settings that govern if and what will be written to `~/.codex/history.jsonl`.
    #[serde(default = "default_history")]
    pub history: Option<History>,

    /// Directory where Codex stores the SQLite state DB.
    /// Defaults to `$CODEX_SQLITE_HOME` when set. Otherwise uses `$CODEX_HOME`.
    pub sqlite_home: Option<AbsolutePathBuf>,

    /// Directory where Codex writes log files. Setting this value explicitly
    /// also enables the TUI text log in this directory.
    /// Defaults to `$CODEX_HOME/log`.
    pub log_dir: Option<AbsolutePathBuf>,

    /// Debugging and reproducibility settings.
    pub debug: Option<DebugToml>,

    /// Optional URI-based file opener. If set, citations to files in the model
    /// output will be hyperlinked using the specified URI scheme.
    pub file_opener: Option<UriBasedFileOpener>,

    /// Collection of settings that are specific to the TUI.
    pub tui: Option<Tui>,

    /// When set to `true`, `AgentReasoning` events will be hidden from the
    /// UI/output. Defaults to `false`.
    #[serde(default = "default_hide_agent_reasoning")]
    pub hide_agent_reasoning: Option<bool>,

    /// When set to `true`, `AgentReasoningRawContentEvent` events will be shown in the UI/output.
    /// Defaults to `false`.
    pub show_raw_agent_reasoning: Option<bool>,

    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    /// Optional verbosity control for GPT-5 models (Responses API `text.verbosity`).
    pub model_verbosity: Option<Verbosity>,
    /// Preferred response length for ordinary Codex chat and status messages.
    pub response_style: Option<ResponseStyle>,
    /// Whether artifact-like generated text should follow `response_style`.
    pub artifact_style: Option<ArtifactStyle>,

    /// Override to force-enable reasoning summaries for the configured model.
    pub model_supports_reasoning_summaries: Option<bool>,

    /// Optional path to a JSON model catalog (applied on startup only).
    /// Per-thread `config` overrides are accepted but do not reapply this (no-ops).
    pub model_catalog_json: Option<AbsolutePathBuf>,

    /// Optionally specify a personality for the model
    pub personality: Option<Personality>,

    /// Optional explicit service tier request id for new turns (for example
    /// `default`, `priority`, or `flex`; legacy `fast` also works).
    pub service_tier: Option<String>,

    /// Base URL for requests to ChatGPT (as opposed to the OpenAI API).
    pub chatgpt_base_url: Option<String>,

    /// Optional product SKU forwarded on host-owned Codex Apps MCP requests.
    pub apps_mcp_product_sku: Option<String>,

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

    /// Experimental / do not use. When set, app-server fetches thread-scoped
    /// config from a remote service at this endpoint.
    pub experimental_thread_config_endpoint: Option<String>,

    /// Removed. Former remote thread-store endpoint setting kept only so we can
    /// fail fast instead of silently falling back to local persistence.
    #[schemars(skip)]
    pub experimental_thread_store_endpoint: Option<String>,

    /// Experimental / do not use. Selects the thread store implementation.
    pub experimental_thread_store: Option<ThreadStoreToml>,
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

    /// Lifecycle hooks configured inline in TOML plus user-level overrides.
    pub hooks: Option<HooksToml>,

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

    /// Suppress warnings about unstable (under development) features.
    pub suppress_unstable_features_warning: Option<bool>,

    /// Compatibility-only settings retained so legacy `ghost_snapshot`
    /// config still loads.
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

    /// Opaque desktop settings stored alongside the rest of config.toml.
    #[serde(default)]
    pub desktop: Option<HashMap<String, JsonValue>>,

    /// OTEL configuration.
    pub otel: Option<OtelConfigToml>,

    /// Windows-specific configuration.
    #[serde(default)]
    pub windows: Option<WindowsToml>,

    /// Collection of in-product notices (different from notifications)
    /// See [`crate::types::Notice`] for more details
    pub notice: Option<Notice>,

    pub experimental_compact_prompt_file: Option<AbsolutePathBuf>,
    pub experimental_use_unified_exec_tool: Option<bool>,
    /// Preferred OSS provider for local models, e.g. "lmstudio" or "ollama".
    pub oss_provider: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigLockfileToml {
    pub version: u32,
    pub codex_version: String,

    /// Replayable effective config captured in the lockfile.
    pub config: ConfigToml,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DebugToml {
    pub config_lockfile: Option<DebugConfigLockToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DebugConfigLockToml {
    /// Directory where Codex writes effective session config lock files.
    pub export_dir: Option<AbsolutePathBuf>,

    /// Lockfile to replay as the authoritative effective config.
    pub load_path: Option<AbsolutePathBuf>,

    /// Allow replaying a lock generated by a different Codex version.
    pub allow_codex_version_mismatch: Option<bool>,

    /// Save fields resolved from the model catalog/session configuration.
    pub save_fields_resolved_from_model_catalog: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadStoreToml {
    Local {},
    #[schemars(skip)]
    InMemory {
        id: String,
    },
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

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterToml {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub discovery: Option<ModelRouterDiscoveryToml>,

    #[serde(default)]
    pub subscription_pricing: Option<ModelRouterSubscriptionPricingToml>,

    #[serde(default)]
    pub savings_reference: Option<ModelRouterSavingsReferenceToml>,

    #[serde(default)]
    pub candidates: Vec<ModelRouterCandidateToml>,

    #[serde(default)]
    pub models: Option<ModelRouterModelsToml>,

    #[serde(default)]
    pub bias: Option<ModelRouterBiasToml>,

    #[serde(default)]
    pub lifecycle: Option<ModelRouterLifecycleToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterDiscoveryToml {
    #[default]
    Curated,
    Manual,
    FromRules,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterModelsToml {
    #[serde(default)]
    pub rules: Vec<ModelRouterModelRuleToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterModelRuleToml {
    pub id: Option<String>,

    #[serde(rename = "type")]
    pub rule_type: ModelRouterModelRuleTypeToml,

    #[serde(default)]
    pub tasks: Vec<String>,

    #[serde(default)]
    pub except_tasks: Vec<String>,

    #[serde(default)]
    pub models: Vec<ModelRouterModelSelectorToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterModelRuleTypeToml {
    Require,
    Exclude,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterModelSelectorToml {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterBiasToml {
    #[serde(default)]
    pub rules: Vec<ModelRouterBiasRuleToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterBiasRuleToml {
    pub id: Option<String>,

    #[serde(default)]
    pub tasks: Vec<String>,

    #[serde(default)]
    pub except_tasks: Vec<String>,

    #[serde(default)]
    pub models: Vec<ModelRouterModelSelectorToml>,

    pub score_bias: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterLifecycleToml {
    #[serde(default)]
    pub defaults: Option<ModelRouterLifecycleDefaultsToml>,

    #[serde(default)]
    pub rules: Vec<ModelRouterLifecycleRuleToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterLifecycleDefaultsToml {
    pub window: Option<String>,
    pub cost_budget_usd: Option<f64>,
    pub token_budget: Option<u64>,
    pub min_evaluated: Option<u64>,
    pub min_confidence: Option<f64>,
    pub min_success_rate: Option<f64>,
    pub shadow_allowed: Option<bool>,
    pub promotion_shadow_sample_rate_limit: Option<f64>,
    pub monitoring_shadow_sample_rate_limit: Option<f64>,
    pub auto_promote: Option<bool>,
    pub auto_demote: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterLifecycleRuleToml {
    pub id: String,

    #[serde(default)]
    pub tasks: Vec<String>,

    #[serde(default)]
    pub except_tasks: Vec<String>,

    #[serde(default)]
    pub models: Vec<ModelRouterModelSelectorToml>,

    pub window: Option<String>,
    pub cost_budget_usd: Option<f64>,
    pub token_budget: Option<u64>,
    pub min_evaluated: Option<u64>,
    pub min_confidence: Option<f64>,
    pub min_success_rate: Option<f64>,
    pub shadow_allowed: Option<bool>,
    pub promotion_shadow_sample_rate_limit: Option<f64>,
    pub monitoring_shadow_sample_rate_limit: Option<f64>,
    pub auto_promote: Option<bool>,
    pub auto_demote: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterSubscriptionPricingToml {
    #[default]
    AmortizedScarce,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterSavingsReferenceToml {
    #[default]
    ImplicitIncumbent,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelRouterCandidateToml {
    pub id: Option<String>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub service_tier: Option<ServiceTier>,
    pub reasoning_effort: Option<ModelRouterReasoningEffortToml>,
    pub account_pool: Option<String>,
    pub account: Option<String>,
    pub intelligence_score: Option<f64>,
    pub success_rate: Option<f64>,
    pub median_latency_ms: Option<u64>,
    pub input_price_per_million: Option<f64>,
    pub cached_input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    pub reasoning_output_price_per_million: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelRouterReasoningEffortToml {
    /// Preserve the reasoning effort already selected by the parent/default config.
    Inherit,
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

impl ModelRouterReasoningEffortToml {
    pub fn as_reasoning_effort(self) -> Option<ReasoningEffort> {
        match self {
            Self::Inherit => None,
            Self::None => Some(ReasoningEffort::None),
            Self::Minimal => Some(ReasoningEffort::Minimal),
            Self::Low => Some(ReasoningEffort::Low),
            Self::Medium => Some(ReasoningEffort::Medium),
            Self::High => Some(ReasoningEffort::High),
            Self::XHigh => Some(ReasoningEffort::XHigh),
        }
    }
}

impl From<ConfigToml> for UserSavedConfig {
    fn from(config_toml: ConfigToml) -> Self {
        Self {
            approval_policy: config_toml.approval_policy,
            sandbox_mode: config_toml.sandbox_mode,
            sandbox_settings: config_toml.sandbox_workspace_write.map(From::from),
            forced_chatgpt_workspace_id: config_toml
                .forced_chatgpt_workspace_id
                .map(ForcedChatgptWorkspaceIds::into_api),
            forced_login_method: config_toml.forced_login_method,
            model: config_toml.model,
            model_reasoning_effort: config_toml.model_reasoning_effort,
            model_reasoning_summary: config_toml.model_reasoning_summary,
            model_verbosity: config_toml.model_verbosity,
            tools: config_toml.tools.map(From::from),
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
    pub experimental_request_user_input: Option<ExperimentalRequestUserInput>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ExperimentalRequestUserInput {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl From<ToolsToml> for Tools {
    fn from(tools_toml: ToolsToml) -> Self {
        Self {
            web_search: tools_toml.web_search.is_some().then_some(true),
        }
    }
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
    /// Whether to record a model-visible message when an agent turn is interrupted.
    /// Defaults to true.
    pub interrupt_message: Option<bool>,

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

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GhostSnapshotToml {
    /// Legacy no-op setting retained for compatibility.
    #[serde(alias = "ignore_untracked_files_over_bytes")]
    pub ignore_large_untracked_files: Option<i64>,
    /// Legacy no-op setting retained for compatibility.
    #[serde(alias = "large_untracked_dir_warning_threshold")]
    pub ignore_large_untracked_dirs: Option<i64>,
    /// Legacy no-op setting retained for compatibility.
    pub disable_warnings: Option<bool>,
}

impl ConfigToml {
    /// Derive the effective permission profile from legacy sandbox config.
    ///
    /// Call this only after ruling out `default_permissions`: named
    /// `[permissions]` profiles must be compiled through the permissions
    /// profile pipeline, not reconstructed from `sandbox_mode`.
    pub async fn derive_permission_profile(
        &self,
        sandbox_mode_override: Option<SandboxMode>,
        windows_sandbox_level: WindowsSandboxLevel,
        active_project: Option<&ProjectConfig>,
        permission_profile_constraint: Option<&crate::Constrained<PermissionProfile>>,
    ) -> PermissionProfile {
        let configured_sandbox_mode = sandbox_mode_override.or(self.sandbox_mode);
        let resolved_sandbox_mode = configured_sandbox_mode
            .or_else(|| {
                // If no sandbox_mode is set but this directory has a trust decision,
                // default to workspace-write except on unsandboxed Windows where we
                // default to read-only.
                active_project
                    .filter(|project| project.is_trusted() || project.is_untrusted())
                    .map(|_| {
                        if cfg!(target_os = "windows")
                            && windows_sandbox_level == WindowsSandboxLevel::Disabled
                        {
                            SandboxMode::ReadOnly
                        } else {
                            SandboxMode::WorkspaceWrite
                        }
                    })
            })
            .unwrap_or_default();
        let effective_sandbox_mode = if cfg!(target_os = "windows")
            // If the experimental Windows sandbox is enabled, do not force a downgrade.
            && windows_sandbox_level == WindowsSandboxLevel::Disabled
            && matches!(resolved_sandbox_mode, SandboxMode::WorkspaceWrite)
        {
            SandboxMode::ReadOnly
        } else {
            resolved_sandbox_mode
        };

        let permission_profile = match effective_sandbox_mode {
            SandboxMode::ReadOnly => PermissionProfile::read_only(),
            SandboxMode::WorkspaceWrite => match self.sandbox_workspace_write.as_ref() {
                Some(SandboxWorkspaceWrite {
                    writable_roots,
                    network_access,
                    exclude_tmpdir_env_var,
                    exclude_slash_tmp,
                }) => {
                    let network_policy = if *network_access {
                        NetworkSandboxPolicy::Enabled
                    } else {
                        NetworkSandboxPolicy::Restricted
                    };
                    PermissionProfile::workspace_write_with(
                        writable_roots,
                        network_policy,
                        *exclude_tmpdir_env_var,
                        *exclude_slash_tmp,
                    )
                }
                None => PermissionProfile::workspace_write(),
            },
            SandboxMode::DangerFullAccess => PermissionProfile::Disabled,
        };
        if configured_sandbox_mode.is_none()
            && let Some(constraint) = permission_profile_constraint
            && let Err(err) = constraint.can_set(&permission_profile)
        {
            tracing::warn!(
                error = %err,
                "default sandbox policy is disallowed by requirements; falling back to required default"
            );
            PermissionProfile::read_only()
        } else {
            permission_profile
        }
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
    normalized_matches.sort_by_key(|(key, _)| *key);
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
        if let Some(sources) = &rule.source {
            for source in sources {
                if source.trim().is_empty() {
                    return Err(format!("{label}: source entries must not be empty"));
                }
            }
        }
        if let (Some(min), Some(max)) = (rule.min_prompt_bytes, rule.max_prompt_bytes)
            && min > max
        {
            return Err(format!(
                "{label}: min_prompt_bytes must be less than or equal to max_prompt_bytes"
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
    let Some(value) = Option::<OneOrManyStrings>::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(Some(match value {
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

pub fn validate_model_router(model_router: &ModelRouterToml) -> Result<(), String> {
    if !model_router.enabled {
        return Ok(());
    }

    for (index, candidate) in model_router.candidates.iter().enumerate() {
        let label = format!("model_router.candidates[{index}]");
        validate_model_router_candidate(&label, candidate)?;
    }

    if let Some(models) = model_router.models.as_ref() {
        validate_model_router_model_rules(models)?;
    }
    if let Some(bias) = model_router.bias.as_ref() {
        validate_model_router_bias_rules(bias)?;
    }
    if let Some(lifecycle) = model_router.lifecycle.as_ref() {
        validate_model_router_lifecycle(lifecycle)?;
    }

    Ok(())
}

fn validate_model_router_model_rules(models: &ModelRouterModelsToml) -> Result<(), String> {
    for (index, rule) in models.rules.iter().enumerate() {
        let label = format!("model_router.models.rules[{index}]");
        validate_optional_rule_id(&label, rule.id.as_deref())?;
        validate_selector_list(&label, "tasks", &rule.tasks)?;
        validate_selector_list(&label, "except_tasks", &rule.except_tasks)?;
        validate_model_selector_list(&label, &rule.models, /*require_non_empty*/ true)?;
    }
    Ok(())
}

fn validate_model_router_bias_rules(bias: &ModelRouterBiasToml) -> Result<(), String> {
    for (index, rule) in bias.rules.iter().enumerate() {
        let label = format!("model_router.bias.rules[{index}]");
        validate_optional_rule_id(&label, rule.id.as_deref())?;
        validate_selector_list(&label, "tasks", &rule.tasks)?;
        validate_selector_list(&label, "except_tasks", &rule.except_tasks)?;
        validate_model_selector_list(&label, &rule.models, /*require_non_empty*/ true)?;
        if !rule.score_bias.is_finite() || !(-1.0..=1.0).contains(&rule.score_bias) {
            return Err(format!(
                "{label}: score_bias must be a finite value between -1.0 and 1.0"
            ));
        }
    }
    Ok(())
}

fn validate_model_router_lifecycle(lifecycle: &ModelRouterLifecycleToml) -> Result<(), String> {
    if let Some(defaults) = lifecycle.defaults.as_ref() {
        validate_lifecycle_values("model_router.lifecycle.defaults", defaults)?;
    }
    for (index, rule) in lifecycle.rules.iter().enumerate() {
        let label = format!("model_router.lifecycle.rules[{index}]");
        if rule.id.trim().is_empty() {
            return Err(format!("{label}: id must not be empty"));
        }
        validate_selector_list(&label, "tasks", &rule.tasks)?;
        validate_selector_list(&label, "except_tasks", &rule.except_tasks)?;
        validate_model_selector_list(&label, &rule.models, /*require_non_empty*/ false)?;
        validate_lifecycle_values(&label, rule)?;
    }
    Ok(())
}

fn validate_optional_rule_id(label: &str, id: Option<&str>) -> Result<(), String> {
    if let Some(id) = id
        && id.trim().is_empty()
    {
        return Err(format!("{label}: id must not be empty"));
    }
    Ok(())
}

fn validate_selector_list(label: &str, field: &str, selectors: &[String]) -> Result<(), String> {
    for (index, selector) in selectors.iter().enumerate() {
        validate_exact_or_regex(&format!("{label}.{field}[{index}]"), selector)?;
    }
    Ok(())
}

fn validate_model_selector_list(
    label: &str,
    selectors: &[ModelRouterModelSelectorToml],
    require_non_empty: bool,
) -> Result<(), String> {
    if require_non_empty && selectors.is_empty() {
        return Err(format!("{label}: models must not be empty"));
    }
    for (index, selector) in selectors.iter().enumerate() {
        let label = format!("{label}.models[{index}]");
        if selector.provider.is_none() && selector.model.is_none() {
            return Err(format!(
                "{label}: at least one of provider or model must be set"
            ));
        }
        validate_optional_exact_or_regex(&label, "provider", selector.provider.as_deref())?;
        validate_optional_exact_or_regex(&label, "model", selector.model.as_deref())?;
    }
    Ok(())
}

fn validate_optional_exact_or_regex(
    label: &str,
    field: &str,
    value: Option<&str>,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_exact_or_regex(&format!("{label}.{field}"), value)?;
    }
    Ok(())
}

fn validate_exact_or_regex(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label}: selector must not be empty"));
    }
    if let Some(pattern) = regex_selector_pattern(value) {
        if pattern.is_empty() {
            return Err(format!("{label}: regex selector must not be empty"));
        }
        regex::Regex::new(pattern).map_err(|err| format!("{label}: invalid regex: {err}"))?;
    }
    Ok(())
}

fn regex_selector_pattern(value: &str) -> Option<&str> {
    (value.len() >= 2 && value.starts_with('/') && value.ends_with('/'))
        .then_some(&value[1..value.len() - 1])
}

/// Shared view over lifecycle defaults and rule overrides for validation.
trait ModelRouterLifecycleValues {
    fn window(&self) -> Option<&str>;
    fn cost_budget_usd(&self) -> Option<f64>;
    fn min_confidence(&self) -> Option<f64>;
    fn min_success_rate(&self) -> Option<f64>;
    fn promotion_shadow_sample_rate_limit(&self) -> Option<f64>;
    fn monitoring_shadow_sample_rate_limit(&self) -> Option<f64>;
}

impl ModelRouterLifecycleValues for ModelRouterLifecycleDefaultsToml {
    fn window(&self) -> Option<&str> {
        self.window.as_deref()
    }

    fn cost_budget_usd(&self) -> Option<f64> {
        self.cost_budget_usd
    }

    fn min_confidence(&self) -> Option<f64> {
        self.min_confidence
    }

    fn min_success_rate(&self) -> Option<f64> {
        self.min_success_rate
    }

    fn promotion_shadow_sample_rate_limit(&self) -> Option<f64> {
        self.promotion_shadow_sample_rate_limit
    }

    fn monitoring_shadow_sample_rate_limit(&self) -> Option<f64> {
        self.monitoring_shadow_sample_rate_limit
    }
}

impl ModelRouterLifecycleValues for ModelRouterLifecycleRuleToml {
    fn window(&self) -> Option<&str> {
        self.window.as_deref()
    }

    fn cost_budget_usd(&self) -> Option<f64> {
        self.cost_budget_usd
    }

    fn min_confidence(&self) -> Option<f64> {
        self.min_confidence
    }

    fn min_success_rate(&self) -> Option<f64> {
        self.min_success_rate
    }

    fn promotion_shadow_sample_rate_limit(&self) -> Option<f64> {
        self.promotion_shadow_sample_rate_limit
    }

    fn monitoring_shadow_sample_rate_limit(&self) -> Option<f64> {
        self.monitoring_shadow_sample_rate_limit
    }
}

fn validate_lifecycle_values(
    label: &str,
    values: &impl ModelRouterLifecycleValues,
) -> Result<(), String> {
    if let Some(window) = values.window() {
        validate_model_router_window(label, window)?;
    }
    validate_non_negative(label, "cost_budget_usd", values.cost_budget_usd())?;
    validate_unit_interval(label, "min_confidence", values.min_confidence())?;
    validate_unit_interval(label, "min_success_rate", values.min_success_rate())?;
    validate_unit_interval(
        label,
        "promotion_shadow_sample_rate_limit",
        values.promotion_shadow_sample_rate_limit(),
    )?;
    validate_unit_interval(
        label,
        "monitoring_shadow_sample_rate_limit",
        values.monitoring_shadow_sample_rate_limit(),
    )?;
    Ok(())
}

fn validate_model_router_window(label: &str, window: &str) -> Result<(), String> {
    let window = window.trim();
    if window.eq_ignore_ascii_case("all") || window.eq_ignore_ascii_case("all-time") {
        return Ok(());
    }
    let (number, unit) = window.split_at(window.len().saturating_sub(1));
    let valid_unit = matches!(unit, "d" | "h" | "m");
    let valid_number = number.parse::<u64>().is_ok_and(|value| value > 0);
    if valid_unit && valid_number {
        Ok(())
    } else {
        Err(format!(
            "{label}: window must be a duration like 30d, 24h, 30m, or all"
        ))
    }
}

fn validate_model_router_candidate(
    label: &str,
    candidate: &ModelRouterCandidateToml,
) -> Result<(), String> {
    if let Some(id) = &candidate.id
        && id.trim().is_empty()
    {
        return Err(format!("{label}: id must not be empty"));
    }
    if candidate.model.is_none()
        && candidate.model_provider.is_none()
        && candidate.service_tier.is_none()
        && candidate.reasoning_effort.is_none()
        && candidate.account_pool.is_none()
        && candidate.account.is_none()
        && candidate.intelligence_score.is_none()
        && candidate.success_rate.is_none()
        && candidate.median_latency_ms.is_none()
        && candidate.input_price_per_million.is_none()
        && candidate.cached_input_price_per_million.is_none()
        && candidate.output_price_per_million.is_none()
        && candidate.reasoning_output_price_per_million.is_none()
    {
        return Err(format!(
            "{label}: candidate must set at least one target field"
        ));
    }
    if candidate.account_pool.is_some() && candidate.account.is_some() {
        return Err(format!(
            "{label}: account_pool and account are mutually exclusive"
        ));
    }
    if let Some(account) = &candidate.account
        && (account.trim().is_empty() || !is_safe_account_id(account))
    {
        return Err(format!(
            "{label}: account must not be empty or contain path separators or parent directory components"
        ));
    }
    if let Some(account_pool) = &candidate.account_pool
        && account_pool.trim().is_empty()
    {
        return Err(format!("{label}: account_pool must not be empty"));
    }
    validate_unit_interval(label, "intelligence_score", candidate.intelligence_score)?;
    validate_unit_interval(label, "success_rate", candidate.success_rate)?;
    validate_non_negative(
        label,
        "input_price_per_million",
        candidate.input_price_per_million,
    )?;
    validate_non_negative(
        label,
        "cached_input_price_per_million",
        candidate.cached_input_price_per_million,
    )?;
    validate_non_negative(
        label,
        "output_price_per_million",
        candidate.output_price_per_million,
    )?;
    validate_non_negative(
        label,
        "reasoning_output_price_per_million",
        candidate.reasoning_output_price_per_million,
    )?;
    Ok(())
}

fn validate_unit_interval(label: &str, field: &str, value: Option<f64>) -> Result<(), String> {
    if let Some(value) = value
        && !(0.0..=1.0).contains(&value)
    {
        return Err(format!("{label}: {field} must be between 0.0 and 1.0"));
    }
    Ok(())
}

fn validate_non_negative(label: &str, field: &str, value: Option<f64>) -> Result<(), String> {
    if let Some(value) = value
        && (!value.is_finite() || value < 0.0)
    {
        return Err(format!("{label}: {field} must be non-negative"));
    }
    Ok(())
}

fn deserialize_model_router<'de, D>(deserializer: D) -> Result<Option<ModelRouterToml>, D::Error>
where
    D: Deserializer<'de>,
{
    let model_router = Option::<ModelRouterToml>::deserialize(deserializer)?;
    if let Some(model_router) = model_router.as_ref() {
        validate_model_router(model_router).map_err(serde::de::Error::custom)?;
    }
    Ok(model_router)
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
    use super::*;
    use pretty_assertions::assert_eq;

    const WORKSPACE_ID_A: &str = "123e4567-e89b-42d3-a456-426614174000";
    const WORKSPACE_ID_B: &str = "123e4567-e89b-42d3-a456-426614174001";

    #[test]
    fn forced_chatgpt_workspace_id_accepts_single_string() {
        let config: ConfigToml = toml::from_str(&format!(
            r#"forced_chatgpt_workspace_id = "{WORKSPACE_ID_A}""#
        ))
        .expect("single workspace id should deserialize");

        assert_eq!(
            config
                .forced_chatgpt_workspace_id
                .expect("workspace id should be set")
                .into_vec(),
            vec![WORKSPACE_ID_A.to_string()]
        );
    }

    #[test]
    fn forced_chatgpt_workspace_id_accepts_string_list() {
        let config: ConfigToml = toml::from_str(&format!(
            r#"forced_chatgpt_workspace_id = ["{WORKSPACE_ID_A}", "{WORKSPACE_ID_B}"]"#
        ))
        .expect("workspace id list should deserialize");

        assert_eq!(
            config
                .forced_chatgpt_workspace_id
                .expect("workspace ids should be set")
                .into_vec(),
            vec![WORKSPACE_ID_A.to_string(), WORKSPACE_ID_B.to_string()]
        );
    }

    #[test]
    fn forced_chatgpt_workspace_id_rejects_comma_separated_string() {
        let err = toml::from_str::<ConfigToml>(&format!(
            r#"forced_chatgpt_workspace_id = "{WORKSPACE_ID_A},{WORKSPACE_ID_B}""#
        ))
        .expect_err("comma-separated string should be rejected");

        let message = err.to_string();
        assert!(message.contains("TOML list of strings"));
        assert!(message.contains("comma-separated strings are not supported"));
    }

    #[test]
    fn model_router_accepts_valid_manual_candidate() {
        let config: ConfigToml = toml::from_str(
            r#"
[model_router]
enabled = true
discovery = "manual"

[[model_router.candidates]]
id = "fast"
model = "gpt-5.4"
reasoning_effort = "low"
intelligence_score = 0.8
"#,
        )
        .expect("model router config should deserialize");

        assert_eq!(
            config.model_router,
            Some(ModelRouterToml {
                enabled: true,
                discovery: Some(ModelRouterDiscoveryToml::Manual),
                candidates: vec![ModelRouterCandidateToml {
                    id: Some("fast".to_string()),
                    model: Some("gpt-5.4".to_string()),
                    reasoning_effort: Some(ModelRouterReasoningEffortToml::Low),
                    intelligence_score: Some(0.8),
                    ..Default::default()
                }],
                ..Default::default()
            })
        );
    }

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
        assert!(account_pool.enabled);
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
        assert!(model_policy.enabled);
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
    fn model_router_rejects_invalid_regex_selector() {
        let err = toml::from_str::<ConfigToml>(
            r#"
[model_router]
enabled = true

[model_router.models]
[[model_router.models.rules]]
type = "require"
models = [{ model = "/[/" }]
"#,
        )
        .expect_err("invalid model selector regex should be rejected");

        let message = err.to_string();
        assert!(message.contains("model_router.models.rules[0].models[0].model"));
        assert!(message.contains("invalid regex"));
    }

    #[test]
    fn model_router_rejects_ambiguous_account_target() {
        let err = toml::from_str::<ConfigToml>(
            r#"
[model_router]
enabled = true

[[model_router.candidates]]
model = "gpt-5.4"
account_pool = "primary"
account = "acct-a"
"#,
        )
        .expect_err("account_pool and account should be mutually exclusive");

        let message = err.to_string();
        assert!(message.contains("model_router.candidates[0]"));
        assert!(message.contains("account_pool and account are mutually exclusive"));
    }
}
