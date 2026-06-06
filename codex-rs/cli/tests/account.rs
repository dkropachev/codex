use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use anyhow::anyhow;
use chrono::Duration as ChronoDuration;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::TokenData;
use codex_login::login_with_api_key;
use codex_login::save_auth;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.env("no_proxy", "127.0.0.1,localhost");
    Ok(cmd)
}

fn write_config(codex_home: &Path, extra: &str) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!("cli_auth_credentials_store = \"file\"\n{extra}"),
    )?;
    Ok(())
}

fn write_chatgpt_auth(account_home: &Path, account_id: &str, email: &str) -> Result<()> {
    write_chatgpt_auth_with_expiration(
        account_home,
        account_id,
        email,
        Utc::now().timestamp() + 3600,
        Utc::now(),
    )
}

fn write_expired_chatgpt_auth(account_home: &Path, account_id: &str, email: &str) -> Result<()> {
    write_chatgpt_auth_with_expiration(
        account_home,
        account_id,
        email,
        Utc::now().timestamp() - 3600,
        Utc::now() - ChronoDuration::days(10),
    )
}

fn write_chatgpt_auth_with_expiration(
    account_home: &Path,
    account_id: &str,
    email: &str,
    expires_at: i64,
    last_refresh: chrono::DateTime<Utc>,
) -> Result<()> {
    let id_token = fake_jwt(json!({
        "email": email,
        "exp": expires_at,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": account_id,
            "user_id": format!("user-{account_id}")
        }
    }))?;
    let auth = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: codex_login::token_data::parse_chatgpt_jwt_claims(&id_token)?,
            access_token: id_token,
            refresh_token: format!("refresh-{account_id}"),
            account_id: Some(account_id.to_string()),
        }),
        last_refresh: Some(last_refresh),
        agent_identity: None,
    };
    save_auth(account_home, &auth, AuthCredentialsStoreMode::File)?;
    Ok(())
}

fn write_api_key_auth(account_home: &Path) -> Result<()> {
    std::fs::create_dir_all(account_home)?;
    login_with_api_key(account_home, "sk-test-key", AuthCredentialsStoreMode::File)?;
    Ok(())
}

fn write_invalid_auth(account_home: &Path) -> Result<()> {
    std::fs::create_dir_all(account_home)?;
    std::fs::write(account_home.join("auth.json"), "{")?;
    Ok(())
}

fn fake_jwt(payload: serde_json::Value) -> Result<String> {
    let header = json!({"alg": "none", "typ": "JWT"});
    Ok(format!(
        "{}.{}.{}",
        base64_url_no_pad(&serde_json::to_vec(&header)?),
        base64_url_no_pad(&serde_json::to_vec(&payload)?),
        base64_url_no_pad(b"sig")
    ))
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let chunk = ((bytes[index] as u32) << 16)
            | ((bytes[index + 1] as u32) << 8)
            | bytes[index + 2] as u32;
        encoded.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        encoded.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        encoded.push(TABLE[(chunk & 0x3f) as usize] as char);
        index += 3;
    }
    match bytes.len() - index {
        1 => {
            let chunk = (bytes[index] as u32) << 16;
            encoded.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            encoded.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        }
        2 => {
            let chunk = ((bytes[index] as u32) << 16) | ((bytes[index + 1] as u32) << 8);
            encoded.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            encoded.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
            encoded.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        }
        0 => {}
        _ => unreachable!("base64 remainder is always 0, 1, or 2"),
    }
    encoded
}

fn account_pool_config(extra: &str) -> String {
    format!(
        r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro", "personal-pro"]
policy = "drain"
{extra}
"#
    )
}

#[test]
fn account_list_human_groups_pool_members_and_statuses() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;
    write_chatgpt_auth(codex_home.path(), "default", "default@example.com")?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("standalone"),
        "standalone",
        "standalone@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert_eq!(
        stdout,
        "Default account:\n  default: logged in\n\nPool codex-pro (default pool, provider openai, policy drain):\n  work-pro: logged in\n  personal-pro: missing\n\nStandalone accounts:\n  standalone: logged in\n"
    );

    Ok(())
}

#[test]
fn account_list_human_marks_invalid_pool_members() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(
        codex_home.path(),
        r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["api-key-pro", "corrupt-pro", "missing-pro"]
policy = "drain"
"#,
    )?;
    write_api_key_auth(&codex_home.path().join("accounts").join("api-key-pro"))?;
    write_invalid_auth(&codex_home.path().join("accounts").join("corrupt-pro"))?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert_eq!(
        stdout,
        "Pool codex-pro (default pool, provider openai, policy drain):\n  api-key-pro: invalid\n  corrupt-pro: invalid\n  missing-pro: missing\n"
    );

    Ok(())
}

