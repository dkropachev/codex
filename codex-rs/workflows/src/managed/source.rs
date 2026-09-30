use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::normalize_windows_device_path;
use std::ffi::OsStr;
use std::net::Ipv6Addr;
use std::path::Path;
use url::Url;

const MAX_SOURCE_LEN: usize = 4096;
const INVALID_SOURCE: &str = "invalid managed workflow Git source";

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkflowGitSourceKind {
    Local(AbsolutePathBuf),
    Https(String),
    Ssh(String),
}

/// A validated local, HTTPS, or SSH source for a managed workflow repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowGitSource(WorkflowGitSourceKind);

impl WorkflowGitSource {
    /// Parses and validates a Git source before it is passed to Git.
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(
            input.len() <= MAX_SOURCE_LEN && !input.chars().any(char::is_control),
            INVALID_SOURCE
        );
        let source = input.trim();
        let remote_helper = source.split_once("::").is_some_and(|(transport, _)| {
            !transport.is_empty()
                && !transport
                    .bytes()
                    .any(|byte| b"/\\:@[]".contains(&byte) || byte.is_ascii_whitespace())
        });
        ensure!(
            !source.is_empty() && !remote_helper && !is_network_path(source),
            INVALID_SOURCE
        );

        let windows_drive = matches!(
            source.as_bytes(),
            [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic()
        );
        if windows_drive {
            return canonical_local(Path::new(source))
                .map(|path| Self(WorkflowGitSourceKind::Local(path)));
        }
        if !source.contains("://")
            && let Ok(path) = canonical_local(Path::new(source))
        {
            return Ok(Self(WorkflowGitSourceKind::Local(path)));
        }
        if source.contains("://") {
            Self::parse_url(source)
        } else {
            Self::parse_scp(source)
        }
    }

    /// Returns the canonical local path or validated remote spelling for Git.
    pub fn as_os_str(&self) -> &OsStr {
        match self {
            Self(WorkflowGitSourceKind::Local(path)) => path.as_path().as_os_str(),
            Self(WorkflowGitSourceKind::Https(source) | WorkflowGitSourceKind::Ssh(source)) => {
                OsStr::new(source)
            }
        }
    }

    fn parse_url(source: &str) -> Result<Self> {
        ensure!(
            !source.chars().any(char::is_whitespace) && !source.contains('\\'),
            INVALID_SOURCE
        );
        let raw_authority = source
            .split_once("://")
            .map_or("", |(_, rest)| rest.split('/').next().unwrap_or_default());
        let raw_userinfo = raw_authority.contains('@');
        let url = Url::parse(source).map_err(|_| anyhow!(INVALID_SOURCE))?;
        ensure!(
            url.query().is_none() && url.fragment().is_none(),
            INVALID_SOURCE
        );

        match url.scheme() {
            "file" => {
                ensure!(
                    !raw_userinfo && url.host_str().is_none() && url.port().is_none(),
                    INVALID_SOURCE
                );
                let path = url.to_file_path().map_err(|()| anyhow!(INVALID_SOURCE))?;
                ensure!(!path.to_str().is_some_and(is_network_path), INVALID_SOURCE);
                Ok(Self(WorkflowGitSourceKind::Local(canonical_local(&path)?)))
            }
            "https" | "ssh" => {
                let url_host_valid = match url.host() {
                    Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => true,
                    Some(url::Host::Domain(host)) => valid_host(host),
                    None => false,
                };
                ensure!(
                    !raw_authority.is_empty()
                        && url_host_valid
                        && !url.path().trim_matches('/').is_empty(),
                    INVALID_SOURCE
                );
                let invalid_auth = if url.scheme() == "https" {
                    raw_userinfo
                } else {
                    url.password().is_some()
                        || raw_userinfo && url.username().is_empty()
                        || !url.username().is_empty() && !valid_user(url.username())
                };
                ensure!(!invalid_auth, INVALID_SOURCE);
                let kind = if url.scheme() == "https" {
                    WorkflowGitSourceKind::Https(source.to_string())
                } else {
                    WorkflowGitSourceKind::Ssh(source.to_string())
                };
                Ok(Self(kind))
            }
            _ => Err(anyhow!(INVALID_SOURCE)),
        }
    }

    fn parse_scp(source: &str) -> Result<Self> {
        ensure!(
            !source.chars().any(char::is_whitespace) && !source.contains(['?', '#']),
            INVALID_SOURCE
        );
        let embedded_password = source
            .split_once('@')
            .is_some_and(|(user, host)| user.contains(':') && host.contains(':'));
        ensure!(!embedded_password, INVALID_SOURCE);
        let authority_and_path = source
            .find("]:")
            .map(|index| (&source[..=index], &source[index + 2..]))
            .or_else(|| source.split_once(':'));
        let (authority, path) = authority_and_path.ok_or_else(|| anyhow!(INVALID_SOURCE))?;
        ensure!(!path.trim_matches('/').is_empty(), INVALID_SOURCE);
        let (user, host) = match authority.split_once('@') {
            Some((user, host)) if !user.contains('@') => (Some(user), host),
            Some(_) => return Err(anyhow!(INVALID_SOURCE)),
            None => (None, authority),
        };
        ensure!(
            !user.is_some_and(|user| !valid_user(user)) && valid_host(host),
            INVALID_SOURCE
        );
        Ok(Self(WorkflowGitSourceKind::Ssh(source.to_string())))
    }
}

fn canonical_local(path: &Path) -> Result<AbsolutePathBuf> {
    let path = AbsolutePathBuf::relative_to_current_dir(path)
        .and_then(|path| path.canonicalize())
        .map_err(|_| anyhow!(INVALID_SOURCE))?;
    ensure!(path.as_path().to_str().is_some(), INVALID_SOURCE);
    Ok(path)
}

fn is_network_path(path: &str) -> bool {
    path.starts_with("//")
        || path.starts_with(r"\\")
            && normalize_windows_device_path(path).is_none_or(|path| path.starts_with(r"\\"))
}

fn valid_user(user: &str) -> bool {
    user.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
}

fn valid_host(host: &str) -> bool {
    if let Some(ipv6) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return ipv6.parse::<Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
