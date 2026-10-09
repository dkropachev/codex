use anyhow::Context;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use super::fs::SecureDirectory;
use super::receipt::ManagedWorkflowReceipt;
use super::receipt::WorkflowRelease;
use super::receipt::validate_id;
use crate::managed::integrity::ActivationPayloadEvidence;

const JOURNAL_SCHEMA_VERSION: u32 = 1;
const MARKER_SCHEMA_VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: usize = 160 * 1024;
const MAX_MARKER_BYTES: usize = 8 * 1024;
const MAX_TRANSACTION_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ManagedWorkflowOperation {
    Install,
    Replace,
    Uninstall,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ManagedWorkflowNextAction {
    MoveCurrentAside,
    PublishRelease,
    WriteReceipt,
    RemoveReceipt,
    Cleanup,
}

/// One durable transaction record. The next action is advanced only after each step is durable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ManagedWorkflowJournal {
    pub(super) schema_version: u32,
    pub(super) transaction_id: String,
    pub(super) id: String,
    pub(super) operation: ManagedWorkflowOperation,
    pub(super) previous_receipt: Option<ManagedWorkflowReceipt>,
    pub(super) next_receipt: ManagedWorkflowReceipt,
    pub(super) evidence: ActivationPayloadEvidence,
    pub(super) next_action: ManagedWorkflowNextAction,
}

/// Marker inside a published release; ties the active directory to one transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ManagedWorkflowMarker {
    pub(super) schema_version: u32,
    pub(super) id: String,
    pub(super) transaction_id: String,
    pub(super) release: WorkflowRelease,
    pub(super) evidence_digest: String,
}

impl ManagedWorkflowJournal {
    pub(super) fn new(
        transaction_id: String,
        id: String,
        previous_receipt: Option<ManagedWorkflowReceipt>,
        next_receipt: ManagedWorkflowReceipt,
        evidence: ActivationPayloadEvidence,
    ) -> anyhow::Result<Self> {
        let operation = if previous_receipt.is_some() {
            ManagedWorkflowOperation::Replace
        } else {
            ManagedWorkflowOperation::Install
        };
        let next_action = if previous_receipt.is_some() {
            ManagedWorkflowNextAction::MoveCurrentAside
        } else {
            ManagedWorkflowNextAction::PublishRelease
        };
        let journal = Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            transaction_id,
            id,
            operation,
            previous_receipt,
            next_receipt,
            evidence,
            next_action,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub(super) fn new_uninstall(
        transaction_id: String,
        previous_receipt: ManagedWorkflowReceipt,
        evidence: ActivationPayloadEvidence,
    ) -> anyhow::Result<Self> {
        let journal = Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            transaction_id,
            id: previous_receipt.id.clone(),
            operation: ManagedWorkflowOperation::Uninstall,
            previous_receipt: Some(previous_receipt.clone()),
            next_receipt: previous_receipt,
            evidence,
            next_action: ManagedWorkflowNextAction::MoveCurrentAside,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION {
            bail!("unsupported managed workflow journal schema version");
        }
        validate_id(&self.id)?;
        validate_transaction_id(&self.transaction_id)?;
        self.next_receipt.serialized_bytes()?;
        if self.next_receipt.id != self.id {
            bail!("managed workflow journal next receipt id does not match transaction id");
        }
        match (&self.operation, &self.previous_receipt) {
            (ManagedWorkflowOperation::Install, None) => {
                if !matches!(
                    self.next_action,
                    ManagedWorkflowNextAction::PublishRelease
                        | ManagedWorkflowNextAction::WriteReceipt
                        | ManagedWorkflowNextAction::Cleanup
                ) {
                    bail!("fresh install has an invalid next action");
                }
            }
            (ManagedWorkflowOperation::Replace, Some(previous)) => {
                previous.serialized_bytes()?;
                if previous.id != self.id {
                    bail!("managed workflow journal previous receipt id does not match");
                }
                if self.next_action == ManagedWorkflowNextAction::RemoveReceipt {
                    bail!("replacement cannot remove its receipt");
                }
            }
            (ManagedWorkflowOperation::Uninstall, Some(previous)) => {
                previous.serialized_bytes()?;
                if previous.id != self.id || self.next_receipt != *previous {
                    bail!("uninstall journal does not match previous receipt");
                }
                if !matches!(
                    self.next_action,
                    ManagedWorkflowNextAction::MoveCurrentAside
                        | ManagedWorkflowNextAction::RemoveReceipt
                        | ManagedWorkflowNextAction::Cleanup
                ) {
                    bail!("uninstall journal has an invalid next action");
                }
            }
            (ManagedWorkflowOperation::Install, Some(_))
            | (ManagedWorkflowOperation::Replace, None)
            | (ManagedWorkflowOperation::Uninstall, None) => {
                bail!("managed workflow journal operation and previous receipt disagree");
            }
        }
        validate_evidence(&self.evidence)?;
        self.marker().validate()?;
        Ok(())
    }

    pub(super) fn marker(&self) -> ManagedWorkflowMarker {
        ManagedWorkflowMarker {
            schema_version: MARKER_SCHEMA_VERSION,
            id: self.id.clone(),
            transaction_id: self.transaction_id.clone(),
            release: self.next_receipt.installed.clone(),
            evidence_digest: self.evidence.sha256.clone(),
        }
    }
}

impl ManagedWorkflowMarker {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != MARKER_SCHEMA_VERSION {
            bail!("unsupported managed workflow marker schema version");
        }
        validate_id(&self.id)?;
        validate_transaction_id(&self.transaction_id)?;
        self.release.validate()?;
        validate_sha256(&self.evidence_digest)?;
        if serialize_marker(self)?.len() > MAX_MARKER_BYTES {
            bail!("managed workflow marker exceeds its size limit");
        }
        Ok(())
    }

