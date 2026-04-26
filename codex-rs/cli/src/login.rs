//! CLI login commands and their direct-user observability surfaces.
//!
//! The TUI path already installs a broader tracing stack with feedback, OpenTelemetry, and other
//! interactive-session layers. Direct `codex login` intentionally does less: it preserves the
//! existing stderr/browser UX and adds only a small file-backed tracing layer for login-specific
//! targets. Keeping that setup local avoids pulling the TUI's session-oriented logging machinery
//! into a one-shot CLI command while still producing a durable `codex-login.log` artifact that
//! support can request from users.

use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::CLIENT_ID;
use codex_login::CodexAuth;
use codex_login::ServerOptions;
use codex_login::login_with_api_key;
use codex_login::logout_with_revoke;
use codex_login::run_device_code_login;
use codex_login::run_login_server;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_utils_cli::CliConfigOverrides;
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const CHATGPT_LOGIN_DISABLED_MESSAGE: &str =
    "ChatGPT login is disabled. Use API key login instead.";
const API_KEY_LOGIN_DISABLED_MESSAGE: &str =
    "API key login is disabled. Use ChatGPT login instead.";
const LOGIN_SUCCESS_MESSAGE: &str = "Successfully logged in";

/// Installs a small file-backed tracing layer for direct `codex login` flows.
///
/// This deliberately duplicates a narrow slice of the TUI logging setup instead of reusing it
/// wholesale. The TUI stack includes session-oriented layers that are valuable for interactive
/// runs but unnecessary for a one-shot login command. Keeping the direct CLI path local lets this
/// command produce a durable `codex-login.log` artifact without coupling it to the TUI's broader
/// telemetry and feedback initialization.
fn init_login_file_logging(config: &Config) -> Option<WorkerGuard> {
    let log_dir = match codex_core::config::log_dir(config) {
        Ok(log_dir) => log_dir,
        Err(err) => {
            eprintln!("Warning: failed to resolve login log directory: {err}");
            return None;
        }
    };

    if let Err(err) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "Warning: failed to create login log directory {}: {err}",
            log_dir.display()
        );
        return None;
    }

    let mut log_file_opts = OpenOptions::new();
    log_file_opts.create(true).append(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_file_opts.mode(0o600);
    }

    let log_path = log_dir.join("codex-login.log");
    let log_file = match log_file_opts.open(&log_path) {
        Ok(log_file) => log_file,
        Err(err) => {
            eprintln!(
                "Warning: failed to open login log file {}: {err}",
                log_path.display()
            );
            return None;
        }
    };

    let (non_blocking, guard) = non_blocking(log_file);
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("codex_cli=info,codex_core=info,codex_login=info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_target(true)
        .with_ansi(false)
        .with_filter(env_filter);

    // Direct `codex login` otherwise relies on ephemeral stderr and browser output.
    // Persist the same login targets to a file so support can inspect auth failures
    // without reproducing them through TUI or app-server.
    if let Err(err) = tracing_subscriber::registry().with(file_layer).try_init() {
        eprintln!(
            "Warning: failed to initialize login log file {}: {err}",
            log_path.display()
        );
        return None;
    }

    Some(guard)
}

fn print_login_server_start(actual_port: u16, auth_url: &str) {
    eprintln!(
        "Starting local login server on http://localhost:{actual_port}.\nIf your browser did not open, navigate to this URL to authenticate:\n\n{auth_url}\n\nOn a remote or headless machine? Use `codex login --device-auth` instead."
    );
}

pub async fn login_with_chatgpt(
    codex_home: PathBuf,
    forced_chatgpt_workspace_id: Option<String>,
    cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    let opts = ServerOptions::new(
        codex_home,
        CLIENT_ID.to_string(),
        forced_chatgpt_workspace_id,
        cli_auth_credentials_store_mode,
    );
    let server = run_login_server(opts)?;

    print_login_server_start(server.actual_port, &server.auth_url);

    server.block_until_done().await
}

fn account_codex_home(codex_home: &Path, account_id: Option<&str>) -> PathBuf {
    match account_id {
        Some(account_id) => {
            if !is_safe_account_id(account_id) {
                eprintln!(
                    "Invalid account id `{account_id}`: account ids must not contain path separators or parent directory components"
                );
                std::process::exit(1);
            }
            codex_home.join("accounts").join(account_id)
        }
        None => codex_home.to_path_buf(),
    }
}

fn is_safe_account_id(account_id: &str) -> bool {
    !account_id.trim().is_empty()
        && account_id != "."
        && account_id != ".."
        && !account_id.contains('/')
        && !account_id.contains('\\')
}

