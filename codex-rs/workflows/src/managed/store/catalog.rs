use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::bail;

use super::fs::SecureDirectory;
use super::receipt::MAX_RECEIPT_BYTES;
use super::receipt::ManagedWorkflowReceipt;

const MAX_RECEIPTS: usize = 1_024;
const MAX_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_ID_COMPONENTS: usize = 32;

#[derive(Clone, Copy)]
struct CatalogLimits {
    receipts: usize,
    entries: usize,
}

const CATALOG_LIMITS: CatalogLimits = CatalogLimits {
    receipts: MAX_RECEIPTS,
    entries: MAX_DIRECTORY_ENTRIES,
};

pub(super) fn collect_receipts(
    receipts: &SecureDirectory,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<Vec<ManagedWorkflowReceipt>> {
    collect_receipts_with_limits(receipts, CATALOG_LIMITS, cancelled)
}

fn collect_receipts_with_limits(
    receipts: &SecureDirectory,
    limits: CatalogLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<Vec<ManagedWorkflowReceipt>> {
    let mut result = Vec::new();
    let mut entries_seen = 0;
    scan_directory(
        receipts,
        "",
        &mut result,
        &mut entries_seen,
        limits,
        cancelled,
    )?;
    result.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(result)
}

fn scan_directory(
    directory: &SecureDirectory,
    prefix: &str,
    receipts: &mut Vec<ManagedWorkflowReceipt>,
    entries_seen: &mut usize,
    limits: CatalogLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<bool> {
    let mut found_receipt = false;
    let remaining = limits
        .entries
        .checked_sub(*entries_seen)
        .context("managed receipt entry count overflow")?;
    for name in directory.list_names(remaining, cancelled)? {
        if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
            bail!("managed receipt scan was cancelled");
        }
        *entries_seen = entries_seen
            .checked_add(1)
            .context("managed receipt entry count overflow")?;
        if *entries_seen > limits.entries {
            bail!("managed receipt scan exceeds its entry limit");
        }
        if name == "receipt.json" {
            if prefix.is_empty() {
                bail!("managed receipt file has no workflow id");
            }
            if receipts.len() >= limits.receipts {
                bail!("managed receipt catalog exceeds its receipt limit");
            }
            let bytes = directory.read_file("receipt.json", MAX_RECEIPT_BYTES as u64)?;
            let receipt = serde_json::from_slice::<ManagedWorkflowReceipt>(&bytes)
                .context("failed to parse managed workflow receipt")?;
            receipt.validate()?;
            if receipt.id != prefix {
                bail!("managed workflow receipt id does not match its directory");
            }
            receipts.push(receipt);
            found_receipt = true;
        } else {
            let child = directory.existing_child(&name)?;
            let id = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            super::receipt::validate_id(&id)?;
            if id.split('/').count() > MAX_ID_COMPONENTS {
                bail!("managed receipt directory exceeds its depth limit");
            }
            found_receipt |=
                scan_directory(&child, &id, receipts, entries_seen, limits, cancelled)?;
        }
    }
    if !prefix.is_empty() && !found_receipt {
        bail!("managed receipt catalog contains an orphan ID directory");
    }
    Ok(found_receipt)
}

#[cfg(all(test, unix))]
#[path = "catalog_tests.rs"]
mod tests;
