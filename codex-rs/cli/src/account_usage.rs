use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use codex_backend_client::Client as BackendClient;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::account::PlanType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Clone, PartialEq, Eq)]
enum AccountUsageSource {
    Default,
    Named,
    PoolMember { pool_id: String },
}

#[derive(Debug)]
struct AccountUsageTarget {
    id: String,
    source: AccountUsageSource,
    codex_home: PathBuf,
}

pub(crate) async fn run_account_limits(
    cli_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let config = load_config(cli_config_overrides).await?;
    let targets = account_usage_targets(&config);
    if targets.is_empty() {
        println!("No accounts found");
        return Ok(());
    }

    for (index, target) in targets.iter().enumerate() {
        if index > 0 {
            println!();
        }
        print_account_usage(&config, target).await;
    }

    Ok(())
}

async fn load_config(cli_config_overrides: CliConfigOverrides) -> anyhow::Result<Config> {
    let cli_overrides = cli_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    Ok(Config::load_with_cli_overrides(cli_overrides).await?)
}

fn account_usage_targets(config: &Config) -> Vec<AccountUsageTarget> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    if account_has_auth(&config.codex_home) {
        seen.insert(config.codex_home.to_path_buf());
        targets.push(AccountUsageTarget {
            id: "default".to_string(),
            source: AccountUsageSource::Default,
            codex_home: config.codex_home.to_path_buf(),
        });
    }

    let pool_members = pool_members_by_account(config);
    for (account_id, pool_id) in &pool_members {
        let codex_home = account_codex_home(&config.codex_home, account_id);
        if seen.insert(codex_home.clone()) {
            targets.push(AccountUsageTarget {
                id: account_id.clone(),
                source: AccountUsageSource::PoolMember {
                    pool_id: pool_id.clone(),
                },
                codex_home,
            });
        }
    }

    let accounts_dir = config.codex_home.join("accounts");
    let Ok(entries) = std::fs::read_dir(accounts_dir) else {
        return targets;
    };
    let mut account_dirs = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("auth.json").exists())
        .collect::<Vec<_>>();
    account_dirs.sort();
    for codex_home in account_dirs {
        if !seen.insert(codex_home.clone()) {
            continue;
        }
        let Some(account_id) = codex_home.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        targets.push(AccountUsageTarget {
            id: account_id.to_string(),
            source: AccountUsageSource::Named,
            codex_home,
        });
    }

    targets
}

