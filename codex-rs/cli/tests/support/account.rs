use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
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
use serde_json::json;

pub(crate) fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.env("no_proxy", "127.0.0.1,localhost");
    Ok(cmd)
}

pub(crate) fn write_config(codex_home: &Path, extra: &str) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!("cli_auth_credentials_store = \"file\"\n{extra}"),
    )?;
    Ok(())
}

pub(crate) fn write_chatgpt_auth(account_home: &Path, account_id: &str, email: &str) -> Result<()> {
    write_chatgpt_auth_with_expiration(
        account_home,
        account_id,
        email,
        Utc::now().timestamp() + 3600,
        Utc::now(),
    )
}

pub(crate) fn write_expired_chatgpt_auth(
    account_home: &Path,
    account_id: &str,
    email: &str,
) -> Result<()> {
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
        personal_access_token: None,
    };
    save_auth(account_home, &auth, AuthCredentialsStoreMode::File)?;
    Ok(())
}

pub(crate) fn write_api_key_auth(account_home: &Path) -> Result<()> {
    std::fs::create_dir_all(account_home)?;
    login_with_api_key(account_home, "sk-test-key", AuthCredentialsStoreMode::File)?;
    Ok(())
}

pub(crate) fn write_invalid_auth(account_home: &Path) -> Result<()> {
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

pub(crate) fn account_pool_config(extra: &str) -> String {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestHttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    headers: Vec<(String, String)>,
}

impl TestHttpRequest {
    pub(crate) fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(key, _value)| key.eq_ignore_ascii_case(name))
            .map(|(_key, value)| value.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestHttpResponse {
    status_code: u16,
    content_type: &'static str,
    body: String,
}

impl TestHttpResponse {
    pub(crate) fn json(status_code: u16, body: &str) -> Self {
        Self {
            status_code,
            content_type: "application/json",
            body: body.to_string(),
        }
    }

    pub(crate) fn text(status_code: u16, body: &str) -> Self {
        Self {
            status_code,
            content_type: "text/plain",
            body: body.to_string(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct TestHttpServer {
    pub(crate) url: String,
    handle: thread::JoinHandle<()>,
    requests: Arc<Mutex<Vec<TestHttpRequest>>>,
}

impl TestHttpServer {
    pub(crate) fn finish(self) -> Result<Vec<TestHttpRequest>> {
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

pub(crate) fn start_http_server(
    expected_requests: usize,
    handler: impl Fn(&TestHttpRequest) -> TestHttpResponse + Send + Sync + 'static,
) -> Result<TestHttpServer> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let handler = Arc::new(handler);
    let requests: Arc<Mutex<Vec<TestHttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let thread_requests = Arc::clone(&requests);
    let thread_handled_requests = Arc::new(AtomicUsize::new(0));
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 30);
        let mut connection_handles = Vec::new();
        loop {
            if thread_handled_requests.load(Ordering::SeqCst) >= expected_requests {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    let connection_requests = Arc::clone(&thread_requests);
                    let connection_handler = Arc::clone(&handler);
                    let connection_handled_requests = Arc::clone(&thread_handled_requests);
                    connection_handles.push(thread::spawn(move || {
                        let _ = stream.set_nonblocking(/*nonblocking*/ false);
                        let mut request = Vec::new();
                        let mut buffer = [0; 1024];
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        while request.len() < 8192 {
                            match stream.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(read) => {
                                    request.extend_from_slice(&buffer[..read]);
                                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(err)
                                    if matches!(
                                        err.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    break;
                                }
                                Err(_) => return,
                            }
                        }
                        let request = parse_http_request(&request);
                        if request.method.is_empty() || request.path.is_empty() {
                            return;
                        }
                        if let Ok(mut requests) = connection_requests.lock() {
                            requests.push(request.clone());
                        }
                        let response = connection_handler(&request);
                        let response = format!(
                            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response.status_code,
                            reason_phrase(response.status_code),
                            response.content_type,
                            response.body.len(),
                            response.body
                        );
                        if stream
                            .write_all(response.as_bytes())
                            .and_then(|_| stream.flush())
                            .is_err()
                        {
                            return;
                        }
                        connection_handled_requests.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.shutdown(Shutdown::Write);
                        let _ =
                            stream.set_read_timeout(Some(Duration::from_millis(/*millis*/ 20)));
                        let drain_deadline =
                            Instant::now() + Duration::from_millis(/*millis*/ 100);
                        loop {
                            match stream.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(err)
                                    if matches!(
                                        err.kind(),
                                        std::io::ErrorKind::TimedOut
                                            | std::io::ErrorKind::WouldBlock
                                    ) =>
                                {
                                    if Instant::now() >= drain_deadline {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        for connection_handle in connection_handles {
            assert!(
                connection_handle.join().is_ok(),
                "test HTTP connection handler panicked"
            );
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
