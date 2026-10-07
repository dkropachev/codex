use anyhow::bail;
use sha2::Digest;
use sha2::Sha256;

use super::fs::SecureDirectory;
use super::receipt::ManagedWorkflowReceipt;
use super::receipt::read_optional_receipt;

/// Identity used to compare a receipt at publication time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::managed) struct ReceiptIdentity([u8; 32]);

impl ReceiptIdentity {
    pub(in crate::managed) fn from_receipt(
        receipt: &ManagedWorkflowReceipt,
    ) -> anyhow::Result<Self> {
        Ok(Self(Sha256::digest(receipt.serialized_bytes()?).into()))
    }
}

/// The receipt state a caller observed before preparing a replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::managed) enum ExpectedCurrent {
    Absent,
    Receipt(ReceiptIdentity),
}

pub(super) fn compare_current(
    receipts: &SecureDirectory,
    id: &str,
    expected: &ExpectedCurrent,
) -> anyhow::Result<Option<ManagedWorkflowReceipt>> {
    let current = read_optional_receipt(receipts, id)?;
    let matches = match (expected, &current) {
        (ExpectedCurrent::Absent, None) => true,
        (ExpectedCurrent::Receipt(identity), Some(receipt)) => {
            identity == &ReceiptIdentity::from_receipt(receipt)?
        }
        (ExpectedCurrent::Absent, Some(_)) | (ExpectedCurrent::Receipt(_), None) => false,
    };
    if !matches {
        bail!("managed workflow receipt changed before publication");
    }
    Ok(current)
}

#[cfg(all(test, unix))]
#[path = "expected_tests.rs"]
mod tests;
