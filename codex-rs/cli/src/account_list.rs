#[path = "account_list_usage.rs"]
mod usage;

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use codex_app_server_protocol::AuthMode;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_core::config::Config;
use codex_login::CodexAuth;
use codex_protocol::auth::AuthMode as ProtocolAuthMode;
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
    usage: usage::AccountUsage,
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
    let mut ids = named_account_ids(config)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let account_pool = config.account_pool.as_ref().filter(|pool| pool.enabled);
    let pool_member_homes = account_pool
        .into_iter()
        .flat_map(|pool| pool.pools.values())
        .flat_map(|pool| &pool.accounts)
        .map(|id| {
            ids.insert(id.clone());
            account_codex_home(&config.codex_home, id)
        })
        .collect::<BTreeSet<_>>();
    let homes = std::iter::once(config.codex_home.to_path_buf())
        .chain(
            ids.iter()
                .map(|id| account_codex_home(&config.codex_home, id)),
        )
        .collect::<BTreeSet<_>>();
    let credentials = futures::future::join_all(homes.into_iter().map(|home| {
        let requirement = if pool_member_homes.contains(&home) {
            usage::CredentialRequirement::Chatgpt
        } else {
            usage::CredentialRequirement::Any
        };
        async move {
            let credential = usage::lookup(config, &home, requirement).await;
            (home, credential)
        }
    }))
    .await
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let default_account = credentials
        .get(config.codex_home.as_path())
        .filter(|credential| credential.status != CredentialStatus::Missing)
        .map(|credential| ListedAccount {
            id: "default".to_string(),
            credential: credential.clone(),
        });
    let effective_default_pool = effective_default_pool(config);
    let pools = account_pool
        .into_iter()
        .flat_map(|pool| &pool.pools)
        .map(|(pool_id, definition)| {
            listed_pool(
                config,
                pool_id,
                definition,
                &effective_default_pool,
                &credentials,
            )
        })
        .collect::<Vec<_>>();
    let pool_member_ids = pools
        .iter()
        .flat_map(|pool| pool.members.iter().map(|member| member.id.clone()))
        .collect::<BTreeSet<_>>();
    let standalone_accounts = ids
        .into_iter()
        .filter(|id| !pool_member_ids.contains(id))
        .map(|id| ListedAccount {
            credential: credentials[&account_codex_home(&config.codex_home, &id)].clone(),
            id,
        })
        .collect();

    ListedAccounts {
        default_account,
        pools,
        standalone_accounts,
    }
}

fn listed_pool(
    config: &Config,
    pool_id: &str,
    definition: &AccountPoolDefinitionToml,
    effective_default_pool: &Option<String>,
    credentials: &BTreeMap<std::path::PathBuf, AccountCredential>,
) -> ListedPool {
    let members = definition
        .accounts
        .iter()
        .map(|id| {
            let credential = credentials[&account_codex_home(&config.codex_home, id)].clone();
            ListedAccount {
                id: id.clone(),
                credential,
            }
        })
        .collect();
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
) -> (AccountCredential, Option<CodexAuth>) {
    let auth_route_config = config.auth_route_config();
    let auth = if codex_home == config.codex_home.as_path() {
        CodexAuth::from_auth_storage(
            codex_home,
            config.cli_auth_credentials_store_mode,
            Some(config.chatgpt_base_url.as_str()),
            config.auth_keyring_backend_kind(),
            &auth_route_config,
        )
        .await
    } else {
        CodexAuth::from_stored_auth_storage(
            codex_home,
            config.cli_auth_credentials_store_mode,
            Some(config.chatgpt_base_url.as_str()),
            config.auth_keyring_backend_kind(),
            &auth_route_config,
        )
        .await
    };
    match auth {
        Ok(Some(auth)) => {
            let status = if require_chatgpt && !auth.is_chatgpt_auth() {
                CredentialStatus::Invalid
            } else {
                CredentialStatus::LoggedIn
            };
            (
                AccountCredential {
                    status,
                    auth_mode: Some(app_server_auth_mode(auth.auth_mode())),
                    usage: usage::AccountUsage::default(),
                },
                Some(auth),
            )
        }
        Ok(None) => {
            if codex_home.join("auth.json").exists() {
                (
                    AccountCredential {
                        status: CredentialStatus::Invalid,
                        auth_mode: None,
                        usage: usage::AccountUsage::default(),
                    },
                    None,
                )
            } else {
                (
                    AccountCredential {
                        status: CredentialStatus::Missing,
                        auth_mode: None,
                        usage: usage::AccountUsage::default(),
                    },
                    None,
                )
            }
        }
        Err(_) => (
            AccountCredential {
                status: CredentialStatus::Invalid,
                auth_mode: None,
                usage: usage::AccountUsage::default(),
            },
            None,
        ),
    }
}

fn app_server_auth_mode(auth_mode: ProtocolAuthMode) -> AuthMode {
    match auth_mode {
        ProtocolAuthMode::ApiKey => AuthMode::ApiKey,
        ProtocolAuthMode::Chatgpt => AuthMode::Chatgpt,
        ProtocolAuthMode::ChatgptAuthTokens => AuthMode::ChatgptAuthTokens,
        ProtocolAuthMode::AgentIdentity => AuthMode::AgentIdentity,
        ProtocolAuthMode::PersonalAccessToken => AuthMode::PersonalAccessToken,
        ProtocolAuthMode::BedrockApiKey => AuthMode::BedrockApiKey,
        ProtocolAuthMode::BedrockAccessKeys => AuthMode::BedrockAccessKeys,
        ProtocolAuthMode::Headers => AuthMode::Headers,
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
        print!("{}", usage::render(&default_account.credential.usage));
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
            print!("{}", usage::render(&member.credential.usage));
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
            print!("{}", usage::render(&account.credential.usage));
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
                        "usage": member.credential.usage,
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
        "usage": account.credential.usage,
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
        AuthMode::BedrockApiKey => "bedrockApiKey",
        AuthMode::BedrockAccessKeys => "bedrockAccessKeys",
        AuthMode::Headers => "headers",
    }
}

fn policy_name(policy: AccountPoolPolicyToml) -> &'static str {
    match policy {
        AccountPoolPolicyToml::Drain => "drain",
        AccountPoolPolicyToml::LoadBalance => "load_balance",
    }
}
