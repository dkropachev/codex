use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;
use tokio::task::JoinSet;

use codex_backend_client::Client as BackendClient;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
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

#[derive(Debug, Clone)]
struct AccountUsageTarget {
    id: String,
    source: AccountUsageSource,
    codex_home: PathBuf,
}

#[derive(Debug, Clone)]
struct AccountUsageSection {
    summary: Option<String>,
    targets: Vec<AccountUsageTarget>,
}

pub(crate) async fn run_account_limits(
    cli_config_overrides: CliConfigOverrides,
) -> anyhow::Result<()> {
    let config = load_config(cli_config_overrides).await?;
    let sections = account_usage_sections(&config).await;
    let targets = sections
        .iter()
        .flat_map(|section| section.targets.iter().cloned())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        println!("No accounts found");
        return Ok(());
    }

    let mut reports = vec![String::new(); targets.len()];
    let mut tasks = JoinSet::new();
    for (index, target) in targets.into_iter().enumerate() {
        let config = config.clone();
        tasks.spawn(async move { (index, render_account_usage(&config, &target).await) });
    }

    while let Some(result) = tasks.join_next().await {
        let (index, report) = result.map_err(|err| anyhow::anyhow!(err))?;
        reports[index] = report;
    }

    let mut report_index = 0;
    let mut wrote_section = false;
    for section in &sections {
        if section.targets.is_empty() {
            continue;
        }
        if wrote_section {
            println!();
        }
        if let Some(summary) = section.summary.as_ref() {
            println!("{summary}");
            println!();
        }
        for target_index in 0..section.targets.len() {
            if target_index > 0 {
                println!();
            }
            print!("{}", reports[report_index]);
            report_index += 1;
        }
        wrote_section = true;
    }

    Ok(())
}

async fn load_config(cli_config_overrides: CliConfigOverrides) -> anyhow::Result<Config> {
    let cli_overrides = cli_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    Ok(Config::load_with_cli_overrides(cli_overrides).await?)
}