pub async fn run_login_with_chatgpt(
    cli_config_overrides: CliConfigOverrides,
    account_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting browser login flow");

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();

    match login_with_chatgpt(
        account_codex_home(&config.codex_home, account_id.as_deref()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
    )
    .await
    {
        Ok(_) => {
            eprintln!("{LOGIN_SUCCESS_MESSAGE}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run_login_with_api_key(
    cli_config_overrides: CliConfigOverrides,
    api_key: String,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting api key login flow");

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Chatgpt)) {
        eprintln!("{API_KEY_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    match login_with_api_key(
        &config.codex_home,
        &api_key,
        config.cli_auth_credentials_store_mode,
    ) {
        Ok(_) => {
            eprintln!("{LOGIN_SUCCESS_MESSAGE}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in: {e}");
            std::process::exit(1);
        }
    }
}

pub fn read_api_key_from_stdin() -> String {
    let mut stdin = std::io::stdin();

    if stdin.is_terminal() {
        eprintln!(
            "--with-api-key expects the API key on stdin. Try piping it, e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`."
        );
        std::process::exit(1);
    }

    eprintln!("Reading API key from stdin...");

    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        eprintln!("Failed to read API key from stdin: {err}");
        std::process::exit(1);
    }

    let api_key = buffer.trim().to_string();
    if api_key.is_empty() {
        eprintln!("No API key provided via stdin.");
        std::process::exit(1);
    }

    api_key
}

/// Login using the OAuth device code flow.
pub async fn run_login_with_device_code(
    cli_config_overrides: CliConfigOverrides,
    account_id: Option<String>,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting device code login flow");
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let mut opts = ServerOptions::new(
        account_codex_home(&config.codex_home, account_id.as_deref()),
        client_id.unwrap_or(CLIENT_ID.to_string()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
    );
    if let Some(iss) = issuer_base_url {
        opts.issuer = iss;
    }
    match run_device_code_login(opts).await {
        Ok(()) => {
            eprintln!("{LOGIN_SUCCESS_MESSAGE}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in with device code: {e}");
            std::process::exit(1);
        }
    }
}

/// Prefers device-code login (with `open_browser = false`) when headless environment is detected, but keeps
/// `codex login` working in environments where device-code may be disabled/feature-gated.
/// If `run_device_code_login` returns `ErrorKind::NotFound` ("device-code unsupported"), this
/// falls back to starting the local browser login server.
pub async fn run_login_with_device_code_fallback_to_browser(
    cli_config_overrides: CliConfigOverrides,
    account_id: Option<String>,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting login flow with device code fallback");
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let mut opts = ServerOptions::new(
        account_codex_home(&config.codex_home, account_id.as_deref()),
        client_id.unwrap_or(CLIENT_ID.to_string()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
    );
    if let Some(iss) = issuer_base_url {
        opts.issuer = iss;
    }
    opts.open_browser = false;

    match run_device_code_login(opts.clone()).await {
        Ok(()) => {
            eprintln!("{LOGIN_SUCCESS_MESSAGE}");
            std::process::exit(0);
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                eprintln!("Device code login is not enabled; falling back to browser login.");
                match run_login_server(opts) {
                    Ok(server) => {
                        print_login_server_start(server.actual_port, &server.auth_url);
                        match server.block_until_done().await {
                            Ok(()) => {
                                eprintln!("{LOGIN_SUCCESS_MESSAGE}");
                                std::process::exit(0);
                            }
                            Err(e) => {
                                eprintln!("Error logging in: {e}");
                                std::process::exit(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error logging in: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                eprintln!("Error logging in with device code: {e}");
                std::process::exit(1);
            }
        }
    }
}

pub async fn run_login_status(cli_config_overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;

    match CodexAuth::from_auth_storage(&config.codex_home, config.cli_auth_credentials_store_mode) {
        Ok(Some(auth)) => match auth.auth_mode() {
            AuthMode::ApiKey => match auth.get_token() {
                Ok(api_key) => {
                    eprintln!("Logged in using an API key - {}", safe_format_key(&api_key));
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Unexpected error retrieving API key: {e}");
                    std::process::exit(1);
                }
            },
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
                eprintln!("Logged in using ChatGPT");
                std::process::exit(0);
            }
            AuthMode::AgentIdentity => {
                eprintln!("Logged in using Agent Identity");
                std::process::exit(0);
            }
        },
        Ok(None) => {
            eprintln!("Not logged in");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error checking login status: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run_logout(
    cli_config_overrides: CliConfigOverrides,
    account_id: Option<String>,
    all: bool,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;

    let result = if all {
        logout_all_accounts(&config).await
    } else if let Some(account_id) = account_id {
        logout_with_revoke(
            &account_codex_home(&config.codex_home, Some(&account_id)),
            config.cli_auth_credentials_store_mode,
        )
        .await
    } else {
        logout_with_revoke(&config.codex_home, config.cli_auth_credentials_store_mode).await
    };

    match result {
        Ok(true) => {
            eprintln!("Successfully logged out");
            std::process::exit(0);
        }
        Ok(false) => {
            eprintln!("Not logged in");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging out: {e}");
            std::process::exit(1);
        }
    }
}

async fn logout_all_accounts(config: &Config) -> std::io::Result<bool> {
    let mut removed =
        logout_with_revoke(&config.codex_home, config.cli_auth_credentials_store_mode).await?;
    let accounts_dir = config.codex_home.join("accounts");
    let Ok(entries) = std::fs::read_dir(accounts_dir) else {
        return Ok(removed);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        removed |= logout_with_revoke(&path, config.cli_auth_credentials_store_mode).await?;
    }
    Ok(removed)
}

pub async fn run_list_accounts(cli_config_overrides: CliConfigOverrides, json: bool) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let pool_member_ids = config
        .account_pool
        .as_ref()
        .map(|pool| {
            pool.pools
                .values()
                .flat_map(|pool| pool.accounts.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let mut accounts = Vec::new();
    if CodexAuth::from_auth_storage(&config.codex_home, config.cli_auth_credentials_store_mode)
        .ok()
        .flatten()
        .is_some()
    {
        accounts.push(serde_json::json!({"id": "default", "type": "account"}));
    }
    if let Some(account_pool) = config.account_pool.as_ref() {
        for pool_id in account_pool.pools.keys() {
            accounts.push(serde_json::json!({"id": pool_id, "type": "pool"}));
        }
    }
    if let Ok(entries) = std::fs::read_dir(config.codex_home.join("accounts")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join("auth.json").exists() {
                continue;
            }
            let Some(account_id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if pool_member_ids.contains(account_id) {
                continue;
            }
            accounts.push(serde_json::json!({"id": account_id, "type": "account"}));
        }
    }
    if json {
        match serde_json::to_string_pretty(&serde_json::json!({ "accounts": accounts })) {
            Ok(payload) => println!("{payload}"),
            Err(err) => {
                eprintln!("Error serializing account list: {err}");
                std::process::exit(1);
            }
        }
    } else if accounts.is_empty() {
        println!("No accounts found");
    } else {
        for account in accounts {
            let id = account
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let kind = account
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            println!("{id}\t{kind}");
        }
    }
    std::process::exit(0);
}

pub async fn run_login_with_account_refresh(
    cli_config_overrides: CliConfigOverrides,
    account_id: Option<String>,
    pool_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    if let Some(account_id) = account_id {
        let manager = AuthManager::new(
            account_codex_home(&config.codex_home, Some(&account_id)),
            /*enable_codex_api_key_env*/ false,
            config.cli_auth_credentials_store_mode,
            Some(config.chatgpt_base_url.clone()),
        );
        if manager.auth().await.is_some() {
            eprintln!("Refreshed account {account_id}");
            std::process::exit(0);
        }
        eprintln!("Account {account_id} is not logged in");
        std::process::exit(1);
    }

    if let Some(pool_id) = pool_id.as_deref() {
        let pool_exists = config
            .account_pool
            .as_ref()
            .is_some_and(|account_pool| account_pool.pools.contains_key(pool_id));
        if !pool_exists {
            eprintln!("Account pool {pool_id} not found");
            std::process::exit(1);
        }
    }

    let manager = AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false);
    manager.refresh_account_pool_usage(pool_id.as_deref()).await;
    eprintln!("Refreshed account pool usage");
    std::process::exit(0);
}

async fn load_config_or_exit(cli_config_overrides: CliConfigOverrides) -> Config {
    let cli_overrides = match cli_config_overrides.parse_overrides() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    match Config::load_with_cli_overrides(cli_overrides).await {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error loading configuration: {e}");
            std::process::exit(1);
        }
    }
}

fn safe_format_key(key: &str) -> String {
    if key.len() <= 13 {
        return "***".to_string();
    }
    let prefix = &key[..8];
    let suffix = &key[key.len() - 5..];
    format!("{prefix}***{suffix}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::account_codex_home;
    use super::is_safe_account_id;
    use super::safe_format_key;

    #[test]
    fn formats_long_key() {
        let key = "sk-proj-1234567890ABCDE";
        assert_eq!(safe_format_key(key), "sk-proj-***ABCDE");
    }

    #[test]
    fn short_key_returns_stars() {
        let key = "sk-proj-12345";
        assert_eq!(safe_format_key(key), "***");
    }

    #[test]
    fn account_codex_home_uses_default_or_named_account_dir() {
        let codex_home = PathBuf::from("/tmp/codex-home");

        assert_eq!(account_codex_home(&codex_home, None), codex_home);
        assert_eq!(
            account_codex_home(&PathBuf::from("/tmp/codex-home"), Some("work")),
            PathBuf::from("/tmp/codex-home/accounts/work")
        );
    }

    #[test]
    fn account_ids_reject_unsafe_values() {
        for account_id in ["work", "work.pro"] {
            assert!(is_safe_account_id(account_id), "{account_id}");
        }
        for account_id in ["", " ", ".", "..", "../work", "team/work", "team\\work"] {
            assert!(!is_safe_account_id(account_id), "{account_id}");
        }
    }
}