#[test]
fn account_list_json_includes_pool_metadata_and_memberships() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;
    write_chatgpt_auth(codex_home.path(), "default", "default@example.com")?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("standalone"),
        "standalone",
        "standalone@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output)?;

    assert_eq!(
        value,
        json!({
            "accounts": [
                {
                    "id": "default",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
                    "pools": [],
                    "poolMembership": []
                },
                {
                    "id": "codex-pro",
                    "type": "pool",
                    "default": true,
                    "provider": "openai",
                    "policy": "drain",
                    "members": ["work-pro", "personal-pro"]
                },
                {
                    "id": "work-pro",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
                    "pools": ["codex-pro"],
                    "poolMembership": [
                        {
                            "poolId": "codex-pro",
                            "default": true,
                            "memberIndex": 0
                        }
                    ]
                },
                {
                    "id": "personal-pro",
                    "type": "account",
                    "credentialStatus": "missing",
                    "authMode": null,
                    "pools": ["codex-pro"],
                    "poolMembership": [
                        {
                            "poolId": "codex-pro",
                            "default": true,
                            "memberIndex": 1
                        }
                    ]
                },
                {
                    "id": "standalone",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
                    "pools": [],
                    "poolMembership": []
                }
            ],
            "pools": [
                {
                    "id": "codex-pro",
                    "default": true,
                    "provider": "openai",
                    "policy": "drain",
                    "memberIds": ["work-pro", "personal-pro"],
                    "members": [
                        {
                            "id": "work-pro",
                            "credentialStatus": "logged in",
                            "authMode": "chatgpt"
                        },
                        {
                            "id": "personal-pro",
                            "credentialStatus": "missing",
                            "authMode": null
                        }
                    ]
                }
            ]
        })
    );

    Ok(())
}

#[test]
fn account_limits_groups_pool_members_and_reports_missing_invalid_in_config_order() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(
        codex_home.path(),
        r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["api-key-pro", "corrupt-pro", "missing-pro"]
policy = "drain"
"#,
    )?;
    write_api_key_auth(&codex_home.path().join("accounts").join("api-key-pro"))?;
    write_invalid_auth(&codex_home.path().join("accounts").join("corrupt-pro"))?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "limits"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert_eq!(
        stdout,
        "codex-pro (default pool, drain): 3 members, 1 missing credential, 2 invalid credentials\n\napi-key-pro (pool: codex-pro)\n  credentials: invalid\n  limits: unavailable\n\ncorrupt-pro (pool: codex-pro)\n  credentials: invalid\n  limits: unavailable\n\nmissing-pro (pool: codex-pro)\n  credentials: missing\n  limits: unavailable\n"
    );

    Ok(())
}

#[test]
fn account_refresh_pool_reports_all_missing_credentials() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Refreshed 0/2 accounts in pool codex-pro; work-pro missing credentials; personal-pro missing credentials",
        ));

    Ok(())
}

#[test]
fn account_refresh_pool_reports_partial_success() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 1, |_request| {
        TestHttpResponse::json(
            /*status_code*/ 200,
            r#"{"rate_limit":{"primary_window":{"used_percent":25.0}}}"#,
        )
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            account_pool_config("")
        ),
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .success()
        .stderr(contains(
            "Refreshed 1/2 accounts in pool codex-pro; personal-pro missing credentials",
        ));
    let requests = server.finish()?;
    assert_eq!(requests.len(), 1);

    Ok(())
}

#[test]
fn account_refresh_pool_reports_blocked_member_and_succeeds_when_another_member_refreshes()
-> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 2, |request| {
        if request.header("ChatGPT-Account-ID").as_deref() == Some("personal-pro") {
            TestHttpResponse::text(/*status_code*/ 403, "account blocked")
        } else {
            TestHttpResponse::json(
                /*status_code*/ 200,
                r#"{"rate_limit":{"primary_window":{"used_percent":25.0}}}"#,
            )
        }
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            account_pool_config("")
        ),
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("personal-pro"),
        "personal-pro",
        "personal@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .success()
        .stderr(contains(
            "Refreshed 1/2 accounts in pool codex-pro; personal-pro failed to fetch codex usage: 403 Forbidden; body=account blocked",
        ));
    let requests = server.finish()?;
    assert_eq!(requests.len(), 2);

    Ok(())
}

#[test]
fn account_refresh_pool_fails_when_all_members_are_blocked() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 1, |_request| {
        TestHttpResponse::text(/*status_code*/ 403, "account blocked")
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro"]
policy = "drain"
"#
        ),
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Refreshed 0/1 accounts in pool codex-pro; work-pro failed to fetch codex usage: 403 Forbidden; body=account blocked",
        ));
    let requests = server.finish()?;
    assert_eq!(requests.len(), 1);

    Ok(())
}

