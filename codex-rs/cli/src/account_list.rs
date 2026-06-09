use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use codex_app_server_protocol::AuthMode;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_core::config::Config;
use codex_login::CodexAuth;
use codex_utils_cli::CliConfigOverrides;
use serde_json::json;

use crate::login::load_config_or_exit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialStatus {
    LoggedIn,
    Missing,
    Invalid,
}

#[derive(Debug, Clone)]
struct AccountCredential {
    status: CredentialStatus,
    auth_mode: Option<AuthMode>,
}

#[derive(Debug, Clone)]
struct ListedAccount {
    id: String,
    credential: AccountCredential,
}

#[derive(Debug, Clone)]
struct ListedPool {
    id: String,
    is_default: bool,
    provider: String,
    policy: AccountPoolPolicyToml,
    members: Vec<ListedAccount>,
}

#[derive(Debug)]
struct ListedAccounts {
    default_account: Option<ListedAccount>,
    pools: Vec<ListedPool>,
    standalone_accounts: Vec<ListedAccount>,
}

pub async fn run_list_accounts(cli_config_overrides: CliConfigOverrides, json_output: bool) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let accounts = collect_accounts(&config).await;

    if json_output {
        match serde_json::to_string_pretty(&accounts_json(&accounts)) {
            Ok(payload) => println!("{payload}"),
            Err(err) => {
                eprintln!("Error serializing account list: {err}");
                std::process::exit(1);
            }
        }
    } else {
        print_accounts_human(&accounts);
    }

    std::process::exit(0);
}

async fn collect_accounts(config: &Config) -> ListedAccounts {
    let default_account =
        credential_for_account_home(&config.codex_home, config, /*require_chatgpt*/ false)
            .await
            .filter(|credential| credential.status != CredentialStatus::Missing)
            .map(|credential| ListedAccount {
                id: "default".to_string(),
                credential,
            });

    let effective_default_pool = effective_default_pool(config);
    let mut pools = Vec::new();
    if let Some(account_pool) = config.account_pool.as_ref().filter(|pool| pool.enabled) {
        for (pool_id, definition) in &account_pool.pools {
            pools.push(listed_pool(config, pool_id, definition, &effective_default_pool).await);
        }
    }

    let pool_member_ids = pools
        .iter()
        .flat_map(|pool| pool.members.iter().map(|member| member.id.clone()))
        .collect::<BTreeSet<_>>();
    let mut standalone_accounts = Vec::new();
    for account_id in named_account_ids(config) {
        if pool_member_ids.contains(&account_id) {
            continue;
        }
        let account_home = account_codex_home(&config.codex_home, &account_id);
        standalone_accounts.push(ListedAccount {
            id: account_id,
            credential: credential_for_account_home(
                &account_home,
                config,
                /*require_chatgpt*/ false,
            )
            .await
            .unwrap_or(AccountCredential {
                status: CredentialStatus::Missing,
                auth_mode: None,
            }),
        });
    }

    ListedAccounts {
        default_account,
        pools,
        standalone_accounts,
    }
}

async fn listed_pool(
    config: &Config,
    pool_id: &str,
    definition: &AccountPoolDefinitionToml,
    effective_default_pool: &Option<String>,
) -> ListedPool {
    let mut members = Vec::new();
    for account_id in &definition.accounts {
        let account_home = account_codex_home(&config.codex_home, account_id);
        members.push(ListedAccount {
            id: account_id.clone(),
            credential: credential_for_account_home(
                &account_home,
                config,
                /*require_chatgpt*/ true,
            )
            .await
            .unwrap_or(AccountCredential {
                status: CredentialStatus::Missing,
                auth_mode: None,
            }),
        });
    }

    ListedPool {
        id: pool_id.to_string(),
        is_default: effective_default_pool.as_deref() == Some(pool_id),
        provider: definition.provider.clone(),
        policy: definition.policy,
        members,
    }
}

async fn credential_for_account_home(
    codex_home: &Path,
    config: &Config,
    require_chatgpt: bool,
) -> Option<AccountCredential> {
    match CodexAuth::from_auth_storage(
        codex_home,
        config.cli_auth_credentials_store_mode,
        Some(&config.chatgpt_base_url),
    )
    .await
    {
        Ok(Some(auth)) => {
            let status = if require_chatgpt && !auth.is_chatgpt_auth() {
                CredentialStatus::Invalid
            } else {
                CredentialStatus::LoggedIn
            };
            Some(AccountCredential {
                status,
                auth_mode: Some(auth.auth_mode()),
            })
        }
        Ok(None) => {
            if codex_home.join("auth.json").exists() {
                Some(AccountCredential {
                    status: CredentialStatus::Invalid,
                    auth_mode: None,
                })
            } else {
                Some(AccountCredential {
                    status: CredentialStatus::Missing,
                    auth_mode: None,
                })
            }
        }
        Err(_) => Some(AccountCredential {
            status: CredentialStatus::Invalid,
            auth_mode: None,
        }),
    }
}