fn pool_members_by_account(config: &Config) -> BTreeMap<String, String> {
    config
        .account_pool
        .as_ref()
        .filter(|account_pool| account_pool.enabled)
        .map(|account_pool| {
            account_pool
                .pools
                .iter()
                .flat_map(|(pool_id, AccountPoolDefinitionToml { accounts, .. })| {
                    accounts
                        .iter()
                        .map(|account_id| (account_id.clone(), pool_id.clone()))
                        .collect::<Vec<_>>()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn account_has_auth(codex_home: &Path) -> bool {
    codex_home.join("auth.json").exists()
}

fn account_codex_home(codex_home: &Path, account_id: &str) -> PathBuf {
    codex_home.join("accounts").join(account_id)
}

async fn print_account_usage(config: &Config, target: &AccountUsageTarget) {
    print_account_header(target);
    let manager = AuthManager::new(
        target.codex_home.clone(),
        /*enable_codex_api_key_env*/ false,
        config.cli_auth_credentials_store_mode,
        Some(config.chatgpt_base_url.clone()),
    );
    let Some(auth) = manager.auth().await else {
        println!("  credentials: empty");
        println!("  limits: unavailable");
        return;
    };

    print_auth_summary(&auth);
    if !auth.uses_codex_backend() {
        println!("  limits: unavailable: chatgpt authentication required");
        return;
    }

    let usage = match BackendClient::from_auth(config.chatgpt_base_url.clone(), &auth) {
        Ok(client) => client.get_rate_limits_many().await,
        Err(err) => {
            println!("  limits: error: failed to construct backend client: {err}");
            return;
        }
    };
    match usage {
        Ok(snapshots) if snapshots.is_empty() => {
            println!("  limits: unavailable");
        }
        Ok(snapshots) => {
            for snapshot in snapshots {
                print_usage_limit(&snapshot);
            }
        }
        Err(err) => {
            println!("  limits: error: {err}");
        }
    }
}

fn print_account_header(target: &AccountUsageTarget) {
    match &target.source {
        AccountUsageSource::Default => println!("{} (default)", target.id),
        AccountUsageSource::Named => println!("{}", target.id),
        AccountUsageSource::PoolMember { pool_id } => {
            println!("{} (pool: {pool_id})", target.id);
        }
    }
}

fn print_auth_summary(auth: &CodexAuth) {
    println!(
        "  email: {}",
        auth.get_account_email().unwrap_or("-".to_string())
    );
    println!(
        "  account_id: {}",
        auth.get_account_id().unwrap_or("-".to_string())
    );
    println!(
        "  plan: {}",
        auth.account_plan_type()
            .map(plan_type_name)
            .unwrap_or("-".to_string())
    );
}

fn plan_type_name(plan_type: PlanType) -> String {
    match plan_type {
        PlanType::Free => "free",
        PlanType::Go => "go",
        PlanType::Plus => "plus",
        PlanType::Pro => "pro",
        PlanType::ProLite => "pro_lite",
        PlanType::Team => "team",
        PlanType::SelfServeBusinessUsageBased => "self_serve_business_usage_based",
        PlanType::Business => "business",
        PlanType::EnterpriseCbpUsageBased => "enterprise_cbp_usage_based",
        PlanType::Enterprise => "enterprise",
        PlanType::Edu => "edu",
        PlanType::Unknown => "unknown",
    }
    .to_string()
}

fn print_usage_limit(snapshot: &RateLimitSnapshot) {
    let name = snapshot
        .limit_name
        .as_deref()
        .or(snapshot.limit_id.as_deref())
        .unwrap_or("Codex");
    println!("  {name}: available");
    for (fallback, window) in [("5h", &snapshot.primary), ("weekly", &snapshot.secondary)] {
        let Some(window) = window else {
            continue;
        };
        println!(
            "    {}: {:.0}% used; refreshes {}",
            window_display_name(window, fallback),
            window.used_percent,
            format_reset_time(window)
        );
    }
}

fn window_display_name(window: &RateLimitWindow, fallback: &str) -> String {
    match window.window_minutes {
        Some(300) => "5h".to_string(),
        Some(10_080) => "weekly".to_string(),
        Some(minutes) if minutes > 0 => format_compact_duration(minutes * 60),
        _ => fallback.to_string(),
    }
}

fn format_reset_time(window: &RateLimitWindow) -> String {
    let Some(reset_at) = window.resets_at else {
        return "-".to_string();
    };
    let Some(reset_time) = chrono::DateTime::from_timestamp(reset_at, 0) else {
        return "-".to_string();
    };
    reset_time
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

fn format_compact_duration(seconds: i64) -> String {
    let mut remaining = seconds.max(0);
    if remaining == 0 {
        return "0s".to_string();
    }
    let units = [
        ("w", 604_800),
        ("d", 86_400),
        ("h", 3_600),
        ("m", 60),
        ("s", 1),
    ];
    let mut parts = Vec::new();
    for (suffix, unit_seconds) in units {
        if remaining < unit_seconds && parts.is_empty() {
            continue;
        }
        if remaining < unit_seconds {
            continue;
        }
        let value = remaining / unit_seconds;
        remaining %= unit_seconds;
        parts.push(format!("{value}{suffix}"));
        if parts.len() == 3 {
            break;
        }
    }
    if parts.is_empty() {
        "0s".to_string()
    } else {
        parts.join(" ")
    }
}
