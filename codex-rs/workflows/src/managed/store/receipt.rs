use anyhow::Context;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;

use super::fs::SecureDirectory;

const RECEIPT_SCHEMA_VERSION: u32 = 1;
pub(super) const MAX_RECEIPT_BYTES: usize = 64 * 1024;
const MAX_SOURCE_BYTES: usize = 8 * 1024;
const MAX_ID_BYTES: usize = 240;
const MAX_ID_COMPONENTS: usize = 32;

/// A stable release identity persisted in receipt v1.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::managed) struct WorkflowRelease {
    pub(in crate::managed) tag: Option<String>,
    pub(in crate::managed) version: Option<String>,
    pub(in crate::managed) commit: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::managed) enum WorkflowUpdatePolicy {
    Prompt,
    Automatic,
    Manual,
}

/// The historical on-disk receipt v1 contract. Payload evidence lives elsewhere.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::managed) struct ManagedWorkflowReceipt {
    pub(in crate::managed) schema_version: u32,
    pub(in crate::managed) id: String,
    pub(in crate::managed) source: String,
    pub(in crate::managed) installed: WorkflowRelease,
    pub(in crate::managed) policy: WorkflowUpdatePolicy,
    pub(in crate::managed) dismissed_release: Option<WorkflowRelease>,
}

impl ManagedWorkflowReceipt {
    pub(in crate::managed) fn new(
        id: String,
        source: String,
        installed: WorkflowRelease,
        policy: WorkflowUpdatePolicy,
    ) -> anyhow::Result<Self> {
        let receipt = Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            id,
            source,
            installed,
            policy,
            dismissed_release: None,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != RECEIPT_SCHEMA_VERSION {
            bail!("unsupported managed workflow receipt schema version");
        }
        validate_id(&self.id)?;
        validate_source(&self.source)?;
        self.installed.validate()?;
        if let Some(dismissed) = &self.dismissed_release {
            dismissed.validate()?;
        }
        Ok(())
    }

    pub(super) fn serialized_bytes(&self) -> anyhow::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECEIPT_BYTES {
            bail!("managed workflow receipt exceeds its size limit");
        }
        Ok(bytes)
    }
}

impl WorkflowRelease {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if !matches!(self.commit.len(), 40 | 64)
            || !self.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.commit.bytes().all(|byte| byte == b'0')
        {
            bail!("managed workflow receipt has an invalid release commit");
        }
        match (&self.tag, &self.version) {
            (Some(tag), Some(version)) => {
                let expected = semver::Version::parse(version)
                    .context("managed workflow receipt has an invalid SemVer version")?;
                if !expected.pre.is_empty() || tag != version && tag != &format!("v{version}") {
                    bail!("managed workflow receipt release tag and version do not match");
                }
            }
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                bail!("managed workflow receipt release requires both tag and version");
            }
        }
        Ok(())
    }
}

pub(super) fn validate_id(id: &str) -> anyhow::Result<()> {
    if id.len() > MAX_ID_BYTES || id.split('/').count() > MAX_ID_COMPONENTS {
        bail!("managed workflow id exceeds its receipt limit");
    }
    let normalized = crate::scaffold::normalize_workflow_id(id)?;
    if normalized != id {
        bail!("managed workflow receipt id must be normalized");
    }
    Ok(())
}