fn named_account_ids(config: &Config) -> Vec<String> {
    let accounts_dir = config.codex_home.join("accounts");
    let Ok(entries) = std::fs::read_dir(accounts_dir) else {
        return Vec::new();
    };

    let mut ids = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.join("auth.json").exists() {
                return None;
            }
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn account_codex_home(codex_home: &Path, account_id: &str) -> std::path::PathBuf {
    codex_home.join("accounts").join(account_id)
}

fn effective_default_pool(config: &Config) -> Option<String> {
    let account_pool = config.account_pool.as_ref()?.clone();
    if !account_pool.enabled {
        return None;
    }
    account_pool
        .default_pool
        .or_else(|| account_pool.pools.keys().next().cloned())
}

fn print_accounts_human(accounts: &ListedAccounts) {
    if accounts.default_account.is_none()
        && accounts.pools.is_empty()
        && accounts.standalone_accounts.is_empty()
    {
        println!("No accounts found");
        return;
    }

    let mut needs_blank = false;
    if let Some(default_account) = accounts.default_account.as_ref() {
        println!("Default account:");
        println!(
            "  {}: {}",
            default_account.id,
            default_account.credential.status.as_str()
        );
        needs_blank = true;
    }

    for pool in &accounts.pools {
        if needs_blank {
            println!();
        }
        let default_label = if pool.is_default {
            "default pool, "
        } else {
            ""
        };
        println!(
            "Pool {} ({}provider {}, policy {}):",
            pool.id,
            default_label,
            pool.provider,
            policy_name(pool.policy)
        );
        for member in &pool.members {
            println!("  {}: {}", member.id, member.credential.status.as_str());
        }
        needs_blank = true;
    }

    if !accounts.standalone_accounts.is_empty() {
        if needs_blank {
            println!();
        }
        println!("Standalone accounts:");
        for account in &accounts.standalone_accounts {
            println!("  {}: {}", account.id, account.credential.status.as_str());
        }
    }
}

fn accounts_json(accounts: &ListedAccounts) -> serde_json::Value {
    let mut account_values = Vec::new();
    if let Some(default_account) = accounts.default_account.as_ref() {
        account_values.push(account_json(default_account, Vec::new()));
    }

    let memberships = pool_memberships(accounts);
    for pool in &accounts.pools {
        account_values.push(json!({
            "id": pool.id,
            "type": "pool",
            "default": pool.is_default,
            "provider": pool.provider,
            "policy": policy_name(pool.policy),
            "members": pool.members.iter().map(|member| member.id.clone()).collect::<Vec<_>>(),
        }));
    }

    let mut emitted_members = BTreeSet::new();
    for pool in &accounts.pools {
        for member in &pool.members {
            if emitted_members.insert(member.id.clone()) {
                account_values.push(account_json(
                    member,
                    memberships.get(&member.id).cloned().unwrap_or_default(),
                ));
            }
        }
    }

    for account in &accounts.standalone_accounts {
        account_values.push(account_json(account, Vec::new()));
    }

    let pool_values = accounts
        .pools
        .iter()
        .map(|pool| {
            json!({
                "id": pool.id,
                "default": pool.is_default,
                "provider": pool.provider,
                "policy": policy_name(pool.policy),
                "memberIds": pool.members.iter().map(|member| member.id.clone()).collect::<Vec<_>>(),
                "members": pool.members.iter().map(|member| {
                    json!({
                        "id": member.id,
                        "credentialStatus": member.credential.status.as_str(),
                        "authMode": member.credential.auth_mode.map(auth_mode_name),
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "accounts": account_values,
        "pools": pool_values,
    })
}

fn pool_memberships(accounts: &ListedAccounts) -> BTreeMap<String, Vec<serde_json::Value>> {
    let mut memberships = BTreeMap::<String, Vec<serde_json::Value>>::new();
    for pool in &accounts.pools {
        for (index, member) in pool.members.iter().enumerate() {
            memberships
                .entry(member.id.clone())
                .or_default()
                .push(json!({
                    "poolId": pool.id,
                    "default": pool.is_default,
                    "memberIndex": index,
                }));
        }
    }
    memberships
}

fn account_json(
    account: &ListedAccount,
    pool_membership: Vec<serde_json::Value>,
) -> serde_json::Value {
    let pools = pool_membership
        .iter()
        .filter_map(|membership| membership.get("poolId").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    json!({
        "id": account.id,
        "type": "account",
        "credentialStatus": account.credential.status.as_str(),
        "authMode": account.credential.auth_mode.map(auth_mode_name),
        "pools": pools,
        "poolMembership": pool_membership,
    })
}

impl CredentialStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::LoggedIn => "logged in",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        }
    }
}

fn auth_mode_name(auth_mode: AuthMode) -> &'static str {
    match auth_mode {
        AuthMode::ApiKey => "apiKey",
        AuthMode::Chatgpt => "chatgpt",
        AuthMode::ChatgptAuthTokens => "chatgptAuthTokens",
        AuthMode::AgentIdentity => "agentIdentity",
        AuthMode::PersonalAccessToken => "personalAccessToken",
    }
}

fn policy_name(policy: AccountPoolPolicyToml) -> &'static str {
    match policy {
        AccountPoolPolicyToml::Drain => "drain",
        AccountPoolPolicyToml::LoadBalance => "load_balance",
    }
}
