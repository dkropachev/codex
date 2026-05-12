use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

const WORKFLOW_MENTION_PREFIX: &str = "workflow://";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkflowIdError {
    #[error("workflow id must not be empty")]
    Empty,
    #[error("workflow id must be a relative path: {0}")]
    Absolute(String),
    #[error("workflow id must not contain '.' or '..': {0}")]
    DotSegment(String),
    #[error("workflow id must use '/' separators: {0}")]
    Backslash(String),
    #[error("workflow id contains a non-UTF-8 path component")]
    NonUtf8,
    #[error("workflow target is invalid: {0}")]
    InvalidTarget(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTarget {
    pub root_path: PathBuf,
    pub id: String,
    pub path: PathBuf,
}

impl WorkflowTarget {
    pub fn new(root_path: PathBuf, id: String) -> Result<Self, WorkflowIdError> {
        let id = normalize_workflow_id(&id)?;
        let path = workflow_path(&root_path, &id)?;
        Ok(Self {
            root_path,
            id,
            path,
        })
    }
}

pub fn normalize_workflow_id(raw: &str) -> Result<String, WorkflowIdError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkflowIdError::Empty);
    }
    if trimmed.contains('\\') {
        return Err(WorkflowIdError::Backslash(raw.to_string()));
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(WorkflowIdError::Absolute(raw.to_string()));
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or(WorkflowIdError::NonUtf8)?;
                components.push(value.to_string());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(WorkflowIdError::DotSegment(raw.to_string()));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(WorkflowIdError::Absolute(raw.to_string()));
            }
        }
    }

    if components.is_empty() {
        Err(WorkflowIdError::Empty)
    } else {
        Ok(components.join("/"))
    }
}

pub fn workflow_path(root: &Path, id: &str) -> Result<PathBuf, WorkflowIdError> {
    let normalized = normalize_workflow_id(id)?;
    let mut path = root.to_path_buf();
    for component in normalized.split('/') {
        path.push(component);
    }
    Ok(path)
}

pub fn mention_target(root_path: &Path, id: &str) -> Result<String, WorkflowIdError> {
    let id = normalize_workflow_id(id)?;
    Ok(format!(
        "{WORKFLOW_MENTION_PREFIX}{}?root={}",
        percent_encode(&id),
        percent_encode(&root_path.to_string_lossy())
    ))
}

pub fn parse_mention_target(target: &str) -> Result<WorkflowTarget, WorkflowIdError> {
    let Some(rest) = target.strip_prefix(WORKFLOW_MENTION_PREFIX) else {
        return Err(WorkflowIdError::InvalidTarget(target.to_string()));
    };
    let Some((id, root)) = rest.split_once("?root=") else {
        return Err(WorkflowIdError::InvalidTarget(target.to_string()));
    };
    let id = percent_decode(id)?;
    let root_path = PathBuf::from(percent_decode(root)?);
    if !root_path.is_absolute() {
        return Err(WorkflowIdError::InvalidTarget(target.to_string()));
    }
    WorkflowTarget::new(root_path, id)
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Result<String, WorkflowIdError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(WorkflowIdError::InvalidTarget(value.to_string()));
            }
            let hi = hex_value(bytes[index + 1])?;
            let lo = hex_value(bytes[index + 2])?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| WorkflowIdError::InvalidTarget(value.to_string()))
}

fn hex_value(byte: u8) -> Result<u8, WorkflowIdError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(WorkflowIdError::InvalidTarget(char::from(byte).to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn normalizes_workflow_ids() {
        assert_eq!(
            normalize_workflow_id("reports/jira-summary/").unwrap(),
            "reports/jira-summary"
        );
        assert_eq!(
            normalize_workflow_id("reports//jira").unwrap(),
            "reports/jira"
        );
    }

    #[test]
    fn rejects_unsafe_workflow_ids() {
        assert_eq!(normalize_workflow_id(""), Err(WorkflowIdError::Empty));
        assert!(matches!(
            normalize_workflow_id("../x"),
            Err(WorkflowIdError::DotSegment(_))
        ));
        assert!(matches!(
            normalize_workflow_id("/reports/jira"),
            Err(WorkflowIdError::Absolute(_))
        ));
        assert!(matches!(
            normalize_workflow_id("reports\\jira"),
            Err(WorkflowIdError::Backslash(_))
        ));
    }

    #[test]
    fn mention_targets_round_trip_root_and_id() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("codex workflows");
        let target = mention_target(&root, "reports/jira-summary").unwrap();
        assert_eq!(
            parse_mention_target(&target).unwrap(),
            WorkflowTarget {
                root_path: root.clone(),
                id: "reports/jira-summary".to_string(),
                path: root.join("reports").join("jira-summary"),
            }
        );
    }
}