fn validate_source(source: &str) -> anyhow::Result<()> {
    if source.is_empty()
        || source.len() > MAX_SOURCE_BYTES
        || source.trim() != source
        || source.chars().any(char::is_control)
        || source.starts_with(['/', '\\'])
        || source.starts_with("file:")
        || matches!(source.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
    {
        bail!("managed workflow receipt source is unsafe");
    }
    if source.contains("://") || source.contains('@') || source.contains(':') {
        crate::managed::WorkflowGitSource::parse(source)
            .context("managed workflow receipt source is invalid")?;
    } else {
        crate::managed::fetch::portable_path(source)?;
    }
    Ok(())
}

pub(super) fn read_receipt(
    receipts: &SecureDirectory,
    id: &str,
) -> anyhow::Result<ManagedWorkflowReceipt> {
    let directory = receipt_directory(receipts, id, ReceiptDirectoryMode::Existing)?;
    let bytes = directory.read_file("receipt.json", MAX_RECEIPT_BYTES as u64)?;
    parse_receipt(&bytes, id)
}

fn parse_receipt(bytes: &[u8], id: &str) -> anyhow::Result<ManagedWorkflowReceipt> {
    let receipt = serde_json::from_slice::<ManagedWorkflowReceipt>(bytes)
        .context("failed to parse managed workflow receipt")?;
    receipt.validate()?;
    if receipt.id != id {
        bail!("managed workflow receipt id does not match its directory");
    }
    Ok(receipt)
}

#[cfg(unix)]
pub(super) fn read_optional_receipt(
    receipts: &SecureDirectory,
    id: &str,
) -> anyhow::Result<Option<ManagedWorkflowReceipt>> {
    read_optional_receipt_with_mode(receipts, id, MissingReceiptFile::Corrupt)
}

#[cfg(unix)]
pub(super) fn read_pending_receipt(
    receipts: &SecureDirectory,
    id: &str,
) -> anyhow::Result<Option<ManagedWorkflowReceipt>> {
    read_optional_receipt_with_mode(receipts, id, MissingReceiptFile::PendingTransaction)
}

#[cfg(unix)]
enum MissingReceiptFile {
    Corrupt,
    PendingTransaction,
}

#[cfg(unix)]
fn read_optional_receipt_with_mode(
    receipts: &SecureDirectory,
    id: &str,
    missing: MissingReceiptFile,
) -> anyhow::Result<Option<ManagedWorkflowReceipt>> {
    use rustix::fs::AtFlags;
    use rustix::fs::statat;
    use rustix::io::Errno;

    validate_id(id)?;
    let mut directory = None;
    for component in id.split('/') {
        let parent = directory.as_ref().unwrap_or(receipts);
        directory = match parent.optional_existing_child(component)? {
            Some(child) => Some(child),
            None => return Ok(None),
        };
    }
    let directory = directory.context("receipt id is empty")?;
    match statat(
        directory.handle(),
        "receipt.json",
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => {
            let bytes = directory.read_file("receipt.json", MAX_RECEIPT_BYTES as u64)?;
            parse_receipt(&bytes, id).map(Some)
        }
        Err(Errno::NOENT) => match missing {
            MissingReceiptFile::Corrupt => {
                bail!("managed workflow receipt directory has no receipt file")
            }
            MissingReceiptFile::PendingTransaction => Ok(None),
        },
        Err(error) => Err(error).context("failed to inspect managed workflow receipt"),
    }
}

pub(super) fn write_receipt(
    receipts: &SecureDirectory,
    receipt: &ManagedWorkflowReceipt,
    replace: bool,
) -> anyhow::Result<()> {
    let bytes = receipt.serialized_bytes()?;
    let directory = receipt_directory(receipts, &receipt.id, ReceiptDirectoryMode::Create)?;
    directory.write_file("receipt.json", &bytes, replace)
}

enum ReceiptDirectoryMode {
    Existing,
    Create,
}

fn receipt_directory(
    receipts: &SecureDirectory,
    id: &str,
    mode: ReceiptDirectoryMode,
) -> anyhow::Result<SecureDirectory> {
    validate_id(id)?;
    let mut directory = None;
    for component in id.split('/') {
        directory = Some(match (directory.as_ref().unwrap_or(receipts), &mode) {
            (parent, ReceiptDirectoryMode::Create) => parent.child(component)?,
            (parent, ReceiptDirectoryMode::Existing) => parent.existing_child(component)?,
        });
    }
    directory.context("receipt id is empty")
}

#[cfg(all(test, unix))]
#[path = "receipt_tests.rs"]
mod tests;
