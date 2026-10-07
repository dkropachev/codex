#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::sync::atomic::AtomicBool;

#[cfg(not(any(unix, windows)))]
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::fs::SecureDirectory;
use crate::managed::fetch::VerificationLimits;

/// Copies a validated checkout into a private directory without following source aliases.
pub(super) fn copy_verified_payload(
    source: &AbsolutePathBuf,
    destination: &SecureDirectory,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    return unix::copy_verified_payload(source, destination, limits, deadline, cancelled);
    #[cfg(windows)]
    return windows::copy_verified_payload(source, destination, limits, deadline, cancelled);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, destination, limits, deadline, cancelled);
        bail!("secure managed workflow payload copy is unavailable on this platform")
    }
}

#[cfg(all(test, unix))]
#[path = "copy_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "copy/windows_tests.rs"]
mod windows_tests;
