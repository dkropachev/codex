use std::sync::atomic::AtomicBool;

use anyhow::bail;

use super::LockedManagedWorkflow;
use super::copy::copy_verified_payload;
use super::fs::SecureDirectory;
use super::journal::ManagedWorkflowJournal;
use super::journal::write_marker;
use super::receipt::ManagedWorkflowReceipt;
use super::stage::TransactionStaging;
use crate::managed::fetch::VerificationLimits;
use crate::managed::integrity::VerifiedWorkflowRelease;
use crate::managed::integrity::verify_materialized_copy;

/// Operation-private copied payload, validated before a journal can make it visible.
pub(in crate::managed) struct PreparedWorkflowRelease<'a> {
    pub(super) staging: TransactionStaging<'a>,
    pub(super) journal: ManagedWorkflowJournal,
}

#[expect(
    clippy::too_many_arguments,
    reason = "keep transaction inputs explicit across the staging boundary"
)]
pub(super) fn prepare_release<'a>(
    staging_root: &'a SecureDirectory,
    locked: &LockedManagedWorkflow,
    verified: VerifiedWorkflowRelease,
    previous_receipt: Option<ManagedWorkflowReceipt>,
    next_receipt: ManagedWorkflowReceipt,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<PreparedWorkflowRelease<'a>> {
    deadline.check(cancelled)?;
    let (staged, evidence) = verified.into_staged_and_evidence();
    let package = crate::WorkflowPackage::load(staged.root().as_path())?;
    if package.manifest.id != locked.id || next_receipt.id != locked.id {
        bail!("verified workflow id does not match locked installation target");
    }
    if next_receipt.source != staged.source().receipt_source()? {
        bail!("next receipt source does not match the fetched workflow source");
    }
    if next_receipt.installed.tag != staged.release().tag
        || next_receipt.installed.version.as_deref()
            != staged
                .release()
                .version
                .as_ref()
                .map(semver::Version::to_string)
                .as_deref()
        || !next_receipt
            .installed
            .commit
            .eq_ignore_ascii_case(&staged.release().advertised_object_id)
    {
        bail!("next receipt does not match the verified workflow release");
    }
    let staging = TransactionStaging::create(staging_root)?;
    let transaction_id = staging.name().to_owned();
    copy_verified_payload(
        staged.root(),
        staging.directory(),
        limits,
        deadline,
        cancelled,
    )?;
    let payload = staging.directory().existing_child("payload")?;
    verify_materialized_copy(
        payload.path().as_path(),
        &evidence,
        limits,
        deadline,
        cancelled,
    )?;
    let journal = ManagedWorkflowJournal::new(
        transaction_id,
        locked.id.clone(),
        previous_receipt,
        next_receipt,
        evidence,
    )?;
    write_marker(&payload, &journal.marker())?;
    deadline.check(cancelled)?;
    Ok(PreparedWorkflowRelease { staging, journal })
}

#[cfg(all(test, unix))]
#[path = "prepare_tests.rs"]
mod tests;
