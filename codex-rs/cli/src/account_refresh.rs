use codex_core::config::Config;
use codex_login::AccountPoolUsageRefreshPoolReport;
use codex_login::AccountPoolUsageRefreshReport;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::token_data::parse_jwt_expiration;
use codex_utils_cli::CliConfigOverrides;

use crate::login::account_codex_home;
use crate::login::load_config_or_exit;

pub async fn run_login_with_account_refresh(
    cli_config_overrides: CliConfigOverrides,
    account_id: Option<String>,
    pool_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;

    if let Some(account_id) = account_id {
        refresh_single_account(&config, &account_id).await;
    }

    if let Some(pool_id) = pool_id.as_deref() {
        let pool_exists = config
            .account_pool
            .as_ref()
            .filter(|account_pool| account_pool.enabled)
            .is_some_and(|account_pool| account_pool.pools.contains_key(pool_id));
        if !pool_exists {
            eprintln!("Account pool {pool_id} not found");
            std::process::exit(1);
        }
        refresh_account_pools(&config, Some(pool_id)).await;
    }

    if config
        .account_pool
        .as_ref()
        .filter(|account_pool| account_pool.enabled)
        .is_some_and(|account_pool| !account_pool.pools.is_empty())
    {
        refresh_account_pools(&config, /*pool_id*/ None).await;
    }

    refresh_single_account(&config, "default").await;
}

async fn refresh_single_account(config: &Config, account_id: &str) -> ! {
    let account_home = if account_id == "default" {
        config.codex_home.to_path_buf()
    } else {
        account_codex_home(&config.codex_home, Some(account_id))
    };
    match CodexAuth::from_auth_storage(
        &account_home,
        config.cli_auth_credentials_store_mode,
        Some(&config.chatgpt_base_url),
    )
    .await
    {
        Ok(Some(auth)) if auth.is_chatgpt_auth() => {
            let manager = AuthManager::new(
                account_home,
                /*enable_codex_api_key_env*/ false,
                config.cli_auth_credentials_store_mode,
                Some(config.chatgpt_base_url.clone()),
            )
            .await;
            if access_token_expired(&auth)
                && let Err(err) = manager.refresh_token().await
            {
                eprintln!("Account {account_id} invalid credentials: {err}");
                std::process::exit(1);
            }
            if manager.auth().await.is_some() {
                eprintln!("Refreshed 1/1 accounts: {account_id}");
                std::process::exit(0);
            }
            eprintln!("Account {account_id} invalid credentials");
            std::process::exit(1);
        }
        Ok(Some(_)) => {
            eprintln!("Account {account_id} invalid credentials: ChatGPT credentials required");
            std::process::exit(1);
        }
        Ok(None) => {
            eprintln!("Account {account_id} missing credentials");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("Account {account_id} invalid credentials: {err}");
            std::process::exit(1);
        }
    }
}

fn access_token_expired(auth: &CodexAuth) -> bool {
    auth.get_token_data()
        .ok()
        .and_then(|tokens| parse_jwt_expiration(&tokens.access_token).ok().flatten())
        .is_some_and(|expires_at| expires_at <= chrono::Utc::now())
}

async fn refresh_account_pools(config: &Config, pool_id: Option<&str>) -> ! {
    let manager = AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false).await;
    let Some(report) = manager.refresh_account_pool_usage_report(pool_id).await else {
        eprintln!("No account pools configured");
        std::process::exit(1);
    };
    print_account_pool_refresh_report(&report);
    if report_has_usable_credentials(&report) {
        std::process::exit(0);
    }
    std::process::exit(1);
}

fn print_account_pool_refresh_report(report: &AccountPoolUsageRefreshReport) {
    if report.pools.is_empty() {
        eprintln!("No account pools matched");
        return;
    }
    for pool in &report.pools {
        eprintln!("{}", format_account_pool_refresh_summary(pool));
    }
}

fn report_has_usable_credentials(report: &AccountPoolUsageRefreshReport) -> bool {
    report
        .pools
        .iter()
        .any(|pool| pool.usable_credentials_count > 0)
}

fn format_account_pool_refresh_summary(pool: &AccountPoolUsageRefreshPoolReport) -> String {
    let mut summary = format!(
        "Refreshed {}/{} accounts in pool {}",
        pool.refreshed_count, pool.member_count, pool.pool_id
    );
    if !pool.problems.is_empty() {
        summary.push_str("; ");
        summary.push_str(
            &pool
                .problems
                .iter()
                .map(|problem| format!("{} {}", problem.account_id, problem.message))
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_login::AccountPoolUsageRefreshProblem;
    use pretty_assertions::assert_eq;

    #[test]
    fn formats_partial_pool_refresh_summary() {
        let pool = AccountPoolUsageRefreshPoolReport {
            pool_id: "codex-pro".to_string(),
            member_count: 2,
            refreshed_count: 1,
            usable_credentials_count: 1,
            problems: vec![AccountPoolUsageRefreshProblem {
                account_id: "personal-pro".to_string(),
                message: "missing credentials".to_string(),
            }],
        };

        assert_eq!(
            format_account_pool_refresh_summary(&pool),
            "Refreshed 1/2 accounts in pool codex-pro; personal-pro missing credentials"
        );
    }
}