    pub(super) fn matches_journal(&self, journal: &ManagedWorkflowJournal) -> bool {
        self == &journal.marker()
    }
}

fn validate_transaction_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > MAX_TRANSACTION_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("managed workflow transaction id is invalid");
    }
    Ok(())
}

fn validate_sha256(digest: &str) -> anyhow::Result<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("managed workflow evidence digest is invalid");
    }
    Ok(())
}

fn validate_evidence(evidence: &ActivationPayloadEvidence) -> anyhow::Result<()> {
    if evidence.format_version != 1
        || evidence.entry_count > 250_000
        || evidence.logical_bytes > 2 * 1024 * 1024 * 1024
    {
        bail!("managed workflow payload evidence is invalid");
    }
    validate_sha256(&evidence.sha256)
}

pub(super) fn journal_file_name(id: &str) -> anyhow::Result<String> {
    validate_id(id)?;
    let digest = Sha256::digest(id.as_bytes());
    Ok(format!("{digest:x}.json"))
}

pub(super) fn write_journal(
    journals: &SecureDirectory,
    journal: &ManagedWorkflowJournal,
    replace: bool,
) -> anyhow::Result<()> {
    journal.validate()?;
    let mut bytes = serde_json::to_vec_pretty(journal)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_JOURNAL_BYTES {
        bail!("managed workflow journal exceeds its size limit");
    }
    journals.write_file(&journal_file_name(&journal.id)?, &bytes, replace)
}

pub(super) fn read_journal(
    journals: &SecureDirectory,
    id: &str,
) -> anyhow::Result<ManagedWorkflowJournal> {
    let journal = read_journal_named(journals, &journal_file_name(id)?)?;
    if journal.id != id {
        bail!("managed workflow journal id does not match requested workflow");
    }
    Ok(journal)
}

pub(super) fn read_journal_named(
    journals: &SecureDirectory,
    name: &str,
) -> anyhow::Result<ManagedWorkflowJournal> {
    if name.len() != 69
        || !name.ends_with(".json")
        || !name.as_bytes()[..64]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("managed workflow journal file name is invalid");
    }
    let bytes = journals.read_file(name, MAX_JOURNAL_BYTES as u64)?;
    let journal = serde_json::from_slice::<ManagedWorkflowJournal>(&bytes)
        .context("failed to parse managed workflow journal")?;
    journal.validate()?;
    if journal_file_name(&journal.id)? != name {
        bail!("managed workflow journal id does not match its file name");
    }
    Ok(journal)
}

pub(super) fn write_marker(
    target: &SecureDirectory,
    marker: &ManagedWorkflowMarker,
) -> anyhow::Result<()> {
    marker.validate()?;
    let bytes = serialize_marker(marker)?;
    target.write_file("codex-managed-workflow", &bytes, /*replace*/ false)
}

fn serialize_marker(marker: &ManagedWorkflowMarker) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(marker)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn read_marker(target: &SecureDirectory) -> anyhow::Result<ManagedWorkflowMarker> {
    let bytes = target.read_file("codex-managed-workflow", MAX_MARKER_BYTES as u64)?;
    let marker = serde_json::from_slice::<ManagedWorkflowMarker>(&bytes)
        .context("failed to parse managed workflow marker")?;
    marker.validate()?;
    Ok(marker)
}

#[cfg(all(test, unix))]
#[path = "journal_tests.rs"]
mod tests;