#[test]
fn account_refresh_pool_fails_when_stale_credentials_cannot_refresh() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 2, |request| {
        if request.path == "/oauth/token" {
            TestHttpResponse::json(
                /*status_code*/ 401,
                r#"{"error":{"code":"refresh_token_expired"}}"#,
            )
        } else {
            TestHttpResponse::text(/*status_code*/ 401, "token expired")
        }
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro"]
policy = "drain"
"#
        ),
    )?;
    write_expired_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.env(
        codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/token", server.url),
    );
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Refreshed 0/1 accounts in pool codex-pro; work-pro failed to fetch codex usage: 401 Unauthorized; body=token expired",
        ));
    let requests = server.finish()?;
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.method.as_str(), request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![("POST", "/oauth/token"), ("GET", "/api/codex/usage")]
    );

    Ok(())
}

#[test]
fn account_refresh_named_account_fails_when_stale_credentials_cannot_refresh() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 1, |request| {
        if request.path == "/oauth/token" {
            TestHttpResponse::json(
                /*status_code*/ 401,
                r#"{"error":{"code":"refresh_token_expired"}}"#,
            )
        } else {
            TestHttpResponse::text(/*status_code*/ 500, "unexpected request")
        }
    })?;
    write_config(codex_home.path(), "")?;
    write_expired_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.env(
        codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/token", server.url),
    );
    cmd.args(["account", "refresh", "work-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Account work-pro invalid credentials: Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.",
        ));
    let requests = server.finish()?;
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.method.as_str(), request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![("POST", "/oauth/token")]
    );

    Ok(())
}

#[test]
fn account_refresh_pool_reports_missing_pool() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "missing"])
        .assert()
        .failure()
        .stderr(contains("Account pool missing not found"));

    Ok(())
}

#[test]
fn account_help_mentions_new_flags_and_arguments() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), "")?;

    let mut login_help = codex_command(codex_home.path())?;
    login_help
        .args(["login", "--help"])
        .assert()
        .success()
        .stdout(contains("--device-auth"));

    let mut list_help = codex_command(codex_home.path())?;
    list_help
        .args(["account", "list", "--help"])
        .assert()
        .success()
        .stdout(contains("--json"));

    let mut refresh_help = codex_command(codex_home.path())?;
    refresh_help
        .args(["account", "refresh", "--help"])
        .assert()
        .success()
        .stdout(contains("[ID]").and(contains("--pool <POOL_ID>")));

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestHttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl TestHttpRequest {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(key, _value)| key.eq_ignore_ascii_case(name))
            .map(|(_key, value)| value.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TestHttpResponse {
    status_code: u16,
    content_type: &'static str,
    body: String,
}

impl TestHttpResponse {
    fn json(status_code: u16, body: &str) -> Self {
        Self {
            status_code,
            content_type: "application/json",
            body: body.to_string(),
        }
    }

    fn text(status_code: u16, body: &str) -> Self {
        Self {
            status_code,
            content_type: "text/plain",
            body: body.to_string(),
        }
    }
}

#[derive(Debug)]
struct TestHttpServer {
    url: String,
    handle: thread::JoinHandle<()>,
    requests: Arc<Mutex<Vec<TestHttpRequest>>>,
}

impl TestHttpServer {
    fn finish(self) -> Result<Vec<TestHttpRequest>> {
        let requests = Arc::clone(&self.requests);
        self.handle
            .join()
            .map_err(|_| anyhow!("test HTTP server panicked"))?;
        let requests = requests
            .lock()
            .map_err(|_| anyhow!("test HTTP server requests lock poisoned"))?
            .clone();
        Ok(requests)
    }
}

fn start_http_server(
    expected_requests: usize,
    handler: impl Fn(&TestHttpRequest) -> TestHttpResponse + Send + Sync + 'static,
) -> Result<TestHttpServer> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let handler = Arc::new(handler);
    let requests: Arc<Mutex<Vec<TestHttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let thread_requests = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 30);
        let mut handled = 0usize;
        loop {
            if handled >= expected_requests {
                return;
            }
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    let mut request = [0; 4096];
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let read = stream.read(&mut request).unwrap_or(0);
                    let request = parse_http_request(&request[..read]);
                    if let Ok(mut requests) = thread_requests.lock() {
                        requests.push(request.clone());
                    }
                    let response = handler(&request);
                    let response = format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response.status_code,
                        reason_phrase(response.status_code),
                        response.content_type,
                        response.body.len(),
                        response.body
                    );
                    if stream.write_all(response.as_bytes()).is_err() {
                        return;
                    }
                    handled += 1;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    });
    Ok(TestHttpServer {
        url: format!("http://{addr}"),
        handle,
        requests,
    })
}

fn parse_http_request(bytes: &[u8]) -> TestHttpRequest {
    let request = String::from_utf8_lossy(bytes);
    let mut lines = request.lines();
    let (method, path) = lines
        .next()
        .and_then(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .unwrap_or_else(|| (String::new(), String::new()));
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    TestHttpRequest {
        method,
        path,
        headers,
    }
}

fn reason_phrase(status_code: u16) -> &'static str {
    match status_code {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        500 => "Internal Server Error",
        _ => "Test Status",
    }
}