async fn account_usage_sections(config: &Config) -> Vec<AccountUsageSection> {
    let mut sections = Vec::new();
    let mut seen = BTreeSet::new();
    if account_has_auth(&config.codex_home) {
        seen.insert(config.codex_home.to_path_buf());
        sections.push(AccountUsageSection {
            summary: None,
            targets: vec![AccountUsageTarget {
                id: "default".to_string(),
                source: AccountUsageSource::Default,
                codex_home: config.codex_home.to_path_buf(),
            }],
        });
    }

    let mut pool_member_ids = BTreeSet::new();
    if let Some(account_pool) = config.account_pool.as_ref().filter(|pool| pool.enabled) {
        let effective_default_pool = account_pool
            .default_pool
            .clone()
            .or_else(|| account_pool.pools.keys().next().cloned());
        for (pool_id, definition) in &account_pool.pools {
            let mut targets = Vec::new();
            let mut missing_count = 0;
            let mut invalid_count = 0;
            for account_id in &definition.accounts {
                pool_member_ids.insert(account_id.clone());
                let codex_home = account_codex_home(&config.codex_home, account_id);
                match credential_status(config, &codex_home, /*require_chatgpt*/ true).await {
                    AccountCredentialStatus::Missing => missing_count += 1,
                    AccountCredentialStatus::Invalid => invalid_count += 1,
                    AccountCredentialStatus::LoggedIn => {}
                }
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
            sections.push(AccountUsageSection {
                summary: Some(pool_summary(
                    pool_id,
                    definition,
                    effective_default_pool.as_deref() == Some(pool_id.as_str()),
                    missing_count,
                    invalid_count,
                )),
                targets,
            });
        }
    }

    let accounts_dir = config.codex_home.join("accounts");
    let Ok(entries) = std::fs::read_dir(accounts_dir) else {
        return sections;
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
        if pool_member_ids.contains(account_id) {
            continue;
        }
        sections.push(AccountUsageSection {
            summary: None,
            targets: vec![AccountUsageTarget {
                id: account_id.to_string(),
                source: AccountUsageSource::Named,
                codex_home,
            }],
        });
    }

    sections
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountCredentialStatus {
    LoggedIn,
    Missing,
    Invalid,
}

async fn credential_status(
    config: &Config,
    codex_home: &Path,
    require_chatgpt: bool,
) -> AccountCredentialStatus {
    match CodexAuth::from_auth_storage(
        codex_home,
        config.cli_auth_credentials_store_mode,
        Some(config.chatgpt_base_url.as_str()),
    )
    .await
    {
        Ok(Some(auth)) if !require_chatgpt || auth.is_chatgpt_auth() => {
            AccountCredentialStatus::LoggedIn
        }
        Ok(Some(_)) | Err(_) => AccountCredentialStatus::Invalid,
        Ok(None) => {
            if codex_home.join("auth.json").exists() {
                AccountCredentialStatus::Invalid
            } else {
                AccountCredentialStatus::Missing
            }
        }
    }
}

fn pool_summary(
    pool_id: &str,
    definition: &AccountPoolDefinitionToml,
    is_default: bool,
    missing_count: usize,
    invalid_count: usize,
) -> String {
    let default = if is_default { "default pool, " } else { "" };
    let mut summary = format!(
        "{pool_id} ({default}{}): {} {}",
        policy_name(definition.policy),
        definition.accounts.len(),
        if definition.accounts.len() == 1 {
            "member"
        } else {
            "members"
        }
    );
    if missing_count > 0 {
        let _ = write!(
            summary,
            ", {missing_count} missing {}",
            if missing_count == 1 {
                "credential"
            } else {
                "credentials"
            }
        );
    }
    if invalid_count > 0 {
        let _ = write!(
            summary,
            ", {invalid_count} invalid {}",
            if invalid_count == 1 {
                "credential"
            } else {
                "credentials"
            }
        );
    }
    summary
}

fn policy_name(policy: AccountPoolPolicyToml) -> &'static str {
    match policy {
        AccountPoolPolicyToml::Drain => "drain",
        AccountPoolPolicyToml::LoadBalance => "load_balance",
    }
}

fn account_has_auth(codex_home: &Path) -> bool {
    codex_home.join("auth.json").exists()
}

fn account_codex_home(codex_home: &Path, account_id: &str) -> PathBuf {
    codex_home.join("accounts").join(account_id)
}

async fn render_account_usage(config: &Config, target: &AccountUsageTarget) -> String {
    let mut output = String::new();
    write_account_header(&mut output, target);
    let require_chatgpt = matches!(target.source, AccountUsageSource::PoolMember { .. });
    match credential_status(config, &target.codex_home, require_chatgpt).await {
        AccountCredentialStatus::LoggedIn => {}
        AccountCredentialStatus::Missing => {
            output.push_str("  credentials: missing\n");
            output.push_str("  limits: unavailable\n");
            return output;
        }
        AccountCredentialStatus::Invalid => {
            output.push_str("  credentials: invalid\n");
            output.push_str("  limits: unavailable\n");
            return output;
        }
    }
    let manager = AuthManager::new(
        target.codex_home.clone(),
        /*enable_codex_api_key_env*/ false,
        config.cli_auth_credentials_store_mode,
        Some(config.chatgpt_base_url.clone()),
    )
    .await;
    let Some(auth) = manager.auth().await else {
        output.push_str("  credentials: missing\n");
        output.push_str("  limits: unavailable\n");
        return output;
    };

    write_auth_summary(&mut output, &auth);
    if !auth.uses_codex_backend() {
        output.push_str("  limits: unavailable: chatgpt authentication required\n");
        return output;
    }

    let usage = match BackendClient::from_auth(config.chatgpt_base_url.clone(), &auth) {
        Ok(client) => client.get_rate_limits_many().await,
        Err(err) => {
            let _ = writeln!(
                output,
                "  limits: error: failed to construct backend client: {err}"
            );
            return output;
        }
    };
    match usage {
        Ok(snapshots) if snapshots.is_empty() => {
            output.push_str("  limits: unavailable\n");
        }
        Ok(snapshots) => {
            for snapshot in snapshots {
                write_usage_limit(&mut output, &snapshot);
            }
        }
        Err(err) => {
            let _ = writeln!(output, "  limits: error: {err}");
        }
    }

    output
}

fn write_account_header(output: &mut String, target: &AccountUsageTarget) {
    match &target.source {
        AccountUsageSource::Default => {
            let _ = writeln!(output, "{} (default)", target.id);
        }
        AccountUsageSource::Named => {
            let _ = writeln!(output, "{}", target.id);
        }
        AccountUsageSource::PoolMember { pool_id } => {
            let _ = writeln!(output, "{} (pool: {pool_id})", target.id);
        }
    }
}

fn write_auth_summary(output: &mut String, auth: &CodexAuth) {
    let _ = writeln!(
        output,
        "  email: {}",
        auth.get_account_email().unwrap_or("-".to_string())
    );
    let _ = writeln!(
        output,
        "  account_id: {}",
        auth.get_account_id().unwrap_or("-".to_string())
    );
    let _ = writeln!(
        output,
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

fn write_usage_limit(output: &mut String, snapshot: &RateLimitSnapshot) {
    let name = snapshot
        .limit_name
        .as_deref()
        .or(snapshot.limit_id.as_deref())
        .unwrap_or("Codex");
    let _ = writeln!(output, "  {name}: available");
    for (fallback, window) in [("5h", &snapshot.primary), ("weekly", &snapshot.secondary)] {
        let Some(window) = window else {
            continue;
        };
        let _ = writeln!(
            output,
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
